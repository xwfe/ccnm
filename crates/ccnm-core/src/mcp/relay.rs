//! `call_mcp_tool`: MCP servers on the Runtime Node, relayed to the session
//! (P49, toexec v4 plan step 3).
//!
//! Which servers: the ones the project declares in its `.mcp.json`, then the
//! ones the runtime account installed for Claude Code (`~/.claude.json`) or
//! Codex (`~/.codex/config.toml`) -- a database server that has to run next
//! to the project is the case this exists for. Same name in two places: the
//! project's wins, as it does in Claude Code (project over user scope).
//! Reading the files, the handshake, the connection pool and trimming the
//! results are the shared crate's (`toexec-mcp`, which gld uses for the
//! same job on its machine); what is here is ccnm's part.
//!
//! # A relayed server is a command
//!
//! Starting one runs a program as the runtime account, with whatever that
//! program can reach, and the project's `.mcp.json` names programs the
//! project chose. So it goes behind every door `exec_command` goes behind
//! and no fewer: only a session that may write has the tool
//! (`WITHHELD_WITHOUT_WRITE`), the exec gate and the runtime credential
//! check run before a server is started, the workspace's OS sandbox wraps it
//! when it has one, the environment is cleaned the same way
//! ([`crate::safety::environment::runtime_child`]), and in a session with a
//! person at it every call asks them (`INTERACTION_TOOLS`). Listing the
//! servers starts nothing and needs none of that.
//!
//! Two things differ from a command, both on purpose:
//!
//! - The server's own `env` from its config is passed, tokens included: the
//!   runtime account (or the project) wrote it there for that server. What
//!   is never passed is an Agent's login (`ANTHROPIC_API_KEY`,
//!   `CLAUDE_CODE_OAUTH_TOKEN`, ...) -- not by name, and not smuggled in
//!   through `${VAR}` either, which only ever looks up names that are not
//!   authentication.
//! - Only stdio servers. A streamable-HTTP server does not need to run next
//!   to the project; the ones installed on the Agent Node are reached from
//!   there (P50, `agent_mcp.rs`). It is listed with that reason instead of
//!   vanishing.
//!
//! # Size
//!
//! A result's text past [`INLINE_BYTES`] -- read_output's largest page -- is
//! kept in the session's retention directory like a command's output, and
//! the rest is read with `read_output`: same paging, same limits, same
//! expiry, no second mechanism.
//!
//! # Shared with the Agent side
//!
//! The steps of one call -- the overview, a server's tool list, the call,
//! trimming the result -- are the same on the Agent Node, where
//! `ccnm internal agent-skills` relays the servers installed there (P50,
//! `agent_mcp.rs`). They are [`call`] over a [`Side`]: each machine says
//! which servers it has, how one starts, which cannot be relayed and why,
//! and where a long result goes.
//!
//! Deleting the feature: this file (keeping what `agent_mcp.rs` uses, or
//! that goes too), the tool in `server.rs`, `[runtime_mcp]` in the config,
//! the name in `session::MCP_TOOLS`, and the `toexec-mcp` dependency.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use toexec_mcp::child::{ChildTransport, Stop};
use toexec_mcp::installed::{self, Installed, Server, Transport as Config};
use toexec_mcp::pool::{CALL_TIMEOUT, Pool};
use toexec_mcp::shape;
use toexec_mcp::{Open, Transport};

use crate::config::RuntimeMcp;
use crate::error::{Error, Result};
use crate::mcp::retention::Output;
use crate::mcp::sandbox::{self, Sandbox};
use crate::process::Cmd;

/// The tool's name.
pub const TOOL: &str = "call_mcp_tool";

/// Text returned inline: `read_output`'s largest page, so a result and the
/// pages after it are the same size.
pub const INLINE_BYTES: usize = crate::mcp::output::MAX_LIMIT;
/// One image or audio clip, base64: `view_image`'s ceiling.
const MAX_MEDIA_BYTES: usize = 3_932_160;
/// One server's tool list. Playwright's 25 tools are 21 KB.
const MAX_LISTING_BYTES: usize = 64 * 1024;
/// A server's own instructions. DeepWiki's are 3 KB.
const MAX_INSTRUCTIONS: usize = 4 * 1024;
/// How often idle servers are looked for (idle means five minutes unused).
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// The description is capped at 2048 UTF-16 units by Claude Code, like
/// `load_skill`'s. What the fixed part leaves goes to server names.
const DESCRIPTION_BUDGET: usize = 2048;
/// What " and 400 more; call without server for all of them." needs.
const MORE_ROOM: usize = 60;
const INTRO: &str = "Use an MCP server on the runtime machine: one this workspace's .mcp.json declares, or one the runtime account installed for Claude Code or Codex. Without server: the servers and their state. With server: its tools, their input schemas and its instructions (this starts it). With server, tool and arguments: call that tool. A server runs as the runtime account, like exec_command. A long result continues with read_output.";

/// Arguments of `call_mcp_tool`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallMcpToolArgs {
    /// A server from this tool's description. Without it: the servers.
    #[serde(default)]
    pub server: Option<String>,
    /// One of that server's tools. Without it: its tools and their input
    /// schemas.
    #[serde(default)]
    pub tool: Option<String>,
    /// The tool's own arguments, as its input schema says.
    #[serde(default)]
    pub arguments: Option<Map<String, Value>>,
}

impl CallMcpToolArgs {
    /// Whether this call starts a server, and so has to pass the exec gate.
    pub fn starts_a_server(&self) -> bool {
        self.server.as_deref().is_some_and(|s| !s.trim().is_empty())
    }
}

/// The relayed servers of one session.
pub struct Relay {
    config: RuntimeMcp,
    home: PathBuf,
    /// `$CODEX_HOME` as this server found it; `None` is `~/.codex`.
    codex_home: Option<PathBuf>,
    root: PathBuf,
    sandbox: Option<Arc<Sandbox>>,
    pool: Arc<Pool>,
    /// The usable servers when the session started, for the description: a
    /// client keeps `tools/list` for the life of the connection, so the
    /// catalog is what the model was told. A call reads the files again.
    catalog: Vec<String>,
}

impl Relay {
    /// `None` when this machine relays nothing (`[runtime_mcp] enabled =
    /// false`).
    pub fn new(
        config: &RuntimeMcp,
        home: &Path,
        codex_home: Option<PathBuf>,
        root: &Path,
        sandbox: Option<Arc<Sandbox>>,
    ) -> Option<Relay> {
        if !config.enabled {
            return None;
        }
        let pool = Arc::new(Pool::new());
        Pool::sweep(&pool, SWEEP_EVERY);
        let mut relay = Relay {
            config: config.clone(),
            home: home.to_path_buf(),
            codex_home,
            root: root.to_path_buf(),
            sandbox,
            pool,
            catalog: Vec::new(),
        };
        relay.catalog = relay
            .read()
            .servers
            .iter()
            .filter(|server| unusable(server).is_none())
            .map(|server| server.name.clone())
            .collect();
        Some(relay)
    }

    /// Whether the tool is worth offering: at least one server could run.
    pub fn offered(&self) -> bool {
        !self.catalog.is_empty()
    }

    /// The tool description: what it does, then the servers by name.
    pub fn description(&self) -> String {
        describe(INTRO, &self.catalog)
    }

    /// Stop every server this session started. Called before the session's
    /// write guard is let go: a server can write the working tree.
    pub fn close_all(&self) {
        self.pool.close_all();
    }

    /// What the configs say now, hidden names left out.
    fn read(&self) -> Installed {
        let mut places = installed::Places::new(&self.home, self.codex_home.as_deref());
        if self.config.project {
            places = places.with_project(&self.root);
        }
        let mut found = installed::read(&places, &lookup);
        found
            .servers
            .retain(|server| !self.config.hidden.contains(&server.name));
        found
    }

    /// One call. Blocking: run it off the async thread.
    pub fn call(&self, args: &CallMcpToolArgs, output: Option<&Output>) -> Result<CallToolResult> {
        let installed = self.read();
        let opener = Opener {
            root: &self.root,
            sandbox: self.sandbox.as_deref(),
        };
        let notes: Vec<String> = self
            .sandbox
            .iter()
            .map(|_| sandbox_note().to_string())
            .collect();
        let rest = |whole: &str, end: usize| -> Result<String> {
            Ok(match output {
                Some(output) => format!(
                    "[the result is {} bytes; this is bytes 0-{end}. The rest is kept like a command's output: read_output output_ref={} offset={end}]",
                    whole.len(),
                    keep(output, whole)?
                ),
                None => format!(
                    "[the result is {} bytes and only bytes 0-{end} are shown: this runtime has no state directory to keep the rest in]",
                    whole.len()
                ),
            })
        };
        call(
            &Side {
                installed: &installed,
                pool: &self.pool,
                opener: &opener,
                unusable: &unusable,
                words: &RUNTIME,
                notes: &notes,
                keep: &rest,
            },
            args,
        )
    }
}

/// What the relay on one machine supplies. The steps of a call are the
/// same on both (module doc).
pub(crate) struct Side<'a> {
    /// What the configs say now, hidden names already left out.
    pub installed: &'a Installed,
    pub pool: &'a Pool,
    pub opener: &'a dyn Open,
    /// Why a server cannot be relayed from here, if it cannot.
    pub unusable: &'a dyn Fn(&Server) -> Option<String>,
    pub words: &'a Words,
    /// Lines the overview adds before its last one.
    pub notes: &'a [String],
    /// Keep a long result's whole text and say, in one bracketed line, how
    /// to read on from byte `end`.
    pub keep: &'a dyn Fn(&str, usize) -> Result<String>,
}

/// The sentences that name the machine.
pub(crate) struct Words {
    /// The overview's first line.
    pub heading: &'static str,
    /// The whole overview when no server is installed at all.
    pub nothing: &'static str,
    /// Why a server of that name is not found when none is installed.
    pub none_installed: &'static str,
    /// The overview's last line.
    pub next: &'static str,
}

const RUNTIME: Words = Words {
    heading: "MCP servers on the runtime machine for this workspace (the project's .mcp.json first):",
    nothing: "No MCP server here: this workspace has no .mcp.json with servers in it, and the runtime account has none installed for Claude Code or Codex.",
    none_installed: "this workspace declares none and the runtime account has none installed",
    next: "Call call_mcp_tool with server=<name> to see its tools and their input schemas (that starts it), then with tool and arguments to call one.",
};

/// One call of `call_mcp_tool` on either machine. Blocking.
pub(crate) fn call(side: &Side, args: &CallMcpToolArgs) -> Result<CallToolResult> {
    let installed = side.installed;
    let Some(name) = args
        .server
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        if args.tool.is_some() || args.arguments.is_some() {
            return Err(Error::invalid_args(
                "tool and arguments need server: which server's tool?",
            ));
        }
        return Ok(text(overview(side)));
    };
    let Some(server) = installed.find(name) else {
        let names: Vec<&str> = installed.servers.iter().map(|s| s.name.as_str()).collect();
        return Err(Error::invalid_args(if names.is_empty() {
            format!("no MCP server {name}: {}", side.words.none_installed)
        } else {
            format!("no MCP server {name} here; there are: {}", names.join(", "))
        }));
    };
    if let Some(problem) = (side.unusable)(server) {
        return Err(Error::config(format!(
            "MCP server {name} is not relayed: {problem}"
        )));
    }
    let timeout = server.tool_timeout.unwrap_or(CALL_TIMEOUT);
    let tool = args
        .tool
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(tool) = tool else {
        if args.arguments.is_some() {
            return Err(Error::invalid_args(
                "arguments need tool: which of the server's tools?",
            ));
        }
        let listed = side
            .pool
            .with(server, "", side.opener, timeout, |live| {
                Ok((
                    live.tools.clone(),
                    live.client.instructions.clone(),
                    live.client.server_name.clone(),
                    live.client.server_version.clone(),
                ))
            })
            .map_err(|error| failure(server, error))?;
        return Ok(text(listing(server, listed)));
    };
    let arguments = Value::Object(args.arguments.clone().unwrap_or_default());
    let called = side
        .pool
        .with(server, "", side.opener, timeout, |live| {
            let offered = live
                .tools
                .iter()
                .any(|t| t["name"] == json!(tool) && server.allows_tool(tool));
            if !offered {
                return Ok(Err(tool_names(server, &live.tools)));
            }
            live.client.call_tool(tool, arguments, timeout).map(Ok)
        })
        .map_err(|error| failure(server, error))?;
    match called {
        Ok(result) => shaped(result, side.keep),
        Err(names) => Err(Error::invalid_args(format!(
            "MCP server {name} has no tool {tool}; it has: {}",
            names.join(", ")
        ))),
    }
}

fn overview(side: &Side) -> String {
    let installed = side.installed;
    if installed.servers.is_empty() {
        return side.words.nothing.to_string();
    }
    let mut lines = vec![side.words.heading.to_string()];
    for server in &installed.servers {
        let state = if let Some(problem) = (side.unusable)(server) {
            format!("not relayed: {problem}")
        } else if let Some(tools) = side.pool.known_tools(&server.name, "") {
            let names: Vec<&str> = tools
                .iter()
                .filter_map(|t| t["name"].as_str())
                .filter(|n| server.allows_tool(n))
                .collect();
            format!("running, tools: {}", names.join(", "))
        } else {
            "not started".to_string()
        };
        lines.push(format!(
            "- {} ({}): {state}",
            server.name,
            server.source.file()
        ));
    }
    for problem in &installed.problems {
        let what = problem
            .server
            .as_deref()
            .map(|name| format!(" entry {name}"))
            .unwrap_or_default();
        lines.push(format!(
            "[{}{what} could not be read: {}]",
            problem.source.file(),
            problem.message
        ));
    }
    for note in side.notes {
        lines.push(format!("[{note}]"));
    }
    lines.push(side.words.next.to_string());
    lines.join("\n")
}

/// The tool description: `intro`, then the servers by name, as many as
/// fit Claude Code's 2048 UTF-16 units.
pub(crate) fn describe(intro: &str, catalog: &[String]) -> String {
    let mut text = format!("{intro} Servers here:");
    let mut used = utf16(&text);
    let mut named = 0;
    for name in catalog {
        let entry = format!(" {name},");
        // Room is kept for the " and N more; ..." that may have to follow.
        if used + utf16(&entry) + MORE_ROOM > DESCRIPTION_BUDGET {
            break;
        }
        used += utf16(&entry);
        text.push_str(&entry);
        named += 1;
    }
    if named > 0 {
        text.pop();
    }
    if named < catalog.len() {
        text.push_str(&format!(
            " and {} more; call without server for all of them.",
            catalog.len() - named
        ));
    } else {
        text.push('.');
    }
    text
}

/// Environment lookup for `${VAR}` in a config. Never an Agent's login or
/// anything named like a credential: a `.mcp.json` must not be able to copy
/// `ANTHROPIC_API_KEY` into a server under another name. Such a variable
/// reads as unset, so the server is reported as missing it.
fn lookup(name: &str) -> Option<String> {
    let os = std::ffi::OsStr::new(name);
    if crate::safety::environment::agent_private(os)
        || crate::safety::environment::authentication(os)
    {
        return None;
    }
    std::env::var(name).ok()
}

/// Why this server cannot be relayed as configured, if it cannot.
fn unusable(server: &Server) -> Option<String> {
    // Codex's `enabled = false`, or `disabled: true` in a JSON config: the
    // client it was installed for would not start it either.
    if server.off_in_source {
        return Some(format!("it is turned off in {}", server.source.file()));
    }
    if !server.missing_env.is_empty() {
        return Some(format!(
            "its config uses {} which the runtime does not pass on (unset here, or named like a credential)",
            server.missing_env.join(", ")
        ));
    }
    match &server.transport {
        Config::Stdio { .. } => None,
        Config::Http { .. } | Config::Sse { .. } => Some(
            "it is an HTTP server, which does not need to run next to the project; ccnm relays only servers started as a program here".to_string(),
        ),
    }
}

fn sandbox_note() -> &'static str {
    "this workspace runs commands in an OS sandbox, and its servers too: they can write only inside the workspace (not .git), $TMPDIR and /tmp, and have no network"
}

/// How ccnm starts a server: as a command would run.
struct Opener<'a> {
    root: &'a Path,
    sandbox: Option<&'a Sandbox>,
}

impl Open for Opener<'_> {
    fn open(&self, server: &Server) -> std::result::Result<Box<dyn Transport>, toexec_mcp::Error> {
        let Config::Stdio {
            command,
            args,
            env,
            cwd,
        } = &server.transport
        else {
            return Err(toexec_mcp::Error::Start(
                "only servers started as a program are relayed here".into(),
            ));
        };
        // The project root, as Claude Code starts a project's servers; a
        // Codex `cwd` is taken relative to it.
        let cwd = cwd
            .as_ref()
            .map(|dir| self.root.join(dir))
            .unwrap_or_else(|| self.root.to_path_buf());
        if sandbox::locate(command, &cwd, std::env::var_os("PATH").as_deref()).is_none() {
            return Err(toexec_mcp::Error::Start(format!(
                "`{command}` is not on the runtime account's PATH (or, as a path, not a file from {})",
                if cwd == self.root {
                    "the workspace root".to_string()
                } else {
                    "its cwd".to_string()
                }
            )));
        }
        let mut cmd = Cmd::new(command).args(args).cwd(&cwd);
        cmd = crate::safety::environment::runtime_child(cmd);
        // After the cleaning, so a token the config gives this server is
        // not stripped with the inherited ones -- but never an Agent's.
        for (name, value) in env {
            if !crate::safety::environment::agent_private(std::ffi::OsStr::new(name)) {
                cmd = cmd.env(name, value);
            }
        }
        if let Some(sandbox) = self.sandbox {
            cmd = sandbox.wrap(cmd, &cwd);
        }
        let mut process = cmd.process();
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std::os::unix::process::CommandExt::process_group(&mut process, 0);
        let child = process.spawn().map_err(|error| {
            toexec_mcp::Error::Start(format!("cannot start `{command}`: {error}"))
        })?;
        Ok(Box::new(ChildTransport::new(child, stop())?))
    }

    fn me(&self) -> (&str, &str) {
        ("ccnm", crate::VERSION)
    }
}

/// A server that did not exit on EOF gets its whole process group killed,
/// the way `process::kill_group` does it: a `kill` of `-<pid>`, since this
/// crate has no `unsafe` for `killpg`. Only then, while the child has not
/// been collected and its pid cannot belong to anyone else; one that exited
/// by itself is left alone for the same reason.
pub(crate) fn stop() -> Stop {
    Box::new(|child, exited| {
        if !exited {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    })
}

fn tool_names(server: &Server, tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .filter(|n| server.allows_tool(n))
        .map(str::to_string)
        .collect()
}

type Listed = (Vec<Value>, Option<String>, Option<String>, Option<String>);

fn listing(server: &Server, (tools, instructions, name, version): Listed) -> String {
    let tools: Vec<Value> = tools
        .into_iter()
        .filter(|t| t["name"].as_str().is_some_and(|n| server.allows_tool(n)))
        .collect();
    let (listed, omitted) = shape::fit_listing(&tools, MAX_LISTING_BYTES);
    let who = match (name, version) {
        (Some(name), Some(version)) => format!(" ({name} {version})"),
        (Some(name), None) => format!(" ({name})"),
        _ => String::new(),
    };
    let mut text = format!(
        "[MCP server {}{who}: {} tool(s)]\n",
        server.name,
        listed.len()
    );
    if let Some(instructions) = instructions {
        text.push_str(&format!(
            "[its instructions]\n{}\n",
            shape::cut_text(&instructions, MAX_INSTRUCTIONS)
        ));
    }
    text.push_str(&Value::Array(listed).to_string());
    if let Some(omitted) = omitted {
        text.push_str(&format!("\n[left out: {omitted}]"));
    }
    text.push_str(&format!(
        "\nCall one with {TOOL} server={} tool=<name> arguments={{...}}.",
        server.name
    ));
    text
}

/// The server's result, trimmed: duplicate `structuredContent` gone, a
/// too-large image replaced by a line, and text past [`INLINE_BYTES`]
/// handed to `keep`, whose note follows the first part.
pub(crate) fn shaped(
    result: Value,
    keep: &dyn Fn(&str, usize) -> Result<String>,
) -> Result<CallToolResult> {
    let shaped = shape::shape(
        &result,
        &shape::Limits {
            inline_bytes: INLINE_BYTES,
            max_media_bytes: MAX_MEDIA_BYTES,
        },
    );
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if let Some(whole) = shaped.long {
        let end = shape::part_end(&whole, 0, INLINE_BYTES);
        let note = keep(&whole, end)?;
        blocks.push(ContentBlock::text(whole[..end].to_string()));
        blocks.push(ContentBlock::text(note));
    }
    for item in shaped.items {
        blocks.push(
            serde_json::from_value::<ContentBlock>(item.clone())
                .unwrap_or_else(|_| ContentBlock::text(item.to_string())),
        );
    }
    if blocks.is_empty() {
        blocks.push(ContentBlock::text("[the tool returned no content]"));
    }
    Ok(if shaped.is_error {
        CallToolResult::error(blocks)
    } else {
        CallToolResult::success(blocks)
    })
}

/// Keep `text` as a finished run of this session and return its output_ref.
fn keep(output: &Output, text: &str) -> Result<String> {
    let (run, mut stdout, _stderr) = output.begin()?;
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::internal("cannot keep the MCP result").with_source(e))?;
    drop(stdout);
    output.finish(&run);
    Ok(run.reference.clone())
}

/// A server that failed, in ccnm's words. Which code decides what the model
/// does next: a server that cannot start is the machine's
/// (`CCNM_E_DEPENDENCY`), arguments the tool refused are the model's to fix
/// (`CCNM_E_INVALID_ARGS`), and a call cut off midway may or may not have
/// done its work -- said in so many words, because retrying something that
/// changed state is not free.
fn failure(server: &Server, error: toexec_mcp::Error) -> Error {
    use toexec_mcp::Error as E;
    let name = &server.name;
    match &error {
        E::Refused { .. } => Error::invalid_args(format!(
            "MCP server {name}: {error}; check the arguments against the tool's input schema"
        )),
        E::Timeout { during, .. } | E::Closed { during, .. } if during == "tools/call" => {
            Error::dependency(format!(
                "MCP server {name}: {error}. Whether the call did its work is unknown; the server is stopped and the next call starts it again, so look before repeating anything that changes state"
            ))
        }
        E::Busy { .. } => {
            Error::dependency(format!("MCP server {name}: {error}; try again in a moment"))
        }
        _ => Error::dependency(format!("MCP server {name} did not start: {error}")),
    }
}

fn text(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn temp(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-relay-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    /// A stdio MCP server in sh: answers initialize, lists `echo` and
    /// `big`, echoes its arguments, and writes 50 000 bytes for `big`.
    const SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"fake","version":"1"},"instructions":"be brief"}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}},{"name":"big","inputSchema":{"type":"object"}},{"name":"env","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"name":"big"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$(head -c 50000 /dev/zero | tr '\0' 'x')" ;;
    *'"name":"env"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"token=%s agent=%s"}],"structuredContent":{"x":1}}}\n' "$id" "$DB_TOKEN" "$ANTHROPIC_API_KEY" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}]}}\n' "$id" ;;
  esac
done
"#;

    fn project(name: &str) -> (TestDir, TestDir) {
        let home = temp(&format!("{name}-home"));
        let root = temp(&format!("{name}-root"));
        let script = root.join("fake-mcp.sh");
        fs::write(&script, SERVER).unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            root.join(".mcp.json"),
            r#"{ "mcpServers": {
                "fake": { "command": "./fake-mcp.sh", "env": { "DB_TOKEN": "t0k", "ANTHROPIC_API_KEY": "leak" } },
                "remote": { "type": "http", "url": "https://example.invalid/mcp" },
                "keyed": { "command": "./fake-mcp.sh", "env": { "K": "${SOME_SECRET_TOKEN}" } },
                "off": { "command": "./fake-mcp.sh", "disabled": true }
            } }"#,
        )
        .unwrap();
        (home, root)
    }

    fn relay(home: &Path, root: &Path) -> Relay {
        Relay::new(&RuntimeMcp::default(), home, None, root, None).unwrap()
    }

    fn texts(result: &CallToolResult) -> Vec<String> {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|t| t.text.clone()))
            .collect()
    }

    fn args(server: Option<&str>, tool: Option<&str>) -> CallMcpToolArgs {
        CallMcpToolArgs {
            server: server.map(str::to_string),
            tool: tool.map(str::to_string),
            arguments: None,
        }
    }

    #[test]
    fn the_catalog_names_only_what_can_run_and_the_overview_says_why_the_rest_cannot() {
        let (home, root) = project("catalog");
        let relay = relay(&home, &root);
        assert!(relay.offered());
        let description = relay.description();
        assert!(
            description.ends_with("Servers here: fake."),
            "{description}"
        );

        let overview = texts(&relay.call(&args(None, None), None).unwrap()).join("\n");
        assert!(
            overview.contains("- fake (.mcp.json): not started"),
            "{overview}"
        );
        assert!(overview.contains("- remote (.mcp.json): not relayed: it is an HTTP server"));
        assert!(
            overview
                .contains("- keyed (.mcp.json): not relayed: its config uses SOME_SECRET_TOKEN"),
            "a credential-looking variable is never looked up: {overview}"
        );
        assert!(
            overview.contains("- off (.mcp.json): not relayed: it is turned off in .mcp.json"),
            "{overview}"
        );
    }

    #[test]
    fn a_projects_server_lists_its_tools_and_answers_a_call() {
        let (home, root) = project("call");
        let relay = relay(&home, &root);
        let listed = texts(&relay.call(&args(Some("fake"), None), None).unwrap()).join("\n");
        assert!(
            listed.starts_with("[MCP server fake (fake 1): 3 tool(s)]"),
            "{listed}"
        );
        assert!(listed.contains("be brief"), "{listed}");

        let called = relay.call(&args(Some("fake"), Some("echo")), None).unwrap();
        assert_eq!(texts(&called), ["echoed"]);
        assert_ne!(called.is_error, Some(true));

        let err = relay
            .call(&args(Some("fake"), Some("nope")), None)
            .unwrap_err();
        assert!(err.to_string().contains("it has: echo, big, env"), "{err}");
        relay.close_all();
    }

    /// The config's own token reaches the server; an Agent login never does,
    /// not even when the config names it.
    #[test]
    fn the_configured_token_is_passed_and_an_agent_login_is_not() {
        let (home, root) = project("env");
        let relay = relay(&home, &root);
        let called = relay.call(&args(Some("fake"), Some("env")), None).unwrap();
        assert_eq!(
            texts(&called),
            ["token=t0k agent="],
            "structuredContent dropped too"
        );
        relay.close_all();
    }

    #[test]
    fn a_long_result_is_kept_for_read_output() {
        let (home, root) = project("long");
        let state = temp("long-state");
        let output = Output::new(&state, "session-1");
        let relay = relay(&home, &root);
        let called = relay
            .call(&args(Some("fake"), Some("big")), Some(&output))
            .unwrap();
        let parts = texts(&called);
        assert_eq!(parts[0].len(), INLINE_BYTES);
        assert!(
            parts[1].contains("read_output output_ref=r-"),
            "{}",
            parts[1]
        );
        let reference = parts[1]
            .split("output_ref=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap();
        let kept = fs::read_to_string(output.dir().join(reference).join("stdout")).unwrap();
        assert_eq!(kept.len(), 50_000);
        relay.close_all();
    }

    #[test]
    fn off_hidden_or_without_project_it_offers_less() {
        let (home, root) = project("off");
        let off = RuntimeMcp {
            enabled: false,
            ..RuntimeMcp::default()
        };
        assert!(Relay::new(&off, &home, None, &root, None).is_none());
        let hidden = RuntimeMcp {
            hidden: ["fake".to_string()].into(),
            ..RuntimeMcp::default()
        };
        assert!(
            !Relay::new(&hidden, &home, None, &root, None)
                .unwrap()
                .offered()
        );
        let no_project = RuntimeMcp {
            project: false,
            ..RuntimeMcp::default()
        };
        assert!(
            !Relay::new(&no_project, &home, None, &root, None)
                .unwrap()
                .offered()
        );
    }

    #[test]
    fn a_long_catalog_names_what_fits_and_counts_the_rest() {
        let (home, root) = project("many");
        let mut relay = relay(&home, &root);
        relay.catalog = (0..400).map(|i| format!("server-number-{i}")).collect();
        let description = relay.description();
        assert!(
            utf16(&description) <= DESCRIPTION_BUDGET,
            "{}",
            utf16(&description)
        );
        assert!(description.contains(" more; call without server for all of them."));
    }
}

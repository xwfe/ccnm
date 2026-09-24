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
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
    /// What closing a server could not clear, whenever it was closed.
    left: Left,
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
            left: Left::default(),
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

    /// Stop every server this session started, and say what could not be
    /// cleared: now, or earlier -- an idle server reaped, a call that timed
    /// out. Called before the session's write guard is let go: a server can
    /// write the working tree, and so can what it left in its process group,
    /// so anything returned here keeps the guard held (C51-01).
    pub fn close_all(&self) -> Vec<String> {
        self.pool.close_all();
        std::mem::take(&mut *self.left.lock().unwrap_or_else(PoisonError::into_inner))
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
            left: &self.left,
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
    left: &'a Left,
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
        let (child, stop) = start(&mut process, &server.name, self.left).map_err(|error| {
            toexec_mcp::Error::Start(format!("cannot start `{command}`: {error}"))
        })?;
        Ok(Box::new(ChildTransport::new(child, stop)?))
    }

    fn me(&self) -> (&str, &str) {
        ("ccnm", crate::VERSION)
    }
}

/// What closing servers could not clear, one line each; drained by
/// whoever decides what that means ([`Relay::close_all`]).
pub(crate) type Left = Arc<Mutex<Vec<String>>>;

/// The program that holds a server's process group ([`start`]).
const ANCHOR: &str = "/bin/cat";
/// Absolute, like the managed stop's (`work.rs`): which `kill` and `ps` run
/// is not up to whatever `PATH` this process was given.
const KILLER: &str = "/bin/kill";
const LISTER: &str = "/bin/ps";
/// How long a process group has to empty after `SIGKILL` before what is
/// still in it is reported. Only something no signal reaches -- a setuid
/// program, a process stuck in the kernel -- lasts this long.
const CLEAR_WITHIN: Duration = Duration::from_secs(5);
const CLEAR_PAUSE_MIN: Duration = Duration::from_millis(20);
const CLEAR_PAUSE_MAX: Duration = Duration::from_millis(500);

/// Spawn `process` -- a stdio server, pipes already set -- in a process
/// group that can be killed whole however the server ends, and return it
/// with the [`Stop`] that does so when the connection closes. What that
/// cannot clear goes to `left`, named after `server`.
///
/// **Why an anchor.** A group is named by its leader's pid, and once that
/// pid is collected the number can go to a stranger, who can then lead a
/// group of the same number. So a server that led its own group could only
/// have it killed while the server itself was uncollected -- when it
/// ignored EOF. One that exited cleanly left its group alone, and a child
/// still in it went on writing after the write guard was let go: C51-01,
/// found in P51 with a server that started a background child and exited.
///
/// Here the group is led by an anchor ccnm holds instead, started first,
/// and the server joins it. The anchor is collected last, once the group is
/// seen empty; until then the number stays reserved -- no pid is handed out
/// while a process group of that number exists, and the anchor, running or
/// a zombie, keeps it existing. So the group is always ccnm's to kill.
///
/// `cat` because every system has it and it ends by itself when ccnm does:
/// its stdin is a pipe only ccnm holds. One per running server.
///
/// Not reached: a descendant that left the group (`setsid`, a daemon's
/// double fork). Nothing short of an OS container sees those; that is the
/// line between what ccnm supervises and what it cannot.
pub(crate) fn start(
    process: &mut Command,
    server: &str,
    left: &Left,
) -> std::io::Result<(Child, Stop)> {
    let mut anchor = Command::new(ANCHOR)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            std::io::Error::other(format!(
                "cannot start {ANCHOR} to hold its process group: {error}"
            ))
        })?;
    process.process_group(anchor.id() as i32);
    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = anchor.kill();
            let _ = anchor.wait();
            return Err(error);
        }
    };
    let server = server.to_string();
    let left = Arc::clone(left);
    let stop: Stop = Box::new(move |_, _| {
        if let Some(problem) = clear(&mut anchor, KILLER, LISTER, CLEAR_WITHIN) {
            tracing::warn!(server, problem, "an MCP server left processes behind");
            left.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("MCP server {server} ({problem})"));
        }
    });
    Ok((child, stop))
}

/// Kill everything in the anchor's group until nothing that can run is
/// left or `within` has passed, then collect the anchor. `None` when the
/// group is cleared; otherwise what is left, in words for the write guard.
///
/// `KILL` straight away, whether or not the server exited: it has had its
/// EOF and `CLOSE_GRACE` to end properly, and what is still in its group
/// now is what it left behind. Killing again on every round is safe for as
/// long as the anchor is uncollected (see [`start`]), and catches a child
/// forked while the first kill was on its way.
fn clear(anchor: &mut Child, killer: &str, lister: &str, within: Duration) -> Option<String> {
    let group = anchor.id();
    let deadline = Instant::now() + within;
    let mut pause = CLEAR_PAUSE_MIN;
    let outcome = loop {
        // Whether `kill` ran is not the answer; the process list is.
        let _ = Command::new(killer)
            .args(["-KILL", "--", &format!("-{group}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let seen = members(lister, group);
        if seen.as_ref().is_ok_and(Vec::is_empty) {
            break None;
        }
        if Instant::now() >= deadline {
            break Some(match seen {
                Ok(pids) => format!(
                    "process group {group}: {} still running after SIGKILL",
                    pids.iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Err(why) => format!("process group {group} could not be checked: {why}"),
            });
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(CLEAR_PAUSE_MAX);
    };
    let _ = anchor.kill();
    let _ = anchor.wait();
    outcome
}

/// The processes of `group` that can still run, from `lister -A -o
/// pid=,pgid=,stat=` (the same on macOS and procps).
fn members(lister: &str, group: u32) -> std::result::Result<Vec<u32>, String> {
    let listed = Command::new(lister)
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run {lister}: {error}"))?;
    if !listed.status.success() {
        return Err(format!("{lister} failed ({})", listed.status));
    }
    running_in(&String::from_utf8_lossy(&listed.stdout), group)
}

/// Neither the anchor, whose pid names the group, nor a zombie -- dead,
/// only waiting for whoever inherited it to collect it. A list that cannot
/// be read proves nothing, so it is an error, never an empty group.
fn running_in(listing: &str, group: u32) -> std::result::Result<Vec<u32>, String> {
    let mut rows = 0;
    let mut found = Vec::new();
    for line in listing.lines().filter(|line| !line.trim().is_empty()) {
        rows += 1;
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (pid, pgid, state) = match fields[..] {
            [pid, pgid, state] => (pid.parse::<u32>(), pgid.parse::<u32>(), state),
            _ => return Err(format!("cannot read the process list line `{line}`")),
        };
        let (Ok(pid), Ok(pgid)) = (pid, pgid) else {
            return Err(format!("cannot read the process list line `{line}`"));
        };
        if pgid == group && pid != group && !state.starts_with('Z') {
            found.push(pid);
        }
    }
    if rows == 0 {
        return Err("the process list was empty".to_string());
    }
    Ok(found)
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
pub(crate) mod tests {
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
    /// `big`, echoes its arguments, writes 50 000 bytes for `big`, and for
    /// `slow` sleeps five seconds and says nothing.
    const SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"fake","version":"1"},"instructions":"be brief"}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}},{"name":"big","inputSchema":{"type":"object"}},{"name":"env","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"name":"big"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$(head -c 50000 /dev/zero | tr '\0' 'x')" ;;
    *'"name":"env"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"token=%s agent=%s"}],"structuredContent":{"x":1}}}\n' "$id" "$DB_TOKEN" "$ANTHROPIC_API_KEY" ;;
    *'"name":"slow"'*) sleep 5 ;;
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

    // -- what a server leaves in its process group (C51-01, P52) --

    /// Whether `pid` can still run: neither gone nor a zombie.
    pub(crate) fn runs(pid: u32) -> bool {
        let state = Command::new(LISTER)
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&state.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }

    pub(crate) fn wait_for_pid(path: &Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no pid in {}", path.display());
    }

    fn group_of(pid: u32) -> u32 {
        let group = Command::new(LISTER)
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&group.stdout)
            .trim()
            .parse()
            .unwrap()
    }

    /// A background helper the way a server starts one: `sleep 60` with its
    /// pipes closed and, the shell having no job control, in the server's
    /// process group -- no `setsid`. Its pid goes to `child.pid`; `then` is
    /// what the server does next.
    fn leaving(name: &str, then: &str) -> (TestDir, TestDir) {
        let (home, root) = project(name);
        let script = root.join("leaving-mcp.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho $$ > server.pid\nsleep 60 </dev/null >/dev/null 2>&1 &\necho $! > child.pid\n{then}\n"
            ),
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            root.join(".mcp.json"),
            r#"{ "mcpServers": { "leaving": { "command": "./leaving-mcp.sh" } } }"#,
        )
        .unwrap();
        (home, root)
    }

    /// C51-01 itself: the server reads EOF and exits cleanly, and until P52
    /// its helper went on running after the write guard was let go.
    #[test]
    fn a_server_that_exits_on_eof_takes_what_it_left_in_its_group_along() {
        let (home, root) = leaving("eof", "exec ./fake-mcp.sh");
        let relay = relay(&home, &root);
        relay.call(&args(Some("leaving"), None), None).unwrap();
        let server = wait_for_pid(&root.join("server.pid"));
        let child = wait_for_pid(&root.join("child.pid"));
        assert_eq!(group_of(child), group_of(server), "same group, no setsid");
        assert_ne!(group_of(server), server, "the group is the anchor's");
        assert!(runs(child));

        assert!(relay.close_all().is_empty(), "nothing is left");
        assert!(!runs(server));
        assert!(!runs(child), "the helper went with the server");
    }

    /// A server still there after the grace was already killed with its
    /// group; now the group is checked as well.
    #[test]
    fn a_server_that_ignores_eof_is_killed_with_what_it_started() {
        let (home, root) = leaving("stubborn", "./fake-mcp.sh\nsleep 60");
        let relay = relay(&home, &root);
        relay.call(&args(Some("leaving"), None), None).unwrap();
        let server = wait_for_pid(&root.join("server.pid"));
        let child = wait_for_pid(&root.join("child.pid"));
        assert!(relay.close_all().is_empty());
        assert!(!runs(server));
        assert!(!runs(child));
    }

    /// Killed from outside, the server leaves its helper in the group; the
    /// close that follows still finds it.
    #[test]
    fn a_server_killed_from_outside_still_has_its_group_cleared() {
        let (home, root) = leaving("killed", "exec ./fake-mcp.sh");
        let relay = relay(&home, &root);
        relay.call(&args(Some("leaving"), None), None).unwrap();
        let server = wait_for_pid(&root.join("server.pid"));
        let child = wait_for_pid(&root.join("child.pid"));
        // The server alone, not its group.
        Command::new(KILLER)
            .args(["-KILL", &server.to_string()])
            .status()
            .unwrap();
        assert!(relay.close_all().is_empty());
        assert!(!runs(child));
    }

    /// Closed because it sat idle or because a call timed out: the same
    /// close, so the same clearing, and nothing reported.
    #[test]
    fn an_idle_server_and_one_whose_call_timed_out_are_cleared_the_same_way() {
        let (home, root) = leaving("idle", "exec ./fake-mcp.sh");
        let installed = relay(&home, &root).read();
        let server = installed.find("leaving").unwrap();
        let left = Left::default();
        let opener = Opener {
            root: &root,
            sandbox: None,
            left: &left,
        };
        let wait = Duration::from_secs(5);

        let pool = Pool::new().idle_after(Duration::ZERO);
        pool.with(server, "first", &opener, wait, |_| Ok(()))
            .unwrap();
        let idle = wait_for_pid(&root.join("child.pid"));
        fs::remove_file(root.join("child.pid")).unwrap();
        // A call reaps what has been idle -- zero seconds is enough -- first.
        pool.with(server, "second", &opener, wait, |_| Ok(()))
            .unwrap();
        assert!(!runs(idle), "reaped with its server");
        let second = wait_for_pid(&root.join("child.pid"));
        pool.close_all();
        assert!(!runs(second));
        fs::remove_file(root.join("child.pid")).unwrap();

        // Idle servers are not reaped here, so it is the timeout that ends
        // this one.
        let pool = Pool::new();
        let error = pool
            .with(server, "", &opener, wait, |live| {
                live.client
                    .call_tool("slow", json!({}), Duration::from_millis(300))
            })
            .unwrap_err();
        assert!(
            matches!(error, toexec_mcp::Error::Timeout { .. }),
            "{error}"
        );
        let timed_out = wait_for_pid(&root.join("child.pid"));
        assert!(!runs(timed_out), "dropped with the broken connection");
        assert!(pool.known_tools("leaving", "").is_none());
        assert!(left.lock().unwrap().is_empty());
    }

    /// The line this cannot cross: a helper that left the group (`setsid`)
    /// is neither killed nor reported, and the guard is handed on beside
    /// it. Pinned so that it is not forgotten; support-matrix.md says the
    /// same. If something here ever does reach it, change both.
    #[test]
    fn a_child_that_left_the_group_is_beyond_reach_and_not_reported() {
        let (home, root) = project("setsid");
        let script = root.join("escaping-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\npython3 -c 'import os,time; os.setsid(); time.sleep(60)' </dev/null >/dev/null 2>&1 &\necho $! > child.pid\nexec ./fake-mcp.sh\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            root.join(".mcp.json"),
            r#"{ "mcpServers": { "escaping": { "command": "./escaping-mcp.sh" } } }"#,
        )
        .unwrap();
        let relay = relay(&home, &root);
        relay.call(&args(Some("escaping"), None), None).unwrap();
        let child = wait_for_pid(&root.join("child.pid"));
        // setsid runs a moment after the fork.
        let deadline = Instant::now() + Duration::from_secs(5);
        while group_of(child) != child && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(group_of(child), child, "it left");

        assert!(relay.close_all().is_empty());
        let escaped = runs(child);
        let _ = Command::new(KILLER)
            .args(["-KILL", &child.to_string()])
            .status();
        assert!(escaped, "out of reach, as documented");
    }

    /// An anchor with one more process in its group, for [`clear`].
    fn group_with_a_member() -> (Child, Child) {
        let anchor = Command::new(ANCHOR)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let member = Command::new("sleep")
            .arg("60")
            .process_group(anchor.id() as i32)
            .spawn()
            .unwrap();
        (anchor, member)
    }

    /// When the group does not empty, what is still in it is said, pid by
    /// pid -- here because the killer never kills.
    #[test]
    fn a_group_that_does_not_empty_is_reported_with_what_is_in_it() {
        let (mut anchor, mut member) = group_with_a_member();
        let group = anchor.id();
        let said = clear(&mut anchor, "false", LISTER, Duration::from_millis(300));
        let running = runs(member.id());
        member.kill().unwrap();
        member.wait().unwrap();
        assert_eq!(
            said.as_deref(),
            Some(
                format!(
                    "process group {group}: {} still running after SIGKILL",
                    member.id()
                )
                .as_str()
            )
        );
        assert!(running);
        assert!(
            anchor.try_wait().unwrap().is_some(),
            "collected all the same"
        );
    }

    /// A process list that cannot be had proves nothing: reported, never
    /// taken for an empty group.
    #[test]
    fn a_group_that_cannot_be_checked_is_reported_not_taken_as_empty() {
        let (mut anchor, mut member) = group_with_a_member();
        let said = clear(&mut anchor, KILLER, "false", Duration::from_millis(300)).unwrap();
        assert!(said.contains("could not be checked"), "{said}");
        // The killer did run: the member is dead, just not seen to be.
        member.wait().unwrap();
    }

    #[test]
    fn the_process_list_counts_what_can_run_and_refuses_what_it_cannot_read() {
        let listing =
            "    1     1 Ss\n  500   500 Z\n  501   500 R+\n  502   500 Z+\n  503   600 S\n";
        assert_eq!(
            running_in(listing, 500),
            Ok(vec![501]),
            "not the anchor, no zombie"
        );
        assert_eq!(running_in(listing, 700), Ok(vec![]));
        assert!(running_in("", 500).is_err());
        assert!(running_in("  501   500\n", 500).is_err());
        assert!(running_in("  x   500 S\n", 500).is_err());
    }

    #[test]
    fn closing_hands_over_what_earlier_closes_left_and_only_once() {
        let (home, root) = project("left");
        let relay = relay(&home, &root);
        relay
            .left
            .lock()
            .unwrap()
            .push("MCP server db (process group 9: 12 still running after SIGKILL)".into());
        assert_eq!(
            relay.close_all(),
            ["MCP server db (process group 9: 12 still running after SIGKILL)"]
        );
        assert!(relay.close_all().is_empty());
    }
}

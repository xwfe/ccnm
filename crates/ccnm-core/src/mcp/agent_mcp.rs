//! MCP servers installed on the Agent Node, for a remote session (P50,
//! toexec v4 plan step 4).
//!
//! `ccnm internal agent-skills`, the small server Claude Code or Codex
//! start on the Agent Node next to ccnm's (P48), gets two more tools:
//! `call_mcp_tool` -- the Runtime's own (P49) in shape and arguments -- over
//! the servers this account installed for Claude Code (`~/.claude.json`) or
//! Codex (`~/.codex/config.toml`), and `read_mcp_result` for a result too
//! long to return at once. The steps of a call are the Runtime's
//! ([`relay::call`]); what is here is what differs on this machine.
//!
//! # Why through ccnm and not in the session's MCP config
//!
//! Measured with a fake model (toexec `evidence/v4-mcp/agent-mcp/`):
//! Claude Code 2.1.278 saves any MCP result over about 50 000 characters to
//! this machine's disk and gives the model a 2 KB preview, to be read with
//! `Read` -- which a remote session does not have; Codex keeps 12 KB of it
//! (top-level tools) or 40 KB (Code Mode). And every tool of every server
//! would sit in every request: Playwright's are 21 KB. Here a result is
//! paged ([`INLINE_BYTES`] at a time, the rest kept in memory), and the
//! model sees one tool with the servers' names.
//!
//! # Which servers
//!
//! A server at an address on another machine is offered unless hidden. One
//! that runs on this machine -- a program, or an address here -- only when
//! `[agent_mcp] local` names it: it can read this disk and run things as
//! this account, which a remote session was built not to do (config doc of
//! [`crate::config::AgentMcp`]). Whether the session gets any is the
//! workspace's call (`agent_tools`, `mcp_servers`), made on the Runtime and
//! carried to [`crate::session::create`].
//!
//! # Environment
//!
//! A program gets what this server got from the client that started it,
//! minus the Agent's own login (`ANTHROPIC_API_KEY`, `CODEX_HOME`, ...) and
//! `SSH_AUTH_SOCK`, plus the `env` its config gives it. Unlike on the
//! Runtime, a credential-looking name is not stripped and `${VAR}` looks
//! everything up: these configs are the account's own, not a project's,
//! and a GitHub server started natively gets `GITHUB_TOKEN` too. Codex hands
//! an MCP server only some of its environment, so under Codex a server that
//! needs another variable reports it missing.
//!
//! Deleting the feature: this file and `curl.rs`, the two tools and the
//! `mcp` half of the payload in `agent_skills.rs`, `AgentTool::McpServers`
//! and `[agent_mcp]` in the config, and the last argument of
//! `session::create`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use toexec_mcp::child::ChildTransport;
use toexec_mcp::installed::{self, Installed, Kind, Server, Transport as Config};
use toexec_mcp::kept::{self, Kept};
use toexec_mcp::pool::Pool;
use toexec_mcp::{Open, Transport};

use crate::config::AgentMcp;
use crate::error::{Error, Result};
use crate::mcp::curl::Curl;
use crate::mcp::relay::{self, CallMcpToolArgs, INLINE_BYTES, Side, Words};
use crate::process::Cmd;

/// The second tool's name.
pub const READ_TOOL: &str = "read_mcp_result";

/// How long a long result can be read on, and how much is kept: one
/// session's server, so one caller.
const KEEP: kept::Limits = kept::Limits {
    keep_for: Duration::from_secs(30 * 60),
    max_item: 16 * 1024 * 1024,
    max_total: 64 * 1024 * 1024,
};
/// How often idle servers are looked for (idle means five minutes unused).
const SWEEP_EVERY: Duration = Duration::from_secs(60);
const INTRO: &str = "Use an MCP server installed on the machine you run on. That is not the project machine: ccnm's call_mcp_tool has the servers there, and when both have a server of the same name, the one next to the project is that one. Without server: the servers and their state. With server: its tools, their input schemas and its instructions (this starts or connects to it). With server, tool and arguments: call that tool. A long result continues with read_mcp_result.";

const AGENT: Words = Words {
    heading: "MCP servers installed on the machine you run on (not the project machine):",
    nothing: "No MCP server is installed on the machine you run on for Claude Code or Codex.",
    none_installed: "none is installed on the machine you run on",
    next: "Call call_mcp_tool with server=<name> to see its tools and their input schemas (that starts or connects to it), then with tool and arguments to call one.",
};

/// What this machine's `[agent_mcp]` says, as the session's server gets it
/// in its payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPayload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// `$CODEX_HOME` when the session was created, `None` for `~/.codex`.
    /// Carried because Codex starts this server with the session's private
    /// home in its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_home: Option<PathBuf>,
}

impl McpPayload {
    pub fn of(config: &AgentMcp) -> McpPayload {
        McpPayload {
            local: config.local.iter().cloned().collect(),
            hidden: config.hidden.iter().cloned().collect(),
            codex_home: std::env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// Arguments of `read_mcp_result`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadMcpResultArgs {
    /// The `ref` a call_mcp_tool result named.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Byte offset to read from, as that result said. Default 0.
    #[serde(default)]
    pub offset: Option<u64>,
}

/// The installed servers, for one session.
pub struct Relay {
    payload: McpPayload,
    home: PathBuf,
    pool: Arc<Pool>,
    kept: Kept,
    /// The usable servers when the session started, for the description
    /// (a client keeps `tools/list` for the connection). A call reads the
    /// files again.
    catalog: Vec<String>,
    /// What closing a server could not clear. Nothing on this machine holds
    /// a write guard, so the warning [`relay::start`] logs is all there is
    /// to do about it; this is only emptied.
    left: relay::Left,
}

impl Relay {
    pub fn new(payload: &McpPayload, home: &Path) -> Relay {
        let pool = Arc::new(Pool::new());
        Pool::sweep(&pool, SWEEP_EVERY);
        let mut relay = Relay {
            payload: payload.clone(),
            home: home.to_path_buf(),
            pool,
            kept: Kept::new(KEEP),
            catalog: Vec::new(),
            left: relay::Left::default(),
        };
        relay.catalog = relay
            .read()
            .servers
            .iter()
            .filter(|server| unusable(server, &relay.payload.local).is_none())
            .map(|server| server.name.clone())
            .collect();
        relay
    }

    /// Whether the tools are worth offering: at least one server is.
    pub fn offered(&self) -> bool {
        !self.catalog.is_empty()
    }

    pub fn description(&self) -> String {
        relay::describe(INTRO, &self.catalog)
    }

    /// Stop every server this session started, each with whatever it left
    /// in its process group ([`relay::start`]).
    pub fn close_all(&self) {
        self.pool.close_all();
        self.left
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn read(&self) -> Installed {
        let places = installed::Places::new(&self.home, self.payload.codex_home.as_deref());
        let mut found = installed::read(&places, &|name| std::env::var(name).ok());
        found
            .servers
            .retain(|server| !self.payload.hidden.contains(&server.name));
        found
    }

    /// One call. Blocking: run it off the async thread.
    pub fn call(&self, args: &CallMcpToolArgs) -> Result<CallToolResult> {
        let installed = self.read();
        let opener = Opener {
            home: &self.home,
            left: &self.left,
        };
        let local = &self.payload.local;
        let keep = |whole: &str, end: usize| -> Result<String> {
            let stored = self.kept.put("", whole.to_string());
            let mut note = format!(
                "[the result is {} bytes; this is bytes 0-{end}. Read on with {READ_TOOL} ref={} offset={end}. Kept for {} minutes.",
                stored.size,
                stored.reference,
                KEEP.keep_for.as_secs() / 60
            );
            if stored.kept < stored.size {
                note.push_str(&format!(
                    " Only the first {} bytes were kept; ask the tool for less to see the rest.",
                    stored.kept
                ));
            }
            note.push(']');
            Ok(note)
        };
        relay::call(
            &Side {
                installed: &installed,
                pool: &self.pool,
                opener: &opener,
                unusable: &|server: &Server| unusable(server, local),
                words: &AGENT,
                notes: &[],
                keep: &keep,
            },
            args,
        )
    }

    /// The next part of a kept result.
    pub fn read_result(&self, args: &ReadMcpResultArgs) -> Result<CallToolResult> {
        let Some(text) = self.kept.get("", &args.reference) else {
            return Err(Error::invalid_args(format!(
                "no kept result {}: results are kept for {} minutes of this session; call the tool again",
                args.reference,
                KEEP.keep_for.as_secs() / 60
            )));
        };
        let offset = args.offset.unwrap_or(0) as usize;
        let Some((start, end)) = kept::page(&text, offset, INLINE_BYTES) else {
            return Err(Error::invalid_args(format!(
                "offset {offset} is past the end ({} bytes)",
                text.len()
            )));
        };
        let note = if end < text.len() {
            format!(
                "[bytes {start}-{end} of {}. Next: {READ_TOOL} ref={} offset={end}]",
                text.len(),
                args.reference
            )
        } else {
            format!("[bytes {start}-{end} of {}; that is the end]", text.len())
        };
        Ok(CallToolResult::success(vec![
            ContentBlock::text(text[start..end].to_string()),
            ContentBlock::text(note),
        ]))
    }
}

/// Why this server is not offered here, if it is not.
fn unusable(server: &Server, local: &[String]) -> Option<String> {
    if server.off_in_source {
        return Some(format!("it is turned off in {}", server.source.file()));
    }
    if !server.missing_env.is_empty() {
        return Some(format!(
            "its config uses {}, which this session's server does not have",
            server.missing_env.join(", ")
        ));
    }
    if matches!(server.transport, Config::Sse { .. }) {
        return Some(
            "it uses the old HTTP+SSE transport, which is deprecated; streamable HTTP is relayed"
                .to_string(),
        );
    }
    let named = local.contains(&server.name);
    match server.kind() {
        Kind::RemoteUrl => None,
        _ if named => None,
        Kind::LocalProcess => Some(
            "it runs as a program on this machine, which ccnm offers only when this machine's [agent_mcp] local names it".to_string(),
        ),
        Kind::LocalUrl => Some(
            "its address is on this machine, which ccnm offers only when this machine's [agent_mcp] local names it".to_string(),
        ),
    }
}

/// How a server starts here: a program in this account's home, or curl.
struct Opener<'a> {
    home: &'a Path,
    left: &'a relay::Left,
}

impl Open for Opener<'_> {
    fn open(&self, server: &Server) -> std::result::Result<Box<dyn Transport>, toexec_mcp::Error> {
        let (command, args, env, cwd) = match &server.transport {
            Config::Http { url, headers } => return Ok(Box::new(Curl::new(url, headers)?)),
            Config::Sse { .. } => {
                return Err(toexec_mcp::Error::Start(
                    "the old HTTP+SSE transport is not relayed".into(),
                ));
            }
            Config::Stdio {
                command,
                args,
                env,
                cwd,
            } => (command, args, env, cwd),
        };
        // The home, not the session's state directory the client runs in.
        let cwd = cwd
            .as_ref()
            .map(|dir| self.home.join(dir))
            .unwrap_or_else(|| self.home.to_path_buf());
        let mut cmd = Cmd::new(command).args(args).cwd(&cwd);
        for (name, _) in std::env::vars_os() {
            if crate::safety::environment::agent_private(&name)
                || ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "SSH_AUTH_SOCK"]
                    .iter()
                    .any(|n| name == *n)
            {
                cmd = cmd.env_remove(name);
            }
        }
        // After the removals, so what the config names reaches the server.
        for (name, value) in env {
            cmd = cmd.env(name, value);
        }
        let mut process = cmd.process();
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, stop) =
            relay::start(&mut process, &server.name, self.left).map_err(|error| {
                toexec_mcp::Error::Start(if error.kind() == std::io::ErrorKind::NotFound {
                    format!("`{command}` is not on the PATH this session's server has")
                } else {
                    format!("cannot start `{command}`: {error}")
                })
            })?;
        Ok(Box::new(ChildTransport::new(child, stop)?))
    }

    fn me(&self) -> (&str, &str) {
        ("ccnm", crate::VERSION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn temp(name: &str) -> TestDir {
        let dir =
            std::env::temp_dir().join(format!("ccnm-agent-mcp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    /// A stdio server in sh: `echo` answers, `big` writes 70 000 bytes,
    /// `env` shows two variables.
    const SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"here","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}},{"name":"big","inputSchema":{"type":"object"}},{"name":"env","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"name":"big"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$(head -c 70000 /dev/zero | tr '\0' 'x')" ;;
    *'"name":"env"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"token=%s agent=%s"}]}}\n' "$id" "$SOME_API_TOKEN" "$ANTHROPIC_API_KEY" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}]}}\n' "$id" ;;
  esac
done
"#;

    /// A home with the sh server installed for Claude Code under two names,
    /// a URL server for Codex, one at a local address, and one Codex has
    /// turned off.
    fn home(name: &str) -> TestDir {
        let home = temp(name);
        let script = home.join("here-mcp.sh");
        fs::write(&script, SERVER).unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            home.join(".claude.json"),
            format!(
                r#"{{ "mcpServers": {{
                    "here": {{ "command": "{0}", "env": {{ "SOME_API_TOKEN": "t0k", "ANTHROPIC_API_KEY": "mine" }} }},
                    "other": {{ "command": "{0}" }},
                    "pencil": {{ "type": "http", "url": "http://127.0.0.1:9/mcp" }}
                }} }}"#,
                script.display()
            ),
        )
        .unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "[mcp_servers.web]\nurl = \"https://example.invalid/mcp\"\n\n\
             [mcp_servers.off]\nurl = \"https://example.invalid/off\"\nenabled = false\n",
        )
        .unwrap();
        home
    }

    fn payload(local: &[&str], hidden: &[&str]) -> McpPayload {
        McpPayload {
            local: local.iter().map(|s| s.to_string()).collect(),
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
            codex_home: None,
        }
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
    fn by_default_only_a_remote_url_is_offered_and_the_rest_say_why() {
        let h = home("default");
        let relay = Relay::new(&payload(&[], &[]), &h);
        assert!(relay.offered());
        let description = relay.description();
        assert!(description.ends_with("Servers here: web."), "{description}");
        assert!(description.starts_with("Use an MCP server installed on the machine you run on."));

        let overview = texts(&relay.call(&args(None, None)).unwrap()).join("\n");
        for line in [
            "- here (~/.claude.json): not relayed: it runs as a program on this machine",
            "- pencil (~/.claude.json): not relayed: its address is on this machine",
            "- web (~/.codex/config.toml): not started",
            "- off (~/.codex/config.toml): not relayed: it is turned off in ~/.codex/config.toml",
        ] {
            assert!(overview.contains(line), "{line} not in {overview}");
        }
        let err = relay.call(&args(Some("here"), None)).unwrap_err();
        assert!(err.to_string().contains("[agent_mcp] local"), "{err}");
    }

    #[test]
    fn a_named_local_server_answers_with_its_env_and_without_the_agents_inherited_login() {
        let h = home("local");
        let relay = Relay::new(&payload(&["here"], &["other"]), &h);
        assert!(relay.description().ends_with("Servers here: here, web."));
        let listed = texts(&relay.call(&args(Some("here"), None)).unwrap()).join("\n");
        assert!(
            listed.starts_with("[MCP server here (here 1): 3 tool(s)]"),
            "{listed}"
        );
        assert_eq!(
            texts(&relay.call(&args(Some("here"), Some("echo"))).unwrap()),
            ["echoed"]
        );
        // The account's own config gives it both; nothing inherited adds to
        // that (the test process has no such login, and would lose it).
        assert_eq!(
            texts(&relay.call(&args(Some("here"), Some("env"))).unwrap()),
            ["token=t0k agent=mine"]
        );
        let hidden = relay.call(&args(Some("other"), None)).unwrap_err();
        assert!(
            hidden.to_string().contains("no MCP server other here"),
            "{hidden}"
        );
        relay.close_all();
    }

    #[test]
    fn a_long_result_is_paged_with_read_mcp_result() {
        let h = home("long");
        let relay = Relay::new(&payload(&["here"], &[]), &h);
        let first = texts(&relay.call(&args(Some("here"), Some("big"))).unwrap());
        assert_eq!(first[0].len(), INLINE_BYTES);
        assert!(
            first[1].contains("the result is 70000 bytes"),
            "{}",
            first[1]
        );
        let reference = first[1]
            .split("ref=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_string();
        let mut whole = first[0].clone();
        let mut offset = INLINE_BYTES as u64;
        loop {
            let page = texts(
                &relay
                    .read_result(&ReadMcpResultArgs {
                        reference: reference.clone(),
                        offset: Some(offset),
                    })
                    .unwrap(),
            );
            whole.push_str(&page[0]);
            offset += page[0].len() as u64;
            if page[1].contains("that is the end") {
                break;
            }
        }
        assert_eq!(whole, "x".repeat(70_000));
        let gone = relay
            .read_result(&ReadMcpResultArgs {
                reference: "r-nothing".into(),
                offset: None,
            })
            .unwrap_err();
        assert!(gone.to_string().contains("no kept result"), "{gone}");
        let past = relay
            .read_result(&ReadMcpResultArgs {
                reference,
                offset: Some(70_001),
            })
            .unwrap_err();
        assert!(past.to_string().contains("past the end"), "{past}");
        relay.close_all();
    }

    /// A local server here closes the way the Runtime's do (C51-01): what
    /// it left in its process group goes with it. Checked on this side on
    /// its own -- the Runtime's tests do not start this `Opener`.
    #[test]
    fn a_local_servers_helper_goes_with_it() {
        use crate::mcp::relay::tests::{runs, wait_for_pid};
        let h = home("leaving");
        let script = h.join("leaving-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\nsleep 60 </dev/null >/dev/null 2>&1 &\necho $! > child.pid\nexec ./here-mcp.sh\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            h.join(".claude.json"),
            format!(
                r#"{{ "mcpServers": {{ "leaving": {{ "command": "{}" }} }} }}"#,
                script.display()
            ),
        )
        .unwrap();
        let relay = Relay::new(&payload(&["leaving"], &[]), &h);
        relay.call(&args(Some("leaving"), None)).unwrap();
        let child = wait_for_pid(&h.join("child.pid"));
        assert!(runs(child));
        relay.close_all();
        assert!(!runs(child));
    }

    #[test]
    fn nothing_installed_is_said_plainly() {
        let h = temp("empty");
        let relay = Relay::new(&payload(&[], &[]), &h);
        assert!(!relay.offered());
        assert_eq!(
            texts(&relay.call(&args(None, None)).unwrap()),
            ["No MCP server is installed on the machine you run on for Claude Code or Codex."]
        );
    }
}

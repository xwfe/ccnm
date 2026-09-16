//! The Remote Workspace MCP entry, through the real binary.
//!
//! Everything here speaks newline-delimited JSON-RPC to a real
//! `ccnm internal mcp-serve` over pipes, exactly as the bridge's ssh does.
//! What only this file can prove is that the mode a workspace granted
//! actually reaches the tools: that a read session is not merely *listed*
//! as read-only but refuses a write tool it is asked for anyway, that a
//! coding session still holds the workspace's single write lock, and that a
//! workspace which never opted in cannot be opened at all.
//!
//! No ssh is dialled and no Agent is started: the bridge's own half is one
//! command, covered by the unit tests in `ccnm-core`; this is the far end of
//! it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use ccnm_core::protocol::payload;
use ccnm_core::runtime::{ExternalMode, ExternalOpenPayload};
use serde_json::{Value, json};

/// One test's own machine: a config, a project, and a state directory the
/// write guard lives in.
struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    root: PathBuf,
}

impl Fixture {
    /// `access` and `instructions` go into the workspace `demo`. A second
    /// workspace, `private`, never mentions external MCP.
    fn new(test: &str, access: &str, instructions: &str) -> Fixture {
        Fixture::build(test, access, instructions, false)
    }

    /// Same, with `exec_command` allowed despite an unconfined runtime.
    /// Only for the cases that must really run a command.
    fn unconfined(test: &str, access: &str) -> Fixture {
        Fixture::build(test, access, "generic", true)
    }

    /// A read-mode workspace whose Runtime home holds a Claude login: the
    /// two things ccnm exists to keep apart are the same account. `waived`
    /// writes the one switch that accepts that.
    fn shared_home(test: &str, waived: bool) -> Fixture {
        let fixture = Fixture::build(test, "read", "generic", false);
        let claude = fixture.dir.join("home/.claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join(".credentials.json"), "{}\n").unwrap();
        if waived {
            let config = std::fs::read_to_string(&fixture.config).unwrap().replace(
                "external_mcp = \"read\"",
                "external_mcp = \"read\"\nallow_unisolated_credentials = true",
            );
            std::fs::write(&fixture.config, config).unwrap();
        }
        fixture
    }

    fn build(test: &str, access: &str, instructions: &str, unconfined: bool) -> Fixture {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-external-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        // Its own tree: two workspaces over one working tree is refused by
        // the write guard, and that refusal is not what these tests are about.
        std::fs::create_dir_all(dir.join("other")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::write(root.join("hello.txt"), "one\ntwo\n").unwrap();
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-node.invalid"

# Open to external clients, and managed by an Agent as well: the same
# working tree reachable through both entries is what P11 is about.
[workspaces.demo]
root = "{}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "{access}"
external_instructions = "{instructions}"
allow_unconfined_exec = {unconfined}

# An ordinary managed workspace that never mentions external MCP. It is a
# real, valid workspace here -- which is the point: being reachable is not
# permission to open it.
[workspaces.private]
root = "{}"
agent_node = "agent"
"#,
                root.display(),
                dir.join("other").display()
            ),
        )
        .unwrap();
        Fixture { dir, config, root }
    }

    fn wire(&self, workspace: &str, mode: ExternalMode, session: &str) -> String {
        payload::encode(&ExternalOpenPayload::new(workspace, session, mode)).unwrap()
    }

    fn serve(&self, wire: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", "mcp-serve", "--payload", wire])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", &self.config);
        cmd
    }

    /// Start a session and finish the MCP handshake.
    fn open(&self, workspace: &str, mode: ExternalMode, session: &str) -> Session {
        Session::start(self.serve(&self.wire(workspace, mode, session)))
    }

    /// Run one to completion instead of talking to it: for the cases that
    /// are refused before `initialize` and therefore have no session.
    fn refused(&self, workspace: &str, mode: ExternalMode) -> Output {
        self.serve(&self.wire(workspace, mode, "bridge-refused"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A live MCP session over pipes.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
    instructions: String,
}

impl Session {
    fn start(mut command: Command) -> Session {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut session = Session {
            child,
            stdin,
            stdout,
            id: 0,
            instructions: String::new(),
        };
        let hello = session.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "external-host-test", "version": "0"}
            }),
        );
        session.instructions = hello["instructions"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        session.notify("notifications/initialized");
        session
    }

    fn notify(&mut self, method: &str) {
        writeln!(self.stdin, r#"{{"jsonrpc":"2.0","method":"{method}"}}"#).unwrap();
        self.stdin.flush().unwrap();
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "server closed stdout while waiting for {method}");
            // Nothing but protocol on stdout: a stray println! here would
            // corrupt the stream a Host is reading.
            let message: Value = serde_json::from_str(line.trim())
                .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC: {line:?} ({e})"));
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(
                    message.get("error").is_none(),
                    "{method} came back as a protocol error: {message}"
                );
                return message["result"].clone();
            }
        }
    }

    fn tools(&mut self) -> Vec<String> {
        let listed = self.rpc("tools/list", json!({}));
        listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": tool, "arguments": args}))
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_string()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const READ_TOOLS: [&str; 4] = ["workspace_info", "read_file", "list_files", "search_text"];
const WITHHELD: [&str; 3] = ["exec_command", "apply_patch", "read_output"];

/// The read session's whole surface: four tools, and the ones it does not
/// have are not merely missing from the list.
#[test]
fn a_read_session_offers_four_tools_and_refuses_the_rest() {
    let fixture = Fixture::new("read", "read", "generic");
    let mut session = fixture.open("demo", ExternalMode::Read, "bridge-read");

    let mut tools = session.tools();
    tools.sort();
    let mut expected = READ_TOOLS.to_vec();
    expected.sort();
    assert_eq!(tools, expected);

    // The read tools work, so the mode is a boundary and not a broken
    // session that happens to refuse everything.
    let read = session.call("read_file", json!({"path": "hello.txt"}));
    assert!(!is_error(&read), "{read}");
    assert!(text(&read).contains("one"), "{}", text(&read));

    // tools/list not naming a tool is a hint, and a Host may ignore hints.
    // This is the part that is not a hint.
    // Valid arguments on purpose: a malformed call would be refused by the
    // parameter check before the mode gate ever ran, which would prove
    // nothing about the gate.
    for (tool, args) in [
        ("exec_command", json!({"cmd": ["/bin/echo", "hi"]})),
        (
            "apply_patch",
            json!({"files": [{"op": "add", "path": "new.txt", "content": "x\n"}]}),
        ),
        ("read_output", json!({"output_ref": "r-0000"})),
    ] {
        let refused = session.call(tool, args);
        assert!(is_error(&refused), "{tool} was not refused: {refused}");
        let said = text(&refused);
        assert!(said.starts_with("CCNM_E_POLICY:"), "{tool}: {said}");
        assert!(said.contains("read mode"), "{tool}: {said}");
    }
    session.shutdown();
}

/// Coding gets all seven, and `apply_patch` really writes: the same tool
/// implementation the managed path uses, reached through the other entry.
#[test]
fn a_coding_session_gets_every_tool_and_can_write() {
    let fixture = Fixture::new("coding", "coding", "generic");
    let mut session = fixture.open("demo", ExternalMode::Coding, "bridge-coding");
    let tools = session.tools();
    assert_eq!(tools.len(), 7, "{tools:?}");
    for tool in WITHHELD {
        assert!(
            tools.contains(&tool.to_string()),
            "{tool} missing: {tools:?}"
        );
    }
    let applied = session.call(
        "apply_patch",
        json!({"files": [{"op": "add", "path": "new.txt", "content": "written\n"}]}),
    );
    assert!(!is_error(&applied), "{}", text(&applied));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("new.txt")).unwrap(),
        "written\n"
    );
    session.shutdown();
}

/// The hard one: an external coding session competes for the same write
/// guard as everything else on that working tree, and a read session does
/// not take it at all.
#[test]
fn coding_holds_the_write_guard_and_read_does_not() {
    let fixture = Fixture::new("guard", "coding", "generic");
    let holder = fixture.open("demo", ExternalMode::Coding, "bridge-holder");

    let second = fixture.refused("demo", ExternalMode::Coding);
    assert!(!second.status.success());
    let said = stderr(&second);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("write guard"), "{said}");

    // A read session opens beside it: it changes nothing, so making it wait
    // for a writer would cost availability and protect nothing.
    let reader = fixture.open("demo", ExternalMode::Read, "bridge-reader");
    reader.shutdown();

    holder.shutdown();
    // And once the holder is gone the guard is free again.
    let after = fixture.open("demo", ExternalMode::Coding, "bridge-after");
    after.shutdown();
}

/// A workspace that never said `external_mcp` is not openable, and neither
/// is one that does not exist. Same code, same words apart from the name
/// the caller itself sent.
#[test]
fn a_workspace_without_the_opt_in_cannot_be_opened() {
    let fixture = Fixture::new("optin", "read", "generic");
    let closed = fixture.refused("private", ExternalMode::Read);
    let missing = fixture.refused("no-such-workspace", ExternalMode::Read);
    for out in [&closed, &missing] {
        assert!(!out.status.success());
        let said = stderr(out);
        assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
        assert!(said.contains("not available to external MCP"), "{said}");
        // Nothing on stdout: a Host must not see half a handshake.
        assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    }
    assert_eq!(
        stderr(&closed).replace("private", "<name>"),
        stderr(&missing).replace("no-such-workspace", "<name>")
    );
}

/// Asking for more than the workspace allows fails at startup rather than
/// arriving as a session whose tools quietly differ from what was asked.
#[test]
fn coding_on_a_read_workspace_is_refused_not_downgraded() {
    let fixture = Fixture::new("escalate", "read", "generic");
    let out = fixture.refused("demo", ExternalMode::Coding);
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("read mode"), "{said}");
    assert!(out.stdout.is_empty());
}

/// The default handshake says which workspace this is and that the session
/// cannot write — and does **not** carry the project's own file.
#[test]
fn generic_instructions_describe_the_mode_and_project_nothing() {
    let fixture = Fixture::new("generic", "read", "generic");
    std::fs::write(fixture.root.join("AGENTS.md"), "project rules here\n").unwrap();
    let session = fixture.open("demo", ExternalMode::Read, "bridge-generic");
    assert!(
        session.instructions.contains("demo"),
        "{}",
        session.instructions
    );
    assert!(
        session.instructions.contains("read-only"),
        "{}",
        session.instructions
    );
    assert!(
        !session.instructions.contains("project rules here"),
        "generic must not project the file: {}",
        session.instructions
    );
    session.shutdown();
}

/// `project` hands over the project's own instructions, choosing by a fixed
/// order rather than by anything the client said about itself.
#[test]
fn project_instructions_prefer_agents_md_over_claude_md() {
    let fixture = Fixture::new("project", "coding", "project");
    std::fs::write(fixture.root.join("CLAUDE.md"), "claude rules\n").unwrap();
    std::fs::write(fixture.root.join("AGENTS.md"), "agents rules\n").unwrap();
    let session = fixture.open("demo", ExternalMode::Coding, "bridge-project");
    assert!(
        session.instructions.contains("agents rules"),
        "{}",
        session.instructions
    );
    assert!(
        !session.instructions.contains("claude rules"),
        "only the first file that exists: {}",
        session.instructions
    );
    assert!(
        session
            .instructions
            .contains("[project instructions: AGENTS.md"),
        "{}",
        session.instructions
    );
    session.shutdown();
}

/// A bridge does not know its Host, so a long project file is cut to what
/// the strictest known one keeps: Claude Code's 2048 UTF-16 code units.
/// Before P13 this handshake was 4600 of them with the marker on the last
/// line, and Claude Code cut exactly that line off.
#[test]
fn a_long_project_file_is_cut_to_what_claude_code_keeps_marker_first() {
    let fixture = Fixture::new("project-long", "read", "project");
    let body = "- 每条规则都写在根目录的说明文件里，写得很长。\n".repeat(300);
    std::fs::write(fixture.root.join("AGENTS.md"), &body).unwrap();
    let session = fixture.open("demo", ExternalMode::Read, "bridge-project-long");
    let text = &session.instructions;
    let units = text.encode_utf16().count();
    assert!(units <= 2048, "{units} UTF-16 code units");
    // Not wasted either: the file is what fills the rest.
    assert!(units > 2000, "{units} UTF-16 code units");
    let marker = text
        .find(&format!(
            "\n[project instructions: AGENTS.md, {} bytes, first ",
            body.len()
        ))
        .unwrap_or_else(|| panic!("{text}"));
    let file = text.find("--- AGENTS.md from the workspace root").unwrap();
    assert!(marker < file, "{text}");
    assert!(text.contains("read_file AGENTS.md for the rest"), "{text}");
    session.shutdown();
}

/// An external client is never told a person is standing by: this server
/// cannot know, and a Host that believes it may skip its own approval.
#[test]
fn an_external_tool_list_never_claims_someone_can_approve() {
    let fixture = Fixture::new("meta", "coding", "generic");
    let mut session = fixture.open("demo", ExternalMode::Coding, "bridge-meta");
    let listed = session.rpc("tools/list", json!({}));
    let exec = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "exec_command")
        .unwrap()
        .clone();
    assert!(
        exec.get("_meta").is_none(),
        "external tools carry no interaction claim: {exec}"
    );
    session.shutdown();
}

/// The annotations a Host uses to decide what to ask about. They are
/// published accurately and they are only hints: the refusals above happen
/// whether or not anybody reads these.
#[test]
fn every_tool_publishes_its_annotations() {
    let fixture = Fixture::new("annotations", "coding", "generic");
    let mut session = fixture.open("demo", ExternalMode::Coding, "bridge-annotations");
    let listed = session.rpc("tools/list", json!({}));
    let by_name: std::collections::HashMap<String, Value> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap().to_string(), t.clone()))
        .collect();

    for tool in READ_TOOLS.iter().chain(["read_output"].iter()) {
        let annotations = &by_name[*tool]["annotations"];
        assert_eq!(annotations["readOnlyHint"], json!(true), "{tool}");
        assert_eq!(annotations["openWorldHint"], json!(false), "{tool}");
    }

    let patch = &by_name["apply_patch"]["annotations"];
    assert_eq!(patch["readOnlyHint"], json!(false));
    assert_eq!(patch["destructiveHint"], json!(true));
    // It can only change files in this workspace.
    assert_eq!(patch["openWorldHint"], json!(false));

    // A command is open-world whatever this particular one looks like.
    let exec = &by_name["exec_command"]["annotations"];
    assert_eq!(exec["readOnlyHint"], json!(false));
    assert_eq!(exec["destructiveHint"], json!(true));
    assert_eq!(exec["openWorldHint"], json!(true));
    session.shutdown();
}

/// The published tool tables say what this server actually serves: the
/// same tools, the same descriptions, the same argument names.
///
/// `scripts/check_protocol.py` cannot catch any of that. It validates the
/// fixtures against a hand-written JSON Schema, and that schema calls
/// `inputSchema` an object and stops there — so a fixture and the schema
/// stay happily consistent with each other while both drift away from the
/// binary. That is how `apply_patch` came to publish `changes` for an
/// argument the wire has always called `files`: a Host written from the
/// fixture sends `{"changes": [...]}` and every call is refused.
///
/// This test is the half that reads the code. What it compares:
///
/// - the set of tool names,
/// - each tool's `description`, byte for byte — it is written by hand in
///   `#[tool(description = ...)]` and copied into the fixture, and it is
///   the text the model on the other side actually reads,
/// - each tool's argument names and which of them are required.
///
/// What it deliberately does not compare is inside each argument: types,
/// bounds, `format`, per-argument descriptions. Those are what `schemars`
/// derives from the Rust types, so pinning them would turn a crate upgrade
/// into a protocol failure. The fixtures' `$note` draws the same line.
///
/// There is no flag to rewrite the fixtures from a live server. Changing a
/// description means editing the fixture by hand, which is the point:
/// `AGENTS.md` does not allow re-recording a golden fixture to make a test
/// pass. The failure prints both strings, so it is a copy away.
#[test]
fn published_tool_tables_match_the_running_server() {
    for (mode, access, fixture_file) in [
        (ExternalMode::Read, "read", "tools-list-read.json"),
        (ExternalMode::Coding, "coding", "tools-list-coding.json"),
    ] {
        let published = fixture_tools(fixture_file);
        let fixture = Fixture::new(&format!("published-{access}"), access, "generic");
        let mut session = fixture.open("demo", mode, "bridge-published");
        let listed = session.rpc("tools/list", json!({}));
        let served = listed["tools"].as_array().unwrap();

        let mut served_names: Vec<&str> = served.iter().map(name_of).collect();
        let mut published_names: Vec<&str> = published.iter().map(name_of).collect();
        served_names.sort_unstable();
        published_names.sort_unstable();
        assert_eq!(
            served_names, published_names,
            "{fixture_file} lists different tools than a {access} session serves"
        );

        for tool in served {
            let tool_name = name_of(tool);
            let mine = published
                .iter()
                .find(|t| name_of(t) == tool_name)
                .unwrap_or_else(|| panic!("{fixture_file} has no {tool_name}"));
            assert_eq!(
                description(mine),
                description(tool),
                "{fixture_file}: {tool_name} publishes a description this server does not serve"
            );
            assert_eq!(
                arguments(mine),
                arguments(tool),
                "{fixture_file}: {tool_name} publishes different argument names than it takes"
            );
            assert_eq!(
                required(mine),
                required(tool),
                "{fixture_file}: {tool_name} publishes a different required set"
            );
        }
        session.shutdown();
    }
}

/// The tools of one published `tools/list` fixture.
fn fixture_tools(file: &str) -> Vec<Value> {
    // From `crates/ccnm-cli` up to the repository root. The fixtures are
    // part of the frozen contract, so they are read where they are
    // published rather than copied next to this test.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/protocol/fixtures-mcp")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc: Value = serde_json::from_str(&text).unwrap();
    doc["message"]["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{file} has no message.result.tools"))
        .clone()
}

fn name_of(tool: &Value) -> &str {
    tool["name"].as_str().unwrap()
}

/// The sentence a model reads before deciding to call the tool. Missing is
/// not the same as empty: a tool with no description at all would be a
/// different kind of wrong, so this panics rather than comparing `""`.
fn description(tool: &Value) -> &str {
    tool["description"]
        .as_str()
        .unwrap_or_else(|| panic!("{}: no description", name_of(tool)))
}

/// One tool's argument names, sorted. A tool that takes none has an empty
/// `properties`, which is not the same as having no schema at all.
fn arguments(tool: &Value) -> Vec<String> {
    let mut names: Vec<String> = tool["inputSchema"]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{}: inputSchema has no properties", name_of(tool)))
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// Which of them are required, sorted. Absent means none.
fn required(tool: &Value) -> Vec<String> {
    let mut names: Vec<String> = tool["inputSchema"]["required"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A path outside the workspace is refused the same way it is on the
/// managed path: the tools are the same code, and the boundary is not a
/// property of who opened the session.
#[test]
fn the_path_policy_is_the_same_through_this_entry() {
    let fixture = Fixture::new("paths", "read", "generic");
    let mut session = fixture.open("demo", ExternalMode::Read, "bridge-paths");
    let refused = session.call("read_file", json!({"path": "../config.toml"}));
    assert!(is_error(&refused), "{refused}");
    let said = text(&refused);
    assert!(said.starts_with("CCNM_E_"), "{said}");
    assert!(
        !said.contains(&fixture.dir.display().to_string()),
        "no absolute path of this machine may appear: {said}"
    );
    session.shutdown();
}

/// EOF ends the session: the Host closing the bridge takes the server with
/// it, and nothing is left holding the workspace.
#[test]
fn closing_stdin_ends_the_session_and_frees_the_workspace() {
    let fixture = Fixture::new("eof", "coding", "generic");
    let session = fixture.open("demo", ExternalMode::Coding, "bridge-eof");
    session.shutdown();
    let again = fixture.open("demo", ExternalMode::Coding, "bridge-eof-2");
    again.shutdown();
    assert!(Path::new(&fixture.root).is_dir());
}

// -- 整条链路：bridge 造的那条命令真的能起来 ------------------------------
//
// 下面这些用一个假 ssh 代替真的：它跳过 `-o` 选项、`-T` 和别名，把剩下的
// argv 当成命令执行——远端 sshd 做的正是这件事。证明的是 bridge 造出来的
// 命令行本身可用，以及远端失败怎么传回来；**不证明任何真实 ssh 或真实
// Host 的行为**，那是 P11/P12。

/// The fake `ssh`, and a config whose node points at it.
struct Chain {
    fixture: Fixture,
    ssh: PathBuf,
}

impl Chain {
    fn new(test: &str, access: &str, remote_ccnm: Option<&str>) -> Chain {
        let fixture = Fixture::new(test, access, "generic");
        let ssh = fixture.dir.join("fake-ssh");
        std::fs::write(
            &ssh,
            r#"#!/bin/sh
# 跳过选项，取出别名，把剩下的当命令执行。真 sshd 就是这样收到它的。
while [ $# -gt 0 ]; do
  case "$1" in
    -o) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
alias=$1
shift
if [ "$alias" = "unreachable.invalid" ]; then
  echo "ssh: connect to host $alias port 22: Operation timed out" >&2
  exit 255
fi
exec "$@"
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The bridge's own config: which alias, and which ccnm on the far
        // side. The workspace list stays on the Runtime, as always.
        let ccnm_bin = remote_ccnm
            .map(str::to_string)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_ccnm").to_string());
        std::fs::write(
            fixture.dir.join("bridge.toml"),
            format!(
                r#"
this = "laptop"
[nodes.laptop]
[nodes.runtime]
ssh = "runtime-alias"
ccnm_bin = "{ccnm_bin}"
[nodes.gone]
ssh = "unreachable.invalid"
ccnm_bin = "{ccnm_bin}"
"#
            ),
        )
        .unwrap();
        Chain { fixture, ssh }
    }

    /// The command `ccnm mcp bridge` would exec, with the fake ssh in place
    /// of the real one.
    fn command(&self, node: &str, mode: ExternalMode) -> Command {
        let config = ccnm_core::Config::load(&self.fixture.dir.join("bridge.toml")).unwrap();
        let request = ccnm_core::mcp::bridge::Request {
            workspace: "demo".into(),
            node: Some(node.into()),
            mode,
        };
        let built = ccnm_core::mcp::bridge::command(&config, &request, "bridge-chain").unwrap();
        let mut cmd = Command::new(&self.ssh);
        cmd.args(&built.args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.fixture.dir.join("home"))
            .env("XDG_STATE_HOME", self.fixture.dir.join("state"))
            .env("CCNM_CONFIG", &self.fixture.config);
        cmd
    }
}

/// The whole chain: the argv the bridge builds, through a transport, into a
/// real server that answers MCP.
#[test]
fn the_command_the_bridge_builds_opens_a_working_session() {
    let chain = Chain::new("chain", "read", None);
    let mut session = Session::start(chain.command("runtime", ExternalMode::Read));
    let mut tools = session.tools();
    tools.sort();
    let mut expected = READ_TOOLS.to_vec();
    expected.sort();
    assert_eq!(tools, expected);
    let read = session.call("read_file", json!({"path": "hello.txt"}));
    assert!(!is_error(&read), "{read}");
    session.shutdown();
}

/// The transport cannot reach the other machine. ssh's own exit code and
/// message come back; nothing on stdout, so a Host sees a server that did
/// not start rather than a broken stream.
#[test]
fn an_unreachable_runtime_fails_without_a_handshake() {
    let chain = Chain::new("unreachable", "read", None);
    let out = chain
        .command("gone", ExternalMode::Read)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(stderr(&out).contains("connect to host"), "{}", stderr(&out));
}

/// A far side that does not know protocol 5 — an older ccnm — must stop
/// with its own diagnostic, not be read as anything else.
#[test]
fn an_old_ccnm_on_the_far_side_says_so() {
    let dir = std::env::temp_dir().join(format!("ccnm-old-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old-ccnm");
    std::fs::write(
        &old,
        "#!/bin/sh\nprintf 'CCNM_E_VERSION:\\nserve payload is protocol 5; this ccnm serves 1..=4\\n' >&2\nexit 4\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();

    let chain = Chain::new("old-peer", "read", Some(&old.display().to_string()));
    let out = chain
        .command("runtime", ExternalMode::Read)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let said = stderr(&out);
    assert!(said.starts_with("CCNM_E_VERSION:"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rubbish on the wire does not take the server down, and does not corrupt
/// the stream: the next real request is still answered.
#[test]
fn a_bad_message_does_not_break_the_stream() {
    let fixture = Fixture::new("badwire", "read", "generic");
    let mut session = fixture.open("demo", ExternalMode::Read, "bridge-badwire");
    // Not JSON; then JSON that is not a request; then one megabyte of it.
    writeln!(session.stdin, "this is not json at all").unwrap();
    writeln!(session.stdin, r#"{{"jsonrpc":"2.0","id":99}}"#).unwrap();
    writeln!(
        session.stdin,
        r#"{{"jsonrpc":"2.0","id":98,"method":"x","params":{{"pad":"{}"}}}}"#,
        "A".repeat(1024 * 1024)
    )
    .unwrap();
    session.stdin.flush().unwrap();
    let info = session.call("workspace_info", json!({}));
    assert!(!is_error(&info), "{info}");
    session.shutdown();
}

/// A killed session is EOF to the client, and the workspace is **not**
/// handed to the next writer: the guard was left held by a process nobody
/// watched die, and children of it may still be running. This is the same
/// rule the managed path has — the next coding session waits for a person,
/// while a read session, which takes no guard, opens normally.
#[test]
fn a_killed_session_is_eof_and_does_not_hand_over_the_guard() {
    let fixture = Fixture::new("dead", "coding", "generic");
    let mut session = fixture.open("demo", ExternalMode::Coding, "bridge-dead");
    session.child.kill().unwrap();
    session.child.wait().unwrap();
    let mut line = String::new();
    assert_eq!(
        session.stdout.read_line(&mut line).unwrap(),
        0,
        "a client reads EOF, not a half message: {line:?}"
    );

    let next = fixture.refused("demo", ExternalMode::Coding);
    assert!(!next.status.success());
    let said = stderr(&next);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("interrupted"), "{said}");

    // Reading is still fine: it never wanted the guard.
    let reader = fixture.open("demo", ExternalMode::Read, "bridge-dead-read");
    reader.shutdown();
}

// -- P11：两个入口，一棵工作树 --------------------------------------------
//
// 这一段证明的不是"外部入口能用"，而是"它没有把第一个入口已经建立的边界撑
// 大"。真实 Host 的允许矩阵仍然是没做的那半边，见 status.json 的 blocker。

impl Fixture {
    /// A managed open: the shape `ccnm run` sends (internal protocol 4).
    fn managed(&self, session: &str) -> Command {
        let identity = ccnm_core::instance::AgentIdentity {
            node: "agent".into(),
            instance: "claude-main".into(),
            provider: ccnm_core::provider::AgentProvider::Claude,
            profile_ref: "default".into(),
        };
        let wire = payload::encode(&ccnm_core::runtime::OpenPayload::new(
            "demo", identity, session,
        ))
        .unwrap();
        self.serve(&wire)
    }
}

/// The hard condition for two entries to coexist: they take the **same**
/// lock on the same working tree. Whichever gets there first, the other
/// waits — and "waits" means it does not start, rather than starting and
/// writing anyway.
#[test]
fn a_managed_session_and_an_external_one_take_the_same_guard() {
    let fixture = Fixture::new("crossguard", "coding", "generic");

    // Managed first.
    let managed = Session::start(fixture.managed("s-managed-1"));
    let external = fixture.refused("demo", ExternalMode::Coding);
    assert!(!external.status.success());
    let said = stderr(&external);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("write guard"), "{said}");
    managed.shutdown();

    // External first, managed second: same answer, other direction.
    let holder = fixture.open("demo", ExternalMode::Coding, "bridge-cross");
    let blocked = fixture
        .managed("s-managed-2")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    let said = stderr(&blocked);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("write guard"), "{said}");
    holder.shutdown();
}

/// A read session takes no guard, so it is not a way to stall a managed
/// writer — and not a way to sneak a writer in beside one either, because
/// it has no tool that writes.
#[test]
fn a_read_session_coexists_with_a_managed_writer() {
    let fixture = Fixture::new("crossread", "read", "generic");
    let managed = Session::start(fixture.managed("s-managed-read"));
    let mut reader = fixture.open("demo", ExternalMode::Read, "bridge-beside");
    let read = reader.call("read_file", json!({"path": "hello.txt"}));
    assert!(!is_error(&read), "{read}");
    assert_eq!(reader.tools().len(), 4);
    reader.shutdown();
    managed.shutdown();
}

/// Annotations are hints; the Runtime is the gate. This checks both halves:
/// that the hints are *accurate* (readOnlyHint is true for exactly the tools
/// a read session gets), and that a Host which ignores them entirely gains
/// nothing by calling what it was not offered.
#[test]
fn a_host_that_ignores_annotations_gains_nothing() {
    let fixture = Fixture::new("ignore", "coding", "generic");
    let mut coding = fixture.open("demo", ExternalMode::Coding, "bridge-hints");
    let listed = coding.rpc("tools/list", json!({}));
    let read_only: Vec<String> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == json!(true))
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    coding.shutdown();

    // What a read session is actually given.
    let mut reader = fixture.open("demo", ExternalMode::Read, "bridge-hints-read");
    let offered = reader.tools();

    // The hint is accurate for every tool but one, and that one is
    // deliberate: read_output says readOnlyHint -- it reads -- yet a read
    // session does not get it, because its references can only come from a
    // tool that session does not have.
    let mut hinted: Vec<String> = read_only
        .iter()
        .filter(|name| name.as_str() != "read_output")
        .cloned()
        .collect();
    hinted.sort();
    let mut given = offered.clone();
    given.sort();
    assert_eq!(hinted, given);

    // Now behave like a Host that never read any of it.
    for (tool, args) in [
        ("exec_command", json!({"cmd": ["/bin/echo", "hi"]})),
        (
            "apply_patch",
            json!({"files": [{"op": "add", "path": "sneaked.txt", "content": "x\n"}]}),
        ),
    ] {
        let refused = reader.call(tool, args);
        assert!(is_error(&refused), "{tool}: {refused}");
        assert!(text(&refused).starts_with("CCNM_E_POLICY:"), "{tool}");
    }
    assert!(
        !fixture.root.join("sneaked.txt").exists(),
        "a refused write must not have happened"
    );
    reader.shutdown();
}

/// Retained output belongs to the session that produced it. Another
/// session's reference resolves to nothing, whichever entry it came in
/// through -- an output_ref is not a handle on the machine.
#[test]
fn an_output_ref_does_not_cross_sessions() {
    let fixture = Fixture::unconfined("outputs", "coding");
    let mut first = fixture.open("demo", ExternalMode::Coding, "bridge-out-1");
    let ran = first.call("exec_command", json!({"cmd": ["/bin/echo", "hello"]}));
    assert!(!is_error(&ran), "{}", text(&ran));
    let said = text(&ran);
    let marker = "output_ref ";
    let start = said
        .find(marker)
        .unwrap_or_else(|| panic!("no ref in {said:?}"))
        + marker.len();
    let reference: String = said[start..]
        .split([',', ']', ' ', '\n'])
        .next()
        .unwrap()
        .to_string();
    first.shutdown();

    let mut second = fixture.open("demo", ExternalMode::Coding, "bridge-out-2");
    let borrowed = second.call("read_output", json!({"output_ref": reference}));
    assert!(is_error(&borrowed), "{borrowed}");
    assert!(
        text(&borrowed).starts_with("CCNM_E_"),
        "{}",
        text(&borrowed)
    );
    second.shutdown();
}

/// The credential boundary is not a property of the managed entry. A
/// Runtime process holding something that looks like an Agent credential
/// does not serve an external client either.
#[test]
fn agent_credentials_stop_the_external_entry_too() {
    let fixture = Fixture::new("credentials", "read", "generic");
    let out = fixture
        .serve(&fixture.wire("demo", ExternalMode::Read, "bridge-creds"))
        .env("ANTHROPIC_API_KEY", "not-a-real-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no handshake may happen");
    let said = stderr(&out);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    // The finding names the check, never the value.
    assert!(said.contains("authentication environment"), "{said}");
    assert!(
        !said.contains("not-a-real-key"),
        "the value must not appear"
    );
}

/// The boundary refusal is real, but it has to describe itself.
///
/// This session is refused before `initialize`, and it has no
/// `exec_command` to refuse -- a read session is four read-only tools.
/// Saying "exec_command is refused", listing confinement rows the gate
/// never read, and closing with "set allow_unconfined_exec = true" sent a
/// real operator to sign an opt-in named after unrestricted command
/// execution, in order to open a read-only link. It would not even have
/// worked: that switch waives nothing this gate reads.
#[test]
fn a_refused_read_session_names_only_the_switch_that_opens_it() {
    let fixture = Fixture::shared_home("creds-read-message", false);
    let out = fixture.refused("demo", ExternalMode::Read);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no handshake may happen");

    let said = stderr(&out);
    assert!(said.starts_with("CCNM_E_POLICY:"), "{said}");
    assert!(said.contains("No Claude credential"), "{said}");
    assert!(said.contains("allow_unisolated_credentials"), "{said}");
    assert!(!said.contains("exec_command"), "{said}");
    assert!(!said.contains("set allow_unconfined_exec = true"), "{said}");
    // The rows this gate does not read stay out of it. They are still real
    // and `ccnm doctor` still shows them; they are not why this failed.
    for unread in ["Not an admin", "No SSH keys", "No sudo", "Runtime user"] {
        assert!(!said.contains(unread), "{unread} is not a reason: {said}");
    }
}

/// And the switch the refusal names is the whole fix: one line, matching
/// the one admission actually being made. Nothing here needs
/// `allow_unconfined_exec`, because nothing here runs a command.
#[test]
fn the_credential_switch_alone_opens_a_read_session() {
    let fixture = Fixture::shared_home("creds-read-waived", true);
    let mut session = fixture.open("demo", ExternalMode::Read, "bridge-waived");

    let mut tools = session.tools();
    tools.sort();
    let mut expected = READ_TOOLS.to_vec();
    expected.sort();
    assert_eq!(tools, expected);

    let read = session.call("read_file", json!({"path": "hello.txt"}));
    assert!(!is_error(&read), "{}", text(&read));
    session.shutdown();
}

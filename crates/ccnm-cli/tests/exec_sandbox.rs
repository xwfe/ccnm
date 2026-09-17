//! `exec_sandbox = "codex"` through the real binary (P33): every
//! `exec_command` of the workspace runs behind `codex sandbox`.
//!
//! The Codex here is `tests/fixtures/fake_codex_sandbox.py`, which does no
//! sandboxing but records what ccnm handed it -- the state JSON, the argv,
//! the CODEX_HOME -- and runs the command with `FAKE_SANDBOXED=1` so the
//! command can prove it ran behind the wrapper. What the real sandbox
//! refuses is measured in toexec `evidence/v2-p/p33-sandbox`; one test here
//! checks the two headline refusals against the real thing, and only when
//! `CCNM_TEST_CODEX_BIN` points at Codex 0.154.0.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use ccnm_core::protocol::payload;
use ccnm_core::runtime::{ExternalMode, ExternalOpenPayload};
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    outside: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Fixture {
    /// The fake Codex, unless `bin` says otherwise; `codex_bin` left out of
    /// the node config when `named` is false.
    fn build(test: &str, bin: Option<&Path>, named: bool) -> Fixture {
        let fake = repo().join("tests/fixtures/fake_codex_sandbox.py");
        let bin = bin.unwrap_or(&fake).canonicalize().unwrap();
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-exec-sandbox-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("project");
        let outside = dir.join("outside");
        for d in [
            &root,
            &outside,
            &dir.join("bare"),
            &dir.join("home"),
            &dir.join("state"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let codex_bin = if named {
            format!("codex_bin = \"{}\"", bin.display())
        } else {
            String::new()
        };
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
this = "runtime"

[nodes.runtime]
{codex_bin}

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{}"
agent = {{ node = "agent", instance = "claude-main" }}
allow_unconfined_exec = true
external_mcp = "coding"
exec_sandbox = "codex"

[workspaces.bare]
root = "{}"
agent = {{ node = "agent", instance = "claude-main" }}
allow_unconfined_exec = true
external_mcp = "coding"
"#,
                root.display(),
                dir.join("bare").display(),
            ),
        )
        .unwrap();
        Fixture {
            log: dir.join("sandbox.log"),
            dir,
            root,
            outside,
            config,
        }
    }

    fn serve(&self, workspace: &str, session: &str) -> Command {
        let wire = payload::encode(&ExternalOpenPayload::new(
            workspace,
            session,
            ExternalMode::Coding,
        ))
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", "mcp-serve", "--payload", &wire])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", &self.config)
            .env("FAKE_SANDBOX_LOG", &self.log);
        cmd
    }

    fn open(&self, workspace: &str, session: &str) -> Session {
        Session::start(self.serve(workspace, session))
    }

    fn refused(&self, mut cmd: Command) -> Output {
        cmd.stdin(Stdio::null()).output().unwrap()
    }

    /// What the fake Codex was asked to sandbox, one entry per command.
    fn sandboxed(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn uri(&self, path: &Path) -> String {
        format!("file://{}", path.display())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One `internal mcp-serve`, spoken to over MCP the way an external Host
/// would (the same client as `external_mcp.rs`).
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
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
        };
        session.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "exec-sandbox-test", "version": "0"}
            }),
        );
        writeln!(
            session.stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        session.stdin.flush().unwrap();
        session
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

    fn exec(&mut self, cmd: &[&str]) -> Value {
        self.exec_in(cmd, None)
    }

    fn exec_in(&mut self, cmd: &[&str], cwd: Option<&str>) -> Value {
        let mut args = json!({"cmd": cmd});
        if let Some(cwd) = cwd {
            args["cwd"] = json!(cwd);
        }
        self.rpc(
            "tools/call",
            json!({"name": "exec_command", "arguments": args}),
        )
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

/// The measured profile, straight from the P21 capture: what the fake
/// Codex must have been handed.
fn captured_profile() -> Value {
    let text = std::fs::read_to_string(
        repo().join("tests/fixtures/codex-0.154.0/exec-server/process-start-workspace-write.json"),
    )
    .unwrap();
    let message: Value = serde_json::from_str(&text).unwrap();
    message["params"]["sandbox"]["permissions"].clone()
}

/// Off is the default, and off means no wrapper at all: the same node
/// names a Codex, and the bare workspace never uses it.
#[test]
fn a_workspace_without_the_switch_runs_commands_bare() {
    let fx = Fixture::build("bare", None, true);
    let mut s = fx.open("bare", "bare-1");
    let ran = s.exec(&["sh", "-c", "echo ${FAKE_SANDBOXED:-bare}"]);
    assert!(!is_error(&ran), "{ran}");
    let out = text(&ran);
    assert!(out.contains("--- stdout\nbare\n"), "{out}");
    assert!(!out.contains("sandboxed"), "{out}");
    s.shutdown();
    assert!(fx.sandboxed().is_empty());
}

/// Every command of the workspace goes behind the wrapper, with the
/// measured profile, the root as the only writable root, the command's
/// own cwd, a private CODEX_HOME under ccnm's state, and a result that
/// says so.
#[test]
fn the_switch_wraps_every_command_with_the_measured_profile() {
    let fx = Fixture::build("wraps", None, true);
    std::fs::create_dir_all(fx.root.join("src")).unwrap();
    let mut s = fx.open("demo", "wraps-1");

    let ran = s.exec(&["sh", "-c", "echo ${FAKE_SANDBOXED:-bare}; echo $CODEX_HOME"]);
    assert!(!is_error(&ran), "{ran}");
    let out = text(&ran);
    assert!(
        out.starts_with("$ sh -c"),
        "the command line is the command, not the wrapper: {out}"
    );
    assert!(
        out.contains("--- stdout\n1\n"),
        "ran behind the wrapper: {out}"
    );
    assert!(out.contains("[sandboxed:"), "{out}");
    assert!(
        out.contains("Operation not permitted"),
        "the note names the refusal text: {out}"
    );

    let inner = s.exec_in(&["pwd"], Some("src"));
    assert!(!is_error(&inner), "{inner}");
    let failed = s.exec(&["sh", "-c", "exit 3"]);
    assert!(
        !is_error(&failed),
        "a failing command is a result: {failed}"
    );
    assert!(text(&failed).contains("exit 3 in"), "{}", text(&failed));
    s.shutdown();

    let seen = fx.sandboxed();
    assert_eq!(seen.len(), 3, "{seen:?}");
    let root = fx.root.canonicalize().unwrap();
    for entry in &seen {
        assert_eq!(entry["state"]["permissionProfile"], captured_profile());
        assert_eq!(entry["state"]["workspaceRoots"], json!([fx.uri(&root)]));
        let home = entry["codex_home"].as_str().unwrap();
        assert!(
            home.starts_with(fx.dir.join("state").to_str().unwrap()),
            "CODEX_HOME under ccnm's state: {home}"
        );
        assert!(home.contains("/exec-sandbox/"), "{home}");
        assert!(
            !Path::new(home).exists(),
            "the CODEX_HOME goes with the server: {home}"
        );
    }
    assert_eq!(
        seen[0]["argv"],
        json!(["sh", "-c", "echo ${FAKE_SANDBOXED:-bare}; echo $CODEX_HOME"])
    );
    assert_eq!(seen[0]["state"]["sandboxCwd"], json!(fx.uri(&root)));
    assert_eq!(seen[1]["argv"], json!(["pwd"]));
    assert_eq!(
        seen[1]["state"]["sandboxCwd"],
        json!(fx.uri(&root.join("src")))
    );
    assert_eq!(seen[1]["cwd"], json!(root.join("src").to_str().unwrap()));
}

/// A program that is not there is a dependency error, as it is bare -- not
/// the sandbox launcher's exit 71 dressed up as a command result.
#[test]
fn a_missing_program_is_still_a_dependency_error_not_a_result() {
    let fx = Fixture::build("missing", None, true);
    let mut s = fx.open("demo", "missing-1");
    let ran = s.exec(&["/nonexistent/program", "x"]);
    assert!(is_error(&ran), "{ran}");
    let out = text(&ran);
    assert!(out.contains("CCNM_E_DEPENDENCY"), "{out}");
    assert!(
        out.contains("/nonexistent/program is not installed"),
        "{out}"
    );
    let ran = s.exec(&["./no-such.sh"]);
    assert!(is_error(&ran), "{ran}");
    assert!(
        text(&ran).contains("./no-such.sh is not installed"),
        "{}",
        text(&ran)
    );
    s.shutdown();
    assert!(fx.sandboxed().is_empty(), "nothing reached the wrapper");
}

/// The switch on a Runtime that cannot provide the sandbox refuses the
/// session at startup -- no `codex_bin`, or the wrong Codex -- instead of
/// running commands bare.
#[test]
fn a_runtime_that_cannot_provide_the_sandbox_refuses_the_session() {
    let fx = Fixture::build("nobin", None, false);
    let out = fx.refused(fx.serve("demo", "nobin-1"));
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.contains("CCNM_E_CONFIG"), "{said}");
    assert!(said.contains("codex_bin"), "{said}");
    assert!(said.contains("exec_sandbox"), "{said}");
    // The bare workspace on the same Runtime is unaffected.
    fx.open("bare", "nobin-2").shutdown();

    let fx = Fixture::build("version", None, true);
    let mut cmd = fx.serve("demo", "version-1");
    cmd.env("FAKE_CODEX_VERSION", "codex-cli 0.155.0");
    let out = fx.refused(cmd);
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.contains("CCNM_E_VERSION"), "{said}");
    assert!(said.contains("0.155.0"), "{said}");
    assert!(said.contains("0.154.0"), "{said}");
}

/// Against Codex 0.154.0 itself, when `CCNM_TEST_CODEX_BIN` names it: the
/// two refusals a coding session meets first, and the write that must
/// still work. Skipped, and says so, without it.
#[test]
fn against_the_real_codex_sandbox_when_configured() {
    let Some(bin) = std::env::var_os("CCNM_TEST_CODEX_BIN") else {
        eprintln!("skipped: set CCNM_TEST_CODEX_BIN to a Codex 0.154.0 binary to run this test");
        return;
    };
    let fx = Fixture::build("real", Some(Path::new(&bin)), true);
    let mut s = fx.open("demo", "real-1");

    let inside = s.exec(&["sh", "-c", "echo inside > inside.txt"]);
    assert!(!is_error(&inside), "{inside}");
    assert!(text(&inside).contains("ok in"), "{}", text(&inside));
    assert_eq!(
        std::fs::read_to_string(fx.root.join("inside.txt")).unwrap(),
        "inside\n"
    );

    let escaped = fx.outside.join("escaped.txt");
    let outside = s.exec(&["sh", "-c", &format!("echo x > {}", escaped.display())]);
    assert!(
        !is_error(&outside),
        "a refused write is the command failing, not an error: {outside}"
    );
    let out = text(&outside);
    assert!(out.contains("exit 1 in"), "{out}");
    assert!(out.contains("Operation not permitted"), "{out}");
    assert!(!escaped.exists());

    let network = s.exec(&[
        "python3",
        "-c",
        "import socket; s = socket.socket(); s.connect(('127.0.0.1', 9))",
    ]);
    assert!(!is_error(&network), "{network}");
    let out = text(&network);
    assert!(
        out.contains("Operation not permitted"),
        "refused by the sandbox, not by the port: {out}"
    );
    s.shutdown();
}

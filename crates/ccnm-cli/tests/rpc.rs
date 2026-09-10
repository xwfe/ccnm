//! Runs the real `ccnm rpc` binary and talks to it over pipes, the way an
//! external program would.
//!
//! The unit tests in `ccnm-core` exercise the dispatcher directly; what only
//! this file can prove is the part a client actually depends on: that the
//! process starts, that **stdout carries nothing but protocol**, that logs
//! stay on stderr, and that EOF ends it cleanly.
//!
//! Nothing here starts an Agent or dials ssh. Every call either fails before
//! the transport or is answered from configuration alone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"integration/1","protocol_versions":["ccnm.machine/1"]}}"#;

fn sandbox(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ccnm-rpc-it-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Pipe `input` through `ccnm rpc` and collect what comes back.
///
/// The environment is cleared so the developer's own config and state can
/// never take part in a test.
fn talk(test: &str, config: &Path, input: &str) -> Output {
    let home = sandbox(test);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ccnm"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("USER", std::env::var_os("USER").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .arg("--config")
        .arg(config)
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ccnm rpc must start");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input.as_bytes())
        .expect("the server must accept input");
    // Dropping stdin above is the EOF the server waits for.
    child.wait_with_output().expect("ccnm rpc must finish")
}

fn lines(out: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout must be protocol JSON, got {line:?}: {e}"))
        })
        .collect()
}

#[test]
fn a_conversation_over_pipes_works_end_to_end() {
    let out = talk(
        "conversation",
        &fixture("agent-instance/runtime.toml"),
        &format!("{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\"}}\n"),
    );
    assert!(out.status.success(), "{:?}", out.status);
    let lines = lines(&out);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["result"]["protocol"], "ccnm.machine/1");
    assert_eq!(lines[0]["result"]["capabilities"]["modes"][0], "print");
    // The Runtime Node knows the binding and not the provider; see the
    // protocol notes on why that field is absent rather than guessed.
    let agent = &lines[1]["result"]["agents"][0];
    assert_eq!(agent["node"], "worker");
    assert_eq!(agent["instance"], "claude-main");
    assert_eq!(agent["workspaces"][0], "demo");
    assert!(agent.get("provider").is_none(), "{agent}");
}

#[test]
fn stdout_carries_only_protocol_even_when_logging_is_on() {
    // The one guarantee a client cannot work around: a stray line on stdout
    // desynchronises the stream. Debug logging is the likeliest source, so
    // it is turned all the way up here.
    let home = sandbox("logging");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ccnm"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("USER", std::env::var_os("USER").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("CCNM_LOG", "debug")
        .arg("--verbose")
        .arg("--config")
        .arg(fixture("agent-instance/runtime.toml"))
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        // A notification: the spec forbids answering it, so the server has
        // to say so somewhere -- and that somewhere must be stderr.
        .write_all(
            format!("{{\"jsonrpc\":\"2.0\",\"method\":\"hello\",\"params\":{{}}}}\n{HELLO}\n")
                .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let lines = lines(&out);
    assert_eq!(lines.len(), 1, "the notification must not be answered");
    for line in &lines {
        assert_eq!(line["jsonrpc"], "2.0");
        assert!(line.get("result").is_some() || line.get("error").is_some());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("notification"), "stderr was {stderr:?}");
}

#[test]
fn eof_on_an_empty_stream_exits_cleanly() {
    let out = talk("eof", &fixture("agent-instance/runtime.toml"), "");
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn a_broken_line_does_not_desync_the_stream() {
    // Newline framing earns its keep here: one unusable line is answered and
    // the next real request is still understood.
    let input = format!("not json\n[]\n{HELLO}\n");
    let out = talk("desync", &fixture("agent-instance/runtime.toml"), &input);
    let lines = lines(&out);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[0]["id"], Value::Null);
    assert_eq!(lines[1]["error"]["code"], -32600);
    assert_eq!(lines[1]["id"], Value::Null);
    assert_eq!(lines[2]["result"]["protocol"], "ccnm.machine/1");
}

#[test]
fn an_oversize_line_is_refused_and_the_stream_continues() {
    let huge = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{{\"client\":\"{}\"}}}}",
        "x".repeat(1024 * 1024)
    );
    let out = talk(
        "oversize",
        &fixture("agent-instance/runtime.toml"),
        &format!("{huge}\n{HELLO}\n"),
    );
    let lines = lines(&out);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["data"]["reason"], "oversize");
    assert_eq!(lines[1]["result"]["protocol"], "ccnm.machine/1");
}

#[test]
fn nothing_can_be_called_before_the_handshake() {
    let out = talk(
        "handshake",
        &fixture("agent-instance/runtime.toml"),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agents.list\"}\n",
    );
    assert_eq!(lines(&out)[0]["error"]["code"], -32014);
}

#[test]
fn a_missing_config_is_reported_as_a_config_error() {
    let out = talk(
        "noconfig",
        Path::new("/nonexistent/ccnm/config.toml"),
        &format!("{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\"}}\n"),
    );
    let lines = lines(&out);
    assert_eq!(lines[1]["error"]["code"], -32001);
    assert_eq!(lines[1]["error"]["data"]["ccnm_code"], "CCNM_E_CONFIG");
    assert_eq!(lines[1]["error"]["data"]["effect"], "none");
}

#[test]
fn a_start_that_cannot_reach_the_agent_still_leaves_a_session_to_ask_about() {
    // The workspace root does not exist here, so the launcher refuses before
    // any ssh. What matters is that the failure is recorded: an accepted
    // start that reports an error but leaves no session is the one outcome a
    // caller cannot act on.
    // A separate directory: `talk` wipes its own sandbox, and writing the
    // config into that one would delete it before the server starts.
    let config = sandbox("failed-start-config").join("config.toml");
    std::fs::write(
        &config,
        r#"
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker.invalid"
[workspaces.demo]
root = "/nonexistent/project"
agent = { node = "worker", instance = "claude-main" }
"#,
    )
    .unwrap();
    let start = r#"{"jsonrpc":"2.0","id":2,"method":"session.start","params":{"workspace":"demo","mode":"print","input":{"prompt":"go"}}}"#;
    let out = talk("failed-start", &config, &format!("{HELLO}\n{start}\n"));
    let first = lines(&out);
    let session = first[1]["result"]["session"]
        .as_str()
        .expect("start must answer with a handle");
    assert!(session.starts_with("s-"));

    // Ask again on a second connection, which is also the reconnect path.
    let status = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"session.status","params":{{"session":"{session}"}}}}"#
    );
    let out = talk_reusing("failed-start", &config, &format!("{HELLO}\n{status}\n"));
    let state = lines(&out)[1]["result"]["state"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ["starting", "running", "failed", "unknown"].contains(&state.as_str()),
        "unexpected state {state}"
    );
}

/// Same sandbox as a previous call, so the session store persists across
/// what looks to the server like two separate clients.
fn talk_reusing(test: &str, config: &Path, input: &str) -> Output {
    let home = std::env::temp_dir().join(format!("ccnm-rpc-it-{}-{test}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_ccnm"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("USER", std::env::var_os("USER").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .arg("--config")
        .arg(config)
        .arg("rpc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

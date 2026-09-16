//! `ccnm internal exec-serve` through the real binary (P22).
//!
//! The executor is `tests/fixtures/fake_exec_server.py`, which ignores every
//! sandbox and really writes files and starts processes. That is the point:
//! whatever reaches it happens, so "ccnm refused this" is proven by what is
//! *not* on disk and not in its log, not by trusting a reply. The requests
//! are the ones Codex 0.154.0 really sent (tests/fixtures/codex-0.154.0/
//! exec-server, captured in P21).
//!
//! With `CCNM_TEST_CODEX_BIN` pointing at a Codex 0.154.0 binary, one more
//! test runs the same checks against the real executor; without it that test
//! says it was skipped. CI has no Codex.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use ccnm_core::instance::AgentIdentity;
use ccnm_core::protocol::payload;
use ccnm_core::provider::AgentProvider;
use ccnm_core::runtime::{ExternalMode, ExternalOpenPayload, NativeOpenPayload, OpenPayload};
use serde_json::{Value, json};

const MARKER: &str = "CCNM_EXEC_SESSION";

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
    fn build(test: &str) -> Fixture {
        let bin = repo().join("tests/fixtures/fake_exec_server.py");
        Self::with_bin(test, &bin, true)
    }

    fn with_bin(test: &str, bin: &Path, opted_in: bool) -> Fixture {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-exec-serve-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("project");
        let outside = dir.join("outside");
        for d in [&root, &outside, &dir.join("home"), &dir.join("state")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
this = "runtime"

[nodes.runtime]
codex_bin = "{}"

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{}"
agent = {{ node = "agent", instance = "codex-main" }}
allow_unconfined_exec = true
codex_exec_server = {opted_in}
external_mcp = "coding"

[workspaces.plain]
root = "{}"
agent = {{ node = "agent", instance = "codex-main" }}
allow_unconfined_exec = true
"#,
                bin.canonicalize().unwrap().display(),
                root.display(),
                dir.join("plain").display(),
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        Fixture {
            log: dir.join("executor.log"),
            dir,
            root,
            outside,
            config,
        }
    }

    fn command(&self, internal: &str, wire: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", internal, "--payload", wire])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", &self.config)
            .env("FAKE_EXEC_LOG", &self.log)
            // An ordinary project variable: it reaches the model's commands.
            // An authentication one (SSH_AUTH_SOCK, *_TOKEN) never gets this
            // far -- the audit refuses the whole session, as it does for MCP.
            .env("CCNM_TEST_PROJECT_VAR", "kept");
        cmd
    }

    fn native_wire(&self, workspace: &str, provider: AgentProvider, session: &str) -> String {
        payload::encode(&NativeOpenPayload::new(
            workspace,
            identity(provider),
            session,
        ))
        .unwrap()
    }

    fn refused(&self, cmd: &mut Command) -> Output {
        cmd.stdin(Stdio::null()).output().unwrap()
    }

    fn open(&self, session: &str) -> Session {
        let wire = self.native_wire("demo", AgentProvider::Codex, session);
        Session::start(self.command("exec-serve", &wire))
    }

    /// What reached the executor.
    fn executor_saw(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// A request Codex 0.154.0 really sent, respelled for this fixture.
    fn captured(&self, name: &str) -> Value {
        let text = std::fs::read_to_string(
            repo()
                .join("tests/fixtures/codex-0.154.0/exec-server")
                .join(name),
        )
        .unwrap()
        .replace("{ROOT}", self.root.to_str().unwrap())
        .replace("{OUTSIDE}", self.outside.to_str().unwrap())
        .replace("{SERVER_HOME}", self.dir.join("home").to_str().unwrap());
        serde_json::from_str(&text).unwrap()
    }

    fn uri(&self, path: &Path) -> String {
        format!("file://{}", path.display())
    }

    /// A captured `process/start`, running `script` in the workspace.
    fn start(&self, id: u64, script: &str) -> Value {
        let mut message = self.captured("process-start-workspace-write.json");
        message["id"] = json!(id);
        message["params"]["processId"] = json!(format!("p{id}"));
        message["params"]["argv"] = json!(["/bin/sh", "-c", script]);
        message
    }

    fn write_file(&self, id: u64, path: &Path, text: &str) -> Value {
        let mut message = self.captured("fs-write-file-workspace-write.json");
        message["id"] = json!(id);
        message["params"]["path"] = json!(self.uri(path));
        message["params"]["dataBase64"] = json!(base64(text));
        message
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn identity(provider: AgentProvider) -> AgentIdentity {
    AgentIdentity {
        node: "agent".into(),
        instance: "codex-main".into(),
        provider,
        profile_ref: "default".into(),
    }
}

/// Standard base64 with padding, which is what `dataBase64` carries. Written
/// out rather than pulled in as a dependency for one test file.
fn base64(text: &str) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n =
            chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One running supervisor, spoken to the way Codex would.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
}

impl Session {
    fn start(mut cmd: Command) -> Session {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        Session {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Send a request and return its reply, skipping the notifications the
    /// real executor sends in between (`process/output` and friends).
    fn call(&mut self, message: &Value) -> Value {
        self.send(message);
        loop {
            let reply = self
                .lines
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("no reply to {message}"));
            if reply.get("id").is_none() && reply.get("method").is_some() {
                continue;
            }
            assert_eq!(reply["id"], message["id"], "{reply}");
            return reply;
        }
    }

    fn handshake(&mut self) {
        let mut init = json!({"id": 1, "method": "initialize", "params": {"clientName": "codex-environment", "resumeSessionId": null}});
        let reply = self.call(&init);
        assert!(reply.get("result").is_some(), "{reply}");
        self.send(&json!({"method": "initialized", "params": {}}));
        init["id"] = json!(0);
    }

    /// Close stdin the way a disconnect does and wait for the supervisor.
    fn close(mut self) -> Output {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        let mut stderr = String::new();
        use std::io::Read;
        let _ = self
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr);
        Output {
            status,
            stdout: Vec::new(),
            stderr: stderr.into_bytes(),
        }
    }
}

fn error_code(reply: &Value) -> i64 {
    reply["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("expected an error, got {reply}"))
}

/// The first things that must hold, all before any executor or guard.
#[test]
fn what_is_refused_before_anything_starts() {
    let fx = Fixture::build("refused");
    let cases = [
        (
            "plain",
            AgentProvider::Codex,
            "CCNM_E_POLICY",
            "does not accept the Codex exec-server chain",
        ),
        (
            "nowhere",
            AgentProvider::Codex,
            "CCNM_E_POLICY",
            "does not accept the Codex exec-server chain",
        ),
        ("demo", AgentProvider::Claude, "CCNM_E_POLICY", "not Codex"),
    ];
    for (workspace, provider, code, text) in cases {
        let wire = fx.native_wire(workspace, provider, "s1");
        let out = fx.refused(&mut fx.command("exec-serve", &wire));
        assert!(!out.status.success(), "{workspace}");
        let said = stderr(&out);
        assert!(said.starts_with(&format!("{code}:")), "{workspace}: {said}");
        assert!(said.contains(text), "{workspace}: {said}");
    }
    // A managed MCP open is another protocol, not a request this command
    // quietly reinterprets.
    let managed = payload::encode(&OpenPayload::new(
        "demo",
        identity(AgentProvider::Codex),
        "s1",
    ))
    .unwrap();
    let said = stderr(&fx.refused(&mut fx.command("exec-serve", &managed)));
    assert!(said.starts_with("CCNM_E_VERSION:"), "{said}");
    // An executor that is not the measured release.
    let said = stderr(
        &fx.refused(
            fx.command(
                "exec-serve",
                &fx.native_wire("demo", AgentProvider::Codex, "s1"),
            )
            .env("FAKE_CODEX_VERSION", "codex-cli 0.155.0"),
        ),
    );
    assert!(said.starts_with("CCNM_E_VERSION:"), "{said}");
    assert!(!fx.log.exists(), "no executor may have been started");
    // None of those took the write guard.
    let mut session = fx.open("after-refusals");
    session.handshake();
    assert!(session.close().status.success());
}

#[test]
fn a_runtime_that_names_no_codex_binary_refuses() {
    let fx = Fixture::build("no-bin");
    let text = std::fs::read_to_string(&fx.config).unwrap();
    let without: String = text
        .lines()
        .filter(|l| !l.starts_with("codex_bin"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&fx.config, without).unwrap();
    let wire = fx.native_wire("demo", AgentProvider::Codex, "s1");
    let said = stderr(&fx.refused(&mut fx.command("exec-serve", &wire)));
    assert!(said.starts_with("CCNM_E_CONFIG:"), "{said}");
    assert!(said.contains("codex_bin"), "{said}");
}

/// Ordinary work passes; everything P21 saw write outside the workspace is
/// stopped before the executor -- proven on the disk and in its log.
#[test]
fn the_rule_table_stands_between_codex_and_the_executor() {
    let fx = Fixture::build("table");
    let mut s = fx.open("table");
    s.handshake();

    // Inside: a command and a patch write, forwarded and really done.
    let inside = fx.root.join("inside.txt");
    let reply = s.call(&fx.start(10, "echo inside > inside.txt; env > env.txt"));
    assert!(reply.get("result").is_some(), "{reply}");
    wait_for("the command's file", || {
        inside.exists() && fx.root.join("env.txt").exists()
    });
    let reply = s.call(&fx.write_file(11, &fx.root.join("patched.txt"), "hello\n"));
    assert!(reply.get("result").is_some(), "{reply}");
    assert_eq!(
        std::fs::read_to_string(fx.root.join("patched.txt")).unwrap(),
        "hello\n"
    );

    // The executor's environment, as the model's command saw it.
    wait_for("env.txt contents", || {
        std::fs::read_to_string(fx.root.join("env.txt")).is_ok_and(|t| t.contains("PATH="))
    });
    let env = std::fs::read_to_string(fx.root.join("env.txt")).unwrap();
    assert!(env.contains(&format!("{MARKER}=table-")), "{env}");
    let codex_home = env
        .lines()
        .find_map(|l| l.strip_prefix("CODEX_HOME="))
        .unwrap()
        .to_string();
    assert!(
        codex_home.starts_with(fx.dir.join("state").to_str().unwrap()),
        "{codex_home}"
    );
    assert!(env.contains("CCNM_TEST_PROJECT_VAR=kept"), "{env}");

    // Outside, the two ways P21 saw it happen once a person approved.
    let escaped = fx.outside.join("escaped.txt");
    let mut escalated = fx.captured("process-start-escalated.json");
    escalated["id"] = json!(20);
    escalated["params"]["argv"] =
        json!(["/bin/sh", "-c", format!("echo x > {}", escaped.display())]);
    assert_eq!(error_code(&s.call(&escalated)), -32600);
    let mut approved = fx.captured("fs-write-file-approved-outside.json");
    approved["id"] = json!(21);
    assert_eq!(error_code(&s.call(&approved)), -32600);
    // Codex's automatic retry without a sandbox.
    let mut retry = fx.write_file(22, &fx.outside.join("retry.txt"), "x");
    retry["params"]["sandbox"] = Value::Null;
    assert_eq!(error_code(&s.call(&retry)), -32600);
    // Network, and the walk above the root (answered by ccnm itself).
    assert_eq!(error_code(&s.call(&json!({"id": 23, "method": "http/request", "params": {"method": "GET", "url": "http://127.0.0.1:9/"}}))), -32600);
    let above = fx.uri(&fx.root.parent().unwrap().join(".git"));
    assert_eq!(error_code(&s.call(&json!({"id": 24, "method": "fs/getMetadata", "params": {"path": above, "sandbox": null}}))), -32004);
    // An unknown notification would close the real executor's connection:
    // dropped, and the session goes on.
    s.send(&json!({"method": "bogus/notify", "params": {}}));
    let reply = s.call(&json!({"id": 25, "method": "fs/getMetadata", "params": {"path": fx.uri(&inside), "sandbox": null}}));
    assert!(reply.get("result").is_some(), "{reply}");

    let out = s.close();
    assert!(out.status.success(), "{}", stderr(&out));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!escaped.exists());
    assert!(!fx.outside.join("patched-outside.txt").exists());
    assert!(!fx.outside.join("retry.txt").exists());
    let saw: Vec<Value> = fx.executor_saw();
    let ids: Vec<i64> = saw.iter().filter_map(|m| m["id"].as_i64()).collect();
    assert_eq!(
        ids,
        vec![1, 10, 11, 25],
        "only the allowed requests reach the executor"
    );
    assert!(!saw.iter().any(|m| m["method"] == "bogus/notify"));
    // A clean end removes the home ccnm made for the executor.
    assert!(!Path::new(&codex_home).exists());
}

#[test]
fn a_message_too_long_ends_the_session_instead_of_reaching_the_executor() {
    let fx = Fixture::build("too-long");
    let mut s = fx.open("too-long");
    s.handshake();
    let huge = "A".repeat(33 * 1024 * 1024);
    let stdin = s.stdin.as_mut().unwrap();
    let _ = stdin.write_all(
        format!(
            "{{\"id\":5,\"method\":\"fs/writeFile\",\"params\":{{\"dataBase64\":\"{huge}\"}}}}\n"
        )
        .as_bytes(),
    );
    let status = s.child.wait().unwrap();
    assert!(status.success());
    assert!(!fx.executor_saw().iter().any(|m| m["id"] == 5));
}

/// One write guard for every way into a working tree.
#[test]
fn the_write_guard_is_shared_with_the_mcp_entries() {
    let fx = Fixture::build("guard");
    let mut first = fx.open("first");
    first.handshake();

    let second = fx.refused(&mut fx.command(
        "exec-serve",
        &fx.native_wire("demo", AgentProvider::Codex, "second"),
    ));
    assert!(
        stderr(&second).contains("write guard is busy"),
        "{}",
        stderr(&second)
    );
    let external = payload::encode(&ExternalOpenPayload::new(
        "demo",
        "ext",
        ExternalMode::Coding,
    ))
    .unwrap();
    let mcp = fx.refused(&mut fx.command("mcp-serve", &external));
    assert!(
        stderr(&mcp).contains("write guard is busy"),
        "{}",
        stderr(&mcp)
    );

    assert!(first.close().status.success());
    let mut third = fx.open("third");
    third.handshake();
    assert!(third.close().status.success());
}

/// exec-server does not kill a process that left its session. The
/// supervisor finds it by marker and kills it before releasing the guard.
#[test]
fn a_process_that_escaped_the_executor_is_killed_before_the_guard_is_released() {
    let fx = Fixture::build("escape");
    let mut s = fx.open("escape");
    s.handshake();
    let pidfile = fx.root.join("daemon.pid");
    let script = "import os, time\nif os.fork() == 0:\n    os.setsid()\n    open('daemon.pid', 'w').write(str(os.getpid()))\n    time.sleep(300)\n";
    let mut start = fx.start(30, "");
    start["params"]["argv"] = json!(["python3", "-c", script]);
    assert!(s.call(&start).get("result").is_some());
    wait_for("the daemon's pid", || {
        std::fs::read_to_string(&pidfile).is_ok_and(|t| !t.is_empty())
    });
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(&pid));

    let out = s.close();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!alive(&pid), "the escaped process outlived the session");
    let mut next = fx.open("next");
    next.handshake();
    assert!(next.close().status.success());
}

/// Guard transfer after a supervisor that died: 20 times, because it is a
/// write-authority fault point (toexec v2 section 9). The guard stays held,
/// the executor still stops, and only an operator clears it.
#[test]
fn a_killed_supervisor_leaves_the_guard_held_twenty_times() {
    let fx = Fixture::build("killed");
    let locks = fx.dir.join("state/ccnm/write-guards");
    for round in 0..20 {
        let mut s = fx.open(&format!("killed-{round}"));
        s.handshake();
        let marker_file = fx.root.join(format!("alive-{round}.txt"));
        let started = s.call(&fx.start(40, &format!("touch {}; sleep 300", marker_file.display())));
        assert!(started.get("result").is_some());
        wait_for("the command", || marker_file.exists());
        s.child.kill().unwrap();
        s.child.wait().unwrap();

        let refused = fx.refused(&mut fx.command(
            "exec-serve",
            &fx.native_wire("demo", AgentProvider::Codex, "after"),
        ));
        let said = stderr(&refused);
        assert!(
            said.contains("left held by an interrupted process"),
            "round {round}: {said}"
        );
        // The executor saw its stdin close and took its processes with it.
        wait_for("the executor's command to stop", || {
            !Command::new("pgrep")
                .args(["-f", &format!("alive-{round}.txt")])
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success()
        });
        // The operator's recovery, as docs/operations.md describes it.
        for entry in std::fs::read_dir(&locks).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
    }
}

/// The executor dying mid-session is not a supervisor crash: the relay
/// ends, the sweep runs, and the guard is released for the next session.
#[test]
fn an_executor_that_dies_releases_the_guard_after_the_sweep_twenty_times() {
    let fx = Fixture::build("crash");
    for round in 0..20 {
        let wire = fx.native_wire("demo", AgentProvider::Codex, &format!("crash-{round}"));
        let mut s = Session::start({
            let mut cmd = fx.command("exec-serve", &wire);
            cmd.env("FAKE_EXEC_CRASH_ON", "fs/readDirectory");
            cmd
        });
        s.handshake();
        s.send(&json!({"id": 50, "method": "fs/readDirectory", "params": {"path": fx.uri(&fx.root), "sandbox": null}}));
        let status = s.child.wait().unwrap();
        assert!(status.success(), "round {round}");
        let mut next = fx.open(&format!("next-{round}"));
        next.handshake();
        assert!(next.close().status.success(), "round {round}");
    }
}

/// The Runtime half of `ccnm doctor`'s probe, reached the way the Agent
/// Node reaches it, minus the network (P27).
///
/// The hop replaced is ssh itself. A Codex Agent's transport is the absolute
/// `/usr/bin/ssh`, which no PATH entry can stand in for, so this runner takes
/// each command the probe would send and runs what the far side would run:
/// the real binary, the verb and payload after the alias, the same stdin and
/// timeout, and this fixture's Runtime environment instead of the test's.
/// `ssh -G` gets a canned answer. Anything else is a call the probe was not
/// expected to make, and fails the test.
struct RuntimeOverSsh<'a> {
    fx: &'a Fixture,
    env: Vec<(&'static str, &'static str)>,
    verbs: std::sync::Mutex<Vec<String>>,
}

impl ccnm_core::ProcessRunner for RuntimeOverSsh<'_> {
    fn run(&self, cmd: &ccnm_core::Cmd) -> ccnm_core::Result<ccnm_core::Output> {
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if args.iter().any(|a| a == "-G") {
            return Ok(ccnm_core::Output::exited(
                0,
                "hostname runtime.invalid\nuser ccrun\nport 22\n",
            ));
        }
        let at = args
            .iter()
            .position(|a| a == "internal")
            .unwrap_or_else(|| panic!("unexpected command: {}", cmd.display()));
        let verb = args[at + 1].clone();
        assert!(
            ["hello", "runtime-audit", "exec-serve"].contains(&verb.as_str()),
            "the probe sent `{verb}`"
        );
        self.verbs.lock().unwrap().push(verb.clone());
        let wire = args.last().unwrap();
        let mut local = ccnm_core::Cmd::new(env!("CARGO_BIN_EXE_ccnm")).args([
            "internal",
            verb.as_str(),
            "--payload",
            wire.as_str(),
        ]);
        // Nothing of the test process's own environment: an inherited
        // SSH_AUTH_SOCK is exactly what the Runtime's audit refuses.
        for (key, _) in std::env::vars_os() {
            if key != "PATH" {
                local = local.env_remove(key);
            }
        }
        local = local
            .env("HOME", self.fx.dir.join("home"))
            .env("XDG_STATE_HOME", self.fx.dir.join("state"))
            .env("CCNM_CONFIG", &self.fx.config)
            .env("FAKE_EXEC_LOG", &self.fx.log);
        for (key, value) in &self.env {
            local = local.env(key, value);
        }
        if let Some(stdin) = &cmd.stdin {
            local = local.stdin(stdin.clone());
        }
        ccnm_core::SystemRunner.run(&local.timeout(cmd.timeout))
    }
}

/// P27: the exec-server row of `ccnm doctor`, through the preflight `ccnm run`
/// makes, against the real `exec-serve` and the fake executor.
///
/// The Agent side is `work::probe` in this process with a Codex instance;
/// the table is `doctor::from_agent`, from the Runtime's own resolve answer.
/// No MCP handshake (`mcp_calls: 0`): that transport is spawned directly,
/// not through a runner, and it has its own tests.
#[test]
fn doctor_reports_the_exec_server_chain_through_the_real_preflight() {
    use ccnm_core::doctor;
    use ccnm_core::protocol::probe::ProbeRequest;
    use ccnm_core::runtime::{ResolveReport, ResolveRequest};
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::build("doctor");
    let agent_home = fx.dir.join("agent-home");
    let profile = agent_home.join(".config/ccnm/agents/codex");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agent_config = fx.dir.join("agent.toml");
    std::fs::write(
        &agent_config,
        "this = \"agent\"\nruntime_node = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"runtime-alias\"\nccnm_bin = \"/opt/runtime/ccnm\"\n[agents.codex-main]\nprovider = \"codex\"\nprofile_ref = \"default\"\n",
    )
    .unwrap();

    // The Runtime's answer, from the real binary.
    let resolved = fx
        .command(
            "runtime-resolve",
            &payload::encode(&ResolveRequest::new("demo", None)).unwrap(),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(resolved.status.success(), "{}", stderr(&resolved));
    let authority: ResolveReport = payload::decode_json(&resolved.stdout).unwrap();
    assert!(authority.codex_exec_server);

    let doctor_with = |env: Vec<(&'static str, &'static str)>| {
        let runner = RuntimeOverSsh {
            fx: &fx,
            env,
            verbs: Default::default(),
        };
        let tools = ccnm_core::work::Tools {
            runner: &runner,
            config: ccnm_core::Config::load(&agent_config).unwrap(),
            local: Some(
                ccnm_core::instance::AgentLocal::new(Default::default(), agent_home.clone(), None)
                    .unwrap(),
            ),
            state: fx.dir.join("agent-state"),
            // Checked for length only (macOS allows 103 bytes of socket
            // path), never created: nothing on this path starts a master.
            control_dir: PathBuf::from(format!("/tmp/ccnm-p27-{}", std::process::id())),
            agents: ccnm_core::provider::AgentBinaries::with_claude(None),
            controller: fx.dir.join("no-controller.sock"),
            tmux: None,
        };
        let probe = ccnm_core::work::probe(
            &ProbeRequest {
                protocol: ccnm_core::instance::INSTANCE_SESSION_PROTOCOL,
                provider: Default::default(),
                agent: authority.agent.clone(),
                workspace: authority.workspace.clone(),
                root: authority.root.clone(),
                runtime_node: authority.runtime_node.clone(),
                provider_config_dir: authority.provider_config_dir.clone(),
                mcp_calls: 0,
                codex_exec_server: authority.codex_exec_server,
            },
            &tools,
        );
        let report = doctor::from_agent(&agent_config, "demo", Ok((&authority, &probe)));
        let verbs = runner.verbs.lock().unwrap().clone();
        (report, verbs)
    };
    let exec_row = |report: &doctor::Report| {
        report
            .checks
            .iter()
            .find(|c| c.name == "Codex exec-server")
            .unwrap_or_else(|| panic!("no exec-server row in\n{}", report.render()))
            .clone()
    };

    // A healthy chain: OK, one empty session, and nothing reached the
    // executor -- the preflight opens and closes, it asks nothing.
    let (report, verbs) = doctor_with(vec![]);
    let row = exec_row(&report);
    assert_eq!(row.status, doctor::Status::Ok, "{}", report.render());
    assert_eq!(verbs, ["hello", "runtime-audit", "exec-serve"]);
    assert!(fx.executor_saw().is_empty(), "{:?}", fx.executor_saw());
    let zh = report.render_in(ccnm_core::Lang::Zh);
    assert!(
        zh.contains("Codex 原生链            正常   empty exec-serve session on runtime"),
        "{zh}"
    );
    // The guard was taken and given back: a writer can start right after.
    let guards: Vec<String> = std::fs::read_dir(fx.dir.join("state/ccnm/write-guards"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(guards, ["released\n"]);
    let mut writer = fx.open("writer");
    writer.handshake();

    // A writer holds the guard: the row fails with the Runtime's own code
    // and reason, not as an unreachable Runtime.
    let (report, _) = doctor_with(vec![]);
    let row = exec_row(&report);
    assert_eq!(
        row.status,
        doctor::Status::Fail(ccnm_core::ErrorCode::Policy),
        "{}",
        report.render()
    );
    assert!(row.detail.contains("write guard is busy"), "{}", row.detail);
    assert!(writer.close().status.success());

    // The Codex binary is not the measured release.
    let (report, _) = doctor_with(vec![("FAKE_CODEX_VERSION", "codex-cli 0.155.0")]);
    let row = exec_row(&report);
    assert_eq!(
        row.status,
        doctor::Status::Fail(ccnm_core::ErrorCode::Version),
        "{}",
        report.render()
    );
    assert!(row.detail.contains("Codex 0.155.0"), "{}", row.detail);
    assert!(row.detail.contains("0.154.0"), "{}", row.detail);

    // The Runtime names no Codex binary.
    let text = std::fs::read_to_string(&fx.config).unwrap();
    let without: String = text
        .lines()
        .filter(|l| !l.starts_with("codex_bin"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&fx.config, without).unwrap();
    let (report, _) = doctor_with(vec![]);
    let row = exec_row(&report);
    assert_eq!(
        row.status,
        doctor::Status::Fail(ccnm_core::ErrorCode::Config),
        "{}",
        report.render()
    );
    assert!(
        row.detail.contains("codex_bin is not set"),
        "{}",
        row.detail
    );
    let en = report.render_in(ccnm_core::Lang::En);
    assert!(
        en.contains("Codex exec-server       FAIL   CCNM_E_CONFIG: "),
        "{en}"
    );

    // None of it left the guard held.
    let guards: Vec<String> = std::fs::read_dir(fx.dir.join("state/ccnm/write-guards"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(guards, ["released\n"]);
}

/// The same table against the real executor, when one is available.
#[test]
fn against_the_real_codex_executor_when_configured() {
    let Some(bin) = std::env::var_os("CCNM_TEST_CODEX_BIN") else {
        eprintln!("skipped: set CCNM_TEST_CODEX_BIN to a Codex 0.154.0 binary to run this");
        return;
    };
    let fx = Fixture::with_bin("real", Path::new(&bin), true);
    let mut s = fx.open("real");
    let reply = s.call(&json!({"id": 1, "method": "initialize", "params": {"clientName": "codex-environment", "resumeSessionId": null}}));
    assert!(reply["result"]["environmentInfo"].is_object(), "{reply}");
    s.send(&json!({"method": "initialized", "params": {}}));

    let inside = fx.root.join("inside.txt");
    let escaped = fx.outside.join("escaped.txt");
    let mut start = fx.start(
        2,
        &format!("echo in > inside.txt; echo out > {}", escaped.display()),
    );
    // The captured request's attribution was replaced by placeholders, and
    // the real executor checks that a thread id is a UUID. It is optional.
    start["params"].as_object_mut().unwrap().remove("metadata");
    start["params"]["env"] = json!({});
    let reply = s.call(&start);
    assert!(reply.get("result").is_some(), "{reply}");
    wait_for("the sandboxed command", || inside.exists());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !escaped.exists(),
        "the executor's own sandbox should stop this one"
    );

    let mut escalated = fx.start(3, &format!("echo out > {}", escaped.display()));
    escalated["params"]
        .as_object_mut()
        .unwrap()
        .remove("metadata");
    escalated["params"]["sandbox"] = Value::Null;
    assert_eq!(error_code(&s.call(&escalated)), -32600);
    let mut approved = fx.captured("fs-write-file-approved-outside.json");
    approved["id"] = json!(4);
    assert_eq!(error_code(&s.call(&approved)), -32600);
    let reply = s.call(&fx.write_file(5, &fx.root.join("patched.txt"), "hello\n"));
    assert!(reply.get("result").is_some(), "{reply}");

    let out = s.close();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!escaped.exists());
    assert!(!fx.outside.join("patched-outside.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fx.root.join("patched.txt")).unwrap(),
        "hello\n"
    );
}

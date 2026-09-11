//! Real Runtime processes and file locks; no network or Agent CLI.
use ccnm_core::protocol::mcp::ServePayload;
use ccnm_core::protocol::payload;
use ccnm_core::session;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    config: PathBuf,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p3-guard-{name}-{}", session::new_id()));
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir(&home).unwrap();
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "this='runtime'\n[nodes.runtime]\n[nodes.agent]\nssh='agent'\n[workspaces.demo]\nagent_node='agent'\nroot='{}'\nallow_unconfined_exec=true\n",
                project.display()
            ),
        )
        .unwrap();
        Self {
            root,
            project,
            config,
        }
    }

    fn command(&self, session: &str) -> Command {
        let wire =
            payload::encode(&ServePayload::new("demo", self.project.clone(), session)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        command
            .args(["internal", "mcp-serve", "--payload", &wire])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("CCNM_CONFIG", &self.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn start(&self, session: &str) -> Server {
        let mut child = self.command(session).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"guard-test","version":"0"}}})).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "server exited before initialize");
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], 1);
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        stdin.flush().unwrap();
        Server {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    #[allow(dead_code)]
    stdout: BufReader<ChildStdout>,
}
impl Server {
    fn graceful(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "{status}");
    }
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

fn refused(fixture: &Fixture, session: &str, contains: &str) {
    let output = fixture.command(session).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(ccnm_core::ErrorCode::Policy.exit_code())
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(contains), "{stderr}");
}

fn alive(pid: i32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn live_owner_is_busy_and_clean_shutdown_allows_reentry() {
    let fixture = Fixture::new("live");
    let first = fixture.start("one");
    refused(&fixture, "two", "write guard is busy");
    first.graceful();
    fixture.start("one").graceful();
}

#[test]
fn abrupt_server_exit_never_transfers_authority_by_timeout() {
    let fixture = Fixture::new("crash");
    fixture.start("one").kill();
    std::thread::sleep(Duration::from_millis(20));
    refused(&fixture, "two", "not transferred automatically");
}

#[test]
fn residual_exec_child_keeps_the_workspace_unknown_until_manual_recovery() {
    let fixture = Fixture::new("child");
    let script = fixture.root.join("owned-child.sh");
    let pid_file = fixture.root.join("owned-child.pid");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut server = fixture.start("one");
    writeln!(server.stdin.as_mut().unwrap(), "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec_command","arguments":{"cmd":[script.to_str().unwrap()]}}})).unwrap();
    server.stdin.as_mut().unwrap().flush().unwrap();
    // Wait for a pid, not for the file: the shell's `>` creates it before
    // `printf` writes into it, so on a busy machine `is_file` is true while
    // the content is still empty and the parse below blows up on "".
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the child never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    };
    server.kill();
    assert!(alive(pid));
    refused(&fixture, "two", "not transferred automatically");
    // What the Runtime operator does by hand. `--` matters: without it
    // Linux `kill` reads `-1234` as a signal, signals nothing and exits 0
    // (see process::kill_group), so this step would quietly do nothing and
    // the assertion below is what noticed.
    let _ = Command::new("/bin/kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output();
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid));
    refused(&fixture, "two", "not transferred automatically");
    let guards = fixture.root.join("state/ccnm/write-guards");
    for entry in std::fs::read_dir(guards).unwrap().flatten() {
        std::fs::remove_file(entry.path()).unwrap();
    }
    fixture.start("two").graceful();
}

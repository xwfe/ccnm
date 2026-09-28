//! P58: the internal wire for runs whose ccnm session id the Runtime chose,
//! through the real binary. No ssh, no controller, no Agent CLI: every case
//! here must end before any of them would be touched, and a fake `ssh` on
//! PATH proves it by leaving a file if it is ever run.
use ccnm_core::protocol::payload;
use ccnm_core::session;
use ccnm_testdir::TestDir;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    dir: TestDir,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-assigned-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            include_str!("../../../tests/fixtures/agent-instance/agent.toml"),
        )
        .unwrap();
        let ssh = dir.join("bin/ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 255\n",
                dir.join("ssh-was-run").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        Fixture {
            dir: TestDir::adopt(dir),
        }
    }

    fn state(&self) -> PathBuf {
        self.dir.join("state/ccnm")
    }

    fn internal(&self, sub: &str, request: Value) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ccnm"))
            .env_clear()
            .env("HOME", self.dir.join("home"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.join("bin").display()),
            )
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", self.dir.join("agent.toml"))
            .args([
                "internal",
                sub,
                "--payload",
                &payload::encode(&request).unwrap(),
            ])
            .output()
            .unwrap()
    }

    fn nothing_dialled(&self) {
        assert!(!self.dir.join("ssh-was-run").exists(), "ssh was run");
    }
}

fn run_request(protocol: u32, session: &str) -> Value {
    json!({
        "protocol": protocol,
        "agent": {"node": "worker", "instance": "claude-main"},
        "workspace": "demo",
        "root": "/srv/demo",
        "runtime_node": "runtime",
        "claude_config_dir": null,
        "permission_mode": "acceptEdits",
        "prompt": "go",
        "timeout_secs": 60,
        "session": session,
    })
}

fn stop_request(session: &str) -> Value {
    json!({
        "protocol": 7,
        "workspace": "demo",
        "agent": {"node": "worker", "instance": "claude-main"},
        "session": session,
        "assigned": true,
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A request that carries an assigned id must say protocol 7, and a build
/// refuses a number it does not know instead of reading it as something
/// older.
#[test]
fn an_assigned_run_under_any_other_protocol_number_is_refused() {
    let f = Fixture::new("version");
    let id = session::new_id();
    for protocol in [3, 8] {
        let out = f.internal("agent-run", run_request(protocol, &id));
        assert!(!out.status.success(), "protocol {protocol}");
        assert!(
            stderr(&out).starts_with("CCNM_E_VERSION"),
            "{protocol}: {}",
            stderr(&out)
        );
        assert!(out.stdout.is_empty());
    }
    assert!(!f.state().join("sessions").join(&id).exists());
    f.nothing_dialled();
}

/// The stop got to the Agent first. It fences the id and answers for that
/// session; the run that arrives afterwards refuses before it dials the
/// Runtime or creates anything.
#[test]
fn a_stop_that_arrives_first_keeps_its_run_from_starting() {
    let f = Fixture::new("fence");
    let id = session::new_id();
    let out = f.internal("agent-stop", stop_request(&id));
    assert!(out.status.success(), "{}", stderr(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["session"], id.as_str());
    assert_eq!(report["killed"], false);
    assert_eq!(report["agent_identity"]["instance"], "claude-main");
    let dir = f.state().join("sessions").join(&id);
    assert!(dir.join("stopping").exists());

    let out = f.internal("agent-run", run_request(7, &id));
    assert!(!out.status.success());
    assert!(
        stderr(&out).starts_with("CCNM_E_NOT_READY"),
        "{}",
        stderr(&out)
    );
    assert!(
        !dir.join("session.json").exists(),
        "nothing was created for it"
    );
    f.nothing_dialled();

    // Stopping it again is still a success that touches nothing.
    let again = f.internal("agent-stop", stop_request(&id));
    assert!(again.status.success(), "{}", stderr(&again));
}

/// An assigned stop names a real ccnm session id; anything else is refused
/// before it names a directory.
#[test]
fn an_assigned_stop_with_a_malformed_id_touches_nothing() {
    let f = Fixture::new("malformed");
    for bad in ["../../escape", "not-a-uuid"] {
        let out = f.internal("agent-stop", stop_request(bad));
        assert!(!out.status.success(), "{bad}");
        assert!(
            stderr(&out).starts_with("CCNM_E_INVALID_ARGS"),
            "{bad}: {}",
            stderr(&out)
        );
    }
    assert!(!f.dir.join("escape").exists());
    assert!(!f.state().join("sessions").exists());
}

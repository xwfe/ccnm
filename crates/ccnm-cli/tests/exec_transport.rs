//! `ccnm internal exec-transport`, through the real binary (P23).
//!
//! Codex spawns this from the session's `environments.toml` and holds both
//! ends of its pipe; it becomes the ssh to the Runtime's `exec-serve`. What
//! only this file can prove is that the verb exists, that it refuses a
//! session that is not on the chain and an identity that is not the
//! session's, and that for a session that is it really does exec the system
//! ssh at the session's Runtime alias -- the alias here is unresolvable, so
//! ssh itself is what answers, with 255.
//!
//! Nothing here reaches a Runtime or starts Codex.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use ccnm_core::instance::AgentIdentity;
use ccnm_core::protocol::payload;
use ccnm_core::provider::AgentProvider;
use ccnm_core::session::{self, Dir, Mode, Spec};

struct Fixture(PathBuf);

impl Fixture {
    fn new(test: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-exec-transport-{test}-{}", session::new_id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    /// The binary in an environment of its own: a HOME with no ssh config,
    /// so the alias below resolves nowhere, and no PATH surprises.
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_STATE_HOME", self.0.join("state"));
        cmd
    }

    fn session(&self, spec: &Spec) -> Dir {
        let dir = Dir::at(self.0.join(&spec.id));
        std::fs::create_dir(dir.path()).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.meta(), serde_json::to_vec(spec).unwrap()).unwrap();
        dir
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn codex() -> AgentIdentity {
    AgentIdentity {
        node: "agent".into(),
        instance: "codex-main".into(),
        provider: AgentProvider::Codex,
        profile_ref: "default".into(),
    }
}

fn spec(id: &str, on_chain: bool) -> Spec {
    Spec {
        protocol: 3,
        runtime_node: Some("runtime".into()),
        provider: AgentProvider::Codex,
        agent_identity: Some(codex()),
        id: id.into(),
        workspace: "demo".into(),
        root: "/srv/projects/demo".into(),
        runtime: Some(session::RuntimeLink {
            alias: "never-connect.invalid".into(),
            ccnm_bin: "/opt/ccnm/bin/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Interactive { prompt: None },
        timeout_secs: 0,
        cwd: "/tmp".into(),
        codex_exec_server: on_chain,
    }
}

fn request(dir: &Dir, identity: Option<AgentIdentity>) -> String {
    payload::encode(&session::transport::Request {
        protocol: if identity.is_some() { 3 } else { 2 },
        identity,
        session_dir: dir.path().to_path_buf(),
    })
    .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A session whose tools are served over MCP has no business here, and
/// neither has a caller naming another identity than the session's. Both
/// are refused by ccnm, before any ssh: the exit code is ccnm's, not 255.
#[test]
fn a_session_off_the_chain_and_a_foreign_identity_are_refused_before_ssh() {
    let f = Fixture::new("refused");
    let mcp = f.session(&spec("mcp-session", false));
    let out = f
        .command()
        .args([
            "internal",
            "exec-transport",
            "--payload",
            &request(&mcp, Some(codex())),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::InvalidArgs.exit_code()),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("MCP"), "{}", stderr(&out));

    let native = f.session(&spec("native-session", true));
    let mut other = codex();
    other.instance = "codex-other".into();
    let out = f
        .command()
        .args([
            "internal",
            "exec-transport",
            "--payload",
            &request(&native, Some(other)),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::InvalidArgs.exit_code()),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("identity"), "{}", stderr(&out));

    // And the MCP verb will not carry a chain session either: a build that
    // reached the wrong verb fails on the name.
    let out = f
        .command()
        .args([
            "internal",
            "agent-transport",
            "--payload",
            &request(&native, Some(codex())),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::InvalidArgs.exit_code()),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("exec-transport"), "{}", stderr(&out));
}

/// For a session on the chain the process becomes the system ssh, pointed
/// at the session's own Runtime alias. The alias cannot resolve, so what
/// answers is ssh -- exit 255 and the alias in its complaint -- which is
/// exactly the proof that ccnm handed over to it rather than answering
/// itself.
#[test]
fn a_chain_session_becomes_the_ssh_to_its_runtime() {
    let f = Fixture::new("exec");
    let native = f.session(&spec("native-session", true));
    let out = f
        .command()
        .args([
            "internal",
            "exec-transport",
            "--payload",
            &request(&native, Some(codex())),
        ])
        .output()
        .unwrap();
    let said = stderr(&out);
    assert_eq!(out.status.code(), Some(255), "{said}");
    assert!(said.contains("never-connect.invalid"), "{said}");
    assert!(
        !said.contains("CCNM_E_"),
        "ccnm did not answer; ssh did: {said}"
    );
}

//! The Runtime-authority open, through the real binary.
//!
//! `crates/ccnm-core/src/runtime.rs` proves the decision; what only this
//! file can prove is that `ccnm internal mcp-serve` actually routes the new
//! wire shape to it, that the old shape still works beside it, and that a
//! protocol number neither of them knows stops the session instead of being
//! retried as the other one.
//!
//! Nothing here starts an Agent, dials ssh, or serves a session: every case
//! is refused before the MCP transport opens.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ccnm_core::instance::AgentIdentity;
use ccnm_core::protocol::mcp::ServePayload;
use ccnm_core::protocol::payload;
use ccnm_core::provider::AgentProvider;
use ccnm_core::runtime::OpenPayload;

fn ccnm() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
    let home = std::env::temp_dir().join(format!("ccnm-open-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("USER", std::env::var_os("USER").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"));
    cmd
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A Runtime config with one instance workspace, and the directory it
/// points at.
fn setup(test: &str, root_exists: bool) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ccnm-open-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("project");
    if root_exists {
        std::fs::create_dir_all(&root).unwrap();
    }
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
this = "runtime"

[nodes.runtime]
runtime_user = "ccrun"

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{}"
agent = {{ node = "agent", instance = "claude-main" }}
allow_unconfined_exec = true
"#,
            root.display()
        ),
    )
    .unwrap();
    (config, root)
}

fn identity() -> AgentIdentity {
    AgentIdentity {
        node: "agent".into(),
        instance: "claude-main".into(),
        provider: AgentProvider::Claude,
        profile_ref: "default".into(),
    }
}

/// `CCNM_CONFIG` rather than `--config`: the Runtime resolves its own
/// authoritative config the way every in-process caller does, and that
/// lookup honours the environment, not this command's flags.
fn serve(config: &Path, wire: &str) -> Output {
    ccnm()
        .env("CCNM_CONFIG", config)
        .args(["internal", "mcp-serve", "--payload", wire])
        .output()
        .unwrap()
}

/// The request names a workspace and nothing else. The root it opens is the
/// one in *this* machine's config, so a config that points nowhere fails
/// with the root the Runtime chose -- naming a directory the caller never
/// sent, which is the whole point of the shape.
#[test]
fn the_runtime_supplies_the_root_the_request_could_not() {
    let (config, root) = setup("resolves", false);
    let wire = payload::encode(&OpenPayload::new("demo", identity(), "session-1")).unwrap();
    let out = serve(&config, &wire);
    assert!(!out.status.success());
    let text = stderr(&out);
    assert!(text.starts_with("CCNM_E_WRONG_WORKSPACE:\n"), "{text}");
    assert!(text.contains(&root.display().to_string()), "{text}");
}

#[test]
fn a_workspace_this_runtime_does_not_have_is_refused() {
    let (config, _) = setup("unknown", true);
    let wire = payload::encode(&OpenPayload::new("not-here", identity(), "session-1")).unwrap();
    let out = serve(&config, &wire);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("workspace"), "{}", stderr(&out));
}

/// The migration boundary: both shapes are served by the same command, and
/// each is decoded as itself. The legacy one still carries its own root, so
/// it fails on that root rather than on the workspace's.
#[test]
fn the_legacy_shape_still_opens_beside_the_new_one() {
    let (config, _) = setup("legacy", true);
    let wire = payload::encode(&ServePayload::new(
        "demo",
        PathBuf::from("/nonexistent/caller/root"),
        "session-1",
    ))
    .unwrap();
    let out = serve(&config, &wire);
    assert!(!out.status.success());
    let text = stderr(&out);
    assert!(text.contains("/nonexistent/caller/root"), "{text}");
}

/// A number from a build this one does not know must stop here. Trying the
/// other shape and hoping is how a new chain ends up running on old rules.
#[test]
fn an_unknown_protocol_stops_rather_than_falling_back() {
    let (config, _) = setup("version", true);
    let json = serde_json::json!({
        "protocol": 99,
        "workspace": "demo",
        "session": "session-1",
    });
    let wire = payload::encode(&json).unwrap();
    let out = serve(&config, &wire);
    assert!(!out.status.success());
    let text = stderr(&out);
    assert!(text.starts_with("CCNM_E_VERSION:\n"), "{text}");
    assert!(text.contains("protocol 99"), "{text}");
}

//! Configuration support is not permission to launch an instance in P2.
use ccnm_core::protocol::payload;
use ccnm_core::provider::AgentProvider;
use ccnm_core::session::{self, Dir, Mode, Spec, SuperviseRequest};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p2-cli-{}", session::new_id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.env_clear().env("HOME", &self.0).env(
            "PATH",
            format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
        );
        cmd
    }
    fn script(&self, name: &str) -> PathBuf {
        let bin = self.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join(name);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 99\n",
                self.0.join("unexpected-child").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        script
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn public_instance_run_is_refused_before_ssh_including_delegating_configs() {
    let f = Fixture::new();
    f.script("ssh");
    let base = include_str!("../../../tests/fixtures/agent-instance/runtime.toml");
    for conflicting_delegation in [false, true] {
        let text = if conflicting_delegation {
            base.replace(
                "this = \"runtime\"",
                "this = \"runtime\"\nruntime_node='other'\n[nodes.other]\nssh='other-alias'",
            )
        } else {
            base.to_string()
        };
        let path = f.0.join("config.toml");
        std::fs::write(&path, text).unwrap();
        for args in [
            vec!["run", "demo", "--print", "not sent"],
            vec!["run", "demo", "--detached"],
        ] {
            let out = f
                .command()
                .arg("--config")
                .arg(&path)
                .args(args)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(
                    if conflicting_delegation {
                        ccnm_core::ErrorCode::Config
                    } else {
                        ccnm_core::ErrorCode::NotReady
                    }
                    .exit_code()
                ),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            if !conflicting_delegation {
                assert!(
                    String::from_utf8_lossy(&out.stderr)
                        .contains("Agent Instance execution is not open")
                );
            }
            assert!(!f.0.join("unexpected-child").exists());
        }
    }
}

#[test]
fn instance_records_cannot_start_supervisors_or_transports_through_legacy_commands() {
    let f = Fixture::new();
    let fake_agent = f.script("agent");
    f.script("ssh");
    let dir = Dir::at(f.0.join("record"));
    std::fs::create_dir(dir.path()).unwrap();
    let spec = Spec {
        protocol: 3,
        agent_identity: Some(ccnm_core::instance::AgentIdentity {
            node: "worker".into(),
            instance: "main".into(),
            provider: AgentProvider::Claude,
            profile_ref: "default".into(),
        }),
        provider: AgentProvider::Claude,
        id: "fixture".into(),
        workspace: "demo".into(),
        root: "/runtime/project".into(),
        runtime: Some(session::RuntimeLink {
            alias: "never-connect.invalid".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Print {
            prompt: "not sent".into(),
        },
        timeout_secs: 10,
        cwd: f.0.clone(),
    };
    std::fs::write(dir.meta(), serde_json::to_vec(&spec).unwrap()).unwrap();
    let supervise = SuperviseRequest::new(dir.path().to_path_buf(), fake_agent.clone());
    let transport = session::transport::Request {
        protocol: 2,
        session_dir: dir.path().to_path_buf(),
    };
    for (command, wire) in [
        ("supervise", payload::encode(&supervise).unwrap()),
        ("agent-transport", payload::encode(&transport).unwrap()),
    ] {
        let out = f
            .command()
            .args(["internal", command, "--payload", &wire])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(ccnm_core::ErrorCode::NotReady.exit_code())
        );
        assert!(!f.0.join("unexpected-child").exists());
    }
    let runner = ccnm_core::process::FakeRunner::new();
    let tools = ccnm_core::controller::Tools {
        runner: &runner,
        agents: ccnm_core::provider::AgentBinaries::with_claude(Some(fake_agent)),
        tmux: None,
        exe: env!("CARGO_BIN_EXE_ccnm").into(),
    };
    let request = ccnm_core::controller::Request::new(ccnm_core::controller::RequestBody::Start {
        session_dir: dir.path().to_path_buf(),
        provider: AgentProvider::Claude,
    });
    let reply = ccnm_core::controller::answer(&request, &tools);
    assert!(matches!(
        reply.body,
        ccnm_core::controller::ReplyBody::Error(_)
    ));
    assert!(runner.calls().is_empty());
    assert!(!f.0.join("unexpected-child").exists());
}

#[test]
fn legacy_mcp_payloads_cannot_execute_an_instance_selected_workspace() {
    let f = Fixture::new();
    let root = f.0.join("project");
    std::fs::create_dir(&root).unwrap();
    let config = f.0.join("config.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
            .replace("/runtime/project", root.to_str().unwrap()),
    )
    .unwrap();
    for provider in AgentProvider::ALL {
        let request = ccnm_core::protocol::mcp::ServePayload::new("demo", root.clone(), "fixture")
            .with_provider(provider);
        let out = f
            .command()
            .env("CCNM_CONFIG", &config)
            .args([
                "internal",
                "mcp-serve",
                "--payload",
                &payload::encode(&request).unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(ccnm_core::ErrorCode::NotReady.exit_code()),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
}

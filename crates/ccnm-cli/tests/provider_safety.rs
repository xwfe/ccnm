//! Offline process-boundary tests: fake SSH, synthetic environment, no Agent CLI.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use ccnm_core::{protocol::payload, provider::AgentProvider, session, ssh::Ssh};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p1-wire-{}", session::new_id()));
        // Tests use separate subdirectories; never touch unrelated state.
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn claude_mcp_launch_plan_and_shared_exec_environment_boundary() {
    // Re-enter this test in a process with synthetic *inherited* credentials.
    // Only the SSH executable is replaced; production's Cmd::process handles
    // argv/environment for both spawn and transport exec. No real SSH connects.
    if let Some(wire) = std::env::var_os("CCNM_P1_TEST_PAYLOAD") {
        let request: session::transport::Request = payload::decode(wire.to_str().unwrap()).unwrap();
        let dir = session::Dir::at(request.session_dir);
        let spec = session::load(&dir).unwrap();
        let mut cmd = session::transport::command(&spec).unwrap();
        assert_eq!(cmd.program, session::SSH_BIN);
        cmd.program = spec.cwd.join("bin/ssh").into_os_string();
        assert!(cmd.process().status().unwrap().success());
        return;
    }
    let f = Fixture::new();
    let root = f.0.join("transport");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let ssh = root.join("bin/ssh");
    std::fs::write(&ssh, "#!/bin/sh\n/usr/bin/env | /usr/bin/cut -d= -f1 | /usr/bin/sort > \"$CCNM_P1_CAPTURE.names\"\nprintf '%s\\n' \"$@\" > \"$CCNM_P1_CAPTURE.args\"\nprintf '%s\\n' \"$CCNM_P1_PROJECT\" > \"$CCNM_P1_CAPTURE.project\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let dir = session::Dir::at(root.join("session"));
    std::fs::create_dir(dir.path()).unwrap();
    let spec = session::Spec {
        runtime_node: None,
        agent_identity: None,
        protocol: 1,
        provider: AgentProvider::Claude,
        id: "fixture".into(),
        workspace: "fixture".into(),
        root: "/runtime/project".into(),
        runtime: Some(session::RuntimeLink {
            alias: "fixture-alias".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: Some("/agent/private-claude".into()),
        permission_mode: Default::default(),
        mode: session::Mode::Print {
            prompt: "fixture".into(),
        },
        timeout_secs: 10,
        cwd: root.clone(),
        codex_exec_server: false,
    };
    std::fs::write(dir.meta(), serde_json::to_vec(&spec).unwrap()).unwrap();
    let launcher =
        session::transport::launcher(&dir, Path::new(env!("CARGO_BIN_EXE_ccnm"))).unwrap();
    let config = ccnm_core::provider::claude::mcp_config(&launcher);
    let server = &config["mcpServers"]["ccnm"];
    let capture = root.join("capture");
    assert_eq!(server["command"], env!("CARGO_BIN_EXE_ccnm"));
    assert_eq!(server["args"][1], "agent-transport");
    let wire = server["args"][3].as_str().unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "claude_mcp_launch_plan_and_shared_exec_environment_boundary",
            "--nocapture",
        ])
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", &root)
        .env("CCNM_P1_TEST_PAYLOAD", wire)
        .env("CCNM_P1_CAPTURE", &capture)
        .env("CCNM_P1_PROJECT", "ordinary")
        .env("CODEX_HOME", "/agent/private-codex")
        .env("CLAUDE_CONFIG_DIR", "/agent/private-claude")
        .env("OPENAI_API_KEY", "SYNTHETIC_SECRET")
        .env("ANTHROPIC_API_KEY", "SYNTHETIC_SECRET")
        .env("UNRECOGNIZED_TOKEN", "SYNTHETIC_SECRET")
        .env("SSH_AUTH_SOCK", "/synthetic/local-auth-socket")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names = std::fs::read_to_string(capture.with_extension("names")).unwrap();
    for name in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "UNRECOGNIZED_TOKEN",
    ] {
        assert!(!names.lines().any(|line| line == name));
    }
    assert!(
        names.lines().any(|name| name == "SSH_AUTH_SOCK"),
        "local authentication is not forwarding"
    );
    assert_eq!(
        std::fs::read_to_string(capture.with_extension("project")).unwrap(),
        "ordinary\n"
    );
    let args = std::fs::read_to_string(capture.with_extension("args")).unwrap();
    assert!(args.contains("ForwardAgent=no\n"));
    assert!(args.contains("ControlPath=none\n"));
    let serve: ccnm_core::protocol::mcp::ServePayload =
        payload::decode(args.lines().last().unwrap()).unwrap();
    assert_eq!(serve.protocol, 1);
    assert_eq!(serve.root, spec.root);
    let wire = serde_json::to_string(&serve).unwrap();
    assert!(!wire.contains("/agent/"));
    assert!(!args.contains("SYNTHETIC_SECRET"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_openssh_options_cannot_restore_setenv_forwarding_or_connection_reuse() {
    let f = Fixture::new();
    let conf = f.0.join("ssh-config");
    std::fs::write(&conf, "Host fixture\n HostName fixture.invalid\n SendEnv OPENAI_API_KEY\n SetEnv CODEX_HOME=SYNTHETIC_PROFILE OPENAI_API_KEY=SYNTHETIC_SECRET\n ForwardAgent yes\n ControlMaster auto\n ControlPath /tmp/untrusted-ccnm-master\n").unwrap();
    for provider in AgentProvider::ALL {
        let cmd = Ssh::new("fixture", "/unused")
            .unwrap()
            .for_provider(provider)
            .mcp_transport_cmd("fixture")
            .unwrap();
        let out = Command::new("/usr/bin/ssh")
            .args(["-G", "-F"])
            .arg(&conf)
            .args(&cmd.args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let resolved = String::from_utf8(out.stdout).unwrap();
        assert!(resolved.contains("forwardagent no\n"));
        assert!(
            resolved.contains("controlmaster false\n") || resolved.contains("controlmaster no\n")
        );
        assert!(resolved.contains("setenv CCNM_TRANSPORT=1\n"));
        assert!(!resolved.contains("SYNTHETIC_PROFILE"));
        assert!(!resolved.contains("SYNTHETIC_SECRET"));
        assert!(!resolved.contains("/tmp/untrusted-ccnm-master"));
        // OpenSSH can retain configured SendEnv entries after a CLI -SendEnv.
        // The separate exec-boundary test proves those values are absent.
    }
    std::fs::remove_file(conf).unwrap();
}

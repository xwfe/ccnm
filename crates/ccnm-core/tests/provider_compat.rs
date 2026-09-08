//! Characterization of the pre-provider implementation, not a new CLI contract.
//! All inputs are synthetic except the existing captured Claude result fixture.
use std::path::{Path, PathBuf};

use ccnm_core::provider::{AgentProvider, Ask};
use ccnm_core::{claude, config::PermissionMode, controller, protocol, session, ssh};
use serde_json::{Value, json};

fn spec(remote: bool, mode: session::Mode) -> session::Spec {
    session::Spec {
        provider: Default::default(),
        protocol: protocol::PROTOCOL,
        id: "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into(),
        workspace: "fixture".into(),
        root: PathBuf::from("/project"),
        runtime: remote.then(|| session::RuntimeLink {
            alias: "runtime-alias".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: Some(PathBuf::from("/agent/config with space")),
        permission_mode: PermissionMode::Plan,
        mode,
        timeout_secs: 73,
        cwd: PathBuf::from(if remote {
            "/agent/workspace"
        } else {
            "/project"
        }),
    }
}

fn command(cmd: ccnm_core::Cmd) -> Value {
    json!({
        "program": cmd.program.to_str().unwrap(),
        "args": cmd.args.iter().map(|s| s.to_str().unwrap()).collect::<Vec<_>>(),
        "env": cmd.env.iter().map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap())).collect::<Vec<_>>(),
        "cwd": cmd.cwd,
        "stdin": cmd.stdin.map(|b| String::from_utf8(b).unwrap()),
        "timeout_secs": cmd.timeout.as_secs(),
    })
}

fn snapshot() -> Value {
    let bin = Path::new("/agent/claude");
    let dir = session::Dir::at("/state/session");
    let ssh = ssh::Ssh::new("runtime-alias", "/state/control").unwrap();
    let modes = [
        session::Mode::Print {
            prompt: "-a 'quoted'\n中文".into(),
        },
        session::Mode::Interactive { prompt: None },
        session::Mode::Interactive {
            prompt: Some("-a 'quoted'\n中文".into()),
        },
    ];
    let launches: Vec<Value> = [true, false]
        .into_iter()
        .flat_map(|remote| modes.iter().map(move |mode| spec(remote, mode.clone())))
        .map(|s| {
            json!({
                "spec": s,
                "command": command(AgentProvider::current().launch_cmd(bin, &s, &dir).unwrap()),
                "settings": session::settings(s.runtime.is_some()),
                "mcp": s.runtime.as_ref().map(|_| session::mcp_config(&s, &ssh).unwrap()),
            })
        })
        .collect();
    let probes: Vec<Value> = [None, Some(Path::new("/agent/config with space"))]
        .into_iter()
        .map(|config| {
            json!({
                "version": command(claude::version_cmd(bin, config)),
                "auth": command(claude::auth_status_cmd(bin, config)),
            })
        })
        .collect();
    let permissions: Vec<Value> = [
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
        PermissionMode::BypassPermissions,
        PermissionMode::Manual,
        PermissionMode::DontAsk,
        PermissionMode::Plan,
    ]
    .into_iter()
    .map(|mode| json!({"mode": mode, "cli": mode.as_cli_value()}))
    .collect();
    let parsed = AgentProvider::current()
        .parse_result(include_bytes!(
            "../../../tests/fixtures/claude-print-2.1.260.json"
        ))
        .unwrap();
    let auth = claude::parse_auth(&ccnm_core::Output::exited(0,
        r#"{"loggedIn":true,"email":"fixture@example.invalid","authMethod":"claude.ai","subscriptionType":"max","unknown":"ignored"}"#,
    )).unwrap();
    let report = ccnm_core::provider::AgentReport {
        path: Some(bin.to_path_buf()),
        version: Ok("2.1.260".into()),
        auth: Ok(auth.clone()),
    };
    let named = [ccnm_core::mcp::context::Named {
        rel: ".claude/rules/style.md".into(),
        bytes: 42,
    }];
    let project = ccnm_core::mcp::context::Project {
        source: "CLAUDE.md",
        bytes: 20,
        text: "project rules\n".into(),
    };
    json!({
        "launches": launches,
        "probes": probes,
        "permissions": permissions,
        "parsed_result": parsed,
        "result_summary": parsed.summary(),
        "auth": auth,
        "auth_summary": auth.describe(),
        "controller_request": controller::Request::new(controller::RequestBody::AgentAuth {
            provider: Default::default(),
            config_dir: Some(PathBuf::from("/agent/config with space")), ask: Ask::Everything,
        }),
        "controller_reply": controller::ReplyBody::Agent(report),
        "supervise": session::SuperviseRequest::new(dir.path().to_path_buf(), bin.to_path_buf()),
        "instructions": ccnm_core::mcp::context::instructions("fixture", Some(&project), &named),
        "no_instructions": ccnm_core::mcp::context::instructions("fixture", None, &[]),
    })
}

#[test]
fn claude_behavior_matches_pre_provider_snapshot_except_documented_ssh_hardening() {
    let actual = snapshot();
    let mut expected: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/claude-provider-baseline.json"
    ))
    .unwrap();
    // P1 intentionally closes forwarding and strips Agent environment on SSH.
    // All official CLI flags, policies, results and v1 records stay frozen.
    for launch in expected["launches"].as_array_mut().unwrap() {
        if let Some(args) = launch
            .pointer_mut("/mcp/mcpServers/ccnm/args")
            .and_then(Value::as_array_mut)
        {
            args.splice(
                0..0,
                [
                    "-o",
                    "SendEnv=-*",
                    "-o",
                    "SetEnv=CCNM_TRANSPORT=1",
                    "-o",
                    "ForwardAgent=no",
                    "-o",
                    "ClearAllForwardings=yes",
                ]
                .into_iter()
                .map(Value::from),
            );
        }
    }
    assert_eq!(actual, expected);
}

fn roundtrip<T: serde::de::DeserializeOwned + serde::Serialize>(wire: Value) -> T {
    let decoded: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    decoded
}

#[test]
fn old_controller_and_session_records_keep_their_wire_names() {
    let baseline: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/claude-provider-baseline.json"
    ))
    .unwrap();
    for launch in baseline["launches"].as_array().unwrap() {
        let session: session::Spec = roundtrip(launch["spec"].clone());
        assert_eq!(session.provider(), AgentProvider::Claude);
    }
    roundtrip::<controller::Request>(baseline["controller_request"].clone());
    roundtrip::<controller::ReplyBody>(baseline["controller_reply"].clone());
    roundtrip::<session::SuperviseRequest>(baseline["supervise"].clone());

    // Older records may omit the config directory and runtime link. Neither
    // means a new provider, nor permits silently changing their session mode.
    let mut old = baseline["launches"][0]["spec"].clone();
    old.as_object_mut().unwrap().remove("claude_config_dir");
    old.as_object_mut().unwrap().remove("runtime");
    let decoded: session::Spec = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.provider(), AgentProvider::Claude);
    assert_eq!(decoded.provider_config_dir, None);
    assert_eq!(decoded.runtime, None);
    assert!(!decoded.mode.is_interactive());
}

#[test]
fn old_launch_and_probe_requests_keep_their_wire_names() {
    let request = json!({
        "protocol": 1, "workspace": "fixture", "root": "/project",
        "runtime_node": "runtime", "claude_config_dir": "/agent/config",
        "permission_mode": "plan", "prompt": "hello", "timeout_secs": 73,
    });
    let run: protocol::run::RunRequest = roundtrip(request.clone());
    assert_eq!(
        run.provider_config_dir.as_deref(),
        Some(Path::new("/agent/config"))
    );
    let mut start = request;
    start.as_object_mut().unwrap().remove("timeout_secs");
    roundtrip::<protocol::run::StartRequest>(start);
    roundtrip::<protocol::probe::ProbeRequest>(json!({
        "protocol": 1, "workspace": "fixture", "root": "/project",
        "runtime_node": "runtime", "claude_config_dir": null, "mcp_calls": 1,
    }));
    let baseline: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/claude-provider-baseline.json"
    ))
    .unwrap();
    let mut agent = baseline["controller_reply"].clone();
    agent.as_object_mut().unwrap().remove("reply");
    let probe: protocol::probe::ProbeReport = roundtrip(json!({
        "protocol": 1,
        "hello": { "protocol": 1, "ccnm_version": "0.2.0", "user": "fixture",
            "platform": "macos/aarch64", "exe": "/agent/ccnm", "root": null },
        "controller": null, "claude": agent, "runtime_ssh": null,
        "runtime_hello": null, "mcp": null, "terminal": null,
    }));
    assert_eq!(probe.agent.version, Ok("2.1.260".into()));
}

#[test]
fn provider_keeps_the_existing_credential_metadata_and_no_extra_names() {
    use std::ffi::OsStr;
    let metadata = AgentProvider::current().credentials();
    assert_eq!(metadata.env_prefixes, &["ANTHROPIC_", "CLAUDE_"]);
    assert_eq!(metadata.config_env, "CLAUDE_CONFIG_DIR");
    assert_eq!(metadata.files, &[".credentials.json", "credentials.json"]);
    assert_eq!(metadata.egress_host, "api.anthropic.com");
    assert_eq!(
        metadata.config_directories(Path::new("/home/fixture"), None),
        vec![PathBuf::from("/home/fixture/.claude")]
    );
    assert_eq!(
        metadata.config_directories(Path::new("/home/fixture"), Some(OsStr::new("/custom"))),
        vec![
            PathBuf::from("/home/fixture/.claude"),
            PathBuf::from("/custom")
        ]
    );
    for name in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(metadata.is_environment_name(OsStr::new(name)));
    }
    for name in [
        "CLAUDECODE",
        "MY_ANTHROPIC_KEY",
        "PATH",
        "HOME",
        "OPENAI_API_KEY",
    ] {
        assert!(!metadata.is_environment_name(OsStr::new(name)));
    }
    // Match the old lossy name check without touching this process's environment.
    use std::os::unix::ffi::OsStrExt;
    assert!(metadata.is_environment_name(OsStr::from_bytes(b"CLAUDE_\xff")));
    assert!(!metadata.is_environment_name(OsStr::from_bytes(b"\xffCLAUDE_")));
}

#[test]
fn provider_permission_spelling_and_invalid_input_are_unchanged() {
    let provider = AgentProvider::current();
    for name in [
        "acceptEdits",
        "auto",
        "bypassPermissions",
        "manual",
        "dontAsk",
        "plan",
    ] {
        assert_eq!(
            provider.parse_permission_mode(name).unwrap().as_cli_value(),
            name
        );
    }
    for name in ["", "Plan", " plan", "plan ", "--plan"] {
        let err = provider.parse_permission_mode(name).unwrap_err();
        assert_eq!(err.code(), ccnm_core::ErrorCode::InvalidArgs);
        assert_eq!(
            err.message(),
            format!(
                "unknown permission mode {name}; one of acceptEdits, auto, bypassPermissions, manual, dontAsk, plan"
            )
        );
    }
}

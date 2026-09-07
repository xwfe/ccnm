//! Characterization of the pre-provider implementation, not a new CLI contract.
//! All inputs are synthetic except the existing captured Claude result fixture.
use std::path::{Path, PathBuf};

use ccnm_core::{claude, config::PermissionMode, controller, protocol, session, ssh};
use serde_json::{Value, json};

fn spec(remote: bool, mode: session::Mode) -> session::Spec {
    session::Spec {
        protocol: protocol::PROTOCOL,
        id: "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into(),
        workspace: "fixture".into(),
        root: PathBuf::from("/project"),
        runtime: remote.then(|| session::RuntimeLink {
            alias: "runtime-alias".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        claude_config_dir: Some(PathBuf::from("/agent/config with space")),
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
                "command": command(claude::launch_cmd(bin, &s, &dir)),
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
    let parsed = claude::parse_print(include_bytes!(
        "../../../tests/fixtures/claude-print-2.1.260.json"
    ))
    .unwrap();
    let auth = claude::parse_auth(&ccnm_core::Output::exited(0,
        r#"{"loggedIn":true,"email":"fixture@example.invalid","authMethod":"claude.ai","subscriptionType":"max","unknown":"ignored"}"#,
    )).unwrap();
    let report = claude::ClaudeReport {
        path: Some(bin.to_path_buf()),
        version: Ok("2.1.260".into()),
        auth: Ok(auth.clone()),
    };
    let named = [ccnm_core::mcp::context::Named {
        rel: ".claude/rules/style.md".into(),
        bytes: 42,
    }];
    let project = ccnm_core::mcp::context::Project {
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
        "controller_request": controller::Request::new(controller::RequestBody::ClaudeAuth {
            config_dir: Some(PathBuf::from("/agent/config with space")), ask: claude::Ask::Everything,
        }),
        "controller_reply": controller::ReplyBody::Claude(report),
        "supervise": session::SuperviseRequest::new(dir.path().to_path_buf(), bin.to_path_buf()),
        "instructions": ccnm_core::mcp::context::instructions("fixture", Some(&project), &named),
        "no_instructions": ccnm_core::mcp::context::instructions("fixture", None, &[]),
    })
}

#[test]
fn claude_behavior_matches_pre_provider_snapshot() {
    let actual = snapshot();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/claude-provider-baseline.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
}

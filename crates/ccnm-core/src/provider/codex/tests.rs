use super::*;
use crate::process::FakeRunner;
use crate::protocol::payload;
use crate::provider::{AgentProvider, AgentResult, PermissionMode};
use crate::session::{Mode, RuntimeLink};

fn spec(mode: Mode) -> Spec {
    Spec {
        runtime_node: None,
        agent_identity: None,
        protocol: 2,
        provider: AgentProvider::Codex,
        id: "fixture-session".into(),
        workspace: "fixture".into(),
        root: "/runtime/project".into(),
        runtime: Some(RuntimeLink {
            alias: "runtime-alias".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: PermissionMode::default(),
        mode,
        timeout_secs: 90,
        cwd: "/agent/state/workspace".into(),
    }
}

/// The instance can name a model; without one the CLI keeps its own
/// default, which is what every measured fixture was captured with.
///
/// It rides on argv rather than the CLI's config file because ccnm starts
/// Codex with `--ignore-user-config` -- deliberately, so that a file on the
/// Agent cannot change measured behaviour. That left no way at all to
/// choose a model until this existed.
#[test]
fn a_named_model_reaches_the_command_line_and_absence_changes_nothing() {
    let spec = spec(Mode::Print {
        prompt: "hi".into(),
    });
    let args = |model: Option<&str>| -> Vec<String> {
        build_launch_cmd(
            Path::new("/agent/codex"),
            &spec,
            &Dir::at("/agent/session"),
            Path::new("/agent/private-codex"),
            Path::new("/agent/ccnm"),
            model,
        )
        .unwrap()
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
    };
    let without = args(None);
    assert!(!without.iter().any(|a| a == "--model"), "{without:?}");

    let with = args(Some("gpt-5.3-codex-spark"));
    let at = with
        .iter()
        .position(|a| a == "--model")
        .expect("the flag is there");
    assert_eq!(with[at + 1], "gpt-5.3-codex-spark");
    // Two flags and their values, nothing more: the model, and Code Mode
    // going away because this model is not in the measured table.
    let mut stripped = with.clone();
    stripped.drain(at..at + 2);
    let mut expected = without.clone();
    let code_mode = expected
        .iter()
        .position(|a| a == "--enable")
        .expect("the default model runs with Code Mode");
    assert_eq!(expected[code_mode + 1], "code_mode_only");
    expected.drain(code_mode..code_mode + 4);
    assert_eq!(stripped, expected);
}

/// The measured fixture records the launch ccnm actually performs, not one
/// that resembles it.
///
/// This is the check that would have caught the drift the Code Mode gate
/// created: the 0.154.0 fixture was captured with Code Mode forced on, and
/// the moment the gate landed, that recorded argv stopped being what ccnm
/// sends. Spot assertions on a flag or two do not notice that; comparing
/// the whole option sequence does.
///
/// Everything from `exec` up to the MCP wiring is compared verbatim. The
/// MCP block is where the harness legitimately differs -- it points the
/// server at `/usr/bin/env` with a scratch fixture payload rather than at
/// the real Agent transport -- so it is excluded rather than fudged.
#[test]
fn the_measured_fixture_records_the_launch_this_adapter_builds() {
    let outcome: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/codex-0.154.0/seven-tools.json"
    ))
    .unwrap();
    let measured: Vec<String> = outcome["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcome["code_mode"], false, "measured without Code Mode");

    let built = build_launch_cmd(
        Path::new("/agent/codex"),
        &spec(Mode::Print {
            prompt: "hi".into(),
        }),
        &Dir::at("/agent/session"),
        Path::new("/agent/private-codex"),
        Path::new("/agent/ccnm"),
        Some("gpt-5.3-codex-spark"),
    )
    .unwrap();
    let built: Vec<String> = built
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let upto_mcp = |args: &[String]| -> Vec<String> {
        let end = args
            .iter()
            .position(|a| a.starts_with("mcp_servers."))
            .expect("the MCP wiring is there");
        args[..end - 1].to_vec()
    };
    // The fixture's argv[0] is the codex binary; the built command carries
    // it separately.
    assert_eq!(upto_mcp(&measured[1..]), upto_mcp(&built));
}

/// What the model can reach when Code Mode is off, measured rather than
/// assumed -- because turning Code Mode off is what the gate above does.
///
/// The answer is the reason the support matrix records a narrower claim for
/// this configuration: Codex's own `apply_patch` is visible to the model,
/// and what keeps it off the Agent's disk is the read-only sandbox rather
/// than the tool being absent. With Code Mode on, the namespace filter was
/// measured to remove it outright (docs/research/codex-provider-probe-2026-09-07.md).
///
/// It is the model reporting its own registry, so it is an observation, not
/// a permission proof -- same limit as that earlier probe.
#[test]
fn the_measured_tool_surface_without_code_mode_still_shows_codex_own_patch_tool() {
    let outcome: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/codex-0.154.0/tool-surface.json"
    ))
    .unwrap();
    assert_eq!(outcome["code_mode"], false);
    assert_eq!(outcome["terminal_event"], "turn.completed");
    assert!(
        outcome["completed_tools"].as_array().unwrap().is_empty(),
        "an inventory prompt must not touch anything"
    );
    let reported = outcome["reported_tools"].as_array().unwrap()[0]
        .as_str()
        .unwrap();
    assert!(reported.contains("functions.apply_patch"), "{reported}");
    // ccnm's seven are reachable through tool search, not at the top level;
    // the seven-tools measurement is what proves the model gets to them.
    assert!(reported.contains("tool_search"), "{reported}");
}

/// Code Mode is an under-development Codex feature that a model may refuse.
/// ccnm turns it on only for models it has measured with it; the CLI default
/// (no `--model`) is the one every fixture was captured on.
///
/// This is not cosmetic. Forcing it onto `gpt-5.3-codex-spark`, which tells
/// Codex it does not support Code Mode, is how one measured parity leg
/// reported success while writing nothing.
#[test]
fn code_mode_is_only_forced_on_a_model_measured_with_it() {
    assert!(code_mode(None), "the CLI default is the measured one");
    assert!(!code_mode(Some("gpt-5.3-codex-spark")));
    assert!(
        !code_mode(Some("some-model-nobody-measured")),
        "an unknown model is not assumed to support an under-development feature"
    );
    for model in CODE_MODE_MODELS {
        assert!(code_mode(Some(model)));
    }
}

#[test]
fn parses_measured_auth_without_retaining_key_or_email() {
    let hint = AgentProvider::Codex.auth_hint(None);
    assert!(!hint.contains("CODEX_HOME"));
    assert!(!hint.contains(".config/ccnm/agents"));
    for (code, text, logged) in [
        (0, "Logged in using ChatGPT\n", true),
        (1, "Not logged in\n", false),
        (0, "Logged in using an API key - DO_NOT_RETAIN", true),
    ] {
        let mut out = Output::exited(code, "");
        out.stderr = text.as_bytes().into();
        let auth = parse_auth(&out).unwrap();
        assert_eq!(auth.logged_in, logged);
        assert!(
            !serde_json::to_string(&auth)
                .unwrap()
                .contains("DO_NOT_RETAIN")
        );
    }
    let mut unexpected = Output::exited(0, "Logged in using ChatGPT");
    assert!(parse_auth(&unexpected).is_err());
    unexpected.stderr = b"Logged in using ChatGPT".to_vec();
    unexpected.timed_out = true;
    assert!(parse_auth(&unexpected).is_err());
}

#[test]
fn version_is_pinned_to_the_measured_cli() {
    assert_eq!(
        parse_version(&Output::exited(0, "codex-cli 0.154.0\n")).unwrap(),
        VERSION
    );
    for (code, text) in [
        (0, "codex-cli 0.154.1"),
        // The version this adapter used to require. Measured once, then
        // superseded; it is refused now exactly like any other unmeasured
        // build, which is what "exact match" has to mean to be worth
        // anything.
        (0, "codex-cli 0.153.4"),
        (1, "codex-cli 0.154.0"),
        (0, "0.154.0"),
    ] {
        assert!(parse_version(&Output::exited(code, text)).is_err());
    }
}

#[test]
fn result_uses_terminal_events_not_warnings_and_keeps_real_usage() {
    let r = result::parse(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/exec-seven-tools.stdout.jsonl"
    ))
    .unwrap();
    assert!(!r.is_error);
    assert_eq!(r.num_turns, 1);
    assert!(!r.warnings.is_empty());
    assert_eq!(r.usage.as_ref().unwrap().input_tokens, 90848);
    assert_eq!(r.usage.as_ref().unwrap().cached_input_tokens, 52352);
    assert!(r.summary().contains("cost and API timing not reported"));
    let wrapped = AgentResult::Codex(r);
    let value = serde_json::to_value(&wrapped).unwrap();
    assert_eq!(value["provider"], "codex");
    assert!(value.get("total_cost_usd").is_none());
    assert_eq!(
        serde_json::from_value::<AgentResult>(value).unwrap(),
        wrapped
    );
    assert!(serde_json::from_str::<AgentResult>(r#"{"provider":"unknown","result":"x"}"#).is_err());
}

#[test]
fn parses_real_rust_controller_to_runtime_round_trip() {
    let parsed = result::parse(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/internal-wiring/run.stdout.jsonl"
    ))
    .unwrap();
    assert!(!parsed.is_error);
    assert!(parsed.permission_denials.is_empty());
    assert_eq!(parsed.usage.as_ref().unwrap().input_tokens, 74066);
    let text = parsed.result.as_deref().unwrap();
    assert!(text.contains("CCNM_WIRING_PATCHED_7291"));
    assert!(text.contains("CCNM_AGENTS_FROM_RUNTIME_4317"));
    let report: crate::protocol::run::RunReport = payload::decode_json(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/internal-wiring/run-report.json"
    ))
    .unwrap();
    assert!(report.outcome.ok());
    assert_eq!(report.provider, AgentProvider::Codex);
    assert_eq!(report.result, Some(AgentResult::Codex(parsed)));
}

#[test]
fn denied_tools_and_failed_auth_are_distinct() {
    let denied = result::parse(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/exec-mcp.stdout.jsonl"
    ))
    .unwrap();
    assert!(!denied.is_error);
    assert_eq!(denied.permission_denials.len(), 3);
    let failed = result::parse(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/exec-logged-out.stdout.jsonl"
    ))
    .unwrap();
    assert!(failed.is_error);
    assert!(failed.usage.is_none());
    assert!(failed.result.unwrap().contains("401 Unauthorized"));
}

#[test]
fn truncated_or_malformed_result_never_becomes_success() {
    for bytes in [
        b"".as_slice(),
        b"not json",
        br#"{"type":"thread.started","thread_id":"x"}
{"type":"turn.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
        br#"{"type":"thread.started","thread_id":"x"}
{"type":"turn.started"}
{"type":"turn.completed","usage":{}}"#,
    ] {
        assert!(result::parse(bytes).is_err());
    }
}

#[test]
fn launch_modes_use_measured_flags_without_sending_home_to_runtime() {
    for mode in [
        Mode::Print {
            prompt: "-quoted '\n中文".into(),
        },
        Mode::Interactive {
            prompt: Some("opening".into()),
        },
        Mode::Interactive { prompt: None },
    ] {
        let spec = spec(mode);
        let cmd = build_launch_cmd(
            Path::new("/agent/codex"),
            &spec,
            &Dir::at("/agent/session"),
            Path::new("/agent/private-codex"),
            Path::new("/agent/ccnm"),
            None,
        )
        .unwrap();
        assert!(
            cmd.env
                .iter()
                .any(|(k, v)| k == "CODEX_HOME" && v == "/agent/private-codex")
        );
        assert!(!cmd.display().contains("/agent/private-codex"));
        assert!(cmd.display().contains("agents.enabled=false"));
        assert!(!cmd.display().contains("--permission-mode"));
        if spec.mode.is_interactive() {
            assert!(cmd.stdin.is_none());
            assert!(!cmd.args.iter().any(|a| a == "exec"));
            if let Mode::Interactive {
                prompt: Some(prompt),
            } = &spec.mode
            {
                assert_eq!(cmd.args[cmd.args.len() - 2], "--");
                assert_eq!(cmd.args.last().unwrap(), prompt.as_str());
            }
        } else {
            assert_eq!(cmd.stdin.unwrap(), "-quoted '\n中文".as_bytes());
            assert!(cmd.args.iter().any(|a| a == "--json"));
        }
        let ssh = transport::command(&spec).unwrap();
        assert!(!ssh.display().contains("/agent/private-codex"));
        assert!(ssh.env_remove.iter().any(|key| key == "CODEX_HOME"));
        assert!(ssh.args.iter().any(|arg| arg == "ForwardAgent=no"));
        let wire = ssh.args.last().unwrap().to_str().unwrap();
        let payload: crate::protocol::mcp::ServePayload = payload::decode(wire).unwrap();
        assert_eq!(payload.provider, AgentProvider::Codex);
        assert_eq!(payload.protocol, 2);
        assert_eq!(payload.root, Path::new("/runtime/project"));
    }
}

#[test]
fn home_metadata_rejects_shared_permissions_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ccnm-codex-home-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(validate_home(&root).is_ok());
    let auth = root.join("auth.json");
    std::fs::write(&auth, "synthetic-non-credential").unwrap();
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(validate_home(&root).is_err());
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(validate_home(&root).is_ok());
    std::fs::rename(&auth, root.join("not-auth")).unwrap();
    symlink("not-auth", &auth).unwrap();
    assert!(validate_home(&root).is_err());
    std::fs::remove_file(&auth).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validate_home(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bound_named_profile_drives_auth_launch_and_redaction_without_runtime_egress() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ccnm-codex-instance-{}", crate::session::new_id()));
    let home = root.join("private-codex");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut spec = spec(Mode::Print {
        prompt: "fixture".into(),
    });
    spec.protocol = 3;
    spec.runtime_node = Some("runtime".into());
    spec.agent_identity = Some(crate::instance::AgentIdentity {
        node: "worker".into(),
        instance: "codex-extra".into(),
        provider: AgentProvider::Codex,
        profile_ref: "extra".into(),
    });
    spec.cwd = cwd;
    let cmd = launch_cmd_at(
        Path::new("/agent/codex"),
        &spec,
        &Dir::at(root.join("session")),
        Some(&home),
        None,
    )
    .unwrap();
    assert!(
        cmd.env
            .iter()
            .any(|(key, value)| key == "CODEX_HOME" && value == home.as_os_str())
    );
    assert!(
        !cmd.args
            .iter()
            .any(|arg| arg.to_string_lossy().contains(home.to_str().unwrap()))
    );

    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "codex-cli 0.154.0\n"));
    let mut auth = Output::exited(0, "");
    auth.stderr = b"Logged in using ChatGPT\n".to_vec();
    runner.push(auth);
    let report = report_at(
        Some(Path::new("/agent/codex")),
        Some(&home),
        &runner,
        Ask::Everything,
    );
    assert_eq!(report.version, Ok(VERSION.into()));
    assert!(report.auth.unwrap().logged_in);
    assert!(
        runner.calls()[1]
            .env
            .iter()
            .any(|(key, value)| key == "CODEX_HOME" && value == home.as_os_str())
    );

    let jsonl = format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"thread\"}}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"error\",\"message\":\"warning at {}/config.toml\"}}}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}\n",
        home.display()
    );
    let parsed = AgentProvider::Codex
        .parse_result_at(jsonl.as_bytes(), Some(&home))
        .unwrap();
    let serialized = serde_json::to_string(&parsed).unwrap();
    assert!(serialized.contains("<agent-private-config>"));
    assert!(!serialized.contains(home.to_str().unwrap()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_messages_cannot_be_sent_as_legacy_claude_protocol() {
    let mut s = spec(Mode::Print {
        prompt: "hi".into(),
    });
    let bytes = serde_json::to_vec(&s).unwrap();
    assert!(payload::decode_json::<Spec>(&bytes).is_ok());
    s.protocol = 1;
    assert!(payload::decode_json::<Spec>(&serde_json::to_vec(&s).unwrap()).is_err());
    let mut value = serde_json::to_value(&s).unwrap();
    value["provider"] = "unknown".into();
    assert!(payload::decode_json::<Spec>(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn invalid_home_and_claude_policy_are_rejected_without_interpretation() {
    let mut s = spec(Mode::Print {
        prompt: "hi".into(),
    });
    s.provider_config_dir = Some("/do-not-read".into());
    assert!(
        validate_spec(&s)
            .unwrap_err()
            .message()
            .contains("caller must not supply")
    );
    s.provider_config_dir = None;
    s.permission_mode = PermissionMode::BypassPermissions;
    assert!(validate_spec(&s).is_err());
    s.permission_mode = PermissionMode::default();
    s.runtime = None;
    assert!(
        validate_spec(&s)
            .unwrap_err()
            .message()
            .contains("colocated")
    );
}

#[test]
fn root_context_follows_measured_override_priority_and_budget() {
    let root = std::env::temp_dir().join(format!("ccnm-codex-context-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("AGENTS.md"), "base").unwrap();
    assert_eq!(
        context::find(&root, 100).unwrap().unwrap().source,
        "AGENTS.md"
    );
    std::fs::write(root.join("AGENTS.override.md"), "").unwrap();
    let empty = context::find(&root, 100).unwrap().unwrap();
    assert_eq!(empty.source, "AGENTS.override.md");
    assert!(empty.text.is_empty());
    std::fs::write(root.join("AGENTS.override.md"), "中文\n".repeat(10000)).unwrap();
    let doc = context::find(&root, context::budget("fixture"))
        .unwrap()
        .unwrap();
    assert!(doc.truncated());
    assert!(
        context::instructions("fixture", Some(&doc)).len()
            <= super::super::context::MAX_INSTRUCTIONS_BYTES
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn controller_rejects_network_supplied_home_before_probing() {
    use crate::controller::{ReplyBody, Request, RequestBody, Tools};
    use crate::process::FakeRunner;
    let runner = FakeRunner::new();
    let tools = Tools {
        local: None,
        config: crate::Config::default(),
        config_path: None,
        runner: &runner,
        agents: crate::provider::AgentBinaries::default(),
        tmux: None,
        exe: "/agent/ccnm".into(),
    };
    let req = Request::new(RequestBody::AgentAuth {
        identity: None,
        provider: AgentProvider::Codex,
        config_dir: Some("/do-not-read".into()),
        ask: Ask::Everything,
    });
    assert_eq!(req.protocol, 2);
    assert!(matches!(
        crate::controller::answer(&req, &tools).body,
        ReplyBody::Error(_)
    ));
    assert!(runner.calls().is_empty());
}

#[test]
fn private_home_is_redacted_from_text_warning_and_denial() {
    let mut r = result::parse(include_bytes!(
        "../../../../../tests/fixtures/codex-0.153.4/exec-mcp.stdout.jsonl"
    ))
    .unwrap();
    r.result = Some("at /agent/private/config.toml".into());
    r.warnings.push("warning /agent/private".into());
    r.permission_denials
        .push(serde_json::json!({"error": {"message": "/agent/private/auth.json"}}));
    r.redact("/agent/private");
    let wire = serde_json::to_string(&r).unwrap();
    assert!(!wire.contains("/agent/private"));
    assert!(wire.contains("<agent-private-config>"));
}

#[test]
fn result_overflow_and_conflicting_threads_fail_closed() {
    let good =
        include_str!("../../../../../tests/fixtures/codex-0.153.4/exec-seven-tools.stdout.jsonl");
    let conflicting = format!("{good}\n{{\"type\":\"thread.started\",\"thread_id\":\"another\"}}");
    assert!(result::parse(conflicting.as_bytes()).is_err());
    let huge = good.replace(
        "\"input_tokens\":90848",
        "\"input_tokens\":18446744073709551615",
    );
    let overflow = format!(
        "{huge}\n{{\"type\":\"turn.started\"}}\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":0,\"reasoning_output_tokens\":0}}}}"
    );
    assert!(result::parse(overflow.as_bytes()).is_err());
    let invalid = good.replace("\"input_tokens\":90848", "\"input_tokens\":-1");
    assert!(result::parse(invalid.as_bytes()).is_err());
    let truncated = format!("{good}\n{{\"type\":\"turn.started\"}}");
    assert!(result::parse(truncated.as_bytes()).is_err());
}

#[test]
fn context_does_not_follow_a_root_instruction_symlink() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!(
        "ccnm-codex-link-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::write(root.join("outside"), "not authorized project context").unwrap();
    symlink(root.join("outside"), root.join("project/AGENTS.md")).unwrap();
    assert!(context::find(&root.join("project"), 1000).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

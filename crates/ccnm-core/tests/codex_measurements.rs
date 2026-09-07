//! Measured CLI observations only. These do not enable a Codex provider or
//! substitute for authenticated interactive/Controller/SSH verification.
use serde_json::Value;

fn events(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn captured_codex_version_and_login_streams_are_explicit() {
    assert_eq!(
        include_str!("../../../tests/fixtures/codex-0.153.4/version.txt"),
        "codex-cli 0.153.4\n"
    );
    let logged_in: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/auth-current.json"
    ))
    .unwrap();
    let logged_out: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/auth-empty-home.json"
    ))
    .unwrap();
    assert_eq!(logged_in["exit_code"], 0);
    assert_eq!(logged_in["stdout"], "");
    assert_eq!(logged_in["stderr"], "Logged in using ChatGPT\n");
    assert_eq!(logged_out["exit_code"], 1);
    assert_eq!(logged_out["stderr"], "Not logged in\n");
}

#[test]
fn real_codex_called_all_seven_runtime_tools_successfully() {
    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/exec-seven-tools.stdout.jsonl"
    ));
    let calls: Vec<_> = events
        .iter()
        .filter(|event| {
            event["type"] == "item.completed" && event["item"]["type"] == "mcp_tool_call"
        })
        .map(|event| &event["item"])
        .collect();
    let names: Vec<_> = calls
        .iter()
        .map(|item| item["tool"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "workspace_info",
            "list_files",
            "search_text",
            "read_file",
            "apply_patch",
            "exec_command",
            "read_output"
        ]
    );
    for item in calls {
        assert_eq!(item["server"], "ccnm");
        assert_eq!(item["status"], "completed");
        assert!(item["error"].is_null());
    }
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/manifest.json"
    ))
    .unwrap();
    assert_eq!(
        manifest["observed_seven_tools"]["runtime_file"],
        "CCNM_RUNTIME_PATCHED_7319\n"
    );
    assert_eq!(
        manifest["observed_seven_tools"]["agent_file"],
        "WRONG_AGENT_NODE_9520\n"
    );
}

#[test]
fn warnings_and_permission_denials_are_not_terminal_failure_events() {
    for stream in [
        include_str!("../../../tests/fixtures/codex-0.153.4/exec-no-tools.stdout.jsonl"),
        include_str!("../../../tests/fixtures/codex-0.153.4/exec-mcp.stdout.jsonl"),
    ] {
        let events = events(stream);
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "item.completed" && event["item"]["type"] == "error")
        );
        assert_eq!(events.last().unwrap()["type"], "turn.completed");
        assert!(!events.iter().any(|event| event["type"] == "turn.failed"));
    }
    let denied = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/exec-mcp.stdout.jsonl"
    ));
    assert_eq!(
        denied
            .iter()
            .filter(|event| event["type"] == "item.completed"
                && event["item"]["status"] == "failed"
                && event["item"]["error"]["message"]
                    == "MCP tool call requires approval, but approval policy is never")
            .count(),
        3
    );
}

#[test]
fn real_failed_turn_has_a_terminal_event_after_retries() {
    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/exec-logged-out.stdout.jsonl"
    ));
    let terminal = events.last().unwrap();
    assert_eq!(terminal["type"], "turn.failed");
    assert!(
        terminal["error"]["message"]
            .as_str()
            .unwrap()
            .contains("401 Unauthorized")
    );
    assert!(!events.iter().any(|event| event["type"] == "turn.completed"));
}

#[test]
fn usage_is_measured_but_claude_cost_and_timing_fields_are_not_reported() {
    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/exec-seven-tools.stdout.jsonl"
    ));
    let terminal = events.last().unwrap();
    assert_eq!(terminal["type"], "turn.completed");
    let usage = &terminal["usage"];
    assert_eq!(usage["input_tokens"], 90848);
    assert_eq!(usage["cached_input_tokens"], 52352);
    assert_eq!(usage["cache_write_input_tokens"], 0);
    assert_eq!(usage["output_tokens"], 337);
    assert_eq!(usage["reasoning_output_tokens"], 0);
    assert!(terminal.get("total_cost_usd").is_none());
    assert!(terminal.get("duration_api_ms").is_none());
}

#[test]
fn native_tool_filtering_is_recorded_separately_from_the_sandbox() {
    let denied = include_str!("../../../tests/fixtures/codex-0.153.4/exec-native-patch.stderr.txt");
    assert!(denied.contains("writing is blocked by read-only sandbox"));
    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/exec-registry-excluded.stdout.jsonl"
    ));
    let reported = events
        .iter()
        .rfind(|event| event["item"]["type"] == "agent_message")
        .unwrap()["item"]["text"]
        .as_str()
        .unwrap();
    // This is a copied runtime registry observation, not a tool permission
    // guarantee for other modes or versions.
    let registry: Value = serde_json::from_str(reported).unwrap();
    assert_eq!(registry["nativePatch"], "undefined");
    assert_eq!(registry["tools"].as_array().unwrap().len(), 8);
    assert!(
        !registry["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "apply_patch")
    );
}

#[test]
fn reverse_interactive_uses_independent_agent_login_without_opening_provider() {
    let auth: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/auth-status.json"
    ))
    .unwrap();
    assert_eq!(auth["exit_code"], 0);
    assert_eq!(auth["logged_in_using_chatgpt"], true);
    assert_eq!(auth["mode"], "0o700");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/manifest.json"
    ))
    .unwrap();
    assert_eq!(manifest["auth_file_mode"], "0600");
    assert_eq!(manifest["auth_file_is_symlink"], false);
    assert_eq!(manifest["provider_enabled"], false);
    assert_eq!(
        ccnm_core::provider::AgentProvider::current(),
        ccnm_core::provider::AgentProvider::Claude
    );
}

#[test]
fn detach_and_reattach_preserved_the_same_codex_and_mcp_processes() {
    let rows = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-observations.jsonl"
    ));
    let phases: Vec<_> = ["attached", "detached", "reattached"]
        .iter()
        .map(|phase| rows.iter().find(|row| row["phase"] == *phase).unwrap())
        .collect();
    for (index, expected) in [1, 0, 1].iter().enumerate() {
        assert_eq!(phases[index]["attached"], *expected);
        assert_eq!(phases[index]["pane_dead"], false);
        for key in ["server_pid", "supervisor_pid", "child_pids", "mcp_pids"] {
            assert_eq!(phases[index][key], phases[0][key]);
        }
    }
    let context: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-context.json"
    ))
    .unwrap();
    // managername alone cannot tell whether the official login is usable.
    assert_eq!(context["manager"], "Background");
    assert_eq!(context["login_confirmed"], true);
}

#[test]
fn closed_transport_was_observed_before_resume_created_a_new_connection() {
    let failure = include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-disconnect.txt"
    );
    assert!(failure.contains("Transport closed"));
    let recovered =
        include_str!("../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-resumed.txt");
    assert!(recovered.contains("1→CCNM_TMUX_RUNTIME_7319"));
    assert!(recovered.contains("CCNM_RESUME_RECONNECTED"));
    let rows = events(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-observations.jsonl"
    ));
    let before = rows.iter().find(|row| row["phase"] == "attached").unwrap();
    let after = rows.iter().find(|row| row["phase"] == "resumed").unwrap();
    assert_eq!(before["server_pid"], after["server_pid"]);
    assert_ne!(before["child_pids"], after["child_pids"]);
    assert!(
        after["mcp_pids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pid| !before["mcp_pids"].as_array().unwrap().contains(pid))
    );
}

#[test]
fn runtime_environment_and_completed_session_receipts_keep_the_boundary() {
    let runtime: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/runtime-probe-result.json"
    ))
    .unwrap();
    assert_eq!(runtime["single_process"], true);
    assert_eq!(runtime["tools"].as_array().unwrap().len(), 7);
    assert_eq!(runtime["agent_home_in_transport"], false);
    let output: Value = serde_json::from_str(
        runtime["retained_output"]
            .as_str()
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output["sensitive_environment_keys"], serde_json::json!([]));
    assert_eq!(output["cwd"], "/runtime-fixture/project");
    for receipt in [
        include_str!(
            "../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-first-exit.json"
        ),
        include_str!("../../../tests/fixtures/codex-0.153.4/reverse-interactive/tmux-exit.json"),
    ] {
        let exit: Value = serde_json::from_str(receipt).unwrap();
        assert_eq!(exit["exit_code"], 0);
        assert_eq!(exit["agent_file"], "WRONG_LOCAL_AGENT_9520\n");
    }
    let cleanup: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.153.4/reverse-interactive/cleanup.json"
    ))
    .unwrap();
    assert_eq!(cleanup["orphaned_observed_mcp_pids"], serde_json::json!([]));
    assert_eq!(cleanup["dedicated_agent_home_preserved"], true);
}

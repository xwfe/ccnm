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

// ---------------------------------------------------------------------
// codex-cli 0.154.0 — the version this adapter now requires. The 0.153.4
// block above stays as the regression baseline: those bytes are what the
// parser was first written against, and a change that breaks them is a
// change in behaviour, not in version.
// ---------------------------------------------------------------------

/// The measured run on 0.154.0: every ccnm tool reached, the work tree
/// changed, the Agent's own directory untouched.
#[test]
fn the_measured_0_154_0_run_reached_all_seven_tools_and_stayed_inside_the_workspace() {
    let outcome: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.154.0/seven-tools.json"
    ))
    .unwrap();
    assert_eq!(outcome["version"], "codex-cli 0.154.0");
    assert_eq!(outcome["exit_code"], 0);
    assert_eq!(outcome["timed_out"], false);
    assert_eq!(outcome["terminal_event"], "turn.completed");
    // The sentinel in the Runtime work tree was rewritten, and the one in
    // the Agent's directory was not: the model worked through ccnm's tools
    // on the far side, not on its own filesystem.
    //
    // Two newlines, not one, and that is the model's doing rather than
    // ccnm's: it replaced `CCNM_RUNTIME_SENTINEL_7319` -- which does not
    // include the line's own newline -- with `CCNM_RUNTIME_PATCHED_7319\n`,
    // so the file kept the newline that was already there. Reproduced on
    // two separate measured runs. ccnm wrote exactly the edit it was given,
    // which is why the byte-exact criterion is worth keeping strict.
    assert_eq!(outcome["runtime_file"], "CCNM_RUNTIME_PATCHED_7319\n\n");
    assert_eq!(outcome["agent_file"], "WRONG_AGENT_NODE_9520\n");

    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.154.0/seven-tools.stdout"
    ));
    let completed: Vec<&str> = events
        .iter()
        .filter(|event| event["type"] == "item.completed")
        .map(|event| &event["item"])
        .filter(|item| item["type"] == "mcp_tool_call" && item["status"] == "completed")
        .filter_map(|item| item["tool"].as_str())
        .collect();
    for tool in [
        "workspace_info",
        "list_files",
        "search_text",
        "read_file",
        "apply_patch",
        "exec_command",
        "read_output",
    ] {
        assert!(completed.contains(&tool), "{tool} never completed");
    }
}

/// `all_tools_succeeded` is false in that capture, and the reason matters:
/// the model guessed at `apply_patch`'s shape four times and ccnm refused
/// each guess by name before the fifth one worked.
///
/// That is the tool contract holding, not a version regression -- including
/// the refusal of Codex's own `*** Begin Patch` format, which ccnm does not
/// accept. It is recorded here so nobody later reads the false flag as a
/// broken adapter.
#[test]
fn the_patch_tool_refused_every_wrong_shape_before_one_worked() {
    // The Code Mode capture, kept because it is the evidence behind the
    // gate: this is the launch shape ccnm no longer produces for a model
    // that does not advertise Code Mode.
    let events = events(include_str!(
        "../../../tests/fixtures/codex-0.154.0/seven-tools-code-mode.stdout"
    ));
    let refusals: Vec<String> = events
        .iter()
        .filter(|event| event["type"] == "item.completed")
        .map(|event| &event["item"])
        .filter(|item| item["type"] == "mcp_tool_call" && item["status"] == "failed")
        .map(|item| {
            item["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(refusals.len(), 4, "{refusals:?}");
    // Each refusal names what was wrong. None of them is a silent success.
    assert!(refusals.iter().any(|r| r.contains("missing field `files`")));
    assert!(refusals.iter().any(|r| r.contains("op is required")));
    assert!(
        refusals
            .iter()
            .any(|r| r.contains("unknown variant `replace`"))
    );
    assert!(
        refusals
            .iter()
            .any(|r| r.contains("needs at least one edit"))
    );
}

/// What the launch actually carried on 0.154.0: the model the instance
/// named, and the new feature that had to be turned off for it.
#[test]
fn the_measured_launch_named_its_model_and_disabled_the_new_exec_path() {
    let outcome: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/codex-0.154.0/seven-tools.json"
    ))
    .unwrap();
    let argv: Vec<&str> = outcome["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = argv.iter().position(|a| *a == "--model").expect("--model");
    assert_eq!(argv[at + 1], "gpt-5.3-codex-spark");
    // 0.154.0 ships unified_exec_tty stable and on. It is another way to
    // run something without going through ccnm's tools, so it is disabled
    // exactly like unified_exec -- a different name the old list did not
    // cover, which is the whole reason the version is pinned.
    let disabled: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "--disable")
        .map(|w| w[1])
        .collect();
    assert!(disabled.contains(&"unified_exec"), "{disabled:?}");
    assert!(disabled.contains(&"unified_exec_tty"), "{disabled:?}");
}

/// The adapter still parses what the new CLI prints. This is the half of
/// the re-measurement that mattered: a field moving in that stream would
/// not fail loudly, it would be read wrong.
#[test]
fn the_result_parser_reads_the_0_154_0_stream() {
    use ccnm_core::provider::{AgentProvider, AgentResult};
    let parsed = AgentProvider::Codex
        .parse_result(include_bytes!(
            "../../../tests/fixtures/codex-0.154.0/seven-tools.stdout"
        ))
        .expect("the measured stream parses");
    let AgentResult::Codex(result) = parsed else {
        panic!("a Codex stream must parse as a Codex result");
    };
    assert!(!result.is_error);
    let usage = result.usage.expect("0.154.0 still reports usage");
    assert!(usage.input_tokens > 0, "{usage:?}");
    assert!(usage.output_tokens > 0, "{usage:?}");
}

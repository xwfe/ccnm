//! Runs the real `ccnm` binary. Exit codes and stdout are the contract that
//! `ccnm run`, the other machine's ccnm and the user's shell see, so they
//! are asserted here rather than through the library API.
//!
//! Nothing here needs a second machine. The one ssh alias used targets a
//! name under the reserved `.invalid` TLD; `ssh -G` resolves it without
//! connecting.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ccnm_core::protocol::hello::{HelloReport, HelloRequest};
use ccnm_core::protocol::mcp::ProbeReport as McpProbeReport;
use ccnm_core::protocol::payload;

fn ccnm() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
    // Never let the developer's own config leak into a test.
    cmd.env_remove("CCNM_CONFIG");
    cmd
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A fresh directory with `root/` inside it and a config pointing there.
/// `home_bin` is what the work machine would invoke on this host.
fn setup(test: &str, home_bin: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            // allow_unconfined_exec so the runtime-safety rows are
            // warnings here: these tests are about the transport, and a
            // machine without a ccrun account would otherwise fail every
            // one of them for the same unrelated reason.
            "version = 1\n[hosts.work]\nssh = \"ccnm-test-nowhere.invalid\"\n[hosts.home]\nssh_from_work = \"ccnm-home\"\nccnm_bin = \"{home_bin}\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nallow_unconfined_exec = true\n",
            root.display()
        ),
    )
    .unwrap();
    (dir, config)
}

#[test]
fn version_prints_the_crate_version() {
    let out = ccnm().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        stdout(&out).trim(),
        format!("ccnm {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_subcommand_is_a_usage_error() {
    let out = ccnm().output().unwrap();
    assert_eq!(out.status.code(), Some(2), "clap usage error");
}

#[test]
fn doctor_with_missing_config_exits_config_code() {
    let out = ccnm()
        .args(["doctor", "--config", "/nonexistent/ccnm/config.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    let text = stdout(&out);
    assert!(
        text.contains("Config                  FAIL   CCNM_E_CONFIG"),
        "{text}"
    );
    assert!(text.contains("/nonexistent/ccnm/config.toml"), "{text}");
    assert!(
        text.ends_with("NOT READY (1 failed, 0 not checked)\n"),
        "{text}"
    );
}

#[test]
fn doctor_config_only_is_ready() {
    let out = ccnm()
        .args(["doctor", "--config"])
        .arg(fixture("config-valid.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.starts_with("ccnm doctor: config\n"), "{text}");
    assert!(
        text.contains("Workspaces              OK     xshun"),
        "{text}"
    );
    assert!(text.ends_with("\nREADY\n"), "{text}");
}

#[test]
fn doctor_rejects_a_typo_in_config() {
    let out = ccnm()
        .args(["doctor", "--config"])
        .arg(fixture("config-unknown-field.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    assert!(stdout(&out).contains("runtime_hots"), "{}", stdout(&out));
}

#[test]
fn doctor_unknown_workspace_exits_config_code() {
    let out = ccnm()
        .args(["doctor", "nope", "--config"])
        .arg(fixture("config-valid.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    assert!(stdout(&out).contains("defined: xshun"), "{}", stdout(&out));
}

#[test]
fn doctor_refuses_hybrid_backend() {
    let out = ccnm()
        .args(["doctor", "legacy", "--config"])
        .arg(fixture("config-hybrid.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("Backend                 FAIL   CCNM_E_CONFIG"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn internal_commands_are_hidden_from_help() {
    let out = ccnm().arg("--help").output().unwrap();
    let text = stdout(&out);
    assert!(text.contains("doctor"), "{text}");
    assert!(!text.contains("internal"), "{text}");
}

#[test]
fn internal_hello_answers_with_this_build_and_the_root() {
    let (dir, _config) = setup("hello", env!("CARGO_BIN_EXE_ccnm"));
    let wire = payload::encode(&HelloRequest::new(Some(dir.join("root")))).unwrap();
    let out = ccnm()
        .args(["internal", "hello", "--payload", &wire])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rep: HelloReport = payload::decode_json(&out.stdout).unwrap();
    assert_eq!(rep.ccnm_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(rep.user, std::env::var("USER").unwrap());
    assert!(rep.root.unwrap().is_ok());
    assert_eq!(
        rep.exe.unwrap().canonicalize().unwrap(),
        Path::new(env!("CARGO_BIN_EXE_ccnm"))
            .canonicalize()
            .unwrap()
    );
    // Exactly one JSON line on stdout, nothing else.
    assert_eq!(stdout(&out).lines().count(), 1);

    let wire = payload::encode(&HelloRequest::new(Some(dir.join("nope")))).unwrap();
    let out = ccnm()
        .args(["internal", "hello", "--payload", &wire])
        .output()
        .unwrap();
    let rep: HelloReport = payload::decode_json(&out.stdout).unwrap();
    assert!(!rep.root.unwrap().exists);
}

#[test]
fn garbage_payload_is_a_version_error() {
    let out = ccnm()
        .args(["internal", "hello", "--payload", "definitely-not-a-payload"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
    assert!(
        stderr(&out).starts_with("CCNM_E_VERSION:\n"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty(), "stdout must stay clean on error");

    let out = ccnm()
        .args(["internal", "probe", "--payload", "eyJwcm90b2NvbCI6OTl9"]) // {"protocol":99}
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
}

#[test]
fn doctor_against_unreachable_work_exits_work_unreachable() {
    // The real test binary stands in for ~/.local/bin/ccnm: same version.
    let (dir, config) = setup("unreachable", env!("CARGO_BIN_EXE_ccnm"));
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(20), "{text}");
    assert!(
        text.contains(&format!(
            "Home ccnm               OK     {} at {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_BIN_EXE_ccnm")
        )),
        "{text}"
    );
    assert!(
        text.contains("Work SSH                FAIL   CCNM_E_WORK_UNREACHABLE"),
        "{text}"
    );
    assert!(
        text.contains("Claude Code             SKIP   not checked: work SSH failed"),
        "{text}"
    );
    assert!(
        text.contains("Work controller         SKIP   not checked: work SSH failed"),
        "{text}"
    );
    assert!(
        text.contains("Remote MCP handshake    SKIP   not checked: work SSH failed"),
        "{text}"
    );
    assert!(
        text.contains("Workspace policy        SKIP   not implemented until phase 2"),
        "{text}"
    );
    // The project has no CLAUDE.md, which is fine and says so.
    assert!(
        text.contains("Project instructions    OK     no CLAUDE.md at"),
        "{text}"
    );
    assert!(
        text.contains("NOT READY (1 failed, 12 not checked)"),
        "{text}"
    );
    // Read-only: nothing appeared in the root.
    assert_eq!(std::fs::read_dir(dir.join("root")).unwrap().count(), 0);
}

#[test]
fn doctor_missing_home_ccnm_exits_version_code() {
    let (_dir, config) = setup("no-bin", "/nonexistent/ccnm-bin/ccnm");
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(11), "{text}");
    assert!(
        text.contains("Home ccnm               FAIL   CCNM_E_VERSION: /nonexistent/ccnm-bin/ccnm"),
        "{text}"
    );
}

/// The phase 1B proof, minus the network: this binary spawns itself as
/// `internal mcp-serve`, speaks MCP to it over pipes, and one process
/// answers every call.
#[test]
fn mcp_probe_local_speaks_to_one_persistent_server() {
    let (dir, config) = setup("mcp-local", env!("CARGO_BIN_EXE_ccnm"));
    // The project's own rules have to survive a real handshake: written
    // here, read by a separate process, and reported back by the client.
    std::fs::write(dir.join("root/CLAUDE.md"), "- 提交要小\n").unwrap();
    let out = ccnm()
        .args([
            "mcp", "probe", "xshun", "--local", "--calls", "25", "--config",
        ])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}\n{}", stderr(&out));
    let mut lines = text.lines();
    let summary = lines.next().unwrap();
    assert!(summary.contains("workspace_info x25"), "{summary}");
    assert!(summary.contains("throughout"), "{summary}");
    let rep: McpProbeReport = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(rep.server_name, "ccnm");
    assert_eq!(rep.server_version, env!("CARGO_PKG_VERSION"));
    let mut tools = rep.tools.clone();
    tools.sort();
    assert_eq!(
        tools,
        vec![
            "apply_patch",
            "exec_command",
            "list_files",
            "read_file",
            "read_output",
            "search_text",
            "workspace_info"
        ]
    );
    assert_eq!(rep.calls, 25);
    assert!(rep.single_process);
    assert!(rep.server_pid > 0);
    assert_eq!(
        rep.project_instructions.as_deref(),
        Some("CLAUDE.md, 15 bytes")
    );
    assert!(rep.instructions_bytes > 0);
    assert!(
        rep.tools_list_bytes < 16 * 1024,
        "schema budget: {} bytes",
        rep.tools_list_bytes
    );
    assert!(rep.call_p50_us <= rep.call_p95_us && rep.call_p95_us <= rep.call_max_us);
    // Read-only: the server added nothing to the root, and the CLAUDE.md
    // it read is still the only thing there.
    let left: Vec<String> = std::fs::read_dir(dir.join("root"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["CLAUDE.md".to_string()]);
}

#[test]
fn mcp_serve_refuses_a_missing_root_before_speaking_mcp() {
    let wire = payload::encode(&ccnm_core::protocol::mcp::ServePayload::new(
        "x",
        PathBuf::from("/nonexistent/ccnm-root"),
        "s",
    ))
    .unwrap();
    let out = ccnm()
        .args(["internal", "mcp-serve", "--payload", &wire])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(30), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with("CCNM_E_WRONG_WORKSPACE:\n"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty(), "nothing but MCP may go to stdout");
}

#[test]
fn ccnm_config_env_var_selects_the_config() {
    let out = ccnm()
        .args(["doctor"])
        .env("CCNM_CONFIG", fixture("config-valid.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
}

#[test]
fn verbose_logs_go_to_stderr_not_stdout() {
    let out = ccnm()
        .args(["-v", "doctor", "--config"])
        .arg(fixture("config-valid.toml"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("loading config"), "stderr: {err}");
    assert!(!stdout(&out).contains("loading config"));
}

/// The setup path someone new actually walks: two commands, no TOML by
/// hand, and running either of them twice is not a mistake.
#[test]
fn init_and_workspace_add_write_a_config_that_loads() {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let project = dir.join("myproj");
    std::fs::create_dir_all(&project).unwrap();
    let config = dir.join("config.toml");

    let init = || {
        ccnm()
            .args([
                "init",
                "--work",
                "work-alias",
                "--home",
                "home-alias",
                "--config",
            ])
            .arg(&config)
            .output()
            .unwrap()
    };
    let out = init();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("hosts.work.ssh = work-alias"),
        "{}",
        stdout(&out)
    );

    let written = std::fs::read_to_string(&config).unwrap();
    assert!(
        !written.contains("version"),
        "nothing writes a schema version any more: {written}"
    );

    // Again: nothing to change, and it says so instead of rewriting.
    let out = init();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("already says that"),
        "{}",
        stdout(&out)
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), written);

    // The workspace defaults to the directory you are standing in.
    let out = ccnm()
        .current_dir(&project)
        .args(["workspace", "add", "myproj", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("added workspaces.myproj"),
        "{}",
        stdout(&out)
    );

    let out = ccnm()
        .args(["workspace", "list", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(stdout(&out).contains("myproj"), "{}", stdout(&out));

    // And the config the whole rest of the program reads is valid.
    let out = ccnm()
        .args(["doctor", "myproj", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("Workspace config        OK"), "{text}");
    assert!(text.contains("Home workspace          OK"), "{text}");
}

/// Two projects with the same directory name is the ordinary case, not a
/// corner one -- `code/web` and `other/web`. Silently repointing the name
/// would change what `ccnm web` opens, and end a session running against
/// the old one the next time it started; that is the user's call.
#[test]
fn a_name_that_is_taken_is_refused_with_something_to_type() {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-collide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let first = dir.join("code/web");
    let second = dir.join("other/web");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let config = dir.join("config.toml");
    let ccnm_ws = |cwd: &Path, args: &[&str]| {
        ccnm()
            .current_dir(cwd)
            .args(["ws"])
            .args(args)
            .args(["--config"])
            .arg(&config)
            .output()
            .unwrap()
    };

    let out = ccnm()
        .args(["init", "--work", "w", "--home", "h", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // No name given: it comes from the directory.
    let out = ccnm_ws(&first, &["add"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("added workspaces.web"),
        "{}",
        stdout(&out)
    );

    // The other `web` cannot quietly take the name.
    let out = ccnm_ws(&second, &["add"]);
    assert_ne!(out.status.code(), Some(0));
    let err = stderr(&out);
    assert!(err.contains("already points at"), "{err}");
    assert!(
        err.contains("other-web"),
        "the suggestion names the parent: {err}"
    );
    assert!(err.contains("--replace"), "{err}");

    // The suggestion works as printed.
    let out = ccnm_ws(&second, &["add", "other-web"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // And a second name for a directory that already has one is refused
    // too: two names is two sessions on one project.
    let out = ccnm_ws(&first, &["add", "frontend"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("is already the workspace `web`"),
        "{}",
        stderr(&out)
    );

    // Explicitly asking to repoint is allowed.
    let third = dir.join("third/web");
    std::fs::create_dir_all(&third).unwrap();
    let out = ccnm_ws(&third, &["add", "web", "--replace"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("root:"), "{}", stdout(&out));
}

/// A workspace has nowhere to go before there is a config, and the error
/// says the command that makes one rather than the schema rule it broke.
#[test]
fn adding_a_workspace_before_init_says_to_init() {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-noinit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = ccnm()
        .current_dir(&dir)
        .args(["workspace", "add", "x", "--config"])
        .arg(dir.join("config.toml"))
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("ccnm init"), "{}", stderr(&out));
}

/// `ccnm <workspace>` is `ccnm run <workspace>`: the thing people do all
/// day should not need the word.
/// Sitting at the work machine, the same command works: the config there
/// knows only how to reach the projects, so a workspace it does not
/// define is a question for the other side rather than an error. It must
/// reach ssh -- proving it delegated -- and not stop at "not defined".
#[test]
fn on_the_work_machine_an_unknown_workspace_is_asked_about_not_refused() {
    let out = ccnm()
        .args(["xshun", "--config"])
        .arg(fixture("config-work-side.toml"))
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        !err.contains("not defined"),
        "the work machine has no workspace list; it must ask home: {err}"
    );
    assert!(
        err.contains("could not start") || err.contains("no-such-host-for-tests"),
        "it should have tried to reach home: {err}"
    );
    // --print has to run where the project is; say so rather than
    // starting something that cannot work.
    let out = ccnm()
        .args(["xshun", "--print", "hi", "--config"])
        .arg(fixture("config-work-side.toml"))
        .output()
        .unwrap();
    assert!(
        stderr(&out).contains("where the projects are"),
        "{}",
        stderr(&out)
    );
}

/// The work machine starts a session by running this exact line on the
/// home machine. It is a string literal in `launcher::start_from_work`,
/// so nothing in the compiler ties the two together: rename the flag and
/// the work-side entry keeps building and breaks at the far end, where
/// the complaint is about an argument and the person is looking at a
/// workspace.
#[test]
fn the_line_the_work_machine_sends_home_is_one_home_accepts() {
    // Both shapes: without an opening line, and with one -- which adds
    // --prompt-stdin and is the half where the two sides are furthest
    // apart, the flag being what tells home the prompt is coming.
    for args in [
        vec!["run", "xshun", "--detached"],
        vec!["run", "xshun", "--detached", "--prompt-stdin"],
    ] {
        let mut cmd = ccnm();
        cmd.args(&args)
            .arg("--config")
            .arg(fixture("config-valid.toml"));
        let out = with_stdin(&mut cmd, "fix the failing test");
        let err = stderr(&out);
        assert!(
            !err.contains("unexpected argument") && !err.contains("Usage"),
            "home must accept the line work sends it ({args:?}): {err}"
        );
        // Past clap and into the local preflight, which is as far as it
        // can get without the project being here.
        assert_eq!(out.status.code(), Some(30), "{args:?}: {err}");
    }
}

/// On the work machine the session is *on this machine*, so attach,
/// status and stop are local: the workspace name is all they need. If any
/// of them reached for the home alias, being let back into a running
/// session -- or ending one -- would depend on the link being up, which
/// is exactly when somebody needs to end one.
#[test]
fn on_the_work_machine_attach_status_and_stop_stay_local() {
    let state = std::env::temp_dir().join(format!("ccnm-cli-{}-worklocal", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    // A name nothing can have a live session for, so `stop` cannot end
    // something belonging to whoever is running the tests.
    let workspace = "ccnm-test-no-such-workspace";
    for verb in ["attach", "status", "stop"] {
        let out = ccnm()
            .env("XDG_STATE_HOME", &state)
            .args([verb, workspace, "--config"])
            .arg(fixture("config-work-side.toml"))
            .output()
            .unwrap();
        let said = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !said.contains("no-such-host-for-tests"),
            "`{verb}` went looking for the home machine: {said}"
        );
        // Positive half, because "no alias in the output" is also true of
        // a config error. Each verb has to have reached its *local*
        // answer: tmux replied, or said it is not installed.
        assert!(
            (said.contains(workspace) || said.contains("tmux")) && !said.contains("not defined"),
            "`{verb}` did not answer from this machine: {said}"
        );
        // 0 nothing to report, 3 nothing running, 35 no tmux here. 10
        // would mean it fell through to the workspace lookup, which on
        // this machine can only fail.
        assert!(
            matches!(out.status.code(), Some(0 | 3 | 35)),
            "`{verb}` exited {:?}, which is not a local answer: {said}",
            out.status.code()
        );
    }
}

/// `ccnm result` on the work machine reads the session off this disk.
///
/// The session's files -- what Claude printed, how it ended -- were
/// written here by this machine's own supervisor. Asking home for them
/// would mean ssh'ing there so that home could ssh straight back to read
/// files that were under the person's feet the whole time, and it would
/// fail outright when the link is down, which is one of the times you
/// most want to see what a run produced. Until this branch existed the
/// command answered `workspace 'x' is not defined` here, which is true of
/// the config and useless as an answer.
#[test]
fn on_the_work_machine_result_is_read_off_this_disk() {
    let xdg = std::env::temp_dir().join(format!("ccnm-cli-{}-workresult", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg);
    let workspace = "ccnm-test-result";
    let id = "7c1d9f60-0a11-4c22-9d33-8e44f5566a77";
    let dir = xdg.join("ccnm/sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("session.json"),
        format!(
            r#"{{"protocol":{},"id":"{id}","workspace":"{workspace}","root":"/home/projects/x","home_alias":"home","home_ccnm_bin":"/opt/home/ccnm","permission_mode":"plan","mode":{{"mode":"print","prompt":"what broke"}},"timeout_secs":600,"cwd":"/tmp"}}"#,
            payload::PROTOCOL
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("stdout"),
        r#"{"is_error":false,"result":"the assertion on line 40","num_turns":3}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("exit"),
        r#"{"exit_code":0,"timed_out":false,"duration_ms":8200}"#,
    )
    .unwrap();

    let out = ccnm()
        .env("XDG_STATE_HOME", &xdg)
        .args(["result", workspace, "--config"])
        .arg(fixture("config-work-side.toml"))
        .output()
        .unwrap();
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(
        said.contains("the assertion on line 40"),
        "Claude's answer, read from this machine's own session directory: {said}"
    );
    assert!(said.contains(id), "which session it was: {said}");
    assert!(
        !said.contains("no-such-host-for-tests") && !said.contains("not defined"),
        "it must not have gone looking for home: {said}"
    );
}

/// A stand-in for `ssh`, put first on PATH so every remote call the real
/// binary makes lands here instead of on the network.
///
/// It records each argv it is given -- one argument per line, a blank
/// line between calls -- and then answers the way `answers` says, which
/// is the body of a `case` over `"$*"`. Possible because ccnm runs its
/// own ssh by name; only the transport line in a session's `mcp.json` is
/// written with an absolute path, and that one Claude runs, not ccnm.
///
/// This is the layer the library tests cannot reach: `main.rs` deciding
/// which side it is on, clap, the config file, and what is printed for
/// a person -- with the real binary and a scripted far end.
struct FakeSsh {
    bin: PathBuf,
    log: PathBuf,
    fed: PathBuf,
}

impl FakeSsh {
    fn install(dir: &Path, answers: &str) -> FakeSsh {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.join("ssh-argv");
        let fed = dir.join("ssh-stdin");
        // stdin is only drained when the remote line asks for it. `cat`
        // on every call would block the attach hop, whose stdin is this
        // test process's own and never reaches EOF.
        let script = format!(
            "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; printf '\\n'; }} >> '{}'\ncase \"$*\" in\n  *--prompt-stdin*) cat >> '{}' ;;\nesac\ncase \"$*\" in\n{answers}\nesac\n",
            log.display(),
            fed.display()
        );
        std::fs::write(bin.join("ssh"), script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        FakeSsh { bin, log, fed }
    }

    /// Everything that was piped down the connection since the last
    /// `forget`.
    fn fed_in(&self) -> String {
        std::fs::read_to_string(&self.fed).unwrap_or_default()
    }

    /// `PATH` with the fake in front of everything else.
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    /// Every argv the fake was run with since the last `forget`, in order.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .split("\n\n")
            .filter(|call| !call.trim().is_empty())
            .map(|call| call.lines().map(str::to_string).collect())
            .collect()
    }

    fn forget(&self) {
        let _ = std::fs::remove_file(&self.log);
        let _ = std::fs::remove_file(&self.fed);
    }
}

/// Run a prepared `ccnm` with `input` on its stdin and wait for it.
/// `Command::output()` gives the child `/dev/null`, which is right for
/// every other test here and useless for the one flag that reads stdin.
fn with_stdin(cmd: &mut Command, input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// ControlPath expands to at most 103 bytes and macOS `temp_dir()` is
/// most of that by itself, so the state directory goes under /tmp.
fn short_state(test: &str) -> PathBuf {
    let dir = PathBuf::from("/tmp/ccnm-cli-st").join(format!("{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The words after `-T`/`-t` in one recorded ssh call: the alias, then
/// the remote command line.
fn remote_line(call: &[String]) -> Vec<String> {
    let at = call
        .iter()
        .position(|a| a == "-T" || a == "-t")
        .unwrap_or_else(|| panic!("no -T/-t in {call:?}"));
    call[at + 1..].to_vec()
}

/// Direction one, through the real binary: `ccnm run <ws>` typed at the
/// home machine, with the work machine scripted.
///
/// `--detached` is the flag the *other* direction depends on: the work
/// machine sends it so that home does not try to give a terminal to a
/// session whose person is elsewhere. So the two halves are pinned here
/// side by side -- with the flag, exactly one ssh and the terminal stays;
/// without it, a second ssh carries the terminal over.
#[test]
fn sitting_at_home_detached_starts_the_session_and_keeps_the_terminal_here() {
    use ccnm_core::protocol::PROTOCOL;
    use ccnm_core::protocol::run::{AttachRequest, StartReport, StartRequest, StatusReport};

    let dir = std::env::temp_dir().join(format!("ccnm-cli-{}-home-loop", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    // Nothing here is a default: the permission mode, the config dir and
    // the opening line all have to be seen arriving, not assumed.
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "version = 1\n[hosts.work]\nssh = \"ccnm-test-nowhere.invalid\"\nclaude_config_dir = \"/x/claude\"\n[hosts.home]\nssh_from_work = \"ccnm-home\"\nccnm_bin = \"/opt/home/ccnm\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nclaude_permission_mode = \"plan\"\nallow_unconfined_exec = true\n",
            root.display()
        ),
    )
    .unwrap();

    let started = serde_json::to_string(&StartReport {
        protocol: PROTOCOL,
        session: Some("2f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b".into()),
        session_dir: Some(PathBuf::from("/Users/bing/.local/state/ccnm/sessions/2f1e")),
        tmux_session: "ccnm-xshun".into(),
        server_pid: 4242,
        already_running: false,
        replaced: None,
        controller: None,
        context: None,
    })
    .unwrap();
    let nothing_running = serde_json::to_string(&StatusReport {
        protocol: PROTOCOL,
        tmux: Ok("3.7c".into()),
        sessions: Vec::new(),
    })
    .unwrap();
    let ssh = FakeSsh::install(
        &dir,
        &format!(
            "  *'internal work-start'*) printf '%s\\n' '{started}' ;;\n  *'internal attach'*) exit 0 ;;\n  *'internal work-status'*) printf '%s\\n' '{nothing_running}' ;;"
        ),
    );
    let state = short_state("home-loop");
    let prepared = |args: &[&str]| {
        let mut cmd = ccnm();
        cmd.env("PATH", ssh.path())
            .env("XDG_STATE_HOME", &state)
            .args(args)
            .args(["--config"])
            .arg(&config);
        cmd
    };
    let run = |args: &[&str]| prepared(args).output().unwrap();
    let pipe_in = |args: &[&str], input: &str| with_stdin(&mut prepared(args), input);

    // ---- with --detached: one ssh, and the terminal stays here -------
    let out = run(&["run", "xshun", "fix the failing test", "--detached"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(
        err.contains("attach when you want it: ccnm attach xshun"),
        "{err}"
    );
    let calls = ssh.calls();
    assert_eq!(calls.len(), 1, "one ssh, to one machine: {calls:?}");
    let line = remote_line(&calls[0]);
    assert_eq!(
        line[..4],
        [
            "ccnm-test-nowhere.invalid",
            "~/.local/bin/ccnm",
            "internal",
            "work-start"
        ],
        "hop 1 goes to the work machine, running the work machine's ccnm"
    );
    assert_eq!(line[4], "--payload");
    // The request, decoded the way the work machine decodes it: the
    // alias to come *back* on, and everything Claude is started with.
    let req: StartRequest = payload::decode(&line[5]).unwrap();
    assert_eq!(req.workspace, "xshun");
    assert_eq!(req.root, root);
    assert_eq!(req.home_alias, "ccnm-home");
    assert_eq!(req.home_ccnm_bin, "/opt/home/ccnm");
    assert_eq!(req.claude_config_dir, Some(PathBuf::from("/x/claude")));
    assert_eq!(req.permission_mode, ccnm_core::config::PermissionMode::Plan);
    assert_eq!(req.prompt.as_deref(), Some("fix the failing test"));
    assert!(
        !calls[0].iter().any(|a| a == "-t"),
        "--detached must not ask for a terminal: {:?}",
        calls[0]
    );

    // ---- without it: the terminal goes over, then home asks how it went
    ssh.forget();
    let out = run(&["xshun"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let calls = ssh.calls();
    assert_eq!(
        calls.len(),
        3,
        "start, attach, then status: {:?}",
        calls.iter().map(|c| remote_line(c)).collect::<Vec<_>>()
    );
    assert!(
        calls[1].iter().any(|a| a == "-t"),
        "attach needs a terminal"
    );
    let attach = remote_line(&calls[1]);
    assert_eq!(
        attach[..3],
        ["ccnm-test-nowhere.invalid", "~/.local/bin/ccnm", "internal"]
    );
    assert_eq!(attach[3], "attach");
    let req: AttachRequest = payload::decode(&attach[5]).unwrap();
    assert_eq!(req.workspace, "xshun");
    assert!(
        err.contains("the session has ended"),
        "after the terminal comes back, home says what became of the session: {err}"
    );

    // ---- the same opening line, arriving on stdin --------------------
    //
    // This is home's half of what the work machine does: the work
    // machine cannot put free text on an ssh command line, so it pipes
    // the bytes and passes --prompt-stdin. What reaches Claude has to be
    // the same as if it had been typed here -- quotes and all, which the
    // payload is base64 precisely so that it can carry.
    ssh.forget();
    let out = pipe_in(
        &["run", "xshun", "--prompt-stdin", "--detached"],
        "fix the \"failing\" test\n",
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let calls = ssh.calls();
    assert_eq!(calls.len(), 1, "{err}");
    let line = remote_line(&calls[0]);
    let req: StartRequest = payload::decode(&line[5]).unwrap();
    assert_eq!(
        req.prompt.as_deref(),
        Some("fix the \"failing\" test"),
        "read off stdin, trailing newline off, quotes intact"
    );
}

/// Direction two, through the real binary: `ccnm <ws>` typed at the work
/// machine, with the home machine scripted.
///
/// This side has no workspace list, so it runs the user-facing command on
/// the home machine and attaches locally. Four things are pinned: the
/// exact line sent (that `--detached` is on it is what stops the far side
/// from waiting for a terminal that is here); that the attach then
/// happens *here*, over no ssh; that where the config says home keeps
/// ccnm is what gets run; and that an opening prompt is refused out loud
/// rather than dropped, which is what used to happen.
#[test]
fn sitting_at_work_the_start_goes_home_and_the_attach_stays_here() {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-{}-work-loop", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A name nothing can have a live session for on this machine.
    let workspace = "ccnm-test-loop";
    let ssh = FakeSsh::install(
        &dir,
        "  *' run ccnm-test-loop --detached') printf 'session   ccnm-ccnm-test-loop (started, tmux server pid 1)\\n' >&2 ;;",
    );
    let state = short_state("work-loop");
    let prepared = |args: &[&str], config: &Path| {
        let mut cmd = ccnm();
        cmd.env("PATH", ssh.path())
            .env("XDG_STATE_HOME", &state)
            .args(args)
            .args(["--config"])
            .arg(config);
        cmd
    };
    let run = |args: &[&str], config: &Path| prepared(args, config).output().unwrap();
    let pipe_in =
        |args: &[&str], config: &Path, input: &str| with_stdin(&mut prepared(args, config), input);
    let work_side = fixture("config-work-side.toml");

    // ---- the plain command: home starts it, this machine attaches -----
    let out = run(&[workspace], &work_side);
    let said = format!("{}{}", stdout(&out), stderr(&out));
    let calls = ssh.calls();
    assert_eq!(
        calls.len(),
        1,
        "one hop, and nothing decided here: {calls:?}"
    );
    assert_eq!(
        remote_line(&calls[0]),
        [
            "no-such-host-for-tests",
            "~/.local/bin/ccnm",
            "run",
            workspace,
            "--detached"
        ],
        "the line home receives is the one a person would type there, plus --detached"
    );
    assert!(
        said.contains("(started, tmux server pid 1)"),
        "what home said about the session is relayed as it was: {said}"
    );
    // Then the local attach, which on a machine with no such session
    // ends at tmux: no session (3), or no tmux at all (35). Either is a
    // local answer; 21 would mean it went looking for home again.
    assert!(
        matches!(out.status.code(), Some(3 | 35)),
        "attach must happen on this machine, exit was {:?}: {said}",
        out.status.code()
    );

    // ---- --detached: the same hop, and no attach ----------------------
    ssh.forget();
    let out = run(&[workspace, "--detached"], &work_side);
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert_eq!(ssh.calls().len(), 1);
    assert!(said.contains("(started, tmux server pid 1)"), "{said}");
    assert!(
        !said.contains("no live session"),
        "--detached must not try to attach: {said}"
    );

    // ---- where home keeps ccnm is read from this machine's config -----
    ssh.forget();
    let elsewhere = dir.join("elsewhere.toml");
    std::fs::write(
        &elsewhere,
        "[hosts.home]\nssh_from_work = \"no-such-host-for-tests\"\nccnm_bin = \"/opt/elsewhere/ccnm\"\n",
    )
    .unwrap();
    let out = run(&[workspace, "--detached"], &elsewhere);
    let calls = ssh.calls();
    assert_eq!(calls.len(), 1, "{}", stderr(&out));
    assert_eq!(
        remote_line(&calls[0])[..3],
        ["no-such-host-for-tests", "/opt/elsewhere/ccnm", "run"],
        "the configured path is the one that runs"
    );

    // ---- the opening line goes over, and it goes over on stdin -------
    //
    // It used to be dropped here without a word. It cannot go on the
    // remote command line -- that line is unquoted, and this prompt has
    // a quote and an apostrophe in it -- so the far side is told to read
    // it from stdin and the bytes go down the same connection.
    ssh.forget();
    let prompt = "fix the \"failing\" test, it's in mod tests";
    let out = run(&[workspace, prompt, "--detached"], &work_side);
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert_eq!(out.status.code(), Some(0), "{said}");
    let calls = ssh.calls();
    assert_eq!(calls.len(), 1, "{said}");
    assert_eq!(
        remote_line(&calls[0]),
        [
            "no-such-host-for-tests",
            "~/.local/bin/ccnm",
            "run",
            workspace,
            "--detached",
            "--prompt-stdin"
        ],
        "the line home receives says where to read the prompt, not what it is"
    );
    assert_eq!(ssh.fed_in(), prompt, "byte for byte, down the connection");
    assert!(
        !calls[0].iter().any(|a| a.contains("failing")),
        "not one word of it in the argv: {:?}",
        calls[0]
    );

    // ---- and it can be piped in here too, which is how newlines get in
    ssh.forget();
    let out = pipe_in(
        &[workspace, "--prompt-stdin", "--detached"],
        &work_side,
        "first line\nsecond line\n",
    );
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert_eq!(
        ssh.fed_in(),
        "first line\nsecond line",
        "trailing newline off, the one in the middle kept"
    );

    // ---- --prompt-stdin with nothing on stdin is refused, not empty ---
    //
    // An empty prompt is indistinguishable from the bug this replaced:
    // Claude opens with nothing and nobody is told why.
    ssh.forget();
    let out = pipe_in(&[workspace, "--prompt-stdin", "--detached"], &work_side, "");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(34), "invalid arguments: {err}");
    assert!(
        ssh.calls().is_empty(),
        "refused before the network: {:?}",
        ssh.calls()
    );
    assert!(err.contains("nothing arrived on stdin"), "{err}");
}

#[test]
fn a_bare_workspace_name_means_run() {
    let out = ccnm()
        .args(["not-a-workspace", "--config"])
        .arg(fixture("config-valid.toml"))
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        !err.contains("unrecognized subcommand"),
        "it must reach `run`, not clap: {err}"
    );
    assert!(err.contains("not defined"), "{err}");
    // A real subcommand still wins over a workspace of the same name.
    let out = ccnm().arg("status").output().unwrap();
    assert!(
        stderr(&out).contains("required") || stderr(&out).contains("Usage"),
        "{}",
        stderr(&out)
    );
}

/// Interactive and print mode share the local preflight, and it is the
/// first thing either does: the project has to be on this machine, because
/// this machine is the one that will serve it.
#[test]
fn run_checks_the_project_is_here_before_touching_the_network() {
    for args in [
        vec!["run", "xshun", "--config"],
        vec!["run", "xshun", "--print", "hi", "--config"],
        vec!["attach", "xshun", "--config"],
        vec!["stop", "xshun", "--config"],
    ] {
        let out = ccnm()
            .args(&args)
            .arg(fixture("config-valid.toml"))
            .output()
            .unwrap();
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(30), "{args:?}: {err}");
        assert!(err.contains("is not a directory on this machine"), "{err}");
    }
}

/// `--print` is one prompt with no terminal and a positional prompt is the
/// terminal's opening line; asking for both is a mistake worth catching
/// before anything is started.
#[test]
fn run_refuses_a_prompt_in_two_places_at_once() {
    let out = ccnm()
        .args(["run", "xshun", "hello", "--print", "hello", "--config"])
        .arg(fixture("config-valid.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot be used with"),
        "{}",
        stderr(&out)
    );
}

/// The real `ccnm internal supervise`, with a script standing in for
/// `claude`: the session's inputs go in, and the outputs come out in the
/// session directory, `exit` last.
#[test]
fn supervise_runs_the_session_and_writes_its_exit_record() {
    use ccnm_core::session::{self, Dir, Mode, Spec, SuperviseRequest};

    let (dir, _config) = setup("supervise", env!("CARGO_BIN_EXE_ccnm"));
    let fake = dir.join("claude");
    std::fs::write(
        &fake,
        "#!/bin/sh\ncat > \"$PWD/prompt-seen\"\nprintf '{\"is_error\":false,\"result\":\"ok\",\"num_turns\":1,\"session_id\":\"'\"$6\"'\"}'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let spec = Spec {
        protocol: ccnm_core::protocol::PROTOCOL,
        id: session::new_id(),
        workspace: "xshun".into(),
        root: dir.join("root"),
        home_alias: "ccnm-home".into(),
        home_ccnm_bin: "~/.local/bin/ccnm".into(),
        claude_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Print {
            prompt: "say ok".into(),
        },
        timeout_secs: 60,
        cwd: dir.clone(),
    };
    let ssh = ccnm_core::ssh::Ssh::new("ccnm-home", "/tmp/ccnm-t/cli-sup").unwrap();
    let session_dir = session::create(&dir, &spec, &ssh).unwrap();

    let wire = payload::encode(&SuperviseRequest::new(
        session_dir.path().to_path_buf(),
        fake,
    ))
    .unwrap();
    let out = ccnm()
        .args(["internal", "supervise", "--payload", &wire])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let d = Dir::at(session_dir.path());
    let outcome = session::read_outcome(&d).unwrap().expect("exit record");
    assert!(outcome.ok(), "{outcome:?}");
    let result = ccnm_core::claude::parse_print(&std::fs::read(d.stdout()).unwrap()).unwrap();
    assert_eq!(result.result.as_deref(), Some("ok"));
    // The prompt arrived on stdin; the session id was on the argv.
    assert_eq!(
        std::fs::read_to_string(dir.join("prompt-seen")).unwrap(),
        "say ok"
    );
    // The prompt is written and stdin is then *closed*. Left open, the
    // stand-in's `cat` would block until the supervisor's own kill, so
    // the gap between an honest run and a hang is the whole 60 s below.
    // The bound is 10 s and not 5: at 5 it failed on a machine that was
    // running a mutation sweep at the same time, which says nothing
    // about stdin and is exactly the kind of red that gets ignored.
    assert!(
        outcome.duration_ms < 10_000,
        "claude must not have waited for a stdin that was never closed: {} ms",
        outcome.duration_ms
    );
}

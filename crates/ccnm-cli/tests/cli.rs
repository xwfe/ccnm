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
    assert!(
        text.contains("NOT READY (1 failed, 13 not checked)"),
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
    assert!(rep.instructions_bytes > 0);
    assert!(
        rep.tools_list_bytes < 16 * 1024,
        "schema budget: {} bytes",
        rep.tools_list_bytes
    );
    assert!(rep.call_p50_us <= rep.call_p95_us && rep.call_p95_us <= rep.call_max_us);
    // Read-only: the server wrote nothing into the root.
    assert_eq!(std::fs::read_dir(dir.join("root")).unwrap().count(), 0);
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

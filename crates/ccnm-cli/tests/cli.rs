//! Runs the real `ccnm` binary. Exit codes and stdout are the contract that
//! `ccnm run`, the other machine's ccnm and the user's shell see, so they
//! are asserted here rather than through the library API.
//!
//! Nothing here needs a second machine. The one ssh alias used targets a
//! name under the reserved `.invalid` TLD; `ssh -G` resolves it without
//! connecting.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
            "version = 1\n[hosts.work]\nssh = \"ccnm-test-nowhere.invalid\"\n[hosts.home]\nssh_from_work = \"ccnm-home\"\nccnm_bin = \"{home_bin}\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\n",
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
fn doctor_local_rows_then_not_ready_on_unimplemented_phases() {
    // The real test binary stands in for ~/.local/bin/ccnm: same version.
    let (dir, config) = setup("local", env!("CARGO_BIN_EXE_ccnm"));
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(
        text.contains(&format!(
            "Home ccnm               OK     {} at {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_BIN_EXE_ccnm")
        )),
        "{text}"
    );
    assert!(
        text.contains("Work SSH                OK     "),
        "ssh -G resolves any name without connecting: {text}"
    );
    assert!(
        text.contains("Remote MCP handshake    SKIP   not implemented until phase 1B"),
        "{text}"
    );
    assert!(
        text.contains("NOT READY (0 failed, 12 not checked)"),
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

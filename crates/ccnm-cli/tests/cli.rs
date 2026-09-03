//! Runs the real `ccnm` binary. Exit codes and stdout are the contract that
//! `ccnm run` and the user's shell see, so they are asserted here rather
//! than through the library API.

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

/// A config whose workspace root is `root`, written under a per-test temp dir.
fn temp_config(test: &str, root: &Path) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-{}-{test}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "version = 1\n[hosts.work]\nssh = \"work\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nruntime_root = \"/Users/Shared/cc-runtime/xshun\"\nshare = \"xshun\"\n",
            root.display()
        ),
    )
    .unwrap();
    path
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
        text.ends_with("NOT READY (1 failed, 0 not implemented)\n"),
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
        text.contains("Workspaces              INFO   xshun"),
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
    assert!(stdout(&out).contains("mount_mod"), "{}", stdout(&out));
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
fn doctor_workspace_with_existing_root_blocks_on_unimplemented_checks() {
    let config = temp_config("root-ok", &std::env::temp_dir());
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    // Exit code is the first SKIP's error code (Workspace identity ->
    // CCNM_E_WRONG_WORKSPACE = 30). It must not be 0 and not a config error.
    assert_eq!(out.status.code(), Some(30));
    let text = stdout(&out);
    assert!(text.contains("Home workspace          OK"), "{text}");
    assert!(
        text.contains("SKIP   not implemented until phase 1"),
        "{text}"
    );
    assert!(
        text.contains("NOT READY (0 failed, 12 not implemented)"),
        "{text}"
    );
}

#[test]
fn doctor_workspace_with_missing_root_exits_config_code() {
    let missing = std::env::temp_dir().join("ccnm-cli-definitely-missing-root");
    let config = temp_config("root-missing", &missing);
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    assert!(
        stdout(&out).contains("does not exist on this machine"),
        "{}",
        stdout(&out)
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

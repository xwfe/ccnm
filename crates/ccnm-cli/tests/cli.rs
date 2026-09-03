//! Runs the real `ccnm` binary. Exit codes and stdout are the contract that
//! `ccnm run`, the other machine's ccnm and the user's shell see, so they
//! are asserted here rather than through the library API.
//!
//! Nothing here needs a second machine. The one ssh attempt targets a name
//! under the reserved `.invalid` TLD, which fails to resolve immediately.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ccnm_core::payload;
use ccnm_core::runner::{HealthReport, HealthRequest};

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
/// `work_ssh` is the alias the home side would ssh to.
fn setup(test: &str, work_ssh: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ccnm-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "version = 1\n[hosts.work]\nssh = \"{work_ssh}\"\n[hosts.home_runner]\nssh_from_work = \"ccnm-home\"\nsmb_user = \"fodelf\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nruntime_root = \"{}\"\nshare = \"xshun\"\n",
            root.display(),
            dir.join("runtime").display()
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
fn internal_commands_are_hidden_from_help() {
    let out = ccnm().arg("--help").output().unwrap();
    let text = stdout(&out);
    assert!(text.contains("doctor"), "{text}");
    assert!(text.contains("mount"), "{text}");
    assert!(!text.contains("  work "), "{text}");
    assert!(!text.contains("  runner "), "{text}");
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
fn workspace_init_then_doctor_against_unreachable_work() {
    let (dir, config) = setup("e2e", "ccnm-test-nowhere.invalid");
    let root = dir.join("root");

    // Identity missing: doctor says how to fix it and exits WRONG_WORKSPACE.
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(30), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("run: ccnm workspace init xshun"),
        "{}",
        stdout(&out)
    );

    // Create it.
    let out = ccnm()
        .args(["workspace", "init", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let printed = stdout(&out);
    assert!(
        printed.starts_with(&format!("{}: ", root.join(".ccnm-workspace-id").display())),
        "{printed}"
    );
    let id = printed.split(": ").nth(1).unwrap().trim().to_string();
    assert_eq!(id.len(), 36);

    // Twice is refused with POLICY.
    let out = ccnm()
        .args(["workspace", "init", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(33), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with("CCNM_E_POLICY:\n"),
        "{}",
        stderr(&out)
    );

    // Now doctor gets as far as ssh, which cannot resolve the alias.
    let out = ccnm()
        .args(["doctor", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(20), "{text}");
    assert!(
        text.contains(&format!("Workspace identity      OK     {id}")),
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
        text.contains("NOT READY (1 failed, 10 not checked)"),
        "{text}"
    );
    // Read-only: nothing but the id file appeared in the root, no socket dir.
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn runner_health_answers_with_the_local_view() {
    let (dir, config) = setup("health", "work");
    let root = dir.join("root");
    let wire = payload::encode(&HealthRequest::new(&root, dir.join("runtime"))).unwrap();

    let out = ccnm()
        .args(["runner", "health", "--payload", &wire])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let rep: HealthReport = payload::decode_json(&out.stdout).unwrap();
    assert_eq!(rep.ccnm_version, env!("CARGO_PKG_VERSION"));
    assert!(rep.root.is_ok());
    assert!(!rep.runtime_root.exists);
    assert_eq!(rep.identity, Ok(None));

    ccnm()
        .args(["workspace", "init", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    let out = ccnm()
        .args(["runner", "health", "--payload", &wire])
        .output()
        .unwrap();
    let rep: HealthReport = payload::decode_json(&out.stdout).unwrap();
    assert!(matches!(rep.identity, Ok(Some(ref id)) if id.len() == 36));
}

#[test]
fn garbage_payload_is_a_version_error() {
    let out = ccnm()
        .args(["runner", "health", "--payload", "definitely-not-a-payload"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
    assert!(
        stderr(&out).starts_with("CCNM_E_VERSION:\n"),
        "{}",
        stderr(&out)
    );

    let out = ccnm()
        .args(["work", "probe", "--payload", "eyJwcm90b2NvbCI6OTl9"]) // {"protocol":99}
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
}

#[test]
fn mount_against_unreachable_work_exits_work_unreachable() {
    let (_dir, config) = setup("mount", "ccnm-test-nowhere.invalid");
    let out = ccnm()
        .args(["mount", "xshun", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(20), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with("CCNM_E_WORK_UNREACHABLE:\n"),
        "{}",
        stderr(&out)
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
    let err = stderr(&out);
    assert!(err.contains("loading config"), "stderr: {err}");
    assert!(!stdout(&out).contains("loading config"));
}

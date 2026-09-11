//! P1 synthetic fixtures only: no real credentials, accounts or network.
use super::{credentials::*, environment::*, *};
use crate::process::FakeRunner;
use crate::{Cmd, Output};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(dir)
    }
    fn file(&self, path: &str) -> PathBuf {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "SYNTHETIC_NEVER_READ_OR_LOG").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn all_provider_and_unknown_auth_names_are_stripped_not_vendor_project_names() {
    let names = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "UNRECOGNIZED_TOKEN",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_CLOUD_PROJECT",
        "CC",
        "PROJECT_MODE",
    ];
    let removed = strip_names(names.map(OsString::from));
    for name in &names[..6] {
        assert!(removed.iter().any(|k| k == name));
    }
    for name in &names[6..] {
        assert!(!removed.iter().any(|k| k == name));
    }
    assert!(validate_runtime_names([OsString::from("UNRECOGNIZED_TOKEN")]).is_err());
    assert!(validate_runtime_names([OsString::from("SSH_AUTH_SOCK")]).is_err());
    assert!(validate_runtime_names(names[6..].iter().map(OsString::from)).is_ok());
}

#[test]
fn explicit_env_cannot_restore_auth_after_removal() {
    let cmd = without_agent_auth(
        Cmd::new("fixture")
            .env("CODEX_HOME", "SYNTHETIC_NEVER_LOG")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "SYNTHETIC_NEVER_LOG")
            .env("UNKNOWN_SECRET", "SYNTHETIC_NEVER_LOG")
            .env("PROJECT_MODE", "fixture"),
    );
    assert_eq!(cmd.env, vec![("PROJECT_MODE".into(), "fixture".into())]);
    assert!(!format!("{cmd:?}").contains("SYNTHETIC_NEVER_LOG"));
    assert!(
        !cmd.env_remove.iter().any(|k| k == "SSH_AUTH_SOCK"),
        "Agent-local SSH can authenticate, but never forward"
    );
    assert!(
        runtime_child(cmd)
            .env_remove
            .iter()
            .any(|k| k == "SSH_AUTH_SOCK")
    );
}

#[test]
fn file_access_uses_os_verdict_not_mode_or_secret_content() {
    let f = Fixture::new();
    let file = f.file("auth.json");
    for (code, expected) in [
        (0, Access::Accessible),
        (1, Access::Inaccessible),
        (2, Access::Unknown),
    ] {
        let runner = FakeRunner::new();
        runner.push(Output::exited(code, ""));
        assert_eq!(access(&file, &runner), expected);
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "/bin/test");
        assert_eq!(calls[0].args[0], "-r");
    }
    assert_eq!(access(&file, &FakeRunner::new()), Access::Unknown);
    let runner = FakeRunner::new();
    let mut out = Output::exited(1, "");
    out.timed_out = true;
    runner.push(out);
    assert_eq!(access(&file, &runner), Access::Unknown);
}

#[test]
fn missing_symlink_non_file_and_relative_path_are_distinct() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    assert_eq!(access(&f.0.join("absent"), &runner), Access::Missing);
    symlink("absent", f.0.join("dangling")).unwrap();
    assert_eq!(access(&f.0.join("dangling"), &runner), Access::Unknown);
    assert_eq!(access(&f.0, &runner), Access::Unknown);
    assert_eq!(
        access(Path::new("relative/auth.json"), &runner),
        Access::Unknown
    );
    let file = f.file("regular");
    assert_eq!(access(&file.join("child"), &runner), Access::Unknown);
    assert!(runner.calls().is_empty());
}

#[test]
fn both_providers_default_managed_and_local_override_paths_are_audited() {
    let f = Fixture::new();
    for file in [
        ".claude/.credentials.json",
        ".codex/auth.json",
        ".config/ccnm/agents/codex/auth.json",
        "config/ccnm/agents/codex/auth.json",
        "custom/auth.json",
        "Library/Keychains/login.keychain-db",
    ] {
        f.file(file);
    }
    let refs = vec![
        (
            "CODEX_HOME".into(),
            Some(f.0.join("custom").into_os_string()),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            Some(f.0.join("config").into_os_string()),
        ),
    ];
    let runner = FakeRunner::new();
    for _ in 0..6 {
        runner.push(Output::exited(0, ""));
    }
    let report = findings_with(&f.0, &refs, &runner);
    assert_eq!(report.len(), 2);
    assert!(
        report
            .iter()
            .all(|f| f.severity == Severity::Fail && f.is_agent_credential())
    );
    // Which switch waives them, in both directions. `allow_unconfined_exec`
    // never has: it says the account is not confined, not that it may read
    // the agent's login, and conflating the two was the whole reason the
    // second switch exists.
    let audit = Audit {
        user: "fixture".into(),
        findings: report.clone(),
    };
    assert!(!audit.agent_boundary_clear(Accepted::unconfined(true)));
    assert!(audit.agent_boundary_clear(Accepted {
        unconfined_exec: true,
        agent_credentials: true,
    }));
    assert_eq!(runner.calls().len(), 6);
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("SYNTHETIC_NEVER_READ_OR_LOG"));
    assert!(!text.contains(f.0.to_str().unwrap()));
}

#[test]
fn invalid_local_references_and_unknown_access_are_not_waived_by_unconfined_exec() {
    let f = Fixture::new();
    for refs in [
        vec![("CODEX_HOME".into(), Some("relative".into()))],
        vec![("XDG_CONFIG_HOME".into(), Some("relative".into()))],
    ] {
        let findings = findings_with(&f.0, &refs, &FakeRunner::new());
        let report = Audit {
            user: "fixture".into(),
            findings,
        };
        assert!(!report.exec_allowed(Accepted::unconfined(true)));
    }
    assert!(
        findings_with(
            &f.0,
            &[("XDG_CONFIG_HOME".into(), Some("".into()))],
            &FakeRunner::new()
        )
        .iter()
        .all(|f| f.severity == Severity::Ok),
        "empty XDG configuration retains the existing default-home semantics"
    );
    f.file(".codex/auth.json");
    let report = Audit {
        user: "fixture".into(),
        findings: findings_with(&f.0, &[], &FakeRunner::new()),
    };
    assert!(!report.exec_allowed(Accepted::unconfined(true)));
    assert!(
        report
            .refusal(Accepted::unconfined(true))
            .contains("unknown")
    );
}

#[test]
fn private_home_binds_effective_uid_not_home_directory_owner() {
    let f = Fixture::new();
    f.file("auth.json");
    let uid = std::fs::metadata(&f.0).unwrap().uid();
    assert!(private_home_for(&f.0, &["auth.json"], uid).is_ok());
    assert!(private_home_for(&f.0, &["auth.json"], uid.wrapping_add(1)).is_err());
    std::fs::set_permissions(
        f.0.join("auth.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(private_home_for(&f.0, &["auth.json"], uid).is_err());
    std::fs::remove_file(f.0.join("auth.json")).unwrap();
    symlink("absent", f.0.join("auth.json")).unwrap();
    assert!(private_home_for(&f.0, &["auth.json"], uid).is_err());
    assert!(effective_uid(&FakeRunner::new()).is_err());
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "invalid"));
    assert!(effective_uid(&runner).is_err());
}

#[test]
fn ordinary_unconfined_acceptance_does_not_authorize_agent_credentials() {
    let report = Audit {
        user: "fixture".into(),
        findings: vec![Finding::fail("Runtime user", "not configured", "configure")],
    };
    assert!(report.exec_allowed(Accepted::unconfined(true)));
    let mut report = report;
    report.findings.push(Finding::fail(
        "No authentication environment",
        "unknown",
        "remove inherited auth",
    ));
    assert!(!report.exec_allowed(Accepted::unconfined(true)));
}

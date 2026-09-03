//! Asking the official Claude Code CLI about itself. ccnm only ever runs
//! `claude --version` and `claude auth status`; it never logs in (design
//! doc section 10).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output};

/// Find the `claude` binary. Non-interactive ssh sessions often have a bare
/// PATH, so the usual install locations are tried after it.
pub fn locate(path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = path_var {
        candidates.extend(std::env::split_paths(path).map(|dir| dir.join("claude")));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/claude"));
        candidates.push(home.join(".claude/local/claude"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/claude"));
    candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    candidates.into_iter().find(|p| is_executable(p))
}

pub fn locate_from_env() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    locate(std::env::var_os("PATH").as_deref(), home.as_deref())
}

pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn version_cmd(bin: &Path, config_dir: Option<&Path>) -> Cmd {
    with_config_dir(Cmd::new(bin).arg("--version"), config_dir).timeout(Duration::from_secs(20))
}

pub fn auth_status_cmd(bin: &Path, config_dir: Option<&Path>) -> Cmd {
    with_config_dir(Cmd::new(bin).args(["auth", "status", "--json"]), config_dir)
        .timeout(Duration::from_secs(20))
}

fn with_config_dir(cmd: Cmd, config_dir: Option<&Path>) -> Cmd {
    match config_dir {
        Some(dir) => cmd.env("CLAUDE_CONFIG_DIR", dir),
        None => cmd,
    }
}

/// `2.1.259 (Claude Code)` -> `2.1.259`.
pub fn parse_version(out: &Output) -> Result<String> {
    if !out.success() {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "claude --version failed (exit {:?}): {}",
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        ));
    }
    let stdout = out.stdout_lossy();
    stdout
        .split_whitespace()
        .next()
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Version,
                format!(
                    "claude --version printed something unexpected: {:?}",
                    stdout.trim()
                ),
            )
        })
}

/// The fields of `claude auth status --json` ccnm cares about. Unknown
/// fields are ignored so a newer Claude does not break doctor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub logged_in: bool,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
}

impl AuthStatus {
    /// `email via claude.ai (max)` style summary.
    pub fn describe(&self) -> String {
        let who = self.email.as_deref().unwrap_or("logged in");
        let mut text = who.to_string();
        if let Some(method) = &self.auth_method {
            text.push_str(&format!(" via {method}"));
        }
        if let Some(sub) = &self.subscription_type {
            text.push_str(&format!(" ({sub})"));
        }
        text
    }
}

/// `claude auth status` exits 0 when logged in and 1 when not, printing
/// JSON either way. Output that is not that JSON means a Claude Code too
/// old to have the command.
pub fn parse_auth(out: &Output) -> Result<AuthStatus> {
    if out.timed_out {
        return Err(Error::new(
            ErrorCode::Version,
            "claude auth status timed out",
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            format!(
                "claude auth status did not print its JSON (exit {:?}); Claude Code too old?\nstdout: {}\nstderr: {}",
                out.exit_code,
                out.stdout_lossy().trim(),
                out.stderr_lossy().trim()
            ),
        )
        .with_source(e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_carry_config_dir_only_when_set() {
        let bin = Path::new("/usr/local/bin/claude");
        let cmd = version_cmd(bin, None);
        assert_eq!(cmd.display(), "/usr/local/bin/claude --version");
        assert!(cmd.env.is_empty());

        let cmd = auth_status_cmd(bin, Some(Path::new("/x/claude")));
        assert_eq!(cmd.display(), "/usr/local/bin/claude auth status --json");
        assert_eq!(cmd.env.len(), 1);
        assert_eq!(cmd.env[0].0, "CLAUDE_CONFIG_DIR");
        assert_eq!(cmd.env[0].1, "/x/claude");
    }

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version(&Output::exited(0, "2.1.259 (Claude Code)\n")).unwrap(),
            "2.1.259"
        );
        let err = parse_version(&Output::exited(0, "Claude Code 2.1\n")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        let err = parse_version(&Output::exited(1, "")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
    }

    /// Captured from Claude Code 2.1.259 on 2026-09-03 (ids shortened).
    const LOGGED_IN: &str = r#"{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "apiProvider": "firstParty",
  "analyticsDisabled": false,
  "projectsDirectory": "/Users/me/.claude/projects",
  "email": "me@example.com",
  "orgId": "1e6d91f7",
  "orgName": "me's Organization",
  "subscriptionType": "max"
}"#;

    const LOGGED_OUT: &str = r#"{
  "loggedIn": false,
  "authMethod": "none",
  "apiProvider": "firstParty",
  "analyticsDisabled": false,
  "projectsDirectory": "/tmp/fresh/projects"
}"#;

    #[test]
    fn auth_parsing() {
        let status = parse_auth(&Output::exited(0, LOGGED_IN)).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.describe(), "me@example.com via claude.ai (max)");

        let status = parse_auth(&Output::exited(1, LOGGED_OUT)).unwrap();
        assert!(!status.logged_in);
        assert_eq!(status.email, None);

        let err = parse_auth(&Output::exited(1, "error: unknown command 'auth'")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("too old"), "{err}");
    }

    #[test]
    fn locate_prefers_path_then_known_dirs() {
        let dir = std::env::temp_dir().join(format!("ccnm-claude-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fake = bin_dir.join("claude");
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var = std::env::join_paths([dir.join("nowhere"), bin_dir.clone()]).unwrap();
        assert_eq!(locate(Some(&path_var), None), Some(fake.clone()));

        // Not on PATH, but in ~/.local/bin.
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::rename(&fake, home.join(".local/bin/claude")).unwrap();
        assert_eq!(
            locate(Some(&path_var), Some(&home)),
            Some(home.join(".local/bin/claude"))
        );

        // A non-executable file does not count.
        std::fs::write(&fake, "").unwrap();
        assert_eq!(locate(Some(&path_var), None), None);
    }
}

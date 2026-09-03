//! `~/.config/ccnm/config.toml`, the home machine's source of truth.
//!
//! Secrets never live here (design doc section 5); SSH keys, SMB passwords
//! and Claude OAuth stay with OpenSSH, the system SMB credential store and
//! Claude Code itself.
//!
//! Unknown keys are an error, not ignored. A typo like `mount_mod` that
//! silently falls back to a default is exactly the drift doctor exists to
//! catch, so the parser refuses it up front.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// The only `version = N` this binary understands.
pub const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// OpenSSH host alias from `~/.ssh/config` used to reach this machine.
    pub ssh: String,
    /// `CLAUDE_CONFIG_DIR` for Claude Code on this host. Unset means Claude's
    /// own default (`~/.claude`) and whatever login is already there. A custom
    /// dir has its own credentials and needs its own `claude auth login`;
    /// ccnm never performs that login (design doc section 10).
    #[serde(default)]
    pub claude_config_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    /// Key into `hosts`.
    pub work_host: String,
    /// Project root. Must be the same absolute path on both machines
    /// (design doc section 6); V1 does no path translation.
    pub root: PathBuf,
    /// Where the restricted runner may write: build output, caches, logs.
    /// Must not overlap `root`, otherwise the runner gains write access to
    /// source and single-writer enforcement is gone.
    pub runtime_root: PathBuf,
    /// SMB share name the work machine mounts.
    pub share: String,
    #[serde(default)]
    pub mount_mode: MountMode,
    #[serde(default)]
    pub claude_permission_mode: PermissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    /// Every remote command is gated by the hash barrier (design doc
    /// section 24). The only mode V1 ships.
    #[default]
    Coherence,
}

/// Values accepted by `claude --permission-mode`, checked against Claude
/// Code 2.1.259 `--help`. Serialized in Claude's own camelCase so the config
/// file and the CLI flag read the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    AcceptEdits,
    Auto,
    BypassPermissions,
    Manual,
    DontAsk,
    Plan,
}

impl PermissionMode {
    /// The exact string to pass after `--permission-mode`.
    pub fn as_cli_value(self) -> &'static str {
        match self {
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Auto => "auto",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Manual => "manual",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }
}

/// A workspace together with the host it runs Claude on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub name: &'a str,
    pub workspace: &'a Workspace,
    pub host: &'a Host,
}

impl Config {
    /// Read, parse and validate the file at `path`.
    pub fn load(path: &Path) -> Result<Config> {
        tracing::debug!(path = %path.display(), "loading config");
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::config(format!(
                    "config not found: {}\ncreate it by hand for now (design doc section 5)",
                    path.display()
                ))
            } else {
                Error::config(format!("cannot read config {}", path.display())).with_source(e)
            }
        })?;
        Config::parse(&text)
            .map_err(|e| Error::config(format!("{}: {}", path.display(), e.message())))
    }

    /// Parse and validate TOML text. Every validation problem is reported
    /// in one error so the user fixes the file once, not once per run.
    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text).map_err(|e| Error::config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Look up a workspace by name and the host it points at.
    pub fn workspace<'a>(&'a self, name: &'a str) -> Result<Resolved<'a>> {
        let workspace = self.workspaces.get(name).ok_or_else(|| {
            if self.workspaces.is_empty() {
                Error::config(format!(
                    "workspace '{name}' is not defined (no workspaces in config)"
                ))
            } else {
                let defined: Vec<&str> = self.workspaces.keys().map(String::as_str).collect();
                Error::config(format!(
                    "workspace '{name}' is not defined; defined: {}",
                    defined.join(", ")
                ))
            }
        })?;
        // validate() already guarantees the host exists.
        let host = self.hosts.get(&workspace.work_host).ok_or_else(|| {
            Error::internal(format!(
                "workspace '{name}' passed validation with unknown host '{}'",
                workspace.work_host
            ))
        })?;
        Ok(Resolved {
            name,
            workspace,
            host,
        })
    }

    fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();

        if self.version != SUPPORTED_VERSION {
            problems.push(format!(
                "version = {} is not supported; this ccnm understands version = {SUPPORTED_VERSION}",
                self.version
            ));
        }

        for (name, host) in &self.hosts {
            let at = format!("hosts.{name}");
            check_name(&at, name, &mut problems);
            if host.ssh.trim().is_empty() {
                problems.push(format!("{at}.ssh must be an SSH host alias"));
            }
            if let Some(dir) = &host.claude_config_dir {
                check_absolute(&format!("{at}.claude_config_dir"), dir, &mut problems);
            }
        }

        for (name, ws) in &self.workspaces {
            let at = format!("workspaces.{name}");
            check_name(&at, name, &mut problems);
            if !self.hosts.contains_key(&ws.work_host) {
                problems.push(format!(
                    "{at}.work_host = \"{}\" does not match any [hosts.*] entry",
                    ws.work_host
                ));
            }
            if ws.share.trim().is_empty() {
                problems.push(format!("{at}.share must be the SMB share name"));
            }
            let root_ok = check_absolute(&format!("{at}.root"), &ws.root, &mut problems);
            let runtime_ok = check_absolute(
                &format!("{at}.runtime_root"),
                &ws.runtime_root,
                &mut problems,
            );
            if root_ok && runtime_ok && overlaps(&ws.root, &ws.runtime_root) {
                problems.push(format!(
                    "{at}.runtime_root must not overlap root: the runner would get write access to source"
                ));
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::config(problems.join("\n")))
        }
    }
}

/// Names end up in tmux session names, share names and state paths, so keep
/// them to characters that are safe everywhere.
fn check_name(at: &str, name: &str, problems: &mut Vec<String>) {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !ok {
        problems.push(format!(
            "{at}: name must be [A-Za-z0-9][A-Za-z0-9_-]*, got \"{name}\""
        ));
    }
}

/// Absolute and free of `.` / `..` so that lexical comparisons between
/// paths mean what they look like.
fn check_absolute(at: &str, path: &Path, problems: &mut Vec<String>) -> bool {
    if !path.is_absolute() {
        problems.push(format!(
            "{at} must be an absolute path, got \"{}\"",
            path.display()
        ));
        return false;
    }
    let dotty = path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir));
    if dotty {
        problems.push(format!(
            "{at} must not contain \".\" or \"..\", got \"{}\"",
            path.display()
        ));
        return false;
    }
    true
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn parse_err(text: &str) -> Error {
        Config::parse(text).expect_err("config should be rejected")
    }

    #[test]
    fn valid_fixture_parses() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        assert_eq!(config.version, 1);
        let resolved = config.workspace("xshun").unwrap();
        assert_eq!(resolved.host.ssh, "work");
        assert_eq!(resolved.host.claude_config_dir, None);
        assert_eq!(
            resolved.workspace.root,
            PathBuf::from("/Users/Shared/cc-workspaces/xshun")
        );
        assert_eq!(resolved.workspace.mount_mode, MountMode::Coherence);
        assert_eq!(
            resolved.workspace.claude_permission_mode,
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn optional_fields_default_and_claude_config_dir_is_read() {
        let config = Config::load(&fixture("config-custom-claude-dir.toml")).unwrap();
        let resolved = config.workspace("xshun").unwrap();
        assert_eq!(
            resolved.host.claude_config_dir,
            Some(PathBuf::from("/Users/me/.ccnm/claude"))
        );
        assert_eq!(resolved.workspace.mount_mode, MountMode::Coherence);
        assert_eq!(
            resolved.workspace.claude_permission_mode,
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn unknown_field_is_rejected_with_its_name() {
        let err = Config::load(&fixture("config-unknown-field.toml")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("mount_mod"), "{err}");
    }

    #[test]
    fn missing_file_names_the_path() {
        let err = Config::load(Path::new("/nonexistent/ccnm/config.toml")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(
            err.message().contains("/nonexistent/ccnm/config.toml"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let err = parse_err("version = 2\n");
        assert!(err.message().contains("version = 2"), "{err}");
    }

    #[test]
    fn unknown_work_host_is_rejected() {
        let err = parse_err(
            r#"
version = 1
[workspaces.x]
work_host = "nope"
root = "/a"
runtime_root = "/b"
share = "x"
"#,
        );
        assert!(err.message().contains("work_host = \"nope\""), "{err}");
    }

    #[test]
    fn relative_and_dotty_paths_are_rejected() {
        let err = parse_err(
            r#"
version = 1
[hosts.work]
ssh = "work"
claude_config_dir = "relative/dir"
[workspaces.x]
work_host = "work"
root = "src"
runtime_root = "/tmp/../x"
share = "x"
"#,
        );
        let msg = err.message();
        assert!(
            msg.contains("claude_config_dir must be an absolute path"),
            "{msg}"
        );
        assert!(msg.contains("root must be an absolute path"), "{msg}");
        assert!(msg.contains("runtime_root must not contain"), "{msg}");
    }

    #[test]
    fn runtime_root_inside_root_is_rejected() {
        let err = parse_err(
            r#"
version = 1
[hosts.work]
ssh = "work"
[workspaces.x]
work_host = "work"
root = "/Users/Shared/cc-workspaces/x"
runtime_root = "/Users/Shared/cc-workspaces/x/target"
share = "x"
"#,
        );
        assert!(err.message().contains("must not overlap root"), "{err}");
    }

    #[test]
    fn bad_names_are_rejected() {
        let err = parse_err(
            r#"
version = 1
[hosts."my host"]
ssh = "work"
[workspaces."-x"]
work_host = "my host"
root = "/a"
runtime_root = "/b"
share = "x"
"#,
        );
        let msg = err.message();
        assert!(msg.contains("hosts.my host: name must be"), "{msg}");
        assert!(msg.contains("workspaces.-x: name must be"), "{msg}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let err = parse_err(
            r#"
version = 3
[workspaces.x]
work_host = "nope"
root = "rel"
runtime_root = "/b"
share = ""
"#,
        );
        let lines = err.message().lines().count();
        assert!(lines >= 4, "expected several problems, got:\n{err}");
    }

    #[test]
    fn unknown_workspace_lists_defined_ones() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        let err = config.workspace("other").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("defined: xshun"), "{err}");
    }

    #[test]
    fn permission_mode_cli_values_match_claude() {
        let modes = [
            (PermissionMode::AcceptEdits, "acceptEdits"),
            (PermissionMode::Auto, "auto"),
            (PermissionMode::BypassPermissions, "bypassPermissions"),
            (PermissionMode::Manual, "manual"),
            (PermissionMode::DontAsk, "dontAsk"),
            (PermissionMode::Plan, "plan"),
        ];
        for (mode, text) in modes {
            assert_eq!(mode.as_cli_value(), text);
            let toml = format!(
                "version = 1\n[hosts.w]\nssh = \"w\"\n[workspaces.x]\nwork_host = \"w\"\nroot = \"/a\"\nruntime_root = \"/b\"\nshare = \"x\"\nclaude_permission_mode = \"{text}\"\n"
            );
            let config = Config::parse(&toml).unwrap();
            assert_eq!(config.workspaces["x"].claude_permission_mode, mode);
        }
    }
}

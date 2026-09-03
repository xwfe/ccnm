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

/// `workspaces.<name>.runner_host` when the file does not say.
pub const DEFAULT_RUNNER_HOST: &str = "home_runner";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
}

/// One machine. Which fields are required depends on the role a workspace
/// gives it: a `work_host` needs `ssh`, a `runner_host` needs
/// `ssh_from_work` and `smb_user`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// Alias in the home machine's `~/.ssh/config` that reaches this host.
    #[serde(default)]
    pub ssh: Option<String>,
    /// Alias in the *work* machine's `~/.ssh/config` that reaches this host.
    /// Its resolved HostName doubles as the SMB server address, so the two
    /// transports always point at the same machine.
    #[serde(default)]
    pub ssh_from_work: Option<String>,
    /// Account the work machine mounts the SMB share as. The password lives
    /// in the work machine's Keychain, never here.
    #[serde(default)]
    pub smb_user: Option<String>,
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
    /// Key into `hosts`: where Claude Code runs.
    pub work_host: String,
    /// Key into `hosts`: where source lives and commands execute. Defaults
    /// to `home_runner`, the name the design doc uses.
    #[serde(default = "default_runner_host")]
    pub runner_host: String,
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

fn default_runner_host() -> String {
    DEFAULT_RUNNER_HOST.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    /// Mount with `nodatacache,nomdatacache,nopassprompt,soft,nobrowse`
    /// (design doc section 39) and gate every remote command with the hash
    /// barrier (section 24). The only mode V1 ships.
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

/// A workspace together with both hosts it spans and the role-specific
/// fields validation has already proven present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub name: &'a str,
    pub workspace: &'a Workspace,
    pub work: &'a Host,
    pub runner: &'a Host,
    /// `hosts.<work_host>.ssh`: home -> work.
    pub work_ssh: &'a str,
    /// `hosts.<runner_host>.ssh_from_work`: work -> home runner.
    pub home_alias: &'a str,
    /// `hosts.<runner_host>.smb_user`.
    pub smb_user: &'a str,
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

    /// Look up a workspace by name together with its hosts.
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
        // validate() already guarantees all of these; a miss here is a bug.
        let bug = |what: &str| {
            Error::internal(format!(
                "workspace '{name}' passed validation but {what} is missing"
            ))
        };
        let work = self
            .hosts
            .get(&workspace.work_host)
            .ok_or_else(|| bug("its work host"))?;
        let runner = self
            .hosts
            .get(&workspace.runner_host)
            .ok_or_else(|| bug("its runner host"))?;
        Ok(Resolved {
            name,
            workspace,
            work,
            runner,
            work_ssh: work.ssh.as_deref().ok_or_else(|| bug("work ssh"))?,
            home_alias: runner
                .ssh_from_work
                .as_deref()
                .ok_or_else(|| bug("runner ssh_from_work"))?,
            smb_user: runner
                .smb_user
                .as_deref()
                .ok_or_else(|| bug("runner smb_user"))?,
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
            for (field, value) in [
                ("ssh", &host.ssh),
                ("ssh_from_work", &host.ssh_from_work),
                ("smb_user", &host.smb_user),
            ] {
                if let Some(value) = value {
                    check_token(&format!("{at}.{field}"), value, &mut problems);
                }
            }
            if let Some(dir) = &host.claude_config_dir {
                check_absolute(&format!("{at}.claude_config_dir"), dir, &mut problems);
            }
        }

        for (name, ws) in &self.workspaces {
            let at = format!("workspaces.{name}");
            check_name(&at, name, &mut problems);
            match self.hosts.get(&ws.work_host) {
                None => problems.push(format!(
                    "{at}.work_host = \"{}\" does not match any [hosts.*] entry",
                    ws.work_host
                )),
                Some(host) if host.ssh.is_none() => problems.push(format!(
                    "{at}.work_host = \"{}\" names a host without `ssh` (the alias the home machine uses to reach it)",
                    ws.work_host
                )),
                Some(_) => {}
            }
            match self.hosts.get(&ws.runner_host) {
                None => problems.push(format!(
                    "{at}.runner_host = \"{}\" does not match any [hosts.*] entry",
                    ws.runner_host
                )),
                Some(host) => {
                    if host.ssh_from_work.is_none() {
                        problems.push(format!(
                            "{at}.runner_host = \"{}\" names a host without `ssh_from_work` (the alias the work machine uses to reach it)",
                            ws.runner_host
                        ));
                    }
                    if host.smb_user.is_none() {
                        problems.push(format!(
                            "{at}.runner_host = \"{}\" names a host without `smb_user` (the account the work machine mounts the share as)",
                            ws.runner_host
                        ));
                    }
                }
            }
            if ws.share.trim().is_empty() {
                problems.push(format!("{at}.share must be the SMB share name"));
            } else {
                check_token(&format!("{at}.share"), &ws.share, &mut problems);
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

/// SSH aliases, SMB users and share names travel through ssh command lines
/// and `//user@host/share` URLs. Restricting them to this set means they
/// never need quoting anywhere.
pub(crate) fn is_token(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_token(at: &str, value: &str, problems: &mut Vec<String>) {
    if !is_token(value) {
        problems.push(format!(
            "{at} must match [A-Za-z0-9._-]+ and not start with '-', got \"{value}\""
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

    /// Minimal valid config with the given workspace body appended.
    fn with_workspace(body: &str) -> String {
        format!(
            "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home_runner]\nssh_from_work = \"ccnm-home\"\nsmb_user = \"me\"\n[workspaces.x]\n{body}\n"
        )
    }

    const VALID_WS: &str =
        "work_host = \"work\"\nroot = \"/a\"\nruntime_root = \"/b\"\nshare = \"x\"";

    #[test]
    fn valid_fixture_parses() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        assert_eq!(config.version, 1);
        let r = config.workspace("xshun").unwrap();
        assert_eq!(r.work_ssh, "work");
        assert_eq!(r.home_alias, "ccnm-home");
        assert_eq!(r.smb_user, "fodelf");
        assert_eq!(r.work.claude_config_dir, None);
        assert_eq!(r.workspace.runner_host, "home_runner");
        assert_eq!(
            r.workspace.root,
            PathBuf::from("/Users/Shared/cc-workspaces/xshun")
        );
        assert_eq!(r.workspace.mount_mode, MountMode::Coherence);
        assert_eq!(
            r.workspace.claude_permission_mode,
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn optional_fields_default_and_claude_config_dir_is_read() {
        let config = Config::load(&fixture("config-custom-claude-dir.toml")).unwrap();
        let r = config.workspace("xshun").unwrap();
        assert_eq!(
            r.work.claude_config_dir,
            Some(PathBuf::from("/Users/me/.ccnm/claude"))
        );
        assert_eq!(r.workspace.runner_host, "runner");
        assert_eq!(r.home_alias, "home");
        assert_eq!(r.workspace.mount_mode, MountMode::Coherence);
        assert_eq!(
            r.workspace.claude_permission_mode,
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
    fn unknown_hosts_are_rejected() {
        let err = parse_err(&with_workspace(
            "work_host = \"nope\"\nrunner_host = \"nada\"\nroot = \"/a\"\nruntime_root = \"/b\"\nshare = \"x\"",
        ));
        let msg = err.message();
        assert!(msg.contains("work_host = \"nope\""), "{msg}");
        assert!(msg.contains("runner_host = \"nada\""), "{msg}");
    }

    #[test]
    fn role_specific_fields_are_required() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh_from_work = \"x\"\n[hosts.home_runner]\nssh = \"y\"\n[workspaces.x]\nwork_host = \"work\"\nroot = \"/a\"\nruntime_root = \"/b\"\nshare = \"x\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("host without `ssh`"), "{msg}");
        assert!(msg.contains("host without `ssh_from_work`"), "{msg}");
        assert!(msg.contains("host without `smb_user`"), "{msg}");
    }

    #[test]
    fn relative_and_dotty_paths_are_rejected() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh = \"work\"\nclaude_config_dir = \"relative/dir\"\n[hosts.home_runner]\nssh_from_work = \"h\"\nsmb_user = \"u\"\n[workspaces.x]\nwork_host = \"work\"\nroot = \"src\"\nruntime_root = \"/tmp/../x\"\nshare = \"x\"\n",
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
        let err = parse_err(&with_workspace(
            "work_host = \"work\"\nroot = \"/Users/Shared/cc-workspaces/x\"\nruntime_root = \"/Users/Shared/cc-workspaces/x/target\"\nshare = \"x\"",
        ));
        assert!(err.message().contains("must not overlap root"), "{err}");
    }

    #[test]
    fn bad_names_and_tokens_are_rejected() {
        let err = parse_err(
            "version = 1\n[hosts.\"my host\"]\nssh = \"-oProxyCommand=x\"\n[hosts.home_runner]\nssh_from_work = \"h\"\nsmb_user = \"u\"\n[workspaces.\"-x\"]\nwork_host = \"my host\"\nroot = \"/a\"\nruntime_root = \"/b\"\nshare = \"has space\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("hosts.my host: name must be"), "{msg}");
        assert!(msg.contains("hosts.my host.ssh must match"), "{msg}");
        assert!(msg.contains("workspaces.-x: name must be"), "{msg}");
        assert!(msg.contains("workspaces.-x.share must match"), "{msg}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let err = parse_err(
            "version = 3\n[workspaces.x]\nwork_host = \"nope\"\nroot = \"rel\"\nruntime_root = \"/b\"\nshare = \"\"\n",
        );
        let lines = err.message().lines().count();
        assert!(lines >= 5, "expected several problems, got:\n{err}");
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
            let toml = with_workspace(&format!("{VALID_WS}\nclaude_permission_mode = \"{text}\""));
            let config = Config::parse(&toml).unwrap();
            assert_eq!(config.workspaces["x"].claude_permission_mode, mode);
        }
    }
}

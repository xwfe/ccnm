//! `~/.config/ccnm/config.toml`, the home machine's source of truth.
//!
//! Secrets never live here (design doc section 5); SSH keys and Claude
//! OAuth stay with OpenSSH and Claude Code itself.
//!
//! Unknown keys are an error, not ignored. A typo like `runtime_hots` that
//! silently falls back to a default is exactly the drift doctor exists to
//! catch, so the parser refuses it up front.
//!
//! Two backends share the schema. `mcp-ssh` (the default and the only one
//! this build implements) needs nothing beyond hosts and `root`. The
//! `hybrid-smb` fallback (appendix A) additionally needs `share`,
//! `runtime_root`, `mount_mode` and the runtime host's `smb_user`; those
//! fields are rejected on an `mcp-ssh` workspace so a half-migrated config
//! cannot look valid.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The only `version = N` this binary understands.
///
/// It is no longer written, and no longer required: a config is what its
/// hosts and workspaces say, and a schema version nobody has ever needed
/// to bump is a line every reader has to wonder about. Old configs still
/// have it, so it is still accepted -- and still checked, because a file
/// that says `version = 2` was written for a ccnm this is not.
pub const SUPPORTED_VERSION: u32 = 1;

/// `workspaces.<name>.runtime_host` when the file does not say.
pub const DEFAULT_RUNTIME_HOST: &str = "home";

/// Where a remote ccnm is invoked when `hosts.<x>.ccnm_bin` is unset. The
/// `~` is expanded by the remote login shell, which is the one thing
/// every POSIX shell and fish agree on; a bare `ccnm` would depend on the
/// PATH of a non-interactive shell (design doc section 7).
pub const DEFAULT_CCNM_BIN: &str = "~/.local/bin/ccnm";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Absent in anything ccnm writes now; see [`SUPPORTED_VERSION`].
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
}

/// One machine. Which fields are required depends on the role a workspace
/// gives it: a `work_host` needs `ssh`, a `runtime_host` needs
/// `ssh_from_work`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// Alias in the home machine's `~/.ssh/config` that reaches this host.
    #[serde(default)]
    pub ssh: Option<String>,
    /// Alias in the *work* machine's `~/.ssh/config` that reaches this host.
    #[serde(default)]
    pub ssh_from_work: Option<String>,
    /// Absolute path of the ccnm binary on this host, for the machine that
    /// sshes in. Unset means [`DEFAULT_CCNM_BIN`].
    #[serde(default)]
    pub ccnm_bin: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` for Claude Code on this host. Unset means Claude's
    /// own default (`~/.claude`) and whatever login is already there. A custom
    /// dir has its own credentials and needs its own `claude auth login`;
    /// ccnm never performs that login (design doc section 21).
    #[serde(default)]
    pub claude_config_dir: Option<PathBuf>,
    /// The dedicated account the MCP runtime must run as on this host
    /// (design doc section 18). ccnm never creates it and never switches
    /// to it; it checks that it is what the runtime is running as, and
    /// refuses `exec_command` when it is not.
    ///
    /// Unset is itself a failure on the runtime host: without it ccnm
    /// cannot tell the dedicated account from the developer's own.
    #[serde(default)]
    pub runtime_user: Option<String>,
    /// Hybrid only: account the work machine mounts the SMB share as.
    #[serde(default)]
    pub smb_user: Option<String>,
}

impl Host {
    /// The path to run on this host from the other machine.
    pub fn ccnm_bin(&self) -> String {
        match &self.ccnm_bin {
            Some(path) => path.to_string_lossy().into_owned(),
            None => DEFAULT_CCNM_BIN.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub backend: Backend,
    /// Key into `hosts`: where Claude Code runs.
    pub work_host: String,
    /// Key into `hosts`: where the project lives and every tool runs.
    #[serde(default = "default_runtime_host")]
    pub runtime_host: String,
    /// Project root on the runtime host. With `mcp-ssh` it neither needs
    /// nor should exist on the work machine.
    pub root: PathBuf,
    #[serde(default)]
    pub claude_permission_mode: PermissionMode,
    /// Run `exec_command` for this workspace even though the runtime
    /// account is not confined (design doc section 18).
    ///
    /// Spelled out rather than shortened on purpose. The default refusal
    /// is the hard gate the design document asks for; this is the way to
    /// say "I know, this is a scratch project, go ahead", and every
    /// result of such a session says so.
    #[serde(default)]
    pub allow_unconfined_exec: bool,
    /// Hybrid only: where the restricted runner may write. Must not overlap
    /// `root`.
    #[serde(default)]
    pub runtime_root: Option<PathBuf>,
    /// Hybrid only: SMB share name the work machine mounts.
    #[serde(default)]
    pub share: Option<String>,
    /// Hybrid only.
    #[serde(default)]
    pub mount_mode: Option<MountMode>,
}

fn default_runtime_host() -> String {
    DEFAULT_RUNTIME_HOST.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// One persistent SSH stdio transport carrying MCP to a ccnm runtime
    /// on the home machine. The primary architecture.
    #[default]
    McpSsh,
    /// SMB mount plus SSH runner (appendix A). Parsed so a config can name
    /// it; not implemented by this build.
    HybridSmb,
}

impl Backend {
    /// The value as written in config.toml.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::McpSsh => "mcp-ssh",
            Backend::HybridSmb => "hybrid-smb",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    /// Mount with `nodatacache,nomdatacache,nopassprompt,soft,nobrowse`
    /// (appendix A.12). The only mode the Hybrid design ever had.
    #[default]
    Coherence,
}

/// Values accepted by `claude --permission-mode`, checked against Claude
/// Code 2.1.260 `--help`. Serialized in Claude's own camelCase so the config
/// file, the session spec and the CLI flag all read the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    pub runtime: &'a Host,
    /// `hosts.<work_host>.ssh`: home -> work.
    pub work_ssh: &'a str,
    /// `hosts.<runtime_host>.ssh_from_work`: work -> home runtime.
    pub home_alias: &'a str,
}

impl Config {
    /// The alias this machine uses to reach the machine holding the
    /// projects, and that host's other settings, when this is a work-side
    /// config.
    ///
    /// The [`Host`] comes back with the alias because `ccnm_bin` is on it:
    /// a caller that took only the alias would run the *default* path on
    /// the far side and ignore what the config said, which fails as
    /// "command not found" on the one machine whose ccnm is somewhere
    /// else.
    ///
    /// A work machine's config is the same file with the workspaces left
    /// out: it says how to reach home and nothing else, because the
    /// workspace list has exactly one home and duplicating it here is how
    /// the two copies start disagreeing about where a project is. That
    /// disagreement is not theoretical -- a session bound to a root the
    /// config no longer names is the failure this project has already
    /// spent an afternoon on.
    ///
    /// `None` unless this really is a work-side config.
    ///
    /// The test is the absence of any `ssh`, not the presence of
    /// `ssh_from_work`: a *home* config has both -- `ssh` to reach the
    /// work machine, `ssh_from_work` to say how the work machine reaches
    /// back -- so keying on `ssh_from_work` alone would make every
    /// mistyped workspace name at home look like a work machine and send
    /// it over ssh. That is not hypothetical; it is what the first
    /// version did, and a test that asks for a workspace which does not
    /// exist is what caught it.
    pub fn home_from_work(&self) -> Option<(&str, &Host)> {
        if self.hosts.values().any(|h| h.ssh.is_some()) {
            return None;
        }
        let mut named = self
            .hosts
            .values()
            .filter_map(|h| Some((h.ssh_from_work.as_deref()?, h)));
        match (named.next(), named.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

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
        let runtime = self
            .hosts
            .get(&workspace.runtime_host)
            .ok_or_else(|| bug("its runtime host"))?;
        Ok(Resolved {
            name,
            workspace,
            work,
            runtime,
            work_ssh: work.ssh.as_deref().ok_or_else(|| bug("work ssh"))?,
            home_alias: runtime
                .ssh_from_work
                .as_deref()
                .ok_or_else(|| bug("runtime ssh_from_work"))?,
        })
    }

    fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();

        if let Some(version) = self.version
            && version != SUPPORTED_VERSION
        {
            problems.push(format!(
                "version = {version} is not supported; this ccnm understands version = {SUPPORTED_VERSION}, and no longer needs the line at all"
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
            if let Some(bin) = &host.ccnm_bin {
                let at = format!("{at}.ccnm_bin");
                if check_absolute(&at, bin, &mut problems) && !is_remote_path(bin) {
                    problems.push(format!(
                        "{at} must contain only [A-Za-z0-9._/-] so the remote shell never has to quote it, got \"{}\"",
                        bin.display()
                    ));
                }
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
            let runtime = self.hosts.get(&ws.runtime_host);
            match runtime {
                None => problems.push(format!(
                    "{at}.runtime_host = \"{}\" does not match any [hosts.*] entry",
                    ws.runtime_host
                )),
                Some(host) if host.ssh_from_work.is_none() => problems.push(format!(
                    "{at}.runtime_host = \"{}\" names a host without `ssh_from_work` (the alias the work machine uses to reach it)",
                    ws.runtime_host
                )),
                Some(_) => {}
            }
            let root_ok = check_absolute(&format!("{at}.root"), &ws.root, &mut problems);

            match ws.backend {
                Backend::McpSsh => {
                    for (field, present) in [
                        ("share", ws.share.is_some()),
                        ("mount_mode", ws.mount_mode.is_some()),
                        ("runtime_root", ws.runtime_root.is_some()),
                    ] {
                        if present {
                            problems.push(format!(
                                "{at}.{field} is only valid with backend = \"hybrid-smb\"; the mcp-ssh runtime has no mount"
                            ));
                        }
                    }
                }
                Backend::HybridSmb => {
                    match &ws.share {
                        None => problems.push(format!(
                            "{at}.share is required with backend = \"hybrid-smb\""
                        )),
                        Some(share) if share.trim().is_empty() => {
                            problems.push(format!("{at}.share must be the SMB share name"));
                        }
                        Some(share) => check_token(&format!("{at}.share"), share, &mut problems),
                    }
                    match &ws.runtime_root {
                        None => problems.push(format!(
                            "{at}.runtime_root is required with backend = \"hybrid-smb\""
                        )),
                        Some(runtime_root) => {
                            let ok = check_absolute(
                                &format!("{at}.runtime_root"),
                                runtime_root,
                                &mut problems,
                            );
                            if root_ok && ok && overlaps(&ws.root, runtime_root) {
                                problems.push(format!(
                                    "{at}.runtime_root must not overlap root: the runner would get write access to source"
                                ));
                            }
                        }
                    }
                    if let Some(host) = runtime
                        && host.smb_user.is_none()
                    {
                        problems.push(format!(
                            "{at}.runtime_host = \"{}\" names a host without `smb_user`, which backend = \"hybrid-smb\" needs to mount the share",
                            ws.runtime_host
                        ));
                    }
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::config(problems.join("\n")))
        }
    }
}

/// Names end up in tmux session names, session ids and state paths, so
/// keep them to characters that are safe everywhere.
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

/// SSH aliases and share names travel through ssh command lines.
/// Restricting them to this set means they never need quoting anywhere.
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

/// A path that will appear verbatim as one word of a remote ssh command.
fn is_remote_path(path: &Path) -> bool {
    path.to_str().is_some_and(|s| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    })
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

    /// A config with only a way home is the work machine's, and a name it
    /// does not know is a question for the other side. A *home* config has
    /// `ssh_from_work` too -- that is how the work machine reaches back --
    /// so keying on that alone would send every mistyped workspace name at
    /// home over ssh to be asked about.
    #[test]
    fn only_a_config_with_no_way_to_reach_work_is_the_work_machines() {
        let alias = |c: &Config| c.home_from_work().map(|(alias, _)| alias.to_string());

        let work_side = Config::parse("[hosts.home]\nssh_from_work = \"xdwmbp\"\n").unwrap();
        assert_eq!(alias(&work_side).as_deref(), Some("xdwmbp"));

        let home_side = Config::parse(
            "[hosts.work]\nssh = \"fodelf\"\n[hosts.home]\nssh_from_work = \"xdwmbp\"\n",
        )
        .unwrap();
        assert_eq!(alias(&home_side), None);

        // Nothing to pick.
        let empty = Config::parse("").unwrap();
        assert_eq!(alias(&empty), None);
        let two =
            Config::parse("[hosts.a]\nssh_from_work = \"x\"\n[hosts.b]\nssh_from_work = \"y\"\n")
                .unwrap();
        assert_eq!(alias(&two), None);
    }

    /// The host comes back with the alias, because `ccnm_bin` is on it.
    /// A work machine whose home keeps ccnm somewhere other than the
    /// default is a supported, documented config; a caller handed only the
    /// alias would silently run the default path instead and fail with
    /// "command not found" on the one machine that was configured
    /// correctly.
    #[test]
    fn the_work_side_lookup_carries_where_ccnm_lives_over_there() {
        let config = Config::parse(
            "[hosts.home]\nssh_from_work = \"xdwmbp\"\nccnm_bin = \"/opt/homebrew/bin/ccnm\"\n",
        )
        .unwrap();
        let (alias, host) = config.home_from_work().unwrap();
        assert_eq!(alias, "xdwmbp");
        assert_eq!(host.ccnm_bin(), "/opt/homebrew/bin/ccnm");

        // Unset still means the default, as everywhere else.
        let plain = Config::parse("[hosts.home]\nssh_from_work = \"xdwmbp\"\n").unwrap();
        assert_eq!(
            plain.home_from_work().unwrap().1.ccnm_bin(),
            DEFAULT_CCNM_BIN
        );
    }
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
            "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home]\nssh_from_work = \"ccnm-home\"\n[workspaces.x]\n{body}\n"
        )
    }

    const VALID_WS: &str = "work_host = \"work\"\nroot = \"/a\"";

    #[test]
    fn valid_fixture_parses_with_defaults() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        assert_eq!(config.version, Some(1));
        let r = config.workspace("xshun").unwrap();
        assert_eq!(r.work_ssh, "work");
        assert_eq!(r.home_alias, "ccnm-home");
        assert_eq!(r.work.claude_config_dir, None);
        assert_eq!(r.work.ccnm_bin(), "~/.local/bin/ccnm");
        assert_eq!(r.runtime.ccnm_bin(), "~/.local/bin/ccnm");
        assert_eq!(r.workspace.backend, Backend::McpSsh);
        assert_eq!(r.workspace.runtime_host, "home");
        assert_eq!(
            r.workspace.root,
            PathBuf::from("/Users/fodelf/Projects/xshun")
        );
        assert_eq!(r.workspace.share, None);
        assert_eq!(r.workspace.runtime_root, None);
        assert_eq!(
            r.workspace.claude_permission_mode,
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn optional_host_fields_are_read() {
        let config = Config::load(&fixture("config-custom-claude-dir.toml")).unwrap();
        let r = config.workspace("xshun").unwrap();
        assert_eq!(
            r.work.claude_config_dir,
            Some(PathBuf::from("/Users/me/.ccnm/claude"))
        );
        assert_eq!(r.work.ccnm_bin(), "/Users/me/bin/ccnm");
        assert_eq!(r.runtime.ccnm_bin(), "/Users/ccrun/.local/bin/ccnm");
        assert_eq!(r.workspace.runtime_host, "runtime");
        assert_eq!(r.home_alias, "home");
    }

    #[test]
    fn hybrid_fixture_parses_but_needs_its_fields() {
        let config = Config::load(&fixture("config-hybrid.toml")).unwrap();
        let ws = &config.workspaces["legacy"];
        assert_eq!(ws.backend, Backend::HybridSmb);
        assert_eq!(ws.share.as_deref(), Some("legacy"));
        assert_eq!(ws.mount_mode, Some(MountMode::Coherence));
        assert_eq!(
            ws.runtime_root,
            Some(PathBuf::from("/Users/Shared/cc-runtime/legacy"))
        );

        let err = parse_err(
            "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home]\nssh_from_work = \"h\"\n[workspaces.x]\nbackend = \"hybrid-smb\"\nwork_host = \"work\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("share is required"), "{msg}");
        assert!(msg.contains("runtime_root is required"), "{msg}");
        assert!(msg.contains("without `smb_user`"), "{msg}");
    }

    #[test]
    fn hybrid_fields_are_rejected_on_mcp_ssh() {
        let err = parse_err(&with_workspace(
            "work_host = \"work\"\nroot = \"/a\"\nshare = \"x\"\nmount_mode = \"coherence\"\nruntime_root = \"/b\"",
        ));
        let msg = err.message();
        for field in ["share", "mount_mode", "runtime_root"] {
            assert!(
                msg.contains(&format!(
                    "{field} is only valid with backend = \"hybrid-smb\""
                )),
                "{field}: {msg}"
            );
        }
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let err = parse_err(&with_workspace(&format!("{VALID_WS}\nbackend = \"nfs\"")));
        assert!(err.message().contains("nfs"), "{err}");
    }

    #[test]
    fn unknown_field_is_rejected_with_its_name() {
        let err = Config::load(&fixture("config-unknown-field.toml")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("runtime_hots"), "{err}");
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
            "work_host = \"nope\"\nruntime_host = \"nada\"\nroot = \"/a\"",
        ));
        let msg = err.message();
        assert!(msg.contains("work_host = \"nope\""), "{msg}");
        assert!(msg.contains("runtime_host = \"nada\""), "{msg}");
    }

    #[test]
    fn role_specific_fields_are_required() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh_from_work = \"x\"\n[hosts.home]\nssh = \"y\"\n[workspaces.x]\nwork_host = \"work\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("host without `ssh`"), "{msg}");
        assert!(msg.contains("host without `ssh_from_work`"), "{msg}");
    }

    #[test]
    fn relative_and_dotty_paths_are_rejected() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh = \"work\"\nclaude_config_dir = \"relative/dir\"\nccnm_bin = \"bin/ccnm\"\n[hosts.home]\nssh_from_work = \"h\"\n[workspaces.x]\nwork_host = \"work\"\nroot = \"/tmp/../x\"\n",
        );
        let msg = err.message();
        assert!(
            msg.contains("claude_config_dir must be an absolute path"),
            "{msg}"
        );
        assert!(msg.contains("ccnm_bin must be an absolute path"), "{msg}");
        assert!(msg.contains("root must not contain"), "{msg}");
    }

    #[test]
    fn ccnm_bin_must_be_a_remote_safe_path() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh = \"work\"\nccnm_bin = \"/Users/me/my tools/ccnm\"\n[hosts.home]\nssh_from_work = \"h\"\n",
        );
        assert!(err.message().contains("never has to quote"), "{err}");
        let ok = Config::parse(
            "version = 1\n[hosts.work]\nssh = \"work\"\nccnm_bin = \"/opt/ccnm-0.1/bin/ccnm\"\n",
        )
        .unwrap();
        assert_eq!(ok.hosts["work"].ccnm_bin(), "/opt/ccnm-0.1/bin/ccnm");
    }

    #[test]
    fn hybrid_runtime_root_inside_root_is_rejected() {
        let err = parse_err(
            "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home]\nssh_from_work = \"h\"\nsmb_user = \"u\"\n[workspaces.x]\nbackend = \"hybrid-smb\"\nwork_host = \"work\"\nroot = \"/Users/Shared/cc-workspaces/x\"\nruntime_root = \"/Users/Shared/cc-workspaces/x/target\"\nshare = \"x\"\n",
        );
        assert!(err.message().contains("must not overlap root"), "{err}");
    }

    #[test]
    fn bad_names_and_tokens_are_rejected() {
        let err = parse_err(
            "version = 1\n[hosts.\"my host\"]\nssh = \"-oProxyCommand=x\"\n[hosts.home]\nssh_from_work = \"h\"\n[workspaces.\"-x\"]\nwork_host = \"my host\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("hosts.my host: name must be"), "{msg}");
        assert!(msg.contains("hosts.my host.ssh must match"), "{msg}");
        assert!(msg.contains("workspaces.-x: name must be"), "{msg}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let err = parse_err("version = 3\n[workspaces.x]\nwork_host = \"nope\"\nroot = \"rel\"\n");
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
            let toml = with_workspace(&format!("{VALID_WS}\nclaude_permission_mode = \"{text}\""));
            let config = Config::parse(&toml).unwrap();
            assert_eq!(config.workspaces["x"].claude_permission_mode, mode);
        }
    }
}

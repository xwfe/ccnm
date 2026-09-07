//! Internal Agent Provider boundary. Phase one deliberately supports only Claude.
//!
//! Providers own official CLI syntax, observations, policy and project context.
//! They do not own topology, SSH aliases, process supervision or runtime tools.
//! v1 config/wire names stay unchanged; there is no plugin registry or selector.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::{Node, Workspace};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, ProcessRunner};
use crate::session::{Dir, Spec};

pub mod claude;
pub mod context;
mod types;

pub use claude::PermissionMode;
pub use types::{AgentReport, Ask, AuthStatus, RunResult, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProvider {
    Claude,
}

/// Names and locations only. This describes existing checks; it grants no
/// permission to read, copy or forward credentials.
pub struct CredentialMetadata {
    pub env_prefixes: &'static [&'static str],
    pub config_env: &'static str,
    pub default_directory: &'static str,
    pub files: &'static [&'static str],
    pub agent_name: &'static str,
    pub vendor_name: &'static str,
    pub login_command: &'static str,
    pub egress_host: &'static str,
}

impl CredentialMetadata {
    pub fn is_environment_name(&self, name: &OsStr) -> bool {
        let name = name.to_string_lossy();
        self.env_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix))
    }

    pub fn config_directories(&self, home: &Path, custom: Option<&OsStr>) -> Vec<PathBuf> {
        let mut dirs = vec![home.join(self.default_directory)];
        if let Some(custom) = custom {
            dirs.push(PathBuf::from(custom));
        }
        dirs
    }
}

impl AgentProvider {
    /// v1 has exactly one provider, including when reading an older session.
    pub const fn current() -> Self {
        Self::Claude
    }

    pub fn locate(self, path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
        match self {
            Self::Claude => claude::locate(path_var, home),
        }
    }

    pub fn locate_from_env(self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.locate(std::env::var_os("PATH").as_deref(), home.as_deref())
    }

    /// Translate the existing config at the boundary, without renaming it.
    pub fn config_dir(self, node: &Node) -> Option<&Path> {
        match self {
            Self::Claude => node.claude_config_dir.as_deref(),
        }
    }

    pub fn permission_mode(self, workspace: &Workspace) -> PermissionMode {
        match self {
            Self::Claude => workspace.claude_permission_mode,
        }
    }

    pub fn parse_permission_mode(self, raw: &str) -> Result<PermissionMode> {
        match self {
            Self::Claude => claude::parse_permission_mode(raw),
        }
    }

    pub fn report(
        self,
        bin: Option<&Path>,
        config_dir: Option<&Path>,
        runner: &dyn ProcessRunner,
        ask: Ask,
    ) -> AgentReport {
        match self {
            Self::Claude => claude::report(bin, config_dir, runner, ask),
        }
    }

    pub fn launch_cmd(self, bin: &Path, spec: &Spec, dir: &Dir) -> Cmd {
        match self {
            Self::Claude => claude::launch_cmd(bin, spec, dir),
        }
    }

    pub fn parse_result(self, stdout: &[u8]) -> Result<RunResult> {
        match self {
            Self::Claude => claude::parse_print(stdout),
        }
    }

    pub fn mcp_config(self, transport: &Cmd) -> serde_json::Value {
        match self {
            Self::Claude => claude::mcp_config(transport),
        }
    }

    pub fn transport_payload(self, dir: &Dir) -> Option<String> {
        match self {
            Self::Claude => claude::transport_payload(dir),
        }
    }

    pub fn settings(self, remote: bool) -> serde_json::Value {
        match self {
            Self::Claude => claude::settings(remote),
        }
    }

    pub(crate) fn write_session_files(self, dir: &Dir, transport: Option<&Cmd>) -> Result<()> {
        match self {
            Self::Claude => claude::write_session_files(dir, transport),
        }
    }

    pub const fn project_file(self) -> &'static str {
        match self {
            Self::Claude => claude::context::PROJECT_FILE,
        }
    }

    pub const fn credentials(self) -> &'static CredentialMetadata {
        match self {
            Self::Claude => &claude::CREDENTIALS,
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
        }
    }

    pub const fn authentication_check(self) -> &'static str {
        match self {
            Self::Claude => "Claude authentication",
        }
    }

    pub fn auth_hint(self, config_dir: Option<&Path>) -> String {
        match self {
            Self::Claude => claude::auth_hint(config_dir),
        }
    }

    pub(crate) fn unexpected_probe_reply(self, report: &AgentReport) -> String {
        match self {
            Self::Claude => claude::unexpected_probe_reply(report),
        }
    }

    pub fn missing_controller_cli(self) -> Error {
        match self {
            Self::Claude => Error::new(
                ErrorCode::Version,
                "claude not found in the controller's environment; it looked in launchd's PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
            ),
        }
    }
}

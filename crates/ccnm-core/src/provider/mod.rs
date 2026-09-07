//! Internal Agent Provider boundary. Public commands still default to Claude;
//! measured Codex integration is selected only by versioned internal requests.
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
pub mod codex;
mod result;
pub use result::AgentResult;
pub mod context;
mod types;

pub use claude::PermissionMode;
pub use types::{AgentReport, Ask, AuthStatus, RunResult, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    #[default]
    Claude,
    Codex,
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
    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
    pub const fn tools_down_hint(self) -> &'static str {
        match self {
            Self::Claude => "TOOLS DOWN (in Claude: /mcp -> ccnm -> Reconnect)",
            Self::Codex => {
                "TOOLS DOWN (exit Codex and use official resume with its exact session ID on the Agent Node)"
            }
        }
    }

    pub fn is_claude(&self) -> bool {
        *self == Self::Claude
    }
    pub const fn control_protocol(self) -> u32 {
        match self {
            Self::Claude => 1,
            Self::Codex => 2,
        }
    }

    /// v1 has exactly one provider, including when reading an older session.
    pub const fn current() -> Self {
        Self::Claude
    }

    pub fn locate(self, path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
        match self {
            Self::Claude => claude::locate(path_var, home),
            Self::Codex => codex::locate(path_var, home),
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
            Self::Codex => None,
        }
    }

    pub fn permission_mode(self, workspace: &Workspace) -> PermissionMode {
        match self {
            Self::Claude => workspace.claude_permission_mode,
            Self::Codex => PermissionMode::default(),
        }
    }

    pub fn parse_permission_mode(self, raw: &str) -> Result<PermissionMode> {
        match self {
            Self::Claude => claude::parse_permission_mode(raw),
            Self::Codex => Err(Error::invalid_args(
                "Codex uses its measured fixed tool policy",
            )),
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
            Self::Codex => codex::report(bin, runner, ask),
        }
    }

    pub fn launch_cmd(self, bin: &Path, spec: &Spec, dir: &Dir) -> Result<Cmd> {
        match self {
            Self::Claude => Ok(claude::launch_cmd(bin, spec, dir)),
            Self::Codex => codex::launch_cmd(bin, spec, dir),
        }
    }

    pub fn parse_result(self, stdout: &[u8]) -> Result<AgentResult> {
        match self {
            Self::Claude => claude::parse_print(stdout).map(AgentResult::Claude),
            Self::Codex => {
                let mut result = codex::result::parse(stdout)?;
                result.redact(&codex::home()?.to_string_lossy());
                Ok(AgentResult::Codex(result))
            }
        }
    }

    pub fn redact_output(self, text: String) -> String {
        match self {
            Self::Claude => text,
            Self::Codex => codex::home().map_or_else(
                |_| "Codex output withheld: Agent home unavailable".into(),
                |home| text.replace(home.to_string_lossy().as_ref(), "<agent-private-config>"),
            ),
        }
    }

    pub fn transport_payload(self, dir: &Dir) -> Option<String> {
        match self {
            Self::Claude => claude::transport_payload(dir),
            Self::Codex => crate::session::load(dir)
                .ok()
                .and_then(|spec| codex::transport::command(&spec).ok())
                .and_then(|cmd| {
                    cmd.args
                        .last()
                        .and_then(|arg| arg.to_str())
                        .and_then(|text| text.split_whitespace().last())
                        .map(str::to_owned)
                }),
        }
    }

    pub(crate) fn write_session_files(self, dir: &Dir, transport: Option<&Cmd>) -> Result<()> {
        match self {
            Self::Claude => claude::write_session_files(dir, transport),
            Self::Codex => Ok(()),
        }
    }

    pub const fn project_file(self) -> &'static str {
        match self {
            Self::Claude => claude::context::PROJECT_FILE,
            Self::Codex => "AGENTS.md",
        }
    }

    pub const fn credentials(self) -> &'static CredentialMetadata {
        match self {
            Self::Claude => &claude::CREDENTIALS,
            Self::Codex => &codex::CREDENTIALS,
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    pub const fn authentication_check(self) -> &'static str {
        match self {
            Self::Claude => "Claude authentication",
            Self::Codex => "Codex authentication",
        }
    }

    pub fn auth_hint(self, config_dir: Option<&Path>) -> String {
        match self {
            Self::Claude => claude::auth_hint(config_dir),
            Self::Codex => "In the Agent Node login session, independently run official codex login using ccnm's dedicated Agent-local home; never copy credentials".into(),
        }
    }

    pub(crate) fn unexpected_probe_reply(self, report: &AgentReport) -> String {
        match self {
            Self::Claude => claude::unexpected_probe_reply(report),
            Self::Codex => format!("Codex({report:?})"),
        }
    }

    pub fn missing_controller_cli(self) -> Error {
        match self {
            Self::Claude => Error::new(
                ErrorCode::Version,
                "claude not found in the controller's environment; it looked in launchd's PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
            ),
            Self::Codex => Error::new(
                ErrorCode::Version,
                "codex not found in the controller environment",
            ),
        }
    }
}

/// CLI paths measured in the process that will start the Agent. No credentials.
#[derive(Debug, Clone, Default)]
pub struct AgentBinaries {
    claude: Option<PathBuf>,
    codex: Option<PathBuf>,
}
impl AgentBinaries {
    pub fn discover() -> Self {
        Self {
            claude: AgentProvider::Claude.locate_from_env(),
            codex: AgentProvider::Codex.locate_from_env(),
        }
    }
    pub fn with_claude(claude: Option<PathBuf>) -> Self {
        Self {
            claude,
            codex: None,
        }
    }
    pub fn with_codex(mut self, codex: Option<PathBuf>) -> Self {
        self.codex = codex;
        self
    }
    pub fn get(&self, provider: AgentProvider) -> Option<&Path> {
        match provider {
            AgentProvider::Claude => self.claude.as_deref(),
            AgentProvider::Codex => self.codex.as_deref(),
        }
    }
}

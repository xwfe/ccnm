//! Internal Agent Provider boundary. Claude Code is the only Agent ccnm
//! starts: P118 removed Codex. The type stays because `provider` is written
//! in the instance config, the session record and the wire, and a `codex`
//! in any of them has to be refused by name rather than read as Claude.
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
mod result;
pub use result::AgentResult;
pub mod context;
mod types;

pub use claude::PermissionMode;
pub use types::{AgentReport, Ask, AuthStatus, RunResult, Usage};

/// What stands in for an Agent-private directory in anything shown outside.
pub const REDACTED: &str = "<agent-private-config>";

/// See [`AgentProvider::output_redaction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redaction {
    /// Nothing private is known to be in it.
    Keep,
    /// Replace every occurrence of this path with [`REDACTED`].
    Replace(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    #[default]
    Claude,
}

/// What a `codex` provider meets since P118, wherever it is read: an
/// instance or profile in the config, a session record, a result, a message
/// from an older ccnm. One sentence, so every place refuses it the same way
/// and the person editing the config learns what to do instead.
pub const CODEX_REMOVED: &str = "provider \"codex\" is no longer supported: since 1.0 ccnm starts Claude Code sessions only. Delete the [agents.*] or [profiles.*] table that names it and point the workspaces using it at a Claude instance; the Codex login directory is left in place for you to delete";

impl<'de> serde::Deserialize<'de> for AgentProvider {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Err(serde::de::Error::custom(CODEX_REMOVED)),
            other => Err(serde::de::Error::unknown_variant(other, &["claude"])),
        }
    }
}

/// Names and locations only. This describes existing checks; it grants no
/// permission to read, copy or forward credentials.
pub struct CredentialMetadata {
    pub env_prefixes: &'static [&'static str],
    pub config_env: &'static str,
    pub default_directory: &'static str,
    pub additional_directories: &'static [&'static str],
    /// Relative to Agent/Runtime-local XDG_CONFIG_HOME, never a wire value.
    pub xdg_config_directory: Option<&'static str>,
    /// Known OS credential containers; access is a conservative risk signal.
    pub containers: &'static [&'static str],
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

/// Codex's login, by name and place only. ccnm no longer starts Codex
/// (P118), but a Runtime account can still have it installed and logged in
/// -- `exec_sandbox = "codex"` runs one -- and a model's command must reach
/// that login no more than Claude's. Dropping this would quietly let
/// `OPENAI_*` variables into every command and take the `.codex` check out
/// of doctor.
pub const CODEX_CREDENTIALS: CredentialMetadata = CredentialMetadata {
    env_prefixes: &["CODEX_", "OPENAI_"],
    config_env: "CODEX_HOME",
    default_directory: ".config/ccnm/agents/codex",
    additional_directories: &[".codex"],
    xdg_config_directory: Some("ccnm/agents/codex"),
    containers: &[],
    files: &["auth.json"],
    agent_name: "Codex",
    vendor_name: "OpenAI",
    login_command: "codex login",
    egress_host: "api.openai.com",
};

/// Every Agent login the safety checks keep away from the Runtime's tools.
pub const KNOWN_CREDENTIALS: [&CredentialMetadata; 2] = [&claude::CREDENTIALS, &CODEX_CREDENTIALS];

impl AgentProvider {
    pub const ALL: [Self; 1] = [Self::Claude];

    pub const fn transport_program(self) -> &'static str {
        "ssh"
    }
    pub const fn cli_name(self) -> &'static str {
        "claude"
    }
    /// What a session whose tools are gone should be told. The English
    /// keeps `TOOLS DOWN`: that is the heading people search
    /// docs/troubleshooting.md for.
    pub fn tools_down_hint(self, lang: crate::Lang) -> String {
        let back = self.way_back(lang);
        lang.pick(format!("断了——{back}"), format!("TOOLS DOWN ({back})"))
    }

    /// How a session gets its tools back, as something a person can type
    /// without it going wrong.
    pub fn way_back(self, lang: crate::Lang) -> String {
        lang.pick(
            "在 Claude 里：/mcp → ccnm → Reconnect",
            "in Claude: /mcp -> ccnm -> Reconnect",
        )
        .to_string()
    }

    pub fn is_claude(&self) -> bool {
        *self == Self::Claude
    }
    pub const fn control_protocol(self) -> u32 {
        1
    }

    /// v1 has exactly one provider, including when reading an older session.
    pub const fn current() -> Self {
        Self::Claude
    }

    pub fn locate(self, path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
        claude::locate(path_var, home)
    }

    pub fn locate_from_env(self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.locate(std::env::var_os("PATH").as_deref(), home.as_deref())
    }

    /// Translate the existing config at the boundary, without renaming it.
    pub fn config_dir(self, node: &Node) -> Option<&Path> {
        node.claude_config_dir.as_deref()
    }

    pub fn permission_mode(self, workspace: &Workspace) -> PermissionMode {
        workspace.claude_permission_mode
    }

    pub fn parse_permission_mode(self, raw: &str) -> Result<PermissionMode> {
        claude::parse_permission_mode(raw)
    }

    pub fn report(
        self,
        bin: Option<&Path>,
        config_dir: Option<&Path>,
        runner: &dyn ProcessRunner,
        ask: Ask,
    ) -> AgentReport {
        self.report_at(bin, config_dir, runner, ask)
    }

    pub fn report_at(
        self,
        bin: Option<&Path>,
        profile_dir: Option<&Path>,
        runner: &dyn ProcessRunner,
        ask: Ask,
    ) -> AgentReport {
        claude::report(bin, profile_dir, runner, ask)
    }

    /// The login's subscription limits, from its own CLI (P105). Only from a
    /// login session, like [`Ask::Everything`].
    ///
    /// Version and login first, through the same commands doctor runs: a
    /// Claude Code with no login answers `/usage` with the same cost lines as
    /// an API key (measured on 2.1.303), so a login missing has to be said
    /// before `/usage` can be misread as "no limits".
    pub fn quota_at(
        self,
        bin: Option<&Path>,
        profile_dir: Option<&Path>,
        runner: &dyn ProcessRunner,
        workdir: &Path,
    ) -> Result<crate::quota::Quota> {
        let report = self.report_at(bin, profile_dir, runner, Ask::Everything);
        let version = report.version.map_err(Error::from)?;
        let auth = report.auth.map_err(Error::from)?;
        if !auth.logged_in {
            return Err(Error::new(ErrorCode::Auth, self.auth_hint(profile_dir)));
        }
        let bin = report
            .path
            .ok_or_else(|| Error::internal("a CLI answered without a path"))?;
        if !claude::usage::measured(&version) {
            return Err(Error::new(
                ErrorCode::Version,
                format!(
                    "Claude Code {version} is older than {}, the first `claude --print /usage` was measured on; not asking it, since an older one may send /usage to the model as a prompt\nupgrade Claude Code there, or open claude on that machine and type /usage",
                    claude::usage::MIN_VERSION
                ),
            ));
        }
        let out = runner.run(&claude::usage::cmd(&bin, profile_dir, workdir))?;
        claude::usage::parse(&out, &version, auth.subscription_type)
    }

    pub fn launch_cmd(self, bin: &Path, spec: &Spec, dir: &Dir) -> Result<Cmd> {
        self.launch_cmd_at(bin, spec, dir, spec.provider_config_dir.as_deref())
    }

    pub fn launch_cmd_at(
        self,
        bin: &Path,
        spec: &Spec,
        dir: &Dir,
        profile_dir: Option<&Path>,
    ) -> Result<Cmd> {
        let mut local = spec.clone();
        local.provider_config_dir = profile_dir.map(Path::to_path_buf);
        Ok(claude::launch_cmd(bin, &local, dir))
    }

    pub fn parse_result(self, stdout: &[u8]) -> Result<AgentResult> {
        self.parse_result_at(stdout, None)
    }

    pub fn parse_result_at(self, stdout: &[u8], profile_dir: Option<&Path>) -> Result<AgentResult> {
        let mut result = claude::parse_print(stdout)?;
        if let Some(dir) = profile_dir {
            result.redact(&dir.to_string_lossy());
        }
        Ok(AgentResult::Claude(result))
    }

    pub fn redact_output_at(self, text: String, profile_dir: Option<&Path>) -> String {
        match self.output_redaction(profile_dir) {
            Redaction::Keep => text,
            Redaction::Replace(private) => text.replace(&private, REDACTED),
        }
    }

    /// What has to go before this provider's output leaves the Agent.
    ///
    /// The one rule both the short tails in a report and the full retained
    /// output view (P59) apply, so the two cannot drift apart: the profile
    /// directory, when there is one.
    pub fn output_redaction(self, profile_dir: Option<&Path>) -> Redaction {
        match profile_dir {
            Some(dir) => Redaction::Replace(dir.to_string_lossy().into_owned()),
            None => Redaction::Keep,
        }
    }

    pub fn transport_payload(self, dir: &Dir) -> Option<String> {
        let wire = crate::session::load(dir)
            .ok()
            .and_then(|spec| crate::session::transport::command(&spec).ok())
            .and_then(|cmd| {
                cmd.args
                    .last()
                    .and_then(|arg| arg.to_str())
                    .map(str::to_owned)
            });
        wire.or_else(|| claude::transport_payload(dir))
    }

    pub(crate) fn write_session_files(
        self,
        dir: &Dir,
        transport: Option<&Cmd>,
        agent_tools: &crate::config::AgentTools,
        agent_server: Option<&crate::mcp::agent_skills::Recorded>,
        session_hook: Option<&str>,
    ) -> Result<()> {
        claude::write_session_files(dir, transport, agent_tools, agent_server, session_hook)
    }

    pub const fn project_file(self) -> &'static str {
        claude::context::PROJECT_FILE
    }

    pub const fn credentials(self) -> &'static CredentialMetadata {
        &claude::CREDENTIALS
    }

    pub const fn display_name(self) -> &'static str {
        "Claude Code"
    }

    pub const fn authentication_check(self) -> &'static str {
        "Claude authentication"
    }

    pub fn auth_hint(self, config_dir: Option<&Path>) -> String {
        claude::auth_hint(config_dir)
    }

    pub(crate) fn unexpected_probe_reply(self, report: &AgentReport) -> String {
        claude::unexpected_probe_reply(report)
    }

    pub fn missing_controller_cli(self) -> Error {
        Error::new(
            ErrorCode::Version,
            "claude not found in the controller's environment; it looked in the controller service's PATH (launchd or systemd, not your shell's), ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
        )
    }
}

/// CLI paths measured in the process that will start the Agent. No credentials.
#[derive(Debug, Clone, Default)]
pub struct AgentBinaries {
    claude: Option<PathBuf>,
}
impl AgentBinaries {
    pub fn discover() -> Self {
        Self {
            claude: AgentProvider::Claude.locate_from_env(),
        }
    }
    pub fn with_claude(claude: Option<PathBuf>) -> Self {
        Self { claude }
    }
    pub fn get(&self, _provider: AgentProvider) -> Option<&Path> {
        self.claude.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P118: a `codex` provider, wherever it is read, is refused with the
    /// sentence that says what to do -- not with serde's "expected claude",
    /// which reads as "rename it" and would point a Codex login at Claude.
    #[test]
    fn a_codex_provider_is_refused_by_name() {
        let err = serde_json::from_str::<AgentProvider>("\"codex\"").unwrap_err();
        assert!(err.to_string().contains(CODEX_REMOVED), "{err}");
        let err = serde_json::from_str::<AgentProvider>("\"gemini\"").unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `gemini`"),
            "{err}"
        );
        assert_eq!(
            serde_json::from_str::<AgentProvider>("\"claude\"").unwrap(),
            AgentProvider::Claude
        );
        assert_eq!(
            serde_json::to_string(&AgentProvider::Claude).unwrap(),
            "\"claude\""
        );
    }

    /// Codex's login stays on the list the Runtime is checked against,
    /// though ccnm no longer starts Codex: the Runtime account may still
    /// have one (P118.1).
    #[test]
    fn codex_credentials_are_still_kept_out() {
        let names: Vec<_> = KNOWN_CREDENTIALS.iter().map(|c| c.agent_name).collect();
        assert_eq!(names, ["Claude", "Codex"]);
        assert!(CODEX_CREDENTIALS.is_environment_name(OsStr::new("OPENAI_API_KEY")));
    }
}

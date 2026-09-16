//! Starting, attaching to, listing and ending Claude sessions on the Agent
//! Node: `agent-run` (print mode, waits and returns the result) and
//! `agent-start` / `attach` / `status` / `stop` (interactive, which return
//! immediately because the session outlives the call).
//!
//! A request names the Runtime Node rather than describing how to dial it.
//! An ssh alias only means something to the machine whose `~/.ssh/config`
//! defines it, so the Agent Node looks the name up in its own config and
//! uses its own alias. That also lets it recognise the case where the name
//! is *itself*: agent and project on one machine, nothing to dial, Claude
//! working with its native tools.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::payload::Protocol;
use crate::config::PermissionMode;
use crate::controller::Context;
use crate::instance::{AgentIdentity, InstanceRef};
use crate::lang::Lang;
use crate::provider::AgentResult;
use crate::session::Outcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    pub workspace: String,
    /// Project root on the Runtime Node; passed through to the MCP payload.
    pub root: PathBuf,
    /// The node holding the project, by name. The Agent Node resolves it
    /// against its own config; when it names the Agent Node itself there
    /// is nothing to dial.
    pub runtime_node: String,
    #[serde(rename = "claude_config_dir")]
    pub provider_config_dir: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    /// The one prompt of a print-mode session.
    pub prompt: String,
    /// Claude is killed after this many seconds.
    pub timeout_secs: u64,
    /// The workspace runs Codex through exec-server (P23). A print session
    /// cannot (`codex exec` needs the project on the Agent Node, P21.1),
    /// so the Agent refuses before creating anything rather than quietly
    /// starting an MCP session instead. Sent only when true, so an Agent
    /// that predates the field still reads every other request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub codex_exec_server: bool,
}

impl Protocol for RunRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() {
            3
        } else {
            self.provider.control_protocol()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    /// The ccnm session id, which is also the name of its directory on both
    /// machines. A provider thread/resume id is separate result metadata.
    pub session: String,
    pub session_dir: PathBuf,
    /// The controller that started it, for the record of which session it
    /// was in.
    pub controller: Context,
    /// The supervisor's pid.
    pub pid: u32,
    pub outcome: Outcome,
    /// Claude's `--output-format json` document, when stdout held one.
    pub result: Option<AgentResult>,
    /// The end of stdout when it was not a result document, and the end of
    /// stderr always: enough to see why, never the whole thing.
    pub stdout_tail: String,
    pub stderr_tail: String,
}

impl Protocol for RunReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

impl RunReport {
    /// What worked and what it cost, in the order someone reading a
    /// terminal wants them.
    pub fn summary(&self) -> String {
        self.summary_in(Lang::En)
    }

    pub fn summary_in(&self, lang: Lang) -> String {
        let mut lines = vec![
            format!("{}{}", label(lang.pick("会话", "session")), self.session),
            format!(
                "{}{}{}",
                label(lang.pick("起于", "started")),
                lang.pick("", "by "),
                self.controller.describe()
            ),
            // The provider's own name is the label here, and it is never
            // translated: `claude` and `codex` are the binaries.
            format!(
                "{}{}",
                label(self.provider.cli_name()),
                self.outcome.describe()
            ),
        ];
        let run = label(lang.pick("结果", "run"));
        match &self.result {
            Some(r) => lines.push(format!("{run}{}", r.summary())),
            None => lines.push(format!(
                "{run}{}",
                lang.pick("stdout 上没有结果文档", "no result document on stdout")
            )),
        }
        lines.join("\n")
    }
}

/// `ccnm internal agent-start`: bring up an interactive session, or say that
/// one is already up. Carries no timeout — an interactive session ends when
/// the person using it ends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    pub workspace: String,
    pub root: PathBuf,
    /// The node holding the project, by name. The Agent Node resolves it
    /// against its own config; when it names the Agent Node itself there
    /// is nothing to dial.
    pub runtime_node: String,
    #[serde(rename = "claude_config_dir")]
    pub provider_config_dir: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    /// What Claude opens with; `None` opens an empty prompt.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The workspace runs Codex through exec-server (P23): a Codex session
    /// started for it gets Codex's own tools over the Runtime's
    /// `exec-serve` instead of ccnm's MCP server. Says nothing to a Claude
    /// session. Sent only when true, so an Agent that predates the field
    /// still reads every other request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub codex_exec_server: bool,
}

impl Protocol for StartRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() {
            3
        } else {
            self.provider.control_protocol()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartReport {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    /// The ccnm session id, when it is known. Not known for a session an
    /// older build started without recording it in the tmux environment.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub session_dir: Option<PathBuf>,
    /// The tmux session to attach to.
    pub tmux_session: String,
    /// The tmux server's pid: one server holds every session on this
    /// machine, and it is what `ccnm status` reports as still alive.
    pub server_pid: u32,
    /// True when nothing was started because the session was already
    /// running. Not an error: `ccnm run` on a live workspace means "put me
    /// back in it", not "start a second Claude on the same project".
    pub already_running: bool,
    /// The root of the session this one replaced, when starting meant
    /// ending one that was working somewhere the workspace no longer
    /// points at.
    #[serde(default)]
    pub replaced: Option<PathBuf>,
    /// The controller, when this call went through it. `None` when the
    /// session was already up, since nothing needed starting.
    #[serde(default)]
    pub controller: Option<Context>,
    /// Where Claude actually runs, measured from inside it by the
    /// supervisor. `None` before it has written that down, which is the
    /// first second or so of a session's life.
    #[serde(default)]
    pub context: Option<crate::session::Context>,
}

impl Protocol for StartReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

impl StartReport {
    pub fn summary(&self) -> String {
        self.summary_in(Lang::En)
    }

    pub fn summary_in(&self, lang: Lang) -> String {
        let what = if self.already_running {
            lang.pick("本来就在跑", "already running")
        } else {
            lang.pick("起好了", "started")
        };
        let mut lines = vec![format!(
            "{}{} ({what}, tmux server pid {})",
            label(lang.pick("会话", "session")),
            self.tmux_session,
            self.server_pid
        )];
        if let Some(old) = &self.replaced {
            let old = old.display();
            lines.push(format!(
                "{}{}",
                label(lang.pick("替掉了", "replaced")),
                lang.pick(
                    format!("一个在 {old} 里干活的会话 —— 这个 workspace 已经不指那儿了"),
                    format!(
                        "a session working in {old} -- this workspace does not point there any more"
                    ),
                )
            ));
        }
        if let Some(id) = &self.session {
            lines.push(format!("{}{id}", label("id")));
        }
        if let Some(ctx) = &self.controller {
            lines.push(format!(
                "{}{}{}",
                label(lang.pick("起于", "started")),
                lang.pick("", "by "),
                ctx.describe()
            ));
        }
        if let Some(context) = &self.context {
            lines.push(format!(
                "{} {} {}",
                self.provider.cli_name(),
                lang.pick("在", "in"),
                context.describe()
            ));
        }
        lines.join("\n")
    }
}

/// A report's left-hand label, padded to a fixed column by display width.
///
/// The labels used to be written with the spaces counted by hand, which
/// only works while every label is ASCII: `会话` is two characters and
/// four columns, so a hand-counted `会话      ` would be two columns too
/// wide and every value after it would sit in a different place.
fn label(text: &str) -> String {
    crate::lang::pad(text, 10)
}

/// `ccnm internal attach`: hand this terminal to the workspace's session.
/// The only internal command that answers with a terminal instead of a
/// JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    pub protocol: u32,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Protocol for AttachRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

/// `ccnm internal agent-stop`: end the workspace's session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {
    pub protocol: u32,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Protocol for StopRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    pub protocol: u32,
    pub tmux_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    /// False when there was nothing to stop, which is not an error.
    pub killed: bool,
}

impl Protocol for StopReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

/// `ccnm internal agent-result`: what a session that already ran produced.
///
/// This exists for the interruption `--print` cannot survive. The session
/// itself does: it is the supervisor's child, not the ssh's, so it runs on
/// and writes its result to the session directory. What breaks is the
/// waiting — the ssh carrying `agent-run` dies with the laptop lid, and the
/// answer is on the other machine with no way to ask for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultRequest {
    pub protocol: u32,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    /// A session id; without one, the workspace's most recent session.
    #[serde(default)]
    pub session: Option<String>,
}

impl Protocol for ResultRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultReport {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    pub session: String,
    pub session_dir: PathBuf,
    /// `print` or `interactive`.
    pub mode: String,
    /// When it started, unix seconds, from the session directory.
    pub started: u64,
    /// `None` while it is still running.
    pub outcome: Option<Outcome>,
    pub result: Option<AgentResult>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

impl Protocol for ResultReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

impl ResultReport {
    pub fn summary(&self) -> String {
        self.summary_in(Lang::En)
    }

    pub fn summary_in(&self, lang: Lang) -> String {
        let state = match &self.outcome {
            None => lang.pick("还在跑", "still running").to_string(),
            Some(o) => o.describe(),
        };
        let mut lines = vec![
            format!(
                "{}{} ({})",
                label(lang.pick("会话", "session")),
                self.session,
                self.mode
            ),
            format!("{}{state}", label(self.provider.cli_name())),
        ];
        if let Some(r) = &self.result {
            lines.push(format!(
                "{}{}",
                label(lang.pick("结果", "run")),
                r.summary()
            ));
        }
        lines.join("\n")
    }
}

/// `ccnm internal agent-purge`: delete what ccnm kept for a workspace.
///
/// Only ccnm's own bookkeeping -- the session records and the directory
/// Claude ran in. **Never the project**: that is the one thing on either
/// machine ccnm did not create, and a cleanup command that could delete
/// someone's source tree is not a cleanup command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeRequest {
    pub protocol: u32,
    pub workspace: String,
}

impl Protocol for PurgeRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeReport {
    pub protocol: u32,
    /// What was deleted, as paths, for the caller to print.
    pub removed: Vec<String>,
    /// The session ids that were removed, so the other machine can clear
    /// its half of the same sessions.
    #[serde(default)]
    pub sessions: Vec<String>,
}

impl Protocol for PurgeReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// `ccnm internal agent-status`: every live session on the Agent Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusRequest {
    pub protocol: u32,
    /// Only this workspace's session; `None` for all of ccnm's.
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Protocol for StatusRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    /// `tmux -V`, or why it could not be asked.
    pub tmux: crate::error::Reported<String>,
    pub sessions: Vec<LiveSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<SessionRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Running,
    Completed,
    Failed,
    Stopping,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session: String,
    pub workspace: String,
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

impl Protocol for StatusReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

/// One live interactive session, as the Agent Node sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSession {
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    pub tmux_session: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    /// Unix seconds.
    pub created: u64,
    /// How many terminals are attached right now; 0 means it is running
    /// with nobody watching, which is the normal state after a detach.
    pub attached: u32,
    #[serde(default)]
    pub context: Option<crate::session::Context>,
    /// Whether the session's MCP transport — the one ssh that carries
    /// every tool call to the project — is still running.
    ///
    /// `None` when it could not be determined. `Some(false)` is the one
    /// state worth interrupting someone for: the terminal still works, the
    /// model still answers, and every tool it has is gone.
    #[serde(default)]
    pub tools: Option<bool>,
}

impl LiveSession {
    /// What to say about this session in one line.
    pub fn describe(&self) -> String {
        let attached = match self.attached {
            0 => "detached".to_string(),
            n => format!("{n} attached"),
        };
        let tools = match self.tools {
            Some(true) => "tools connected",
            Some(false) => self.provider.tools_down_hint(),
            None => "tools unknown",
        };
        format!(
            "{}  {}  {attached}  {tools}  ({})",
            self.tmux_session,
            self.workspace.as_deref().unwrap_or("-"),
            self.context.as_ref().map_or(
                "context unknown".to_string(),
                crate::session::Context::describe
            ),
        )
    }
}

impl StatusReport {
    pub fn render(&self) -> String {
        self.render_in(Lang::En)
    }

    pub fn render_in(&self, lang: Lang) -> String {
        let mut out = match &self.tmux {
            Ok(v) => lang.pick(
                format!("Agent Node 上的 tmux {v}\n"),
                format!("tmux {v} on the Agent Node\n"),
            ),
            Err(e) => format!("tmux: {}\n", e.message),
        };
        if self.sessions.is_empty() {
            // Worth reading twice before trusting: this counts tmux
            // sessions on the Agent Node, and a `--print` run is not one
            // of them even though it holds the workspace's write guard.
            // See docs/operations.md.
            out.push_str(lang.pick(
                "没有在跑的会话（--print 的运行不算在内）\n",
                "no live sessions\n",
            ));
        }
        for s in &self.sessions {
            out.push_str(&s.describe());
            out.push('\n');
        }
        for record in &self.records {
            out.push_str(&format!(
                "{}  {}  {:?}  {}\n",
                record.session,
                record.workspace,
                record.state,
                record
                    .agent_identity
                    .as_ref()
                    .map_or("legacy".to_string(), |id| id.instance.clone()),
            ));
        }
        out
    }
}

/// `ccnm internal agent-history`: the sessions this Agent Node has kept a
/// record of, finished ones included, newest first. What `ccnm log` shows.
///
/// A separate command rather than a field on [`StatusRequest`]: that one
/// is `deny_unknown_fields`, so an older Agent would refuse the whole
/// status call instead of only the part it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    pub protocol: u32,
    /// Only this workspace's sessions; `None` for all of them.
    #[serde(default)]
    pub workspace: Option<String>,
    /// At most this many, after sorting.
    pub limit: u32,
}

impl Protocol for HistoryRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryReport {
    pub protocol: u32,
    pub sessions: Vec<HistoryEntry>,
}

impl Protocol for HistoryReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// One session record, as far as the files in its directory can tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub session: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// `interactive` or `print`.
    pub mode: String,
    /// The first line of what it opened with, cut short. `None` for a
    /// session that opened empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Unix seconds: when the session record was written.
    pub started: u64,
    /// Unix seconds: when the outcome was written; `None` while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<u64>,
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

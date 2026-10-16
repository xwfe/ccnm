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
    /// The workspace's `agent_tools` (P46), left out when it is
    /// [`AgentTools::omitted`](crate::config::AgentTools::omitted) so an
    /// Agent that predates the field still reads the request -- not when it
    /// is the config default, which since P77 an older Agent would read as
    /// fewer tools.
    #[serde(
        default = "crate::config::AgentTools::omitted",
        skip_serializing_if = "crate::config::AgentTools::is_omitted"
    )]
    pub agent_tools: crate::config::AgentTools,
    /// The ccnm session id the Runtime already chose and recorded (P58), so
    /// it can stop exactly this run before the run reports back. `None` for
    /// the human `ccnm run --print`, where the Agent picks one as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Protocol for RunRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.session.is_some() {
            crate::instance::ASSIGNED_SESSION_PROTOCOL
        } else if self.agent.is_some() {
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
    ///
    /// The session id stays (`ccnm result --session` takes it); which
    /// controller started it does not (P94) -- `ccnm doctor` shows the
    /// controller, and nobody reading an answer was looking for its pid.
    pub fn summary(&self) -> String {
        self.summary_in(Lang::En)
    }

    pub fn summary_in(&self, lang: Lang) -> String {
        let mut lines = vec![
            format!("{}{}", label(lang.pick("会话", "session")), self.session),
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
    /// The workspace's `agent_tools` (P46); see [`RunRequest::agent_tools`].
    #[serde(
        default = "crate::config::AgentTools::omitted",
        skip_serializing_if = "crate::config::AgentTools::is_omitted"
    )]
    pub agent_tools: crate::config::AgentTools,
    /// Carry on this workspace's latest conversation rather than open an
    /// empty one (`ccnm run --continue`, P92). The Agent picks which: only
    /// it has the session records and Claude's transcripts. Left out when
    /// false, so an Agent too old to know it refuses the field by name
    /// instead of quietly starting a new conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_last: bool,
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
    /// The earlier session whose conversation this one carries on, when it
    /// was started with `--continue` (P92).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
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

    /// What the person about to be put into the session needs: did it
    /// start, is it the conversation they asked for, and anything that
    /// will go wrong in front of them.
    ///
    /// Not the tmux server pid, the session id, the controller or where the
    /// session runs (P94). Those were printed on every start and read by
    /// nobody; `ccnm status` shows where it runs, `ccnm log` the id and
    /// `ccnm doctor` the controller. A Keychain the session cannot reach is
    /// the exception, said because it is the one that makes the next screen
    /// a login prompt.
    pub fn summary_in(&self, lang: Lang) -> String {
        let mut lines = vec![
            match (self.already_running, &self.continues) {
                (true, _) => lang.pick("会话本来就在跑", "session already running"),
                (false, Some(_)) => lang.pick(
                    "会话起好了，接着上次的对话",
                    "session started, carrying on the last conversation",
                ),
                (false, None) => lang.pick("会话起好了", "session started"),
            }
            .to_string(),
        ];
        if let Some(old) = &self.replaced {
            let old = old.display();
            lines.push(lang.pick(
                format!("替掉了在 {old} 里的旧会话：这个 workspace 已经不指那儿了"),
                format!(
                    "replaced a session working in {old}: this workspace does not point there any more"
                ),
            ));
        }
        if self.context.as_ref().and_then(|c| c.keychain) == Some(false) {
            let cli = self.provider.cli_name();
            lines.push(lang.pick(
                format!("注意：{cli} 在那边读不到 macOS 钥匙串，可能会让你重新登录"),
                format!(
                    "note: {cli} cannot reach the macOS Keychain there and may ask you to log in"
                ),
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
    /// `session` is an id the Runtime assigned to a print run it sent or is
    /// about to send (P58). Stop exactly that run; if the Agent has not
    /// created it yet, fence the id so the run refuses when it arrives. Never
    /// falls back to "whatever the workspace is running" -- that fallback
    /// looked only at tmux, missed every print run and stopped an unrelated
    /// interactive session instead (P57 B1/B2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assigned: bool,
}

impl Protocol for StopRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.assigned {
            crate::instance::ASSIGNED_SESSION_PROTOCOL
        } else if self.agent.is_some() {
            3
        } else {
            1
        }
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

/// `ccnm internal agent-output`: a slice of the retained view of one output
/// stream of a finished print session (P59).
///
/// Names the session by the id the Runtime assigned and the stream by name;
/// never a path. The Agent answers from a view it builds once -- redacted,
/// valid UTF-8, capped, frozen -- see [`crate::session::view`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRequest {
    pub protocol: u32,
    pub workspace: String,
    pub agent: InstanceRef,
    pub session: String,
    pub stream: crate::session::view::Stream,
    pub offset: u64,
    /// At most this many bytes back. The Agent may send fewer.
    pub limit: u64,
}

impl Protocol for OutputRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        crate::instance::OUTPUT_PROTOCOL
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputReport {
    pub protocol: u32,
    pub agent_identity: AgentIdentity,
    pub session: String,
    pub stream: crate::session::view::Stream,
    /// Which build of the view this slice comes from; see
    /// [`crate::session::view::Meta`].
    pub generation: String,
    pub view_bytes: u64,
    pub source_bytes: u64,
    pub source_truncated: bool,
    pub offset: u64,
    /// The slice, base64 (standard alphabet). Base64 rather than a string
    /// because a slice boundary may fall inside a character.
    pub data: String,
}

impl Protocol for OutputReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        crate::instance::OUTPUT_PROTOCOL
    }
}

/// `ccnm internal agent-guard` (P60): ask the Runtime, over this Agent's own
/// link to it, what the workspace's write guard looks like.
///
/// Relayed rather than asked directly because the Agent's ssh is the one
/// that lands on the Runtime Executor -- the account whose state directory
/// holds the guard. The Operator knows the Runtime Node by name only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentGuardRequest {
    pub protocol: u32,
    pub workspace: String,
    pub agent: InstanceRef,
    pub runtime_node: String,
}

impl Protocol for AgentGuardRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        crate::runtime::GUARD_PROTOCOL
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentGuardReport {
    pub protocol: u32,
    pub agent_identity: AgentIdentity,
    /// The Runtime's answer, as it gave it.
    pub runtime: crate::runtime::GuardReport,
    /// What this Agent's own records say about the session the marker
    /// names. The one fact the Runtime cannot see: a guard held for a
    /// session the Agent finished hours ago is an orphan server, not a
    /// busy workspace (see `overview`). Absent when the marker names
    /// nobody, or a session that was never an Agent session (an external
    /// client's `bridge-…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_on_agent: Option<OwnerOnAgent>,
}

impl Protocol for AgentGuardReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        crate::runtime::GUARD_PROTOCOL
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerOnAgent {
    /// This Agent has no record of it: another Agent's session, or one
    /// whose records were removed.
    NotHere,
    /// This Agent's record, and what state it is in.
    State(SessionState),
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

/// `ccnm internal agent-purge`, as an Operator older than P61 sends it.
/// Kept only so this build can decode it and refuse by name; the cleanup
/// it used to trigger is [`crate::cleanup`].
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
    /// Instances other than the one asked about that have a live session
    /// of the requested workspace (F6).
    ///
    /// A workspace has one terminal, and a status that names an instance
    /// lists only that instance's session. Left at that, asking about the
    /// default instance while another one holds the terminal reads "no live
    /// sessions" -- true of the instance, and the opposite of what the
    /// person wanted to know about the project. Empty when nothing was left
    /// out, and absent on the wire then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_instances: Vec<String>,
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
        let asked = self.agent_identity.as_ref().map(|id| id.instance.as_str());
        if self.sessions.is_empty() {
            // Worth reading twice before trusting: this counts tmux
            // sessions on the Agent Node, and a `--print` run is not one
            // of them even though it holds the workspace's write guard.
            // See docs/operations.md.
            match asked.filter(|_| !self.other_instances.is_empty()) {
                // Said about the instance, because said about the project
                // it would be false (F6).
                Some(instance) => out.push_str(&lang.pick(
                    format!("实例 {instance} 没有在跑的会话（--print 的运行不算在内）\n"),
                    format!("no live sessions for instance {instance}\n"),
                )),
                None => out.push_str(lang.pick(
                    "没有在跑的会话（--print 的运行不算在内）\n",
                    "no live sessions\n",
                )),
            }
        }
        for s in &self.sessions {
            out.push_str(&s.describe());
            out.push('\n');
        }
        for other in &self.other_instances {
            out.push_str(&lang.pick(
                format!("这个项目的终端现在是实例 {other} 的会话：加 --agent {other} 看它\n"),
                format!(
                    "this workspace's terminal is a session of instance {other}: add --agent {other} to see it\n"
                ),
            ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn start(continue_last: bool) -> StartRequest {
        StartRequest {
            protocol: crate::protocol::PROTOCOL,
            provider: Default::default(),
            agent: None,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/xshun"),
            runtime_node: "runtime".into(),
            provider_config_dir: None,
            permission_mode: PermissionMode::default(),
            prompt: None,
            agent_tools: Default::default(),
            continue_last,
        }
    }

    /// P92: a start that does not ask to continue reads exactly as it did
    /// before the field existed, so an older Agent still takes it; one that
    /// does is refused by that Agent by name (`deny_unknown_fields`) rather
    /// than quietly opening an empty conversation.
    #[test]
    fn continue_last_is_on_the_wire_only_when_asked() {
        let plain = serde_json::to_value(start(false)).unwrap();
        assert!(plain.get("continue_last").is_none(), "{plain}");
        let asked = serde_json::to_value(start(true)).unwrap();
        assert_eq!(asked["continue_last"], true);

        // The field set as v0.16.1 had it.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Before {
            protocol: u32,
            #[serde(default)]
            provider: Option<serde_json::Value>,
            #[serde(default)]
            agent: Option<serde_json::Value>,
            workspace: String,
            root: PathBuf,
            runtime_node: String,
            claude_config_dir: Option<PathBuf>,
            permission_mode: PermissionMode,
            #[serde(default)]
            prompt: Option<String>,
            #[serde(default)]
            agent_tools: Option<serde_json::Value>,
        }
        serde_json::from_value::<Before>(plain).expect("an older Agent reads it");
        let refused = serde_json::from_value::<Before>(asked).err().unwrap();
        assert!(refused.to_string().contains("continue_last"), "{refused}");
    }

    fn started(already_running: bool, continues: Option<&str>, keychain: bool) -> StartReport {
        StartReport {
            protocol: crate::protocol::PROTOCOL,
            provider: Default::default(),
            agent_identity: None,
            session: Some("d13df3ae-8716".into()),
            session_dir: None,
            tmux_session: "ccnm-keex".into(),
            server_pid: 83949,
            already_running,
            replaced: None,
            controller: None,
            context: Some(crate::session::Context {
                manager: Some("Background".into()),
                keychain: Some(keychain),
            }),
            continues: continues.map(str::to_string),
        }
    }

    /// P94: one line saying what happened, not the five lines of pids and
    /// launchd domains it used to be. The Keychain gets a line only when
    /// the session cannot reach it.
    #[test]
    fn a_start_says_what_happened_in_one_line() {
        let carried = started(false, Some("99a6a118"), true).summary_in(Lang::Zh);
        assert_eq!(carried, "会话起好了，接着上次的对话");
        assert_eq!(
            started(false, None, true).summary_in(Lang::Zh),
            "会话起好了"
        );
        assert_eq!(
            started(true, Some("99a6a118"), true).summary(),
            "session already running"
        );
        for noise in ["83949", "d13df3ae", "99a6a118", "Background", "ccnm-keex"] {
            assert!(!carried.contains(noise), "{noise} in {carried}");
        }

        let blocked = started(false, None, false).summary_in(Lang::Zh);
        assert_eq!(blocked.lines().count(), 2, "{blocked}");
        assert!(blocked.contains("钥匙串"), "{blocked}");
    }
}

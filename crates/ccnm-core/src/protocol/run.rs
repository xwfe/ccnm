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

    /// One line (P95): which session, how it ended, and the numbers worth
    /// reading. It was three labelled lines, the last an English run of
    /// token counts in both languages.
    pub fn summary_in(&self, lang: Lang) -> String {
        let how = how_it_went(
            self.provider,
            Some(&self.outcome),
            self.result.as_ref(),
            lang,
        );
        lang.pick(
            format!("会话 {}：{how}", self.session),
            format!("session {}: {how}", self.session),
        )
    }
}

/// `claude <how it ended>, <turns, cost>`, for [`RunReport`] and
/// [`ResultReport`] alike. The provider's name is never translated:
/// `claude` and `codex` are the binaries.
fn how_it_went(
    provider: crate::provider::AgentProvider,
    outcome: Option<&Outcome>,
    result: Option<&crate::provider::AgentResult>,
    lang: Lang,
) -> String {
    let cli = provider.cli_name();
    let mut parts = vec![match outcome {
        Some(outcome) => format!("{cli} {}", outcome.describe_in(lang)),
        None => lang.pick(format!("{cli} 还在跑"), format!("{cli} still running")),
    }];
    match result {
        Some(r) => {
            parts.push(r.brief(lang));
            if r.is_error() {
                parts.push(lang.pick(
                    format!("{cli} 自己报了错，见下面"),
                    format!("{cli} reported an error, below"),
                ));
            }
        }
        // Still running has no result yet, and saying so would be noise.
        None if outcome.is_some() => parts.push(
            lang.pick("没有拿到结果文档", "no result document on stdout")
                .to_string(),
        ),
        None => {}
    }
    parts.join(lang.pick("，", ", "))
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
    /// What the project's previous session left (P115), for the new one
    /// to start from. Left out when there is nothing, so an Agent too old
    /// to know it still starts sessions that carry nothing, and refuses by
    /// name one that would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<Carried>,
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
    /// What entering a session that was already running did about its
    /// tools (P100). Absent when they were up, or nobody could tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsOnEntry>,
}

/// A running session's tools were down when somebody came to enter it
/// (P100), and this is what was done about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsOnEntry {
    /// `/mcp reconnect ccnm` was typed and they came back.
    Reconnected,
    /// Nothing was typed: the input box held something unsent, or was not
    /// one ccnm recognises. The person types it once they are in.
    NotTyped,
    /// They could not be brought back, and the person is not put into a
    /// session that cannot reach the project. Why, as the error that
    /// stopped it.
    Failed(crate::error::ErrorReport),
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
        if let Some(said) = self.tools.as_ref().and_then(|t| tools_said(t, lang)) {
            lines.push(said);
        }
        lines.join("\n")
    }

    /// The session is running but its tools are down and stayed down
    /// (P100): what to tell the person instead of putting them into it, and
    /// the exit code. `None` when there is nothing to refuse.
    pub fn refusal(&self, workspace: &str, lang: Lang) -> Option<(String, i32)> {
        tools_refused(self.tools.as_ref()?, self.provider, workspace, lang)
    }
}

/// What entering a session did about its tools (P100), in a line; `None`
/// when there is nothing to say, or when [`tools_refused`] says it.
pub fn tools_said(tools: &ToolsOnEntry, lang: Lang) -> Option<String> {
    let reconnect = crate::provider::claude::tui::reconnect_command();
    match tools {
        ToolsOnEntry::Reconnected => Some(lang.pick(
            format!("工具断了，已替你接上（{reconnect}）"),
            format!("its tools were down; reconnected them ({reconnect})"),
        )),
        ToolsOnEntry::NotTyped => Some(lang.pick(
            format!(
                "工具断了，输入框里有没发出去的字（或开着别的界面），没替你敲；进去后敲：{reconnect}"
            ),
            format!(
                "its tools are down, and the input box holds something unsent (or another view is open), so nothing was typed; once in, type: {reconnect}"
            ),
        )),
        ToolsOnEntry::Failed(_) => None,
    }
}

/// Tools that stayed down: what to tell the person instead of putting them
/// into the session, and the exit code. The way in regardless is
/// `ccnm attach --force` (P116): plain `attach` checks too.
pub fn tools_refused(
    tools: &ToolsOnEntry,
    provider: crate::provider::AgentProvider,
    workspace: &str,
    lang: Lang,
) -> Option<(String, i32)> {
    let ToolsOnEntry::Failed(why) = tools else {
        return None;
    };
    let reason = match provider {
        // Nothing was tried: Codex has no way to reconnect a running
        // session, so the reason is the way back.
        crate::provider::AgentProvider::Codex => provider.way_back(workspace, lang),
        crate::provider::AgentProvider::Claude => why.message.clone(),
    };
    let text = lang.pick(
        format!(
            "这个会话的工具断了，ccnm 没能替你接上，所以没让你进去。\n原因：{reason}\n网络或项目机器好了再 ccnm {workspace}；不管工具先进去看：ccnm attach {workspace} --force"
        ),
        format!(
            "this session's tools are down and ccnm could not reconnect them, so you were not put into it.\nreason: {reason}\nonce the network or the project machine is back: ccnm {workspace}; to look at it anyway: ccnm attach {workspace} --force"
        ),
    );
    Some((text, why.code().exit_code()))
}

/// `ccnm internal agent-attach-check` (P116): a running session's tools,
/// looked at -- and brought back if they are down -- before a terminal is
/// handed to it. Its own request, ahead of the attach, so the answer comes
/// back to the machine the person typed on and is said in their language;
/// the attach itself hands over a terminal and says nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachCheck {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsOnEntry>,
}

impl Protocol for AttachCheck {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        1
    }
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

    /// The same one line as [`RunReport::summary_in`], with which kind of
    /// session it was: `ccnm result` without `--session` picks the latest,
    /// and that is not always the `--print` run someone had in mind.
    pub fn summary_in(&self, lang: Lang) -> String {
        let how = how_it_went(
            self.provider,
            self.outcome.as_ref(),
            self.result.as_ref(),
            lang,
        );
        let mode = match self.mode.as_str() {
            "print" => "--print",
            "interactive" => lang.pick("交互", "interactive"),
            other => other,
        };
        lang.pick(
            format!("会话 {}（{mode}）：{how}", self.session),
            format!("session {} ({mode}): {how}", self.session),
        )
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

/// What a new session is handed from the project's previous one (P115):
/// the handoff that session ended with, or -- when it ended without one,
/// as it does when its login ran out -- the tail of its conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carried {
    pub kind: CarriedKind,
    /// The AI machine it comes from, as the machine holding the project
    /// names it. Empty as the Agent reports it: it does not know that name.
    #[serde(default)]
    pub from: String,
    /// The ccnm session it comes from.
    pub session: String,
    /// Unix seconds: when it was last written.
    pub written: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarriedKind {
    /// The `[ccnm handoff]` the session's last reply ended with (P102).
    Handoff,
    /// What the person and the AI said, without tool calls or output.
    Conversation,
}

/// `ccnm internal agent-handoff` (P115): what this AI machine's most recent
/// session of a project left for the next one. Asked by the machine holding
/// the project before it starts a session, on this machine or another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffRequest {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    pub workspace: String,
    pub root: PathBuf,
    /// A legacy workspace's `claude_config_dir`: where that Claude keeps its
    /// transcripts.
    #[serde(
        rename = "claude_config_dir",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_config_dir: Option<PathBuf>,
}

impl Protocol for HandoffRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReport {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<Carried>,
}

impl Protocol for HandoffReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        1
    }
}

/// `ccnm internal agent-usage` (P105): the subscription limits of the
/// login an Agent runs under. Names the login the way a session does --
/// an instance, or a legacy Claude's config directory -- and no workspace:
/// a limit belongs to the login, whichever project used it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRequest {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    /// A legacy workspace's `claude_config_dir`, from the Runtime's config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_config_dir: Option<PathBuf>,
}

impl Protocol for UsageRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent.is_some() { 3 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    pub quota: crate::quota::Quota,
}

impl Protocol for UsageReport {
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
    /// How often the session's current conversation has been compacted,
    /// when it has been (P102): the person sees how long the window has
    /// been going on summaries, not only the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions: Option<u32>,
    /// What the Controller's watch did about its tools (P116), when it did
    /// anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnects: Option<Reconnects>,
}

/// What the Controller's watch did about one session's tools (P116).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconnects {
    /// Times they were brought back without anybody asking.
    #[serde(default)]
    pub count: u32,
    /// Unix seconds of the last of those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<u64>,
    /// While they are down and still being tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<Failing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failing {
    /// Unix seconds: the first try that failed.
    pub since: u64,
    /// Why the last try failed, in one line.
    pub reason: String,
    /// How many tries have failed in a row.
    pub tries: u32,
    /// Unix seconds: when the next try is due.
    pub next: u64,
}

impl LiveSession {
    /// [`AgentProvider::tools_down_hint`](crate::provider::AgentProvider::tools_down_hint)
    /// for this session, naming its own workspace. A session from a build
    /// that did not record one gets a placeholder rather than a wrong name.
    pub fn tools_down_hint(&self, lang: Lang) -> String {
        let workspace = self
            .workspace
            .as_deref()
            .unwrap_or(lang.pick("<项目>", "<workspace>"));
        self.provider.tools_down_hint(workspace, lang)
    }

    /// The session in one diagnostic line, for doctor's `Terminal session`
    /// row: tmux name, launchd domain and all. Not what `ccnm status`
    /// prints any more (P95); that is [`crate::overview::session_rows`].
    pub fn describe(&self) -> String {
        let attached = match self.attached {
            0 => "detached".to_string(),
            n => format!("{n} attached"),
        };
        // Just the state: doctor says the way back on its own line (P96),
        // and it used to be said twice, once here in brackets.
        let tools = match self.tools {
            Some(true) => "tools connected",
            Some(false) => "TOOLS DOWN",
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
        self.render_at(lang, crate::overview::now_secs())
    }

    /// `ccnm status <ws>`, in the same block `ccnm status` prints for each
    /// project (P95). `now` is a parameter so a test can pin how long a
    /// session has been running.
    ///
    /// It used to open with the Agent's tmux version and give each session
    /// one English line of tmux session name, launchd domain and Keychain
    /// state. A tmux that cannot be asked is still said, first: then "no
    /// live sessions" would be a claim nobody checked, and is left out.
    pub fn render_at(&self, lang: Lang, now: u64) -> String {
        let mut out = match &self.tmux {
            Ok(_) => String::new(),
            Err(e) => lang.pick(
                format!("问不到 Agent Node 上的 tmux：{}\n", e.message),
                format!("cannot ask tmux on the Agent Node: {}\n", e.message),
            ),
        };
        let asked = self.agent_identity.as_ref().map(|id| id.instance.as_str());
        if self.sessions.is_empty() && self.tmux.is_ok() {
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
        // Grouped by workspace, in the order the Agent listed them: one
        // when a workspace was named, every one with `--all`.
        let mut names: Vec<&str> = Vec::new();
        for s in &self.sessions {
            let name = s.workspace.as_deref().unwrap_or("-");
            if !names.contains(&name) {
                names.push(name);
            }
        }
        for name in names {
            let rows: Vec<Vec<String>> = self
                .sessions
                .iter()
                .filter(|s| s.workspace.as_deref().unwrap_or("-") == name)
                .flat_map(|s| crate::overview::session_rows(s, now, lang))
                .collect();
            out.push_str(&format!("{name}\n{}", crate::overview::block(&rows)));
        }
        for other in &self.other_instances {
            out.push_str(&lang.pick(
                format!("这个项目的终端现在是实例 {other} 的会话：加 --on {other} 看它\n"),
                format!(
                    "this workspace's terminal is a session of instance {other}: add --on {other} to see it\n"
                ),
            ));
        }
        // `--session`: the full id, since that is what was typed, and the
        // state in words rather than its Rust name.
        for record in &self.records {
            let state = crate::overview::state_word(record.state, lang);
            let instance = record.agent_identity.as_ref().map_or(String::new(), |id| {
                lang.pick(
                    format!("（实例 {}）", id.instance),
                    format!(" (instance {})", id.instance),
                )
            });
            out.push_str(&lang.pick(
                format!("会话 {}：{state}{instance}\n", record.session),
                format!("session {}: {state}{instance}\n", record.session),
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
            carried: None,
        }
    }

    /// P115: a start that carries nothing reads exactly as before, so an
    /// AI machine older than P115 still takes it; one that carries a
    /// handoff is refused by that machine by name, which the caller turns
    /// into a start without it.
    #[test]
    fn carried_is_on_the_wire_only_when_there_is_something() {
        let plain = serde_json::to_value(start(false)).unwrap();
        assert!(plain.get("carried").is_none(), "{plain}");
        let with = StartRequest {
            carried: Some(Carried {
                kind: CarriedKind::Conversation,
                from: "agent".into(),
                session: "s".into(),
                written: 1,
                text: "[user]\nhi".into(),
            }),
            ..start(false)
        };
        let with = serde_json::to_value(with).unwrap();
        assert_eq!(with["carried"]["kind"], "conversation");

        // The field set as v0.21.1 had it.
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
            #[serde(default)]
            continue_last: bool,
        }
        serde_json::from_value::<Before>(plain).expect("an older Agent reads it");
        let refused = serde_json::from_value::<Before>(with).err().unwrap();
        let said = refused.to_string();
        assert!(
            said.contains("unknown field") && said.contains("carried"),
            "{said}"
        );
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
            tools: None,
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

    /// P95: what a `--print` run and `ccnm result` open with is one line in
    /// the person's language -- how it ended, turns, cost -- not three
    /// labelled lines ending in an English run of token counts.
    #[test]
    fn a_print_summary_is_one_line_in_the_persons_language() {
        let run: RunReport = serde_json::from_str(
            r#"{"protocol":1,"session":"7c1d9f60-0a11-4c22-9d33-8e44f5566a78","session_dir":"/s/7c1d","controller":{"hello":{"protocol":1,"ccnm_version":"x","user":"u","platform":"macos/aarch64","exe":null,"root":null},"pid":1,"manager":{"Ok":"Aqua"}},"pid":2,"outcome":{"exit_code":0,"timed_out":false,"duration_ms":8200,"error":null},"result":{"is_error":false,"result":"done","num_turns":3,"duration_ms":8100,"duration_api_ms":6900,"total_cost_usd":0.0412,"usage":{"input_tokens":12,"output_tokens":340,"cache_creation_input_tokens":5100,"cache_read_input_tokens":20400}},"stdout_tail":"","stderr_tail":""}"#,
        )
        .unwrap();
        assert_eq!(
            run.summary_in(Lang::Zh),
            "会话 7c1d9f60-0a11-4c22-9d33-8e44f5566a78：claude 跑完了，8.2 秒，3 轮，$0.04"
        );
        assert_eq!(
            run.summary(),
            "session 7c1d9f60-0a11-4c22-9d33-8e44f5566a78: claude exited 0 in 8.2 s, 3 turns, $0.04"
        );

        let still: ResultReport = serde_json::from_str(
            r#"{"protocol":1,"session":"99a6a118","session_dir":"/s/99a6","mode":"print","started":1,"outcome":null,"result":null,"stdout_tail":"","stderr_tail":""}"#,
        )
        .unwrap();
        assert_eq!(
            still.summary_in(Lang::Zh),
            "会话 99a6a118（--print）：claude 还在跑"
        );

        let refused: ResultReport = serde_json::from_str(
            r#"{"protocol":1,"session":"99a6a118","session_dir":"/s/99a6","mode":"interactive","started":1,"outcome":{"exit_code":1,"timed_out":false,"duration_ms":3000},"result":{"is_error":true,"result":"no","num_turns":1,"permission_denials":[{"tool_name":"Bash"},{"tool_name":"Edit"}]},"stdout_tail":"","stderr_tail":""}"#,
        )
        .unwrap();
        let said = refused.summary_in(Lang::Zh);
        assert_eq!(
            said,
            "会话 99a6a118（交互）：claude 退出码 1，跑了 3.0 秒，1 轮，2 次权限被拒，claude 自己报了错，见下面"
        );
        assert!(!refused.summary().contains("tokens"));
    }

    /// P95: `ccnm status <ws>` is the block `ccnm status` prints for the
    /// project, not a tmux version line and an English row of tmux session
    /// name and launchd domain; `--session` says the state in words.
    #[test]
    fn a_workspace_status_reads_like_the_overview() {
        let id = "d13df3ae-8716-4874-bee3-9399c1f34561";
        let report = StatusReport {
            protocol: crate::protocol::PROTOCOL,
            agent_identity: None,
            tmux: Ok("3.7c".into()),
            sessions: vec![LiveSession {
                provider: Default::default(),
                agent_identity: None,
                tmux_session: "ccnm-keex".into(),
                workspace: Some("keex".into()),
                session: Some(id.into()),
                created: 1_000,
                attached: 0,
                context: Some(crate::session::Context {
                    manager: Some("Aqua".into()),
                    keychain: Some(true),
                }),
                tools: Some(true),
                compactions: None,
                reconnects: None,
            }],
            records: vec![SessionRecord {
                session: id.into(),
                workspace: "keex".into(),
                provider: Default::default(),
                agent_identity: None,
                state: SessionState::Running,
                provider_session_id: None,
                outcome: None,
            }],
            other_instances: vec![],
        };
        assert_eq!(
            report.render_at(Lang::Zh, 1_000 + 3_700),
            format!(
                "keex\n  会话     d13df3ae 运行中，已运行 1 小时 1 分，没有终端接着\n  工具              通\n会话 {id}：运行中\n"
            )
        );
        let en = report.render_at(Lang::En, 1_000 + 3_700);
        for noise in ["ccnm-keex", "Aqua", "keychain", "tmux", "Running", "legacy"] {
            assert!(!en.contains(noise), "{noise} in:\n{en}");
        }
        assert!(en.contains("running for 1h 1m, detached"), "{en}");

        // No tmux, no claim that nothing is running.
        let unasked = StatusReport {
            tmux: Err(crate::error::ErrorReport::new(
                crate::ErrorCode::Dependency,
                "tmux is not installed",
            )),
            sessions: vec![],
            records: vec![],
            ..report
        };
        let said = unasked.render_at(Lang::Zh, 0);
        assert_eq!(said, "问不到 Agent Node 上的 tmux：tmux is not installed\n");
    }
}

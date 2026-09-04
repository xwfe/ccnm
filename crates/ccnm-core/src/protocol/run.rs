//! Starting, attaching to, listing and ending Claude sessions on the work
//! machine: `work-run` (print mode, waits and brings the result home) and
//! `work-start` / `attach` / `status` / `stop` (interactive, which return
//! immediately because the session outlives the call).
//!
//! Like the probe, the work machine has no config file: everything it
//! needs is in the request, and everything it learned is in the report.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::payload::Protocol;
use crate::claude::PrintResult;
use crate::config::PermissionMode;
use crate::controller::Context;
use crate::session::Outcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    pub protocol: u32,
    pub workspace: String,
    /// Project root on the home machine; passed through to the MCP payload.
    pub root: PathBuf,
    pub home_alias: String,
    pub home_ccnm_bin: String,
    pub claude_config_dir: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    /// The one prompt of a print-mode session.
    pub prompt: String,
    /// Claude is killed after this many seconds.
    pub timeout_secs: u64,
}

impl Protocol for RunRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub protocol: u32,
    /// The session id, which is also the name of its directory on both
    /// machines and the id Claude was told to use.
    pub session: String,
    pub session_dir: PathBuf,
    /// The controller that started it, for the record of which session it
    /// was in.
    pub controller: Context,
    /// The supervisor's pid.
    pub pid: u32,
    pub outcome: Outcome,
    /// Claude's `--output-format json` document, when stdout held one.
    pub result: Option<PrintResult>,
    /// The end of stdout when it was not a result document, and the end of
    /// stderr always: enough to see why, never the whole thing.
    pub stdout_tail: String,
    pub stderr_tail: String,
}

impl Protocol for RunReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

impl RunReport {
    /// What worked and what it cost, in the order someone reading a
    /// terminal wants them.
    pub fn summary(&self) -> String {
        let mut lines = vec![
            format!("session   {}", self.session),
            format!("started   by {}", self.controller.describe()),
            format!("claude    {}", self.outcome.describe()),
        ];
        match &self.result {
            Some(r) => lines.push(format!("run       {}", r.summary())),
            None => lines.push("run       no result document on stdout".to_string()),
        }
        lines.join("\n")
    }
}

/// `ccnm internal work-start`: bring up an interactive session, or say that
/// one is already up. Carries no timeout — an interactive session ends when
/// the person using it ends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub protocol: u32,
    pub workspace: String,
    pub root: PathBuf,
    pub home_alias: String,
    pub home_ccnm_bin: String,
    pub claude_config_dir: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    /// What Claude opens with; `None` opens an empty prompt.
    #[serde(default)]
    pub prompt: Option<String>,
}

impl Protocol for StartRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartReport {
    pub protocol: u32,
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
}

impl StartReport {
    pub fn summary(&self) -> String {
        let what = if self.already_running {
            "already running"
        } else {
            "started"
        };
        let mut lines = vec![format!(
            "session   {} ({what}, tmux server pid {})",
            self.tmux_session, self.server_pid
        )];
        if let Some(id) = &self.session {
            lines.push(format!("id        {id}"));
        }
        if let Some(ctx) = &self.controller {
            lines.push(format!("started   by {}", ctx.describe()));
        }
        if let Some(context) = &self.context {
            lines.push(format!("claude in {}", context.describe()));
        }
        lines.join("\n")
    }
}

/// `ccnm internal attach`: hand this terminal to the workspace's session.
/// The only internal command that answers with a terminal instead of a
/// JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRequest {
    pub protocol: u32,
    pub workspace: String,
}

impl Protocol for AttachRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// `ccnm internal work-stop`: end the workspace's session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRequest {
    pub protocol: u32,
    pub workspace: String,
}

impl Protocol for StopRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    pub protocol: u32,
    pub tmux_session: String,
    /// False when there was nothing to stop, which is not an error.
    pub killed: bool,
}

impl Protocol for StopReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// `ccnm internal work-status`: every live session on the work machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusRequest {
    pub protocol: u32,
    /// Only this workspace's session; `None` for all of ccnm's.
    #[serde(default)]
    pub workspace: Option<String>,
}

impl Protocol for StatusRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub protocol: u32,
    /// `tmux -V`, or why it could not be asked.
    pub tmux: crate::error::Reported<String>,
    pub sessions: Vec<LiveSession>,
}

impl Protocol for StatusReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// One live interactive session, as the work machine sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSession {
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
            Some(false) => "TOOLS DOWN (in Claude: /mcp -> ccnm -> Reconnect)",
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
        let mut out = match &self.tmux {
            Ok(v) => format!("tmux {v} on the work machine\n"),
            Err(e) => format!("tmux: {}\n", e.message),
        };
        if self.sessions.is_empty() {
            out.push_str("no live sessions\n");
            return out;
        }
        for s in &self.sessions {
            out.push_str(&s.describe());
            out.push('\n');
        }
        out
    }
}

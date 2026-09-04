//! `ccnm internal work-run`: start a Claude session on the work machine
//! and, in print mode, wait for it and bring the result home.
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

//! `ccnm internal probe`: everything doctor wants to know about the Agent
//! Node and, through it, the Runtime Node, in one round trip.
//!
//! The request names the Runtime Node; the Agent Node resolves that name
//! in its own config, because an ssh alias only means something on the
//! machine that dials it. Everything it learned goes back in the report,
//! errors included, so doctor can render one row per fact -- including
//! which alias it ended up using, which is the fact doctor wants and the
//! runtime side cannot know.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::hello::HelloReport;
use super::payload::Protocol;
use crate::claude::ClaudeReport;
use crate::error::Reported;
use crate::ssh::ResolvedSsh;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub protocol: u32,
    pub workspace: String,
    /// Project root on the runtime host; the Agent side only passes it on.
    pub root: PathBuf,
    /// The node holding the project, by name. The Agent Node resolves it
    /// against its own config.
    pub runtime_node: String,
    pub claude_config_dir: Option<PathBuf>,
    /// How many `workspace_info` calls the MCP handshake should make over
    /// the reverse ssh; 0 skips the handshake.
    #[serde(default)]
    pub mcp_calls: u32,
}

impl Protocol for ProbeRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub protocol: u32,
    /// The Agent Node's own hello.
    pub hello: HelloReport,
    /// The login-session controller, as reached from this ssh session.
    /// `None` only from a ccnm build that predates it.
    #[serde(default)]
    pub controller: Option<Reported<crate::controller::Context>>,
    /// Claude Code on the Agent Node.
    ///
    /// Asked **through the controller** whenever one is running, because
    /// an ssh session gets the wrong answer about the login: it cannot
    /// read the Keychain, so a logged-in machine reports "Not logged in"
    /// (see [`crate::controller`]). With no controller, `version` still
    /// comes from this session — it needs no credential — and `auth` is a
    /// `CCNM_E_NOT_READY` error rather than a guess.
    pub claude: ClaudeReport,
    /// What the Agent Node's own alias for the Runtime Node resolves to,
    /// via `ssh -G`. `None` when agent and project are the same machine,
    /// which dials nothing.
    #[serde(default)]
    pub runtime_ssh: Option<Reported<ResolvedSsh>>,
    /// The Runtime Node's hello, fetched over the reverse ssh. `None` when
    /// agent and project are the same machine.
    #[serde(default)]
    pub runtime_hello: Option<Reported<HelloReport>>,
    /// One MCP session over the reverse ssh (`None` when not requested or
    /// when the hello already failed).
    #[serde(default)]
    pub mcp: Option<Reported<super::mcp::ProbeReport>>,
    /// tmux and this workspace's terminal session, for the "Terminal
    /// session" row. `None` from a ccnm build that predates it.
    #[serde(default)]
    pub terminal: Option<super::run::StatusReport>,
}

impl Protocol for ProbeReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

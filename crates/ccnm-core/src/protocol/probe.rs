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
use crate::error::Reported;
use crate::provider::AgentReport;
use crate::ssh::ResolvedSsh;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::instance::InstanceRef>,
    pub workspace: String,
    /// Project root on the runtime host; the Agent side only passes it on.
    pub root: PathBuf,
    /// The node holding the project, by name. The Agent Node resolves it
    /// against its own config.
    pub runtime_node: String,
    #[serde(rename = "claude_config_dir")]
    pub provider_config_dir: Option<PathBuf>,
    /// How many `workspace_info` calls the MCP handshake should make over
    /// the reverse ssh; 0 skips the handshake.
    #[serde(default)]
    pub mcp_calls: u32,
    /// The workspace runs Codex through exec-server (P23), so the probe
    /// also opens one empty `exec-serve` session when the Agent is Codex
    /// (P27). Sent only when true: this struct refuses unknown fields, and
    /// every other request stays readable by a build that predates it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub codex_exec_server: bool,
}

impl Protocol for ProbeRequest {
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
pub struct ProbeReport {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<crate::instance::AgentIdentity>,
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
    #[serde(rename = "claude")]
    pub agent: AgentReport,
    /// What the Agent Node's own alias for the Runtime Node resolves to,
    /// via `ssh -G`. `None` when agent and project are the same machine,
    /// which dials nothing.
    #[serde(default)]
    pub runtime_ssh: Option<Reported<ResolvedSsh>>,
    /// The Runtime Node's hello, fetched over the reverse ssh. `None` when
    /// agent and project are the same machine.
    #[serde(default)]
    pub runtime_hello: Option<Reported<HelloReport>>,
    /// What the Runtime Executor says about itself and the project, asked
    /// over the same reverse ssh so the answer describes the account that
    /// would really run the tools rather than whoever ran doctor.
    ///
    /// `None` when there is no reverse link (colocated) or the plain hello
    /// already failed; the row then says "not checked" rather than
    /// borrowing this machine's own audit and calling it the Runtime's.
    #[serde(default)]
    pub runtime_audit: Option<Reported<crate::runtime::AuditReport>>,
    /// One MCP session over the reverse ssh (`None` when not requested or
    /// when the hello already failed).
    #[serde(default)]
    pub mcp: Option<Reported<super::mcp::ProbeReport>>,
    /// The exec-server chain's own preflight, the one `ccnm run` makes
    /// before starting Codex (P27). `None` when it was not run: the request
    /// did not ask, the Agent is not Codex, the hello failed, or the build
    /// that answered predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_server: Option<Reported<()>>,
    /// tmux and this workspace's terminal session, for the "Terminal
    /// session" row. `None` from a ccnm build that predates it.
    #[serde(default)]
    pub terminal: Option<super::run::StatusReport>,
}

impl Protocol for ProbeReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() { 3 } else { 1 }
    }
}

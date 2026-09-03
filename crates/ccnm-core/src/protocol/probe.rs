//! `ccnm internal probe`: everything doctor wants to know about the work
//! machine and, through it, the home runtime, in one round trip.
//!
//! The work machine has no config file. Everything it needs arrives in
//! the request; everything it learned goes back in the report, errors
//! included, so doctor can render one row per fact.

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
    /// Project root on the runtime host; the work side only passes it on.
    pub root: PathBuf,
    /// Alias in the work machine's `~/.ssh/config` for the home runtime.
    pub home_alias: String,
    /// ccnm path to invoke on the home runtime (design doc section 7).
    pub home_ccnm_bin: String,
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
    /// The work machine's own hello.
    pub hello: HelloReport,
    pub claude: ClaudeReport,
    /// What `ssh -G <home_alias>` resolves to on the work machine.
    pub home_ssh: Reported<ResolvedSsh>,
    /// The home runtime's hello, fetched over the reverse ssh.
    pub home_hello: Reported<HelloReport>,
    /// One MCP session over the reverse ssh (`None` when not requested or
    /// when the hello already failed).
    #[serde(default)]
    pub mcp: Option<Reported<super::mcp::ProbeReport>>,
}

impl Protocol for ProbeReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

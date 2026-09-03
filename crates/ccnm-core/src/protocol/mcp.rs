//! The one control-protocol message that starts an MCP session, and the
//! report a probe of that session produces.
//!
//! [`ServePayload`] rides on the argv of `ccnm internal mcp-serve` exactly
//! once, when Claude Code (or a probe) spawns the ssh transport. From then
//! on stdin/stdout belong to MCP JSON-RPC; nothing here wraps that
//! (design doc section 9).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::payload::{PROTOCOL, Protocol};

/// What `ccnm internal mcp-serve --payload` needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServePayload {
    pub protocol: u32,
    pub workspace: String,
    /// Project root on this (runtime) host. Canonicalized at startup; every
    /// tool path is relative to it (design doc section 17).
    pub root: PathBuf,
    /// Session id chosen by the launcher; names the retained-output
    /// directory later.
    pub session: String,
    /// Tool policy. Only `coding` exists.
    pub policy: String,
}

impl ServePayload {
    pub fn new(workspace: &str, root: PathBuf, session: &str) -> Self {
        ServePayload {
            protocol: PROTOCOL,
            workspace: workspace.to_string(),
            root,
            session: session.to_string(),
            policy: "coding".to_string(),
        }
    }
}

impl Protocol for ServePayload {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// What one probe of a live MCP server observed. Every number is measured
/// by the client side of the transport, so over ssh it includes the
/// network (design doc section 27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// Spawning the transport and finishing `initialize`, in microseconds.
    pub connect_us: u64,
    pub server_name: String,
    pub server_version: String,
    /// Bytes of `initialize.result.instructions` the server sent.
    pub instructions_bytes: usize,
    pub tools: Vec<String>,
    /// `tools/list` result serialized as JSON, in bytes: the schema budget
    /// of design doc section 27.
    pub tools_list_bytes: usize,
    /// How many `workspace_info` calls were made after `initialize`.
    pub calls: u32,
    pub call_p50_us: u64,
    pub call_p95_us: u64,
    pub call_max_us: u64,
    /// Process id the server reported in its first `workspace_info`.
    pub server_pid: u32,
    /// Every call came back from the same pid and the server's own call
    /// counter went 1..=calls: one process served the whole session, so
    /// there was one ssh, not one per call.
    pub single_process: bool,
}

impl ProbeReport {
    pub fn summary(&self) -> String {
        format!(
            "initialize in {} ms, tools/list ({} tool{}, {} B), workspace_info x{} p50 {} ms p95 {} ms max {} ms, pid {} throughout",
            self.connect_us / 1000,
            self.tools.len(),
            if self.tools.len() == 1 { "" } else { "s" },
            self.tools_list_bytes,
            self.calls,
            self.call_p50_us / 1000,
            self.call_p95_us / 1000,
            self.call_max_us / 1000,
            self.server_pid
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_payload_defaults_to_coding_policy_and_roundtrips() {
        let p = ServePayload::new("xshun", PathBuf::from("/Users/me/p"), "s-1");
        assert_eq!(p.policy, "coding");
        let wire = crate::protocol::payload::encode(&p).unwrap();
        assert!(crate::ssh::is_remote_safe(&wire));
        let back: ServePayload = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn summary_reads_like_a_doctor_detail() {
        let rep = ProbeReport {
            connect_us: 412_345,
            server_name: "ccnm".into(),
            server_version: "0.1.0".into(),
            instructions_bytes: 120,
            tools: vec!["workspace_info".into()],
            tools_list_bytes: 380,
            calls: 100,
            call_p50_us: 21_000,
            call_p95_us: 25_500,
            call_max_us: 40_100,
            server_pid: 4242,
            single_process: true,
        };
        assert_eq!(
            rep.summary(),
            "initialize in 412 ms, tools/list (1 tool, 380 B), workspace_info x100 p50 21 ms p95 25 ms max 40 ms, pid 4242 throughout"
        );
    }
}

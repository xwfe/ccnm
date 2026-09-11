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

/// Who opened this server, and therefore what it may do.
///
/// Not a wire field. Which entry a session came in through is decided by
/// the payload's own protocol number (4 = managed, 5 = external), so
/// putting it on the wire as well would let the two disagree — and a peer
/// that could send `Managed` alongside an external open would be asking for
/// the write guard and the seven tools by claiming to be something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Entry {
    /// A ccnm-managed Agent session. The only entry there was before P10.
    #[default]
    Managed,
    /// An external MCP client, in the mode this Runtime granted it.
    External(crate::runtime::ExternalMode),
}

impl Entry {
    /// Whether this session may change anything — the one question the
    /// write guard, the tool list and every write tool ask.
    pub fn writes(self) -> bool {
        match self {
            Entry::Managed => true,
            Entry::External(mode) => mode.writes(),
        }
    }

    pub fn is_external(self) -> bool {
        matches!(self, Entry::External(_))
    }
}

/// What `ccnm internal mcp-serve --payload` needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServePayload {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<crate::instance::WorkspaceBinding>,
    pub workspace: String,
    /// Project root on this (runtime) host. Canonicalized at startup; every
    /// tool path is relative to it (design doc section 17).
    pub root: PathBuf,
    /// Session id chosen by the launcher; names the retained-output
    /// directory later.
    pub session: String,
    /// Tool policy. Only `coding` exists.
    pub policy: String,
    /// Whether a person is at a terminal for this session.
    ///
    /// It decides one thing: whether `exec_command` is marked as needing
    /// the user's say-so on every call. That mark is worth having exactly
    /// where somebody can answer it. `ccnm run --print` deliberately runs
    /// with no prompting at all, so the same mark there does not make the
    /// tool safer, it makes it impossible -- Claude denies the call and
    /// reports that it could not ask.
    ///
    /// Defaults to false: a payload from anything that does not set it
    /// (a probe, a test) is not a person waiting at a keyboard.
    #[serde(default)]
    pub interactive: bool,
    /// In-process only; see [`Entry`]. `skip` rather than `default`: it
    /// must not appear on the wire in either direction.
    #[serde(skip)]
    pub entry: Entry,
}

impl ServePayload {
    pub fn new(workspace: &str, root: PathBuf, session: &str) -> Self {
        ServePayload {
            provider: Default::default(),
            binding: None,
            protocol: PROTOCOL,
            workspace: workspace.to_string(),
            root,
            session: session.to_string(),
            policy: "coding".to_string(),
            interactive: false,
            entry: Entry::Managed,
        }
    }

    /// Say this server is being opened for an external MCP client.
    pub fn with_entry(mut self, entry: Entry) -> Self {
        self.entry = entry;
        self
    }

    pub fn with_provider(mut self, provider: crate::provider::AgentProvider) -> Self {
        self.provider = provider;
        self.protocol = provider.control_protocol();
        self
    }

    pub fn with_binding(mut self, binding: crate::instance::WorkspaceBinding) -> Self {
        self.provider = binding.agent.provider;
        self.protocol = crate::instance::INSTANCE_SESSION_PROTOCOL;
        self.binding = Some(binding);
        self
    }

    /// Say a person is at a terminal for this session.
    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

impl Protocol for ServePayload {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.binding.is_some() {
            3
        } else {
            self.provider.control_protocol()
        }
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
    /// What the instructions said about the project's CLAUDE.md — the
    /// `[project instructions: ...]` line, without its brackets (design
    /// doc section 20). `None` from a build that sends no such line.
    ///
    /// It is here so the projection is proved *through the transport*:
    /// doctor can read the file itself, but only this says the bytes
    /// reached a client on the other side of the ssh.
    #[serde(default)]
    pub project_instructions: Option<String>,
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
        let project = match &self.project_instructions {
            Some(what) => format!(" ({what})"),
            None => String::new(),
        };
        format!(
            "initialize in {} ms, tools/list ({} tool{}, {} B), instructions {} B{project}, workspace_info x{} p50 {} ms p95 {} ms max {} ms, pid {} throughout",
            self.connect_us / 1000,
            self.tools.len(),
            if self.tools.len() == 1 { "" } else { "s" },
            self.tools_list_bytes,
            self.instructions_bytes,
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
            project_instructions: Some("CLAUDE.md, 2731 bytes".into()),
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
            "initialize in 412 ms, tools/list (1 tool, 380 B), instructions 120 B (CLAUDE.md, 2731 bytes), workspace_info x100 p50 21 ms p95 25 ms max 40 ms, pid 4242 throughout"
        );
        let old = ProbeReport {
            project_instructions: None,
            ..rep
        };
        assert!(old.summary().contains("instructions 120 B, workspace_info"));
    }
}

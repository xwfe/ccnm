//! Agent observations consumed by orchestration. Serialized field names remain
//! the v1 contract; only a measured second provider may justify changing it.
use crate::error::Reported;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Login metadata reported by the official Agent CLI, never credentials.
/// camelCase is retained for the existing v1 report contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub logged_in: bool,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
}

impl AuthStatus {
    /// `email via claude.ai (max)` style summary.
    pub fn describe(&self) -> String {
        let who = self.email.as_deref().unwrap_or("logged in");
        let mut text = who.to_string();
        if let Some(method) = &self.auth_method {
            text.push_str(&format!(" via {method}"));
        }
        if let Some(sub) = &self.subscription_type {
            text.push_str(&format!(" ({sub})"));
        }
        text
    }
}

/// What ccnm knows about the Agent CLI on one machine. Both halves are
/// reported separately: installed but logged out is a different problem
/// from not installed,
/// and doctor renders them as separate rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub path: Option<PathBuf>,
    pub version: Reported<String>,
    pub auth: Reported<AuthStatus>,
}

/// How much of [`super::AgentProvider::report`] to ask for.
///
/// The login half only means something from a login session. Everywhere
/// else this is [`Ask::VersionOnly`] — not because running the command
/// would fail, but because its answer would be wrong, and a command whose
/// result has to be discarded should not be run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ask {
    /// Version and login. Only from a login session (see
    /// [`crate::controller`]).
    #[default]
    Everything,
    /// Version only; the login is reported as `CCNM_E_NOT_READY`.
    VersionOnly,
}

/// Token counts reported by the official CLI, not an estimate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// The fields of the print-mode result document ccnm cares about. Every
/// one is `default`ed: a newer Claude that drops or renames a field
/// degrades a number to zero, not the whole run to a parse error.
///
/// Shape captured from 2.1.260 on 2026-09-04
/// (`tests/fixtures/claude-print-2.1.260.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub subtype: Option<String>,
    /// The final assistant text, or the error text when `is_error`.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub num_turns: u32,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub duration_api_ms: u64,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub usage: Usage,
    /// Tool calls the Agent requested and was refused.
    #[serde(default)]
    pub permission_denials: Vec<serde_json::Value>,
}

impl RunResult {
    pub fn summary(&self) -> String {
        format!(
            "{} turn{} in {:.1} s (api {:.1} s); tokens in {} out {} cache-write {} cache-read {}; ${:.2}; {} permission denial{}",
            self.num_turns,
            if self.num_turns == 1 { "" } else { "s" },
            self.duration_ms as f64 / 1000.0,
            self.duration_api_ms as f64 / 1000.0,
            self.usage.input_tokens,
            self.usage.output_tokens,
            self.usage.cache_creation_input_tokens,
            self.usage.cache_read_input_tokens,
            self.total_cost_usd,
            self.permission_denials.len(),
            if self.permission_denials.len() == 1 {
                ""
            } else {
                "s"
            },
        )
    }
}

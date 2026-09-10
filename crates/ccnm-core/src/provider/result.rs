//! Preserve the old Claude result wire document while tagging Codex observations.
use super::{RunResult, codex::result::CodexResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentResult {
    Claude(RunResult),
    Codex(CodexResult),
}
impl AgentResult {
    pub fn provider_session_id(&self) -> Option<&str> {
        match self {
            Self::Claude(r) => r.session_id.as_deref(),
            Self::Codex(r) => Some(&r.session_id),
        }
    }
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Claude(r) => r.result.as_deref(),
            Self::Codex(r) => r.result.as_deref(),
        }
    }
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Claude(r) => r.result,
            Self::Codex(r) => r.result,
        }
    }
    /// Input and output token counts, whichever provider reported them.
    ///
    /// `None` means the provider said nothing. Both shapes default their
    /// counts to zero when the field is missing, so a run that reported
    /// nothing is indistinguishable from one that reported zeros — and the
    /// protocol is explicit that an absent `usage` does not mean zero. Given
    /// the choice, leave the field out rather than publish a number nobody
    /// measured. A real turn never spends zero tokens.
    pub fn tokens(&self) -> Option<(u64, u64)> {
        let (input, output) = match self {
            Self::Claude(r) => (r.usage.input_tokens, r.usage.output_tokens),
            Self::Codex(r) => {
                let usage = r.usage.as_ref()?;
                (usage.input_tokens, usage.output_tokens)
            }
        };
        (input != 0 || output != 0).then_some((input, output))
    }

    /// What the run cost, in USD. **Only Claude reports this** — the Codex
    /// result document has no cost field at all, so it is always `None`
    /// there rather than a zero that would read as "free".
    pub fn total_cost_usd(&self) -> Option<f64> {
        match self {
            Self::Claude(r) => (r.total_cost_usd > 0.0).then_some(r.total_cost_usd),
            Self::Codex(_) => None,
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            Self::Claude(r) => r.is_error,
            Self::Codex(r) => r.is_error,
        }
    }
    pub fn permission_denials(&self) -> &[Value] {
        match self {
            Self::Claude(r) => &r.permission_denials,
            Self::Codex(r) => &r.permission_denials,
        }
    }
    pub fn summary(&self) -> String {
        match self {
            Self::Claude(r) => r.summary(),
            Self::Codex(r) => r.summary(),
        }
    }
}
impl Serialize for AgentResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Claude(r) => r.serialize(serializer),
            Self::Codex(r) => r.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for AgentResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value.get("provider") {
            None => serde_json::from_value(value).map(Self::Claude),
            Some(Value::String(provider)) if provider == "codex" => {
                serde_json::from_value(value).map(Self::Codex)
            }
            _ => return Err(serde::de::Error::custom("unknown Agent result provider")),
        }
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex::result::Usage as CodexUsage;
    use crate::provider::types::Usage as ClaudeUsage;

    fn claude(usage: ClaudeUsage, cost: f64) -> AgentResult {
        AgentResult::Claude(RunResult {
            is_error: false,
            subtype: None,
            result: None,
            session_id: None,
            num_turns: 1,
            duration_ms: 0,
            duration_api_ms: 0,
            total_cost_usd: cost,
            usage,
            permission_denials: vec![],
        })
    }

    fn codex(usage: Option<CodexUsage>) -> AgentResult {
        AgentResult::Codex(
            serde_json::from_value(serde_json::json!({
                "provider": "codex",
                "session_id": "s-1",
                "result": null,
                "is_error": false,
                "num_turns": 1,
                "usage": usage,
                "warnings": [],
                "permission_denials": [],
            }))
            .expect("codex result fixture"),
        )
    }

    #[test]
    fn reported_counts_come_through_for_both_providers() {
        let claude = claude(
            ClaudeUsage {
                input_tokens: 34,
                output_tokens: 137,
                ..Default::default()
            },
            0.19,
        );
        assert_eq!(claude.tokens(), Some((34, 137)));
        let codex = codex(Some(CodexUsage {
            input_tokens: 11,
            output_tokens: 22,
            ..Default::default()
        }));
        assert_eq!(codex.tokens(), Some((11, 22)));
    }

    /// Both shapes default their counts to zero, so all-zero is how "the
    /// provider said nothing" arrives. Publishing that as a measurement
    /// would contradict the protocol's "absent does not mean zero".
    #[test]
    fn nothing_reported_is_absent_not_zero() {
        assert_eq!(claude(ClaudeUsage::default(), 0.0).tokens(), None);
        assert_eq!(codex(None).tokens(), None);
        assert_eq!(codex(Some(CodexUsage::default())).tokens(), None);
    }

    #[test]
    fn one_nonzero_count_is_still_a_report() {
        let only_output = ClaudeUsage {
            output_tokens: 7,
            ..Default::default()
        };
        assert_eq!(claude(only_output, 0.0).tokens(), Some((0, 7)));
    }

    #[test]
    fn cost_is_claude_only_and_never_a_made_up_zero() {
        assert_eq!(
            claude(ClaudeUsage::default(), 0.19).total_cost_usd(),
            Some(0.19)
        );
        assert_eq!(claude(ClaudeUsage::default(), 0.0).total_cost_usd(), None);
        // Codex's result document has no cost field to read.
        assert_eq!(codex(Some(CodexUsage::default())).total_cost_usd(), None);
    }
}

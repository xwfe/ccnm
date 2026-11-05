//! Preserve the Claude result wire document. One tagged `"provider": "codex"`
//! came from a Codex session, which ccnm no longer runs (P118), and is
//! refused by name.
use super::RunResult;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentResult {
    Claude(RunResult),
}
impl AgentResult {
    pub fn provider_session_id(&self) -> Option<&str> {
        let Self::Claude(r) = self;
        r.session_id.as_deref()
    }
    pub fn text(&self) -> Option<&str> {
        let Self::Claude(r) = self;
        r.result.as_deref()
    }
    pub fn into_text(self) -> Option<String> {
        let Self::Claude(r) = self;
        r.result
    }
    /// Input and output token counts, when the run reported them.
    ///
    /// `None` means it said nothing. The counts default to zero when the
    /// field is missing, so a run that reported nothing is
    /// indistinguishable from one that reported zeros — and the protocol is
    /// explicit that an absent `usage` does not mean zero. Given the choice,
    /// leave the field out rather than publish a number nobody measured. A
    /// real turn never spends zero tokens.
    pub fn tokens(&self) -> Option<(u64, u64)> {
        let Self::Claude(r) = self;
        let (input, output) = (r.usage.input_tokens, r.usage.output_tokens);
        (input != 0 || output != 0).then_some((input, output))
    }

    /// What the run cost, in USD; `None` rather than a zero that would read
    /// as "free".
    pub fn total_cost_usd(&self) -> Option<f64> {
        let Self::Claude(r) = self;
        (r.total_cost_usd > 0.0).then_some(r.total_cost_usd)
    }

    pub fn is_error(&self) -> bool {
        let Self::Claude(r) = self;
        r.is_error
    }
    pub fn permission_denials(&self) -> &[Value] {
        let Self::Claude(r) = self;
        &r.permission_denials
    }
    pub fn summary(&self) -> String {
        let Self::Claude(r) = self;
        r.summary()
    }

    /// The few numbers a person reads after a run (P95): turns, the cost
    /// when there is one, and permission denials only when there were any.
    /// [`summary`](Self::summary) has the token breakdown and API time;
    /// they stay in the result document in the session directory, which is
    /// where anyone who needs them looks.
    pub fn brief(&self, lang: crate::Lang) -> String {
        let Self::Claude(r) = self;
        let (turns, denied) = (r.num_turns, r.permission_denials.len());
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let mut parts = vec![lang.pick(
            format!("{turns} 轮"),
            format!("{turns} turn{}", plural(turns as usize)),
        )];
        if let Some(cost) = self.total_cost_usd() {
            parts.push(format!("${cost:.2}"));
        }
        if denied > 0 {
            parts.push(lang.pick(
                format!("{denied} 次权限被拒"),
                format!("{denied} permission denial{}", plural(denied)),
            ));
        }
        parts.join(lang.pick("，", ", "))
    }
}
impl Serialize for AgentResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Self::Claude(r) = self;
        r.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for AgentResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value.get("provider") {
            None => serde_json::from_value(value)
                .map(Self::Claude)
                .map_err(serde::de::Error::custom),
            Some(Value::String(provider)) if provider == "codex" => {
                Err(serde::de::Error::custom(super::CODEX_REMOVED))
            }
            _ => Err(serde::de::Error::custom("unknown Agent result provider")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn reported_counts_come_through() {
        let claude = claude(
            ClaudeUsage {
                input_tokens: 34,
                output_tokens: 137,
                ..Default::default()
            },
            0.19,
        );
        assert_eq!(claude.tokens(), Some((34, 137)));
    }

    /// The counts default to zero, so all-zero is how "the run said
    /// nothing" arrives. Publishing that as a measurement would contradict
    /// the protocol's "absent does not mean zero".
    #[test]
    fn nothing_reported_is_absent_not_zero() {
        assert_eq!(claude(ClaudeUsage::default(), 0.0).tokens(), None);
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
    fn cost_is_never_a_made_up_zero() {
        assert_eq!(
            claude(ClaudeUsage::default(), 0.19).total_cost_usd(),
            Some(0.19)
        );
        assert_eq!(claude(ClaudeUsage::default(), 0.0).total_cost_usd(), None);
    }

    /// A result a Codex session left behind is refused with the sentence
    /// that says Codex is gone, not read as a Claude result (P118).
    #[test]
    fn a_codex_result_is_refused_by_name() {
        let err = serde_json::from_value::<AgentResult>(serde_json::json!({
            "provider": "codex", "session_id": "s-1", "is_error": false, "num_turns": 1
        }))
        .unwrap_err();
        assert!(
            err.to_string().contains(crate::provider::CODEX_REMOVED),
            "{err}"
        );
    }
}

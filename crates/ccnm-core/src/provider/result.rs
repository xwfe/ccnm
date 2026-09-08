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

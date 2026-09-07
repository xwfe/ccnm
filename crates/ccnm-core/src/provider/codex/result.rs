//! The JSONL shape captured from Codex 0.153.4; warnings are not turn failures.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Tag {
    #[serde(rename = "codex")]
    Codex,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexResult {
    provider: Tag,
    pub session_id: String,
    pub result: Option<String>,
    pub is_error: bool,
    pub num_turns: u32,
    pub usage: Option<Usage>,
    pub warnings: Vec<String>,
    pub permission_denials: Vec<Value>,
}

impl CodexResult {
    pub fn redact(&mut self, private_home: &str) {
        fn scrub(value: &mut Value, private_home: &str) {
            match value {
                Value::String(s) => *s = s.replace(private_home, "<agent-private-config>"),
                Value::Array(values) => {
                    for value in values {
                        scrub(value, private_home);
                    }
                }
                Value::Object(values) => {
                    for value in values.values_mut() {
                        scrub(value, private_home);
                    }
                }
                _ => {}
            }
        }
        if let Some(text) = &mut self.result {
            *text = text.replace(private_home, "<agent-private-config>");
        }
        for warning in &mut self.warnings {
            *warning = warning.replace(private_home, "<agent-private-config>");
        }
        for denial in &mut self.permission_denials {
            scrub(denial, private_home);
        }
    }

    pub fn summary(&self) -> String {
        let tokens = match &self.usage {
            Some(u) => format!(
                "reported tokens in {} out {} cached {} cache-write {} reasoning {}",
                u.input_tokens,
                u.output_tokens,
                u.cached_input_tokens,
                u.cache_write_input_tokens,
                u.reasoning_output_tokens
            ),
            None => "token usage not reported".into(),
        };
        format!(
            "{} turn{}; {tokens}; {} permission denial{}; cost and API timing not reported",
            self.num_turns,
            if self.num_turns == 1 { "" } else { "s" },
            self.permission_denials.len(),
            if self.permission_denials.len() == 1 {
                ""
            } else {
                "s"
            }
        )
    }
}

pub fn parse(stdout: &[u8]) -> Result<CodexResult> {
    if stdout.len() > 16 * 1024 * 1024 {
        return Err(Error::internal("Codex result exceeds 16 MiB"));
    }
    let mut result = CodexResult {
        provider: Tag::Codex,
        session_id: String::new(),
        result: None,
        is_error: false,
        num_turns: 0,
        usage: None,
        warnings: Vec::new(),
        permission_denials: Vec::new(),
    };
    let mut active = false;
    let mut terminal = false;
    for line in stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        let event: Value = serde_json::from_slice(line)
            .map_err(|e| Error::internal("Codex stdout is not JSONL").with_source(e))?;
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                let id = event["thread_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| Error::internal("Codex thread.started has no id"))?;
                if !result.session_id.is_empty() && result.session_id != id {
                    return Err(Error::internal("Codex stdout contains multiple threads"));
                }
                result.session_id = id.into();
            }
            Some("turn.started") => {
                if active {
                    return Err(Error::internal("overlapping Codex turns"));
                }
                active = true;
                terminal = false;
                result.result = None;
            }
            Some("turn.completed" | "turn.failed") => {
                if !active {
                    return Err(Error::internal(
                        "Codex terminal event without an active turn",
                    ));
                }
                active = false;
                terminal = true;
                result.num_turns = result
                    .num_turns
                    .checked_add(1)
                    .ok_or_else(|| Error::internal("Codex turn count overflow"))?;
                let failed = event["type"] == "turn.failed";
                result.is_error |= failed;
                if failed {
                    result.result = Some(
                        event["error"]["message"]
                            .as_str()
                            .unwrap_or("Codex turn failed")
                            .into(),
                    );
                } else {
                    let usage = &event["usage"];
                    let total_usage = result.usage.get_or_insert_with(Usage::default);
                    for (name, total) in [
                        ("input_tokens", &mut total_usage.input_tokens),
                        ("cached_input_tokens", &mut total_usage.cached_input_tokens),
                        (
                            "cache_write_input_tokens",
                            &mut total_usage.cache_write_input_tokens,
                        ),
                        ("output_tokens", &mut total_usage.output_tokens),
                        (
                            "reasoning_output_tokens",
                            &mut total_usage.reasoning_output_tokens,
                        ),
                    ] {
                        let n = usage.get(name).and_then(Value::as_u64).ok_or_else(|| {
                            Error::internal("Codex token count missing or invalid")
                        })?;
                        *total = total
                            .checked_add(n)
                            .ok_or_else(|| Error::internal("Codex token count overflow"))?;
                    }
                }
            }
            Some("item.completed") => {
                let item = &event["item"];
                match item["type"].as_str() {
                    Some("agent_message") if active => {
                        result.result = item["text"].as_str().map(str::to_owned)
                    }
                    Some("error") => {
                        if let Some(message) = item["message"].as_str() {
                            result.warnings.push(message.into());
                        }
                    }
                    Some("mcp_tool_call")
                        if item["status"] == "failed"
                            && item["error"]["message"]
                                .as_str()
                                .is_some_and(|s| s.contains("requires approval")) =>
                    {
                        result.permission_denials.push(item.clone());
                    }
                    _ => {}
                }
            }
            Some("error") => {
                if let Some(message) = event["message"].as_str() {
                    result.warnings.push(message.into());
                }
            }
            _ => {}
        }
    }
    if result.session_id.is_empty() || active || !terminal {
        return Err(Error::internal(
            "Codex stdout has no completed terminal turn",
        ));
    }
    Ok(result)
}

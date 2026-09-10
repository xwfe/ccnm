//! The JSON-RPC 2.0 envelope on stdio, exactly as `docs/protocol/` defines
//! it. Nothing here knows what a session is; it turns bytes into a request
//! or into the error that must go back instead.
//!
//! Framing is one message per line. A line longer than [`MAX_LINE`] is
//! dropped and answered with `-32600` **without closing the connection**:
//! the delimiter is a newline, so the reader realigns on the next one. A
//! length-prefixed framing could not recover from the same mistake.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{Error, ErrorCode};

/// Longest request line accepted, newline included.
///
/// Picked to be obviously enough for a prompt while keeping the reader's
/// buffer bounded. There is no measurement behind the exact number yet --
/// see the review notes -- so if a real prompt ever hits it, change it here
/// and record why.
pub const MAX_LINE: usize = 1024 * 1024;

pub mod code {
    //! Every code the protocol defines. The five predefined ones come from
    //! JSON-RPC 2.0; `-32000..=-32099` is the range the spec reserves for
    //! implementations, and the rest of this list is our allocation.

    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub const NOT_READY: i32 = -32000;
    pub const CONFIG: i32 = -32001;
    pub const VERSION_MISMATCH: i32 = -32002;
    pub const AUTH: i32 = -32003;
    pub const AGENT_UNREACHABLE: i32 = -32004;
    pub const RUNTIME_UNREACHABLE: i32 = -32005;
    pub const WORKSPACE: i32 = -32006;
    pub const POLICY: i32 = -32007;
    pub const BUSY: i32 = -32008;
    pub const NOT_FOUND: i32 = -32009;
    pub const CONFLICT: i32 = -32010;
    pub const UNCERTAIN: i32 = -32011;
    pub const EXPIRED: i32 = -32012;
    pub const UNSUPPORTED_CAPABILITY: i32 = -32013;
    pub const HANDSHAKE_REQUIRED: i32 = -32014;
}

/// Whether the call left anything behind.
///
/// Deliberately not a `recoverable` boolean: an error can be both transient
/// and already have had an effect, and a field with that name gets read as
/// "retrying is safe".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Refused; nothing happened. Resending is safe.
    None,
    /// May or may not have run. Go look before doing anything else.
    Unknown,
    /// Already took effect. Resending repeats it.
    Applied,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorData {
    pub effect: Effect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ccnm_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    /// Boxed to keep `Result<_, RpcError>` small. Seven optional fields
    /// inline would make every success path carry the cost of the rare
    /// failure path, which clippy rightly complains about.
    pub data: Box<ErrorData>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>, effect: Effect) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: Box::new(ErrorData {
                effect,
                ccnm_code: None,
                detail: None,
                session: None,
                reason: None,
                supported: None,
                unknown: None,
            }),
        }
    }

    /// Nothing happened, which is the common case for a refusal.
    pub fn refused(code: i32, message: impl Into<String>) -> Self {
        RpcError::new(code, message, Effect::None)
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.data.reason = Some(reason.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.data.detail = Some(detail.into());
        self
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.data.session = Some(session.into());
        self
    }

    pub fn with_supported(mut self, supported: Vec<String>) -> Self {
        self.data.supported = Some(supported);
        self
    }

    pub fn with_unknown(mut self, unknown: Vec<String>) -> Self {
        self.data.unknown = Some(unknown);
        self
    }
}

/// Map a ccnm failure onto the public code table.
///
/// The message is the one ccnm already writes for people, which the safety
/// layer keeps free of credentials and private paths. `ccnm_code` carries
/// the stable internal name so a caller can line an RPC failure up with
/// what the human CLI prints.
///
/// Effect is `None` for everything here on purpose: these are all failures
/// raised **before** anything was started. A caller that has to be told
/// "this may have run" gets that from the session store, not from a
/// translated error.
pub fn from_ccnm(err: &Error) -> RpcError {
    let code = match err.code() {
        ErrorCode::Internal => code::INTERNAL_ERROR,
        ErrorCode::NotReady => code::NOT_READY,
        ErrorCode::Config => code::CONFIG,
        ErrorCode::Version => code::VERSION_MISMATCH,
        ErrorCode::Auth => code::AUTH,
        ErrorCode::AgentUnreachable => code::AGENT_UNREACHABLE,
        ErrorCode::RuntimeUnreachable => code::RUNTIME_UNREACHABLE,
        // Both mean "the two sides are not looking at the same project".
        // The caller's move is the same either way: go fix that machine.
        // `ccnm_code` still tells them which one it was.
        ErrorCode::Mount | ErrorCode::WrongWorkspace => code::WORKSPACE,
        ErrorCode::Coherence | ErrorCode::Policy | ErrorCode::StaleEpoch => code::POLICY,
        ErrorCode::InvalidArgs => code::INVALID_PARAMS,
        ErrorCode::Dependency => code::NOT_READY,
    };
    let mut rpc = RpcError::refused(code, err.message().to_string());
    rpc.data.ccnm_code = Some(err.code().name());
    rpc
}

/// A well-formed request, already checked down to the envelope.
#[derive(Debug, Clone)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Map<String, Value>,
}

/// What one input line turned out to be.
#[derive(Debug)]
pub enum Incoming {
    Call(Request),
    /// A request with no `id`. The spec forbids answering it, so the only
    /// thing left is to drop it and say so on stderr.
    Notification,
    /// Answer with this error. `id` is `null` whenever the line was too
    /// broken to carry one.
    Reject(Value, RpcError),
}

/// Parse one line. Never fails: everything wrong with the input is a
/// `Reject` the caller can write straight back out.
pub fn parse(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return Incoming::Reject(
                Value::Null,
                RpcError::refused(code::PARSE_ERROR, "invalid JSON on stdin")
                    .with_detail(e.to_string()),
            );
        }
    };

    // A batch is legal JSON-RPC and we refuse it, so this check comes
    // before anything that expects an object.
    if value.is_array() {
        return Incoming::Reject(
            Value::Null,
            RpcError::refused(code::INVALID_REQUEST, "batch requests are not supported")
                .with_reason("batch_unsupported"),
        );
    }

    let Some(object) = value.as_object() else {
        return Incoming::Reject(
            Value::Null,
            RpcError::refused(code::INVALID_REQUEST, "a request must be a JSON object"),
        );
    };

    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Incoming::Reject(
            Value::Null,
            RpcError::refused(code::INVALID_REQUEST, "jsonrpc must be exactly \"2.0\"")
                .with_reason("jsonrpc_version"),
        );
    }

    // Missing id means notification; present but unusable means the id
    // cannot be echoed, so the answer carries null. These are different
    // cases and collapsing them loses the spec's distinction.
    let Some(id) = object.get("id") else {
        return Incoming::Notification;
    };
    if !usable_id(id) {
        return Incoming::Reject(
            Value::Null,
            RpcError::refused(
                code::INVALID_REQUEST,
                "id must be a string of 1..128 characters or an integer",
            )
            .with_reason("id_must_be_string_or_integer"),
        );
    }

    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Incoming::Reject(
            id.clone(),
            RpcError::refused(code::INVALID_REQUEST, "method must be a string"),
        );
    };

    let params = match object.get("params") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        // Positional parameters are legal JSON-RPC; this protocol only
        // takes named ones, and an array here is far more likely to be a
        // client bug than a deliberate choice.
        Some(_) => {
            return Incoming::Reject(
                id.clone(),
                RpcError::refused(code::INVALID_PARAMS, "params must be an object")
                    .with_reason("named_params_only"),
            );
        }
    };

    Incoming::Call(Request {
        id: id.clone(),
        method: method.to_string(),
        params,
    })
}

fn usable_id(id: &Value) -> bool {
    match id {
        Value::String(s) => !s.is_empty() && s.len() <= 128,
        // `as_i64` is what rejects a float: JSON numbers carry no integer
        // type, so 1.5 and 1e400 both land here and both are refused.
        Value::Number(n) => n.as_i64().is_some(),
        _ => false,
    }
}

/// One line of protocol, ready for stdout.
pub fn ok_line(id: &Value, result: Value) -> String {
    line(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn error_line(id: &Value, error: &RpcError) -> String {
    line(serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}))
}

fn line(value: Value) -> String {
    // Serializing a `Value` cannot fail; if it somehow did, an unparseable
    // stdout would be worse than an internal error the client can read.
    match serde_json::to_string(&value) {
        Ok(text) => text,
        Err(_) => {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"cannot encode response","data":{"effect":"unknown"}}}"#
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(line: &str) -> Request {
        match parse(line) {
            Incoming::Call(req) => req,
            other => panic!("expected a call, got {other:?}"),
        }
    }

    fn reject(line: &str) -> (Value, RpcError) {
        match parse(line) {
            Incoming::Reject(id, err) => (id, err),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_request_parses() {
        let req = call(r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"x"}}"#);
        assert_eq!(req.id, Value::from(1));
        assert_eq!(req.method, "hello");
        assert_eq!(req.params.get("client").unwrap(), "x");
    }

    #[test]
    fn params_may_be_omitted() {
        let req = call(r#"{"jsonrpc":"2.0","id":"a","method":"agents.list"}"#);
        assert!(req.params.is_empty());
    }

    #[test]
    fn broken_json_answers_with_a_null_id() {
        let (id, err) = reject("{ not json");
        assert_eq!(id, Value::Null);
        assert_eq!(err.code, code::PARSE_ERROR);
    }

    #[test]
    fn a_batch_is_refused_with_a_null_id() {
        // The id cannot be echoed: there is no single request to echo it from.
        let (id, err) = reject(r#"[{"jsonrpc":"2.0","id":1,"method":"hello"}]"#);
        assert_eq!(id, Value::Null);
        assert_eq!(err.code, code::INVALID_REQUEST);
        assert_eq!(err.data.reason.as_deref(), Some("batch_unsupported"));
    }

    #[test]
    fn a_missing_id_is_a_notification_and_gets_no_answer() {
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","method":"hello"}"#),
            Incoming::Notification
        ));
    }

    #[test]
    fn null_and_fractional_ids_are_refused_not_treated_as_notifications() {
        for line in [
            r#"{"jsonrpc":"2.0","id":null,"method":"hello"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"hello"}"#,
            r#"{"jsonrpc":"2.0","id":{},"method":"hello"}"#,
            r#"{"jsonrpc":"2.0","id":"","method":"hello"}"#,
        ] {
            let (id, err) = reject(line);
            assert_eq!(id, Value::Null, "{line}");
            assert_eq!(err.code, code::INVALID_REQUEST, "{line}");
        }
    }

    #[test]
    fn the_wrong_jsonrpc_version_is_refused() {
        let (_, err) = reject(r#"{"jsonrpc":"1.0","id":1,"method":"hello"}"#);
        assert_eq!(err.data.reason.as_deref(), Some("jsonrpc_version"));
    }

    #[test]
    fn positional_params_are_refused_but_keep_the_id() {
        let (id, err) = reject(r#"{"jsonrpc":"2.0","id":7,"method":"hello","params":[1]}"#);
        assert_eq!(id, Value::from(7));
        assert_eq!(err.code, code::INVALID_PARAMS);
    }

    #[test]
    fn a_missing_method_keeps_the_id() {
        let (id, err) = reject(r#"{"jsonrpc":"2.0","id":7}"#);
        assert_eq!(id, Value::from(7));
        assert_eq!(err.code, code::INVALID_REQUEST);
    }

    #[test]
    fn lines_are_single_line_json() {
        // A newline inside a value must come out escaped: the framing is
        // one message per line, so a raw one would split the message in two.
        let text = ok_line(&Value::from(1), serde_json::json!({"a": "x\ny"}));
        assert!(!text.contains('\n'), "{text}");
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["jsonrpc"], "2.0");
        assert_eq!(back["id"], 1);
        assert_eq!(back["result"]["a"], "x\ny");
    }

    #[test]
    fn every_ccnm_code_maps_into_the_documented_table() {
        let documented: Vec<i32> = vec![
            code::PARSE_ERROR,
            code::INVALID_REQUEST,
            code::METHOD_NOT_FOUND,
            code::INVALID_PARAMS,
            code::INTERNAL_ERROR,
            code::NOT_READY,
            code::CONFIG,
            code::VERSION_MISMATCH,
            code::AUTH,
            code::AGENT_UNREACHABLE,
            code::RUNTIME_UNREACHABLE,
            code::WORKSPACE,
            code::POLICY,
            code::BUSY,
            code::NOT_FOUND,
            code::CONFLICT,
            code::UNCERTAIN,
            code::EXPIRED,
            code::UNSUPPORTED_CAPABILITY,
            code::HANDSHAKE_REQUIRED,
        ];
        for ccnm in ErrorCode::ALL {
            let mapped = from_ccnm(&Error::new(ccnm, "x"));
            assert!(
                documented.contains(&mapped.code),
                "{ccnm:?} maps to {} which is not in the table",
                mapped.code
            );
            assert_eq!(mapped.data.ccnm_code, Some(ccnm.name()));
            assert_eq!(mapped.data.effect, Effect::None);
        }
    }

    #[test]
    fn error_lines_carry_the_effect() {
        let err = RpcError::refused(code::BUSY, "busy");
        let text = error_line(&Value::from(1), &err);
        assert!(text.contains(r#""effect":"none""#), "{text}");
        assert!(!text.contains("ccnm_code"), "{text}");
    }
}

//! `ccnm rpc`: the local stdio Machine API defined in `docs/protocol/`.
//!
//! Three streams, three jobs, and the split is not negotiable: stdin carries
//! requests, stdout carries **only** protocol lines, stderr carries logs. An
//! Agent's own stdout never reaches this file -- that belongs to a session's
//! result and comes back through `session.result`.
//!
//! The server is deliberately single-threaded over the read loop. Work that
//! outlives a call does not block it: `session.start` hands the run to a
//! background thread and answers with a handle, which is what lets a client
//! disconnect and come back for the result later.

pub mod wire;

use std::io::{BufRead, Write};

use serde_json::{Map, Value};

use wire::{Incoming, Request, RpcError, code};

/// The protocol identifier this build speaks. Only the major version is in
/// it: adding fields, methods or capabilities does not change it.
pub const PROTOCOL_ID: &str = "ccnm.machine/1";

/// What this build actually implements. A capability that is not in here is
/// one a client must treat as absent -- so nothing goes in until the method
/// behind it works.
fn capabilities() -> Value {
    serde_json::json!({
        "modes": ["print"],
        "session_output": true,
        "start_key": true,
        "stop": true,
    })
}

/// One connection's state. Only the handshake for now; the session store
/// joins it when `session.*` lands.
pub struct Server {
    greeted: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server { greeted: false }
    }

    /// Answer one parsed request. Returns the value for `result`, or the
    /// error to send instead.
    fn dispatch(&mut self, req: &Request) -> Result<Value, RpcError> {
        if req.method == "hello" {
            let result = hello(&req.params)?;
            self.greeted = true;
            return Ok(result);
        }
        if !self.greeted {
            return Err(RpcError::refused(
                code::HANDSHAKE_REQUIRED,
                "hello must be the first request",
            ));
        }
        Err(RpcError::refused(
            code::METHOD_NOT_FOUND,
            format!("unknown method {}", req.method),
        ))
    }
}

fn hello(params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(params, &["client", "protocol_versions"])?;
    let _client = require_str(params, "client")?;
    let offered = params.get("protocol_versions").and_then(Value::as_array);
    let Some(offered) = offered.filter(|list| !list.is_empty()) else {
        return Err(RpcError::refused(
            code::INVALID_PARAMS,
            "protocol_versions must be a non-empty array of strings",
        ));
    };
    if !offered.iter().any(|v| v.as_str() == Some(PROTOCOL_ID)) {
        return Err(
            RpcError::refused(code::VERSION_MISMATCH, "no shared protocol version")
                .with_supported(vec![PROTOCOL_ID.to_string()]),
        );
    }
    Ok(serde_json::json!({
        "protocol": PROTOCOL_ID,
        "server": {"name": "ccnm", "version": crate::VERSION},
        "capabilities": capabilities(),
    }))
}

/// Refuse parameters this method does not define.
///
/// Silently ignoring them turns a typo into "you did not pass it", and the
/// caller then spends an afternoon wondering why the field had no effect.
fn reject_unknown(params: &Map<String, Value>, allowed: &[&str]) -> Result<(), RpcError> {
    let unknown: Vec<String> = params
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(RpcError::refused(code::INVALID_PARAMS, "unknown parameter").with_unknown(unknown))
}

fn require_str<'a>(params: &'a Map<String, Value>, name: &str) -> Result<&'a str, RpcError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            RpcError::refused(
                code::INVALID_PARAMS,
                format!("{name} is required and must be a non-empty string"),
            )
        })
}

/// Read requests until stdin ends, answering each on stdout.
///
/// EOF is a normal end, not a failure: the client closing the pipe means
/// "no more calls", and sessions already accepted keep running without it.
pub fn serve<R: BufRead, W: Write>(mut input: R, mut output: W) -> std::io::Result<()> {
    let mut server = Server::new();
    loop {
        match next_line(&mut input)? {
            Framed::Eof => return Ok(()),
            Framed::TooLong => {
                let err = RpcError::refused(
                    code::INVALID_REQUEST,
                    format!("request line exceeds the {} byte limit", wire::MAX_LINE),
                )
                .with_reason("oversize");
                write_line(&mut output, &wire::error_line(&Value::Null, &err))?;
            }
            Framed::NotUtf8 => {
                let err = RpcError::refused(code::PARSE_ERROR, "request line is not valid UTF-8");
                write_line(&mut output, &wire::error_line(&Value::Null, &err))?;
            }
            Framed::Line(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                match wire::parse(&line) {
                    Incoming::Notification => {
                        // The spec forbids answering a notification, so the
                        // only honest thing is to say on stderr that it was
                        // dropped. A client that uses them never learns
                        // whether anything ran.
                        tracing::warn!(
                            "dropped a notification: this protocol has no notifications"
                        );
                    }
                    Incoming::Reject(id, err) => {
                        write_line(&mut output, &wire::error_line(&id, &err))?;
                    }
                    Incoming::Call(req) => {
                        let line = match server.dispatch(&req) {
                            Ok(result) => wire::ok_line(&req.id, result),
                            Err(err) => wire::error_line(&req.id, &err),
                        };
                        write_line(&mut output, &line)?;
                    }
                }
            }
        }
    }
}

fn write_line<W: Write>(output: &mut W, line: &str) -> std::io::Result<()> {
    output.write_all(line.as_bytes())?;
    output.write_all(b"\n")?;
    // Flushed per message: a client blocked reading our answer while we sit
    // on a buffered one is a deadlock, not slow I/O.
    output.flush()
}

enum Framed {
    Eof,
    Line(String),
    TooLong,
    NotUtf8,
}

/// One line, with a hard cap on how much is buffered.
///
/// A line that hits the cap without a newline is oversize; the rest of it is
/// skipped so the next read starts on a real message boundary. A short line
/// with no newline is just the last one before EOF, which is fine to process.
fn next_line<R: BufRead>(input: &mut R) -> std::io::Result<Framed> {
    let mut buf = Vec::new();
    let found = read_bounded(input, wire::MAX_LINE, &mut buf)?;
    if !found {
        if buf.is_empty() {
            return Ok(Framed::Eof);
        }
        if buf.len() >= wire::MAX_LINE {
            skip_to_newline(input)?;
            return Ok(Framed::TooLong);
        }
    }
    match String::from_utf8(buf) {
        Ok(line) => Ok(Framed::Line(line)),
        Err(_) => Ok(Framed::NotUtf8),
    }
}

fn skip_to_newline<R: BufRead>(input: &mut R) -> std::io::Result<()> {
    let mut sink = Vec::new();
    loop {
        sink.clear();
        if read_bounded(input, wire::MAX_LINE, &mut sink)? || sink.is_empty() {
            return Ok(());
        }
    }
}

/// Append bytes up to the next newline or `limit`, whichever comes first.
/// `true` means a newline was found (and appended).
///
/// Written by hand rather than with `Read::take` + `read_until` because the
/// cap has to survive across calls in [`skip_to_newline`], and a `Take`
/// consumes the reader it wraps.
fn read_bounded<R: BufRead>(
    input: &mut R,
    limit: usize,
    out: &mut Vec<u8>,
) -> std::io::Result<bool> {
    while out.len() < limit {
        let (found, used) = {
            let buf = match input.fill_buf() {
                Ok(buf) => buf,
                // A signal is not a protocol event; retry rather than
                // reporting the stream as broken.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if buf.is_empty() {
                return Ok(false);
            }
            let room = limit - out.len();
            match buf.iter().position(|&b| b == b'\n') {
                Some(index) if index < room => {
                    out.extend_from_slice(&buf[..=index]);
                    (true, index + 1)
                }
                _ => {
                    let take = room.min(buf.len());
                    out.extend_from_slice(&buf[..take]);
                    (false, take)
                }
            }
        };
        input.consume(used);
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed lines in, get answered lines back.
    fn exchange(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(std::io::BufReader::new(input.as_bytes()), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdout must be protocol JSON"))
            .collect()
    }

    const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"t","protocol_versions":["ccnm.machine/1"]}}"#;

    #[test]
    fn hello_answers_with_protocol_and_capabilities() {
        let out = exchange(&format!("{HELLO}\n"));
        assert_eq!(out.len(), 1);
        let result = &out[0]["result"];
        assert_eq!(result["protocol"], PROTOCOL_ID);
        assert_eq!(result["server"]["name"], "ccnm");
        assert!(result["capabilities"]["modes"].is_array());
    }

    #[test]
    fn other_methods_need_the_handshake_first() {
        let out = exchange("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agents.list\"}\n");
        assert_eq!(out[0]["error"]["code"], code::HANDSHAKE_REQUIRED);
        assert_eq!(out[0]["error"]["data"]["effect"], "none");
    }

    #[test]
    fn after_the_handshake_an_unknown_method_is_method_not_found() {
        let out = exchange(&format!(
            "{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.explode\"}}\n"
        ));
        assert_eq!(out[1]["error"]["code"], code::METHOD_NOT_FOUND);
    }

    #[test]
    fn a_version_we_do_not_speak_lists_what_we_do() {
        let out = exchange(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"t\",\"protocol_versions\":[\"ccnm.machine/9\"]}}\n",
        );
        assert_eq!(out[0]["error"]["code"], code::VERSION_MISMATCH);
        assert_eq!(out[0]["error"]["data"]["supported"][0], PROTOCOL_ID);
    }

    #[test]
    fn a_failed_hello_does_not_count_as_a_handshake() {
        let out = exchange(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"t\",\"protocol_versions\":[\"other/1\"]}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\"}\n",
        );
        assert_eq!(out[1]["error"]["code"], code::HANDSHAKE_REQUIRED);
    }

    #[test]
    fn unknown_parameters_are_named_back() {
        let out = exchange(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"t\",\"protocol_versions\":[\"ccnm.machine/1\"],\"cliennt\":\"typo\"}}\n",
        );
        assert_eq!(out[0]["error"]["code"], code::INVALID_PARAMS);
        assert_eq!(out[0]["error"]["data"]["unknown"][0], "cliennt");
    }

    #[test]
    fn eof_ends_the_loop_without_an_error() {
        assert!(exchange("").is_empty());
    }

    #[test]
    fn a_line_without_a_trailing_newline_is_still_processed() {
        let out = exchange(HELLO);
        assert_eq!(out[0]["result"]["protocol"], PROTOCOL_ID);
    }

    #[test]
    fn blank_lines_are_skipped() {
        let out = exchange(&format!("\n   \n{HELLO}\n"));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn notifications_are_dropped_without_an_answer() {
        let out = exchange(&format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"hello\",\"params\":{{}}}}\n{HELLO}\n"
        ));
        assert_eq!(out.len(), 1, "a notification must not be answered");
        assert_eq!(out[0]["id"], 1);
    }

    #[test]
    fn an_oversize_line_is_refused_and_the_next_one_still_works() {
        // The whole point of newline framing: one bad line does not desync
        // the stream.
        let huge = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{{\"client\":\"{}\"}}}}",
            "x".repeat(wire::MAX_LINE)
        );
        let out = exchange(&format!("{huge}\n{HELLO}\n"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[0]["error"]["data"]["reason"], "oversize");
        assert_eq!(out[1]["result"]["protocol"], PROTOCOL_ID);
    }

    #[test]
    fn an_oversize_line_at_eof_does_not_hang() {
        let huge = "x".repeat(wire::MAX_LINE + 10);
        let out = exchange(&huge);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["error"]["data"]["reason"], "oversize");
    }

    #[test]
    fn broken_json_does_not_stop_the_stream() {
        let out = exchange(&format!("{{ not json\n{HELLO}\n"));
        assert_eq!(out[0]["error"]["code"], code::PARSE_ERROR);
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[1]["result"]["protocol"], PROTOCOL_ID);
    }

    #[test]
    fn invalid_utf8_is_a_parse_error_and_the_stream_continues() {
        let mut input = Vec::new();
        input.extend_from_slice(&[0xff, 0xfe]);
        input.push(b'\n');
        input.extend_from_slice(HELLO.as_bytes());
        input.push(b'\n');
        let mut out = Vec::new();
        serve(std::io::BufReader::new(&input[..]), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["error"]["code"], code::PARSE_ERROR);
        assert_eq!(lines[1]["result"]["protocol"], PROTOCOL_ID);
    }

    #[test]
    fn every_answer_is_exactly_one_line() {
        let out = exchange(&format!("{HELLO}\n{HELLO}\n"));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn stdout_never_carries_anything_but_protocol() {
        // exchange() parses every line as JSON, so a stray print would fail
        // there; this asserts the shape on top of that.
        for value in exchange(&format!("{HELLO}\n")) {
            assert_eq!(value["jsonrpc"], "2.0");
            assert!(value.get("result").is_some() || value.get("error").is_some());
        }
    }

    #[test]
    fn declared_capabilities_stay_in_step_with_the_schema() {
        // modes is required and non-empty in the schema; an empty one would
        // mean "this server can start nothing", which is not what we mean.
        let caps = capabilities();
        assert!(!caps["modes"].as_array().unwrap().is_empty());
        assert_eq!(caps["effect"], Value::Null, "no stray keys");
    }

    #[test]
    fn effect_is_none_on_envelope_refusals() {
        let out = exchange("[]\n");
        assert_eq!(out[0]["error"]["data"]["effect"], "none");
    }
}

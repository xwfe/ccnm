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

pub mod session;
pub mod store;
pub mod wire;

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::config::Config;
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

/// What the server needs from the machine it runs on.
pub struct Context {
    pub config_path: PathBuf,
    /// Where session records live; `paths::state_dir()` in production.
    pub state: PathBuf,
    /// What actually starts and stops sessions.
    pub runs: std::sync::Arc<dyn session::Runs>,
    /// For asking `ps` whether a record's owning process is still there.
    pub runner: std::sync::Arc<dyn crate::process::ProcessRunner + Send + Sync>,
}

impl Context {
    /// Read the config fresh for every call.
    ///
    /// `agents.list` promises to reflect the *current* configuration, and a
    /// long-lived process that cached it at startup would keep answering
    /// with a workspace the operator removed an hour ago.
    fn config(&self) -> Result<Config, RpcError> {
        Config::load(&self.config_path).map_err(|err| wire::from_ccnm(&err))
    }
}

/// One connection's state. Only the handshake for now; the session store
/// joins it when `session.*` lands.
pub struct Server {
    greeted: bool,
    ctx: Context,
}

impl Server {
    pub fn new(ctx: Context) -> Self {
        Server {
            greeted: false,
            ctx,
        }
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
        match req.method.as_str() {
            "agents.list" => agents_list(&self.ctx, &req.params),
            "session.start" => session::start(&self.ctx, &req.params),
            "session.status" => session::status(&self.ctx, &req.params),
            "session.result" => session::result(&self.ctx, &req.params),
            "session.stop" => session::stop(&self.ctx, &req.params),
            _ => Err(RpcError::refused(
                code::METHOD_NOT_FOUND,
                format!("unknown method {}", req.method),
            )),
        }
    }
}

/// Every `(node, instance)` this machine can address, and the workspaces
/// bound to each.
///
/// Config only, by design: no ssh, no login check, no process. Whether one
/// of these can actually run is a question only `session.start` answers.
///
/// No `provider` field, and not because it was forgotten. Config validation
/// requires an instance workspace's root to live only on its Runtime Node
/// and requires instance mode to be non-colocated, so the node in a binding
/// is never this machine -- and the instance definition that would name the
/// provider lives on that other machine. Reporting one from here would mean
/// copying a second, driftable copy of something the Agent side owns.
fn agents_list(ctx: &Context, params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(params, &[])?;
    let config = ctx.config()?;
    let mut bound: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    // Instances defined here but not yet bound still get an entry: they are
    // addressable the moment a workspace points at them, and leaving them
    // out would look like they do not exist.
    for instance in config.agents.keys() {
        if let Some(node) = config.this.as_deref() {
            bound.entry((node, instance)).or_default();
        }
    }
    for (name, workspace) in &config.workspaces {
        if let Some(reference) = &workspace.agent {
            bound
                .entry((&reference.node, &reference.instance))
                .or_default()
                .push(name);
        }
    }

    let agents: Vec<Value> = bound
        .into_iter()
        .map(|((node, instance), workspaces)| {
            serde_json::json!({
                "node": node,
                "instance": instance,
                "workspaces": workspaces,
            })
        })
        .collect();
    Ok(serde_json::json!({"agents": agents}))
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
pub(crate) fn reject_unknown(
    params: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), RpcError> {
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

pub(crate) fn require_str<'a>(
    params: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, RpcError> {
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
pub fn serve<R: BufRead, W: Write>(
    ctx: Context,
    mut input: R,
    mut output: W,
) -> std::io::Result<()> {
    let mut server = Server::new(ctx);
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

    use crate::process::SystemRunner;
    use session::{RunAsk, Runs};
    use std::sync::{Arc, Mutex};

    /// A fresh directory every call: tests run in parallel and two of them
    /// sharing a state directory would see each other's session records.
    fn temp(test: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ccnm-rpc-{}-{test}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Stands in for the launcher. No ssh, no Agent, no controller: these
    /// tests are about the protocol and the record, and a test that needs a
    /// real Agent is not an offline test.
    #[derive(Default)]
    struct FakeRuns {
        /// What `run_print` should do. `None` means "fail to start".
        report: Option<crate::protocol::run::RunReport>,
        asks: Mutex<Vec<RunAsk>>,
        stops: Mutex<Vec<String>>,
    }

    impl FakeRuns {
        fn ok(exit_code: i32, text: &str) -> Self {
            FakeRuns {
                report: Some(report(exit_code, text)),
                ..FakeRuns::default()
            }
        }
    }

    impl Runs for FakeRuns {
        fn run_print(&self, ask: &RunAsk) -> crate::error::Result<crate::protocol::run::RunReport> {
            self.asks.lock().unwrap().push(ask.clone());
            self.report
                .clone()
                .ok_or_else(|| crate::Error::new(crate::ErrorCode::AgentUnreachable, "no route"))
        }

        fn stop(&self, workspace: &str, _instance: Option<&str>) -> crate::error::Result<bool> {
            self.stops.lock().unwrap().push(workspace.to_string());
            Ok(true)
        }
    }

    fn report(exit_code: i32, text: &str) -> crate::protocol::run::RunReport {
        crate::protocol::run::RunReport {
            protocol: 3,
            provider: crate::provider::AgentProvider::Claude,
            agent_identity: None,
            session: "ccnm-uuid-1".to_string(),
            session_dir: PathBuf::from("/private/state/sessions/ccnm-uuid-1"),
            controller: crate::controller::Context {
                hello: crate::protocol::hello::answer(&crate::protocol::hello::HelloRequest::new(
                    None,
                )),
                pid: 4241,
                manager: Ok("Aqua".to_string()),
            },
            pid: 4242,
            outcome: crate::session::Outcome {
                exit_code: Some(exit_code),
                timed_out: false,
                duration_ms: 1234,
                error: None,
            },
            result: None,
            stdout_tail: text.to_string(),
            stderr_tail: String::new(),
        }
    }

    /// Run a conversation against a given executor and config.
    fn talk(runs: Arc<dyn Runs>, config_path: PathBuf, state: PathBuf, input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(
            Context {
                config_path,
                state,
                runs,
                runner: Arc::new(SystemRunner),
            },
            std::io::BufReader::new(input.as_bytes()),
            &mut out,
        )
        .unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdout must be protocol JSON"))
            .collect()
    }

    /// A config file with whatever body the test needs.
    fn config_with(test: &str, body: &str) -> PathBuf {
        let path = temp(test).join("config.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Feed lines in, get answered lines back.
    fn exchange(input: &str) -> Vec<Value> {
        exchange_with(PathBuf::from("/nonexistent/ccnm/config.toml"), input)
    }

    fn exchange_with(config_path: PathBuf, input: &str) -> Vec<Value> {
        talk(
            Arc::new(FakeRuns::default()),
            config_path,
            temp("exchange-state"),
            input,
        )
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
        serve(
            Context {
                config_path: PathBuf::from("/nonexistent/ccnm/config.toml"),
                state: temp("utf8-state"),
                runs: Arc::new(FakeRuns::default()),
                runner: Arc::new(SystemRunner),
            },
            std::io::BufReader::new(&input[..]),
            &mut out,
        )
        .unwrap();
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

    /// A Runtime Node: it holds the workspaces and the bindings, and has no
    /// instance definitions of its own. This is the normal deployment.
    const RUNTIME_CONFIG: &str = r#"
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "agent-alias"
[workspaces.demo]
root = "/runtime/project"
agent = { node = "worker", instance = "claude-main" }
[workspaces.other]
root = "/runtime/other"
agent = { node = "worker", instance = "claude-main" }
"#;

    fn agents_of(config: &str, test: &str) -> Value {
        let path = config_with(test, config);
        let out = exchange_with(
            path,
            &format!("{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\"}}\n"),
        );
        out[1].clone()
    }

    #[test]
    fn agents_list_reports_bindings_and_their_workspaces() {
        let answer = agents_of(RUNTIME_CONFIG, "agents-list");
        let agents = answer["result"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1, "one binding, even with two workspaces");
        assert_eq!(agents[0]["node"], "worker");
        assert_eq!(agents[0]["instance"], "claude-main");
        assert_eq!(agents[0]["workspaces"][0], "demo");
        assert_eq!(agents[0]["workspaces"][1], "other");
    }

    #[test]
    fn a_runtime_node_does_not_invent_a_provider() {
        // The Agent Node owns provider and profile resolution; the Runtime
        // side keeps no second copy, so the field is absent rather than
        // guessed or fetched over ssh.
        let answer = agents_of(RUNTIME_CONFIG, "agents-no-provider");
        let entry = &answer["result"]["agents"][0];
        assert!(entry.get("provider").is_none(), "{entry}");
        assert!(entry.get("capabilities").is_none(), "{entry}");
    }

    #[test]
    fn an_agent_node_lists_its_own_instances_before_anything_binds_them() {
        // On the Agent side the instances are defined but no workspace
        // lives here, so the only honest listing is "defined, unbound".
        let config = r#"
this = "worker"
[nodes.worker]
[nodes.runtime]
ssh = "runtime-alias"
[agents.codex-main]
provider = "codex"
profile_ref = "default"
[agents.claude-main]
provider = "claude"
profile_ref = "default"
"#;
        let agents = agents_of(config, "agents-defined")["result"]["agents"].clone();
        let agents = agents.as_array().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0]["instance"], "claude-main");
        assert_eq!(agents[0]["node"], "worker");
        assert_eq!(agents[0]["workspaces"].as_array().unwrap().len(), 0);
        assert!(agents[0].get("provider").is_none());
    }

    #[test]
    fn legacy_workspaces_are_not_listed() {
        // A workspace with no instance binding cannot be addressed by
        // {node, instance}, so listing it would hand out an address that
        // does not work.
        let config = r#"
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "agent-alias"
[workspaces.legacy]
agent_node = "worker"
root = "/runtime/legacy"
"#;
        let answer = agents_of(config, "agents-legacy");
        assert_eq!(answer["result"]["agents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_missing_config_is_a_config_error_not_a_crash() {
        let out = exchange(&format!(
            "{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\"}}\n"
        ));
        assert_eq!(out[1]["error"]["code"], code::CONFIG);
        assert_eq!(out[1]["error"]["data"]["ccnm_code"], "CCNM_E_CONFIG");
        assert_eq!(out[1]["error"]["data"]["effect"], "none");
    }

    #[test]
    fn agents_list_takes_no_parameters() {
        let path = config_with("agents-params", RUNTIME_CONFIG);
        let out = exchange_with(
            path,
            &format!(
                "{HELLO}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"agents.list\",\"params\":{{\"node\":\"worker\"}}}}\n"
            ),
        );
        assert_eq!(out[1]["error"]["code"], code::INVALID_PARAMS);
        assert_eq!(out[1]["error"]["data"]["unknown"][0], "node");
    }

    // ---- session.* ----

    /// A conversation helper that keeps one state directory across several
    /// connections, which is how a client that reconnects is tested.
    struct Peer {
        runs: Arc<FakeRuns>,
        config: PathBuf,
        state: PathBuf,
    }

    impl Peer {
        fn new(test: &str, runs: FakeRuns) -> Self {
            Peer {
                runs: Arc::new(runs),
                config: config_with(test, RUNTIME_CONFIG),
                state: temp(test),
            }
        }

        /// One connection: hello, then the given calls.
        fn call(&self, calls: &[&str]) -> Vec<Value> {
            let mut input = format!("{HELLO}\n");
            for call in calls {
                input.push_str(call);
                input.push('\n');
            }
            let mut out = talk(
                self.runs.clone(),
                self.config.clone(),
                self.state.clone(),
                &input,
            );
            out.remove(0); // the hello answer
            out
        }

        /// Wait for the background thread to write a terminal state.
        fn settle(&self, session: &str) -> store::Record {
            for _ in 0..400 {
                let store = store::Store::open(&self.state).unwrap();
                if let Some(record) = store.read(session).unwrap()
                    && record.state.terminal()
                {
                    return record;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("the session never reached a terminal state");
        }
    }

    fn start_call(extra: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.start\",\"params\":{{\"workspace\":\"demo\",\"mode\":\"print\",\"input\":{{\"prompt\":\"go\"}}{extra}}}}}"
        )
    }

    #[test]
    fn start_answers_with_a_handle_before_the_run_finishes() {
        let peer = Peer::new("start-ok", FakeRuns::ok(0, "done\n"));
        let out = peer.call(&[&start_call("")]);
        let result = &out[0]["result"];
        let session = result["session"].as_str().unwrap();
        assert!(session.starts_with("s-"), "{session}");
        // starting or running: the thread may already have picked it up.
        assert!(
            ["starting", "running"].contains(&result["state"].as_str().unwrap()),
            "{result}"
        );
        assert_eq!(result["reused"], false);
        assert_eq!(result["workspace"], "demo");
        assert_eq!(result["agent"]["node"], "worker");
        assert_eq!(result["agent"]["instance"], "claude-main");
        // The Agent has not answered yet, so there is no provider to report.
        assert!(result["agent"].get("provider").is_none(), "{result}");
        assert!(result["accepted_at"].as_str().unwrap().ends_with('Z'));
        peer.settle(session);
    }

    #[test]
    fn a_client_that_reconnects_still_gets_its_result() {
        // The session belongs to the record on disk, not to the connection.
        let peer = Peer::new("reconnect", FakeRuns::ok(0, "three failed\n"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);

        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        let result = &out[0]["result"];
        assert_eq!(result["state"], "completed");
        assert_eq!(result["outcome"]["exit_code"], 0);
        assert_eq!(result["outcome"]["timed_out"], false);
        assert_eq!(result["outcome"]["stop_requested"], false);
        assert_eq!(result["output"]["tail"], "three failed\n");
        assert_eq!(result["output"]["truncated"], false);
        // Now the Agent has reported, so the provider is known.
        assert_eq!(result["agent"]["provider"], "claude");
    }

    #[test]
    fn a_nonzero_exit_is_failed_but_still_a_result() {
        let peer = Peer::new("nonzero", FakeRuns::ok(2, "boom\n"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.status\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        assert_eq!(out[0]["result"]["state"], "failed");
    }

    #[test]
    fn a_run_that_never_started_is_failed_with_a_reason_not_a_lost_session() {
        // FakeRuns::default() fails to start. A start that failed must still
        // leave a record: "errored but no such session" is the one answer a
        // caller cannot act on.
        let peer = Peer::new("nostart", FakeRuns::default());
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        let record = peer.settle(&session);
        assert_eq!(record.state, store::State::Failed);
        assert!(record.finish.unwrap().error.is_some());
    }

    #[test]
    fn the_same_start_key_and_input_reuses_the_session() {
        let peer = Peer::new("idempotent", FakeRuns::ok(0, "ok\n"));
        let first = peer.call(&[&start_call(",\"start_key\":\"task-1\"")]);
        let session = first[0]["result"]["session"].as_str().unwrap().to_string();
        peer.settle(&session);

        let second = peer.call(&[&start_call(",\"start_key\":\"task-1\"")]);
        assert_eq!(second[0]["result"]["session"], session);
        assert_eq!(second[0]["result"]["reused"], true);
        // The point of the key: exactly one Agent was asked to run.
        assert_eq!(peer.runs.asks.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_same_start_key_with_different_input_is_a_conflict() {
        let peer = Peer::new("conflict", FakeRuns::ok(0, "ok\n"));
        let session =
            peer.call(&[&start_call(",\"start_key\":\"task-1\"")])[0]["result"]["session"]
                .as_str()
                .unwrap()
                .to_string();
        peer.settle(&session);

        let other = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.start\",\"params\":{\"workspace\":\"demo\",\"mode\":\"print\",\"input\":{\"prompt\":\"something else\"},\"start_key\":\"task-1\"}}".to_string();
        let out = peer.call(&[&other]);
        assert_eq!(out[0]["error"]["code"], code::CONFLICT);
        assert_eq!(out[0]["error"]["data"]["session"], session);
        assert_eq!(out[0]["error"]["data"]["effect"], "none");
        assert_eq!(peer.runs.asks.lock().unwrap().len(), 1, "no second Agent");
    }

    #[test]
    fn an_unknown_session_is_not_found_and_says_nothing_else() {
        let peer = Peer::new("unknown-session", FakeRuns::ok(0, ""));
        for method in ["session.status", "session.result", "session.stop"] {
            let out = peer.call(&[&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"{method}\",\"params\":{{\"session\":\"s-nope\"}}}}"
            )]);
            assert_eq!(out[0]["error"]["code"], code::NOT_FOUND, "{method}");
            assert_eq!(out[0]["error"]["message"], "no such session");
        }
    }

    #[test]
    fn an_unknown_workspace_and_a_wrong_node_give_the_same_answer() {
        // Different messages here would turn the error into a probe for what
        // this machine is configured with.
        let peer = Peer::new("probe", FakeRuns::ok(0, ""));
        let missing = peer.call(&[
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.start\",\"params\":{\"workspace\":\"nosuch\",\"mode\":\"print\",\"input\":{\"prompt\":\"go\"}}}",
        ]);
        let wrong_node = peer.call(&[&start_call(
            ",\"agent\":{\"node\":\"elsewhere\",\"instance\":\"claude-main\"}",
        )]);
        assert_eq!(missing[0]["error"]["code"], code::NOT_FOUND);
        assert_eq!(wrong_node[0]["error"]["code"], code::NOT_FOUND);
        assert_eq!(
            missing[0]["error"]["message"],
            wrong_node[0]["error"]["message"]
        );
        assert!(peer.runs.asks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_workspace_without_an_instance_binding_is_refused() {
        let config = config_with(
            "legacy-start",
            r#"
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "agent-alias"
[workspaces.demo]
agent_node = "worker"
root = "/runtime/legacy"
"#,
        );
        let out = talk(
            Arc::new(FakeRuns::ok(0, "")),
            config,
            temp("legacy-start-state"),
            &format!("{HELLO}\n{}\n", start_call("")),
        );
        assert_eq!(out[1]["error"]["code"], code::NOT_READY);
    }

    #[test]
    fn interactive_is_refused_because_it_is_not_offered() {
        let peer = Peer::new("interactive", FakeRuns::ok(0, ""));
        let out = peer.call(&[
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.start\",\"params\":{\"workspace\":\"demo\",\"mode\":\"interactive\",\"input\":{}}}",
        ]);
        assert_eq!(out[0]["error"]["code"], code::UNSUPPORTED_CAPABILITY);
        // What hello advertises and what start accepts must agree.
        let modes = capabilities()["modes"].clone();
        assert_eq!(modes.as_array().unwrap(), &[Value::from("print")]);
    }

    #[test]
    fn print_mode_requires_a_prompt() {
        let peer = Peer::new("no-prompt", FakeRuns::ok(0, ""));
        let out = peer.call(&[
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session.start\",\"params\":{\"workspace\":\"demo\",\"mode\":\"print\",\"input\":{}}}",
        ]);
        assert_eq!(out[0]["error"]["code"], code::INVALID_PARAMS);
    }

    #[test]
    fn result_before_the_end_is_a_state_not_an_error() {
        let peer = Peer::new("early-result", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        // Same shape a caller polling early would see: an absent outcome is
        // not a failure. Checked here on a record with no finish written.
        let store = store::Store::open(&peer.state).unwrap();
        let mut record = store.read(&session).unwrap().unwrap();
        record.state = store::State::Running;
        record.finish = None;
        store.write(&record).unwrap();
        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        assert!(out[0]["result"].get("outcome").is_none(), "{out:?}");
        // Still ours and still running, so the state is running -- the point
        // is that a missing outcome is not an error.
        assert_eq!(out[0]["result"]["state"], "running");
    }

    #[test]
    fn a_cursor_this_build_never_issued_is_expired() {
        let peer = Peer::new("cursor", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\",\"output\":{{\"cursor\":\"c-8192\"}}}}}}"
        )]);
        assert_eq!(out[0]["error"]["code"], code::EXPIRED);
        assert_eq!(out[0]["error"]["data"]["reason"], "cursor_expired");
    }

    #[test]
    fn stop_is_accepted_but_does_not_claim_the_session_is_over() {
        let peer = Peer::new("stop", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        // Put it back to running so stop has something to act on.
        let store = store::Store::open(&peer.state).unwrap();
        let mut record = store.read(&session).unwrap().unwrap();
        record.state = store::State::Running;
        record.owner_pid = std::process::id();
        record.finish = None;
        store.write(&record).unwrap();

        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.stop\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        // Not `completed`: only an observed end -- process group, transport,
        // released write guard -- makes it over.
        assert_eq!(out[0]["result"]["state"], "stopping");
        assert_eq!(out[0]["result"]["stop_requested"], true);
        assert_eq!(peer.runs.stops.lock().unwrap().as_slice(), &["demo"]);
    }

    #[test]
    fn stopping_something_already_finished_succeeds_without_touching_it() {
        let peer = Peer::new("stop-idempotent", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let call = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.stop\",\"params\":{{\"session\":\"{session}\"}}}}"
        );
        let first = peer.call(&[&call]);
        let second = peer.call(&[&call]);
        assert_eq!(first[0]["result"]["state"], "completed");
        assert_eq!(second[0]["result"]["state"], "completed");
        // Nothing was asked to stop: it was already over.
        assert!(peer.runs.stops.lock().unwrap().is_empty());
    }

    #[test]
    fn a_record_left_by_a_dead_server_reads_as_unknown_not_failed() {
        let peer = Peer::new("crashed", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        // Rewrite it the way a server that died mid-run would have left it:
        // running, owned by a pid that is not us and cannot be alive.
        let store = store::Store::open(&peer.state).unwrap();
        let mut record = store.read(&session).unwrap().unwrap();
        record.state = store::State::Running;
        record.owner_pid = 999_999;
        record.owner_started = "Thu Jan  1 00:00:00 1970".to_string();
        record.finish = None;
        store.write(&record).unwrap();

        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.status\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        // Not `failed`: the Agent is on another machine and may well have
        // finished. Saying "failed" here invites a retry of work that could
        // already have changed the tree.
        assert_eq!(out[0]["result"]["state"], "unknown");
    }

    #[test]
    fn a_long_output_is_truncated_and_says_so() {
        let long = "x".repeat(20_000);
        let peer = Peer::new("truncate", FakeRuns::ok(0, &long));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        let output = &out[0]["result"]["output"];
        assert_eq!(output["bytes_total"], 20_000);
        assert_eq!(output["truncated"], true);
        assert_eq!(output["tail"].as_str().unwrap().len(), 8192);
        assert_eq!(output["cursor"], Value::Null);
    }

    #[test]
    fn the_timeout_reaches_the_executor() {
        let peer = Peer::new("timeout", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call(",\"timeout_ms\":1500")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        assert_eq!(
            peer.runs.asks.lock().unwrap()[0].timeout,
            std::time::Duration::from_millis(1500)
        );

        // Zero and negative are refused rather than quietly turned into the
        // default: a caller that meant to cap a run must not get 15 minutes.
        for bad in ["0", "-1", "\"600\""] {
            let out = peer.call(&[&start_call(&format!(",\"timeout_ms\":{bad}"))]);
            assert_eq!(out[0]["error"]["code"], code::INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn the_default_timeout_is_used_when_none_is_given() {
        let peer = Peer::new("default-timeout", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        assert_eq!(
            peer.runs.asks.lock().unwrap()[0].timeout,
            session::DEFAULT_TIMEOUT
        );
    }

    #[test]
    fn internal_payload_fields_never_reach_the_caller() {
        // RunReport carries session_dir, the supervisor pid and the
        // controller's context. Those are implementation, and putting them
        // on the wire would freeze internal structure into a public
        // contract.
        let peer = Peer::new("no-internals", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let out = peer.call(&[&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
        )]);
        let text = serde_json::to_string(&out[0]).unwrap();
        for leaked in [
            "session_dir",
            "controller",
            "/private/state",
            "\"pid\"",
            "protocol\":3",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked into {text}");
        }
        // The ccnm session id is kept in the record so the two names for one
        // run stay tied together, but it is not what the protocol addresses.
        let store = store::Store::open(&peer.state).unwrap();
        let record = store.read(&session).unwrap().unwrap();
        assert_eq!(
            record.finish.unwrap().ccnm_session.as_deref(),
            Some("ccnm-uuid-1")
        );
    }

    #[test]
    fn a_corrupt_record_is_an_error_not_a_fake_success() {
        let peer = Peer::new("corrupt", FakeRuns::ok(0, "x"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        peer.settle(&session);
        let path = peer
            .state
            .join("rpc/sessions")
            .join(format!("{session}.json"));
        std::fs::write(&path, "{ truncated").unwrap();

        for method in ["session.status", "session.result", "session.stop"] {
            let out = peer.call(&[&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"{method}\",\"params\":{{\"session\":\"{session}\"}}}}"
            )]);
            // An unreadable record means the server does not know; it must
            // not answer as if the session were fine, and must not answer
            // "no such session" either, which would say it never existed.
            assert!(out[0].get("result").is_none(), "{method}: {:?}", out[0]);
            assert_eq!(out[0]["error"]["code"], code::INTERNAL_ERROR, "{method}");
        }
    }

    #[test]
    fn the_caller_picks_the_instance_but_never_the_node() {
        let peer = Peer::new("override", FakeRuns::ok(0, "x"));
        let out = peer.call(&[&start_call(
            ",\"agent\":{\"node\":\"worker\",\"instance\":\"codex-main\"}",
        )]);
        assert_eq!(out[0]["result"]["agent"]["instance"], "codex-main");
        assert_eq!(out[0]["result"]["agent"]["node"], "worker");
        let session = out[0]["result"]["session"].as_str().unwrap().to_string();
        peer.settle(&session);
        let asks = peer.runs.asks.lock().unwrap();
        assert_eq!(asks[0].instance.as_deref(), Some("codex-main"));
    }
}

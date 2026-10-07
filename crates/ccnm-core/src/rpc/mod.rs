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

pub mod output;
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
        "output_streams": ["stdout", "stderr"],
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
    /// Output pages this process handed out (P59). Lives and dies with the
    /// process: a cursor from before a restart is `expired`.
    pub cursors: output::Cursors,
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
    use ccnm_testdir::TestDir;
    use session::{RunAsk, Runs};
    use std::sync::{Arc, Mutex};

    /// A fresh directory every call: tests run in parallel and two of them
    /// sharing a state directory would see each other's session records.
    fn temp(test: &str) -> TestDir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ccnm-rpc-{}-{test}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    /// Stands in for the launcher. No ssh, no Agent, no controller: these
    /// tests are about the protocol and the record, and a test that needs a
    /// real Agent is not an offline test.
    ///
    /// With `hold`, a run waits until the test (or a stop) releases its
    /// session, which is how the P58 windows -- stop while running, stop
    /// racing the end -- are driven without sleeping and hoping.
    #[derive(Default)]
    struct FakeRuns {
        /// What `run_print` answers. `None` means it fails with `fail`.
        report: Option<crate::protocol::run::RunReport>,
        fail: Option<crate::ErrorCode>,
        hold: bool,
        released: Mutex<std::collections::HashSet<String>>,
        wake: std::sync::Condvar,
        /// Sessions a stop ended, the way the Agent ends them: killed, no
        /// exit code.
        killed: Mutex<std::collections::HashSet<String>>,
        /// A stop only records itself instead of ending the run.
        stop_leaves_it_running: bool,
        /// Before answering a stop, wait until this record says the run is
        /// over: "the end lands before the stop" as a fixed order.
        stop_waits_for: Mutex<Option<PathBuf>>,
        asks: Mutex<Vec<RunAsk>>,
        stops: Mutex<Vec<session::StopAsk>>,
        /// The Agent's retained views, by stream: what `output` serves.
        views: Mutex<std::collections::HashMap<crate::session::view::Stream, Vec<u8>>>,
        /// `output` fails with this, from the n-th call on (0 = always).
        output_fails: Mutex<Option<(crate::ErrorCode, usize)>>,
        /// The generation `output` reports.
        generation: Mutex<String>,
        /// From this call on, `output` reports a different generation: the
        /// view changing under a copy in progress.
        shift_after: Mutex<Option<usize>>,
        outputs: Mutex<Vec<session::OutputAsk>>,
        /// What the Runtime says about the write guard; `None` is free.
        guard_seen: Mutex<
            Option<(
                crate::mcp::write_guard::Observed,
                crate::mcp::write_guard::Reason,
            )>,
        >,
        /// `guard` cannot get an answer at all, with this.
        guard_fails: Mutex<Option<crate::ErrorCode>>,
        guards: Mutex<Vec<session::GuardAsk>>,
    }

    impl FakeRuns {
        fn ok(exit_code: i32, text: &str) -> Self {
            // What the Agent retained holds at least the tail its report
            // carried; here, exactly that.
            let views = std::collections::HashMap::from([(
                crate::session::view::Stream::Stdout,
                text.as_bytes().to_vec(),
            )]);
            FakeRuns {
                report: Some(report(exit_code, text)),
                views: Mutex::new(views),
                ..FakeRuns::default()
            }
        }

        fn holding(exit_code: i32) -> Self {
            FakeRuns {
                hold: true,
                ..FakeRuns::ok(exit_code, "held\n")
            }
        }

        fn failing(code: crate::ErrorCode) -> Self {
            FakeRuns {
                fail: Some(code),
                ..FakeRuns::default()
            }
        }

        fn release(&self, session: &str) {
            self.released.lock().unwrap().insert(session.to_string());
            self.wake.notify_all();
        }

        /// The managed id of the n-th run that reached the "Agent".
        fn started(&self, n: usize) -> String {
            for _ in 0..1000 {
                if let Some(ask) = self.asks.lock().unwrap().get(n) {
                    return ask.session.clone();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("run {n} never reached the Agent");
        }
    }

    impl Runs for FakeRuns {
        fn run_print(&self, ask: &RunAsk) -> crate::error::Result<crate::protocol::run::RunReport> {
            self.asks.lock().unwrap().push(ask.clone());
            if self.hold {
                let released = self.released.lock().unwrap();
                let (released, timeout) = self
                    .wake
                    .wait_timeout_while(released, std::time::Duration::from_secs(20), |r| {
                        !r.contains(&ask.session)
                    })
                    .unwrap();
                assert!(!timeout.timed_out(), "a held run was never released");
                drop(released);
            }
            let Some(mut report) = self.report.clone() else {
                return Err(crate::Error::new(
                    self.fail.unwrap_or(crate::ErrorCode::NotReady),
                    "no route",
                ));
            };
            // The real Agent runs under the id it was given.
            report.session = ask.session.clone();
            if self.killed.lock().unwrap().contains(&ask.session) {
                report.outcome.exit_code = None;
            }
            Ok(report)
        }

        fn output(
            &self,
            ask: &session::OutputAsk,
        ) -> crate::error::Result<crate::protocol::run::OutputReport> {
            use base64::Engine as _;
            let n = {
                let mut outputs = self.outputs.lock().unwrap();
                outputs.push(ask.clone());
                outputs.len() - 1
            };
            if let Some((code, from)) = *self.output_fails.lock().unwrap()
                && n >= from
            {
                return Err(crate::Error::new(code, "no output here"));
            }
            let views = self.views.lock().unwrap();
            let view = views.get(&ask.stream).cloned().unwrap_or_default();
            let start = (ask.offset as usize).min(view.len());
            let end = (start + ask.limit as usize).min(view.len());
            Ok(crate::protocol::run::OutputReport {
                protocol: crate::instance::OUTPUT_PROTOCOL,
                agent_identity: crate::instance::AgentIdentity {
                    node: ask.node.clone(),
                    instance: ask.instance.clone(),
                    provider: crate::provider::AgentProvider::Claude,
                    profile_ref: "default".into(),
                },
                session: ask.session.clone(),
                stream: ask.stream,
                generation: match *self.shift_after.lock().unwrap() {
                    Some(from) if n >= from => "shifted".into(),
                    _ => self.generation.lock().unwrap().clone(),
                },
                view_bytes: view.len() as u64,
                source_bytes: view.len() as u64 + 7,
                source_truncated: true,
                offset: ask.offset,
                data: base64::engine::general_purpose::STANDARD.encode(&view[start..end]),
            })
        }

        fn stop(&self, ask: &session::StopAsk) -> crate::error::Result<bool> {
            self.stops.lock().unwrap().push(ask.clone());
            if !self.stop_leaves_it_running {
                if self.stop_waits_for.lock().unwrap().is_none() {
                    self.killed.lock().unwrap().insert(ask.session.clone());
                }
                self.release(&ask.session);
            }
            if let Some(path) = self.stop_waits_for.lock().unwrap().clone() {
                for _ in 0..2000 {
                    let over = std::fs::read(&path)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<store::Record>(&b).ok())
                        .is_some_and(|r| r.state.terminal());
                    if over {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            Ok(true)
        }

        fn guard(
            &self,
            ask: &session::GuardAsk,
        ) -> crate::error::Result<crate::protocol::run::AgentGuardReport> {
            use crate::mcp::write_guard::{
                Observation, Observed, Owner, Reason, Resource, ResourceKind,
            };
            self.guards.lock().unwrap().push(ask.clone());
            if let Some(code) = *self.guard_fails.lock().unwrap() {
                return Err(crate::Error::new(code, "no answer about the guard"));
            }
            let (state, reason) = self
                .guard_seen
                .lock()
                .unwrap()
                .unwrap_or((Observed::Free, Reason::Released));
            Ok(crate::protocol::run::AgentGuardReport {
                protocol: crate::runtime::GUARD_PROTOCOL,
                agent_identity: crate::instance::AgentIdentity {
                    node: "worker".into(),
                    instance: ask.instance.clone().unwrap_or("claude-main".into()),
                    provider: crate::provider::AgentProvider::Claude,
                    profile_ref: "default".into(),
                },
                runtime: crate::runtime::GuardReport {
                    protocol: crate::runtime::GUARD_PROTOCOL,
                    workspace: ask.workspace.clone(),
                    observation: Observation {
                        state,
                        reason,
                        resource: Resource {
                            kind: ResourceKind::GitCommonDir,
                            id: "0123456789abcdef".into(),
                        },
                        owner: (state != Observed::Free).then(|| Owner {
                            session: "11111111-2222-4333-8444-555555555555".into(),
                            workspace: Some(ask.workspace.clone()),
                            pid: Some(4242),
                            process: None,
                            legacy: false,
                        }),
                        leftovers: None,
                        observed_at: 0,
                    },
                },
                owner_on_agent: None,
            })
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
                platform: None,
                linger: None,
            },
            pid: 4242,
            outcome: crate::session::Outcome {
                exit_code: Some(exit_code),
                timed_out: false,
                duration_ms: 1234,
                error: None,
                stopped: false,
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
                cursors: Default::default(),
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

    /// A config file with whatever body the test needs, and the directory
    /// it lives in: the file is gone once that is dropped.
    fn config_with(test: &str, body: &str) -> (TestDir, PathBuf) {
        let dir = temp(test);
        let path = dir.join("config.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    /// Feed lines in, get answered lines back.
    fn exchange(input: &str) -> Vec<Value> {
        exchange_with(PathBuf::from("/nonexistent/ccnm/config.toml"), input)
    }

    fn exchange_with(config_path: PathBuf, input: &str) -> Vec<Value> {
        let state = temp("exchange-state");
        talk(
            Arc::new(FakeRuns::default()),
            config_path,
            state.to_path_buf(),
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
        let state = temp("utf8-state");
        serve(
            Context {
                config_path: PathBuf::from("/nonexistent/ccnm/config.toml"),
                state: state.to_path_buf(),
                runs: Arc::new(FakeRuns::default()),
                runner: Arc::new(SystemRunner),
                cursors: Default::default(),
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
        let (_dir, path) = config_with(test, config);
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
        let (_dir, path) = config_with("agents-params", RUNTIME_CONFIG);
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
        _config_dir: TestDir,
        state: TestDir,
    }

    impl Peer {
        fn new(test: &str, runs: FakeRuns) -> Self {
            let (config_dir, config) = config_with(test, RUNTIME_CONFIG);
            Peer {
                runs: Arc::new(runs),
                config,
                _config_dir: config_dir,
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
                self.state.to_path_buf(),
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

    fn whole_result(session: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
        )
    }

    /// F3. On the P62 machines a session whose Agent never came up -- the
    /// CLI there was logged out, or the two ends were different builds --
    /// reached the caller as a bare `failed`: exit code null, text null, no
    /// output. The reason existed, but only in the Operator's own record.
    /// `failure` hands it over in the vocabulary the caller already
    /// branches on: the error this start would have been refused with.
    #[test]
    fn a_session_that_never_started_says_why_in_its_result() {
        for (refused, state, wire_code, name) in [
            (crate::ErrorCode::Auth, "failed", code::AUTH, "CCNM_E_AUTH"),
            (
                crate::ErrorCode::Version,
                "failed",
                code::VERSION_MISMATCH,
                "CCNM_E_VERSION",
            ),
            (
                crate::ErrorCode::RuntimeUnreachable,
                "failed",
                code::RUNTIME_UNREACHABLE,
                "CCNM_E_RUNTIME_UNREACHABLE",
            ),
            // Lost rather than refused: the reason is then why the server
            // cannot tell, and the state stays `unknown`.
            (
                crate::ErrorCode::AgentUnreachable,
                "unknown",
                code::AGENT_UNREACHABLE,
                "CCNM_E_AGENT_UNREACHABLE",
            ),
        ] {
            let peer = Peer::new(&format!("why-{name}"), FakeRuns::failing(refused));
            let session = handle_of(&peer.call(&[&start_call("")]));
            peer.settle(&session);
            let out = peer.call(&[&whole_result(&session)]);
            let result = &out[0]["result"];
            assert_eq!(result["state"], state, "{out:?}");
            assert_eq!(result["failure"]["code"], wire_code, "{out:?}");
            assert_eq!(result["failure"]["ccnm_code"], name, "{out:?}");
            assert_eq!(result["failure"]["detail"], "no route", "{out:?}");
            // The process-level outcome is still there and still says
            // nothing ran to an exit.
            assert_eq!(result["outcome"]["exit_code"], Value::Null, "{out:?}");
        }
    }

    /// `failure` is for a session that did not end with the Agent's own
    /// exit. One that did -- whatever the exit code -- has none, and neither
    /// does one a stop ended: those are what `outcome` is for.
    #[test]
    fn a_session_that_ran_to_an_exit_has_no_failure() {
        for exit_code in [0, 3] {
            let peer = Peer::new(&format!("ran-{exit_code}"), FakeRuns::ok(exit_code, "x"));
            let session = handle_of(&peer.call(&[&start_call("")]));
            peer.settle(&session);
            let out = peer.call(&[&whole_result(&session)]);
            assert!(out[0]["result"].get("failure").is_none(), "{out:?}");
            assert_eq!(out[0]["result"]["outcome"]["exit_code"], exit_code);
        }
    }

    /// Two things about the reason's text. A record written before P65 kept
    /// the message and not its code, so it has a `detail` and nothing to
    /// branch on. And no field may carry an absolute path into somebody's
    /// home (contract section 9): the message is the Agent's or ssh's own
    /// words, and those name profile and state directories.
    #[test]
    fn an_older_record_gives_only_the_detail_and_home_paths_are_not_passed_on() {
        let peer = Peer::new("why-legacy", FakeRuns::failing(crate::ErrorCode::Auth));
        let session = handle_of(&peer.call(&[&start_call("")]));
        peer.settle(&session);
        let path = peer
            .state
            .join("rpc/sessions")
            .join(format!("{session}.json"));
        let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let finish = record["finish"].as_object_mut().unwrap();
        finish.remove("error_code");
        finish.insert(
            "error".into(),
            Value::from(
                "profile /Users/someone/.config/ccnm/agents/codex is not private\nsee /home/ccrun/.local/state/ccnm/x and \"/Users/a b/c\"; /usr/bin/ssh is fine",
            ),
        );
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

        let out = peer.call(&[&whole_result(&session)]);
        let failure = &out[0]["result"]["failure"];
        assert!(failure.get("code").is_none(), "{out:?}");
        assert!(failure.get("ccnm_code").is_none(), "{out:?}");
        assert_eq!(
            failure["detail"],
            "profile ~/.config/ccnm/agents/codex is not private\nsee ~/.local/state/ccnm/x and \"~ b/c\"; /usr/bin/ssh is fine",
            "{out:?}"
        );
    }

    // ---- P60：启动前问 Runtime 的写锁 ----

    /// Runtime 说另一个 writer 正持锁：-32008、什么都没起、没留记录、键也没占。
    #[test]
    fn a_tree_the_runtime_says_is_held_is_busy_and_nothing_starts() {
        use crate::mcp::write_guard::{Observed, Reason};
        let runs = FakeRuns::ok(0, "ok\n");
        *runs.guard_seen.lock().unwrap() = Some((Observed::Held, Reason::LiveHolder));
        let peer = Peer::new("busy", runs);
        let out = peer.call(&[&start_call(",\"start_key\":\"task-busy\"")]);
        let error = &out[0]["error"];
        assert_eq!(error["code"], code::BUSY, "{out:?}");
        assert_eq!(error["data"]["effect"], "none");
        assert_eq!(error["data"]["reason"], "live_holder");
        assert!(error["data"].get("session").is_none(), "no handle was made");
        assert!(
            peer.runs.asks.lock().unwrap().is_empty(),
            "nothing reached the Agent"
        );
        assert_eq!(
            peer.runs.guards.lock().unwrap()[0],
            session::GuardAsk {
                workspace: "demo".into(),
                instance: None,
            }
        );
        // 键没被占：树空出来以后，同一个 start 是新任务，不是复用一个没跑过的记录。
        *peer.runs.guard_seen.lock().unwrap() = None;
        let again = peer.call(&[&start_call(",\"start_key\":\"task-busy\"")]);
        assert_eq!(again[0]["result"]["reused"], false, "{again:?}");
        peer.settle(again[0]["result"]["session"].as_str().unwrap());
        let records = std::fs::read_dir(peer.state.join("rpc/sessions"))
            .unwrap()
            .count();
        assert_eq!(records, 1, "the refused start left no record");
    }

    /// 没人持锁、却也不是空闲：要人来看，不是等一等再试的 busy。
    #[test]
    fn a_guard_nobody_can_hand_over_is_policy_not_busy() {
        use crate::mcp::write_guard::{Observed, Reason};
        for (state, reason, name) in [
            (
                Observed::Abandoned,
                Reason::KeptOnPurpose,
                "kept_on_purpose",
            ),
            (Observed::Unknown, Reason::LeftHeld, "left_held"),
            (
                Observed::Unknown,
                Reason::MalformedMarker,
                "malformed_marker",
            ),
            (Observed::Unknown, Reason::Unreadable, "unreadable"),
            (
                Observed::Unknown,
                Reason::LockQueryFailed,
                "lock_query_failed",
            ),
        ] {
            let runs = FakeRuns::ok(0, "ok\n");
            *runs.guard_seen.lock().unwrap() = Some((state, reason));
            let peer = Peer::new("policy", runs);
            let out = peer.call(&[&start_call("")]);
            assert_eq!(out[0]["error"]["code"], code::POLICY, "{name}: {out:?}");
            assert_eq!(out[0]["error"]["data"]["effect"], "none");
            assert_eq!(out[0]["error"]["data"]["reason"], name);
            assert!(peer.runs.asks.lock().unwrap().is_empty(), "{name}");
        }
    }

    /// 问不到不是空闲，也不是占用：不下结论，照 P59 的样子启动，由会话打开时的
    /// guard 裁决。
    #[test]
    fn no_answer_about_the_guard_is_no_verdict() {
        for fails in [
            crate::ErrorCode::AgentUnreachable,
            crate::ErrorCode::RuntimeUnreachable,
            crate::ErrorCode::Version,
        ] {
            let runs = FakeRuns::ok(0, "ok\n");
            *runs.guard_fails.lock().unwrap() = Some(fails);
            let peer = Peer::new("no-verdict", runs);
            let out = peer.call(&[&start_call("")]);
            let session = out[0]["result"]["session"]
                .as_str()
                .unwrap_or_else(|| panic!("{fails:?}: {out:?}"));
            peer.settle(session);
            assert_eq!(peer.runs.asks.lock().unwrap().len(), 1, "{fails:?}");
        }
    }

    /// 同一个 start_key 的重查先于预检：占着锁的正是它自己的 writer，也得把
    /// 原会话还回去，而不是回 busy。
    #[test]
    fn a_taken_start_key_is_answered_before_the_guard_is_asked() {
        use crate::mcp::write_guard::{Observed, Reason};
        let peer = Peer::new("key-first", FakeRuns::ok(0, "ok\n"));
        let first = peer.call(&[&start_call(",\"start_key\":\"task-9\"")]);
        let session = first[0]["result"]["session"].as_str().unwrap().to_string();
        assert_eq!(peer.runs.guards.lock().unwrap().len(), 1);
        *peer.runs.guard_seen.lock().unwrap() = Some((Observed::Held, Reason::LiveHolder));
        let again = peer.call(&[&start_call(",\"start_key\":\"task-9\"")]);
        assert_eq!(again[0]["result"]["session"], session.as_str(), "{again:?}");
        assert_eq!(again[0]["result"]["reused"], true);
        let other =
            peer.call(&[&start_call(",\"start_key\":\"task-9\"").replace("\"go\"", "\"else\"")]);
        assert_eq!(other[0]["error"]["code"], code::CONFLICT, "{other:?}");
        assert_eq!(
            peer.runs.guards.lock().unwrap().len(),
            1,
            "a taken key never asks the Runtime"
        );
        peer.settle(&session);
    }

    /// 清理过的会话：result 回 expired（不是 not_found），status 照常，同一个
    /// start_key 仍然指回它、不重跑（P61 CL-06）。
    #[test]
    fn a_cleaned_session_is_expired_but_still_known_and_never_rerun() {
        let peer = Peer::new("cleaned", FakeRuns::ok(0, "answer\n"));
        let first = peer.call(&[&start_call(",\"start_key\":\"task-c\"")]);
        let session = first[0]["result"]["session"].as_str().unwrap().to_string();
        peer.settle(&session);
        let store = store::Store::open(&peer.state).unwrap();
        assert_eq!(
            store.clean(&session, |_| true).unwrap(),
            Some(store::Cleaned::Done)
        );

        let out = peer.call(&[
            &format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
            ),
            &format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"session.status\",\"params\":{{\"session\":\"{session}\"}}}}"
            ),
            &start_call(",\"start_key\":\"task-c\""),
        ]);
        let error = &out[0]["error"];
        assert_eq!(error["code"], code::EXPIRED, "{out:?}");
        assert_eq!(error["data"]["reason"], "cleaned");
        assert_eq!(error["data"]["session"], session.as_str());
        assert_eq!(out[1]["result"]["state"], "completed");
        assert_eq!(out[2]["result"]["session"], session.as_str());
        assert_eq!(out[2]["result"]["reused"], true);
        assert_eq!(peer.runs.asks.lock().unwrap().len(), 1, "never run again");
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
        let (_dir, config) = config_with(
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
        let state = temp("legacy-start-state");
        let out = talk(
            Arc::new(FakeRuns::ok(0, "")),
            config,
            state.to_path_buf(),
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
        // Addressed by the session the run was sent under, not by workspace.
        assert_eq!(
            peer.runs.stops.lock().unwrap().as_slice(),
            &[session::StopAsk {
                workspace: "demo".into(),
                node: "worker".into(),
                instance: "claude-main".into(),
                session: record.managed_session.clone().unwrap(),
            }]
        );
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

    /// `ps`, answered at the worst possible moment: by the time it says the
    /// owner's pid is gone, the owner has written how the session ended.
    struct OwnerFinishedMeanwhile {
        state: PathBuf,
        ended: store::Record,
    }

    impl crate::process::ProcessRunner for OwnerFinishedMeanwhile {
        fn run(&self, _cmd: &crate::process::Cmd) -> crate::Result<crate::process::Output> {
            store::Store::open(&self.state)
                .unwrap()
                .write(&self.ended)
                .unwrap();
            // What `ps -p <pid>` says about a pid that no longer exists.
            Ok(crate::process::Output::exited(1, ""))
        }
    }

    /// An owner writes the outcome and then exits. A reader that loaded the
    /// record just before that and asked `ps` just after used to put the two
    /// together as "still running, owner gone" and answer `unknown` -- for a
    /// session that had ended normally and whose record already said so.
    /// Since P63 every session has an owner process of its own that exits
    /// the moment it is done, so every session passes through that window;
    /// a caller polling `session.status` hit it about once in forty runs.
    /// `unknown` is a terminal answer that tells the caller not to retry.
    #[test]
    fn an_owner_that_finished_between_the_read_and_the_ps_is_not_unknown() {
        let peer = Peer::new("owner-finished-meanwhile", FakeRuns::ok(0, "done"));
        let session = peer.call(&[&start_call("")])[0]["result"]["session"]
            .as_str()
            .unwrap()
            .to_string();
        let ended = peer.settle(&session);
        assert_eq!(ended.state, store::State::Completed);

        let calls = [
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.status\",\"params\":{{\"session\":\"{session}\"}}}}"
            ),
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\"}}}}"
            ),
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"session.stop\",\"params\":{{\"session\":\"{session}\"}}}}"
            ),
        ];
        for call in &calls {
            // Put the record back to what the reader sees first: running,
            // owned by another process.
            let store = store::Store::open(&peer.state).unwrap();
            let mut running = ended.clone();
            running.state = store::State::Running;
            running.owner_pid = 999_999;
            running.owner_started = "Thu Jan  1 00:00:00 1970".to_string();
            running.finish = None;
            store.write(&running).unwrap();
            let mut finished = ended.clone();
            finished.owner_pid = running.owner_pid;
            finished.owner_started = running.owner_started.clone();

            let mut out = Vec::new();
            serve(
                Context {
                    config_path: peer.config.clone(),
                    state: peer.state.to_path_buf(),
                    runs: peer.runs.clone(),
                    runner: Arc::new(OwnerFinishedMeanwhile {
                        state: peer.state.to_path_buf(),
                        ended: finished,
                    }),
                    cursors: Default::default(),
                },
                std::io::BufReader::new(format!("{HELLO}\n{call}\n").as_bytes()),
                &mut out,
            )
            .unwrap();
            let answers: Vec<Value> = String::from_utf8(out)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                answers[1]["result"]["state"], "completed",
                "{call}\n{answers:?}"
            );
        }
        // Nothing was asked to stop: by the time anyone looked, it was over.
        assert!(peer.runs.stops.lock().unwrap().is_empty());
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
        // Since P59 the rest is there to page back to, not dropped.
        assert!(
            output["cursor"].as_str().unwrap().starts_with("c-"),
            "{output}"
        );
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
        // Since P58 it is chosen here before the run is sent, and the Agent's
        // answer carries the same one back.
        let store = store::Store::open(&peer.state).unwrap();
        let record = store.read(&session).unwrap().unwrap();
        let managed = record.managed_session.clone().unwrap();
        assert!(crate::session::valid_id(&managed), "{managed}");
        assert!(!text.contains(&managed), "the managed id is internal");
        assert_eq!(record.finish.unwrap().ccnm_session, Some(managed));
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

    // ---- P58: exact control, atomic state, keys and handles ----

    fn stop_line(session: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.stop\",\"params\":{{\"session\":\"{session}\"}}}}"
        )
    }

    fn status_line(session: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session.status\",\"params\":{{\"session\":\"{session}\"}}}}"
        )
    }

    fn handle_of(out: &[Value]) -> String {
        out[0]["result"]["session"].as_str().unwrap().to_string()
    }

    /// CT-01: two runs on one workspace; the stop for B names B's session on
    /// the Agent and nothing else, and A keeps running.
    #[test]
    fn ct01_a_stop_names_exactly_its_own_session() {
        let peer = Peer::new("ct01", FakeRuns::holding(0));
        let a = handle_of(&peer.call(&[&start_call("")]));
        let managed_a = peer.runs.started(0);
        let b = handle_of(&peer.call(&[&start_call("")]));
        let managed_b = peer.runs.started(1);
        assert_ne!(managed_a, managed_b);

        let out = peer.call(&[&stop_line(&b)]);
        // The fake lets the run end the moment it is asked to stop, so the
        // run can be on record as over before the stop answers -- about one
        // time in ten with 64 test threads. Both answers are the contract's;
        // what must hold either way is that the request is on record.
        let answered = out[0]["result"]["state"].as_str().unwrap();
        assert!(["stopping", "failed"].contains(&answered), "{out:?}");
        assert_eq!(out[0]["result"]["stop_requested"], true, "{out:?}");
        let stops = peer.runs.stops.lock().unwrap().clone();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].session, managed_b);

        let ended_b = peer.settle(&b);
        assert_eq!(ended_b.state, store::State::Failed);
        assert!(ended_b.stop_requested);
        assert_eq!(
            peer.call(&[&status_line(&a)])[0]["result"]["state"],
            "running"
        );
        peer.runs.release(&managed_a);
        let ended_a = peer.settle(&a);
        assert_eq!(ended_a.state, store::State::Completed);
        assert!(!ended_a.stop_requested);
    }

    /// CT-02: a stop that lands before the run is sent keeps it from ever
    /// being sent, and asks the Agent nothing.
    #[test]
    fn ct02_a_stop_before_the_run_is_sent_means_it_is_never_sent() {
        let peer = Peer::new("ct02", FakeRuns::ok(0, "x"));
        let store = store::Store::open(&peer.state).unwrap();
        let managed = crate::session::new_id();
        let record = store::Record {
            session: "s-ct02".into(),
            launch: store::Launch {
                workspace: "demo".into(),
                agent: Some(crate::instance::InstanceRef {
                    node: "worker".into(),
                    instance: "claude-main".into(),
                }),
                mode: "print".into(),
                prompt: "go".into(),
            },
            start_key: None,
            state: store::State::Starting,
            accepted_at: "2026-09-28T00:00:00Z".into(),
            stop_requested: false,
            timeout_ms: None,
            owner_pid: std::process::id(),
            owner_started: String::new(),
            managed_session: Some(managed.clone()),
            dispatched: false,
            finish: None,
            cleaned_at: None,
        };
        store.create(&record).unwrap();
        let out = peer.call(&[&stop_line("s-ct02")]);
        assert_eq!(out[0]["result"]["state"], "stopping");
        assert!(
            peer.runs.stops.lock().unwrap().is_empty(),
            "nothing was sent, so nothing to stop there"
        );

        // The thread's turn comes after the stop.
        session::run_to_end(
            peer.runs.as_ref(),
            &store,
            "s-ct02",
            &RunAsk {
                workspace: "demo".into(),
                instance: None,
                prompt: "go".into(),
                timeout: session::DEFAULT_TIMEOUT,
                session: managed,
            },
        );
        assert!(
            peer.runs.asks.lock().unwrap().is_empty(),
            "a stopped start must not be sent"
        );
        let ended = store.read("s-ct02").unwrap().unwrap();
        assert_eq!(ended.state, store::State::Failed);
        assert!(ended.stop_requested);
        assert!(!ended.dispatched);

        // The caller is told why there is no exit code (F3). This reason is
        // none of the protocol's error codes, so it is a sentence and
        // nothing to branch on -- `stop_requested` is what says who did it.
        let out = peer.call(&[&whole_result("s-ct02")]);
        let result = &out[0]["result"];
        assert_eq!(
            result["failure"],
            serde_json::json!({"detail": "stopped before it was sent to the Agent"}),
            "{out:?}"
        );
        assert_eq!(result["outcome"]["stop_requested"], true);
    }

    /// CT-03: a stop recorded while the run is going survives the run's own
    /// write of how it ended.
    #[test]
    fn ct03_the_stop_flag_survives_the_end_of_the_run() {
        let peer = Peer::new(
            "ct03-flag",
            FakeRuns {
                stop_leaves_it_running: true,
                ..FakeRuns::holding(0)
            },
        );
        let s = handle_of(&peer.call(&[&start_call("")]));
        let managed = peer.runs.started(0);
        assert_eq!(
            peer.call(&[&stop_line(&s)])[0]["result"]["stop_requested"],
            true
        );
        peer.runs.release(&managed);
        let ended = peer.settle(&s);
        assert_eq!(ended.state, store::State::Completed, "it ended on its own");
        assert!(ended.stop_requested, "and the stop is still on record");
    }

    /// CT-03: when the end is written first, the stop does not turn it back
    /// into `stopping` or drop the result.
    #[test]
    fn ct03_an_end_written_before_the_stop_is_kept() {
        let peer = Peer::new("ct03-order", FakeRuns::holding(0));
        let s = handle_of(&peer.call(&[&start_call("")]));
        peer.runs.started(0);
        *peer.runs.stop_waits_for.lock().unwrap() =
            Some(peer.state.join("rpc/sessions").join(format!("{s}.json")));
        let out = peer.call(&[&stop_line(&s)]);
        assert_eq!(out[0]["result"]["state"], "completed", "{out:?}");
        let ended = store::Store::open(&peer.state)
            .unwrap()
            .read(&s)
            .unwrap()
            .unwrap();
        assert_eq!(ended.state, store::State::Completed);
        assert!(
            ended.finish.is_some(),
            "the result the Agent sent back is kept"
        );
        assert!(ended.stop_requested);
    }

    /// CT-04 at the protocol level: two keys the old layout merged are two
    /// tasks.
    #[test]
    fn ct04_distinct_keys_are_distinct_tasks() {
        let peer = Peer::new("ct04", FakeRuns::ok(0, "x"));
        let first = handle_of(&peer.call(&[&start_call(",\"start_key\":\"任务-一\"")]));
        let second = handle_of(&peer.call(&[&start_call(",\"start_key\":\"任务-二\"")]));
        assert_ne!(first, second);
        let again = peer.call(&[&start_call(",\"start_key\":\"任务-二\"")]);
        assert_eq!(handle_of(&again), second);
        assert_eq!(again[0]["result"]["reused"], true);
        peer.settle(&first);
        peer.settle(&second);
        assert_eq!(peer.runs.asks.lock().unwrap().len(), 2);
        // The record of the start that lost the key was taken back.
        let records = std::fs::read_dir(peer.state.join("rpc/sessions"))
            .unwrap()
            .count();
        assert_eq!(records, 2);
    }

    /// CT-05: an old half-written key is "cannot tell", with no empty handle
    /// in the answer and no run.
    #[test]
    fn ct05_a_half_written_old_key_is_uncertain_and_starts_nothing() {
        let peer = Peer::new("ct05", FakeRuns::ok(0, "x"));
        let keys = peer.state.join("rpc/keys/demo");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("k-empty"), "").unwrap();
        let out = peer.call(&[&start_call(",\"start_key\":\"k-empty\"")]);
        assert_eq!(out[0]["error"]["code"], code::UNCERTAIN);
        assert_eq!(out[0]["error"]["data"]["effect"], "unknown");
        assert!(out[0]["error"]["data"].get("session").is_none(), "{out:?}");
        assert!(peer.runs.asks.lock().unwrap().is_empty());
    }

    /// CT-06: a handle this server could not have issued is refused before
    /// any file is read, by every method.
    #[test]
    fn ct06_handles_outside_the_issued_shape_are_invalid_params() {
        let peer = Peer::new("ct06", FakeRuns::ok(0, "x"));
        for handle in ["../outside", "/etc/passwd", "a/b", ".hidden", "-x"] {
            for method in ["session.status", "session.result", "session.stop"] {
                let out = peer.call(&[&format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"{method}\",\"params\":{{\"session\":\"{handle}\"}}}}"
                )]);
                assert_eq!(
                    out[0]["error"]["code"],
                    code::INVALID_PARAMS,
                    "{method} {handle}"
                );
            }
        }
    }

    /// CT-07: rebinding the workspace to another node does not carry an old
    /// handle's stop to the new machine, and does not record a stop that was
    /// never sent.
    #[test]
    fn ct07_an_old_handle_does_not_follow_a_rebound_workspace() {
        let peer = Peer::new("ct07", FakeRuns::holding(0));
        let s = handle_of(&peer.call(&[&start_call("")]));
        let managed = peer.runs.started(0);
        std::fs::write(
            &peer.config,
            r#"
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "agent-alias"
[nodes.worker2]
ssh = "agent2-alias"
[workspaces.demo]
root = "/runtime/project"
agent = { node = "worker2", instance = "claude-main" }
"#,
        )
        .unwrap();
        let out = peer.call(&[&stop_line(&s)]);
        assert_eq!(out[0]["error"]["code"], code::POLICY, "{out:?}");
        assert_eq!(out[0]["error"]["data"]["effect"], "none");
        assert!(peer.runs.stops.lock().unwrap().is_empty());
        assert_eq!(
            peer.call(&[&status_line(&s)])[0]["result"]["stop_requested"],
            false
        );
        peer.runs.release(&managed);
        peer.settle(&s);
    }

    /// CT-08: after the run was sent, only a refusal the Agent gave before
    /// starting anything is `failed`. A broken connection or an internal
    /// failure may have left a run behind: `unknown`.
    #[test]
    fn ct08_an_unclear_end_is_unknown_not_failed() {
        for (code, expected) in [
            (crate::ErrorCode::AgentUnreachable, store::State::Unknown),
            (crate::ErrorCode::Internal, store::State::Unknown),
            (crate::ErrorCode::NotReady, store::State::Failed),
            (crate::ErrorCode::Auth, store::State::Failed),
            (crate::ErrorCode::Policy, store::State::Failed),
            (crate::ErrorCode::Version, store::State::Failed),
        ] {
            let peer = Peer::new(&format!("ct08-{}", code.name()), FakeRuns::failing(code));
            let s = handle_of(&peer.call(&[&start_call("")]));
            assert_eq!(peer.settle(&s).state, expected, "{code:?}");
        }
    }

    /// A running record from before P58 carries no Agent-side id. It is not
    /// stopped by workspace as a fallback; it is refused, with nothing sent.
    #[test]
    fn a_session_from_before_p58_is_not_stopped_by_guess() {
        let peer = Peer::new("legacy-stop", FakeRuns::ok(0, "x"));
        let s = handle_of(&peer.call(&[&start_call("")]));
        peer.settle(&s);
        let store = store::Store::open(&peer.state).unwrap();
        let mut record = store.read(&s).unwrap().unwrap();
        record.state = store::State::Running;
        record.finish = None;
        record.managed_session = None;
        record.dispatched = false;
        store.write(&record).unwrap();
        let out = peer.call(&[&stop_line(&s)]);
        assert_eq!(out[0]["error"]["code"], code::NOT_READY, "{out:?}");
        assert!(peer.runs.stops.lock().unwrap().is_empty());
        assert_eq!(
            peer.call(&[&status_line(&s)])[0]["result"]["stop_requested"],
            false
        );
    }

    // ---- P59: output snapshots and pages ----

    fn result_line(session: &str, output: &str) -> String {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"session.result\",\"params\":{{\"session\":\"{session}\",\"output\":{output}}}}}"
        )
    }

    fn finished_with(test: &str, stdout: &[u8], stderr: &[u8]) -> (Peer, String) {
        let runs = FakeRuns::ok(0, "old tail");
        runs.views
            .lock()
            .unwrap()
            .insert(crate::session::view::Stream::Stdout, stdout.to_vec());
        runs.views
            .lock()
            .unwrap()
            .insert(crate::session::view::Stream::Stderr, stderr.to_vec());
        *runs.generation.lock().unwrap() = "g1".into();
        let peer = Peer::new(test, runs);
        let s = handle_of(&peer.call(&[&start_call("")]));
        peer.settle(&s);
        (peer, s)
    }

    /// Reassemble the way the contract says: first page is the end, every
    /// later page goes in front. All on one connection, since cursors live
    /// in the process that issued them.
    fn read_all(peer: &Peer, s: &str, stream: &str, budget: u64) -> Result<Vec<u8>, Value> {
        let mut whole = Vec::new();
        let mut cursor = Value::Null;
        let mut lines = Vec::new();
        // One process for the whole walk: pages are requested one per line,
        // but each needs the previous answer, so drive `serve` a page at a
        // time with a shared Context.
        let ctx = Context {
            config_path: peer.config.clone(),
            state: peer.state.to_path_buf(),
            runs: peer.runs.clone(),
            runner: Arc::new(SystemRunner),
            cursors: Default::default(),
        };
        loop {
            let params = serde_json::json!({"session": s, "output": {"stream": stream, "max_bytes": budget, "cursor": cursor}});
            let answer = session::result(&ctx, params.as_object().unwrap());
            let out = match answer {
                Ok(v) => v["output"].clone(),
                Err(e) => return Err(serde_json::to_value(&e).unwrap()),
            };
            let page = out["tail"].as_str().unwrap().as_bytes().to_vec();
            assert!(page.len() as u64 <= budget, "a page over max_bytes");
            lines.push(out.clone());
            whole.splice(0..0, page);
            cursor = out["cursor"].clone();
            if cursor.is_null() {
                return Ok(whole);
            }
        }
    }

    /// OUT-01/07: every byte of the retained view comes back, in order,
    /// under any budget, and the two streams stay apart.
    #[test]
    fn out_pages_reassemble_to_the_view_under_any_budget() {
        let stdout = format!("{}中文😀{}", "x".repeat(5000), "y".repeat(3000)).into_bytes();
        let (peer, s) = finished_with("out-pages", &stdout, b"only stderr\n");
        for budget in [4, 5, 7, 100, 4096, 1 << 20] {
            assert_eq!(
                read_all(&peer, &s, "stdout", budget).unwrap(),
                stdout,
                "budget {budget}"
            );
        }
        assert_eq!(read_all(&peer, &s, "stderr", 4).unwrap(), b"only stderr\n");
        // Copied once per stream, however many pages were read.
        let fetched: Vec<_> = peer
            .runs
            .outputs
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.stream)
            .collect();
        assert_eq!(
            fetched,
            [
                crate::session::view::Stream::Stdout,
                crate::session::view::Stream::Stderr
            ]
        );
    }

    /// A budget that cannot hold the next character is refused with the
    /// size it needs -- not an empty page that would loop forever.
    #[test]
    fn out_a_budget_below_one_character_says_what_it_needs() {
        let (peer, s) = finished_with("out-small", "ab😀".as_bytes(), b"");
        let err = read_all(&peer, &s, "stdout", 3).unwrap_err();
        assert_eq!(err["code"], code::INVALID_PARAMS);
        assert_eq!(err["data"]["reason"], "max_bytes_too_small");
        assert_eq!(err["data"]["min_bytes"], 4);
        assert_eq!(read_all(&peer, &s, "stdout", 4).unwrap(), "ab😀".as_bytes());
    }

    #[test]
    fn out_bad_budgets_and_streams_are_invalid_params() {
        let (peer, s) = finished_with("out-params", b"x", b"");
        for bad in [
            "{\"max_bytes\":0}",
            "{\"max_bytes\":-1}",
            "{\"max_bytes\":\"8\"}",
            "{\"max_bytes\":1.5}",
            "{\"stream\":\"both\"}",
            "{\"cursor\":5}",
            "{\"extra\":1}",
        ] {
            let out = peer.call(&[&result_line(&s, bad)]);
            assert_eq!(out[0]["error"]["code"], code::INVALID_PARAMS, "{bad}");
        }
    }

    /// Cursors belong to the process, the session, the stream and the view
    /// they were cut from.
    #[test]
    fn out_cursors_expire_outside_what_issued_them() {
        let (peer, s) = finished_with("out-cursor", &[b'a'; 3000], &[b'b'; 3000]);
        let first = peer.call(&[&result_line(&s, "{\"max_bytes\":1000}")]);
        let cursor = first[0]["result"]["output"]["cursor"]
            .as_str()
            .unwrap()
            .to_string();
        // `peer.call` is a new process every time: the cursor is gone.
        let later = peer.call(&[&result_line(
            &s,
            &format!("{{\"max_bytes\":1000,\"cursor\":\"{cursor}\"}}"),
        )]);
        assert_eq!(later[0]["error"]["code"], code::EXPIRED);
        assert_eq!(later[0]["error"]["data"]["reason"], "cursor_expired");
        // Inside one process: the stream and the session have to match too.
        let one = peer.call(&[
            &result_line(&s, "{\"max_bytes\":1000}"),
            &result_line(&s, "{\"max_bytes\":1000}"),
        ]);
        let c = one[0]["result"]["output"]["cursor"].as_str().unwrap();
        assert_eq!(
            one[1]["result"]["output"]["cursor"], c,
            "the same page keeps its cursor"
        );
        let crossed = peer.call(&[
            &result_line(&s, "{\"max_bytes\":1000}"),
            &result_line(
                &s,
                &format!("{{\"max_bytes\":1000,\"stream\":\"stderr\",\"cursor\":\"{c}\"}}"),
            ),
        ]);
        assert_eq!(crossed[1]["error"]["code"], code::EXPIRED);
    }

    /// OUT-06: no Agent, no view: the old tail, saying why, never
    /// presented as the whole output. A later read gets the real thing.
    #[test]
    fn out_an_unreachable_agent_falls_back_to_the_old_tail_and_says_so() {
        let (peer, s) = finished_with("out-fallback", b"the whole thing", b"");
        *peer.runs.output_fails.lock().unwrap() = Some((crate::ErrorCode::AgentUnreachable, 0));
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert_eq!(out["unavailable_reason"], "agent_unreachable");
        assert_eq!(out["tail"], "old tail");
        assert!(out.get("source_truncated").is_none());
        *peer.runs.output_fails.lock().unwrap() = Some((crate::ErrorCode::Version, 0));
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert_eq!(out["unavailable_reason"], "agent_refused");
        *peer.runs.output_fails.lock().unwrap() = None;
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert!(out.get("unavailable_reason").is_none(), "{out}");
        assert_eq!(out["tail"], "the whole thing");
        assert_eq!(
            out["source_truncated"], true,
            "the Agent's own flag is passed on"
        );
    }

    /// F23: the Agent refuses a view because the session's raw stream is
    /// gone. Before P68 the Agent served it as 0 bytes and this side passed
    /// on "empty and complete"; a refusal has to come out as the old tail,
    /// named as such, every time it is asked.
    #[test]
    fn out_a_stream_the_agent_lost_is_never_reported_complete() {
        let (peer, s) = finished_with("out-lost", b"", b"");
        *peer.runs.output_fails.lock().unwrap() = Some((crate::ErrorCode::Internal, 0));
        for _ in 0..2 {
            let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
            assert_eq!(out["unavailable_reason"], "agent_refused", "{out}");
            assert_eq!(out["tail"], "old tail");
            assert!(out.get("source_bytes").is_none(), "{out}");
        }
        let kept: Vec<_> = std::fs::read_dir(peer.state.join("rpc/outputs").join(&s))
            .unwrap()
            .collect();
        assert!(kept.is_empty(), "no snapshot is kept for a refused copy");
    }

    /// OUT-06: a copy that breaks half way, or whose view changes under it,
    /// leaves nothing behind that could later pass for complete.
    #[test]
    fn out_a_broken_or_shifting_copy_leaves_no_snapshot() {
        let big = vec![b'z'; 3 * 1024 * 1024 + 10];
        let (peer, s) = finished_with("out-broken", &big, b"");
        *peer.runs.output_fails.lock().unwrap() = Some((crate::ErrorCode::AgentUnreachable, 2));
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert_eq!(out["unavailable_reason"], "agent_unreachable");
        let dir = peer.state.join("rpc/outputs").join(&s);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");

        *peer.runs.output_fails.lock().unwrap() = None;
        // The view changes after the first slice: refused, nothing kept.
        peer.runs.outputs.lock().unwrap().clear();
        *peer.runs.shift_after.lock().unwrap() = Some(1);
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert_eq!(out["unavailable_reason"], "agent_refused");
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");

        *peer.runs.shift_after.lock().unwrap() = None;
        peer.runs.outputs.lock().unwrap().clear();
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert!(out.get("unavailable_reason").is_none(), "{out}");
        assert_eq!(out["bytes_total"], big.len() as u64);
        let copied = peer.runs.outputs.lock().unwrap().len();
        assert_eq!(copied, 4, "3 MiB + 10 bytes in 1 MiB slices");
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
    }

    /// A record from before P58 has no Agent-side id to ask with: its old
    /// tail is paged and named for what it is. A run stopped before it was
    /// sent has nothing at all, and says nothing more.
    #[test]
    fn out_records_without_an_agent_run_page_what_they_kept() {
        let (peer, s) = finished_with("out-legacy", b"never read", b"");
        let store = store::Store::open(&peer.state).unwrap();
        let mut record = store.read(&s).unwrap().unwrap();
        record.managed_session = None;
        store.write(&record).unwrap();
        let out = &peer.call(&[&result_line(&s, "{\"max_bytes\":4}")])[0]["result"]["output"];
        assert_eq!(out["unavailable_reason"], "legacy_tail");
        assert_eq!(out["tail"], "tail");
        assert_eq!(out["truncated"], true);

        let mut record = store.read(&s).unwrap().unwrap();
        record.managed_session = Some(crate::session::new_id());
        record.dispatched = false;
        record.finish = Some(store::Finish {
            error: Some("stopped before it was sent to the Agent".into()),
            ..Default::default()
        });
        record.state = store::State::Failed;
        store.write(&record).unwrap();
        peer.runs.outputs.lock().unwrap().clear();
        let out = &peer.call(&[&result_line(&s, "{}")])[0]["result"]["output"];
        assert_eq!(out["bytes_total"], 0);
        assert!(out.get("unavailable_reason").is_none(), "{out}");
        assert!(
            peer.runs.outputs.lock().unwrap().is_empty(),
            "nothing ran, so the Agent is not asked"
        );
    }
}

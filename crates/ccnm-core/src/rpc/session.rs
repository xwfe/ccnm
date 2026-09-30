//! `session.start` / `status` / `result` / `stop`.
//!
//! These call the same application functions the human CLI calls --
//! `launcher::run_print_assigned`, `launcher::stop_assigned` -- rather than
//! shelling out to `ccnm` and reading its output. Parsing a CLI's prose
//! would freeze wording into an API and lose everything the wording rounds
//! off.
//!
//! `session.start` answers as soon as it has a handle and hands the run to
//! the session's own owner process (`ccnm internal rpc-run`, P63). That is
//! what lets a client disconnect and come back: the record on disk, not the
//! connection, is what a session belongs to. Before P63 the run rode on a
//! thread of the `ccnm rpc` process and died when the client closed stdin
//! (F16).
//!
//! **One handle, one Agent session, from the first moment** (P58). The
//! record gets a ccnm session id before anything is sent, the Agent is told
//! to use exactly that id, and a stop names exactly that id. Before P58 the
//! id only came back with the finished run, so a stop could only say "stop
//! this workspace": on the Agent that looked at tmux alone, missed every
//! print run, and stopped someone's interactive session instead (P57 B).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::store::{Finish, KeyClaim, Launch, OwnerCheck, Record, State, Store};
use super::wire::{self, Effect, RpcError, code};
use super::{Context, reject_unknown, require_str};
use crate::config::Config;
// ccnm 自己的 Result 是单参数别名，和 std 的同名，起个别名免得混。
use crate::error::{ErrorCode, Result as CcnmResult};
use crate::instance::InstanceRef;
use crate::protocol::run::RunReport;

/// One thing to run, with everything the executor needs and nothing else.
#[derive(Debug, Clone)]
pub struct RunAsk {
    pub workspace: String,
    /// Instance name only. The node is never taken from the caller -- see
    /// [`resolve_agent`].
    pub instance: Option<String>,
    pub prompt: String,
    pub timeout: Duration,
    /// The ccnm session id the Agent must run this under: already in the
    /// record, so a stop can name it before the run answers.
    pub session: String,
}

impl RunAsk {
    /// What an accepted record says to run.
    ///
    /// Whoever carries the run -- a thread of the `ccnm rpc` that accepted
    /// it, or the session's own owner process (P63) -- builds the ask from
    /// the record on disk, so both send exactly what was accepted. The
    /// instance is the one the record was bound to at acceptance, not
    /// "the workspace default" looked up again later.
    pub fn from_record(record: &Record) -> Option<RunAsk> {
        Some(RunAsk {
            workspace: record.launch.workspace.clone(),
            instance: record.launch.agent.as_ref().map(|a| a.instance.clone()),
            prompt: record.launch.prompt.clone(),
            timeout: record
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_TIMEOUT),
            session: record.managed_session.clone()?,
        })
    }
}

/// One session to stop, named the way the record names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAsk {
    pub workspace: String,
    /// The node the session was started on. Checked against the current
    /// binding by whoever resolves the connection, so an edited config
    /// cannot redirect an old handle to another machine.
    pub node: String,
    pub instance: String,
    /// The ccnm session id the run was sent under.
    pub session: String,
}

/// A slice of the retained output of one session, asked of the Agent it ran
/// on (P59). Named like a stop: the node it was started on, and the id it
/// was sent under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputAsk {
    pub workspace: String,
    pub node: String,
    pub instance: String,
    pub session: String,
    pub stream: crate::session::view::Stream,
    pub offset: u64,
    pub limit: u64,
}

/// Whose write guard to look at before a start (P60): the workspace, and
/// the instance whose Agent relays the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardAsk {
    pub workspace: String,
    pub instance: Option<String>,
}

/// What actually executes. The real one goes through `launcher`; tests put
/// in one that finishes immediately, so no test starts an Agent or dials
/// ssh.
pub trait Runs: Send + Sync + 'static {
    fn run_print(&self, ask: &RunAsk) -> CcnmResult<RunReport>;
    /// Stop exactly `ask.session`: kill it, or fence it off if the run has
    /// not reached the Agent yet. Never "whatever the workspace is running".
    fn stop(&self, ask: &StopAsk) -> CcnmResult<bool>;
    /// A slice of `ask.session`'s retained output view on the Agent.
    fn output(&self, ask: &OutputAsk) -> CcnmResult<crate::protocol::run::OutputReport>;
    /// The workspace's write guard, as its Runtime Executor sees it.
    fn guard(&self, ask: &GuardAsk) -> CcnmResult<crate::protocol::run::AgentGuardReport>;
    /// Hand the run of `handle` to a process of its own and return its pid,
    /// or `None` to carry it on a thread of this process.
    ///
    /// A thread is what the in-process test executors use. The real one
    /// must not: the `ccnm rpc` process exits when its client closes stdin,
    /// and a thread dies with it -- before P63 that meant a run accepted
    /// but not yet sent was never sent, and one already sent stayed
    /// `unknown` for good, the opposite of what protocol section 8.1
    /// promises (F16).
    fn detach(&self, _handle: &str) -> CcnmResult<Option<u32>> {
        Ok(None)
    }
}

/// How long a run may take when the caller does not say.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);

/// The real executor: the same `launcher` calls `ccnm run --print` and
/// `ccnm stop` make.
///
/// Config is loaded per call rather than held, for the same reason
/// `agents.list` reloads it: this process outlives edits to the file, and a
/// run started against a workspace definition from an hour ago is worse than
/// a slightly slower call.
pub struct SystemRuns {
    pub config_path: std::path::PathBuf,
}

impl SystemRuns {
    fn env() -> CcnmResult<crate::launcher::Env<'static>> {
        Ok(crate::launcher::Env {
            runner: &crate::process::SystemRunner,
            control_dir: crate::paths::state_dir()?.join("ssh"),
            current_exe: std::env::current_exe()?,
        })
    }
}

impl Runs for SystemRuns {
    fn run_print(&self, ask: &RunAsk) -> CcnmResult<RunReport> {
        let config = Config::load(&self.config_path)?;
        let resolved = config.workspace(&ask.workspace)?;
        crate::launcher::run_print_assigned(
            &resolved,
            &Self::env()?,
            &ask.prompt,
            ask.timeout,
            ask.instance.as_deref(),
            &ask.session,
        )
    }

    fn stop(&self, ask: &StopAsk) -> CcnmResult<bool> {
        let config = Config::load(&self.config_path)?;
        let resolved = config.workspace(&ask.workspace)?;
        // The session layer checked this against the config it loaded; this
        // is the config the connection is actually built from.
        if resolved.workspace.agent.as_ref().map(|a| a.node.as_str()) != Some(ask.node.as_str()) {
            return Err(crate::Error::policy(REBOUND));
        }
        crate::launcher::stop_assigned(&resolved, &Self::env()?, &ask.instance, &ask.session)
    }

    fn output(&self, ask: &OutputAsk) -> CcnmResult<crate::protocol::run::OutputReport> {
        let config = Config::load(&self.config_path)?;
        let resolved = config.workspace(&ask.workspace)?;
        // Same rule as stop: the output of a session is read from the machine
        // it ran on, not from whatever the workspace points at now.
        if resolved.workspace.agent.as_ref().map(|a| a.node.as_str()) != Some(ask.node.as_str()) {
            return Err(crate::Error::policy(REBOUND));
        }
        crate::launcher::read_output_assigned(
            &resolved,
            &Self::env()?,
            &ask.instance,
            &ask.session,
            ask.stream,
            ask.offset,
            ask.limit,
        )
    }

    fn guard(&self, ask: &GuardAsk) -> CcnmResult<crate::protocol::run::AgentGuardReport> {
        let config = Config::load(&self.config_path)?;
        let resolved = config.workspace(&ask.workspace)?;
        crate::launcher::observe_guard(&resolved, &Self::env()?, ask.instance.as_deref())
    }

    /// `ccnm --config <this config> internal rpc-run --handle <handle>`, in
    /// a process group of its own with nothing on its stdio.
    ///
    /// Its own group, so that whatever ends the `ccnm rpc` that accepted the
    /// run -- the client closing it, killing it, a Ctrl-C in the terminal
    /// that started both -- does not reach the run. Its environment is this
    /// process's: the same state directory, the same PATH to ssh. What it
    /// runs comes from the record, so nothing else needs to cross.
    fn detach(&self, handle: &str) -> CcnmResult<Option<u32>> {
        use std::os::unix::process::CommandExt as _;
        use std::process::{Command, Stdio};

        let exe = std::env::current_exe()?;
        let mut child = Command::new(&exe)
            .arg("--config")
            .arg(&self.config_path)
            .args(["internal", "rpc-run", "--handle", handle])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| {
                crate::Error::internal(format!("cannot start the owner of {handle}")).with_source(e)
            })?;
        let pid = child.id();
        // Reaped here while this process lives; once it exits, whoever
        // inherits the orphan does it.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(Some(pid))
    }
}

/// The body of `ccnm internal rpc-run`: carry one accepted run to its end.
///
/// The session's owner (P63). Everything it needs is in the record under
/// `state`; the record's own lock orders what it writes against a stop or
/// another owner, exactly as it did when this ran on a thread.
pub fn run_owned(
    state: &std::path::Path,
    config_path: std::path::PathBuf,
    handle: &str,
) -> CcnmResult<()> {
    if !super::store::valid_handle(handle) {
        return Err(crate::Error::invalid_args(
            "handle is not one this server issues",
        ));
    }
    let store = Store::open(state)?;
    let record = store
        .read(handle)?
        .ok_or_else(|| crate::Error::invalid_args(format!("no session {handle}")))?;
    let ask = RunAsk::from_record(&record).ok_or_else(|| {
        crate::Error::invalid_args(format!(
            "{handle} was accepted by a build before P58 and names no Agent-side session"
        ))
    })?;
    run_to_end(&SystemRuns { config_path }, &store, handle, &ask);
    Ok(())
}

const REBOUND: &str = "this workspace is bound to another Agent node than the one this session was started on; its stop is not sent to the new one";

pub fn start(ctx: &Context, params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(
        params,
        &[
            "workspace",
            "agent",
            "mode",
            "input",
            "start_key",
            "timeout_ms",
        ],
    )?;
    let workspace = require_str(params, "workspace")?.to_string();
    let mode = require_str(params, "mode")?;
    if mode != "print" {
        // interactive is not in the capabilities this build advertises, and
        // calling something the server never offered is its own error.
        return Err(RpcError::refused(
            code::UNSUPPORTED_CAPABILITY,
            format!("mode {mode} is not offered by this server"),
        )
        .with_reason(mode));
    }
    let prompt = params
        .get("input")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::refused(code::INVALID_PARAMS, "input must be an object"))
        .and_then(|input| {
            reject_unknown(input, &["prompt"])?;
            require_str(input, "prompt").map(str::to_string)
        })?;
    let timeout = match params.get("timeout_ms") {
        None => DEFAULT_TIMEOUT,
        Some(value) => match value.as_u64().filter(|ms| *ms > 0) {
            Some(ms) => Duration::from_millis(ms),
            None => {
                return Err(RpcError::refused(
                    code::INVALID_PARAMS,
                    "timeout_ms must be a positive integer",
                ));
            }
        },
    };
    let start_key = match params.get("start_key") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|key| !key.is_empty() && key.chars().count() <= 128)
                .ok_or_else(|| {
                    RpcError::refused(
                        code::INVALID_PARAMS,
                        "start_key must be a string of 1..128 characters",
                    )
                })?
                .to_string(),
        ),
    };

    let config = ctx.config()?;
    let bound = binding(&config, &workspace)?;
    let instance = resolve_agent(params, &bound)?;
    let agent = InstanceRef {
        node: bound.node.clone(),
        instance: instance.clone().unwrap_or(bound.instance.clone()),
    };

    let store = ctx.store()?;
    let launch = Launch {
        workspace: workspace.clone(),
        agent: Some(agent.clone()),
        mode: "print".to_string(),
        prompt: prompt.clone(),
    };

    // A key that is already taken is answered from its record before the
    // Runtime is asked anything: the guard may well be held by that very
    // session, and a client looking its task up again must get the task
    // back, not `busy` (P60).
    if let Some(key) = &start_key
        && let Some(existing) = store
            .find_key(&workspace, key)
            .map_err(|e| wire::from_ccnm(&e))?
    {
        return reuse_or_conflict(ctx, &store, existing.as_deref(), &launch);
    }
    preflight(
        ctx.runs.as_ref(),
        &GuardAsk {
            workspace: workspace.clone(),
            instance: instance.clone(),
        },
    )?;

    let owner_pid = std::process::id();
    let record = Record {
        session: new_session_id(),
        launch: launch.clone(),
        start_key: start_key.clone(),
        state: State::Starting,
        accepted_at: now_rfc3339(),
        stop_requested: false,
        timeout_ms: Some(timeout.as_millis() as u64),
        owner_pid,
        owner_started: super::store::process_started(ctx.runner.as_ref(), owner_pid)
            .unwrap_or_default(),
        managed_session: Some(crate::session::new_id()),
        dispatched: false,
        finish: None,
        cleaned_at: None,
    };
    // The record first, then the key: whoever finds the key finds the
    // record behind it. Nothing is sent to the Agent until both exist.
    store.create(&record).map_err(|e| wire::from_ccnm(&e))?;
    if let Some(key) = &start_key {
        match store.claim_key(&workspace, key, &record.session) {
            Ok(KeyClaim::Taken) => {}
            Ok(KeyClaim::Held(existing)) => {
                // Never handed out and never sent: taking it back is safe.
                store.remove_unpublished(&record.session);
                return reuse_or_conflict(ctx, &store, existing.as_deref(), &launch);
            }
            Err(err) => {
                store.remove_unpublished(&record.session);
                return Err(wire::from_ccnm(&err));
            }
        }
    }

    let ask = RunAsk::from_record(&record).expect("a new record carries its managed session");
    match ctx.runs.detach(&record.session) {
        // The owner is on record before this call answers: a client that
        // hangs up the moment it has the handle still finds someone
        // carrying the run when it comes back.
        Ok(Some(pid)) => {
            let started =
                super::store::process_started(ctx.runner.as_ref(), pid).unwrap_or_default();
            store
                .update(&record.session, |r| {
                    r.owner_pid = pid;
                    r.owner_started = started;
                })
                .map_err(|e| wire::from_ccnm(&e))?;
        }
        Ok(None) => spawn_run(
            ctx.runs.clone(),
            ctx.state.clone(),
            record.session.clone(),
            ask,
        ),
        Err(err) => {
            // Nothing was sent, and nothing ever will be for this handle.
            let _ = store.update(&record.session, |r| {
                if !r.state.terminal() {
                    r.state = State::Failed;
                    r.finish = Some(Finish {
                        error: Some(err.message().to_string()),
                        ..Finish::default()
                    });
                }
            });
            return Err(wire::from_ccnm(&err).with_session(&record.session));
        }
    }

    Ok(serde_json::json!({
        "session": record.session,
        "state": "starting",
        "reused": false,
        "workspace": record.launch.workspace,
        "agent": agent_value(&agent, None),
        "accepted_at": record.accepted_at,
    }))
}

/// A `start_key` that is already taken: same input means the same session,
/// different input is a conflict the server refuses to guess about.
fn reuse_or_conflict(
    ctx: &Context,
    store: &Store,
    existing: Option<&str>,
    launch: &Launch,
) -> Result<Value, RpcError> {
    let Some(existing) = existing else {
        // An entry from before P58 that its server never finished writing:
        // there is no session id to give back, and an empty one is not a
        // handle anybody can look up.
        return Err(RpcError::new(
            code::UNCERTAIN,
            "start_key has an entry an earlier server never finished writing; which session it names cannot be known",
            Effect::Unknown,
        ));
    };
    let Some(record) = store.read(existing).map_err(|e| wire::from_ccnm(&e))? else {
        // The key points at a record that is gone. Something removed it
        // behind our back; that is not a state to start a second Agent from.
        return Err(RpcError::new(
            code::UNCERTAIN,
            "start_key points at a session this server can no longer find",
            Effect::Unknown,
        )
        .with_session(existing));
    };
    if record.launch != *launch {
        return Err(RpcError::refused(
            code::CONFLICT,
            "start_key already used with different input",
        )
        .with_session(existing));
    }
    let state = record.observed_state(ctx.owner_of(&record));
    Ok(serde_json::json!({
        "session": record.session,
        "state": state_name(state),
        "reused": true,
        "workspace": record.launch.workspace,
        "agent": agent_value(
            record.launch.agent.as_ref().expect("recorded launches bind an instance"),
            record.finish.as_ref().and_then(|f| f.provider),
        ),
        "accepted_at": record.accepted_at,
    }))
}

/// Refuse a start the Runtime has already said cannot write (P60).
///
/// **Not a reservation.** A `free` here grants nothing: another client can
/// take the guard a moment later, and the one thing that hands out write
/// authority is still the Runtime's guard when the session's tools open --
/// which refuses the later of two writers exactly as before. What this buys
/// is the common case: a tree that is plainly taken is refused now, with
/// nothing started, instead of accepted and failed a minute later.
///
/// Only an answer from the Runtime refuses. When the question cannot be put
/// at all -- the Agent is unreachable, or a peer is too old to know it --
/// there is no verdict either way, and the start goes on exactly as it did
/// before this check existed. That is not reading silence as `free`: the
/// start was never authorized by this check, and the guard still decides.
fn preflight(runs: &dyn Runs, ask: &GuardAsk) -> Result<(), RpcError> {
    use crate::mcp::write_guard::Observed;
    let report = match runs.guard(ask) {
        Ok(report) => report,
        Err(error) => {
            tracing::info!(
                workspace = %ask.workspace,
                %error,
                "write guard not observed; the start goes on and the Runtime decides when the session opens"
            );
            return Ok(());
        }
    };
    let seen = &report.runtime.observation;
    let reason = serde_json::to_value(seen.reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    match seen.state {
        Observed::Free => Ok(()),
        // Another process holds it now: waiting can help.
        Observed::Held => Err(RpcError::refused(
            code::BUSY,
            "another writer holds this workspace's write guard on the Runtime; nothing was started",
        )
        .with_reason(reason)),
        // Nothing is holding it, and it is not free either: something a
        // person has to look at. Retrying on a timer would only repeat this.
        Observed::Abandoned | Observed::Unknown => Err(RpcError::refused(
            code::POLICY,
            "the Runtime cannot hand this workspace's write guard to a new session until someone recovers it; nothing was started",
        )
        .with_reason(reason)),
    }
}

pub fn status(ctx: &Context, params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(params, &["session"])?;
    let id = require_str(params, "session")?;
    let record = load(ctx, id)?;
    let state = record.observed_state(ctx.owner_of(&record));
    let mut out = serde_json::json!({
        "session": record.session,
        "state": state_name(state),
        "workspace": record.launch.workspace,
        "agent": agent_value(
            record.launch.agent.as_ref().expect("recorded launches bind an instance"),
            record.finish.as_ref().and_then(|f| f.provider),
        ),
        "started_at": record.accepted_at,
        "stop_requested": record.stop_requested,
    });
    if let Some(id) = record
        .finish
        .as_ref()
        .and_then(|f| f.provider_session_id.clone())
    {
        out["provider_session_id"] = Value::String(id);
    }
    Ok(out)
}

pub fn result(ctx: &Context, params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(params, &["session", "output"])?;
    let id = require_str(params, "session")?;
    // Checked before anything is read, so a bad budget or stream costs
    // nothing and a bad cursor is judged against a record that exists.
    let asked = super::output::params(params.get("output"))?;
    let record = load(ctx, id)?;
    // Cleaned by `ccnm cleanup` (P61). Not `not_found`: the session did
    // run, `status` still says how it ended, and its start key still names
    // it -- only what it produced is gone, and no cursor into it can work.
    if record.cleaned_at.is_some() {
        return Err(RpcError::refused(
            code::EXPIRED,
            "this session's result was removed by ccnm cleanup; its state is still kept",
        )
        .with_reason("cleaned")
        .with_session(&record.session));
    }
    let state = record.observed_state(ctx.owner_of(&record));
    let mut out = serde_json::json!({
        "session": record.session,
        "state": state_name(state),
        "workspace": record.launch.workspace,
        "agent": agent_value(
            record.launch.agent.as_ref().expect("recorded launches bind an instance"),
            record.finish.as_ref().and_then(|f| f.provider),
        ),
    });
    let Some(finish) = &record.finish else {
        // Not over yet is not an error: the caller gets the state and comes
        // back. An absent outcome must not be read as failure.
        return Ok(out);
    };
    out["outcome"] = serde_json::json!({
        "exit_code": finish.exit_code,
        "timed_out": finish.timed_out,
        "duration_ms": finish.duration_ms,
        "stop_requested": record.stop_requested,
    });
    out["text"] = match &finish.text {
        Some(text) => Value::String(text.clone()),
        None => Value::Null,
    };
    if let Some(id) = &finish.provider_session_id {
        out["provider_session_id"] = Value::String(id.clone());
    }
    if finish.input_tokens.is_some() || finish.output_tokens.is_some() {
        let mut usage = Map::new();
        if let Some(v) = finish.input_tokens {
            usage.insert("input_tokens".into(), Value::from(v));
        }
        if let Some(v) = finish.output_tokens {
            usage.insert("output_tokens".into(), Value::from(v));
        }
        out["usage"] = Value::Object(usage);
    }
    if let Some(cost) = finish.total_cost_usd {
        out["cost"] = serde_json::json!({"total_usd": cost});
    }
    out["output"] = super::output::page(ctx, &record, finish, &asked)?;
    Ok(out)
}

pub fn stop(ctx: &Context, params: &Map<String, Value>) -> Result<Value, RpcError> {
    reject_unknown(params, &["session", "mode"])?;
    let id = require_str(params, "session")?;
    if let Some(mode) = params.get("mode")
        && mode.as_str() != Some("graceful")
    {
        return Err(RpcError::refused(
            code::INVALID_PARAMS,
            "mode must be \"graceful\"",
        ));
    }
    let record = load(ctx, id)?;
    let state = record.observed_state(ctx.owner_of(&record));
    if state.terminal() {
        // Idempotent: stopping something already over is a success, so a
        // client retrying does not have to check the state first.
        return Ok(stop_answer(&record.session, state, record.stop_requested));
    }
    let Some(managed) = record.managed_session.clone() else {
        // Accepted by a build before P58, which kept no Agent-side id to
        // name. The only stop left would be "this workspace's session",
        // which is exactly what could hit the wrong one.
        return Err(RpcError::refused(
            code::NOT_READY,
            "this session was started by an older ccnm that did not record its Agent-side id, so it cannot be stopped exactly; look on the Agent Node before stopping anything by workspace",
        )
        .with_session(id));
    };
    let agent = record
        .launch
        .agent
        .clone()
        .expect("recorded launches bind an instance");
    // The handle stays bound to the machine it was started on. A workspace
    // rebound in the meantime does not carry the stop to the new Agent.
    let config = ctx.config()?;
    let bound = binding(&config, &record.launch.workspace)?;
    if bound.node != agent.node {
        return Err(RpcError::refused(code::POLICY, REBOUND).with_session(id));
    }

    let store = ctx.store()?;
    // Decided under the store lock against the run thread's own check: the
    // run is either not sent yet -- and now never will be -- or sent, and
    // the Agent has to be asked. Either way the request is on record from
    // here on: before P63 a sent run only got the flag after the Agent
    // confirmed, so any other answer lost it for good (F17).
    let dispatched = store
        .update(id, |r| {
            if r.state.terminal() {
                return None;
            }
            r.stop_requested = true;
            if !r.dispatched {
                r.state = State::Stopping;
            }
            Some(r.dispatched)
        })
        .map_err(|e| wire::from_ccnm(&e))?
        .flatten();
    match dispatched {
        // Over in the meantime, or no longer there to change.
        None => {
            let now = load(ctx, id)?;
            let state = now.observed_state(ctx.owner_of(&now));
            Ok(stop_answer(&now.session, state, now.stop_requested))
        }
        Some(false) => Ok(stop_answer(id, State::Stopping, true)),
        Some(true) => {
            let asked = ctx.runs.stop(&StopAsk {
                workspace: record.launch.workspace.clone(),
                node: agent.node,
                instance: agent.instance,
                session: managed,
            });
            match asked {
                Ok(_) => {}
                // The Agent took the stop and cannot say yet that the run
                // is over: on a real machine the signal lands before the
                // process group is gone. That is what `stopping` means
                // (section 5.6), not a failed stop; the run's own end
                // decides the rest, and stop stays safe to send again.
                Err(err) if err.code() == ErrorCode::NotReady => {
                    tracing::info!(session = id, %err, "stop sent; the Agent has not seen the run end yet");
                }
                Err(err) => return Err(stop_failed(&err, id)),
            }
            // Not a terminal state: the request was accepted, and only an
            // observed end -- the run's own answer -- makes it over.
            // Reporting `completed` here would hand the write permission to
            // the next caller on a guess. A run that already ended keeps its
            // end; the stop is still on record.
            let now = store
                .update(id, |r| {
                    r.stop_requested = true;
                    if !r.state.terminal() {
                        r.state = State::Stopping;
                    }
                    r.state
                })
                .map_err(|e| wire::from_ccnm(&e))?
                .unwrap_or(State::Stopping);
            Ok(stop_answer(id, now, true))
        }
    }
}

fn stop_answer(session: &str, state: State, stop_requested: bool) -> Value {
    serde_json::json!({
        "session": session,
        "state": state_name(state),
        "stop_requested": stop_requested,
    })
}

/// A stop the Agent could not confirm.
///
/// When the ssh broke or the answer made no sense, the stop may still have
/// landed: `effect: unknown`. Resending a stop is harmless either way; the
/// point is not to tell the caller that nothing happened.
fn stop_failed(err: &crate::Error, session: &str) -> RpcError {
    let mut rpc = wire::from_ccnm(err).with_session(session);
    let may_have_landed = match err.code() {
        // ssh that never reached the Agent cannot have delivered anything
        // (F14); the caller should just send the stop again.
        ErrorCode::AgentUnreachable => !crate::ssh::never_reached(err.message()),
        ErrorCode::Internal => true,
        _ => false,
    };
    if may_have_landed {
        rpc.data.effect = Effect::Unknown;
    }
    rpc
}

fn load(ctx: &Context, id: &str) -> Result<Record, RpcError> {
    ctx.store()?
        .read(id)
        .map_err(|e| wire::from_ccnm(&e))?
        .ok_or_else(|| {
            // Same answer for "never existed" and "cleaned up long ago":
            // telling them apart would make the error a probe.
            RpcError::refused(code::NOT_FOUND, "no such session").with_session(id)
        })
}

/// The instance a workspace is bound to, or why it cannot be used.
struct Binding {
    node: String,
    instance: String,
}

fn binding(config: &Config, workspace: &str) -> Result<Binding, RpcError> {
    let Some(defined) = config.workspaces.get(workspace) else {
        return Err(RpcError::refused(
            code::NOT_FOUND,
            "no such workspace or instance",
        ));
    };
    let Some(reference) = &defined.agent else {
        return Err(RpcError::refused(
            code::NOT_READY,
            "this workspace has no Agent instance binding; the machine API only addresses instances",
        ));
    };
    Ok(Binding {
        node: reference.node.clone(),
        instance: reference.instance.clone(),
    })
}

/// The instance override a caller asked for, if any.
///
/// The caller may pick a different instance on the bound node; it may not
/// pick a different node. Which machine a workspace's Agent runs on is the
/// configuration's answer, not the caller's, for the same reason the caller
/// cannot name the workspace root.
fn resolve_agent(params: &Map<String, Value>, bound: &Binding) -> Result<Option<String>, RpcError> {
    let Some(agent) = params.get("agent") else {
        return Ok(None);
    };
    let agent = agent
        .as_object()
        .ok_or_else(|| RpcError::refused(code::INVALID_PARAMS, "agent must be an object"))?;
    reject_unknown(agent, &["node", "instance"])?;
    let node = require_str(agent, "node")?;
    let instance = require_str(agent, "instance")?;
    if node != bound.node {
        // Not "wrong node": that would confirm which node the workspace is
        // bound to for anyone probing.
        return Err(RpcError::refused(
            code::NOT_FOUND,
            "no such workspace or instance",
        ));
    }
    crate::instance::identifier(instance).map_err(|e| wire::from_ccnm(&e))?;
    Ok(Some(instance.to_string()))
}

fn agent_value(agent: &InstanceRef, provider: Option<crate::provider::AgentProvider>) -> Value {
    let mut value = serde_json::json!({"node": agent.node, "instance": agent.instance});
    // Only present once the Agent Node has said so itself.
    if let Some(provider) = provider
        && let Ok(name) = serde_json::to_value(provider)
    {
        value["provider"] = name;
    }
    value
}

pub fn state_name(state: State) -> &'static str {
    match state {
        State::Starting => "starting",
        State::Running => "running",
        State::Stopping => "stopping",
        State::Completed => "completed",
        State::Failed => "failed",
        State::Unknown => "unknown",
    }
}

/// Run it on a thread of its own; see [`run_to_end`].
fn spawn_run(runs: Arc<dyn Runs>, state_dir: std::path::PathBuf, handle: String, ask: RunAsk) {
    std::thread::spawn(move || match Store::open(&state_dir) {
        Ok(store) => run_to_end(runs.as_ref(), &store, &handle, &ask),
        Err(err) => {
            tracing::error!(%err, "cannot open the rpc store; the session record stays stale");
        }
    });
}

/// Send the run unless a stop got there first, then write down how it
/// ended.
///
/// Both writes change the record as it is on disk at that moment, so a stop
/// recorded meanwhile keeps its flag, and an end already recorded is never
/// replaced. Every path leaves a terminal state behind: a record stuck at
/// `running` with nobody to finish it is exactly the case the owner check
/// has to rescue later.
pub(super) fn run_to_end(runs: &dyn Runs, store: &Store, handle: &str, ask: &RunAsk) {
    let send = store.update(handle, |r| {
        if r.state.terminal() {
            return false;
        }
        if r.stop_requested {
            r.state = State::Failed;
            r.finish = Some(Finish {
                error: Some("stopped before it was sent to the Agent".to_string()),
                ..Finish::default()
            });
            return false;
        }
        r.state = State::Running;
        r.dispatched = true;
        true
    });
    match send {
        Ok(Some(true)) => {}
        Ok(_) => return,
        Err(err) => {
            // Not recorded as sent, so not sent: a run nobody could stop
            // exactly is worse than one that never started.
            tracing::error!(%err, "cannot record that the session is being sent; not sending it");
            let _ = store.update(handle, |r| {
                if !r.state.terminal() {
                    r.state = State::Failed;
                    r.finish = Some(Finish {
                        error: Some(
                            "the session could not be recorded as sent, so it was not sent"
                                .to_string(),
                        ),
                        ..Finish::default()
                    });
                }
            });
            return;
        }
    }
    let (state, finish) = match runs.run_print(ask) {
        Ok(report) => finish_from(&report),
        Err(err) => (
            after_dispatch(&err),
            Finish {
                error: Some(err.message().to_string()),
                ..Finish::default()
            },
        ),
    };
    let written = store.update(handle, |r| {
        if !r.state.terminal() {
            r.state = state;
            r.finish = Some(finish);
        }
    });
    if let Err(err) = written {
        tracing::error!(%err, "cannot record how the session ended");
    }
}

/// What a run that failed after it was sent is, as far as this side knows.
///
/// Every refusal the Agent gives before it starts anything carries its own
/// code -- no controller, not logged in, the Runtime busy or unreachable, a
/// fenced id -- and that is `failed`: nothing ran. Two codes cannot promise
/// that. `AgentUnreachable` is ssh dying or timing out, which happens just
/// as well after the Agent started the run. `Internal` covers the Agent
/// failing once the supervisor was up and an answer that does not match
/// what was sent. Either may have left a run behind, so either is `unknown`:
/// `failed` would invite a retry of work that may already have changed the
/// tree.
///
/// Except when ssh itself says it never got that far (F14): a name that
/// does not resolve, a refused connection, a refused login. Before P63 those
/// were `unknown` too, which told a caller to go and inspect a run that
/// could not exist; on macOS the black-box tests never noticed, because a
/// longer temp directory tripped the ControlPath check first.
fn after_dispatch(err: &crate::Error) -> State {
    match err.code() {
        ErrorCode::AgentUnreachable if crate::ssh::never_reached(err.message()) => State::Failed,
        ErrorCode::AgentUnreachable | ErrorCode::Internal => State::Unknown,
        _ => State::Failed,
    }
}

fn finish_from(report: &RunReport) -> (State, Finish) {
    let outcome = &report.outcome;
    let state = if outcome.ok() {
        State::Completed
    } else {
        State::Failed
    };
    let result = report.result.as_ref();
    let tokens = result.and_then(|r| r.tokens());
    const TAIL: usize = 8192;
    let stdout = &report.stdout_tail;
    (
        state,
        Finish {
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            duration_ms: outcome.duration_ms,
            provider: Some(report.provider),
            ccnm_session: Some(report.session.clone()),
            provider_session_id: result
                .and_then(|r| r.provider_session_id())
                .map(str::to_string),
            text: result.and_then(|r| r.text()).map(str::to_string),
            total_cost_usd: result.and_then(|r| r.total_cost_usd()),
            input_tokens: tokens.map(|(input, _)| input),
            output_tokens: tokens.map(|(_, output)| output),
            output: tail(stdout, TAIL),
            output_total: stdout.len() as u64,
            stderr: tail(&report.stderr_tail, TAIL),
            error: outcome.error.clone(),
        },
    )
}

/// The last `max` bytes, cut on a character boundary.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let start = text.len() - max;
    let start = (start..text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_string()
}

fn new_session_id() -> String {
    // `s-` so a handle is recognisable in a log next to ccnm's own uuids.
    format!("s-{}", uuid::Uuid::new_v4().hyphenated())
}

/// RFC 3339 in UTC.
///
/// Written by hand rather than pulling in a date crate for one field. UTC
/// keeps it to arithmetic: no zone table, and `Z` is a valid offset, which
/// is what the protocol asks for.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339(secs)
}

fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

impl Context {
    fn store(&self) -> Result<Store, RpcError> {
        Store::open(&self.state).map_err(|e| wire::from_ccnm(&e))
    }

    fn owner_of(&self, record: &Record) -> OwnerCheck {
        if record.owner_pid == std::process::id() {
            // Our own record: the thread that owns it lives as long as we
            // do, so there is nothing to verify against `ps`.
            return OwnerCheck::Alive;
        }
        super::store::owner_check(
            self.runner.as_ref(),
            record.owner_pid,
            &record.owner_started,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_rfc3339_in_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_788_000_000), "2026-08-29T10:40:00Z");
        // Leap days and the century rule are where a hand-written calendar
        // goes wrong: 2024 is a leap year, 2000 is one only because of the
        // 400-year exception, and the last second of a year must not roll
        // the date forward.
        assert_eq!(rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_767_225_599), "2025-12-31T23:59:59Z");
        assert!(now_rfc3339().ends_with('Z'));
    }

    #[test]
    fn the_tail_never_splits_a_character() {
        let text = "。".repeat(100);
        let cut = tail(&text, 10);
        assert!(cut.len() <= 10);
        assert!(text.ends_with(&cut));
        assert_eq!(tail("short", 100), "short");
    }

    /// F14 (P62, found on Linux): ssh failing before it ever reached the
    /// Agent's shell cannot have started anything, so the run is `failed`
    /// -- a caller may retry it. Anything that may have happened after the
    /// remote command started stays `unknown`.
    #[test]
    fn an_ssh_that_never_reached_the_agent_is_failed_not_unknown() {
        let unreachable = |why: &str| {
            crate::Error::new(ErrorCode::AgentUnreachable, format!("ssh worker: {why}"))
        };
        for why in [
            // macOS and Linux (glibc) word a resolver failure differently.
            "ssh: Could not resolve hostname worker.invalid: nodename nor servname provided, or not known",
            "ssh: Could not resolve hostname worker.invalid: Name or service not known",
            "ssh: connect to host 100.79.121.33 port 22: Connection refused",
            "ssh: connect to host 100.79.121.33 port 22: Operation timed out",
            "ssh: connect to host 10.0.0.1 port 22: No route to host",
            "fodelf@100.79.121.33: Permission denied (publickey,keyboard-interactive).",
            "Host key verification failed.",
            "kex_exchange_identification: read: Connection reset by peer",
        ] {
            assert_eq!(after_dispatch(&unreachable(why)), State::Failed, "{why}");
        }
        for why in [
            "timed out after 60s",
            "Connection to 100.79.121.33 closed by remote host.",
            "client_loop: send disconnect: Broken pipe",
            "ssh exited 255 without a message",
            // A remote program's own words, not ssh's: not an auth failure.
            "caused by: Permission denied (os error 13)",
        ] {
            assert_eq!(after_dispatch(&unreachable(why)), State::Unknown, "{why}");
        }
        assert_eq!(
            after_dispatch(&crate::Error::internal("answer did not match")),
            State::Unknown
        );
        assert_eq!(
            after_dispatch(&crate::Error::new(ErrorCode::Auth, "not logged in")),
            State::Failed
        );
    }

    #[test]
    fn session_ids_are_prefixed_and_unique() {
        let a = new_session_id();
        assert!(a.starts_with("s-"), "{a}");
        assert_ne!(a, new_session_id());
    }

    fn report_with(result: Option<crate::provider::AgentResult>) -> RunReport {
        RunReport {
            protocol: 3,
            provider: crate::provider::AgentProvider::Claude,
            agent_identity: None,
            session: "ccnm-uuid-1".to_string(),
            session_dir: std::path::PathBuf::from("/private/state/sessions/ccnm-uuid-1"),
            controller: crate::controller::Context {
                hello: crate::protocol::hello::answer(&crate::protocol::hello::HelloRequest::new(
                    None,
                )),
                pid: 1,
                manager: Ok("Aqua".to_string()),
            },
            pid: 2,
            outcome: crate::session::Outcome {
                exit_code: Some(0),
                timed_out: false,
                duration_ms: 1,
                error: None,
                stopped: false,
            },
            result,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        }
    }

    /// The contract publishes `usage` and `cost`, and both providers parse
    /// them, but this conversion used to hardcode `None` and drop them on
    /// the floor — so the fields were promised and never sent.
    #[test]
    fn what_the_provider_reported_reaches_the_record() {
        let result = crate::provider::AgentResult::Claude(crate::provider::RunResult {
            is_error: false,
            subtype: None,
            result: None,
            session_id: None,
            num_turns: 1,
            duration_ms: 0,
            duration_api_ms: 0,
            total_cost_usd: 0.42,
            usage: crate::provider::Usage {
                input_tokens: 18422,
                output_tokens: 1204,
                ..Default::default()
            },
            permission_denials: vec![],
        });
        let (_, finish) = finish_from(&report_with(Some(result)));
        assert_eq!(finish.input_tokens, Some(18422));
        assert_eq!(finish.output_tokens, Some(1204));
        assert_eq!(finish.total_cost_usd, Some(0.42));
    }

    /// No result document at all: nothing to report, and nothing invented.
    #[test]
    fn an_absent_result_leaves_the_numbers_out() {
        let (_, finish) = finish_from(&report_with(None));
        assert_eq!(finish.input_tokens, None);
        assert_eq!(finish.output_tokens, None);
        assert_eq!(finish.total_cost_usd, None);
    }
}

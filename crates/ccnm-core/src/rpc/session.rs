//! `session.start` / `status` / `result` / `stop`.
//!
//! These call the same application functions the human CLI calls --
//! `launcher::run_print_with_agent`, `launcher::stop_selected` -- rather
//! than shelling out to `ccnm` and reading its output. Parsing a CLI's prose
//! would freeze wording into an API and lose everything the wording rounds
//! off.
//!
//! `session.start` answers as soon as it has a handle and hands the run to a
//! background thread. That is what lets a client disconnect and come back:
//! the record on disk, not the connection, is what a session belongs to.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::store::{Finish, KeyClaim, Launch, OwnerCheck, Record, State, Store};
use super::wire::{self, Effect, RpcError, code};
use super::{Context, reject_unknown, require_str};
use crate::config::Config;
// ccnm 自己的 Result 是单参数别名，和 std 的同名，起个别名免得混。
use crate::error::Result as CcnmResult;
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
}

/// What actually executes. The real one goes through `launcher`; tests put
/// in one that finishes immediately, so no test starts an Agent or dials
/// ssh.
pub trait Runs: Send + Sync + 'static {
    fn run_print(&self, ask: &RunAsk) -> CcnmResult<RunReport>;
    /// Stop whatever managed session the workspace currently has.
    fn stop(&self, workspace: &str, instance: Option<&str>) -> CcnmResult<bool>;
}

/// How long a run may take when the caller does not say.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);

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
                .filter(|key| !key.is_empty() && key.len() <= 128)
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

    // Claim the key before creating anything: whoever wins the claim is the
    // one that gets to start an Agent.
    let session = new_session_id();
    if let Some(key) = &start_key
        && let KeyClaim::Held(existing) = store
            .claim_key(&workspace, key, &session)
            .map_err(|e| wire::from_ccnm(&e))?
    {
        return reuse_or_conflict(ctx, &store, &existing, &launch);
    }

    let owner_pid = std::process::id();
    let record = Record {
        session: session.clone(),
        launch,
        start_key: start_key.clone(),
        state: State::Starting,
        accepted_at: now_rfc3339(),
        stop_requested: false,
        timeout_ms: Some(timeout.as_millis() as u64),
        owner_pid,
        owner_started: super::store::process_started(ctx.runner.as_ref(), owner_pid)
            .unwrap_or_default(),
        finish: None,
    };
    if let Err(err) = store.write(&record) {
        // Nothing was started, so give the key back rather than leaving it
        // pointing at a session that does not exist.
        if let Some(key) = &start_key {
            store.release_key(&workspace, key);
        }
        return Err(wire::from_ccnm(&err));
    }

    spawn_run(
        ctx.runs.clone(),
        ctx.state.clone(),
        record.clone(),
        RunAsk {
            workspace,
            instance,
            prompt,
            timeout,
        },
    );

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
    existing: &str,
    launch: &Launch,
) -> Result<Value, RpcError> {
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
    if let Some(output) = params.get("output") {
        let output = output
            .as_object()
            .ok_or_else(|| RpcError::refused(code::INVALID_PARAMS, "output must be an object"))?;
        reject_unknown(output, &["max_bytes", "cursor"])?;
        // One page is all this build keeps, so any cursor a caller sends
        // back is one this build never issued.
        if output.get("cursor").is_some_and(|c| !c.is_null()) {
            return Err(
                RpcError::refused(code::EXPIRED, "output cursor is no longer valid")
                    .with_reason("cursor_expired")
                    .with_session(id),
            );
        }
    }
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
    out["output"] = serde_json::json!({
        "bytes_total": finish.output_total,
        "truncated": finish.output_total > finish.output.len() as u64,
        "cursor": Value::Null,
        "tail": finish.output,
    });
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
    let mut record = load(ctx, id)?;
    let state = record.observed_state(ctx.owner_of(&record));
    if state.terminal() {
        // Idempotent: stopping something already over is a success, so a
        // client retrying does not have to check the state first.
        return Ok(serde_json::json!({
            "session": record.session,
            "state": state_name(state),
            "stop_requested": record.stop_requested,
        }));
    }

    // Addressed by workspace, not by ccnm session id, because in print mode
    // that id only comes back when the run ends. It is still precise: P3.3's
    // write guard means one workspace has at most one managed write session,
    // so "the workspace's session" is this one. The Agent side does the
    // process-group verification before it reports anything stopped.
    let instance = record.launch.agent.as_ref().map(|a| a.instance.clone());
    ctx.runs
        .stop(&record.launch.workspace, instance.as_deref())
        .map_err(|e| wire::from_ccnm(&e))?;

    let store = ctx.store()?;
    record.stop_requested = true;
    // Deliberately not a terminal state: the request was accepted, and only
    // an observed end -- process group, MCP transport, released write guard
    // -- makes it over. Reporting `completed` here would hand the write
    // permission to the next caller on a guess.
    record.state = State::Stopping;
    store.write(&record).map_err(|e| wire::from_ccnm(&e))?;
    Ok(serde_json::json!({
        "session": record.session,
        "state": "stopping",
        "stop_requested": true,
    }))
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

/// Run it, then write down what happened.
///
/// The thread owns the record from here on. Every exit path writes a
/// terminal state: a record stuck at `running` with nobody to finish it is
/// exactly the case the owner check has to rescue later.
fn spawn_run(runs: Arc<dyn Runs>, state_dir: std::path::PathBuf, mut record: Record, ask: RunAsk) {
    std::thread::spawn(move || {
        let store = match Store::open(&state_dir) {
            Ok(store) => store,
            Err(err) => {
                tracing::error!(%err, "cannot open the rpc store; the session record stays stale");
                return;
            }
        };
        record.state = State::Running;
        if let Err(err) = store.write(&record) {
            tracing::error!(%err, "cannot mark the session running");
        }
        let (state, finish) = match runs.run_print(&ask) {
            Ok(report) => finish_from(&report),
            Err(err) => (
                State::Failed,
                Finish {
                    error: Some(err.message().to_string()),
                    ..Finish::default()
                },
            ),
        };
        // A stop that was asked for mid-run keeps its flag; the state comes
        // from what actually happened.
        record.state = state;
        record.finish = Some(finish);
        if let Err(err) = store.write(&record) {
            tracing::error!(%err, "cannot record how the session ended");
        }
    });
}

fn finish_from(report: &RunReport) -> (State, Finish) {
    let outcome = &report.outcome;
    let state = if outcome.ok() {
        State::Completed
    } else {
        State::Failed
    };
    let result = report.result.as_ref();
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
            total_cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            output: tail(stdout, TAIL),
            output_total: stdout.len() as u64,
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

    #[test]
    fn session_ids_are_prefixed_and_unique() {
        let a = new_session_id();
        assert!(a.starts_with("s-"), "{a}");
        assert_ne!(a, new_session_id());
    }
}

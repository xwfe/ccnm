//! What ccnm kept for a workspace, removed only after a preview and only by
//! the account that owns it (P61).
//!
//! Three accounts keep three kinds of things, and each removes its own:
//!
//! | who | what | where, in its own state directory |
//! | --- | --- | --- |
//! | Agent | a session's record, stdout/stderr and views; with `--purge`, the CLI's working directory | `sessions/<id>/`, `workspaces/<name>/` |
//! | Runtime Executor | what `exec_command` kept for a session | `sessions/<id>/output/` |
//! | Operator | `ccnm rpc`'s copy of a session's output and the large parts of its record | `rpc/outputs/<handle>/`, `rpc/sessions/<handle>.json` |
//!
//! The Operator asks the Agent, and the Agent asks the Runtime over its own
//! link, which lands on the account the session's tools ran as. Before P61
//! the Operator instead removed `sessions/<id>/` from *its own* state
//! directory, which in the recommended deployment is not where the
//! Runtime's output is: the Executor's copy stayed and nothing could find
//! it again (P57 E).
//!
//! Never removed: the project, the write guard, anything a session still
//! uses, anything whose state cannot be told, and the start key of a
//! Machine API session -- a key that forgot its session would start the
//! task a second time. Cleanup is not recovery: it does not kill, unlock or
//! take over anything.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Resolved;
use crate::error::{Error, ErrorCode, Result};
use crate::instance::{AgentIdentity, InstanceRef};
use crate::lang::Lang;
use crate::launcher::Env;
use crate::process::ProcessRunner;
use crate::protocol::payload::Protocol;
use crate::protocol::run::SessionState;
use crate::rpc::store::{Cleaned, OwnerCheck, Record, State, Store};
use crate::session;
use crate::work::Tools;

/// The internal wire version of a cleanup, Agent and Runtime alike. A peer
/// that predates it refuses with `CCNM_E_VERSION`; one that still speaks
/// `agent-purge` (protocol 1) is refused by this build, not served with the
/// old "delete every record of the workspace" behaviour.
pub const CLEANUP_PROTOCOL: u32 = 10;

/// How long a preview's token may be applied. Long enough to read the list;
/// short enough that "I checked it" still means something.
pub const PREVIEW_VALID: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Operator,
    Agent,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The Agent's `sessions/<id>/`.
    AgentSession,
    /// The Agent's `workspaces/<name>/`; only when the workspace goes.
    AgentWorkdir,
    /// The Runtime Executor's `sessions/<id>/output/`.
    RuntimeOutput,
    /// The Operator's output copy and the large parts of the record; the
    /// rest of the record stays as a tombstone.
    RpcResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    Remove,
    Keep,
}

/// Why something is kept, or was not removed after all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The session has not ended.
    NotEnded,
    /// Whether it ended cannot be told.
    Unknown,
    /// The Runtime's write guard is not free and names this session, or
    /// names nobody readable. What it kept is recovery evidence.
    Guard,
    /// Something uses it right now.
    InUse,
    /// Not a plain directory: a link or something else ccnm did not make.
    NotADirectory,
    /// Owned by another account than the one answering.
    ForeignOwner,
    /// Whether something uses it could not be checked (`ps` failed).
    Unchecked,
    /// The Runtime could not be asked. Its half of this session may still
    /// be there, and this record is how a later cleanup finds it.
    RuntimeNotAsked,
    /// The Runtime half of this session is kept, or was not removed.
    RuntimeHalfPending,
    /// Different now from what was previewed.
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Done {
    Removed,
    Skipped,
    Failed,
}

/// One thing a cleanup would remove or keep. `version` is opaque and
/// belongs to the node that owns the thing: its size and newest change, so
/// that anything written, replaced or removed since the preview is seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub role: Role,
    pub kind: Kind,
    pub id: String,
    pub bytes: u64,
    pub version: String,
    pub plan: Plan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<Why>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<Done>,
    /// Why it failed, in words that name no private path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Item {
    fn new(
        role: Role,
        kind: Kind,
        id: &str,
        bytes: u64,
        version: String,
        why: Option<Why>,
    ) -> Item {
        Item {
            role,
            kind,
            id: id.to_string(),
            bytes,
            version,
            plan: if why.is_some() {
                Plan::Keep
            } else {
                Plan::Remove
            },
            why,
            done: None,
            detail: None,
        }
    }

    fn keep(&mut self, why: Why) {
        if self.plan == Plan::Remove {
            self.plan = Plan::Keep;
            self.why = Some(why);
        }
    }

    fn finish(mut self, done: Done, why: Option<Why>, detail: Option<String>) -> Item {
        self.done = Some(done);
        if why.is_some() {
            self.why = why;
        }
        self.detail = detail;
        self
    }

    fn same(&self, other: &Item) -> bool {
        self.role == other.role && self.kind == other.kind && self.id == other.id
    }
}

// ---- wire ----

/// `ccnm internal agent-cleanup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCleanupRequest {
    pub protocol: u32,
    pub workspace: String,
    /// `None` for a workspace that names only an Agent Node (legacy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<InstanceRef>,
    pub runtime_node: String,
    /// The workspace is going away: its working directory goes too.
    #[serde(default)]
    pub purge: bool,
    /// Absent: preview. Present: remove exactly these, each checked again
    /// against what is there now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<Vec<Item>>,
}

impl Protocol for AgentCleanupRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        CLEANUP_PROTOCOL
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCleanupReport {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentity>,
    /// The account that answered on the Agent, by uid.
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_uid: Option<u32>,
    /// Why the Runtime could not be asked, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_error: Option<String>,
    pub items: Vec<Item>,
}

impl Protocol for AgentCleanupReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        CLEANUP_PROTOCOL
    }
}

/// One of the workspace's sessions as the Agent knows it: the Runtime keeps
/// no record of which workspace a session belonged to or how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnd {
    pub id: String,
    pub ended: bool,
}

/// `ccnm internal runtime-cleanup`, sent by the Agent over its own link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCleanupRequest {
    pub protocol: u32,
    pub workspace: String,
    /// The Agent Node asking; it must be the one the workspace is bound to.
    pub node: String,
    pub sessions: Vec<SessionEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<Vec<Item>>,
}

impl Protocol for RuntimeCleanupRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        CLEANUP_PROTOCOL
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCleanupReport {
    pub protocol: u32,
    pub workspace: String,
    pub uid: Option<u32>,
    /// The session the write guard names while it is not free. The Agent
    /// keeps that session's record too: recovery starts from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_session: Option<String>,
    /// The guard is not free and names nobody readable: everything stays.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guard_unclear: bool,
    pub items: Vec<Item>,
}

impl Protocol for RuntimeCleanupReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        CLEANUP_PROTOCOL
    }
}

// ---- shared ----

/// Bytes under `path` and a version for it, without following links.
///
/// The version is the size and the newest modification time anywhere
/// inside: a file written, added, replaced or removed since changes one of
/// them. `skip` names a child directory that belongs to someone else.
fn measure(path: &Path, skip: Option<&str>) -> (u64, String) {
    let mut bytes = 0u64;
    let mut newest = 0u128;
    let mut stack = vec![path.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&next) else {
            continue;
        };
        let changed = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |at| at.as_nanos());
        newest = newest.max(changed);
        if meta.is_dir() {
            let Ok(entries) = fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                if next == path && skip.is_some_and(|skip| entry.file_name() == skip) {
                    continue;
                }
                stack.push(entry.path());
            }
        } else {
            bytes += meta.len();
        }
    }
    (bytes, format!("{bytes}-{newest}"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An error kind for a report, never the path it happened at.
fn failure(error: &std::io::Error) -> String {
    format!("{:?}", error.kind())
}

// ---- Runtime ----

/// What the Runtime looks up once per request.
struct Facts {
    uid: Option<u32>,
    guard_session: Option<String>,
    guard_unclear: bool,
    /// Sessions an `mcp-serve` serves now; `None` when `ps` could not say.
    live: Option<Vec<String>>,
}

impl Facts {
    fn gather(state: &Path, root: &Path, runner: &dyn ProcessRunner) -> Facts {
        use crate::mcp::write_guard::Observed;
        let seen = crate::mcp::write_guard::observe(state, root, runner);
        let owner = seen.owner.map(|owner| owner.session);
        let (guard_session, guard_unclear) = match seen.state {
            Observed::Free => (None, false),
            // A live holder is served, so `live` already keeps it.
            Observed::Held => (owner, false),
            Observed::Abandoned | Observed::Unknown => {
                let unclear = owner.is_none();
                (owner, unclear)
            }
        };
        Facts {
            uid: crate::runtime::current_uid(runner),
            guard_session,
            guard_unclear,
            live: crate::overview::try_scan_servers(runner)
                .map(|servers| servers.into_iter().map(|server| server.session).collect()),
        }
    }
}

/// `ccnm internal runtime-cleanup`: the Runtime Executor's half, answered
/// from its own state directory.
pub fn runtime(
    config: &crate::config::Config,
    request: &RuntimeCleanupRequest,
    state: &Path,
    runner: &dyn ProcessRunner,
) -> Result<RuntimeCleanupReport> {
    if request.protocol != CLEANUP_PROTOCOL {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "cleanup request is protocol {}, this Runtime answers protocol {CLEANUP_PROTOCOL}",
                request.protocol
            ),
        ));
    }
    // Every id becomes a path below; refuse the request before building one.
    let named = request
        .sessions
        .iter()
        .map(|s| s.id.as_str())
        .chain(request.apply.iter().flatten().map(|item| item.id.as_str()));
    for id in named {
        if !session::valid_id(id) {
            return Err(Error::invalid_args(
                "cleanup names something that is not a ccnm session id",
            ));
        }
    }
    let root = crate::runtime::bound_root(config, &request.workspace, &request.node)?;
    let facts = Facts::gather(state, &root, runner);
    let current: Vec<Item> = request
        .sessions
        .iter()
        .filter_map(|s| runtime_item(state, &s.id, s.ended, &facts))
        .collect();
    let items = match &request.apply {
        None => current,
        Some(asked) => asked
            .iter()
            .filter(|item| {
                item.role == Role::Runtime
                    && item.kind == Kind::RuntimeOutput
                    && item.plan == Plan::Remove
            })
            .map(|item| apply_runtime(state, item, &current))
            .collect(),
    };
    Ok(RuntimeCleanupReport {
        protocol: CLEANUP_PROTOCOL,
        workspace: request.workspace.clone(),
        uid: facts.uid,
        guard_session: facts.guard_session,
        guard_unclear: facts.guard_unclear,
        items,
    })
}

fn runtime_item(state: &Path, id: &str, ended: bool, facts: &Facts) -> Option<Item> {
    let session = crate::paths::session_dir(state, id);
    let output = session.join("output");
    let not_a_dir = || {
        Some(Item::new(
            Role::Runtime,
            Kind::RuntimeOutput,
            id,
            0,
            "-".into(),
            Some(Why::NotADirectory),
        ))
    };
    if !fs::symlink_metadata(&session).ok()?.is_dir() {
        return not_a_dir();
    }
    let meta = fs::symlink_metadata(&output).ok()?;
    if !meta.is_dir() {
        return not_a_dir();
    }
    let (bytes, version) = measure(&output, None);
    let why = if facts.uid.is_some_and(|uid| meta.uid() != uid) {
        Some(Why::ForeignOwner)
    } else if !ended {
        Some(Why::NotEnded)
    } else if facts.guard_unclear || facts.guard_session.as_deref() == Some(id) {
        Some(Why::Guard)
    } else {
        match &facts.live {
            None => Some(Why::Unchecked),
            Some(live) if live.iter().any(|served| served == id) => Some(Why::InUse),
            Some(_) if crate::mcp::retention::any_in_progress(&output) => Some(Why::InUse),
            Some(_) => None,
        }
    };
    Some(Item::new(
        Role::Runtime,
        Kind::RuntimeOutput,
        id,
        bytes,
        version,
        why,
    ))
}

fn apply_runtime(state: &Path, asked: &Item, current: &[Item]) -> Item {
    let Some(now) = current.iter().find(|item| item.same(asked)) else {
        return asked
            .clone()
            .finish(Done::Skipped, Some(Why::Changed), None);
    };
    if now.plan != Plan::Remove {
        return asked.clone().finish(Done::Skipped, now.why, None);
    }
    if now.version != asked.version {
        return asked
            .clone()
            .finish(Done::Skipped, Some(Why::Changed), None);
    }
    let session = crate::paths::session_dir(state, &asked.id);
    match fs::remove_dir_all(session.join("output")) {
        Ok(()) => {
            // Only if nothing else is in it: with one state directory for
            // both roles, the Agent's record lives beside it.
            let _ = fs::remove_dir(&session);
            asked.clone().finish(Done::Removed, None, None)
        }
        Err(e) => asked.clone().finish(Done::Failed, None, Some(failure(&e))),
    }
}

// ---- Agent ----

/// `ccnm internal agent-cleanup`: the Agent's half, and the Runtime's half
/// asked over this Agent's own link to it.
///
/// On apply the Runtime goes first. An Agent record whose Runtime half was
/// not removed stays, because it is the only thing that says which session
/// ids belong to this workspace: without it a later cleanup could not find
/// that output again.
pub fn agent(request: &AgentCleanupRequest, tools: &Tools<'_>) -> Result<AgentCleanupReport> {
    let identity = request
        .agent
        .as_ref()
        .map(|agent| tools.config.resolve_identity(agent))
        .transpose()?;
    let node = match &identity {
        Some(identity) => identity.node.clone(),
        None => tools.config.this.clone().ok_or_else(|| {
            Error::config("this machine's config.toml has no `this`, so it cannot say which Agent Node is asking")
        })?,
    };
    let uid = crate::runtime::current_uid(tools.runner);
    let mut found = agent_items(tools, &request.workspace, request.purge, uid);
    let sessions: Vec<SessionEnd> = found
        .iter()
        .filter(|item| item.kind == Kind::AgentSession)
        .map(|item| SessionEnd {
            id: item.id.clone(),
            ended: !matches!(item.why, Some(Why::NotEnded | Why::Unknown)),
        })
        .collect();
    let runtime_apply = request.apply.as_ref().map(|asked| {
        asked
            .iter()
            .filter(|item| item.role == Role::Runtime && item.plan == Plan::Remove)
            .cloned()
            .collect::<Vec<_>>()
    });
    // Asked on apply too, even with nothing of its own to remove: the
    // guard may have come to name one of these sessions since the preview.
    let runtime = match tools.runtime_link(&request.runtime_node) {
        // Colocated: the Runtime's output would be inside the session
        // directory itself, and goes with it.
        Ok(None) => Ok(None),
        Ok(Some(link)) => {
            let ssh = crate::ssh::Ssh::new(&link.alias, &tools.control_dir).map(|ssh| {
                let ssh = ssh.with_ccnm_bin(&link.ccnm_bin);
                match &identity {
                    Some(identity) => ssh.for_provider(identity.provider),
                    None => ssh,
                }
            });
            ssh.and_then(|ssh| {
                ssh.call_ccnm::<_, RuntimeCleanupReport>(
                    tools.runner,
                    crate::ssh::Master::Reuse,
                    &["internal", "runtime-cleanup"],
                    &RuntimeCleanupRequest {
                        protocol: CLEANUP_PROTOCOL,
                        workspace: request.workspace.clone(),
                        node: node.clone(),
                        sessions: sessions.clone(),
                        apply: runtime_apply.clone(),
                    },
                    Duration::from_secs(600),
                    ErrorCode::RuntimeUnreachable,
                )
            })
            .map(Some)
        }
        Err(e) => Err(e),
    };
    let (runtime, runtime_error) = match runtime {
        Ok(Some(report)) if report.workspace == request.workspace => (Some(report), None),
        Ok(Some(_)) => (
            None,
            Some("the Runtime answered about another workspace".to_string()),
        ),
        Ok(None) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    };
    let runtime_uid = runtime.as_ref().and_then(|r| r.uid);
    // What the Runtime said decides what stays here.
    if let Some(report) = &runtime {
        for item in found.iter_mut().filter(|i| i.kind == Kind::AgentSession) {
            if report.guard_unclear || report.guard_session.as_deref() == Some(item.id.as_str()) {
                item.keep(Why::Guard);
            }
        }
    }
    let items = match &request.apply {
        None => {
            if runtime_error.is_some() {
                for item in found.iter_mut().filter(|i| i.kind == Kind::AgentSession) {
                    item.keep(Why::RuntimeNotAsked);
                }
            }
            let runtime_items = runtime.map(|r| r.items).unwrap_or_default();
            for item in found.iter_mut().filter(|i| i.kind == Kind::AgentSession) {
                if runtime_items
                    .iter()
                    .any(|r| r.id == item.id && r.plan == Plan::Keep)
                {
                    item.keep(Why::RuntimeHalfPending);
                }
            }
            runtime_items.into_iter().chain(found).collect()
        }
        Some(asked) => {
            let runtime_done: Vec<Item> = match (&runtime, &runtime_error) {
                (Some(report), _) => report.items.clone(),
                (None, error) => runtime_apply
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| {
                        let detail = error
                            .clone()
                            .unwrap_or_else(|| "this Agent has no link to the Runtime".into());
                        item.finish(Done::Failed, None, Some(detail))
                    })
                    .collect(),
            };
            let pending = |id: &str| {
                asked
                    .iter()
                    .filter(|item| item.role == Role::Runtime && item.id == id)
                    .any(|item| {
                        runtime_done
                            .iter()
                            .find(|done| done.same(item))
                            .is_none_or(|done| done.done != Some(Done::Removed))
                    })
            };
            let agent_done: Vec<Item> = asked
                .iter()
                .filter(|item| item.role == Role::Agent && item.plan == Plan::Remove)
                .map(|item| {
                    if item.kind == Kind::AgentSession && pending(&item.id) {
                        return item.clone().finish(
                            Done::Skipped,
                            Some(Why::RuntimeHalfPending),
                            None,
                        );
                    }
                    apply_agent(tools, &request.workspace, item, &found)
                })
                .collect();
            runtime_done.into_iter().chain(agent_done).collect()
        }
    };
    Ok(AgentCleanupReport {
        protocol: CLEANUP_PROTOCOL,
        agent_identity: identity,
        uid,
        runtime_uid,
        runtime_error,
        items,
    })
}

/// This workspace's sessions on the Agent, and with `purge` its working
/// directory. Only directories named by a session id, holding a record that
/// names this workspace: anything else is not attributable to it and is not
/// looked at, let alone listed.
fn agent_items(tools: &Tools<'_>, workspace: &str, purge: bool, uid: Option<u32>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut all_ended = true;
    if let Ok(entries) = fs::read_dir(crate::paths::sessions_dir(&tools.state)) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !session::valid_id(&id) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let dir = session::Dir::at(entry.path());
            let Ok(spec) = session::load(&dir) else {
                continue;
            };
            if spec.id != id || spec.workspace != workspace {
                continue;
            }
            let outcome = session::read_outcome(&dir).ok().flatten();
            let state = crate::work::session_state(&spec, &dir, outcome.as_ref(), tools);
            // `output/` in here is the Runtime's, when one state directory
            // serves both roles: it is measured and removed by the Runtime.
            let (bytes, version) = measure(dir.path(), Some("output"));
            let why = match state {
                _ if uid.is_some_and(|uid| meta.uid() != uid) => Some(Why::ForeignOwner),
                SessionState::Completed | SessionState::Failed => None,
                SessionState::Unknown => Some(Why::Unknown),
                SessionState::Starting | SessionState::Running | SessionState::Stopping => {
                    Some(Why::NotEnded)
                }
            };
            all_ended &= !matches!(why, Some(Why::NotEnded | Why::Unknown));
            items.push(Item::new(
                Role::Agent,
                Kind::AgentSession,
                &id,
                bytes,
                version,
                why,
            ));
        }
    }
    if purge {
        let path = crate::paths::workspace_dir(&tools.state, workspace);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            let (bytes, version) = measure(&path, None);
            let why = if !meta.is_dir() {
                Some(Why::NotADirectory)
            } else if uid.is_some_and(|uid| meta.uid() != uid) {
                Some(Why::ForeignOwner)
            } else if !all_ended {
                // A session that may still run is working in it.
                Some(Why::InUse)
            } else {
                None
            };
            items.push(Item::new(
                Role::Agent,
                Kind::AgentWorkdir,
                workspace,
                bytes,
                version,
                why,
            ));
        }
    }
    items
}

fn apply_agent(tools: &Tools<'_>, workspace: &str, asked: &Item, current: &[Item]) -> Item {
    let Some(now) = current.iter().find(|item| item.same(asked)) else {
        return asked
            .clone()
            .finish(Done::Skipped, Some(Why::Changed), None);
    };
    if now.plan != Plan::Remove {
        return asked.clone().finish(Done::Skipped, now.why, None);
    }
    if now.version != asked.version {
        return asked
            .clone()
            .finish(Done::Skipped, Some(Why::Changed), None);
    }
    let removed = match asked.kind {
        Kind::AgentSession => {
            let dir = session::Dir::at(crate::paths::session_dir(&tools.state, &asked.id));
            // Step aside for a start, a stop or a view being built rather
            // than pulling files out from under it.
            match session::Control::try_lock(&dir) {
                Ok(Some(_held)) => fs::remove_dir_all(dir.path()),
                Ok(None) => return asked.clone().finish(Done::Skipped, Some(Why::InUse), None),
                Err(e) => Err(std::io::Error::other(e.to_string())),
            }
        }
        Kind::AgentWorkdir => {
            fs::remove_dir_all(crate::paths::workspace_dir(&tools.state, workspace))
        }
        Kind::RuntimeOutput | Kind::RpcResult => {
            return asked
                .clone()
                .finish(Done::Skipped, Some(Why::Changed), None);
        }
    };
    match removed {
        Ok(()) => asked.clone().finish(Done::Removed, None, None),
        Err(e) => asked.clone().finish(Done::Failed, None, Some(failure(&e))),
    }
}

// ---- Operator ----

/// The Operator's own `ccnm rpc` records of this workspace. A tombstone
/// with no copy left is not listed: there is nothing more to remove, and
/// its start key has to stay.
fn operator_items(
    store: &Store,
    workspace: &str,
    owner_of: &dyn Fn(&Record) -> OwnerCheck,
) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for id in store.handles()? {
        let Ok(Some(record)) = store.read(&id) else {
            continue;
        };
        if record.launch.workspace != workspace {
            continue;
        }
        let Some(item) = operator_item(store, &record, owner_of) else {
            continue;
        };
        items.push(item);
    }
    Ok(items)
}

fn operator_item(
    store: &Store,
    record: &Record,
    owner_of: &dyn Fn(&Record) -> OwnerCheck,
) -> Option<Item> {
    let outputs = store.outputs_of(&record.session)?;
    let copy = fs::symlink_metadata(&outputs).ok();
    if record.cleaned_at.is_some() && copy.is_none() {
        return None;
    }
    let (copy_bytes, copy_version) = match &copy {
        Some(_) => measure(&outputs, None),
        None => (0, "-".into()),
    };
    let kept = record.finish.as_ref().map_or(0, |finish| {
        finish.text.as_ref().map_or(0, String::len) + finish.output.len() + finish.stderr.len()
    });
    let json = serde_json::to_vec(record).ok()?;
    let version = format!("{:016x}-{copy_version}", crate::paths::fnv1a(&json));
    let why = if copy.as_ref().is_some_and(|meta| !meta.is_dir()) {
        Some(Why::NotADirectory)
    } else if record.cleaned_at.is_some() {
        // A copy made after the record was cleaned: nothing reads it.
        None
    } else {
        match record.observed_state(owner_of(record)) {
            State::Completed | State::Failed => None,
            State::Unknown => Some(Why::Unknown),
            State::Starting | State::Running | State::Stopping => Some(Why::NotEnded),
        }
    };
    Some(Item::new(
        Role::Operator,
        Kind::RpcResult,
        &record.session,
        copy_bytes + kept as u64,
        version,
        why,
    ))
}

fn apply_operator(store: &Store, asked: &Item, owner_of: &dyn Fn(&Record) -> OwnerCheck) -> Item {
    let cleaned = store.clean(&asked.id, |record| {
        operator_item(store, record, owner_of)
            .is_some_and(|now| now.plan == Plan::Remove && now.version == asked.version)
    });
    match cleaned {
        Ok(Some(Cleaned::Done)) => asked.clone().finish(Done::Removed, None, None),
        Ok(Some(Cleaned::InUse)) => asked.clone().finish(Done::Skipped, Some(Why::InUse), None),
        Ok(Some(Cleaned::Changed) | None) => {
            asked
                .clone()
                .finish(Done::Skipped, Some(Why::Changed), None)
        }
        Err(e) => asked
            .clone()
            .finish(Done::Failed, None, Some(e.message().to_string())),
    }
}

// ---- Operator: preview, token, apply ----

/// Everything a cleanup would do, from the nodes that own each thing.
#[derive(Debug, Clone)]
pub struct Preview {
    pub workspace: String,
    pub operator_uid: Option<u32>,
    pub agent_uid: Option<u32>,
    pub runtime_uid: Option<u32>,
    /// The Agent could not be asked: nothing of its or the Runtime's is
    /// listed, and nothing of theirs will be removed.
    pub agent_error: Option<String>,
    pub runtime_error: Option<String>,
    pub items: Vec<Item>,
    pub token: String,
    pub expires: u64,
    purge: bool,
}

/// What an apply did.
#[derive(Debug, Clone)]
pub struct Applied {
    pub preview: Preview,
    pub items: Vec<Item>,
}

impl Applied {
    /// Every item the preview meant to remove was removed, and every node
    /// was asked. Kept items were planned and do not count against it.
    pub fn complete(&self) -> bool {
        self.preview.agent_error.is_none()
            && self.preview.runtime_error.is_none()
            && self
                .items
                .iter()
                .filter(|item| item.plan == Plan::Remove)
                .all(|item| item.done == Some(Done::Removed))
    }

    /// Nothing ccnm kept for the workspace is left: what `--purge` needs
    /// before it may forget where the workspace was.
    pub fn nothing_left(&self) -> bool {
        self.complete() && self.items.iter().all(|item| item.plan == Plan::Remove)
    }
}

/// List what a cleanup of this workspace would do. Removes nothing.
pub fn preview(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    state: &Path,
    purge: bool,
) -> Result<Preview> {
    collect(
        resolved,
        env,
        state,
        purge,
        now_secs() + PREVIEW_VALID.as_secs(),
    )
}

/// Carry out a preview the person has seen, if it still describes what is
/// there. The list is collected again from every node and must give the
/// same token; each node then checks every item once more as it removes it.
pub fn apply(resolved: &Resolved<'_>, env: &Env<'_>, state: &Path, token: &str) -> Result<Applied> {
    let expires = token
        .split_once('-')
        .and_then(|(expires, _)| u64::from_str_radix(expires, 16).ok())
        .ok_or_else(|| {
            Error::invalid_args("that is not a token `ccnm cleanup` printed; preview again")
        })?;
    if now_secs() > expires {
        return Err(Error::new(
            ErrorCode::NotReady,
            "the preview has expired; run `ccnm cleanup <workspace>` again and apply the new token",
        ));
    }
    let fresh = collect(resolved, env, state, false, expires)?;
    if fresh.token != token {
        return Err(Error::new(
            ErrorCode::NotReady,
            "what is there is no longer what was previewed (a session ran, a file changed, or the config did); nothing was removed -- preview again",
        ));
    }
    Ok(carry_out(resolved, env, state, fresh))
}

/// `workspace remove --purge`: the same list, applied at once. The flag is
/// the confirmation, as it always was; what changed is that the list comes
/// from the owners and anything that stays is reported, so the caller can
/// keep the workspace's config until nothing is left.
pub fn purge(resolved: &Resolved<'_>, env: &Env<'_>, state: &Path) -> Result<Applied> {
    let fresh = collect(
        resolved,
        env,
        state,
        true,
        now_secs() + PREVIEW_VALID.as_secs(),
    )?;
    Ok(carry_out(resolved, env, state, fresh))
}

fn collect(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    state: &Path,
    purge: bool,
    expires: u64,
) -> Result<Preview> {
    let store = Store::open(state)?;
    let owner_of = |record: &Record| {
        crate::rpc::store::owner_check(env.runner, record.owner_pid, &record.owner_started)
    };
    let mut items = operator_items(&store, resolved.name, &owner_of)?;
    // A workspace only external MCP clients use has no Agent Node, so no
    // Agent sessions; the Runtime drops an external session's output when
    // it disconnects. Nothing to ask is not "could not ask".
    let (agent, agent_error) = match resolved.agent {
        None => (None, None),
        Some(_) => match crate::launcher::cleanup(resolved, env, purge, None) {
            Ok(report) => (Some(report), None),
            Err(e) => (None, Some(e.to_string())),
        },
    };
    if let Some(report) = &agent {
        items.extend(report.items.iter().cloned());
    }
    items.sort_by(|a, b| (a.role, a.kind, &a.id, a.plan).cmp(&(b.role, b.kind, &b.id, b.plan)));
    let mut preview = Preview {
        workspace: resolved.name.to_string(),
        operator_uid: crate::runtime::current_uid(env.runner),
        agent_uid: agent.as_ref().and_then(|a| a.uid),
        runtime_uid: agent.as_ref().and_then(|a| a.runtime_uid),
        runtime_error: agent.as_ref().and_then(|a| a.runtime_error.clone()),
        agent_error,
        items,
        token: String::new(),
        expires,
        purge,
    };
    preview.token = token(
        resolved,
        &preview,
        agent.as_ref().and_then(|a| a.agent_identity.as_ref()),
    );
    Ok(preview)
}

/// What the token binds: the configuration that decided where to ask, who
/// answered, and every item with its version. Not a secret -- anyone who
/// can run the preview can make one -- but a check that what is applied is
/// what was read.
fn token(resolved: &Resolved<'_>, preview: &Preview, identity: Option<&AgentIdentity>) -> String {
    let binding = serde_json::json!({
        "expires": preview.expires,
        "workspace": resolved.name,
        "root": resolved.workspace.root,
        "agent": resolved.workspace.agent,
        "agent_node": resolved.workspace.agent_node,
        "runtime_node": resolved.workspace.runtime_node,
        "identity": identity,
        "uids": [preview.operator_uid, preview.agent_uid, preview.runtime_uid],
        "agent_asked": preview.agent_error.is_none(),
        "runtime_asked": preview.runtime_error.is_none(),
        "purge": preview.purge,
        "items": preview.items,
    });
    format!(
        "{:x}-{:016x}",
        preview.expires,
        crate::paths::fnv1a(binding.to_string().as_bytes())
    )
}

fn carry_out(resolved: &Resolved<'_>, env: &Env<'_>, state: &Path, preview: Preview) -> Applied {
    let mut done: Vec<Item> = Vec::new();
    match Store::open(state) {
        Ok(store) => {
            let owner_of = |record: &Record| {
                crate::rpc::store::owner_check(env.runner, record.owner_pid, &record.owner_started)
            };
            for item in preview
                .items
                .iter()
                .filter(|i| i.role == Role::Operator && i.plan == Plan::Remove)
            {
                done.push(apply_operator(&store, item, &owner_of));
            }
        }
        Err(e) => {
            for item in preview
                .items
                .iter()
                .filter(|i| i.role == Role::Operator && i.plan == Plan::Remove)
            {
                done.push(
                    item.clone()
                        .finish(Done::Failed, None, Some(e.message().to_string())),
                );
            }
        }
    }
    let remote: Vec<Item> = preview
        .items
        .iter()
        .filter(|i| i.role != Role::Operator && i.plan == Plan::Remove)
        .cloned()
        .collect();
    if !remote.is_empty() {
        match crate::launcher::cleanup(resolved, env, preview.purge, Some(remote.clone())) {
            Ok(report) => {
                for item in remote {
                    let answered = report.items.iter().find(|done| done.same(&item)).cloned();
                    done.push(answered.unwrap_or_else(|| {
                        item.finish(
                            Done::Skipped,
                            Some(Why::Changed),
                            Some("the Agent did not report it".into()),
                        )
                    }));
                }
            }
            Err(e) => {
                for item in remote {
                    done.push(item.finish(Done::Failed, None, Some(e.to_string())));
                }
            }
        }
    }
    for item in preview.items.iter().filter(|i| i.plan == Plan::Keep) {
        done.push(item.clone().finish(Done::Skipped, None, None));
    }
    done.sort_by(|a, b| (a.role, a.kind, &a.id).cmp(&(b.role, b.kind, &b.id)));
    Applied {
        preview,
        items: done,
    }
}

// ---- words ----

/// `1.5 MiB`, the way `du -h` would say it.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn role_word(role: Role) -> &'static str {
    match role {
        Role::Operator => "Operator",
        Role::Agent => "Agent",
        Role::Runtime => "Runtime",
    }
}

fn kind_word(kind: Kind, lang: Lang) -> &'static str {
    match kind {
        Kind::AgentSession => lang.pick("会话记录", "session record"),
        Kind::AgentWorkdir => lang.pick("CLI 工作目录", "CLI working dir"),
        Kind::RuntimeOutput => lang.pick("exec_command 输出", "exec_command output"),
        Kind::RpcResult => lang.pick("Machine API 结果", "Machine API result"),
    }
}

fn why_words(why: Why, lang: Lang) -> &'static str {
    match why {
        Why::NotEnded => lang.pick("还没结束", "not ended"),
        Why::Unknown => lang.pick("说不清结没结束", "cannot tell whether it ended"),
        Why::Guard => lang.pick(
            "写锁标着它（恢复要用），先按运维手册处理写锁",
            "the write guard names it (recovery needs it); deal with the guard first",
        ),
        Why::InUse => lang.pick("正在被用", "in use"),
        Why::NotADirectory => {
            lang.pick("不是 ccnm 建的普通目录", "not a plain directory ccnm made")
        }
        Why::ForeignOwner => lang.pick("属于别的账号", "owned by another account"),
        Why::Unchecked => lang.pick(
            "查不了有没有人在用（ps 跑不了）",
            "cannot check whether it is in use (no ps)",
        ),
        Why::RuntimeNotAsked => lang.pick(
            "Runtime 问不到，这条先留着，否则以后找不到 Runtime 那一半",
            "the Runtime could not be asked; kept so its half can still be found",
        ),
        Why::RuntimeHalfPending => lang.pick(
            "Runtime 那一半没删，这条先留着",
            "its Runtime half stays, so this stays too",
        ),
        Why::Changed => lang.pick("和预览时不一样了", "changed since the preview"),
    }
}

fn uid_word(uid: Option<u32>) -> String {
    uid.map_or("uid ?".into(), |uid| format!("uid {uid}"))
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

impl Preview {
    fn uid_of(&self, role: Role) -> Option<u32> {
        match role {
            Role::Operator => self.operator_uid,
            Role::Agent => self.agent_uid,
            Role::Runtime => self.runtime_uid,
        }
    }

    fn unreached(&self, lang: Lang) -> String {
        let mut out = String::new();
        if let Some(e) = &self.agent_error {
            out.push_str(&lang.pick(
                format!("! Agent 问不到（{e}）：它和 Runtime 上的东西这次都没列、也不会动\n"),
                format!("! could not ask the Agent ({e}): nothing of its or the Runtime's is listed or touched\n"),
            ));
        }
        if let Some(e) = &self.runtime_error {
            out.push_str(&lang.pick(
                format!("! Runtime 问不到（{e}）：它的输出没列，Agent 上对应的会话记录也先留着\n"),
                format!("! could not ask the Runtime ({e}): its output is not listed, and the Agent's records stay\n"),
            ));
        }
        out
    }

    /// The preview, for a person: what goes, what stays and why, and the
    /// one command that carries it out.
    pub fn render(&self, lang: Lang) -> String {
        let mut out = lang.pick(
            format!("预览 {}：只是看，什么都没删\n", self.workspace),
            format!("preview for {}: nothing has been removed\n", self.workspace),
        );
        out.push_str(&self.unreached(lang));
        if self.items.is_empty() {
            out.push_str(lang.pick(
                "ccnm 没为它留下可清理的东西\n",
                "ccnm kept nothing for it that could be cleaned\n",
            ));
            return out;
        }
        let rows: Vec<Vec<String>> = self
            .items
            .iter()
            .map(|item| {
                let what = match (item.plan, item.why) {
                    (Plan::Remove, _) if item.kind == Kind::RpcResult => lang
                        .pick(
                            "删（记录和 start_key 留着，之后 session.result 回 expired）",
                            "remove (record and start_key stay; session.result then says expired)",
                        )
                        .to_string(),
                    (Plan::Remove, _) => lang.pick("删", "remove").to_string(),
                    (Plan::Keep, why) => lang.pick(
                        format!("留：{}", why.map_or("", |w| why_words(w, lang))),
                        format!("keep: {}", why.map_or("", |w| why_words(w, lang))),
                    ),
                };
                vec![
                    String::new(),
                    role_word(item.role).to_string(),
                    uid_word(self.uid_of(item.role)),
                    kind_word(item.kind, lang).to_string(),
                    short(&item.id).to_string(),
                    size(item.bytes),
                    what,
                ]
            })
            .collect();
        out.push_str(&crate::overview::table(&rows));
        let removing: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| i.plan == Plan::Remove)
            .collect();
        if removing.is_empty() {
            out.push_str(lang.pick(
                "没有可以删的，都留着\n",
                "nothing can be removed; everything stays\n",
            ));
            return out;
        }
        let bytes: u64 = removing.iter().map(|i| i.bytes).sum();
        let minutes = PREVIEW_VALID.as_secs() / 60;
        out.push_str(&lang.pick(
            format!(
                "会删 {} 项、{}。确认无误后执行（{minutes} 分钟内有效；这期间清单有任何变化，apply 会拒绝）：\n  ccnm cleanup {} --apply {}\n",
                removing.len(),
                size(bytes),
                self.workspace,
                self.token
            ),
            format!(
                "{} item(s), {}, would be removed. To do it (valid for {minutes} minutes; any change meanwhile makes apply refuse):\n  ccnm cleanup {} --apply {}\n",
                removing.len(),
                size(bytes),
                self.workspace,
                self.token
            ),
        ));
        out
    }
}

impl Applied {
    pub fn render(&self, lang: Lang) -> String {
        let mut out = lang.pick(
            format!("清理 {}\n", self.preview.workspace),
            format!("cleanup of {}\n", self.preview.workspace),
        );
        out.push_str(&self.preview.unreached(lang));
        let rows: Vec<Vec<String>> = self
            .items
            .iter()
            .map(|item| {
                let (word, why) = match item.done {
                    Some(Done::Removed) => (lang.pick("删了", "removed"), String::new()),
                    Some(Done::Failed) => (
                        lang.pick("失败", "FAILED"),
                        item.detail.clone().unwrap_or_default(),
                    ),
                    _ => (
                        lang.pick("留着", "kept"),
                        item.why
                            .map(|w| why_words(w, lang).to_string())
                            .unwrap_or_default(),
                    ),
                };
                vec![
                    String::new(),
                    word.to_string(),
                    role_word(item.role).to_string(),
                    kind_word(item.kind, lang).to_string(),
                    short(&item.id).to_string(),
                    size(item.bytes),
                    why,
                ]
            })
            .collect();
        out.push_str(&crate::overview::table(&rows));
        let removed: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| i.done == Some(Done::Removed))
            .collect();
        let bytes: u64 = removed.iter().map(|i| i.bytes).sum();
        let left = self.items.len() - removed.len();
        out.push_str(&lang.pick(
            format!(
                "删了 {} 项、{}；留着 {left} 项\n",
                removed.len(),
                size(bytes)
            ),
            format!(
                "removed {} item(s), {}; {left} left\n",
                removed.len(),
                size(bytes)
            ),
        ));
        if !self.complete() {
            out.push_str(lang.pick(
                "没做完：再预览一次（ccnm cleanup <workspace>）就能对剩下的重试，已删的不会重来\n",
                "not finished: preview again (ccnm cleanup <workspace>) to retry what is left; what was removed stays removed\n",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests;

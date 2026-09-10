//! Runtime authority: what the Runtime Executor decides for itself.
//!
//! One question — *which project, where, opened for whom* — and one place
//! that answers it, from this machine's own config and filesystem.
//!
//! The old serve payload carries a `root`. It is checked against the
//! Runtime's workspace definition when the payload is a bound one, but the
//! shape still asks a caller where the project is, and a caller that is
//! trusted for the path is trusted for the machine: every tool call, the
//! write guard, the retained output and the safety verdict all hang off
//! that directory. [`OpenPayload`] has no `root` field at all. It names a
//! workspace and says who is calling; the Runtime looks up the rest.
//!
//! This is the boundary P7.4 Batch B asks for
//! (docs/plan/runtime-surfaces.md). It is deliberately reachable without a
//! launcher: `open` is a pure function of a config and a request, so the
//! whole decision can be tested offline, and the public launcher still
//! sends the old payload until Batch C switches the control path over.
//!
//! What it does **not** do is replace the checks `mcp-serve` makes when it
//! opens: binding, safety audit, root canonicalization and write guard all
//! run again there. A resolve is not a capability token — the answer can be
//! stale by the time the server starts, so the server asks again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::instance::{AgentIdentity, WorkspaceBinding, identifier};
use crate::process::ProcessRunner;
use crate::protocol::mcp::ServePayload;
use crate::protocol::payload::Protocol;

/// The wire version of a Runtime-authority open.
///
/// It is a separate number from the bound-session protocol (3) because it
/// is a different message with a different trust model, and an old peer
/// must fail loudly rather than read it as something it understands.
pub const OPEN_PROTOCOL: u32 = 4;

/// The only tool policy that exists. A caller may not invent one.
const POLICY: &str = "coding";

/// A request to open a workspace on this Runtime.
///
/// Note what is missing: **no root, no node addresses, no paths at all.**
/// The caller names a workspace it was told about and states the Agent
/// identity it resolved locally; everything else is the Runtime's to
/// decide. `deny_unknown_fields` makes that a wire property rather than a
/// convention — a peer that tries to send a root gets a decode error, not
/// a quietly ignored field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenPayload {
    pub protocol: u32,
    /// Workspace name, resolved in *this* machine's registry.
    pub workspace: String,
    /// Who the caller resolved itself to be.
    ///
    /// The Runtime checks the half it owns — the workspace's Agent Node —
    /// and the topology and capability that go with it. The instance,
    /// provider and profile are the Agent's local facts: `ccnm run --agent`
    /// is a supported override, so another instance *on that node* is not
    /// an attack, and the Agent's own registry is what accepts or rejects
    /// it. Checking it twice here would break the override and prove
    /// nothing the Agent does not already prove.
    pub agent: AgentIdentity,
    /// Session id chosen by the launcher; names the retained-output
    /// directory later, so it is validated here rather than trusted.
    pub session: String,
    pub policy: String,
    /// Whether a person is at a terminal for this session.
    pub interactive: bool,
}

impl OpenPayload {
    pub fn new(workspace: &str, agent: AgentIdentity, session: &str) -> Self {
        OpenPayload {
            protocol: OPEN_PROTOCOL,
            workspace: workspace.to_string(),
            agent,
            session: session.to_string(),
            policy: POLICY.to_string(),
            interactive: false,
        }
    }

    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
}

impl Protocol for OpenPayload {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        OPEN_PROTOCOL
    }
}

/// The Runtime's own answer. Every field here came from this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub workspace: String,
    /// Canonical, resolved on this host. Not the caller's idea of it.
    pub root: PathBuf,
    pub binding: WorkspaceBinding,
    /// `runtime_user`: the identity the Runtime Executor is expected to be.
    pub runtime_user: Option<String>,
    pub allow_unconfined_exec: bool,
}

impl Opened {
    /// The in-process shape the MCP server already knows how to open, with
    /// the root this Runtime resolved rather than one a caller supplied.
    ///
    /// Kept as a conversion instead of a second server constructor so the
    /// new path lands on exactly the same startup sequence — binding
    /// re-verified, audit, canonicalization, write guard — as the old one.
    pub fn serve_payload(&self, request: &OpenPayload) -> ServePayload {
        ServePayload::new(&self.workspace, self.root.clone(), &request.session)
            .with_binding(self.binding.clone())
            .with_interactive(request.interactive)
    }
}

/// Resolve an open request against this Runtime's authoritative config.
///
/// Fails rather than guesses. The caller learns that the workspace is not
/// openable for that identity, not where anything lives: an error here is
/// read by the other machine.
pub fn open(config: &Config, request: &OpenPayload) -> Result<Opened> {
    if request.protocol != OPEN_PROTOCOL {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "open request is protocol {}, this Runtime opens protocol {OPEN_PROTOCOL}",
                request.protocol
            ),
        ));
    }
    if request.policy != POLICY {
        return Err(Error::policy("unknown tool policy"));
    }
    // The session id becomes a directory name on this machine.
    identifier(&request.session)?;
    // bind_workspace is the cross-check: the workspace exists here, this
    // machine is its authoritative Runtime, the caller's node is the one it
    // names, and the topology and provider capability support the binding.
    let binding = config.bind_workspace(&request.workspace, &request.agent)?;
    let root = canonical_root(&binding.root)?;
    let runtime_user = config
        .nodes
        .get(&binding.runtime_node)
        .and_then(|node| node.runtime_user.clone());
    let allow_unconfined_exec = config
        .workspaces
        .get(&request.workspace)
        .is_some_and(|workspace| workspace.allow_unconfined_exec);
    Ok(Opened {
        workspace: request.workspace.clone(),
        root,
        binding,
        runtime_user,
        allow_unconfined_exec,
    })
}

/// What the Agent Node asks the Runtime before starting a session on its
/// own machine (P7.4 Batch C).
///
/// The Agent Node holds no workspace list and must not grow one — two
/// registries are two answers to "where is this project", and one of them
/// goes stale. So it asks. What it used to do instead was send the whole
/// public `ccnm run` back to the Runtime over ssh and let *that* machine
/// start the session, which meant the Runtime Executor ran the launcher and
/// had to hold an outbound key to the Agent Node. That is the hop this
/// message removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub protocol: u32,
    pub workspace: String,
    /// `--agent`: another instance on the workspace's own Agent Node. The
    /// Runtime pins the node; only the instance name is the caller's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl ResolveRequest {
    pub fn new(workspace: &str, agent: Option<&str>) -> Self {
        ResolveRequest {
            protocol: OPEN_PROTOCOL,
            workspace: workspace.to_string(),
            agent: agent.map(str::to_string),
        }
    }
}

impl Protocol for ResolveRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        OPEN_PROTOCOL
    }
}

/// Everything the Agent needs to start the session locally, and nothing
/// else: no credentials, no runtime identity, no state paths. The project
/// root is here because the Agent records and displays it, not because the
/// Agent may choose it — an open request cannot carry one back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveReport {
    pub protocol: u32,
    pub workspace: String,
    pub root: PathBuf,
    pub runtime_node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::instance::InstanceRef>,
    #[serde(rename = "claude_config_dir")]
    pub provider_config_dir: Option<PathBuf>,
    pub permission_mode: crate::config::PermissionMode,
}

impl Protocol for ResolveReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        OPEN_PROTOCOL
    }
}

/// Answer one Agent-side resolve from this Runtime's authoritative config.
///
/// Read-only: it starts nothing, creates no session and touches no state.
/// The Agent may call it and then never start anything, and calling it
/// twice must give the same answer.
pub fn resolve(config: &Config, request: &ResolveRequest) -> Result<ResolveReport> {
    if request.protocol != OPEN_PROTOCOL {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "resolve request is protocol {}, this Runtime answers protocol {OPEN_PROTOCOL}",
                request.protocol
            ),
        ));
    }
    let resolved = config.workspace(&request.workspace)?;
    let agent = resolved.agent_reference(request.agent.as_deref())?;
    let provider = crate::provider::AgentProvider::current();
    Ok(ResolveReport {
        protocol: OPEN_PROTOCOL,
        workspace: request.workspace.clone(),
        root: resolved.workspace.root.clone(),
        runtime_node: resolved.workspace.runtime_node.clone(),
        agent,
        provider_config_dir: provider.config_dir(resolved.agent).map(Path::to_path_buf),
        permission_mode: provider.permission_mode(resolved.workspace),
    })
}

/// What the Runtime Executor is asked about itself (P7.4 Batch D).
///
/// Doctor cannot answer this from the machine it runs on. Its identity
/// checks judge the process that calls them, so run by an Operator they
/// describe the Operator — P7.3 saw the same workspace read 0 failed as
/// `ccrun` and 7 failed as the operator's own login, in the same minute.
/// The account that matters is the one the Agent's ssh lands on, so the
/// question is asked over that connection and answered there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRequest {
    pub protocol: u32,
    pub workspace: String,
}

impl AuditRequest {
    pub fn new(workspace: &str) -> Self {
        AuditRequest {
            protocol: OPEN_PROTOCOL,
            workspace: workspace.to_string(),
        }
    }
}

impl Protocol for AuditRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        OPEN_PROTOCOL
    }
}

/// Whether the project is usable by the identity that would run its tools
/// — not whether a directory happens to be there.
///
/// "The directory is writable" was the old claim, and P7.3 showed what it
/// misses: a work tree owned by somebody else is writable through a 0777
/// parent and still makes every git command fail, while doctor's row stays
/// green. Ownership and git's own verdict are the two facts that decide it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootStatus {
    pub present: bool,
    pub is_dir: bool,
    /// Owned by the identity that answered, by uid.
    pub owned: bool,
    pub git: GitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    /// A repository this identity can work in.
    Usable,
    /// Not a repository at all, which is not a problem.
    NotARepo,
    /// Git refused because the repository belongs to someone else. Every
    /// git command fails this way, including the ones an Agent runs.
    RefusedOwnership,
    /// Git could not be asked: not installed, or it failed in a way this
    /// does not recognise. Reported as unknown rather than as either
    /// answer.
    Unknown,
}

impl RootStatus {
    pub fn of(root: &Path, runner: &dyn ProcessRunner) -> RootStatus {
        let meta = std::fs::metadata(root).ok();
        let present = meta.is_some();
        let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
        let owned = meta.as_ref().is_some_and(|meta| {
            use std::os::unix::fs::MetadataExt;
            current_uid(runner).is_some_and(|uid| uid == meta.uid())
        });
        RootStatus {
            present,
            is_dir,
            owned,
            git: git_status(root, runner),
        }
    }

    /// Nothing here stops the project being served.
    pub fn usable(&self) -> bool {
        self.present && self.is_dir && self.git != GitStatus::RefusedOwnership
    }
}

fn current_uid(runner: &dyn ProcessRunner) -> Option<u32> {
    let out = runner
        .run(&crate::process::Cmd::new("/usr/bin/id").args(["-u"]))
        .ok()?;
    out.success()
        .then(|| out.stdout_lossy().trim().parse().ok())?
}

fn git_status(root: &Path, runner: &dyn ProcessRunner) -> GitStatus {
    let cmd = crate::process::Cmd::new("git")
        .args(["-C", &root.to_string_lossy(), "rev-parse", "--git-dir"])
        .timeout(std::time::Duration::from_secs(10));
    let Ok(out) = runner.run(&cmd) else {
        return GitStatus::Unknown;
    };
    if out.success() {
        return GitStatus::Usable;
    }
    let said = out.stderr_lossy().to_lowercase();
    if said.contains("dubious ownership") {
        GitStatus::RefusedOwnership
    } else if said.contains("not a git repository") {
        GitStatus::NotARepo
    } else {
        GitStatus::Unknown
    }
}

/// The Runtime Executor's own answer about itself and the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub protocol: u32,
    pub audit: crate::safety::Audit,
    pub root: RootStatus,
    /// Whether this workspace has accepted an unconfined Runtime, as the
    /// Runtime's own config says. It rides along because it decides how a
    /// finding is rendered, and the copy that matters is the one
    /// `exec_command`'s gate reads -- not the reader's.
    #[serde(default)]
    pub allow_unconfined_exec: bool,
}

impl Protocol for AuditReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        OPEN_PROTOCOL
    }
}

/// Answer an audit request as whoever is running this process.
///
/// Meant to be reached over the Agent's ssh into the Runtime Executor, so
/// that the verdict describes the account that will really execute the
/// tools. Nothing here reveals a path outside the workspace, an
/// environment value or a credential: the findings are the same structured
/// conclusions `exec_command`'s own gate uses.
pub fn audit(
    config: &Config,
    request: &AuditRequest,
    runner: &dyn ProcessRunner,
) -> Result<AuditReport> {
    if request.protocol != OPEN_PROTOCOL {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "audit request is protocol {}, this Runtime answers protocol {OPEN_PROTOCOL}",
                request.protocol
            ),
        ));
    }
    let resolved = config.workspace(&request.workspace)?;
    let home = crate::paths::home_dir().unwrap_or_else(|_| PathBuf::from("/nonexistent"));
    Ok(AuditReport {
        protocol: OPEN_PROTOCOL,
        audit: crate::safety::audit(resolved.runtime.runtime_user.as_deref(), &home, runner),
        root: RootStatus::of(&resolved.workspace.root, runner),
        allow_unconfined_exec: resolved.workspace.allow_unconfined_exec,
    })
}

/// What arrived on `internal mcp-serve --payload`.
///
/// Two shapes, told apart by the `protocol` number that every ccnm message
/// carries. There is no third option and no fallback: a number this build
/// does not know is `CCNM_E_VERSION`, because the alternative — trying the
/// other shape and hoping — is how a new chain silently runs on old rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeRequest {
    /// Protocol 1..=3. Carries a caller-supplied root.
    Legacy(ServePayload),
    /// Protocol 4. Runtime authority decides the root.
    Managed(OpenPayload),
}

/// Decode a serve payload without deciding in advance which shape it is.
pub fn decode_serve(text: &str) -> Result<ServeRequest> {
    #[derive(Deserialize)]
    struct Version {
        protocol: u32,
    }
    let bytes = base64_decode(text)?;
    let version: Version = serde_json::from_slice(&bytes).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            "serve payload has no protocol number; ccnm versions probably differ",
        )
        .with_source(e)
    })?;
    match version.protocol {
        OPEN_PROTOCOL => Ok(ServeRequest::Managed(crate::protocol::payload::decode(
            text,
        )?)),
        1..=3 => Ok(ServeRequest::Legacy(crate::protocol::payload::decode(
            text,
        )?)),
        other => Err(Error::new(
            ErrorCode::Version,
            format!(
                "serve payload is protocol {other}; this ccnm serves 1..=3 (root from the caller) and {OPEN_PROTOCOL} (root from this Runtime)"
            ),
        )),
    }
}

fn base64_decode(text: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|e| {
            Error::new(
                ErrorCode::Version,
                "payload is not base64url; ccnm versions probably differ",
            )
            .with_source(e)
        })
}

/// The workspace root as this host sees it: symlinks resolved, existence
/// proved, and a directory rather than a file.
///
/// `CCNM_E_WRONG_WORKSPACE` because that is what it means — this machine
/// cannot serve that project — and the launcher surfaces it as a failed
/// `initialize` rather than an MCP session that half works.
pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(root).map_err(|e| {
        Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace root {} is not usable on this host",
                root.display()
            ),
        )
        .with_source(e)
    })?;
    if !canonical.is_dir() {
        return Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!("workspace root {} is not a directory", canonical.display()),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::AgentProvider;

    fn config(root: &Path) -> Config {
        let toml = format!(
            r#"
this = "runtime"

[nodes.runtime]
runtime_user = "ccrun"

[nodes.agent]
ssh = "agent-node"

[agents.claude-main]
provider = "claude"
profile_ref = "default"

[workspaces.demo]
root = "{}"
agent = {{ node = "agent", instance = "claude-main" }}
"#,
            root.display()
        );
        toml::from_str(&toml).expect("test config")
    }

    fn identity() -> AgentIdentity {
        AgentIdentity {
            node: "agent".into(),
            instance: "claude-main".into(),
            provider: AgentProvider::Claude,
            profile_ref: "default".into(),
        }
    }

    fn workspace_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-runtime-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        dir
    }

    #[test]
    fn the_root_comes_from_this_machine_and_is_canonical() {
        let dir = workspace_dir("root");
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("project"), &link).unwrap();
        // The config points at the symlink; the answer is the real path.
        let config = config(&link);
        let opened = open(&config, &OpenPayload::new("demo", identity(), "s-1")).unwrap();
        assert_eq!(opened.root, dir.join("project").canonicalize().unwrap());
        assert_eq!(opened.runtime_user.as_deref(), Some("ccrun"));
        assert_eq!(opened.binding.agent, identity());
    }

    /// The whole point of the new shape: there is nowhere to put a root.
    /// Not "it is ignored" — the message does not decode.
    #[test]
    fn a_request_carrying_a_root_is_not_a_valid_open() {
        let json = serde_json::json!({
            "protocol": OPEN_PROTOCOL,
            "workspace": "demo",
            "agent": {"node": "agent", "instance": "claude-main",
                      "provider": "claude", "profile_ref": "default"},
            "session": "s-1",
            "policy": "coding",
            "interactive": false,
            "root": "/tmp/somewhere-else",
        });
        let err = serde_json::from_value::<OpenPayload>(json).unwrap_err();
        assert!(err.to_string().contains("root"), "{err}");
    }

    /// The Runtime authorizes the *machine*, not the instance: which
    /// Agent Node may open this workspace is its decision, and which
    /// instance runs there is the Agent's, because `ccnm run --agent`
    /// deliberately lets an operator pick another instance on the same
    /// node. An identity naming a different node is refused; an identity
    /// naming another instance on the right node binds here and is then
    /// resolved — or rejected — by the Agent's own registry.
    #[test]
    fn the_runtime_authorizes_the_agent_node_and_leaves_the_instance_to_it() {
        let dir = workspace_dir("identity");
        let config = config(&dir.join("project"));

        let elsewhere = OpenPayload::new(
            "demo",
            AgentIdentity {
                node: "elsewhere".into(),
                ..identity()
            },
            "s-1",
        );
        assert!(open(&config, &elsewhere).is_err());

        let override_instance = AgentIdentity {
            instance: "claude-other".into(),
            ..identity()
        };
        let opened = open(
            &config,
            &OpenPayload::new("demo", override_instance.clone(), "s-1"),
        )
        .unwrap();
        assert_eq!(opened.binding.agent, override_instance);
    }

    #[test]
    fn an_unknown_workspace_is_refused_rather_than_guessed() {
        let dir = workspace_dir("unknown");
        let config = config(&dir.join("project"));
        let request = OpenPayload::new("not-a-workspace", identity(), "s-1");
        assert!(open(&config, &request).is_err());
    }

    /// A session id names a directory on this machine, so it is validated
    /// at the boundary rather than after it has been joined onto a path.
    #[test]
    fn a_session_id_that_is_not_an_identifier_is_refused() {
        let dir = workspace_dir("session");
        let config = config(&dir.join("project"));
        for bad in ["../escape", "with/slash", ""] {
            let request = OpenPayload::new("demo", identity(), bad);
            assert!(open(&config, &request).is_err(), "{bad}");
        }
        // A real session id is a hyphenated uuid and must still pass.
        let ok = OpenPayload::new("demo", identity(), &crate::session::new_id());
        assert!(open(&config, &ok).is_ok());
    }

    #[test]
    fn a_policy_the_runtime_does_not_have_is_refused() {
        let dir = workspace_dir("policy");
        let config = config(&dir.join("project"));
        let mut request = OpenPayload::new("demo", identity(), "s-1");
        request.policy = "readonly".into();
        assert!(open(&config, &request).is_err());
    }

    #[test]
    fn the_two_wire_shapes_are_told_apart_by_their_protocol_number() {
        let legacy = crate::protocol::payload::encode(&ServePayload::new(
            "demo",
            PathBuf::from("/tmp/demo"),
            "s-1",
        ))
        .unwrap();
        assert!(matches!(
            decode_serve(&legacy).unwrap(),
            ServeRequest::Legacy(_)
        ));

        let managed =
            crate::protocol::payload::encode(&OpenPayload::new("demo", identity(), "s-1")).unwrap();
        assert!(matches!(
            decode_serve(&managed).unwrap(),
            ServeRequest::Managed(_)
        ));
    }

    /// The other half of the migration boundary: a build that only knows
    /// the old shape cannot read this one as something familiar. It has no
    /// `root`, which the old payload requires, and an `agent` field, which
    /// it forbids — so the failure is a version error on the peer, not a
    /// session that opens with a default root.
    #[test]
    fn an_old_peer_cannot_read_a_managed_request_as_a_legacy_one() {
        let wire =
            crate::protocol::payload::encode(&OpenPayload::new("demo", identity(), "s-1")).unwrap();
        let err = crate::protocol::payload::decode::<ServePayload>(&wire).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
    }

    /// A protocol number from the future must stop the session, not be
    /// retried as an older shape until one of them parses.
    #[test]
    fn an_unknown_protocol_is_a_version_error_not_a_fallback() {
        let json = serde_json::json!({"protocol": OPEN_PROTOCOL + 1, "workspace": "demo"});
        let wire = {
            use base64::Engine as _;
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&json).unwrap())
        };
        let err = decode_serve(&wire).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("protocol 5"), "{err}");
    }

    #[test]
    fn the_resolved_payload_carries_the_runtime_root_not_the_request() {
        let dir = workspace_dir("payload");
        let config = config(&dir.join("project"));
        let request = OpenPayload::new("demo", identity(), "s-1").with_interactive(true);
        let opened = open(&config, &request).unwrap();
        let payload = opened.serve_payload(&request);
        assert_eq!(payload.root, opened.root);
        assert_eq!(payload.binding.as_ref(), Some(&opened.binding));
        assert!(payload.interactive);
        assert_eq!(payload.protocol, crate::instance::INSTANCE_SESSION_PROTOCOL);
    }
}

//! One Agent session on the Agent Node: what it is, where its files
//! live, and the supervisor that runs it.
//!
//! ```text
//! ~/.local/state/ccnm/sessions/<uuid>/          (Agent Node)
//! ├── session.json     the Spec: everything needed to start it
//! ├── mcp.json         --mcp-config: the one ssh transport to the Runtime Node
//! ├── settings.json    --settings: which tools run without a prompt, and which are denied
//! ├── stdout           Claude's stdout (in print mode, the JSON result)
//! ├── stderr           Claude's stderr
//! ├── supervisor.log   the supervisor's own diagnostics
//! └── exit             written last: how Claude ended, as JSON
//! ```
//!
//! The same id names `sessions/<uuid>/output/` on the Runtime Node, where
//! `exec_command` keeps what commands printed. One session, two halves.
//!
//! # Who writes what
//!
//! The ssh-side `agent-run` creates the directory and the three inputs: it
//! is the same account on the same disk, and none of that needs a login
//! session. The controller does the one thing that does — spawn the
//! supervisor — and nothing else. The supervisor runs Claude and writes
//! the outputs. No file has two writers.
//!
//! # Why a supervisor process at all
//!
//! Something has to be Claude's parent: only a parent gets the exit
//! status, and the ssh side wants it. The controller could be that parent
//! and watch its children on threads, but then a controller restart (every
//! `ccnm controller install` after a binary upgrade) would orphan or
//! kill every running session. A supervisor is a process that lives
//! exactly as long as its Claude, in its own process group, and owes the
//! controller nothing once started. Design doc section 23: the session's
//! lifetime is Claude's, not any outer process's.

pub mod transport;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::PermissionMode;
use crate::error::{Error, Result};
use crate::paths;
use crate::process;
use crate::protocol::mcp::ServePayload;
use crate::protocol::payload::{self, PROTOCOL, Protocol};
use crate::provider::AgentProvider;
use crate::ssh::Ssh;

/// The tools `ccnm internal mcp-serve` offers, as the settings allow-list
/// needs them. A test in the server module keeps this in step with the
/// real `tools/list`.
///
/// Seven until P36 added `load_skill`, P39 `view_image`, P40
/// `read_notebook` and P41 `stop_command`. New tools go
/// last so that the order of the first seven -- which golden fixtures and
/// people's memory both hold -- does not move.
pub const MCP_TOOLS: [&str; 11] = [
    "workspace_info",
    "read_file",
    "list_files",
    "search_text",
    "apply_patch",
    "exec_command",
    "read_output",
    "load_skill",
    "view_image",
    "read_notebook",
    "stop_command",
];

// Compatibility export; native tool names belong to the provider.
pub use crate::provider::claude::NATIVE_TOOLS_DENIED;

/// The transport program, absolute. Claude starts it from launchd's
/// environment, whose `PATH` is not a login shell's.
pub const SSH_BIN: &str = "/usr/bin/ssh";

/// Everything needed to start a session, written once by whoever creates
/// it and read by the supervisor. Carries `protocol` because it crosses a
/// process boundary and outlives the binary that wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<crate::instance::AgentIdentity>,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    /// The ccnm lifecycle UUID. Provider thread/resume ids stay in results.
    pub id: String,
    pub workspace: String,
    /// Project root on the Runtime Node. Never a path on this one.
    pub root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_node: Option<String>,
    /// How this machine dials the Runtime Node: its own alias for it, and
    /// where ccnm lives there.
    ///
    /// `None` when the project is on this machine too, which is the whole
    /// difference between the two topologies the supervisor can be asked
    /// for: with a link it writes an MCP config and denies Claude's native
    /// tools, without one Claude works the project directly.
    #[serde(default)]
    pub runtime: Option<RuntimeLink>,
    #[serde(default)]
    #[serde(rename = "claude_config_dir")]
    pub provider_config_dir: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    pub mode: Mode,
    /// Claude is killed after this many seconds. The supervisor's hard
    /// limit, so a session that wedges cannot outlive everyone waiting.
    pub timeout_secs: u64,
    /// Claude's working directory on the Agent Node: the workspace's
    /// long-lived state directory, never the project (which is not here).
    /// Stable per workspace, so Claude's own session storage under
    /// `~/.claude/projects/` collects in one place instead of one
    /// directory per run.
    pub cwd: PathBuf,
    /// Codex runs its own execution tools through the Runtime's
    /// `exec-serve` (P23), not ccnm's MCP server. Only a bound, interactive
    /// Codex session can be one; [`Spec::validate_identity`] says so. Absent
    /// in every record written before the field existed, which is exactly
    /// what those sessions were.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub codex_exec_server: bool,
    /// Which native Agent features a remote session keeps (P46). A record
    /// written before the field existed reads as the default, web search
    /// on; that only matters if such a record is launched by a newer
    /// binary, and then it gets what every new session gets.
    #[serde(default, skip_serializing_if = "crate::config::AgentTools::is_default")]
    pub agent_tools: crate::config::AgentTools,
}

impl Spec {
    pub fn validate_identity(&self) -> Result<()> {
        if self.codex_exec_server {
            // The chain runs Codex's tools, opens on the Runtime by bound
            // identity, and `codex exec` cannot reach a project that is
            // not on this machine (P21.1). Each of these is checked again
            // where it matters; this is the record refusing to describe a
            // session that cannot exist.
            if self.provider != AgentProvider::Codex
                || self.agent_identity.is_none()
                || !self.mode.is_interactive()
            {
                return Err(Error::invalid_args(
                    "the exec-server chain needs a bound, interactive Codex session",
                ));
            }
        }
        if let Some(identity) = &self.agent_identity {
            identity.validate()?;
            crate::instance::identifier(&self.workspace)?;
            if self.runtime.is_none() || !crate::instance::profiles::absolute(&self.root) {
                return Err(Error::invalid_args(
                    "instance session requires an absolute Runtime root and a remote link",
                ));
            }
            let runtime_node = self.runtime_node.as_deref().ok_or_else(|| {
                Error::invalid_args("instance session has no Runtime Node binding")
            })?;
            crate::instance::identifier(runtime_node)?;
            if runtime_node == identity.node {
                return Err(Error::invalid_args(
                    "instance session cannot claim colocated execution",
                ));
            }
            if self.provider_config_dir.is_some()
                || self.permission_mode != PermissionMode::default()
            {
                return Err(Error::invalid_args(
                    "instance session cannot carry legacy profile or permission overrides",
                ));
            }
            if identity.provider != self.provider {
                return Err(Error::invalid_args(
                    "session provider differs from its bound Agent identity",
                ));
            }
        }
        Ok(())
    }

    pub fn check_agent_binding(&self, identity: &crate::instance::AgentIdentity) -> Result<()> {
        self.validate_identity()?;
        if self.agent_identity.as_ref() != Some(identity) {
            return Err(Error::invalid_args(
                "session Agent identity does not match the resolved binding",
            ));
        }
        Ok(())
    }

    pub fn workspace_binding(&self) -> Result<Option<crate::instance::WorkspaceBinding>> {
        self.validate_identity()?;
        self.agent_identity
            .as_ref()
            .map(|identity| {
                let binding = crate::instance::WorkspaceBinding {
                    workspace: self.workspace.clone(),
                    runtime_node: self.runtime_node.clone().expect("validated above"),
                    root: self.root.clone(),
                    agent: identity.clone(),
                };
                binding.validate()?;
                Ok(binding)
            })
            .transpose()
    }
    /// v1 sessions have no provider selector. Preserve old records and default
    /// behavior until a real second provider establishes the next contract.
    pub fn provider(&self) -> AgentProvider {
        self.provider
    }
}

impl Protocol for Spec {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.agent_identity.is_some() {
            crate::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            self.provider.control_protocol()
        }
    }
}

/// The Runtime Node as this machine dials it.
///
/// Resolved on the Agent Node from its own config, never sent by whoever
/// asked for the session: an ssh alias means something only to the machine
/// whose `~/.ssh/config` defines it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLink {
    pub alias: String,
    pub ccnm_bin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Mode {
    /// `claude -p`: one prompt in, one JSON result out, no terminal.
    Print { prompt: String },
    /// The real Claude Code terminal, inside tmux on the Agent Node, with
    /// the person's own terminal attached over ssh (design doc section 23).
    /// The optional prompt is what it starts with; without one it opens
    /// empty.
    Interactive {
        #[serde(default)]
        prompt: Option<String>,
    },
}

impl Mode {
    pub fn is_interactive(&self) -> bool {
        matches!(self, Mode::Interactive { .. })
    }
}

/// A fresh session id. Hyphenated UUID v4: valid for `claude --session-id`,
/// and passes [`paths::safe_name`] untouched.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// The directory of one session and the fixed names inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir(PathBuf);

impl Dir {
    pub fn at(path: impl Into<PathBuf>) -> Dir {
        Dir(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn meta(&self) -> PathBuf {
        self.0.join("session.json")
    }

    pub fn mcp_config(&self) -> PathBuf {
        self.0.join("mcp.json")
    }

    pub fn settings(&self) -> PathBuf {
        self.0.join("settings.json")
    }

    /// How this session starts the Agent's installed-skills server
    /// (P48); absent when it does not get one.
    pub fn agent_skills(&self) -> PathBuf {
        self.0.join("agent-skills.json")
    }

    pub fn stdout(&self) -> PathBuf {
        self.0.join("stdout")
    }

    pub fn stderr(&self) -> PathBuf {
        self.0.join("stderr")
    }

    pub fn supervisor_log(&self) -> PathBuf {
        self.0.join("supervisor.log")
    }

    pub fn supervisor_pid(&self) -> PathBuf {
        self.0.join("supervisor.pid")
    }

    pub fn agent_pid(&self) -> PathBuf {
        self.0.join("agent.pid")
    }

    pub fn stopping(&self) -> PathBuf {
        self.0.join("stopping")
    }

    pub fn exit(&self) -> PathBuf {
        self.0.join("exit")
    }

    /// What was measured about the context Claude actually ran in,
    /// written by the supervisor from inside it.
    pub fn context(&self) -> PathBuf {
        self.0.join("context")
    }

    /// The `CODEX_HOME` of an exec-server session (P23): Codex reads its
    /// transport from `environments.toml` in there, so it is per session
    /// rather than the shared profile. Made by the supervisor at launch.
    pub fn codex_home(&self) -> PathBuf {
        self.0.join("codex-home")
    }
}

/// Two facts about the place Claude is running, measured there rather
/// than inferred from who started it.
///
/// They are separate because on 2026-09-04 they turned out to disagree,
/// and the disagreement is the interesting part:
///
/// ```text
/// tmux server started by      managername   login Keychain
/// the controller (a gui/ LaunchAgent)   Background    answers
/// an ssh session                        Background    "User interaction is not allowed"
/// ```
///
/// So the controller-starts-tmux rule is right — a server forked from an
/// ssh session really cannot reach the Keychain — but `managername` cannot
/// show it: tmux daemonizes out of the `gui/` launchd domain and reports
/// `Background` either way. What survives that is the audit session, which
/// is what the Keychain actually gates on. A session judged by
/// `managername` alone would look broken when it works, which is the same
/// class of lie the controller exists to stop telling (design doc section
/// 21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// `launchctl managername`: `Aqua` for the GUI login session,
    /// `Background` for an ssh session *and* for anything that
    /// daemonized.
    #[serde(default)]
    pub manager: Option<String>,
    /// Whether the login Keychain answers here. `None` when the question
    /// could not be put (no login keychain on this machine, `security`
    /// missing).
    ///
    /// This reads no secret. It asks the *keychain* about its own lock
    /// settings — `security show-keychain-info`, whose whole output is a
    /// line like `Keychain "…/login.keychain-db" no-timeout` — and keeps
    /// only whether that succeeded. ccnm still never reads a credential
    /// (design doc section 6).
    #[serde(default)]
    pub keychain: Option<bool>,
}

impl Context {
    /// One phrase for a status line.
    pub fn describe(&self) -> String {
        let manager = self.manager.as_deref().unwrap_or("session unknown");
        match self.keychain {
            Some(true) => format!("{manager}, keychain reachable"),
            Some(false) => format!("{manager}, keychain blocked"),
            None => manager.to_string(),
        }
    }
}

/// What the supervisor measured, or `None` while it has not written it
/// yet: evidence, and a missing measurement must never read as a measured
/// failure.
pub fn read_context(dir: &Dir) -> Option<Context> {
    let bytes = fs::read(dir.context()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Create the session directory with its inputs. Refuses to reuse an
/// existing directory: two sessions with one id would share an `output/`
/// on the Runtime Node and overwrite each other's `exit` here.
///
/// `ssh` is `None` for a colocated workspace. That session gets no MCP
/// config and no native-tool denial, because there is no remote runtime to
/// route tools to -- Claude works the project in front of it. Writing an
/// MCP config there would point a transport at this same machine and take
/// away the tools that are the only ones able to do the job.
/// `machine_skills` is this (Agent) machine's own `[machine_skills]`: a
/// remote session gets the server that offers them unless it is off.
pub fn create(
    state: &Path,
    spec: &Spec,
    ssh: Option<&Ssh>,
    machine_skills: &crate::config::MachineSkills,
) -> Result<Dir> {
    spec.validate_identity()?;
    if spec.agent_identity.is_some() && ssh.is_none() {
        return Err(Error::invalid_args(
            "bound Agent session requires its verified remote SSH transport",
        ));
    }
    if spec.provider() == AgentProvider::Codex {
        crate::provider::codex::validate_spec(spec)?;
    }
    let dir = Dir::at(paths::session_dir(state, &spec.id));
    if spec.provider() == AgentProvider::Codex
        && dir.path().starts_with(crate::provider::codex::home()?)
    {
        return Err(Error::invalid_args(
            "session state cannot live in Codex's private authentication directory",
        ));
    }
    if dir.path().exists() {
        return Err(Error::internal(format!(
            "session directory already exists: {}",
            dir.path().display()
        )));
    }
    fs::create_dir_all(dir.path())?;
    // The settings file names what the model may do; keep it the owner's.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    fs::write(dir.meta(), pretty(spec)?)?;
    // MCP JSON cannot express env_remove. Both official CLIs launch this
    // Agent-local boundary, which strips credentials immediately before SSH.
    let exe = std::env::current_exe()?;
    let transport = ssh
        .map(|_| transport::launcher_for(&dir, &exe, spec.agent_identity.as_ref()))
        .transpose()?;
    // Only a remote session: a colocated one has the natives' own skills,
    // with the tools to use them.
    let agent_skills = match transport {
        Some(_) => crate::mcp::agent_skills::launcher(&exe, &spec.id, machine_skills)?,
        None => None,
    };
    if let Some(cmd) = &agent_skills {
        fs::write(
            dir.agent_skills(),
            pretty(&crate::mcp::agent_skills::Recorded::of(cmd))?,
        )?;
    }
    spec.provider().write_session_files(
        &dir,
        transport.as_ref(),
        &spec.agent_tools,
        agent_skills.as_ref(),
    )?;
    Ok(dir)
}

pub fn load(dir: &Dir) -> Result<Spec> {
    let bytes = fs::read(dir.meta()).map_err(|e| {
        Error::internal(format!("cannot read {}", dir.meta().display())).with_source(e)
    })?;
    let spec: Spec = payload::decode_json(&bytes)?;
    spec.validate_identity()?;
    Ok(spec)
}

pub(crate) fn pretty<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| Error::internal("cannot serialize session file").with_source(e))
}

/// Compatibility encoder for a direct SSH MCP command. Product sessions use
/// the Agent-local transport launcher; a JSON document cannot carry env_remove.
pub fn mcp_config(spec: &Spec, ssh: &Ssh) -> Result<serde_json::Value> {
    if spec.provider() != AgentProvider::Claude {
        return Err(Error::invalid_args(
            "Codex injects MCP configuration through its CLI, not a Claude JSON file",
        ));
    }
    Ok(crate::provider::claude::mcp_config(
        &mcp_transport(spec, ssh)?,
        None,
    ))
}

fn mcp_transport(spec: &Spec, ssh: &Ssh) -> Result<process::Cmd> {
    let wire = payload::encode(
        &ServePayload::new(&spec.workspace, spec.root.clone(), &spec.id)
            // Only an interactive session has somebody who can answer a
            // permission prompt; `--print` runs with prompting off.
            .with_interactive(matches!(spec.mode, Mode::Interactive { .. }))
            .with_provider(spec.provider()),
    )?;
    ssh.mcp_transport_cmd(&wire)
}

/// Compatibility helper for the current provider's session policy.
pub fn settings(remote: bool, agent_tools: &crate::config::AgentTools) -> serde_json::Value {
    crate::provider::claude::settings(remote, agent_tools, false)
}

/// How a session ended. Written by the supervisor as the last thing it
/// does, so its presence means "finished" and its absence "still running".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// `None` when Claude died from a signal — the supervisor's timeout
    /// included — or never started.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    /// Set when Claude could not be started at all, so the waiter learns
    /// why instead of waiting for a process that never existed.
    #[serde(default)]
    pub error: Option<String>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && self.error.is_none()
    }

    pub fn describe(&self) -> String {
        let secs = self.duration_ms as f64 / 1000.0;
        match (&self.error, self.timed_out, self.exit_code) {
            (Some(e), _, _) => format!("could not start: {e}"),
            (None, true, _) => format!("killed after {secs:.1} s (timeout)"),
            (None, false, Some(0)) => format!("exited 0 in {secs:.1} s"),
            (None, false, Some(code)) => format!("exited {code} in {secs:.1} s"),
            (None, false, None) => format!("killed by a signal after {secs:.1} s"),
        }
    }
}

/// `None` while the session is still running.
pub fn read_outcome(dir: &Dir) -> Result<Option<Outcome>> {
    match fs::read(dir.exit()) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            Error::internal(format!("cannot parse {}", dir.exit().display())).with_source(e)
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(Error::internal(format!("cannot read {}", dir.exit().display())).with_source(e))
        }
    }
}

/// Record where this process is running, best effort.
///
/// Never fails the run: the answer is diagnostic. A session that works is
/// worth more than one refused because `launchctl` was slow.
fn write_context(dir: &Dir) {
    let measured = measure_context(&crate::process::SystemRunner);
    tracing::info!(context = %measured.describe(), "claude will run here");
    if let Ok(json) = pretty(&measured) {
        let _ = fs::write(dir.context(), json);
    }
}

/// `security show-keychain-info <login keychain>`: does the login Keychain
/// answer in this context? Nothing about its contents is asked for or
/// kept — see [`Context::keychain`].
fn keychain_cmd(home: &Path) -> process::Cmd {
    process::Cmd::new("/usr/bin/security")
        .arg("show-keychain-info")
        .arg(home.join("Library/Keychains/login.keychain-db"))
        .timeout(Duration::from_secs(10))
}

fn measure_context(runner: &dyn process::ProcessRunner) -> Context {
    let manager = runner
        .run(&crate::controller::managername_cmd())
        .and_then(|out| crate::controller::parse_managername(&out))
        .ok();
    let keychain = paths::home_dir().ok().and_then(|home| {
        let path = home.join("Library/Keychains/login.keychain-db");
        // No login keychain on this machine: the question does not apply,
        // which is not the same answer as "blocked".
        if !path.exists() {
            return None;
        }
        runner
            .run(&keychain_cmd(&home))
            .ok()
            .map(|out| out.success())
    });
    Context { manager, keychain }
}

/// Written through a temporary file and a rename, so a reader polling for
/// it never sees half a document.
fn write_outcome(dir: &Dir, outcome: &Outcome) -> Result<()> {
    let tmp = dir.path().join("exit.tmp");
    fs::write(&tmp, pretty(outcome)?)?;
    fs::rename(&tmp, dir.exit())?;
    Ok(())
}

pub fn record_terminal_failure(dir: &Dir, message: &str) -> Result<()> {
    write_outcome(
        dir,
        &Outcome {
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            error: Some(message.to_string()),
        },
    )
}

pub fn write_supervisor_pid(dir: &Dir, pid: u32) -> Result<()> {
    let tmp = dir.path().join("supervisor.pid.tmp");
    fs::write(&tmp, format!("{pid}\n"))?;
    fs::rename(tmp, dir.supervisor_pid())?;
    Ok(())
}

pub fn read_supervisor_pid(dir: &Dir) -> Option<u32> {
    fs::read_to_string(dir.supervisor_pid())
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub fn write_agent_pid(dir: &Dir, pid: u32) -> Result<()> {
    let tmp = dir.path().join("agent.pid.tmp");
    fs::write(&tmp, format!("{pid}\n"))?;
    fs::rename(tmp, dir.agent_pid())?;
    Ok(())
}

pub fn read_agent_pid(dir: &Dir) -> Option<u32> {
    fs::read_to_string(dir.agent_pid())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// What `ccnm internal supervise --payload` is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuperviseRequest {
    pub protocol: u32,
    #[serde(
        default,
        skip_serializing_if = "crate::provider::AgentProvider::is_claude"
    )]
    pub provider: crate::provider::AgentProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<crate::instance::AgentIdentity>,
    pub session_dir: PathBuf,
    /// The `claude` the controller found in launchd's environment — the
    /// one Claude would be started with anyway.
    #[serde(rename = "claude_bin")]
    pub agent_bin: PathBuf,
}

impl SuperviseRequest {
    pub fn new(session_dir: PathBuf, agent_bin: PathBuf) -> Self {
        SuperviseRequest {
            identity: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            session_dir,
            agent_bin,
        }
    }
}

impl Protocol for SuperviseRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.identity.is_some() {
            3
        } else {
            self.provider.control_protocol()
        }
    }
}

/// Run Claude for the session and record how it ended. The process that
/// calls this is Claude's parent for its whole life.
///
/// Always leaves an `exit` file, even when Claude could not be spawned:
/// the ssh side is polling for that file, and a missing one would keep it
/// waiting for the full timeout to learn nothing.
pub fn supervise(req: &SuperviseRequest) -> Result<Outcome> {
    let dir = Dir::at(&req.session_dir);
    let spec = load(&dir)?;
    let launch = (|| {
        spec.validate_identity()?;
        if spec.agent_identity != req.identity {
            return Err(Error::invalid_args(
                "supervisor Agent identity does not match session",
            ));
        }
        if req.provider != spec.provider() {
            return Err(Error::invalid_args(
                "supervisor provider does not match session",
            ));
        }
        // The profile directory and the model both come from the Agent's own
        // registry, re-read here rather than carried in the session record:
        // they are local facts, and the record crosses machines.
        let local = req
            .identity
            .as_ref()
            .map(|identity| {
                let config = crate::Config::load(&crate::paths::effective_config_path()?)?;
                let resolved = config.resolve_instance_local(&identity.reference())?;
                if resolved.identity() != identity {
                    return Err(Error::invalid_args(
                        "Agent registry identity changed before supervisor launch",
                    ));
                }
                resolved.profile().validate_private_directory()?;
                Ok((
                    resolved.profile().directory().to_path_buf(),
                    resolved.model().map(str::to_string),
                ))
            })
            .transpose()?;
        let (profile, model) = match &local {
            Some((dir, model)) => (Some(dir.as_path()), model.as_deref()),
            None => (None, None),
        };
        spec.provider()
            .launch_cmd_at(&req.agent_bin, &spec, &dir, profile, model)
    })();
    // Measured here rather than assumed, because here is the one place
    // that is inside whatever context Claude will run in: under tmux, that
    // is the tmux server's, which is not necessarily the controller's.
    write_context(&dir);
    let ran = launch.and_then(|cmd| {
        tracing::info!(session = %spec.id, cmd = %cmd.display(), "starting {}", spec.provider().cli_name());
        if spec.mode.is_interactive() {
            // stdin/stdout/stderr are the tmux pane. Nothing is captured and
            // nothing is killed on a clock: the person at the terminal decides
            // when this session is over.
            process::run_attached_observed(&cmd, |pid| write_agent_pid(&dir, pid))
        } else {
            let out = fs::File::create(dir.stdout())?;
            let err = fs::File::create(dir.stderr())?;
            process::run_captured_observed(&cmd, out, err, |pid| write_agent_pid(&dir, pid))
        }
    });
    let outcome = match ran {
        Ok(captured) => Outcome {
            exit_code: captured.exit_code,
            timed_out: captured.timed_out,
            duration_ms: captured.duration.as_millis() as u64,
            error: None,
        },
        Err(e) => Outcome {
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            error: Some(if spec.agent_identity.is_some() {
                format!(
                    "{}: bound Agent launch validation failed; private details withheld",
                    e.code()
                )
            } else {
                e.to_string()
            }),
        },
    };
    tracing::info!(session = %spec.id, provider = %spec.provider().cli_name(), outcome = %outcome.describe(), "Agent ended");
    write_outcome(&dir, &outcome)?;
    Ok(outcome)
}

/// Wait for the supervisor's `exit` file. `timeout` should be the
/// session's own timeout plus a margin; the supervisor kills Claude at the
/// session timeout, so an `exit` that still has not appeared by then means
/// the supervisor itself is gone.
pub fn wait_for_outcome(dir: &Dir, timeout: Duration) -> Result<Outcome> {
    const POLL: Duration = Duration::from_millis(250);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(outcome) = read_outcome(dir)? {
            return Ok(outcome);
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::internal(format!(
                "no exit record after {timeout:?} at {}; the supervisor did not finish -- see {}",
                dir.exit().display(),
                dir.supervisor_log().display()
            )));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};
    use crate::protocol::payload;
    use ccnm_testdir::TestDir;

    /// The two facts are reported separately because they disagree: a
    /// tmux session that says `Background` can still reach the Keychain,
    /// and collapsing that into one word is how a working session gets
    /// called broken.
    #[test]
    fn the_context_keeps_the_manager_and_the_keychain_apart() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "Background\n"));
        fake.push(Output::exited(
            0,
            "Keychain \"login.keychain-db\" no-timeout\n",
        ));
        let measured = measure_context(&fake);
        assert_eq!(measured.manager.as_deref(), Some("Background"));
        // The keychain answer depends on this machine having a login
        // keychain; the shape of the sentence does not.
        assert!(
            measured.describe().starts_with("Background"),
            "{}",
            measured.describe()
        );

        assert_eq!(
            Context {
                manager: Some("Background".into()),
                keychain: Some(true),
            }
            .describe(),
            "Background, keychain reachable"
        );
        assert_eq!(
            Context {
                manager: Some("Aqua".into()),
                keychain: Some(false),
            }
            .describe(),
            "Aqua, keychain blocked"
        );
        assert_eq!(
            Context {
                manager: None,
                keychain: None,
            }
            .describe(),
            "session unknown"
        );
    }

    /// The keychain question is asked of the *login* keychain by name.
    /// Asked without one, `security` answers about the default keychain,
    /// which is the System keychain and answers "yes" from everywhere —
    /// a probe that cannot fail is not a probe.
    #[test]
    fn the_keychain_probe_names_the_login_keychain_and_reads_no_secret() {
        let cmd = keychain_cmd(Path::new("/Users/me"));
        let line = cmd.display();
        assert_eq!(
            line,
            "/usr/bin/security show-keychain-info /Users/me/Library/Keychains/login.keychain-db"
        );
        // Nothing that could return a secret: no find-generic-password, no
        // -w, no unlock.
        assert!(!line.contains("find-"), "{line}");
        assert!(!line.contains("unlock"), "{line}");
        assert!(!line.contains(" -w"), "{line}");
    }

    fn spec() -> Spec {
        Spec {
            runtime_node: None,
            agent_identity: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            id: "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into(),
            workspace: "fixture".into(),
            root: PathBuf::from("/Users/bing/ccnm-fixture"),
            runtime: Some(RuntimeLink {
                alias: "xdwmbp".into(),
                ccnm_bin: "~/.local/bin/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: PermissionMode::AcceptEdits,
            mode: Mode::Print {
                prompt: "fix the failing test".into(),
            },
            timeout_secs: 600,
            cwd: PathBuf::from("/Users/fodelf/.local/state/ccnm/workspaces/fixture"),
            codex_exec_server: false,
            agent_tools: Default::default(),
        }
    }

    fn ssh() -> Ssh {
        Ssh::new("xdwmbp", "/tmp/ccnm-t/session")
            .unwrap()
            .with_ccnm_bin("~/.local/bin/ccnm")
    }

    fn temp(test: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-session-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    #[test]
    fn ids_are_uuids_that_survive_safe_name() {
        let id = new_id();
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(paths::safe_name(&id, "x"), id);
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    /// The transport in mcp.json must be byte-for-byte the one the probe
    /// used. If doctor's handshake passes, the session reaches the home
    /// machine the same way.
    #[test]
    fn mcp_config_is_the_probes_transport_with_an_absolute_ssh() {
        let ssh = ssh();
        let cfg = mcp_config(&spec(), &ssh).unwrap();
        let server = &cfg["mcpServers"]["ccnm"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/usr/bin/ssh");
        let args: Vec<&str> = server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        // The same argv the probe's transport builds, minus its program.
        let wire = payload::encode(&ServePayload::new(
            "fixture",
            PathBuf::from("/Users/bing/ccnm-fixture"),
            "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d",
        ))
        .unwrap();
        let probe = ssh.mcp_transport_cmd(&wire).unwrap();
        assert_eq!(
            probe.program, "ssh",
            "SSH_BIN stands in for a bare `ssh` only"
        );
        let probe_args: Vec<String> = probe
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, probe_args);
        assert!(args.contains(&"ControlMaster=no"));
        assert!(args.contains(&"SendEnv=-ANTHROPIC_*"));
        assert_eq!(args[args.len() - 3], "mcp-serve");
        // The payload names this session, so exec_command output lands
        // under the same id on the Runtime Node.
        let sent: ServePayload = payload::decode(args.last().unwrap()).unwrap();
        assert_eq!(sent.session, "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d");
        assert_eq!(sent.root, PathBuf::from("/Users/bing/ccnm-fixture"));
        // This spec is a --print run, and the far side uses that to decide
        // whether exec_command may demand a permission prompt. Claude is
        // started with prompting off here, so a tool that insists on one
        // is a tool that can never run.
        assert!(!sent.interactive);
    }

    /// The other half of the same decision: an interactive session says
    /// so, which is what puts the permission prompt back on
    /// `exec_command`. Getting this wrong in either direction is silent
    /// -- either the prompt never appears, or every command is denied.
    #[test]
    fn an_interactive_session_tells_the_far_side_somebody_is_there() {
        let mut spec = spec();
        spec.mode = Mode::Interactive { prompt: None };
        let config = mcp_config(&spec, &ssh()).unwrap();
        let wire = config["mcpServers"]["ccnm"]["args"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let sent: ServePayload = payload::decode(&wire).unwrap();
        assert!(sent.interactive);
    }

    #[test]
    fn settings_allow_exactly_the_ccnm_tools_and_deny_the_native_ones() {
        // Every agent tool off: the shape every remote session had before
        // P46. Their names on the deny list are covered in the provider.
        let s = settings(true, &crate::config::AgentTools::none());
        let allow: Vec<&str> = s["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(allow.len(), MCP_TOOLS.len());
        for tool in MCP_TOOLS {
            assert!(
                allow.contains(&format!("mcp__ccnm__{tool}").as_str()),
                "{tool}"
            );
        }
        let deny: Vec<&str> = s["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(deny[..NATIVE_TOOLS_DENIED.len()], NATIVE_TOOLS_DENIED);
        // Nothing that would change how the user's Claude behaves elsewhere.
        assert_eq!(s.as_object().unwrap().len(), 1, "{s}");
    }

    #[test]
    fn create_writes_the_three_inputs_and_refuses_a_second_time() {
        let state = temp("create");
        let dir = create(&state, &spec(), Some(&ssh()), &Default::default()).unwrap();
        assert_eq!(
            dir.path(),
            state.join("sessions/0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d")
        );
        let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(dir.mcp_config().exists());
        assert!(dir.settings().exists());
        assert_eq!(load(&dir).unwrap(), spec());
        assert!(read_outcome(&dir).unwrap().is_none(), "nothing has run yet");

        let err = create(&state, &spec(), Some(&ssh()), &Default::default()).unwrap_err();
        assert!(err.message().contains("already exists"), "{err}");
    }

    /// A remote session gets the server that offers this machine's
    /// installed skills, allowed like ccnm's own tools, unless the machine's
    /// `[machine_skills]` says no (P48). A colocated one never does: it has
    /// the natives' own skills and the tools to use them.
    #[test]
    fn a_remote_session_gets_this_machines_installed_skills_unless_they_are_off() {
        use crate::mcp::agent_skills;
        let servers = |dir: &Dir| -> Vec<String> {
            let text = fs::read_to_string(dir.mcp_config()).unwrap();
            let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
            doc["mcpServers"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        };
        let allowed = |dir: &Dir| -> bool {
            fs::read_to_string(dir.settings())
                .unwrap()
                .contains(agent_skills::TOOL_PERMISSION)
        };

        let on = temp("skills-on");
        let dir = create(&on, &spec(), Some(&ssh()), &Default::default()).unwrap();
        assert_eq!(servers(&dir), ["ccnm", "ccnm_agent"]);
        assert!(allowed(&dir));
        let recorded = agent_skills::Recorded::read(&dir.agent_skills())
            .unwrap()
            .unwrap();
        assert_eq!(
            recorded.args[..3],
            ["internal", "agent-skills", "--payload"]
        );

        let off = temp("skills-off");
        let config = crate::config::MachineSkills {
            enabled: false,
            ..Default::default()
        };
        let dir = create(&off, &spec(), Some(&ssh()), &config).unwrap();
        assert_eq!(servers(&dir), ["ccnm"]);
        assert!(!allowed(&dir));
        assert!(!dir.agent_skills().exists());

        let colocated = temp("skills-colocated");
        let mut spec = spec();
        spec.runtime = None;
        let dir = create(&colocated, &spec, None, &Default::default()).unwrap();
        assert!(!dir.agent_skills().exists());
        assert!(!dir.mcp_config().exists());
    }

    #[test]
    fn outcome_is_written_atomically_and_read_back() {
        let state = temp("outcome");
        let dir = Dir::at(state.join("s"));
        fs::create_dir_all(dir.path()).unwrap();
        let outcome = Outcome {
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 7900,
            error: None,
        };
        write_outcome(&dir, &outcome).unwrap();
        assert!(
            !dir.path().join("exit.tmp").exists(),
            "the temp file must be renamed away"
        );
        assert_eq!(read_outcome(&dir).unwrap(), Some(outcome));
        assert_eq!(
            wait_for_outcome(&dir, Duration::from_secs(1))
                .unwrap()
                .duration_ms,
            7900
        );
    }

    #[test]
    fn waiting_gives_up_and_names_the_supervisor_log() {
        let state = temp("wait");
        let dir = Dir::at(state.join("s"));
        fs::create_dir_all(dir.path()).unwrap();
        let err = wait_for_outcome(&dir, Duration::from_millis(300)).unwrap_err();
        assert!(err.message().contains("supervisor.log"), "{err}");
    }

    #[test]
    fn outcome_descriptions_name_the_way_it_ended() {
        let base = Outcome {
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 1500,
            error: None,
        };
        assert!(base.ok());
        assert_eq!(base.describe(), "exited 0 in 1.5 s");
        let failed = Outcome {
            exit_code: Some(2),
            ..base.clone()
        };
        assert!(!failed.ok());
        assert_eq!(failed.describe(), "exited 2 in 1.5 s");
        let killed = Outcome {
            exit_code: None,
            timed_out: true,
            ..base.clone()
        };
        assert!(killed.describe().contains("timeout"));
        let never = Outcome {
            exit_code: None,
            error: Some("cannot spawn claude".into()),
            ..base
        };
        assert!(!never.ok());
        assert!(never.describe().starts_with("could not start:"));
    }

    /// The whole supervisor path with a stand-in for `claude`: the script
    /// records its argv and stdin, prints a JSON result, and exits 0.
    #[test]
    fn supervise_runs_claude_and_records_the_outcome() {
        let state = temp("supervise");
        let dir = create(&state, &spec(), Some(&ssh()), &Default::default()).unwrap();
        let fake = state.join("claude");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$PWD/argv\"\ncat > \"$PWD/prompt\"\necho '{\"is_error\":false,\"result\":\"done\"}'\necho oops >&2\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        // cwd in the spec does not exist on this machine; point it here.
        let mut spec = spec();
        spec.cwd = state.to_path_buf();
        fs::write(dir.meta(), pretty(&spec).unwrap()).unwrap();

        let req = SuperviseRequest::new(dir.path().to_path_buf(), fake);
        let outcome = supervise(&req).unwrap();
        assert!(outcome.ok(), "{outcome:?}");
        assert_eq!(read_outcome(&dir).unwrap(), Some(outcome));
        assert_eq!(
            fs::read_to_string(dir.stdout()).unwrap(),
            "{\"is_error\":false,\"result\":\"done\"}\n"
        );
        assert_eq!(fs::read_to_string(dir.stderr()).unwrap(), "oops\n");
        // The prompt went in on stdin, not on the command line.
        assert_eq!(
            fs::read_to_string(state.join("prompt")).unwrap(),
            "fix the failing test"
        );
        let argv = fs::read_to_string(state.join("argv")).unwrap();
        assert!(!argv.contains("fix the failing test"), "{argv}");
        assert!(argv.contains("--print\n"), "{argv}");
    }

    #[test]
    fn supervise_still_leaves_an_exit_record_when_claude_cannot_start() {
        let state = temp("supervise-missing");
        let dir = create(&state, &spec(), Some(&ssh()), &Default::default()).unwrap();
        let req = SuperviseRequest::new(dir.path().to_path_buf(), state.join("no-such-claude"));
        assert!(
            supervise(&req).is_ok(),
            "a start failure is an outcome, not a crash"
        );
        let outcome = read_outcome(&dir).unwrap().expect("exit file");
        assert!(!outcome.ok());
        assert!(
            outcome.error.as_deref().unwrap().contains("cannot spawn"),
            "{outcome:?}"
        );
    }
}

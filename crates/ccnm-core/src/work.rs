//! What `ccnm` does on the Agent Node when another node calls it
//! over ssh: `probe` (read-only, for doctor) and `agent-run` (create a
//! session, have the controller start it, wait for the result).
//!
//! This code runs in an **ssh session**, which is not the login session.
//! Anything that needs the login session — asking Claude about its
//! credentials, starting it — is forwarded to [`crate::controller`]
//! rather than done here. Everything else (writing the session files,
//! waiting, reading the output) is done here: same account, same disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::controller;
use crate::error::{Error, ErrorCode, ErrorReport, Reported, Result};
use crate::mcp;
use crate::paths;
use crate::process::ProcessRunner;
use crate::protocol::PROTOCOL;
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::mcp::{ProbeReport as McpProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::protocol::run::{
    AttachRequest, HistoryEntry, HistoryReport, HistoryRequest, PurgeReport, PurgeRequest,
    ResultReport, ResultRequest, RunReport, RunRequest, SessionRecord, SessionState, StartReport,
    StartRequest, StatusReport, StatusRequest, StopReport, StopRequest,
};
use crate::protocol::{self};
use crate::provider::{AgentProvider, AgentReport, AgentResult, Ask};
use crate::session::{self, Mode, RuntimeLink, Spec};
use crate::ssh::{Master, Ssh};
use crate::tmux;

#[derive(Clone)]
struct SelectedAgent {
    provider: AgentProvider,
    identity: Option<crate::instance::AgentIdentity>,
    profile_dir: Option<PathBuf>,
}

impl SelectedAgent {
    fn session_protocol(&self) -> u32 {
        if self.identity.is_some() {
            crate::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            self.provider.control_protocol()
        }
    }

    fn report_protocol(&self) -> u32 {
        if self.identity.is_some() {
            crate::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            PROTOCOL
        }
    }

    fn binding(
        &self,
        workspace: &str,
        root: &Path,
        runtime_node: &str,
    ) -> Result<Option<crate::instance::WorkspaceBinding>> {
        self.identity
            .as_ref()
            .map(|identity| {
                let binding = crate::instance::WorkspaceBinding {
                    workspace: workspace.to_string(),
                    runtime_node: runtime_node.to_string(),
                    root: root.to_path_buf(),
                    agent: identity.clone(),
                };
                binding.validate()?;
                Ok(binding)
            })
            .transpose()
    }
}

fn select_agent(
    reference: Option<&crate::instance::InstanceRef>,
    provider: AgentProvider,
    config_dir: Option<&Path>,
    permission: crate::config::PermissionMode,
    tools: &Tools<'_>,
) -> Result<SelectedAgent> {
    let Some(reference) = reference else {
        validate_provider_request(provider, config_dir, permission)?;
        return Ok(SelectedAgent {
            provider,
            identity: None,
            profile_dir: config_dir.map(Path::to_path_buf),
        });
    };
    if provider != AgentProvider::Claude
        || config_dir.is_some()
        || permission != crate::config::PermissionMode::default()
    {
        return Err(Error::invalid_args(
            "instance requests select provider/profile on the Agent and cannot carry legacy provider, path or permission overrides",
        ));
    }
    let resolved = tools
        .local
        .as_ref()
        .ok_or_else(|| Error::config("Agent-local profile registry is unavailable"))?
        .resolve(&tools.config, reference)?;
    resolved.profile().validate_private_directory()?;
    Ok(SelectedAgent {
        provider: resolved.identity().provider,
        identity: Some(resolved.identity().clone()),
        profile_dir: Some(resolved.profile().directory().to_path_buf()),
    })
}

fn requested_identity(
    reference: Option<&crate::instance::InstanceRef>,
    tools: &Tools<'_>,
) -> Result<Option<crate::instance::AgentIdentity>> {
    reference
        .map(|reference| tools.config.resolve_identity(reference))
        .transpose()
}

fn check_session_selection(
    spec: &Spec,
    workspace: &str,
    reference: Option<&crate::instance::InstanceRef>,
    tools: &Tools<'_>,
) -> Result<()> {
    if spec.workspace != workspace {
        return Err(Error::invalid_args(format!(
            "session {} belongs to workspace {}, not {workspace}",
            spec.id, spec.workspace
        )));
    }
    if let Some(expected) = requested_identity(reference, tools)?
        && spec.agent_identity.as_ref() != Some(&expected)
    {
        return Err(Error::new(
            ErrorCode::NotReady,
            "running/session record Agent identity differs from the selected instance",
        ));
    }
    Ok(())
}

fn profile_for_spec(spec: &Spec, tools: &Tools<'_>) -> Result<Option<PathBuf>> {
    let Some(identity) = spec.agent_identity.as_ref() else {
        return Ok(spec.provider_config_dir.clone());
    };
    let resolved = tools
        .local
        .as_ref()
        .ok_or_else(|| Error::config("Agent-local profile registry is unavailable"))?
        .resolve(&tools.config, &identity.reference())?;
    if resolved.identity() != identity {
        return Err(Error::new(
            ErrorCode::NotReady,
            "Agent registry identity changed since this session was bound",
        ));
    }
    Ok(Some(resolved.profile().directory().to_path_buf()))
}

/// What the Agent-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// This machine's own config, for turning the Runtime Node's *name*
    /// into the alias this machine dials it by.
    ///
    /// The caller no longer sends an alias: one only means something to
    /// the machine whose `~/.ssh/config` defines it. Looking it up here
    /// also settles the colocated case, where the name is this node and
    /// there is nothing to dial at all.
    pub config: Config,
    pub local: Option<crate::instance::AgentLocal>,
    /// This machine's state root; sessions and workspace dirs go under it.
    pub state: PathBuf,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`AgentProvider::locate`] found one. Only used as
    /// a fallback for the version when no controller is running; the
    /// controller finds its own, in launchd's environment.
    pub agents: crate::provider::AgentBinaries,
    /// The controller's socket on this machine.
    pub controller: PathBuf,
    /// tmux, for interactive sessions. Found in *this* (ssh) environment,
    /// which is enough to talk to a server the controller started: the
    /// socket belongs to the user, not to a session.
    pub tmux: Option<PathBuf>,
}

impl Tools<'_> {
    /// tmux or the one error that says how to get it.
    fn tmux(&self) -> Result<tmux::Tmux> {
        self.tmux
            .clone()
            .map(tmux::Tmux::new)
            .ok_or_else(tmux::missing)
    }

    /// How this machine reaches the node holding the project.
    ///
    /// `None` means the name is this machine: agent and project colocated,
    /// nothing to dial, Claude working the project with its native tools.
    fn runtime_link(&self, node: &str) -> Result<Option<RuntimeLink>> {
        if self.config.this.as_deref() == Some(node) {
            return Ok(None);
        }
        let Some(entry) = self.config.nodes.get(node) else {
            return Err(Error::config(format!(
                "the session names Runtime Node '{node}', which is not in this machine's config.toml\nadd it here, or correct runtime_node on the workspace where it is defined:\n  ccnm init --runtime <ssh alias>"
            )));
        };
        let Some(alias) = entry.ssh.as_deref() else {
            return Err(Error::config(format!(
                "nodes.{node} in this machine's config.toml has no `ssh` alias, so there is no way to reach the project from here"
            )));
        };
        Ok(Some(RuntimeLink {
            alias: alias.to_string(),
            ccnm_bin: entry.ccnm_bin(),
        }))
    }

    /// The transport to the Runtime Node, and the version/root handshake
    /// that has to succeed before a session is created against it.
    ///
    /// Colocated workspaces have neither: the project is right here, so
    /// there is no far side to agree with.
    fn dial_runtime(
        &self,
        node: &str,
        workspace: &str,
        root: &Path,
        provider: AgentProvider,
    ) -> Result<Option<Ssh>> {
        let Some(link) = self.runtime_link(node)? else {
            return Ok(None);
        };
        let ssh = Ssh::new(&link.alias, &self.control_dir)?
            .with_ccnm_bin(&link.ccnm_bin)
            .for_provider(provider);
        greet(&ssh, workspace, root, self)?;
        Ok(Some(ssh))
    }
}

/// Grace beyond the session's own timeout before giving up on the `exit`
/// file. The supervisor kills Claude at the session timeout and writes
/// the file right after; if that has not happened this much later, the
/// supervisor itself is gone.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// Start a print-mode session and wait for its result.
///
/// Refuses without a controller in a login session — the same rule doctor
/// applies, for the same reason: a Claude started from anywhere else
/// cannot read its own credentials, and the failure it would produce
/// ("not logged in") is a lie about the machine.
pub fn run(req: &RunRequest, tools: &Tools<'_>) -> Result<RunReport> {
    let selected = select_agent(
        req.agent.as_ref(),
        req.provider,
        req.provider_config_dir.as_deref(),
        req.permission_mode,
        tools,
    )?;
    // The chain serves interactive Codex only: `codex exec` canonicalizes
    // its working directory on this machine and exits when the project is
    // not here (P21.1). Said before anything is dialled or created, and
    // not answered by quietly starting an MCP session instead -- the
    // workspace asked for one thing and cannot have it in this mode.
    if req.codex_exec_server && selected.provider == AgentProvider::Codex {
        return Err(Error::invalid_args(format!(
            "workspace {} runs Codex through exec-server, which serves interactive sessions only: codex exec needs the project on this machine\nstart it interactively (ccnm run {}), or turn codex_exec_server off on the workspace to use the MCP tools",
            req.workspace, req.workspace
        )));
    }
    require_remote_topology(&req.runtime_node, &selected, tools)?;
    let ctx = controller::context(&tools.controller)?;
    if !ctx.login_session() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "the controller answers from {}, not from a login session, so a {} it started could not read its credentials\nrun on the Agent Node: ccnm controller install",
                ctx.describe(),
                if selected.provider == AgentProvider::Claude {
                    "Claude"
                } else {
                    "Codex"
                }
            ),
        ));
    }
    if selected.identity.is_some() || selected.provider == AgentProvider::Codex {
        agent_readiness(&selected, tools)?;
    }
    let ssh = tools.dial_runtime(
        &req.runtime_node,
        &req.workspace,
        &req.root,
        selected.provider,
    )?;
    if selected.identity.is_some() || selected.provider == AgentProvider::Codex {
        let ssh = ssh.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::NotReady,
                "selected Agent topology requires the measured remote SSH MCP path",
            )
        })?;
        provider_runtime_preflight(&selected, &req.workspace, &req.root, &req.runtime_node, ssh)?;
    }

    let runtime = tools.runtime_link(&req.runtime_node)?;
    let cwd = if runtime.is_none() {
        if !req.root.is_dir() {
            return Err(Error::new(
                ErrorCode::WrongWorkspace,
                "colocated workspace root is not a directory on the Agent Node",
            ));
        }
        req.root.clone()
    } else {
        paths::workspace_dir(&tools.state, &req.workspace)
    };
    std::fs::create_dir_all(&cwd)?;
    let spec = Spec {
        runtime_node: selected.identity.as_ref().map(|_| req.runtime_node.clone()),
        agent_identity: selected.identity.clone(),
        provider: selected.provider,
        protocol: selected.session_protocol(),
        id: session::new_id(),
        workspace: req.workspace.clone(),
        root: req.root.clone(),
        runtime,
        provider_config_dir: selected
            .identity
            .is_none()
            .then(|| req.provider_config_dir.clone())
            .flatten(),
        permission_mode: req.permission_mode,
        mode: Mode::Print {
            prompt: req.prompt.clone(),
        },
        timeout_secs: req.timeout_secs,
        cwd,
        codex_exec_server: false,
        agent_tools: req.agent_tools.clone(),
    };
    let dir = session::create(
        &tools.state,
        &spec,
        ssh.as_ref(),
        &tools.config.machine_skills,
        &tools.config.agent_mcp,
    )?;
    let pid = match controller::start_for_identity(
        &tools.controller,
        dir.path(),
        spec.provider(),
        spec.agent_identity.as_ref(),
    ) {
        Ok(pid) => pid,
        Err(error) => {
            let safe = selected
                .provider
                .redact_output_at(error.to_string(), selected.profile_dir.as_deref());
            session::record_terminal_failure(&dir, &safe)?;
            return Err(error);
        }
    };
    session::write_supervisor_pid(&dir, pid)?;
    let outcome = redact_outcome(
        spec.provider(),
        selected.profile_dir.as_deref(),
        session::wait_for_outcome(&dir, Duration::from_secs(req.timeout_secs) + EXIT_GRACE)?,
    );

    let stdout = std::fs::read(dir.stdout()).unwrap_or_default();
    let result = spec
        .provider()
        .parse_result_at(&stdout, selected.profile_dir.as_deref())
        .ok();
    let stdout_tail = if result.is_some() {
        String::new()
    } else {
        spec.provider()
            .redact_output_at(tail(&stdout), selected.profile_dir.as_deref())
    };
    let stderr_tail = spec.provider().redact_output_at(
        tail(&std::fs::read(dir.stderr()).unwrap_or_default()),
        selected.profile_dir.as_deref(),
    );
    Ok(RunReport {
        agent_identity: spec.agent_identity.clone(),

        provider: spec.provider(),
        protocol: selected.report_protocol(),
        session: spec.id,
        session_dir: dir.path().to_path_buf(),
        controller: ctx,
        pid,
        outcome,
        result,
        stdout_tail,
        stderr_tail,
    })
}

/// Start an interactive session, or report the one that is already there.
///
/// Unlike [`run`] this returns as soon as the session exists: the session
/// outlives the ssh call that made it, which is the whole point of putting
/// it in tmux (design doc section 23). What comes back is what the home
/// machine needs to attach.
pub fn start(req: &StartRequest, tools: &Tools<'_>) -> Result<StartReport> {
    let selected = select_agent(
        req.agent.as_ref(),
        req.provider,
        req.provider_config_dir.as_deref(),
        req.permission_mode,
        tools,
    )?;
    require_remote_topology(&req.runtime_node, &selected, tools)?;
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    tmux::check_name(&name)?;

    // Already up: attach to that, do not start a second Claude on the same
    // project. This path needs no controller, which is deliberate -- being
    // put back into a running session must not depend on the controller
    // being healthy right now.
    if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
        let session = live_session_id(&tmux, tools, &name);
        let dir = session
            .as_ref()
            .map(|id| paths::session_dir(&tools.state, id));
        let existing = dir
            .as_ref()
            .and_then(|dir| session::load(&session::Dir::at(dir)).ok());
        if existing.as_ref().is_some_and(|spec| {
            spec.provider() != selected.provider || spec.agent_identity != selected.identity
        }) || (selected.identity.is_some() && existing.is_none())
        {
            return Err(Error::new(
                ErrorCode::NotReady,
                "existing session provider differs or is unknown; stop it explicitly before selecting another Agent",
            ));
        }
        // A session's root is fixed when it starts: it is in the payload
        // the MCP transport was spawned with, and nothing can repoint it.
        // So a live session whose root is not the one being asked for is
        // working somewhere the config no longer names -- and if that path
        // has since been moved away, one where every tool fails for
        // reasons that sound like something else.
        //
        // P3 binds a running session immutably. A config edit must not end a
        // live conversation as a side effect of the next `run` command.
        let stale_root = dir
            .as_ref()
            .map(session::Dir::at)
            .and_then(|d| session::load(&d).ok())
            .map(|spec| spec.root)
            .filter(|root| root != &req.root);
        if let Some(old) = stale_root {
            return Err(Error::new(
                ErrorCode::NotReady,
                format!(
                    "running session {} is bound to {}; the workspace now names {}\nstop that exact session before starting against a different root",
                    session.as_deref().unwrap_or("<unknown>"),
                    old.display(),
                    req.root.display(),
                ),
            ));
        }
        let context = dir
            .as_ref()
            .and_then(|path| session::read_context(&session::Dir::at(path)));
        return Ok(StartReport {
            agent_identity: selected.identity.clone(),

            provider: selected.provider,
            protocol: selected.report_protocol(),
            session,
            session_dir: dir,
            tmux_session: name,
            server_pid: server_pid(&tmux, tools)?,
            already_running: true,
            replaced: None,
            controller: None,
            context,
        });
    }

    let (ctx, ssh) = preflight(req, &selected, tools)?;
    start_fresh(req, &selected, tools, &tmux, name, ctx, ssh)
}

fn require_remote_topology(
    runtime_node: &str,
    selected: &SelectedAgent,
    tools: &Tools<'_>,
) -> Result<()> {
    if tools.runtime_link(runtime_node)?.is_some() {
        return Ok(());
    }
    let detail = if selected.identity.is_some() {
        "instance colocated execution has not been measured and is not enabled"
    } else if selected.provider == AgentProvider::Codex {
        "Codex colocated execution has not been measured and is not enabled"
    } else {
        "Claude colocated execution is disabled until the corrected native-tool launch is verified with the installed CLI"
    };
    Err(Error::new(ErrorCode::NotReady, detail))
}

/// One round trip to the Runtime Node before a session is built, to
/// answer the two questions that are cheap now and expensive later.
///
/// **Do the two binaries agree?** They have to be the same build: the
/// control protocol is versioned but the tools are not, so two builds
/// that still decode each other's messages can disagree about what a tool
/// does. `doctor` has always checked this, but `doctor` is what somebody
/// runs when they already suspect something. A session started against a
/// mismatched pair fails later, somewhere that does not mention versions.
///
/// **Is the project still there?** A moved or renamed root used to be
/// found out from inside the session, where the failure arrives as a
/// tool blaming the program it could not run. It costs one `stat` here.
///
/// One SSH round trip, paid once per session, to buy the two errors that
/// are worst to debug from the far end.
///
/// Measured on the real pair: **430-490 ms**, five runs, which is a whole
/// SSH handshake and not a round trip on an open connection. An earlier
/// version of this comment claimed 30 ms; that would be the cost with a
/// master already up, and nothing on this path ever starts one.
///
/// `Master::Off`, not `Reuse`. Nothing on this path ever *creates* a
/// master, so a ControlPath could only be used if some other command left
/// one lying around -- while the 104-byte `sun_path` limit it must fit
/// inside applies every time. Requiring it would mean a state directory
/// too long for that limit could no longer start a session at all, which
/// it always could before: the MCP transport sets `ControlPath=none` and
/// never had one. Refusing to work in order to be able to reuse something
/// that is usually not there is the wrong way round.
fn greet(ssh: &Ssh, workspace: &str, root: &Path, tools: &Tools<'_>) -> Result<()> {
    let hello: HelloReport = ssh.call_ccnm(
        tools.runner,
        Master::Off,
        &["internal", "hello"],
        &HelloRequest::new(Some(root.to_path_buf())),
        Duration::from_secs(30),
        ErrorCode::RuntimeUnreachable,
    )?;
    if hello.ccnm_version != crate::VERSION {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "the Runtime Node runs ccnm {}, this one runs {}; install the same build on both before starting a session",
                hello.ccnm_version,
                crate::VERSION
            ),
        ));
    }
    match hello.root {
        Some(status) if status.is_ok() => Ok(()),
        Some(status) => Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace `{workspace}` says its root is {}, and on that machine it is {}\nif the project moved: ccnm ws add {workspace} <new path> --replace",
                root.display(),
                status.describe()
            ),
        )),
        // No answer at all, from something calling itself the same
        // version. That is the case the version numbers cannot catch:
        // `VERSION` is the Cargo version, so every build of 0.1.0 compares
        // equal to every other, and during development different builds
        // carrying the same number is the normal state rather than the
        // exception. A missing field is the one piece of hard evidence
        // available that the two are not the same binary.
        None => Err(Error::new(
            ErrorCode::Version,
            format!(
                "the Runtime Node reports ccnm {} like this one, but its reply is missing the project-root check, so the two are not the same build\ninstall this build there: scripts/deploy.sh <its alias>",
                hello.ccnm_version
            ),
        )),
    }
}

/// Everything that has to be true before a session can be built, checked
/// before anything is created *or destroyed*.
///
/// Order matters twice over. The controller is local and costs nothing,
/// so it goes first: no point spending a network round trip to find out
/// the LaunchAgent is not installed. And both of them come before the
/// `tmux kill` that replaces a stale session, because a handshake failing
/// afterwards would mean somebody's Claude was ended and not replaced,
/// for a reason -- a link that blinked, a version that does not match --
/// that has nothing to do with the session they just lost.
fn preflight(
    req: &StartRequest,
    selected: &SelectedAgent,
    tools: &Tools<'_>,
) -> Result<(controller::Context, Option<Ssh>)> {
    let ctx = controller::context(&tools.controller)?;
    if !ctx.login_session() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "the controller answers from {}, not from a login session, so a {} it started could not read its credentials\nrun on the Agent Node: ccnm controller install",
                ctx.describe(),
                if selected.provider == AgentProvider::Claude {
                    "Claude"
                } else {
                    "Codex"
                }
            ),
        ));
    }
    if selected.identity.is_some() || selected.provider == AgentProvider::Codex {
        agent_readiness(selected, tools)?;
    }
    let ssh = tools.dial_runtime(
        &req.runtime_node,
        &req.workspace,
        &req.root,
        selected.provider,
    )?;
    if selected.identity.is_some() || selected.provider == AgentProvider::Codex {
        let ssh = ssh.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::NotReady,
                "selected Agent topology requires the measured remote SSH MCP path",
            )
        })?;
        provider_runtime_preflight(selected, &req.workspace, &req.root, &req.runtime_node, ssh)?;
        if req.codex_exec_server && selected.provider == AgentProvider::Codex {
            native_runtime_preflight(
                selected,
                &req.workspace,
                &req.root,
                &req.runtime_node,
                ssh,
                tools.runner,
            )?;
        }
    }

    Ok((ctx, ssh))
}

/// Create the session and have the controller start it.
fn start_fresh(
    req: &StartRequest,
    selected: &SelectedAgent,
    tools: &Tools<'_>,
    _tmux: &tmux::Tmux,
    name: String,
    ctx: controller::Context,
    ssh: Option<Ssh>,
) -> Result<StartReport> {
    let runtime = tools.runtime_link(&req.runtime_node)?;
    let cwd = if runtime.is_none() {
        if !req.root.is_dir() {
            return Err(Error::new(
                ErrorCode::WrongWorkspace,
                "colocated workspace root is not a directory on the Agent Node",
            ));
        }
        req.root.clone()
    } else {
        paths::workspace_dir(&tools.state, &req.workspace)
    };
    std::fs::create_dir_all(&cwd)?;
    let spec = Spec {
        runtime_node: selected.identity.as_ref().map(|_| req.runtime_node.clone()),
        agent_identity: selected.identity.clone(),
        provider: selected.provider,
        protocol: selected.session_protocol(),
        id: session::new_id(),
        workspace: req.workspace.clone(),
        root: req.root.clone(),
        runtime,
        provider_config_dir: selected
            .identity
            .is_none()
            .then(|| req.provider_config_dir.clone())
            .flatten(),
        permission_mode: req.permission_mode,
        mode: Mode::Interactive {
            prompt: req.prompt.clone(),
        },
        // Not used interactively: nothing kills this session on a clock.
        timeout_secs: 0,
        cwd,
        // The workspace's choice, and only Codex can take it up: a Claude
        // session on the same workspace keeps its MCP tools.
        codex_exec_server: req.codex_exec_server && selected.provider == AgentProvider::Codex,
        agent_tools: req.agent_tools.clone(),
    };
    let dir = session::create(
        &tools.state,
        &spec,
        ssh.as_ref(),
        &tools.config.machine_skills,
        &tools.config.agent_mcp,
    )?;
    let server_pid = match controller::start_for_identity(
        &tools.controller,
        dir.path(),
        spec.provider(),
        spec.agent_identity.as_ref(),
    ) {
        Ok(pid) => pid,
        Err(error) => {
            let safe = selected
                .provider
                .redact_output_at(error.to_string(), selected.profile_dir.as_deref());
            session::record_terminal_failure(&dir, &safe)?;
            return Err(error);
        }
    };
    Ok(StartReport {
        agent_identity: spec.agent_identity.clone(),

        provider: selected.provider,
        protocol: selected.report_protocol(),
        session: Some(spec.id),
        session_dir: Some(dir.path().to_path_buf()),
        tmux_session: name,
        server_pid,
        already_running: false,
        replaced: None,
        controller: Some(ctx),
        // The supervisor writes this from inside tmux a moment from now;
        // `ccnm status` is where it shows up.
        context: wait_for_context(&dir),
    })
}

/// Give the supervisor a moment to record which security session it is in.
///
/// Bounded and best effort: this is evidence for a status line, and a
/// session that is up must not be reported as failed because a `launchctl`
/// call was slow.
fn wait_for_context(dir: &session::Dir) -> Option<session::Context> {
    const WAIT: Duration = Duration::from_secs(5);
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        if let Some(measured) = session::read_context(dir) {
            return Some(measured);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Hand this process's terminal to the workspace's session.
///
/// Runs under `ssh -t`, so "this process's terminal" is the one on the
/// Runtime Node. Returns tmux's own exit code: 0 both when the person
/// detaches and when Claude ends.
pub fn attach(req: &AttachRequest, tools: &Tools<'_>) -> Result<i32> {
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    if !tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
        return Err(tmux::no_session(&name));
    }
    let live_id = if req.agent.is_some() || req.session.is_some() {
        live_session_id(&tmux, tools, &name)
    } else {
        None
    };
    if let Some(expected) = req.session.as_ref()
        && live_id.as_deref() != Some(expected)
    {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "workspace {} is not running session {expected}",
                req.workspace
            ),
        ));
    }
    if let Some(id) = live_id {
        let spec = session::load(&session::Dir::at(paths::session_dir(&tools.state, &id)))?;
        check_session_selection(&spec, &req.workspace, req.agent.as_ref(), tools)?;
    } else if req.agent.is_some() || req.session.is_some() {
        return Err(Error::new(
            ErrorCode::NotReady,
            "live tmux session has no verifiable ccnm identity",
        ));
    }
    let captured = crate::process::run_attached(&tmux.attach_cmd(&name))?;
    Ok(captured.exit_code.unwrap_or(1))
}

/// Nothing is running, which is exactly the state `stop` exists to reach,
/// so say so instead of failing.
///
/// Before v1 this returned `CCNM_E_NOT_READY` and exit code 3. It was not
/// a lie -- there really was no session to stop -- but it made every
/// cleanup script that calls `stop` unconditionally look like it failed,
/// and a `--print` run that had already finished on its own could never be
/// stopped successfully.
///
/// Idempotent means "already in the requested state is success". It does
/// **not** mean stop can never fail: when a terminal *is* running and ccnm
/// cannot verify it is the selected one, that stays an error, because that
/// check exists to keep ccnm from killing someone else's session.
fn already_stopped(
    req: &StopRequest,
    tmux_session: String,
    tools: &Tools<'_>,
) -> Result<StopReport> {
    // The selection is resolved from this machine's own registry, the same
    // way a live session's would be: the Runtime side compares the returned
    // identity against what it asked for and fails the call if they differ.
    let identity = requested_identity(req.agent.as_ref(), tools)?;
    let mut session = None;
    if let Some(id) = req.session.as_deref() {
        // Reaching here with a named session means its record loaded and it
        // has no outcome -- the terminal died without recording one. Write
        // the terminal failure now, or the record stays outcome-less and
        // `status` keeps reporting it as unknown for good.
        let dir = session::Dir::at(paths::session_dir(&tools.state, id));
        if session::read_outcome(&dir)?.is_none() {
            session::record_terminal_failure(
                &dir,
                "no managed terminal was running when ccnm stopped this session",
            )?;
        }
        session = Some(id.to_string());
    }
    Ok(StopReport {
        protocol: if identity.is_some() {
            crate::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            PROTOCOL
        },
        tmux_session,
        session,
        agent_identity: identity,
        killed: false,
    })
}

/// End the workspace's session: tmux kills the supervisor, which kills
/// Claude, which drops the ssh transport its MCP server was on.
pub fn stop(req: &StopRequest, tools: &Tools<'_>) -> Result<StopReport> {
    if let Some(id) = req.session.as_deref() {
        if paths::safe_name(id, "") != id {
            return Err(Error::invalid_args(
                "session id is not a valid ccnm identifier",
            ));
        }
        let dir = session::Dir::at(paths::session_dir(&tools.state, id));
        let spec = session::load(&dir).map_err(|_| {
            Error::new(
                ErrorCode::NotReady,
                format!("no session {id} on this machine"),
            )
        })?;
        check_session_selection(&spec, &req.workspace, req.agent.as_ref(), tools)?;
        if !spec.mode.is_interactive() {
            return stop_print_session(&spec, &dir, tools);
        }
        if session::read_outcome(&dir)?.is_some() {
            return Ok(StopReport {
                protocol: if spec.agent_identity.is_some() {
                    crate::instance::INSTANCE_SESSION_PROTOCOL
                } else {
                    PROTOCOL
                },
                tmux_session: tmux::session_name(&spec.workspace),
                session: Some(spec.id.clone()),
                agent_identity: spec.agent_identity,
                killed: false,
            });
        }
    }
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    let live_id = if req.agent.is_some() || req.session.is_some() {
        if !tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
            return already_stopped(req, name, tools);
        }
        live_session_id(&tmux, tools, &name)
    } else {
        None
    };
    let mut identity = None;
    if let Some(expected) = req.session.as_ref()
        && live_id.as_deref() != Some(expected)
    {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "workspace {} is not running session {expected}",
                req.workspace
            ),
        ));
    }
    if let Some(id) = live_id.as_ref() {
        let spec = session::load(&session::Dir::at(paths::session_dir(&tools.state, id)))?;
        check_session_selection(&spec, &req.workspace, req.agent.as_ref(), tools)?;
        identity = spec.agent_identity;
    } else if req.agent.is_some() || req.session.is_some() {
        // Not the idempotent case: a terminal *is* running, ccnm just
        // cannot tell whether it is the one that was selected. Killing it
        // on that evidence is exactly what this check prevents.
        return Err(Error::new(
            ErrorCode::NotReady,
            "a terminal is running for this workspace but carries no verifiable ccnm session identity; refusing to stop it",
        ));
    }
    let tracked_dir = live_id
        .as_ref()
        .map(|id| session::Dir::at(paths::session_dir(&tools.state, id)));
    if (identity.is_some() || req.session.is_some())
        && let Some(dir) = tracked_dir.as_ref()
    {
        std::fs::write(dir.stopping(), b"requested\n")?;
    }
    let out = tools.runner.run(&tmux.kill_cmd(&name))?;
    let stderr = out.stderr_lossy();
    if !out.success() && !tmux::no_server(&stderr) && !stderr.contains("can't find session") {
        return Err(Error::internal(format!(
            "tmux kill-session failed (exit {:?}): {}",
            out.exit_code,
            stderr.trim()
        )));
    }
    if out.success() && (identity.is_some() || req.session.is_some()) {
        if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
            return Err(Error::internal(
                "tmux accepted stop but the selected session is still running",
            ));
        }
        if let Some(dir) = tracked_dir.as_ref() {
            match transport_alive(dir, tools) {
                Some(false) => {}
                Some(true) => {
                    return Err(Error::new(
                        ErrorCode::NotReady,
                        "terminal ended but its Runtime MCP transport is still alive; state remains stopping",
                    ));
                }
                None => {
                    return Err(Error::new(
                        ErrorCode::NotReady,
                        "terminal ended but Runtime MCP process state is unknown; stop is not confirmed",
                    ));
                }
            }
        }
        if let Some(dir) = tracked_dir.as_ref()
            && session::read_outcome(dir)?.is_none()
        {
            session::record_terminal_failure(
                dir,
                "stopped by ccnm after the managed terminal ended",
            )?;
        }
    }
    Ok(StopReport {
        agent_identity: identity.clone(),
        session: live_id,

        protocol: if identity.is_some() { 3 } else { PROTOCOL },
        tmux_session: name,
        killed: out.success(),
    })
}

fn stop_print_session(spec: &Spec, dir: &session::Dir, tools: &Tools<'_>) -> Result<StopReport> {
    if session::read_outcome(dir)?.is_some() {
        confirm_recorded_print_groups_ended(dir, tools)?;
        return Ok(StopReport {
            protocol: if spec.agent_identity.is_some() {
                3
            } else {
                PROTOCOL
            },
            tmux_session: tmux::session_name(&spec.workspace),
            session: Some(spec.id.clone()),
            agent_identity: spec.agent_identity.clone(),
            killed: false,
        });
    }
    let pid = session::read_supervisor_pid(dir).ok_or_else(|| {
        Error::new(
            ErrorCode::NotReady,
            "print session has no terminal outcome and no verified supervisor pid; state is unknown",
        )
    })?;
    let ps = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/ps").args([
            "-ww",
            "-p",
            &pid.to_string(),
            "-o",
            "pgid=",
            "-o",
            "command=",
        ]))?;
    let supervisor = known_process(ps, "recorded supervisor")?
        .ok_or_else(|| Error::new(ErrorCode::NotReady, "recorded supervisor is not running"))?;
    let fields: Vec<_> = supervisor.split_whitespace().collect();
    if fields.len() < 5
        || fields[0].parse::<u32>().ok() != Some(pid)
        || fields[fields.len() - 4..fields.len() - 1] != ["internal", "supervise", "--payload"]
    {
        return Err(Error::policy(
            "recorded pid is not the owned ccnm supervisor process group",
        ));
    }
    let wire = fields[fields.len() - 1].to_string();
    let request: session::SuperviseRequest = payload::decode(&wire).map_err(|_| {
        Error::policy(
            "recorded pid is not the ccnm supervisor for this session; refusing to signal it",
        )
    })?;
    if request.session_dir != dir.path()
        || request.provider != spec.provider()
        || request.identity != spec.agent_identity
    {
        return Err(Error::policy(
            "recorded pid belongs to another supervisor identity; refusing to signal it",
        ));
    }
    let agent_pid = session::read_agent_pid(dir).ok_or_else(|| {
        Error::new(
            ErrorCode::NotReady,
            "print session has no verified Agent child pid; state is unknown",
        )
    })?;
    let agent = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/ps").args([
            "-p",
            &agent_pid.to_string(),
            "-o",
            "pgid=,ppid=",
        ]))?;
    if let Some(agent) = known_process(agent, "recorded Agent child")? {
        let ids: Vec<_> = agent.split_whitespace().collect();
        if ids.len() != 2
            || ids[0].parse::<u32>().ok() != Some(agent_pid)
            || ids[1].parse::<u32>().ok() != Some(pid)
        {
            return Err(Error::policy(
                "recorded Agent child is not the supervisor's owned process-group leader; refusing to signal it",
            ));
        }
        std::fs::write(dir.stopping(), b"requested\n")?;
        let killed = tools.runner.run(
            // `--` before the negative pid: see process::kill_group. Without
            // it Linux `kill` reads it as a signal and signals nothing.
            &crate::process::Cmd::new("/bin/kill").args(["-TERM", "--", &format!("-{agent_pid}")]),
        )?;
        if process_group_alive(agent_pid, tools)? {
            return Err(Error::new(
                ErrorCode::NotReady,
                if killed.success() {
                    "Agent process group has not ended; state remains stopping"
                } else {
                    "could not stop the selected Agent process group; state remains stopping"
                },
            ));
        }
    } else if process_group_alive(agent_pid, tools)? {
        return Err(Error::new(
            ErrorCode::NotReady,
            "Agent leader ended but its process group still exists; refusing unverified group signalling",
        ));
    }
    std::fs::write(dir.stopping(), b"requested\n")?;
    let _ = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/kill").args(["-TERM", "--", &format!("-{pid}")]))?;
    if process_group_alive(pid, tools)? {
        return Err(Error::new(
            ErrorCode::NotReady,
            "supervisor process group has not ended; state remains stopping",
        ));
    }
    if session::read_outcome(dir)?.is_none() {
        session::record_terminal_failure(
            dir,
            "stopped by ccnm after the managed print process group ended",
        )?;
    }
    Ok(StopReport {
        protocol: if spec.agent_identity.is_some() {
            3
        } else {
            PROTOCOL
        },
        tmux_session: tmux::session_name(&spec.workspace),
        session: Some(spec.id.clone()),
        agent_identity: spec.agent_identity.clone(),
        killed: true,
    })
}

fn confirm_recorded_print_groups_ended(dir: &session::Dir, tools: &Tools<'_>) -> Result<()> {
    // An outcome describes the leader, not surviving children. Never signal
    // these historical PIDs: they may already belong to unrelated processes.
    for path in [dir.supervisor_pid(), dir.agent_pid()] {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            // Old records and failures before spawn may have no PID files.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(Error::new(
                    ErrorCode::NotReady,
                    "cannot read recorded print pid; stop is not confirmed",
                ));
            }
        };
        let pid = raw
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|pid| (2..=i32::MAX as u32).contains(pid))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::NotReady,
                    "invalid recorded print pid; stop is not confirmed",
                )
            })?;
        if process_group_alive(pid, tools)? {
            return Err(Error::new(
                ErrorCode::NotReady,
                "a recorded print process group still exists despite its outcome; stop is not confirmed",
            ));
        }
    }
    Ok(())
}

fn process_group_alive(pgid: u32, tools: &Tools<'_>) -> Result<bool> {
    let output = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/ps").args(["-axo", "pid=,pgid="]))?;
    if !output.success() || output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::new(
            ErrorCode::NotReady,
            "cannot verify process groups; state is unknown",
        ));
    }
    let mut found = false;
    for line in output
        .stdout_lossy()
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2 || fields.iter().any(|field| field.parse::<u32>().is_err()) {
            return Err(Error::new(
                ErrorCode::NotReady,
                "invalid process-group observation; state is unknown",
            ));
        }
        found |= fields[1].parse::<u32>().ok() == Some(pgid);
    }
    Ok(found)
}

fn known_process(output: crate::process::Output, subject: &str) -> Result<Option<String>> {
    let stdout = output.stdout_lossy();
    if output.success() && !stdout.trim().is_empty() {
        return Ok(Some(stdout));
    }
    if output.exit_code == Some(1) && stdout.trim().is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }
    Err(Error::new(
        ErrorCode::NotReady,
        format!("cannot verify {subject}; process state is unknown"),
    ))
}

/// Every live ccnm session on this machine, with what is known about each.
pub fn status(req: &StatusRequest, tools: &Tools<'_>) -> StatusReport {
    status_checked(req, tools).unwrap_or_else(|error| StatusReport {
        agent_identity: None,
        protocol: PROTOCOL,
        tmux: Err(error.into()),
        sessions: Vec::new(),
        records: Vec::new(),
    })
}

pub fn status_checked(req: &StatusRequest, tools: &Tools<'_>) -> Result<StatusReport> {
    if req.workspace.is_none() && (req.agent.is_some() || req.session.is_some()) {
        return Err(Error::invalid_args(
            "exact session or Agent status requires a workspace",
        ));
    }
    let expected = requested_identity(req.agent.as_ref(), tools)?;
    let records = req
        .session
        .as_ref()
        .map(|id| session_record(id, req.workspace.as_deref(), req.agent.as_ref(), tools))
        .transpose()?
        .into_iter()
        .collect();
    let (tmux_version, sessions) = match tools.tmux() {
        Err(e) => (Err(e.into()), Vec::new()),
        Ok(tmux) => {
            let version = tools
                .runner
                .run(&tmux.version_cmd())
                .and_then(|out| {
                    if out.success() {
                        Ok(out.stdout_lossy().trim().replace("tmux ", ""))
                    } else {
                        Err(Error::dependency(format!(
                            "tmux -V failed: {}",
                            out.stderr_lossy().trim()
                        )))
                    }
                })
                .map_err(Into::into);
            let sessions = live_sessions(&tmux, tools, req.workspace.as_deref())
                .into_iter()
                .filter(|session| {
                    expected
                        .as_ref()
                        .is_none_or(|identity| session.agent_identity.as_ref() == Some(identity))
                })
                .collect();
            (version, sessions)
        }
    };
    Ok(StatusReport {
        agent_identity: expected.clone(),
        protocol: if expected.is_some() { 3 } else { PROTOCOL },
        tmux: tmux_version,
        sessions,
        records,
    })
}

fn session_record(
    id: &str,
    workspace: Option<&str>,
    reference: Option<&crate::instance::InstanceRef>,
    tools: &Tools<'_>,
) -> Result<SessionRecord> {
    if paths::safe_name(id, "") != id {
        return Err(Error::invalid_args(
            "session id is not a valid ccnm identifier",
        ));
    }
    let dir = session::Dir::at(paths::session_dir(&tools.state, id));
    let spec = session::load(&dir).map_err(|_| {
        Error::new(
            ErrorCode::NotReady,
            format!("no session {id} on this machine"),
        )
    })?;
    if spec.id != id {
        return Err(Error::new(
            ErrorCode::NotReady,
            "session record id does not match its directory",
        ));
    }
    if let Some(workspace) = workspace {
        check_session_selection(&spec, workspace, reference, tools)?;
    }
    let outcome = session::read_outcome(&dir)?;
    let state = session_state(&spec, &dir, outcome.as_ref(), tools);
    let result = match profile_for_spec(&spec, tools) {
        Ok(profile) => std::fs::read(dir.stdout()).ok().and_then(|stdout| {
            spec.provider()
                .parse_result_at(&stdout, profile.as_deref())
                .ok()
        }),
        Err(_) => None,
    };
    Ok(SessionRecord {
        session: id.to_string(),
        workspace: spec.workspace.clone(),
        provider: spec.provider(),
        agent_identity: spec.agent_identity,
        state,
        provider_session_id: result
            .as_ref()
            .and_then(AgentResult::provider_session_id)
            .map(str::to_string),
        outcome,
    })
}

/// What a session record says about its session, asking tmux and `ps` only
/// when the record alone cannot: an outcome file means it finished.
fn session_state(
    spec: &Spec,
    dir: &session::Dir,
    outcome: Option<&session::Outcome>,
    tools: &Tools<'_>,
) -> SessionState {
    match outcome {
        Some(outcome) if outcome.ok() => SessionState::Completed,
        Some(_) => SessionState::Failed,
        None => {
            let interactive_live = tools.tmux().ok().is_some_and(|tmux| {
                let name = tmux::session_name(&spec.workspace);
                tools
                    .runner
                    .run(&tmux.has_session_cmd(&name))
                    .is_ok_and(|out| out.success())
                    && live_session_id(&tmux, tools, &name).as_deref() == Some(spec.id.as_str())
            });
            let supervisor_live = session::read_supervisor_pid(dir).is_some_and(|pid| {
                tools
                    .runner
                    .run(&crate::process::Cmd::new("/bin/ps").args([
                        "-p",
                        &pid.to_string(),
                        "-o",
                        "pid=",
                    ]))
                    .is_ok_and(|out| out.success() && !out.stdout_lossy().trim().is_empty())
            });
            if dir.stopping().exists() {
                SessionState::Stopping
            } else if interactive_live || supervisor_live {
                SessionState::Running
            } else if session::read_supervisor_pid(dir).is_none() {
                SessionState::Starting
            } else {
                SessionState::Unknown
            }
        }
    }
}

/// Every session this machine kept a record of, newest first.
///
/// Reads only ccnm's own session directories. The state of the few that
/// have no outcome yet is asked of tmux and `ps` after the list is cut to
/// `limit`, so a long history costs file reads, not processes.
pub fn history(req: &HistoryRequest, tools: &Tools<'_>) -> Result<HistoryReport> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(paths::sessions_dir(&tools.state)) {
        for entry in entries.flatten() {
            let dir = session::Dir::at(entry.path());
            let Ok(spec) = session::load(&dir) else {
                continue;
            };
            if req.workspace.as_ref().is_some_and(|w| *w != spec.workspace) {
                continue;
            }
            let started = modified_secs(&dir.meta()).unwrap_or(0);
            found.push((started, spec, dir));
        }
    }
    found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    found.truncate(req.limit as usize);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let sessions = found
        .into_iter()
        .map(|(started, spec, dir)| {
            let outcome = session::read_outcome(&dir).ok().flatten();
            let mut state = session_state(&spec, &dir, outcome.as_ref(), tools);
            // "Starting" means the supervisor has not written its pid yet.
            // Ten minutes on, it is not going to: the record outlived a
            // session that was killed before it could write an outcome.
            if state == SessionState::Starting && now.saturating_sub(started) > 600 {
                state = SessionState::Unknown;
            }
            let (mode, prompt) = match &spec.mode {
                Mode::Print { prompt } => ("print", Some(prompt.as_str())),
                Mode::Interactive { prompt } => ("interactive", prompt.as_deref()),
            };
            HistoryEntry {
                session: spec.id.clone(),
                workspace: spec.workspace.clone(),
                instance: spec.agent_identity.as_ref().map(|id| id.instance.clone()),
                mode: mode.to_string(),
                prompt: prompt.and_then(prompt_preview),
                started,
                ended: outcome.as_ref().and_then(|_| modified_secs(&dir.exit())),
                state,
                outcome,
            }
        })
        .collect();
    Ok(HistoryReport {
        protocol: PROTOCOL,
        sessions,
    })
}

/// The first non-empty line of a prompt, at most 60 characters.
fn prompt_preview(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut preview: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        preview.push('…');
    }
    Some(preview)
}

fn modified_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn live_sessions(
    tmux: &tmux::Tmux,
    tools: &Tools<'_>,
    only: Option<&str>,
) -> Vec<protocol::run::LiveSession> {
    let wanted = only.map(tmux::session_name);
    let Ok(out) = tools.runner.run(&tmux.list_cmd()) else {
        return Vec::new();
    };
    tmux::parse_list(&out.stdout_lossy())
        .into_iter()
        .filter(|live| wanted.as_ref().is_none_or(|name| &live.name == name))
        .map(|live| {
            let session = live_session_id(tmux, tools, &live.name);
            let dir = session
                .as_ref()
                .map(|id| session::Dir::at(paths::session_dir(&tools.state, id)));
            let context = dir.as_ref().and_then(session::read_context);
            let tools_up = dir.as_ref().and_then(|dir| transport_alive(dir, tools));
            protocol::run::LiveSession {
                agent_identity: dir
                    .as_ref()
                    .and_then(|dir| session::load(dir).ok())
                    .and_then(|spec| spec.agent_identity),
                provider: dir
                    .as_ref()
                    .and_then(|dir| session::load(dir).ok())
                    .map_or(AgentProvider::Claude, |spec| spec.provider()),
                workspace: live.workspace().map(str::to_string),
                tmux_session: live.name,
                session,
                created: live.created,
                attached: live.attached,
                context,
                tools: tools_up,
            }
        })
        .collect()
}

/// Delete ccnm's own bookkeeping for a workspace: the session records and
/// the directory Claude ran in.
///
/// **Never the project.** The root is the one thing here ccnm did not
/// create, and it is not even looked at. Everything removed is under this
/// machine's `~/.local/state/ccnm`.
pub fn purge(req: &PurgeRequest, tools: &Tools<'_>) -> PurgeReport {
    let mut removed = Vec::new();
    let mut sessions = Vec::new();

    let dir = paths::sessions_dir(&tools.state);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let session_dir = session::Dir::at(entry.path());
            let Ok(spec) = session::load(&session_dir) else {
                continue;
            };
            if spec.workspace != req.workspace {
                continue;
            }
            if std::fs::remove_dir_all(session_dir.path()).is_ok() {
                removed.push(session_dir.path().display().to_string());
                sessions.push(spec.id);
            }
        }
    }

    let workspace_dir = paths::workspace_dir(&tools.state, &req.workspace);
    if workspace_dir.is_dir() && std::fs::remove_dir_all(&workspace_dir).is_ok() {
        removed.push(workspace_dir.display().to_string());
    }

    PurgeReport {
        protocol: PROTOCOL,
        removed,
        sessions,
    }
}

/// What a session produced, for a caller that was not there when it
/// finished.
pub fn result(req: &ResultRequest, tools: &Tools<'_>) -> Result<ResultReport> {
    let expected_identity = requested_identity(req.agent.as_ref(), tools)?;
    let (id, dir, started) = match &req.session {
        Some(id) => {
            if paths::safe_name(id, "") != *id {
                return Err(Error::invalid_args(
                    "session id is not a valid ccnm identifier",
                ));
            }
            let dir = session::Dir::at(paths::session_dir(&tools.state, id));
            if !dir.meta().is_file() {
                return Err(Error::new(
                    ErrorCode::NotReady,
                    format!("no session {id} on this machine"),
                ));
            }
            let started = started_at(&dir);
            (id.clone(), dir, started)
        }
        None => newest_session(&tools.state, &req.workspace, expected_identity.as_ref())?,
    };
    let spec = session::load(&dir)?;
    if spec.id != id {
        return Err(Error::new(
            ErrorCode::NotReady,
            "session record id does not match its directory",
        ));
    }
    check_session_selection(&spec, &req.workspace, req.agent.as_ref(), tools)?;
    let profile = profile_for_spec(&spec, tools)?;
    let stdout = std::fs::read(dir.stdout()).unwrap_or_default();
    let result = spec
        .provider()
        .parse_result_at(&stdout, profile.as_deref())
        .ok();
    Ok(ResultReport {
        agent_identity: spec.agent_identity.clone(),

        provider: spec.provider(),
        protocol: if spec.agent_identity.is_some() {
            3
        } else {
            PROTOCOL
        },
        session: id,
        session_dir: dir.path().to_path_buf(),
        mode: if spec.mode.is_interactive() {
            "interactive".into()
        } else {
            "print".into()
        },
        started,
        outcome: session::read_outcome(&dir)?
            .map(|outcome| redact_outcome(spec.provider(), profile.as_deref(), outcome)),
        result,
        stdout_tail: spec
            .provider()
            .redact_output_at(tail(&stdout), profile.as_deref()),
        stderr_tail: spec.provider().redact_output_at(
            tail(&std::fs::read(dir.stderr()).unwrap_or_default()),
            profile.as_deref(),
        ),
    })
}

/// The workspace's most recent **print** session, by when its directory
/// was made.
///
/// Print only, because that is what this command is for. An interactive
/// session's output went to a terminal as it happened and there is nothing
/// stored to hand back; naming one explicitly still works, and says
/// "still running" or how it ended.
fn newest_session(
    state: &Path,
    workspace: &str,
    identity: Option<&crate::instance::AgentIdentity>,
) -> Result<(String, session::Dir, u64)> {
    let sessions = paths::sessions_dir(state);
    let mut best: Option<(String, session::Dir, u64)> = None;
    let entries = std::fs::read_dir(&sessions).map_err(|e| {
        Error::new(
            ErrorCode::NotReady,
            format!("no sessions on this machine yet ({})", sessions.display()),
        )
        .with_source(e)
    })?;
    for entry in entries.flatten() {
        let dir = session::Dir::at(entry.path());
        let Ok(spec) = session::load(&dir) else {
            continue;
        };
        if spec.workspace != workspace
            || spec.mode.is_interactive()
            || identity.is_some_and(|identity| spec.agent_identity.as_ref() != Some(identity))
        {
            continue;
        }
        let started = started_at(&dir);
        if best.as_ref().is_none_or(|(_, _, best)| started > *best) {
            best = Some((spec.id, dir, started));
        }
    }
    best.ok_or_else(|| {
        Error::new(
            ErrorCode::NotReady,
            format!(
                "no `--print` session for workspace {workspace} on this machine\nan interactive session prints to its own terminal; `ccnm attach {workspace}` goes back to it"
            ),
        )
    })
}

/// Unix seconds the session directory was created, or 0 if that cannot be
/// read. Only used for ordering and display.
fn started_at(dir: &session::Dir) -> u64 {
    std::fs::metadata(dir.meta())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Is this session's MCP transport still running?
///
/// The transport is one ssh, started by Claude from the session's
/// `mcp.json`, and it is every tool the model has. When it dies Claude
/// does not restart it: the terminal keeps working, the model keeps
/// answering, and it quietly has nothing to reach the project with — the
/// worst kind of failure, because it looks like a working session. So
/// `ccnm status` looks for the process by the exact payload that session's
/// `mcp.json` names, which is unique to it.
///
/// `None` means the question could not be answered (no mcp.json, `ps`
/// unavailable), never "no".
fn transport_alive(dir: &session::Dir, tools: &Tools<'_>) -> Option<bool> {
    let payload = transport_payload(dir)?;
    let out = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/ps").args(["-Awwo", "command="]))
        .ok()?;
    if !out.success() {
        return None;
    }
    Some(out.stdout_lossy().contains(&payload))
}

/// The `--payload` argument out of a session's `mcp.json`.
fn transport_payload(dir: &session::Dir) -> Option<String> {
    session::load(dir)
        .map(|spec| spec.provider())
        .unwrap_or(AgentProvider::Claude)
        .transport_payload(dir)
}

/// The ccnm session id a live tmux session was tagged with.
fn live_session_id(tmux: &tmux::Tmux, tools: &Tools<'_>, name: &str) -> Option<String> {
    let out = tools.runner.run(&tmux.session_id_cmd(name)).ok()?;
    out.success()
        .then(|| tmux::parse_session_id(&out.stdout_lossy()))
        .flatten()
}

fn server_pid(tmux: &tmux::Tmux, tools: &Tools<'_>) -> Result<u32> {
    let out = tools.runner.run(&tmux.server_pid_cmd())?;
    Ok(out.stdout_lossy().trim().parse().unwrap_or(0))
}

/// The last 2 KiB, on a character boundary. Enough to see why, never the
/// whole thing: the whole thing is in the session directory.
fn tail(bytes: &[u8]) -> String {
    const KEEP: usize = 2048;
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= KEEP {
        return text.into_owned();
    }
    let mut start = text.len() - KEEP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}

/// Everything doctor wants to know about this machine, in one round trip.
/// Read-only: no master connection, no file written. The MCP handshake
/// starts a server on the Runtime Node and shuts it down again before
/// returning (design doc section 4); so does the exec-server preflight.
/// Both take the workspace write guard and give it back on the far side,
/// which is as close to read-only as proving them gets: while a session is
/// writing, both report the guard busy instead.
pub fn probe(req: &ProbeRequest, tools: &Tools<'_>) -> ProbeReport {
    let selected = match select_agent(
        req.agent.as_ref(),
        req.provider,
        req.provider_config_dir.as_deref(),
        crate::config::PermissionMode::default(),
        tools,
    ) {
        Ok(selected) => selected,
        Err(error) => return rejected_probe(req, tools, error),
    };
    // A colocated workspace has no reverse link at all, and says so with
    // `None` rather than with an error: there is nothing broken about a
    // machine that is already holding the project.
    let (runtime_ssh, runtime_hello, runtime_audit, mcp, exec_server) =
        match tools.runtime_link(&req.runtime_node) {
            Ok(None) => (None, None, None, None, None),
            Err(e) => (Some(Err(e.into())), None, None, None, None),
            Ok(Some(link)) => {
                match Ssh::new(&link.alias, &tools.control_dir).map(|ssh| {
                    ssh.with_ccnm_bin(&link.ccnm_bin)
                        .for_provider(selected.provider)
                }) {
                    Err(e) => (
                        Some(Err(e.into())),
                        Some(Err(Error::new(
                            ErrorCode::RuntimeUnreachable,
                            format!(
                                "not attempted: the alias for {} is invalid",
                                req.runtime_node
                            ),
                        )
                        .into())),
                        None,
                        None,
                        None,
                    ),
                    Ok(ssh) => {
                        let runtime_ssh = ssh.resolve(tools.runner).map_err(Into::into);
                        let runtime_hello = ssh
                            .check_control_path()
                            .and_then(|()| {
                                ssh.call_ccnm::<_, HelloReport>(
                                    tools.runner,
                                    Master::Reuse,
                                    &["internal", "hello"],
                                    &HelloRequest::new(Some(req.root.clone())),
                                    Duration::from_secs(30),
                                    ErrorCode::RuntimeUnreachable,
                                )
                            })
                            .map_err(Into::into);
                        // The safety verdict has to come from the far end. This
                        // ssh lands on the Runtime Executor, so asking here is
                        // the only way doctor learns about the account that will
                        // actually run the tools instead of about its own.
                        let runtime_audit = runtime_hello.is_ok().then(|| {
                            ssh.call_ccnm::<_, crate::runtime::AuditReport>(
                                tools.runner,
                                Master::Reuse,
                                &["internal", "runtime-audit"],
                                &crate::runtime::AuditRequest::new(&req.workspace),
                                Duration::from_secs(30),
                                ErrorCode::RuntimeUnreachable,
                            )
                            .map_err(Into::into)
                        });
                        // Only worth the round trips if the plain reverse ssh worked.
                        let mcp = (req.mcp_calls > 0 && runtime_hello.is_ok())
                            .then(|| mcp_handshake(req, &selected, &ssh).map_err(Into::into));
                        // The chain's own preflight, the very call `ccnm run`
                        // makes before starting Codex, so a green row means
                        // the same check passed (P27). Gated on the hello
                        // alone, like the audit and the handshake: a failed
                        // handshake is its own row, and this one may fail
                        // for a reason that row cannot see -- no codex_bin,
                        // the wrong Codex.
                        let exec_server = (req.codex_exec_server
                            && selected.provider == AgentProvider::Codex
                            && runtime_hello.is_ok())
                        .then(|| {
                            native_runtime_preflight(
                                &selected,
                                &req.workspace,
                                &req.root,
                                &req.runtime_node,
                                &ssh,
                                tools.runner,
                            )
                            .map_err(Into::into)
                        });
                        (
                            Some(runtime_ssh),
                            Some(runtime_hello),
                            runtime_audit,
                            mcp,
                            exec_server,
                        )
                    }
                }
            }
        };

    let (controller, agent) = ask_about_agent(tools, &selected);
    ProbeReport {
        agent_identity: selected.identity.clone(),
        provider: selected.provider,
        protocol: selected.report_protocol(),
        // Colocated: the project is on this machine, so this hello is
        // the one that can say whether the root is really there.
        hello: hello::answer(&HelloRequest::new(
            runtime_ssh.is_none().then(|| req.root.clone()),
        )),
        controller: Some(controller),
        agent,
        runtime_ssh,
        runtime_hello,
        runtime_audit,
        mcp,
        exec_server,
        // Read-only, like everything else here: tmux is asked its version
        // and which sessions exist, and nothing is started or stopped.
        terminal: Some(status(
            &StatusRequest {
                agent: req.agent.clone(),
                session: None,

                protocol: selected.report_protocol(),
                workspace: Some(req.workspace.clone()),
            },
            tools,
        )),
    }
}

fn rejected_probe(req: &ProbeRequest, tools: &Tools<'_>, error: Error) -> ProbeReport {
    let report = ErrorReport::from(&error);
    ProbeReport {
        agent_identity: None,
        provider: req.provider,
        protocol: PROTOCOL,
        hello: hello::answer(&HelloRequest::new(None)),
        controller: Some(Err(report.clone())),
        agent: AgentReport {
            path: None,
            version: Err(report.clone()),
            auth: Err(report),
        },
        runtime_ssh: None,
        runtime_hello: None,
        runtime_audit: None,
        mcp: None,
        exec_server: None,
        terminal: Some(status(
            &StatusRequest {
                agent: None,
                session: None,
                protocol: PROTOCOL,
                workspace: Some(req.workspace.clone()),
            },
            tools,
        )),
    }
}

/// Claude's login state, from the only context whose answer means
/// anything.
///
/// With a controller, everything about Claude comes from it: not just the
/// login but the binary and version too, because the controller's `PATH`
/// is launchd's, and that is the `claude` a session would really start.
///
/// Without one, the version is still worth reporting — it needs no
/// credential — but the login is left as `CCNM_E_NOT_READY`. This session
/// *can* run `claude auth status`; the point is that its answer would be
/// wrong, and a wrong row sends the user to log in on a machine that is
/// already logged in.
fn ask_about_agent(
    tools: &Tools<'_>,
    selected: &SelectedAgent,
) -> (Reported<controller::Context>, AgentReport) {
    match controller::context(&tools.controller) {
        Ok(ctx) => {
            // A controller that is not in a login session is asked only
            // for the version. Its answer about the login would be no
            // better than this session's, and the rule holds everywhere:
            // do not run a command whose result has to be thrown away.
            let ask = if ctx.login_session() {
                Ask::Everything
            } else {
                Ask::VersionOnly
            };
            let asked = match selected.identity.as_ref() {
                Some(identity) => controller::agent_auth_instance(&tools.controller, identity, ask),
                None => controller::agent_auth_for(
                    &tools.controller,
                    selected.provider,
                    selected.profile_dir.as_deref(),
                    ask,
                ),
            };
            let agent = asked
                .map(|report| redact_agent_report(selected, report))
                .unwrap_or_else(|error| {
                    let error = redact_error(selected, error);
                    AgentReport {
                        path: None,
                        version: Err((&error).into()),
                        auth: Err(error.into()),
                    }
                });
            (Ok(ctx), agent)
        }
        Err(missing) => {
            let mut agent = redact_agent_report(
                selected,
                selected.provider.report_at(
                    tools.agents.get(selected.provider),
                    selected.profile_dir.as_deref(),
                    tools.runner,
                    Ask::VersionOnly,
                ),
            );
            agent.auth = Err(ErrorReport::new(
                ErrorCode::NotReady,
                format!(
                    "not checked: no controller to ask, and this ssh session's answer would be wrong\n{}",
                    missing.message()
                ),
            ));
            (Err(missing.into()), agent)
        }
    }
}

/// One MCP session from this Agent Node to the Runtime Executor, and what
/// it measured.
///
/// `ccnm mcp probe` on the Agent Node used to send the whole public command
/// to the Runtime, which then dialled back here to do exactly this. The
/// Agent already holds the credential for the direction that is allowed --
/// inbound to the executor -- so it opens the transport itself
/// (P7.4 Batch D2).
pub fn mcp_probe(req: &ProbeRequest, tools: &Tools<'_>) -> Result<McpProbeReport> {
    let selected = select_agent(
        req.agent.as_ref(),
        req.provider,
        req.provider_config_dir.as_deref(),
        crate::config::PermissionMode::default(),
        tools,
    )?;
    let link = tools.runtime_link(&req.runtime_node)?.ok_or_else(|| {
        Error::new(
            ErrorCode::NotReady,
            format!(
                "{} is this machine, so there is no MCP transport to open to it",
                req.runtime_node
            ),
        )
    })?;
    let ssh = Ssh::new(&link.alias, &tools.control_dir)?
        .with_ccnm_bin(&link.ccnm_bin)
        .for_provider(selected.provider);
    mcp_handshake(req, &selected, &ssh)
}

fn mcp_handshake(
    req: &ProbeRequest,
    selected: &SelectedAgent,
    ssh: &Ssh,
) -> Result<McpProbeReport> {
    // The same wire a real session opens with, so a green handshake means
    // the session's own path works. A bound one asks the Runtime to
    // resolve the workspace (protocol 4); a legacy one still carries its
    // root, as its sessions do.
    let session = format!("probe-{}", uuid::Uuid::new_v4().hyphenated());
    let wire = match selected.binding(&req.workspace, &req.root, &req.runtime_node)? {
        Some(binding) => payload::encode(&crate::runtime::OpenPayload::new(
            &req.workspace,
            binding.agent,
            &session,
        ))?,
        None => payload::encode(
            &ServePayload::new(&req.workspace, req.root.clone(), &session)
                .with_provider(selected.provider),
        )?,
    };
    let cmd = ssh.mcp_transport_cmd(&wire)?;
    mcp::probe::probe(
        &cmd,
        req.mcp_calls,
        Duration::from_secs(30) + Duration::from_millis(500) * req.mcp_calls,
        ErrorCode::RuntimeUnreachable,
    )
}

fn validate_provider_request(
    provider: AgentProvider,
    config_dir: Option<&Path>,
    permission: crate::config::PermissionMode,
) -> Result<()> {
    if provider == AgentProvider::Codex
        && (config_dir.is_some() || permission != crate::config::PermissionMode::default())
    {
        return Err(Error::invalid_args(
            "Codex requests cannot supply an Agent home or Claude permission mode",
        ));
    }
    Ok(())
}
fn provider_runtime_preflight(
    selected: &SelectedAgent,
    workspace: &str,
    root: &Path,
    runtime_node: &str,
    ssh: &Ssh,
) -> Result<()> {
    let wire = match selected.binding(workspace, root, runtime_node)? {
        Some(binding) => crate::protocol::payload::encode(&crate::runtime::OpenPayload::new(
            workspace,
            binding.agent,
            "provider-preflight",
        ))?,
        None => crate::protocol::payload::encode(
            &ServePayload::new(workspace, root.to_path_buf(), "provider-preflight")
                .with_provider(selected.provider),
        )?,
    };
    let cmd = ssh.mcp_transport_cmd(&wire)?;
    mcp::probe::probe(
        &cmd,
        1,
        Duration::from_secs(30),
        ErrorCode::RuntimeUnreachable,
    )?;
    Ok(())
}

/// The exec-server chain's own preflight (P23), after the MCP one has
/// shown the Runtime is reachable and the workspace opens: one empty
/// `exec-serve` session. What it catches before Codex is started: no
/// `codex_exec_server` on the far side, no `codex_bin`, the wrong Codex
/// version, an exec-server that does not start. Inside Codex all of those
/// would read as "environment unavailable".
fn native_runtime_preflight(
    selected: &SelectedAgent,
    workspace: &str,
    root: &Path,
    runtime_node: &str,
    ssh: &Ssh,
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let binding = selected
        .binding(workspace, root, runtime_node)?
        .ok_or_else(|| {
            Error::invalid_args("the exec-server chain needs an instance-bound Codex session")
        })?;
    let wire = payload::encode(&crate::runtime::NativeOpenPayload::new(
        workspace,
        binding.agent,
        "provider-preflight",
    ))?;
    ssh.exec_transport_preflight(runner, &wire)
}

fn agent_readiness(selected: &SelectedAgent, tools: &Tools<'_>) -> Result<()> {
    let report = match selected.identity.as_ref() {
        Some(identity) => {
            controller::agent_auth_instance(&tools.controller, identity, Ask::Everything)
                .map_err(|error| redact_error(selected, error))?
        }
        None => controller::agent_auth_for(
            &tools.controller,
            selected.provider,
            selected.profile_dir.as_deref(),
            Ask::Everything,
        )
        .map_err(|error| redact_error(selected, error))?,
    };
    let report = redact_agent_report(selected, report);
    report.version.map_err(Error::from)?;
    if !report.auth.map_err(Error::from)?.logged_in {
        return Err(Error::new(
            ErrorCode::Auth,
            selected.provider.auth_hint(None),
        ));
    }
    Ok(())
}

fn redact_error(selected: &SelectedAgent, error: Error) -> Error {
    Error::new(
        error.code(),
        selected
            .provider
            .redact_output_at(error.message().to_string(), selected.profile_dir.as_deref()),
    )
}

fn redact_agent_report(selected: &SelectedAgent, mut report: AgentReport) -> AgentReport {
    let redact = |mut error: ErrorReport| {
        error.message = selected
            .provider
            .redact_output_at(error.message, selected.profile_dir.as_deref());
        error
    };
    report.version = report.version.map_err(&redact);
    report.auth = report.auth.map_err(redact);
    report
}

fn redact_outcome(
    provider: AgentProvider,
    profile_dir: Option<&Path>,
    mut outcome: session::Outcome,
) -> session::Outcome {
    outcome.error = outcome
        .error
        .map(|text| provider.redact_output_at(text, profile_dir));
    outcome
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::process::{Cmd, FakeRunner, Output};
    use crate::protocol::hello::PathStatus;
    use ccnm_testdir::TestDir;

    /// What an Agent Node has on disk: which node it is, and the one alias
    /// it dials the projects by. Every value is non-default, so a code
    /// path that fell back to a default would fail a test rather than pass
    /// one.
    fn agent_config() -> Config {
        Config::parse(
            "this = \"agent\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"to-runtime\"\nccnm_bin = \"/opt/runtime/ccnm\"\n",
        )
        .unwrap()
    }

    /// The same machine, but the workspace's project is on it: `runtime`
    /// resolves to this node, so nothing is dialled.
    fn colocated_config() -> Config {
        Config::parse("this = \"agent\"\n[nodes.agent]\n").unwrap()
    }

    fn temp(test: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-work-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let sockets = control(&dir);
        TestDir::adopt(dir).also(sockets)
    }

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    fn hello_json(root_ok: bool) -> String {
        let rep = HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from("/Users/ccrun/.local/bin/ccnm")),
            root: Some(PathStatus {
                exists: root_ok,
                is_dir: root_ok,
            }),
        };
        serde_json::to_string(&rep).unwrap()
    }

    /// What the Runtime Executor answers about itself, over the same
    /// reverse ssh the hello used.
    fn audit_json() -> String {
        serde_json::to_string(&crate::runtime::AuditReport {
            protocol: crate::runtime::OPEN_PROTOCOL,
            audit: crate::safety::Audit {
                user: "ccrun".into(),
                findings: vec![],
            },
            root: crate::runtime::RootStatus {
                present: true,
                is_dir: true,
                owned: true,
                git: crate::runtime::GitStatus::Usable,
            },
            allow_unconfined_exec: false,
            allow_unisolated_credentials: false,
            allow_unattended_exec: false,
        })
        .unwrap()
    }

    fn request() -> ProbeRequest {
        ProbeRequest {
            agent: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/ccrun/Projects/xshun"),
            runtime_node: "runtime".into(),
            provider_config_dir: Some(PathBuf::from("/x/claude")),
            mcp_calls: 0,
            codex_exec_server: false,
        }
    }

    /// A socket path no controller is on, so the probe takes the
    /// no-controller branch.
    fn absent_socket(test: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/ccnm-absent-{}-{test}.sock",
            std::process::id()
        ))
    }

    /// P27: the probe runs the exec-server chain's preflight only where
    /// `ccnm run` would -- the workspace asks for it and the Agent is
    /// Codex -- and runs the same call, so it refuses what run refuses.
    #[test]
    fn probe_runs_the_exec_server_preflight_only_for_codex_on_the_chain() {
        fn scripted<'a>(dir: &Path, fake: &'a FakeRunner, test: &str) -> Tools<'a> {
            fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
            fake.push(Output::exited(0, hello_json(true)));
            fake.push(Output::exited(0, audit_json()));
            Tools {
                local: None,
                config: agent_config(),
                runner: fake,
                state: dir.to_path_buf(),
                control_dir: control(dir),
                agents: crate::provider::AgentBinaries::with_claude(None),
                tmux: None,
                controller: absent_socket(test),
            }
        }

        // Claude on a workspace that opted in keeps its MCP tools: nothing
        // is asked beyond the usual three calls.
        let dir = temp("probe-native-claude");
        let fake = FakeRunner::new();
        let tools = scripted(&dir, &fake, "probe-native-claude");
        let rep = probe(
            &ProbeRequest {
                codex_exec_server: true,
                ..request()
            },
            &tools,
        );
        assert_eq!(rep.exec_server, None);
        assert_eq!(fake.calls().len(), 3);

        // Codex on the same workspace gets the preflight. This one is not
        // instance-bound, which `ccnm run` refuses before dialling, and the
        // row carries that same refusal.
        let dir = temp("probe-native-codex");
        let fake = FakeRunner::new();
        let tools = scripted(&dir, &fake, "probe-native-codex");
        let rep = probe(
            &ProbeRequest {
                provider: AgentProvider::Codex,
                provider_config_dir: None,
                codex_exec_server: true,
                ..request()
            },
            &tools,
        );
        let refused = rep.exec_server.unwrap().unwrap_err();
        assert_eq!(refused.code(), ErrorCode::InvalidArgs);
        assert!(
            refused.message.contains("instance-bound"),
            "{}",
            refused.message
        );

        // Codex off the chain: no preflight.
        let dir = temp("probe-native-off");
        let fake = FakeRunner::new();
        let tools = scripted(&dir, &fake, "probe-native-off");
        let rep = probe(
            &ProbeRequest {
                provider: AgentProvider::Codex,
                provider_config_dir: None,
                ..request()
            },
            &tools,
        );
        assert_eq!(rep.exec_server, None);
    }

    #[test]
    fn probe_collects_every_fact_in_one_report() {
        let dir = temp("probe");
        let fake = FakeRunner::new();
        // Call order: ssh -G, ssh internal hello, ssh internal
        // runtime-audit, claude --version. No `claude auth status`: with no
        // controller its answer would be wrong, so it is not asked at all.
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        fake.push(Output::exited(0, audit_json()));
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));

        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                "/usr/local/bin/claude",
            ))),
            tmux: None,
            controller: absent_socket("probe"),
        };
        let rep = probe(&request(), &tools);

        assert_eq!(rep.hello.ccnm_version, crate::VERSION);
        assert_eq!(
            rep.runtime_ssh.as_ref().unwrap().as_ref().unwrap().target(),
            "ccrun@home.ts"
        );
        let home = rep.runtime_hello.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(home.user, "ccrun");
        assert!(home.root.unwrap().is_ok());
        assert_eq!(rep.agent.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.agent.auth.as_ref().unwrap_err().code(),
            ErrorCode::NotReady,
            "an unaskable login must not be reported as logged out"
        );
        assert_eq!(
            rep.controller
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .code(),
            ErrorCode::NotReady
        );
        assert_eq!(rep.mcp, None, "mcp_calls = 0 means no handshake");

        // The verdict about the Runtime Executor came from the Runtime
        // Executor, over the same reverse ssh.
        let executor = rep.runtime_audit.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(executor.audit.user, "ccrun");
        assert!(executor.root.usable());

        let calls = fake.calls();
        assert_eq!(
            calls.len(),
            4,
            "{:?}",
            calls.iter().map(Cmd::display).collect::<Vec<_>>()
        );
        assert_eq!(
            calls[0].display(),
            "ssh -o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 -o ForwardAgent=no -o ClearAllForwardings=yes -G to-runtime"
        );
        let reverse = calls[1].display();
        assert!(
            reverse.contains("ControlMaster=no"),
            "doctor path must not start a master: {reverse}"
        );
        assert!(
            reverse.contains("-T to-runtime /opt/runtime/ccnm internal hello --payload"),
            "{reverse}"
        );
        // The hello asked the Runtime side to look at the workspace root.
        let wire = calls[1].args.last().unwrap().to_string_lossy().into_owned();
        let sent: HelloRequest = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(
            sent.root,
            Some(PathBuf::from("/Users/ccrun/Projects/xshun"))
        );
        let audit_call = calls[2].display();
        assert!(
            audit_call.contains("-T to-runtime /opt/runtime/ccnm internal runtime-audit --payload"),
            "{audit_call}"
        );
        assert!(
            calls[3]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );

        // Nothing was written by probe.
        assert!(
            !control(&dir).exists(),
            "probe must not create the control dir"
        );

        let json = serde_json::to_vec(&rep).unwrap();
        let back: ProbeReport = crate::protocol::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn probe_records_failures_instead_of_aborting() {
        let dir = temp("probe-fail");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        let mut unreachable = Output::exited(255, "");
        unreachable.stderr =
            b"ssh: connect to host home.ts port 22: Operation timed out\n".to_vec();
        fake.push(unreachable);

        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: absent_socket("probe-fail"),
        };
        let rep = probe(
            &ProbeRequest {
                agent: None,

                mcp_calls: 5,
                ..request()
            },
            &tools,
        );
        let err = rep.runtime_hello.unwrap().unwrap_err();
        assert_eq!(err.code(), ErrorCode::RuntimeUnreachable);
        assert!(err.message.contains("Operation timed out"));
        assert_eq!(rep.mcp, None, "no MCP attempt after a failed hello");
        assert_eq!(rep.agent.path, None);
        assert_eq!(rep.agent.version.unwrap_err().code(), ErrorCode::Version);
        assert_eq!(fake.calls().len(), 2, "no claude calls without a binary");
    }

    #[test]
    fn missing_runtime_binary_is_a_version_error_naming_the_path() {
        let dir = temp("probe-127");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        fake.push(Output::exited(127, ""));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: absent_socket("probe-127"),
        };
        let rep = probe(&request(), &tools);
        let err = rep.runtime_hello.unwrap().unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message.contains("/opt/runtime/ccnm"), "{}", err.message);
    }

    fn run_request(prompt: &str) -> RunRequest {
        RunRequest {
            agent: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            workspace: "fixture".into(),
            root: PathBuf::from("/Users/bing/ccnm-fixture"),
            runtime_node: "runtime".into(),
            provider_config_dir: None,
            permission_mode: crate::config::PermissionMode::AcceptEdits,
            prompt: prompt.into(),
            timeout_secs: 5,
            codex_exec_server: false,
            agent_tools: Default::default(),
        }
    }

    /// The chain has no print mode (P21.1), and the answer is a refusal
    /// before anything is dialled, asked or written -- not an MCP session
    /// the workspace did not ask for.
    #[test]
    fn run_refuses_a_print_session_on_the_exec_server_chain_before_anything() {
        let dir = temp("run-native-print");
        let runner = FakeRunner::new();
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &runner,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: absent_socket("run-native-print"),
        };
        let mut req = run_request("x");
        req.provider = AgentProvider::Codex;
        req.permission_mode = crate::config::PermissionMode::default();
        req.codex_exec_server = true;
        let err = run(&req, &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("interactive"), "{err}");
        assert!(runner.calls().is_empty(), "nothing is dialled");
        assert!(!dir.join("sessions").exists(), "no session may be created");
        // The same word on a Claude request means nothing: Claude keeps
        // its MCP tools, and this run fails for the usual reason instead.
        req.provider = AgentProvider::Claude;
        req.permission_mode = crate::config::PermissionMode::AcceptEdits;
        let err = run(&req, &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
    }

    /// Two builds that still decode each other's control messages can
    /// still disagree about what a tool does, so a session is not built
    /// until both sides say the same version. `doctor` has always
    /// checked this; `doctor` is not what somebody runs before they
    /// suspect anything.
    #[test]
    fn a_session_is_not_started_against_a_different_build() {
        let dir = temp("greet-version");
        let fake = FakeRunner::new();
        let mut other = HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: None,
            root: Some(PathStatus {
                exists: true,
                is_dir: true,
            }),
        };
        other.ccnm_version = format!("{}-and-a-half", crate::VERSION);
        fake.push(Output::exited(0, serde_json::to_string(&other).unwrap()));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: dir.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        let err = greet(&ssh, "fixture", Path::new("/Users/bing/fixture"), &tools)
            .expect_err("a mismatched pair must not get a session");
        assert_eq!(err.code(), ErrorCode::Version);
        // Both versions, because "install the same build" is useless
        // without knowing which two builds are in play.
        assert!(err.message().contains(&other.ccnm_version), "{err}");
        assert!(err.message().contains(crate::VERSION), "{err}");
    }

    /// P3 makes the binding immutable: a config change cannot silently stop
    /// a running conversation, even when a replacement would be reachable.
    #[test]
    fn a_stale_session_is_refused_before_controller_network_or_kill() {
        let dir = temp("stale-greet");
        let id = "5f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";
        let sdir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(sdir.path()).unwrap();
        let spec = Spec {
            runtime_node: None,
            agent_identity: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            id: id.into(),
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/somewhere-else"),
            runtime: Some(RuntimeLink {
                alias: "to-runtime".into(),
                ccnm_bin: "/opt/runtime/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            mode: Mode::Interactive { prompt: None },
            timeout_secs: 0,
            cwd: dir.to_path_buf(),
            codex_exec_server: false,
            agent_tools: Default::default(),
        };
        std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));

        let tools = tmux_tools(&fake, &dir, "stale-greet");
        let err = start(&start_request(), &tools).expect_err("the binding changed");

        assert_eq!(err.code(), ErrorCode::NotReady, "{err}");
        assert!(err.message().contains("stop that exact session"), "{err}");
        let ran: Vec<String> = fake.calls().iter().map(|cmd| cmd.display()).collect();
        assert!(
            !ran.iter().any(|line| line.contains("kill-session")),
            "a binding mismatch must not reach the controller/network/kill: {ran:?}"
        );
        assert_eq!(ran.len(), 2);
    }

    /// A state directory too long for macOS's 104-byte `sun_path` could
    /// always start a session -- the MCP transport names no socket. The
    /// handshake must not change that, so it names no socket either: on
    /// this path nothing ever creates a master, so a ControlPath could
    /// only be used if some other command happened to leave one, while
    /// the length limit would apply every single time.
    #[test]
    fn a_state_directory_too_long_for_a_socket_still_starts_a_session() {
        let deep = std::env::temp_dir().join("x".repeat(crate::ssh::CONTROL_PATH_MAX_LEN));
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: deep.clone(),
            control_dir: deep.join("control"),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: deep.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        greet(&ssh, "fixture", Path::new("/Users/bing/fixture"), &tools)
            .expect("a long state directory is not a reason to refuse a session");

        let line = fake.calls()[0].display();
        assert!(line.contains("ControlPath=none"), "{line}");
        assert!(line.contains("ControlMaster=no"), "{line}");
        // And the path that would not fit never appears.
        assert!(!line.contains(&deep.display().to_string()), "{line}");
    }

    /// A project that moved used to be found out from inside the session,
    /// where it arrives as a tool blaming the program it could not run.
    /// One stat before anything starts, and the message says how to
    /// repoint the workspace.
    #[test]
    fn a_root_that_is_gone_is_refused_before_a_session_exists() {
        let dir = temp("greet-root");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, hello_json(false)));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: dir.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        let err = greet(&ssh, "fixture", Path::new("/Users/bing/moved"), &tools)
            .expect_err("a missing root must not get a session");
        assert_eq!(err.code(), ErrorCode::WrongWorkspace);
        assert!(err.message().contains("/Users/bing/moved"), "{err}");
        assert!(err.message().contains("missing"), "{err}");
        assert!(err.message().contains("ccnm ws add fixture"), "{err}");
    }

    fn start_request() -> StartRequest {
        StartRequest {
            agent: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/xshun"),
            runtime_node: "runtime".into(),
            provider_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            prompt: None,
            codex_exec_server: false,
            agent_tools: Default::default(),
        }
    }

    fn tmux_tools<'a>(fake: &'a FakeRunner, dir: &Path, test: &str) -> Tools<'a> {
        Tools {
            local: None,
            config: agent_config(),
            runner: fake,
            state: dir.to_path_buf(),
            control_dir: control(dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
            controller: absent_socket(test),
        }
    }

    /// `ccnm run` on a workspace that already has a session means "put me
    /// back into it". That must not need the controller: being let back in
    /// cannot depend on the component that starts things being healthy.
    #[test]
    fn start_on_a_live_session_reports_it_without_asking_the_controller() {
        let dir = temp("start-live");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.context(),
            r#"{"manager":"Background","keychain":true}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n"))); // show-environment
        fake.push(Output::exited(0, "4242\n")); // display-message

        let rep = start(&start_request(), &tmux_tools(&fake, &dir, "start-live")).unwrap();
        assert!(rep.already_running);
        assert_eq!(rep.tmux_session, "ccnm-xshun");
        assert_eq!(rep.session.as_deref(), Some(id));
        assert_eq!(rep.server_pid, 4242);
        assert_eq!(
            rep.context
                .as_ref()
                .map(session::Context::describe)
                .as_deref(),
            Some("Background, keychain reachable")
        );
        assert!(rep.controller.is_none(), "no controller was needed");
        assert!(
            rep.summary().contains("already running"),
            "{}",
            rep.summary()
        );
    }

    /// With nothing running, starting one needs the controller, and the
    /// same login-session rule print mode has: a Claude started anywhere
    /// else cannot read its own credentials.
    #[test]
    fn start_without_a_controller_creates_nothing() {
        let dir = temp("start-none");
        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "")); // has-session: nothing there
        let err = start(&start_request(), &tmux_tools(&fake, &dir, "start-none")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    /// A session's root is fixed when it starts. Being handed back into
    /// one that works somewhere else is how a moved project turns into an
    /// hour of tools failing for reasons that sound like something else --
    /// so the active one is never reused and must be explicitly stopped.
    #[test]
    fn start_never_replaces_a_live_session_bound_to_a_different_root() {
        let dir = temp("moved-root");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let sdir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(sdir.path()).unwrap();
        let spec = Spec {
            runtime_node: None,
            agent_identity: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            id: id.into(),
            workspace: "xshun".into(),
            // Where it was when it started.
            root: PathBuf::from("/Users/bing/xshun"),
            runtime: Some(RuntimeLink {
                alias: "to-runtime".into(),
                ccnm_bin: "/opt/runtime/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            mode: Mode::Interactive { prompt: None },
            timeout_secs: 0,
            cwd: dir.to_path_buf(),
            codex_exec_server: false,
            agent_tools: Default::default(),
        };
        std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));

        // The request says somewhere else -- the project was moved.
        let mut req = start_request();
        req.root = PathBuf::from("/Users/bing/moved/xshun");
        // No controller, so the preflight fails. The stale session is
        // still running afterwards: nothing is torn down until a
        // replacement is known to be possible. This used to kill first
        // and discover the problem second, which cost the person their
        // conversation and gave them an unrelated error for it.
        let err = start(&req, &tmux_tools(&fake, &dir, "moved-root")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady, "{err}");
        let ran: Vec<String> = fake.calls().iter().map(|cmd| cmd.display()).collect();
        assert!(
            !ran.iter().any(|line| line.contains("kill-session")),
            "nothing may be killed before the preflight passes: {ran:?}"
        );
    }

    /// The same rule print mode has, and for the same reason: a Claude
    /// started outside the login session cannot read its own credentials,
    /// and the failure it produces is a lie about the machine.
    #[test]
    fn start_refuses_a_controller_outside_the_login_session() {
        let dir = temp("start-bg");
        let socket = PathBuf::from(format!("/tmp/ccnm-ws-bg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Background\n"));
            let tools = crate::controller::Tools {
                local: None,
                config: crate::Config::default(),
                config_path: None,
                runner: &inner,
                agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                    "/opt/homebrew/bin/claude",
                ))),
                tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "")); // has-session: nothing running
        let mut tools = tmux_tools(&fake, &dir, "start-bg");
        tools.controller = socket;
        let err = start(&start_request(), &tools).unwrap_err();
        served.join().unwrap();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("Background"), "{err}");
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn without_tmux_every_interactive_command_says_how_to_get_it() {
        let dir = temp("no-tmux");
        let fake = FakeRunner::new();
        let mut tools = tmux_tools(&fake, &dir, "no-tmux");
        tools.tmux = None;
        let err = start(&start_request(), &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Dependency);
        assert!(err.message().contains("brew install tmux"), "{err}");

        let stop_err = stop(
            &StopRequest {
                agent: None,
                session: None,

                protocol: PROTOCOL,
                workspace: "xshun".into(),
            },
            &tools,
        )
        .unwrap_err();
        assert_eq!(stop_err.code(), ErrorCode::Dependency);

        // Status reports it as a row rather than failing: it is a status
        // command, and "tmux is not installed" is the status.
        let rep = status(
            &StatusRequest {
                agent: None,
                session: None,

                protocol: PROTOCOL,
                workspace: None,
            },
            &tools,
        );
        assert!(rep.sessions.is_empty());
        assert!(
            rep.render().contains("brew install tmux"),
            "{}",
            rep.render()
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn stopping_what_is_not_running_is_not_an_error() {
        let dir = temp("stop-none");
        let fake = FakeRunner::new();
        // Exactly what tmux 3.7c says, on stderr, with exit 1.
        fake.push(Output {
            stderr: b"can't find session: ccnm-xshun\n".to_vec(),
            ..Output::exited(1, "")
        });
        let rep = stop(
            &StopRequest {
                agent: None,
                session: None,

                protocol: PROTOCOL,
                workspace: "xshun".into(),
            },
            &tmux_tools(&fake, &dir, "stop-none"),
        )
        .unwrap();
        assert!(!rep.killed);
        assert_eq!(rep.tmux_session, "ccnm-xshun");
    }

    /// Status is about live sessions, and what it says about each one is
    /// measured, not assumed: the security session comes from the file the
    /// supervisor wrote from inside it.
    #[test]
    fn status_lists_live_sessions_with_what_was_measured_about_them() {
        let dir = temp("status");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.context(),
            r#"{"manager":"Background","keychain":true}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.mcp_config(),
            r#"{"mcpServers":{"ccnm":{"command":"/usr/bin/ssh","args":["-T","home","ccnm","internal","mcp-serve","--payload","eyJwIjoxfQ"]}}}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "tmux 3.7c\n")); // -V
        fake.push(Output::exited(
            0,
            "ccnm-xshun\t1788496263\t1\t1\nccnm-other\t1788496264\t0\t1\n",
        ));
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        // ps: the transport for that payload is running.
        fake.push(Output::exited(
            0,
            "/usr/bin/ssh -T to-runtime ccnm internal mcp-serve --payload eyJwIjoxfQ\nlogin -pf me\n",
        ));
        // The other session is tagged, but its directory has no mcp.json,
        // so the transport question cannot be put at all.
        fake.push(Output::exited(0, "CCNM_SESSION=no-such-session\n"));

        let rep = status(
            &StatusRequest {
                agent: None,
                session: None,

                protocol: PROTOCOL,
                workspace: None,
            },
            &tmux_tools(&fake, &dir, "status"),
        );
        assert_eq!(rep.tmux, Ok("3.7c".into()));
        assert_eq!(rep.sessions.len(), 2);
        assert_eq!(rep.sessions[0].workspace.as_deref(), Some("xshun"));
        assert_eq!(rep.sessions[0].session.as_deref(), Some(id));
        assert_eq!(
            rep.sessions[0].context.as_ref().unwrap().describe(),
            "Background, keychain reachable"
        );
        assert_eq!(rep.sessions[0].tools, Some(true));
        assert_eq!(rep.sessions[1].session.as_deref(), Some("no-such-session"));
        assert_eq!(rep.sessions[1].context, None);
        assert_eq!(
            rep.sessions[1].tools, None,
            "a question that could not be put is unknown, never a guessed no"
        );
        let text = rep.render();
        assert!(
            text.contains(
                "ccnm-xshun  xshun  1 attached  tools connected  (Background, keychain reachable)"
            ),
            "{text}"
        );
        assert!(
            text.contains("ccnm-other  other  detached  tools unknown  (context unknown)"),
            "{text}"
        );
    }

    /// The interruption `--print` cannot survive: the ssh carrying the
    /// call dies, the session runs on and writes its answer, and without
    /// this the answer is on another machine with no way to ask for it.
    #[test]
    fn result_finds_the_last_print_session_and_reads_what_it_wrote() {
        let dir = temp("result");
        let write_session = |id: &str, mode: Mode, stdout: Option<&str>| {
            let sdir = session::Dir::at(paths::session_dir(&dir, id));
            std::fs::create_dir_all(sdir.path()).unwrap();
            let spec = Spec {
                runtime_node: None,
                agent_identity: None,
                provider: Default::default(),
                protocol: PROTOCOL,
                id: id.to_string(),
                workspace: "fixture".into(),
                root: PathBuf::from("/Users/bing/ccnm-fixture"),
                runtime: Some(RuntimeLink {
                    alias: "home".into(),
                    ccnm_bin: "ccnm".into(),
                }),
                provider_config_dir: None,
                permission_mode: crate::config::PermissionMode::default(),
                mode,
                timeout_secs: 600,
                cwd: dir.to_path_buf(),
                codex_exec_server: false,
                agent_tools: Default::default(),
            };
            std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();
            if let Some(text) = stdout {
                std::fs::write(sdir.stdout(), text).unwrap();
                std::fs::write(
                    sdir.exit(),
                    r#"{"exit_code":0,"timed_out":false,"duration_ms":4200}"#,
                )
                .unwrap();
            }
            sdir
        };

        let older = write_session(
            "11111111-1111-4111-8111-111111111111",
            Mode::Print {
                prompt: "old".into(),
            },
            Some(r#"{"is_error":false,"result":"the older answer","num_turns":1}"#),
        );
        // Make the wanted one newer by a clear margin.
        std::thread::sleep(Duration::from_millis(1100));
        write_session(
            "22222222-2222-4222-8222-222222222222",
            Mode::Print {
                prompt: "new".into(),
            },
            Some(r#"{"is_error":false,"result":"the answer nobody heard","num_turns":3}"#),
        );
        // Newest of all, and not what `ccnm result` is for.
        std::thread::sleep(Duration::from_millis(1100));
        write_session(
            "33333333-3333-4333-8333-333333333333",
            Mode::Interactive { prompt: None },
            None,
        );

        let fake = FakeRunner::new();
        let tools = tmux_tools(&fake, &dir, "result");
        let rep = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.session, "22222222-2222-4222-8222-222222222222");
        assert_eq!(rep.mode, "print");
        assert_eq!(
            rep.result.as_ref().unwrap().text(),
            Some("the answer nobody heard")
        );
        assert!(rep.outcome.as_ref().unwrap().ok());
        assert!(
            rep.summary().contains("exited 0 in 4.2 s"),
            "{}",
            rep.summary()
        );

        // Naming one explicitly reaches an older session, and an
        // interactive one.
        let older_id = "11111111-1111-4111-8111-111111111111";
        let rep = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some(older_id.into()),
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.result.unwrap().text(), Some("the older answer"));
        assert_eq!(rep.session_dir, older.path());

        // "Most recent" is by time, not by whatever order the directory
        // happens to be read in: touch the older one and it wins.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(older.meta(), std::fs::read_to_string(older.meta()).unwrap()).unwrap();
        let rep = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.session, older_id, "newest by mtime, not by read order");

        let rep = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some("33333333-3333-4333-8333-333333333333".into()),
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.mode, "interactive");
        assert!(rep.outcome.is_none(), "still running");

        let missing = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some("44444444-4444-4444-8444-444444444444".into()),
            },
            &tools,
        )
        .unwrap_err();
        assert_eq!(missing.code(), ErrorCode::NotReady);
    }

    /// A workspace with only interactive sessions has nothing stored to
    /// hand back, and the error says where the output actually went.
    #[test]
    fn result_without_a_print_session_says_where_to_look_instead() {
        let dir = temp("result-none");
        std::fs::create_dir_all(paths::sessions_dir(&dir)).unwrap();
        let fake = FakeRunner::new();
        let err = result(
            &ResultRequest {
                agent: None,

                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tmux_tools(&fake, &dir, "result-none"),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("ccnm attach fixture"), "{err}");
    }

    /// A session whose transport died is the worst failure this system
    /// has: the terminal works, the model answers, and every tool it has
    /// is gone. The status line has to say so and say what to do.
    #[test]
    fn a_session_that_lost_its_tools_says_how_to_get_them_back() {
        let dir = temp("tools-down");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.mcp_config(),
            r#"{"mcpServers":{"ccnm":{"command":"/usr/bin/ssh","args":["--payload","eyJwIjoxfQ"]}}}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "tmux 3.7c\n"));
        fake.push(Output::exited(0, "ccnm-xshun\t1788496263\t0\t1\n"));
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        // ps: everything else on the machine, but not that transport.
        fake.push(Output::exited(0, "/usr/bin/ssh -T home something else\n"));

        let rep = status(
            &StatusRequest {
                agent: None,
                session: None,

                protocol: PROTOCOL,
                workspace: None,
            },
            &tmux_tools(&fake, &dir, "tools-down"),
        );
        assert_eq!(rep.sessions[0].tools, Some(false));
        let text = rep.render();
        assert!(text.contains("TOOLS DOWN"), "{text}");
        assert!(text.contains("/mcp -> ccnm -> Reconnect"), "{text}");
    }

    #[test]
    fn unmeasured_colocated_execution_is_refused_before_controller_or_children() {
        let dir = temp("colocated");
        let caller = FakeRunner::new();
        let tools = Tools {
            local: None,
            config: colocated_config(),
            runner: &caller,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: absent_socket("colocated"),
        };
        let mut req = run_request("do the thing");
        req.runtime_node = "agent".into(); // this node is also the project's
        req.root = dir.join("colocated-project");
        std::fs::create_dir(&req.root).unwrap();
        let error = run(&req, &tools).unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotReady);
        assert!(error.message().contains("disabled until"), "{error}");
        assert!(
            caller.calls().is_empty(),
            "unsupported topology must fail before any process: {:?}",
            caller.calls().iter().map(Cmd::display).collect::<Vec<_>>()
        );
        assert!(!paths::sessions_dir(&dir).exists());
    }

    #[test]
    fn run_refuses_without_a_controller_and_creates_nothing() {
        let dir = temp("run-none");
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &FakeRunner::new(),
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: absent_socket("run-none"),
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn run_refuses_a_controller_outside_the_login_session() {
        let dir = temp("run-bg");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-bg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Background\n"));
            let tools = crate::controller::Tools {
                local: None,
                config: crate::Config::default(),
                config_path: None,
                runner: &inner,
                agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                    "/opt/homebrew/bin/claude",
                ))),
                tmux: None,
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
        });
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &FakeRunner::new(),
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: socket,
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        served.join().unwrap();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("Background"), "{err}");
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn start_failure_after_session_creation_leaves_a_terminal_record() {
        let dir = temp("run-start-failure");
        let socket = PathBuf::from(format!(
            "/tmp/ccnm-wr-start-failure-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let runner = FakeRunner::new();
            runner.push(Output::exited(0, "Aqua\n"));
            let tools = crate::controller::Tools {
                config: crate::Config::default(),
                local: None,
                config_path: None,
                runner: &runner,
                agents: crate::provider::AgentBinaries::with_claude(None),
                tmux: None,
                exe: PathBuf::from("/synthetic/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
            listener.serve_one(&tools).unwrap();
        });
        let runner = FakeRunner::new();
        runner.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            config: agent_config(),
            local: None,
            runner: &runner,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: socket,
        };
        assert!(run(&run_request("fixture"), &tools).is_err());
        served.join().unwrap();
        let records: Vec<_> = std::fs::read_dir(paths::sessions_dir(&dir))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(records.len(), 1);
        let session = session::Dir::at(records[0].path());
        let outcome = session::read_outcome(&session).unwrap().unwrap();
        assert!(!outcome.ok());
        assert!(outcome.error.unwrap().contains("not found"));
    }

    /// The whole print-mode path with everything real except Claude: a
    /// real socket, the real Start handler, a real detached spawn of a
    /// stand-in supervisor, the real wait, the real parse. The stand-in
    /// records the argv it was given and finishes the session the way the
    /// real supervisor would: by writing `exit` last.
    #[test]
    fn run_starts_a_session_through_the_controller_and_brings_back_the_result() {
        let dir = temp("run-ok");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-ok-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let sessions = dir.join("sessions");
        let supervisor = dir.join("fake-supervisor");
        std::fs::write(
            &supervisor,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nfor s in {sessions}/*/; do\n  printf '{{\"is_error\":false,\"result\":\"hi from claude\",\"num_turns\":1}}' > \"$s/stdout\"\n  : > \"$s/stderr\"\n  printf '{{\"exit_code\":0,\"timed_out\":false,\"duration_ms\":42}}' > \"$s/exit.tmp\"\n  mv \"$s/exit.tmp\" \"$s/exit\"\ndone\n",
                argv = dir.join("supervisor-argv").display(),
                sessions = sessions.display(),
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&supervisor, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn({
            let supervisor = supervisor.clone();
            move || {
                let inner = FakeRunner::new();
                inner.push(Output::exited(0, "Aqua\n"));
                let tools = crate::controller::Tools {
                    local: None,
                    config: crate::Config::default(),
                    config_path: None,
                    runner: &inner,
                    agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                        "/opt/homebrew/bin/claude",
                    ))),
                    tmux: None,
                    exe: supervisor,
                };
                listener.serve_one(&tools).unwrap(); // hello
                listener.serve_one(&tools).unwrap(); // start
            }
        });

        // The version-and-root handshake `run` makes before it builds a
        // session.
        let caller = FakeRunner::new();
        caller.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &caller,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: socket,
        };
        let rep = run(&run_request("fix the failing test"), &tools).unwrap();
        served.join().unwrap();

        assert!(rep.outcome.ok(), "{:?}", rep.outcome);
        assert_eq!(rep.outcome.duration_ms, 42);
        let result = rep.result.expect("a parsed result");
        assert_eq!(result.text(), Some("hi from claude"));
        assert!(rep.stdout_tail.is_empty(), "no tail when the result parsed");
        assert!(rep.pid > 0);
        assert!(rep.controller.login_session());

        // The session directory is the report's, named by the id, with
        // the inputs Claude would have been started with.
        assert_eq!(rep.session_dir, sessions.join(&rep.session));
        let session_dir = crate::session::Dir::at(&rep.session_dir);
        let spec = crate::session::load(&session_dir).unwrap();
        assert_eq!(spec.workspace, "fixture");
        assert_eq!(spec.cwd, dir.join("workspaces/fixture"));
        assert!(
            spec.cwd.is_dir(),
            "Claude's cwd must exist before it starts"
        );
        assert!(session_dir.mcp_config().exists());
        assert!(session_dir.settings().exists());
        assert!(session_dir.supervisor_log().exists());

        // The supervisor got exactly one payload naming this session and
        // the controller's claude, not this session's.
        let argv = std::fs::read_to_string(dir.join("supervisor-argv")).unwrap();
        let mut lines = argv.lines();
        assert_eq!(lines.next(), Some("internal"));
        assert_eq!(lines.next(), Some("supervise"));
        assert_eq!(lines.next(), Some("--payload"));
        let req: crate::session::SuperviseRequest =
            crate::protocol::payload::decode(lines.next().unwrap()).unwrap();
        assert_eq!(req.session_dir, rep.session_dir);
        assert_eq!(req.agent_bin, PathBuf::from("/opt/homebrew/bin/claude"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail(b"short"), "short");
        let long = format!("{}中文结尾", "x".repeat(3000));
        let t = tail(long.as_bytes());
        assert!(t.starts_with("..."));
        assert!(t.ends_with("中文结尾"));
        assert!(t.len() <= 2048 + 3 + 3, "{}", t.len());
    }

    #[test]
    fn provider_probe_errors_never_expose_the_agent_private_profile() {
        let selected = SelectedAgent {
            provider: AgentProvider::Claude,
            identity: None,
            profile_dir: Some("/agent/private/claude".into()),
        };
        let error = ErrorReport::new(
            ErrorCode::Auth,
            "unexpected output from /agent/private/claude/.credentials.json",
        );
        let report = redact_agent_report(
            &selected,
            AgentReport {
                path: Some("/agent/claude".into()),
                version: Err(error.clone()),
                auth: Err(error),
            },
        );
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("/agent/private/claude"));
        assert!(text.contains("<agent-private-config>"));
    }

    /// The point of the whole phase: when a controller is listening, every
    /// question about Claude goes to it, and this ssh session runs no
    /// `claude` at all -- not even the version, because the controller's
    /// PATH is the one Claude will really be started from.
    #[test]
    fn with_a_controller_claude_is_asked_there_and_not_here() {
        let dir = temp("probe-controller");
        let socket = PathBuf::from(format!("/tmp/ccnm-wp-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();

        // The controller's own environment: a login session, and a claude
        // that answers both questions.
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Aqua\n"));
            inner.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
            inner.push(Output::exited(
                0,
                r#"{"loggedIn":true,"email":"me@x","authMethod":"claude.ai"}"#,
            ));
            let tools = crate::controller::Tools {
                local: None,
                config: crate::Config::default(),
                config_path: None,
                runner: &inner,
                agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                    "/opt/homebrew/bin/claude",
                ))),
                tmux: None,
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap(); // hello
            listener.serve_one(&tools).unwrap(); // claude-auth
            inner.calls()
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        fake.push(Output::exited(0, audit_json()));
        let tools = Tools {
            local: None,
            config: agent_config(),
            runner: &fake,
            state: dir.to_path_buf(),
            control_dir: control(&dir),
            agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                "/usr/local/bin/claude",
            ))),
            tmux: None,
            controller: socket.clone(),
        };
        let rep = probe(&request(), &tools);

        let ctx = rep.controller.as_ref().unwrap().as_ref().unwrap();
        assert!(ctx.login_session(), "{ctx:?}");
        assert!(rep.agent.auth.as_ref().unwrap().logged_in);
        assert_eq!(rep.agent.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.agent.path,
            Some(PathBuf::from("/opt/homebrew/bin/claude")),
            "the binary reported must be the controller's, not this session's"
        );
        // ssh -G, the hello, and the Runtime Executor's own audit.
        let ssh_calls: Vec<String> = fake.calls().iter().map(Cmd::display).collect();
        assert_eq!(ssh_calls.len(), 3, "{ssh_calls:?}");
        assert!(
            !ssh_calls.iter().any(|c| c.contains("claude")),
            "the ssh session must not run claude itself: {ssh_calls:?}"
        );

        // ...and the controller ran exactly the two claude commands, with
        // the config dir from the request.
        let inner_calls = served.join().unwrap();
        assert_eq!(inner_calls.len(), 3);
        assert!(inner_calls[1].display().contains("--version"));
        assert!(inner_calls[2].display().contains("auth status"));
        assert!(
            inner_calls[2]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );
    }
}

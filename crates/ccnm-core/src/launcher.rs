//! The home-launcher role's commands other than doctor: `ccnm mcp probe`
//! (the phase 1B persistence measurement) and `ccnm run --print`.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::{Resolved, Topology};
use crate::error::{Error, ErrorCode, Result};
use crate::mcp;
use crate::process::{Cmd, ProcessRunner};
use crate::protocol::PROTOCOL;
use crate::protocol::mcp::{ProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport as WorkProbeReport, ProbeRequest};
use crate::protocol::run::{
    AttachRequest, HistoryReport, HistoryRequest, PurgeReport, PurgeRequest, ResultReport,
    ResultRequest, RunReport, RunRequest, StartReport, StartRequest, StatusReport, StatusRequest,
    StopReport, StopRequest,
};
use crate::provider::AgentProvider;
use crate::ssh::{Master, Ssh};

/// `ccnm run <workspace> --print <prompt>`: one Claude session on the
/// Agent Node, its result brought back here.
///
/// The local preflight is only what this machine can see (design doc
/// section 10): the project must exist here, because here is where the
/// runtime will serve it from. Everything about the Agent Node is
/// checked by the Agent Node and reported back in the same round trip.
pub fn run_print(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    prompt: &str,
    timeout: Duration,
) -> Result<RunReport> {
    run_print_with_agent(resolved, env, prompt, timeout, None)
}

pub fn run_print_with_agent(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    prompt: &str,
    timeout: Duration,
    agent: Option<&str>,
) -> Result<RunReport> {
    check_local_root(resolved)?;
    let root = &resolved.workspace.root;
    let ssh = Ssh::new(resolved.agent_ssh()?, &env.control_dir)?
        .with_ccnm_bin(resolved.require_agent()?.ccnm_bin());
    ssh.check_control_path()?;
    let selected = resolved.agent_reference(agent)?;
    let req = RunRequest {
        agent: selected.clone(),

        provider: Default::default(),
        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
        root: root.clone(),
        runtime_node: resolved.workspace.runtime_node.clone(),
        provider_config_dir: resolved
            .agent
            .and_then(|node| AgentProvider::current().config_dir(node))
            .map(|dir| dir.to_path_buf()),
        permission_mode: AgentProvider::current().permission_mode(resolved.workspace),
        prompt: prompt.to_string(),
        timeout_secs: timeout.as_secs(),
        codex_exec_server: resolved.workspace.codex_exec_server,
        agent_tools: resolved.workspace.agent_tools.clone(),
    };
    // The Agent side waits the session timeout plus its grace; this call
    // has to outlive both, plus the ssh itself.
    let report: RunReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-run"],
        &req,
        timeout + Duration::from_secs(120),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), report.agent_identity.as_ref())?;
    Ok(report)
}

pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on the Runtime Node.
    pub control_dir: PathBuf,
    /// This binary, for the local (no ssh) probe.
    pub current_exe: PathBuf,
}

/// `ccnm run <workspace>`: bring up the interactive session on the work
/// machine, without attaching to it yet.
///
/// Same local preflight as [`run_print`], and for the same reason: the
/// project has to be here, because here is where the runtime serves it
/// from.
pub fn start_interactive(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    prompt: Option<&str>,
) -> Result<StartReport> {
    start_interactive_with_agent(resolved, env, prompt, None)
}

pub fn start_interactive_with_agent(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    prompt: Option<&str>,
    agent: Option<&str>,
) -> Result<StartReport> {
    let ssh = agent_ssh(resolved, env)?;
    let selected = resolved.agent_reference(agent)?;
    let req = StartRequest {
        agent: selected.clone(),

        provider: Default::default(),
        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
        root: resolved.workspace.root.clone(),
        runtime_node: resolved.workspace.runtime_node.clone(),
        provider_config_dir: resolved
            .agent
            .and_then(|node| AgentProvider::current().config_dir(node))
            .map(|dir| dir.to_path_buf()),
        permission_mode: AgentProvider::current().permission_mode(resolved.workspace),
        prompt: prompt.map(str::to_string),
        codex_exec_server: resolved.workspace.codex_exec_server,
        agent_tools: resolved.workspace.agent_tools.clone(),
    };
    let report: StartReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-start"],
        &req,
        Duration::from_secs(120),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), report.agent_identity.as_ref())?;
    Ok(report)
}

/// The command that hands this terminal to the Agent Node's tmux.
///
/// `-t` because the far side needs a terminal to give Claude, and no
/// timeout because this lasts as long as the person wants it to. Run it
/// with [`crate::process::run_attached`]: it needs this process's real
/// stdin and stdout, not pipes.
/// Ask the Runtime Node what this workspace is, so the Agent Node can
/// start the session itself.
///
/// The Agent Node has no workspace list and must not grow one: the Runtime
/// is where a project's root is defined, and a second copy of that is a
/// second answer to "where is this project", which is how a session ends up
/// bound to a directory that has moved. So it asks, every time.
///
/// What it used to do instead was send the whole public `ccnm run` back
/// over ssh and let the Runtime start the session -- work -> home -> work.
/// That bought one definition of every workspace, and it cost the thing
/// P7.3 measured: the account the Agent lands on is the Runtime Executor,
/// so *it* ran the public launcher, and it needed an outbound key back to
/// the Agent Node to do it. An execution identity that dials out is not
/// confined by anything the account itself can prove
/// (docs/production-safety.md).
///
/// Now only the question crosses. The answer is workspace data -- root,
/// runtime node, instance reference, permission mode -- and the session is
/// created here, where the Agent runs, by the same `work::start` the
/// Runtime-initiated path reaches over ssh. The far side starts nothing,
/// so nothing on the far side needs to dial anywhere.
pub fn resolve_from_agent(
    runtime_alias: &str,
    runtime_ccnm_bin: &str,
    workspace: &str,
    agent: Option<&str>,
    env: &Env<'_>,
) -> Result<crate::runtime::ResolveReport> {
    let ssh = Ssh::new(runtime_alias, env.control_dir.clone())?.with_ccnm_bin(runtime_ccnm_bin);
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "runtime-resolve"],
        &crate::runtime::ResolveRequest::new(workspace, agent),
        Duration::from_secs(60),
        ErrorCode::RuntimeUnreachable,
    )
}

pub fn attach_cmd(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<Cmd> {
    attach_cmd_selected(resolved, env, None, None)
}

pub fn attach_cmd_selected(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    agent: Option<&str>,
    session: Option<&str>,
) -> Result<Cmd> {
    let ssh = agent_ssh(resolved, env)?;
    let selected = resolved.agent_reference(agent)?;
    let wire = payload::encode(&AttachRequest {
        agent: selected.clone(),
        session: session.map(str::to_string),

        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
    })?;
    ssh.interactive_ccnm_cmd(&["internal", "attach", "--payload", &wire])
}

pub fn stop(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<StopReport> {
    stop_selected(resolved, env, None, None)
}

pub fn stop_selected(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    agent: Option<&str>,
    session: Option<&str>,
) -> Result<StopReport> {
    let ssh = agent_ssh(resolved, env)?;
    let selected = resolved.agent_reference(agent)?;
    let req = StopRequest {
        agent: selected.clone(),
        session: session.map(str::to_string),

        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
    };
    let report: StopReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-stop"],
        &req,
        Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), report.agent_identity.as_ref())?;
    Ok(report)
}

/// What a session produced, for a `--print` run whose ssh did not survive
/// to hear the answer.
pub fn result(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    session: Option<&str>,
) -> Result<ResultReport> {
    result_selected(resolved, env, session, None)
}

pub fn result_selected(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    session: Option<&str>,
    agent: Option<&str>,
) -> Result<ResultReport> {
    let ssh = agent_ssh(resolved, env)?;
    let selected = resolved.agent_reference(agent)?;
    let req = ResultRequest {
        agent: selected.clone(),

        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
        session: session.map(str::to_string),
    };
    let report: ResultReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-result"],
        &req,
        Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), report.agent_identity.as_ref())?;
    Ok(report)
}

/// Delete what ccnm kept for a workspace, on both machines.
///
/// The Agent Node knows which sessions belonged to it; this machine
/// holds the other half of those same sessions (what `exec_command`
/// printed). Neither half is the project.
pub fn purge(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<PurgeReport> {
    let ssh = agent_ssh(resolved, env)?;
    let req = PurgeRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
    };
    let mut report: PurgeReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-purge"],
        &req,
        Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )?;

    // This machine's half: the retained output of those same sessions.
    if let Ok(state) = crate::paths::state_dir() {
        for id in &report.sessions {
            let dir = crate::paths::session_dir(&state, id);
            if dir.is_dir() && std::fs::remove_dir_all(&dir).is_ok() {
                report.removed.push(dir.display().to_string());
            }
        }
    }
    Ok(report)
}

pub fn status(resolved: &Resolved<'_>, env: &Env<'_>, all: bool) -> Result<StatusReport> {
    status_selected(resolved, env, all, None, None)
}

pub fn status_selected(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    all: bool,
    agent: Option<&str>,
    session: Option<&str>,
) -> Result<StatusReport> {
    let ssh = agent_ssh(resolved, env)?;
    let selected = if all {
        if agent.is_some() || session.is_some() {
            return Err(Error::invalid_args(
                "--all cannot be combined with an Agent or exact session selection",
            ));
        }
        None
    } else {
        resolved.agent_reference(agent)?
    };
    let req = StatusRequest {
        agent: selected.clone(),
        session: session.map(str::to_string),

        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: (!all).then(|| resolved.name.to_string()),
    };
    let report: StatusReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-status"],
        &req,
        Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), report.agent_identity.as_ref())?;
    Ok(report)
}

/// The Agent Node's session records, for `ccnm log`.
pub fn history(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    workspace: Option<&str>,
    limit: u32,
) -> Result<HistoryReport> {
    let ssh = agent_ssh(resolved, env)?;
    let req = HistoryRequest {
        protocol: PROTOCOL,
        workspace: workspace.map(str::to_string),
        limit,
    };
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "agent-history"],
        &req,
        Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )
    .map_err(|e| {
        // The one failure this command adds: an Agent from before it.
        if e.message().contains("agent-history") {
            Error::new(
                ErrorCode::Version,
                format!(
                    "the ccnm on the Agent Node is too old for `ccnm log`; install the same version on both machines\n({})",
                    e.message()
                ),
            )
        } else {
            e
        }
    })
}

fn verify_identity(
    requested: Option<&crate::instance::InstanceRef>,
    returned: Option<&crate::instance::AgentIdentity>,
) -> Result<()> {
    match (requested, returned) {
        (None, None) => Ok(()),
        (Some(requested), Some(returned)) if returned.reference() == *requested => Ok(()),
        _ => Err(Error::new(
            ErrorCode::Version,
            "Agent response identity differs from the Runtime selection",
        )),
    }
}

/// Refuse before the network when the project is not where this machine
/// is supposed to be keeping it.
///
/// Only when this machine is the Runtime Node. In the other two
/// topologies the project is on the far side -- the agent's own disk when
/// the two roles are colocated, or another machine entirely when this one
/// is only launching -- and checking a path here would reject a workspace
/// that is perfectly fine, on evidence this machine does not have.
fn check_local_root(resolved: &Resolved<'_>) -> Result<()> {
    if resolved.topology() != Topology::FromRuntime {
        return Ok(());
    }
    let root = &resolved.workspace.root;
    if !root.is_dir() {
        return Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace root {} is not a directory on this machine, which is the Runtime Node for '{}'",
                root.display(),
                resolved.name
            ),
        ));
    }
    Ok(())
}

/// The ssh to the Agent Node, with the project checked here first.
fn agent_ssh(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<Ssh> {
    check_local_root(resolved)?;
    let ssh = Ssh::new(resolved.agent_ssh()?, &env.control_dir)?
        .with_ccnm_bin(resolved.require_agent()?.ccnm_bin());
    ssh.check_control_path()?;
    Ok(ssh)
}

/// A fresh session id for a probe; the retained-output directory of a
/// real session will be named the same way.
pub fn probe_session_id() -> String {
    format!("probe-{}", uuid::Uuid::new_v4().hyphenated())
}

/// Speak MCP to `ccnm internal mcp-serve` in a child of this process: the
/// runtime cost with no network in it (design doc section 27).
pub fn mcp_probe_local(resolved: &Resolved<'_>, env: &Env<'_>, calls: u32) -> Result<ProbeReport> {
    let wire = payload::encode(&ServePayload::new(
        resolved.name,
        resolved.workspace.root.clone(),
        &probe_session_id(),
    ))?;
    let cmd = Cmd::new(&env.current_exe).args(["internal", "mcp-serve", "--payload", &wire]);
    mcp::probe::probe(&cmd, calls, probe_timeout(calls), ErrorCode::Internal)
}

/// Ask the Agent Node to probe the Runtime Node over its own ssh: the
/// path Claude Code will use. Returns the MCP part of the work probe.
pub fn mcp_probe_remote(resolved: &Resolved<'_>, env: &Env<'_>, calls: u32) -> Result<ProbeReport> {
    mcp_probe_remote_selected(resolved, env, calls, None)
}

pub fn mcp_probe_remote_selected(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    calls: u32,
    agent: Option<&str>,
) -> Result<ProbeReport> {
    let ssh = Ssh::new(resolved.agent_ssh()?, &env.control_dir)?
        .with_ccnm_bin(resolved.require_agent()?.ccnm_bin());
    ssh.check_control_path()?;
    let selected = resolved.agent_reference(agent)?;
    let req = ProbeRequest {
        agent: selected.clone(),

        provider: Default::default(),
        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: resolved.name.to_string(),
        root: resolved.workspace.root.clone(),
        runtime_node: resolved.workspace.runtime_node.clone(),
        provider_config_dir: resolved
            .agent
            .and_then(|node| AgentProvider::current().config_dir(node))
            .map(|dir| dir.to_path_buf()),
        mcp_calls: calls,
        // An MCP probe: the exec-server chain has its own row in doctor.
        codex_exec_server: false,
    };
    let rep: WorkProbeReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "probe"],
        &req,
        probe_timeout(calls) + Duration::from_secs(60),
        ErrorCode::AgentUnreachable,
    )?;
    verify_identity(selected.as_ref(), rep.agent_identity.as_ref())?;
    match rep.mcp {
        Some(Ok(mcp)) => Ok(mcp),
        Some(Err(e)) => Err(e.into()),
        None => Err(match rep.runtime_hello {
            Some(Err(e)) => Error::new(
                ErrorCode::RuntimeUnreachable,
                format!("reverse ssh failed before the MCP probe: {}", e.message),
            ),
            None => Error::new(
                ErrorCode::Config,
                format!(
                    "workspace '{}' runs the agent and the project on the same node, so there is no MCP transport to probe",
                    resolved.name
                ),
            ),
            Some(Ok(_)) => Error::internal("the Agent Node did not run the MCP probe"),
        }),
    }
}

/// Generous per-call budget so a slow relay does not read as a hang.
pub fn probe_timeout(calls: u32) -> Duration {
    Duration::from_secs(30) + Duration::from_millis(500) * calls
}

#[cfg(test)]
mod tests {
    //! The two ways a session gets started, tested as whole loops rather
    //! than one hop at a time.
    //!
    //! ```text
    //! sitting at home   home --ssh--> work --ssh--> home
    //!                   (asks)        (runs Claude) (serves the project)
    //!
    //! sitting at work   work --ssh--> home --ssh--> work --ssh--> home
    //!                   (asks)        (the line above, from its start)
    //! ```
    //!
    //! Every process here is scripted, so nothing touches the network --
    //! but the *messages* are the real ones, encoded by the sending code
    //! and decoded by the receiving code. That is the point. Each end
    //! already has unit tests, and each of those builds the message it
    //! wants by hand; a swap of the two aliases would leave every one of
    //! them passing and bring up a session pointed at the wrong machine.
    //! Only running one end's output into the other end's input can see
    //! it.

    /// The config an Agent Node keeps: who it is, and the one alias it
    /// dials the projects by. `ccnm_bin` is non-default so a test that
    /// silently used the default path would fail instead of passing.
    fn agent_config() -> crate::config::Config {
        crate::config::Config::parse(
            "this = \"agent\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"to-runtime\"\nccnm_bin = \"/opt/runtime/ccnm\"\n",
        )
        .unwrap()
    }

    use super::*;
    use crate::config::{Config, PermissionMode};
    use crate::error::ErrorCode;
    use crate::process::{FakeRunner, Output};
    use crate::protocol::hello::{HelloReport, PathStatus};
    use crate::protocol::run::StartReport;
    use crate::session::{self, Mode};
    use crate::work;
    use ccnm_testdir::TestDir;

    fn temp(test: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-launcher-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    /// ControlPath expands to at most 103 bytes and macOS `temp_dir()` is
    /// most of that on its own, so sockets go straight under /tmp.
    fn control(test: &str) -> PathBuf {
        PathBuf::from("/tmp/ccnm-lt").join(format!("{}-{test}", std::process::id()))
    }

    /// What the Agent Node sends back when it has started a session.
    fn start_report_json() -> String {
        serde_json::to_string(&StartReport {
            agent_identity: None,

            provider: Default::default(),
            protocol: PROTOCOL,
            session: Some("2f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b".into()),
            session_dir: Some(PathBuf::from("/Users/bing/.local/state/ccnm/sessions/2f1e")),
            tmux_session: "ccnm-xshun".into(),
            server_pid: 4242,
            already_running: false,
            replaced: None,
            controller: None,
            context: None,
        })
        .unwrap()
    }

    /// What the Runtime Node answers the Agent Node's handshake with.
    fn hello_json() -> String {
        serde_json::to_string(&HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from("/opt/home/ccnm")),
            root: Some(PathStatus {
                exists: true,
                is_dir: true,
            }),
        })
        .unwrap()
    }

    /// The whole loop, home to work and back.
    ///
    /// `ccnm xshun` typed at home tells the Agent Node the alias it
    /// should come *back* on, and the Agent Node writes that alias into
    /// the session's `mcp.json` -- the ssh Claude starts to reach the
    /// project. This test carries one real message from each end into the
    /// other and checks that the project the third hop opens is the
    /// project the first hop named.
    ///
    /// The two aliases and the two binary paths are deliberately four
    /// different strings, so a swap cannot pass by coincidence. The same
    /// goes for the three things Claude itself is started with -- the
    /// permission mode, the config dir, the opening line: each is set to
    /// something other than its default, because "the default arrived"
    /// and "the config's value arrived" have to be told apart.
    #[test]
    fn the_alias_the_agent_dials_is_the_one_the_session_comes_back_on() {
        let dir = temp("loop");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();

        // ---- hop 1: home asks work ---------------------------------
        let config = Config::parse(&format!(
            "version = 1\n\
             this = \"runtime\"\n[nodes.agent]\nssh = \"to-work\"\nccnm_bin = \"/opt/work/ccnm\"\nclaude_config_dir = \"/x/claude\"\n\
             [nodes.runtime]\nccnm_bin = \"/opt/home/ccnm\"\n\
             [workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\nclaude_permission_mode = \"plan\"\n",
            root.display()
        ))
        .unwrap();
        let resolved = config.workspace("xshun").unwrap();

        let home = FakeRunner::new();
        home.push(Output::exited(0, start_report_json()));
        let env = Env {
            runner: &home,
            control_dir: control("loop"),
            current_exe: PathBuf::from("/opt/home/ccnm"),
        };
        start_interactive(&resolved, &env, Some("fix the failing test"))
            .expect("home's half of the call");

        let calls = home.calls();
        assert_eq!(calls.len(), 1, "one ssh, to one machine");
        let line = calls[0].display();
        assert!(
            line.contains("-T to-work /opt/work/ccnm internal agent-start"),
            "hop 1 goes to the Agent Node, running the Agent Node's ccnm: {line}"
        );

        // The message itself, decoded exactly the way work decodes it.
        let wire = calls[0].args.last().unwrap().to_string_lossy().into_owned();
        let req: StartRequest = payload::decode(&wire).unwrap();
        assert_eq!(
            req.runtime_node, "runtime",
            "work is told the alias to come back on, not the one home came in on"
        );
        assert_eq!(req.root, root);
        assert_eq!(req.workspace, "xshun");
        // What the supervisor will hand Claude: `--permission-mode`,
        // `CLAUDE_CONFIG_DIR`, the opening line. A default arriving here
        // in place of the config's value is a session with more or less
        // permission than the person wrote down, and nothing on the work
        // machine can tell.
        assert_eq!(req.permission_mode, PermissionMode::Plan);
        assert_eq!(req.provider_config_dir, Some(PathBuf::from("/x/claude")));
        assert_eq!(req.prompt.as_deref(), Some("fix the failing test"));

        // ---- hop 2: work builds the session -------------------------
        // From here this is the Agent Node: a controller in a login
        // session on a unix socket, and a scripted ssh back to home.
        let socket = PathBuf::from(format!("/tmp/ccnm-loop-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let state = dir.join("work-state");
        let watched = state.clone();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Aqua\n")); // hello: the login session
            inner.push(Output::exited(1, "")); // has-session: nothing up
            inner.push(Output::exited(0, "")); // new-session
            inner.push(Output::exited(0, "4242\n")); // the server's pid
            inner.push(Output::exited(0, "C-b\n")); // prefix, for the status bar
            inner.push(Output::exited(0, "")); // the status bar itself
            let tools = crate::controller::Tools {
                local: None,
                config: crate::Config::default(),
                config_path: None,
                runner: &inner,
                agents: crate::provider::AgentBinaries::with_claude(Some(PathBuf::from(
                    "/opt/homebrew/bin/claude",
                ))),
                tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
                exe: PathBuf::from("/opt/work/ccnm"),
            };
            listener.serve_one(&tools).expect("hello");
            listener.serve_one(&tools).expect("start");
            // Standing in for the supervisor, which writes this from
            // inside tmux a moment after the controller answers.
            if let Ok(entries) = std::fs::read_dir(crate::paths::sessions_dir(&watched)) {
                for entry in entries.flatten() {
                    let _ = std::fs::write(
                        session::Dir::at(entry.path()).context(),
                        r#"{"manager":"Aqua","keychain":true}"#,
                    );
                }
            }
        });

        let agent_runner = FakeRunner::new();
        agent_runner.push(Output::exited(1, "")); // has-session: nothing up yet
        agent_runner.push(Output::exited(0, hello_json())); // the handshake home
        let tools = work::Tools {
            local: None,
            config: agent_config(),
            runner: &agent_runner,
            state: state.clone(),
            control_dir: control("loop-work"),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
            controller: socket.clone(),
        };
        let rep = work::start(&req, &tools).expect("work's half of the call");
        served.join().unwrap();
        let _ = std::fs::remove_file(&socket);

        assert!(!rep.already_running);
        assert_eq!(rep.tmux_session, "ccnm-xshun");
        assert_eq!(rep.server_pid, 4242);

        // The version-and-root handshake went to the alias *home* named.
        let greeting = agent_runner.calls()[1].display();
        assert!(
            greeting.contains("-T to-runtime /opt/runtime/ccnm internal hello"),
            "hop 2 dials the runtime by the agent's own alias, running its ccnm: {greeting}"
        );

        // ---- hop 3: the session reaches back ------------------------
        // This file is what Claude Code will run. It is the only thing
        // the model has to touch the project with, and nothing later
        // rewrites it.
        let session_dir = session::Dir::at(rep.session_dir.clone().unwrap());
        let mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(session_dir.mcp_config()).unwrap()).unwrap();
        let server = &mcp["mcpServers"][crate::mcp::server::SERVER_NAME];
        assert_eq!(
            server["command"],
            std::env::current_exe().unwrap().to_string_lossy().as_ref()
        );
        let args: Vec<String> = server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        assert_eq!(&args[..3], ["internal", "agent-transport", "--payload"]);
        let local: session::transport::Request = payload::decode(&args[3]).unwrap();
        assert_eq!(local.session_dir, session_dir.path());
        let transport = session::transport::command(&session::load(&session_dir).unwrap()).unwrap();
        let args: Vec<String> = transport
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "--payload")
            .expect("the transport carries a payload");
        assert_eq!(
            args[at - 4],
            "to-runtime",
            "the third hop dials the runtime"
        );
        assert_eq!(
            args[at - 3],
            "/opt/runtime/ccnm",
            "running the runtime's ccnm"
        );
        assert_eq!(args[at - 2..at], ["internal", "mcp-serve"]);

        let serve: ServePayload = payload::decode(&args[at + 1]).unwrap();
        assert_eq!(
            serve.root, root,
            "the project the third hop opens is the one the first hop named"
        );
        assert_eq!(serve.workspace, "xshun");
        assert_eq!(serve.session, rep.session.clone().unwrap());
        assert!(
            serve.interactive,
            "somebody is at a terminal for this one, so exec_command may ask them"
        );

        // And the loop is one session, not two: the id in the transport
        // is the id of the directory holding it.
        let spec = session::load(&session_dir).unwrap();
        assert_eq!(spec.id, serve.session);
        assert_eq!(spec.runtime.as_ref().unwrap().alias, "to-runtime");
        assert_eq!(spec.root, root);
        // The spec is what the supervisor reads to start Claude, so this
        // is where the three values from hop 1 have to have landed.
        assert_eq!(spec.permission_mode, PermissionMode::Plan);
        assert_eq!(spec.provider_config_dir, Some(PathBuf::from("/x/claude")));
        assert_eq!(
            spec.mode,
            Mode::Interactive {
                prompt: Some("fix the failing test".into())
            }
        );
    }

    /// The same loop for `--print`: home asks, waits, and gets the answer
    /// back in the same call.
    ///
    /// Print mode has its own request type, its own Agent-side entry and
    /// its own copy of the request-to-spec mapping, so the interactive
    /// loop passing says nothing about it. Two things are specific to it.
    /// The transport must say *nobody is watching*: `exec_command` asks
    /// before it runs, and a print session that waited for an answer
    /// would wait for its whole timeout and report nothing. And the
    /// report comes back through the same ssh that carried the request,
    /// so the last hop is home decoding what work really wrote -- not a
    /// document the test made up.
    #[test]
    fn a_print_run_comes_back_on_the_alias_the_agent_dialled_and_asks_nobody() {
        let dir = temp("print-loop");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let config = Config::parse(&format!(
            "version = 1\n\
             this = \"runtime\"\n[nodes.agent]\nssh = \"to-work\"\nccnm_bin = \"/opt/work/ccnm\"\nclaude_config_dir = \"/x/claude\"\n\
             [nodes.runtime]\nccnm_bin = \"/opt/home/ccnm\"\n\
             [workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\nclaude_permission_mode = \"plan\"\n",
            root.display()
        ))
        .unwrap();
        let resolved = config.workspace("xshun").unwrap();

        // ---- hop 1: home asks work, and waits ----------------------
        // Work's real answer does not exist yet; it is made further down
        // and fed back at the end. This first call only has to send.
        let home = FakeRunner::new();
        home.push(Output::exited(0, "not an answer yet"));
        let env = Env {
            runner: &home,
            control_dir: control("print-loop"),
            current_exe: PathBuf::from("/opt/home/ccnm"),
        };
        let session = Duration::from_secs(60);
        let _ = run_print(&resolved, &env, "say hi", session);

        let calls = home.calls();
        assert_eq!(calls.len(), 1, "one ssh, to one machine");
        let line = calls[0].display();
        assert!(
            line.contains("-T to-work /opt/work/ccnm internal agent-run"),
            "{line}"
        );
        let wire = calls[0].args.last().unwrap().to_string_lossy().into_owned();
        let req: RunRequest = payload::decode(&wire).unwrap();
        assert_eq!(req.runtime_node, "runtime");
        assert_eq!(req.root, root);
        assert_eq!(req.workspace, "xshun");
        assert_eq!(req.prompt, "say hi");
        assert_eq!(req.timeout_secs, 60);
        assert_eq!(req.permission_mode, PermissionMode::Plan);
        assert_eq!(req.provider_config_dir, Some(PathBuf::from("/x/claude")));

        // ---- hop 2: work runs it ------------------------------------
        // A controller on a socket, and a script standing in for the
        // supervisor: it ends every session it finds the way the real one
        // would, by writing `exit` last.
        let socket = PathBuf::from(format!("/tmp/ccnm-ploop-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let state = dir.join("work-state");
        let sessions = crate::paths::sessions_dir(&state);
        let supervisor = dir.join("fake-supervisor");
        std::fs::write(
            &supervisor,
            format!(
                "#!/bin/sh\nfor s in {sessions}/*/; do\n  printf '{{\"is_error\":false,\"result\":\"hi from claude\",\"num_turns\":1}}' > \"$s/stdout\"\n  : > \"$s/stderr\"\n  printf '{{\"exit_code\":0,\"timed_out\":false,\"duration_ms\":42}}' > \"$s/exit.tmp\"\n  mv \"$s/exit.tmp\" \"$s/exit\"\ndone\n",
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
                inner.push(Output::exited(0, "Aqua\n")); // hello: the login session
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
                listener.serve_one(&tools).expect("hello");
                listener.serve_one(&tools).expect("start");
            }
        });

        let agent_runner = FakeRunner::new();
        agent_runner.push(Output::exited(0, hello_json())); // the handshake home
        let tools = work::Tools {
            local: None,
            config: agent_config(),
            runner: &agent_runner,
            state: state.clone(),
            control_dir: control("print-loop-work"),
            agents: crate::provider::AgentBinaries::with_claude(None),
            tmux: None,
            controller: socket.clone(),
        };
        let rep = work::run(&req, &tools).expect("work's half of the call");
        served.join().unwrap();
        let _ = std::fs::remove_file(&socket);

        assert!(rep.outcome.ok(), "{:?}", rep.outcome);
        assert_eq!(
            rep.result.as_ref().and_then(|r| r.text()),
            Some("hi from claude")
        );
        let greeting = agent_runner.calls()[0].display();
        assert!(
            greeting.contains("-T to-runtime /opt/runtime/ccnm internal hello"),
            "hop 2 dials the runtime by the agent's own alias, running its ccnm: {greeting}"
        );

        // ---- hop 3: the session reaches back ------------------------
        let session_dir = session::Dir::at(&rep.session_dir);
        let mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(session_dir.mcp_config()).unwrap()).unwrap();
        let args: Vec<String> = mcp["mcpServers"][crate::mcp::server::SERVER_NAME]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        assert_eq!(&args[..3], ["internal", "agent-transport", "--payload"]);
        let local: session::transport::Request = payload::decode(&args[3]).unwrap();
        assert_eq!(local.session_dir, session_dir.path());
        let transport = session::transport::command(&session::load(&session_dir).unwrap()).unwrap();
        let args: Vec<String> = transport
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|a| a == "--payload").unwrap();
        assert_eq!(
            args[at - 4],
            "to-runtime",
            "the third hop dials the runtime"
        );
        assert_eq!(
            args[at - 3],
            "/opt/runtime/ccnm",
            "running the runtime's ccnm"
        );
        let serve: ServePayload = payload::decode(&args[at + 1]).unwrap();
        assert_eq!(serve.root, root);
        assert_eq!(serve.workspace, "xshun");
        assert_eq!(serve.session, rep.session);
        assert!(
            !serve.interactive,
            "nobody is at a terminal, so exec_command must not wait for one"
        );

        let spec = session::load(&session_dir).unwrap();
        assert_eq!(spec.id, rep.session);
        assert_eq!(spec.runtime.as_ref().unwrap().alias, "to-runtime");
        assert_eq!(spec.permission_mode, PermissionMode::Plan);
        assert_eq!(spec.provider_config_dir, Some(PathBuf::from("/x/claude")));
        assert_eq!(
            spec.mode,
            Mode::Print {
                prompt: "say hi".into()
            }
        );

        // ---- and back: home decodes what work actually wrote ---------
        home.push(Output::exited(0, serde_json::to_string(&rep).unwrap()));
        let back = run_print(&resolved, &env, "say hi", session)
            .expect("home decodes the report work really produced");
        assert_eq!(back.session, rep.session);
        assert_eq!(
            back.result.and_then(|r| r.into_text()),
            Some("hi from claude".to_string())
        );
    }

    /// Direction two: the same session, asked for from the Agent Node.
    ///
    /// The Agent Node has no workspace list, so it asks -- and that is all
    /// that crosses. It used to send the *user-facing* `ccnm run` back to
    /// the Runtime and let that machine start the session, which meant the
    /// Runtime Executor ran the launcher and needed an outbound key to the
    /// Agent Node. Now the question goes over, the answer comes back, and
    /// the session is created on this side, where Claude runs anyway.
    #[test]
    fn from_the_agent_node_only_the_question_crosses() {
        let config = Config::parse(
            "this = \"agent\"\nruntime_node = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"to-runtime\"\nccnm_bin = \"/opt/runtime/ccnm\"\n",
        )
        .unwrap();
        let (alias, host) = config
            .runtime_from_agent()
            .expect("a config with no workspace list is the Agent Node's");

        let fake = FakeRunner::new();
        fake.push(Output::exited(
            0,
            r#"{"protocol":4,"workspace":"xshun","root":"/Users/me/xshun","runtime_node":"runtime","agent":{"node":"agent","instance":"claude-main"},"claude_config_dir":null,"permission_mode":"acceptEdits"}"#,
        ));
        let env = Env {
            runner: &fake,
            control_dir: control("resolve"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        let report = resolve_from_agent(alias, &host.ccnm_bin(), "xshun", None, &env).unwrap();
        assert_eq!(report.root, PathBuf::from("/Users/me/xshun"));
        assert_eq!(report.runtime_node, "runtime");

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "one question, and no second hop");
        let line = calls[0].display();
        assert!(
            line.contains("-T to-runtime /opt/runtime/ccnm internal runtime-resolve --payload"),
            "{line}"
        );
        // Nothing that starts anything is sent over. The far side reads its
        // config and answers; it does not launch, and it does not dial.
        assert!(!line.contains(" run xshun"), "{line}");
        assert!(!line.contains("--detached"), "{line}");
        let at = calls[0]
            .args
            .iter()
            .position(|a| a == "--payload")
            .expect("a payload argument");
        let sent: crate::runtime::ResolveRequest =
            payload::decode(&calls[0].args[at + 1].to_string_lossy()).unwrap();
        assert_eq!(sent.workspace, "xshun");
        assert_eq!(sent.agent, None);
    }

    /// The opening line does not make the trip any more.
    ///
    /// It used to, on stdin, because the far side was the one starting the
    /// session and a prompt with a quote or a newline in it cannot go on an
    /// unquoted remote command line (`ssh::is_remote_safe`). Now the
    /// session starts here, so the Runtime never sees what the person
    /// typed: [`resolve_from_agent`] has nowhere to put it, which the
    /// compiler enforces better than any assertion here could.
    ///
    /// What is still worth pinning is the rule that forced it onto stdin in
    /// the first place, because the question that replaced it still crosses
    /// a login shell: every argument after the alias must survive it
    /// unquoted.
    #[test]
    fn what_crosses_now_still_survives_an_unquoted_login_shell() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(
            0,
            r#"{"protocol":4,"workspace":"xshun","root":"/p","runtime_node":"runtime","agent":null,"claude_config_dir":null,"permission_mode":"acceptEdits"}"#,
        ));
        let env = Env {
            runner: &fake,
            control_dir: control("prompt-over"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        resolve_from_agent(
            "to-home",
            "/opt/home/ccnm",
            "xshun",
            Some("codex-main"),
            &env,
        )
        .unwrap();

        let call = fake.calls().remove(0);
        assert_eq!(call.stdin, None, "nothing rides stdin any more");
        let alias = call.args.iter().position(|a| a == "to-home").unwrap();
        for arg in call.args.iter().skip(alias + 1) {
            let arg = arg.to_string_lossy();
            assert!(
                crate::ssh::is_remote_safe(&arg),
                "{arg} would need quoting on the way over"
            );
        }
    }

    /// The Runtime Node keeping ccnm somewhere other than the default is
    /// a supported config, and it is set on the Agent Node's own file.
    /// Ignoring it produced "command not found" for a path the person had
    /// spelled out correctly -- so this pins that the configured path is
    /// the one that gets run, and that being wrong says which path it
    /// tried and where to fix it.
    #[test]
    fn where_ccnm_lives_on_the_runtime_machine_is_read_and_named() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(127, ""));
        let env = Env {
            runner: &fake,
            control_dir: control("notfound"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        let err = resolve_from_agent("to-home", "/opt/homebrew/bin/ccnm", "xshun", None, &env)
            .unwrap_err();

        assert!(
            fake.calls()[0]
                .display()
                .contains("/opt/homebrew/bin/ccnm internal runtime-resolve"),
            "the configured path is the one that runs: {}",
            fake.calls()[0].display()
        );
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("/opt/homebrew/bin/ccnm"), "{err}");
        assert!(err.message().contains("ccnm_bin"), "{err}");
    }

    /// A Runtime that never answered and a Runtime that answered "no" are
    /// two different problems with two different fixes, and the Agent Node
    /// only ever sees an exit code. Reporting a refusal as unreachable
    /// sends somebody to debug their network over a typo'd workspace name.
    ///
    /// The refusal now keeps the far side's own error code as well:
    /// `CCNM_E_CONFIG` on the way in stays `CCNM_E_CONFIG` on the way out,
    /// where the delegated public command could only ever say "the Runtime
    /// failed".
    #[test]
    fn a_runtime_that_refused_is_not_reported_as_one_that_was_not_there() {
        fn env_for(fake: &FakeRunner) -> Env<'_> {
            Env {
                runner: fake,
                control_dir: control("refused"),
                current_exe: PathBuf::from("/opt/work/ccnm"),
            }
        }

        let down = FakeRunner::new();
        let mut timeout = Output::exited(255, "");
        timeout.stderr = b"ssh: connect to host to-home port 22: Operation timed out\n".to_vec();
        down.push(timeout);
        let err = resolve_from_agent("to-home", "/opt/home/ccnm", "xshun", None, &env_for(&down))
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::RuntimeUnreachable);
        assert!(err.message().contains("Operation timed out"), "{err}");

        // Reached, looked, and said no. Its words are relayed and its code
        // survives the trip.
        let refused = FakeRunner::new();
        let mut no = Output::exited(ErrorCode::Config.exit_code(), "");
        no.stderr =
            b"CCNM_E_CONFIG:\nworkspace 'xshun' is not defined; defined: fixture\n".to_vec();
        refused.push(no);
        let err = resolve_from_agent(
            "to-home",
            "/opt/home/ccnm",
            "xshun",
            None,
            &env_for(&refused),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("xshun"), "{err}");
        assert!(err.message().contains("runtime-resolve"), "{err}");
    }

    /// Both roles refuse before the network when the project is not on
    /// this machine, because this machine is the one that would serve it.
    /// The check is in `agent_ssh`, which every Runtime-side command goes
    /// through, so it is worth a test that names all of them.
    #[test]
    fn every_home_side_command_checks_the_project_is_here_first() {
        let dir = temp("no-root");
        let config = Config::parse(&format!(
            "version = 1\n\
             this = \"runtime\"\n[nodes.agent]\nssh = \"to-work\"\n\
             [nodes.runtime]\n\
             [workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\n",
            dir.join("gone").display()
        ))
        .unwrap();
        let resolved = config.workspace("xshun").unwrap();
        let fake = FakeRunner::new();
        let env = Env {
            runner: &fake,
            control_dir: control("no-root"),
            current_exe: PathBuf::from("/opt/home/ccnm"),
        };

        let errors = [
            start_interactive(&resolved, &env, None).unwrap_err(),
            run_print(&resolved, &env, "hi", Duration::from_secs(5)).unwrap_err(),
            attach_cmd(&resolved, &env).unwrap_err(),
            stop(&resolved, &env).unwrap_err(),
            status(&resolved, &env, false).unwrap_err(),
            result(&resolved, &env, None).unwrap_err(),
        ];
        for err in errors {
            assert_eq!(err.code(), ErrorCode::WrongWorkspace, "{err}");
            assert!(err.message().contains("is not a directory"), "{err}");
        }
        assert!(
            fake.calls().is_empty(),
            "nothing may go over the network before the project is found: {:?}",
            fake.calls().iter().map(Cmd::display).collect::<Vec<_>>()
        );
    }

    /// `--print` waits for the answer, so its ssh has to outlive the
    /// session's own timeout *and* the Agent side's grace on top of it. A
    /// timeout shorter than the thing it is waiting for turns every long
    /// run into "the Agent Node is unreachable".
    #[test]
    fn the_print_call_outlives_the_session_it_is_waiting_for() {
        let dir = temp("print");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let config = Config::parse(&format!(
            "version = 1\n\
             this = \"runtime\"\n[nodes.agent]\nssh = \"to-work\"\n\
             [nodes.runtime]\n\
             [workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\n",
            root.display()
        ))
        .unwrap();
        let resolved = config.workspace("xshun").unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "not json"));
        let env = Env {
            runner: &fake,
            control_dir: control("print"),
            current_exe: PathBuf::from("/opt/home/ccnm"),
        };
        let session = Duration::from_secs(600);
        let _ = run_print(&resolved, &env, "hello", session);

        let cmd = &fake.calls()[0];
        assert!(
            cmd.timeout > session,
            "the ssh must outlive the session: {:?} vs {session:?}",
            cmd.timeout
        );
        let wire = cmd.args.last().unwrap().to_string_lossy().into_owned();
        let req: RunRequest = payload::decode(&wire).unwrap();
        assert_eq!(req.timeout_secs, session.as_secs());
        assert_eq!(req.prompt, "hello");
        assert_eq!(req.runtime_node, "runtime");
    }

    /// Nothing here should need to know what a path looks like on the
    /// other machine, but `mcp_transport_cmd` refuses anything that would
    /// need shell quoting -- so a `ccnm_bin` with a space in it has to
    /// fail where somebody can read it, not inside a session.
    #[test]
    fn a_remote_path_that_would_need_quoting_is_refused_by_name() {
        let ssh = Ssh::new("to-home", control("quote"))
            .unwrap()
            .with_ccnm_bin("/Users/me/my tools/ccnm");
        let err = ssh.mcp_transport_cmd("x").unwrap_err();
        assert!(err.message().contains("my tools"), "{err}");
    }
}

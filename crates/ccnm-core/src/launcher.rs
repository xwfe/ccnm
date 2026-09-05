//! The home-launcher role's commands other than doctor: `ccnm mcp probe`
//! (the phase 1B persistence measurement) and `ccnm run --print`.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::Resolved;
use crate::error::{Error, ErrorCode, Result};
use crate::mcp;
use crate::process::{Cmd, ProcessRunner};
use crate::protocol::PROTOCOL;
use crate::protocol::mcp::{ProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport as WorkProbeReport, ProbeRequest};
use crate::protocol::run::{
    AttachRequest, PurgeReport, PurgeRequest, ResultReport, ResultRequest, RunReport, RunRequest,
    StartReport, StartRequest, StatusReport, StatusRequest, StopReport, StopRequest,
};
use crate::ssh::{Master, RemoteOutcome, Ssh};

/// `ccnm run <workspace> --print <prompt>`: one Claude session on the
/// work machine, its result brought back here.
///
/// The local preflight is only what this machine can see (design doc
/// section 10): the project must exist here, because here is where the
/// runtime will serve it from. Everything about the work machine is
/// checked by the work machine and reported back in the same round trip.
pub fn run_print(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    prompt: &str,
    timeout: Duration,
) -> Result<RunReport> {
    let root = &resolved.workspace.root;
    if !root.is_dir() {
        return Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace root {} is not a directory on this machine, and this machine is the runtime host",
                root.display()
            ),
        ));
    }
    let ssh =
        Ssh::new(resolved.work_ssh, &env.control_dir)?.with_ccnm_bin(resolved.work.ccnm_bin());
    ssh.check_control_path()?;
    let req = RunRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
        root: root.clone(),
        home_alias: resolved.home_alias.to_string(),
        home_ccnm_bin: resolved.runtime.ccnm_bin(),
        claude_config_dir: resolved.work.claude_config_dir.clone(),
        permission_mode: resolved.workspace.claude_permission_mode,
        prompt: prompt.to_string(),
        timeout_secs: timeout.as_secs(),
    };
    // The work side waits the session timeout plus its grace; this call
    // has to outlive both, plus the ssh itself.
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-run"],
        &req,
        timeout + Duration::from_secs(120),
        ErrorCode::WorkUnreachable,
    )
}

pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on the home machine.
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
    let ssh = work_ssh(resolved, env)?;
    let req = StartRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
        root: resolved.workspace.root.clone(),
        home_alias: resolved.home_alias.to_string(),
        home_ccnm_bin: resolved.runtime.ccnm_bin(),
        claude_config_dir: resolved.work.claude_config_dir.clone(),
        permission_mode: resolved.workspace.claude_permission_mode,
        prompt: prompt.map(str::to_string),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-start"],
        &req,
        Duration::from_secs(120),
        ErrorCode::WorkUnreachable,
    )
}

/// The command that hands this terminal to the work machine's tmux.
///
/// `-t` because the far side needs a terminal to give Claude, and no
/// timeout because this lasts as long as the person wants it to. Run it
/// with [`crate::process::run_attached`]: it needs this process's real
/// stdin and stdout, not pipes.
/// Start a session from the *work* machine, by asking the home machine to
/// do it.
///
/// The work machine has no workspace list and must not grow one: the home
/// machine is where a project's root is defined, and a second copy of that
/// is a second answer to "where is this project", which is how a session
/// ends up bound to a directory that has moved.
///
/// So this delegates the whole thing -- config lookup, the version and
/// root handshake, the controller -- to exactly the code path that runs
/// when somebody types the command at home. The session is created on
/// this machine either way, because that is where Claude runs; all that
/// changes is who asked for it. Attaching afterwards needs no config at
/// all, only the workspace name, so it happens locally.
///
/// The cost is one extra hop, work -> home -> work. That buys a single
/// definition of every workspace and not one line of duplicated
/// launching.
///
/// `prompt` is the line Claude opens with, and it does **not** go on the
/// command line: it is free text, and nothing that would need shell
/// quoting is allowed on a remote command line (design doc section 8,
/// [`crate::ssh::is_remote_safe`]). It rides the connection's stdin
/// instead, which is bytes and needs no quoting, so `--prompt-stdin`
/// tells the far side to read it there. Newlines and quotes survive.
pub fn start_from_work(
    home_alias: &str,
    home_ccnm_bin: &str,
    workspace: &str,
    prompt: Option<&str>,
    env: &Env<'_>,
) -> Result<()> {
    let ssh = Ssh::new(home_alias, env.control_dir.clone())?.with_ccnm_bin(home_ccnm_bin);
    let mut argv: Vec<&str> = vec![ssh.ccnm_bin(), "run", workspace, "--detached"];
    if prompt.is_some() {
        argv.push("--prompt-stdin");
    }
    let mut cmd = ssh.remote_cmd(Master::Reuse, &argv, Duration::from_secs(180))?;
    if let Some(prompt) = prompt {
        cmd = cmd.stdin(prompt.as_bytes());
    }
    let out = env.runner.run(&cmd)?;
    // Same three diagnoses every other remote call gets, from the same
    // function. This used to read the exit code by hand, so the two
    // failures that have a fix in one sentence -- ccnm is somewhere else
    // over there, ccnm is not executable over there -- arrived as "could
    // not start the session" with nothing under it.
    match crate::ssh::classify(out) {
        RemoteOutcome::Unreachable(why) => Err(Error::new(
            ErrorCode::HomeUnreachable,
            format!("ssh {home_alias}: {why}"),
        )),
        RemoteOutcome::CommandNotFound => Err(Error::new(
            ErrorCode::Version,
            format!(
                "{home_ccnm_bin} not found on {home_alias} (the login shell exited 127)\ninstall the same ccnm build there, or set ccnm_bin under [hosts.home] in this machine's config.toml"
            ),
        )),
        RemoteOutcome::NotExecutable => Err(Error::new(
            ErrorCode::Version,
            format!(
                "{home_ccnm_bin} on {home_alias} is there but not executable (exit 126)\nssh {home_alias} 'chmod +x {home_ccnm_bin}'"
            ),
        )),
        RemoteOutcome::Completed(out) => {
            // The far side already says everything worth saying about the
            // session it started, and it says it on stderr -- including
            // why it refused, when it refused. Relayed as it is, then a
            // single line saying whose failure it was.
            let said = out.stderr_lossy();
            if !said.trim().is_empty() {
                eprint!("{said}");
            }
            if !out.success() {
                return Err(Error::new(
                    ErrorCode::HomeUnreachable,
                    format!(
                        "{home_alias} could not start `{workspace}`{}",
                        match out.exit_code {
                            Some(code) => format!(" (ccnm there exited {code})"),
                            None => " (ccnm there was killed)".to_string(),
                        }
                    ),
                ));
            }
            Ok(())
        }
    }
}

pub fn attach_cmd(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<Cmd> {
    let ssh = work_ssh(resolved, env)?;
    let wire = payload::encode(&AttachRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
    })?;
    ssh.interactive_ccnm_cmd(&["internal", "attach", "--payload", &wire])
}

pub fn stop(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<StopReport> {
    let ssh = work_ssh(resolved, env)?;
    let req = StopRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-stop"],
        &req,
        Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
    )
}

/// What a session produced, for a `--print` run whose ssh did not survive
/// to hear the answer.
pub fn result(
    resolved: &Resolved<'_>,
    env: &Env<'_>,
    session: Option<&str>,
) -> Result<ResultReport> {
    let ssh = work_ssh(resolved, env)?;
    let req = ResultRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
        session: session.map(str::to_string),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-result"],
        &req,
        Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
    )
}

/// Delete what ccnm kept for a workspace, on both machines.
///
/// The work machine knows which sessions belonged to it; this machine
/// holds the other half of those same sessions (what `exec_command`
/// printed). Neither half is the project.
pub fn purge(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<PurgeReport> {
    let ssh = work_ssh(resolved, env)?;
    let req = PurgeRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
    };
    let mut report: PurgeReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-purge"],
        &req,
        Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
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
    let ssh = work_ssh(resolved, env)?;
    let req = StatusRequest {
        protocol: PROTOCOL,
        workspace: (!all).then(|| resolved.name.to_string()),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "work-status"],
        &req,
        Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
    )
}

/// The ssh to the work machine, with the project checked here first.
fn work_ssh(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<Ssh> {
    let root = &resolved.workspace.root;
    if !root.is_dir() {
        return Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace root {} is not a directory on this machine, and this machine is the runtime host",
                root.display()
            ),
        ));
    }
    let ssh =
        Ssh::new(resolved.work_ssh, &env.control_dir)?.with_ccnm_bin(resolved.work.ccnm_bin());
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

/// Ask the work machine to probe the home runtime over its own ssh: the
/// path Claude Code will use. Returns the MCP part of the work probe.
pub fn mcp_probe_remote(resolved: &Resolved<'_>, env: &Env<'_>, calls: u32) -> Result<ProbeReport> {
    let ssh =
        Ssh::new(resolved.work_ssh, &env.control_dir)?.with_ccnm_bin(resolved.work.ccnm_bin());
    ssh.check_control_path()?;
    let req = ProbeRequest {
        protocol: PROTOCOL,
        workspace: resolved.name.to_string(),
        root: resolved.workspace.root.clone(),
        home_alias: resolved.home_alias.to_string(),
        home_ccnm_bin: resolved.runtime.ccnm_bin(),
        claude_config_dir: resolved.work.claude_config_dir.clone(),
        mcp_calls: calls,
    };
    let rep: WorkProbeReport = ssh.call_ccnm(
        env.runner,
        Master::Reuse,
        &["internal", "probe"],
        &req,
        probe_timeout(calls) + Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
    )?;
    match rep.mcp {
        Some(Ok(mcp)) => Ok(mcp),
        Some(Err(e)) => Err(e.into()),
        None => Err(match rep.home_hello {
            Err(e) => Error::new(
                ErrorCode::HomeUnreachable,
                format!("reverse ssh failed before the MCP probe: {}", e.message),
            ),
            Ok(_) => Error::internal("work did not run the MCP probe"),
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

    use super::*;
    use crate::config::{Config, PermissionMode};
    use crate::error::ErrorCode;
    use crate::process::{FakeRunner, Output};
    use crate::protocol::hello::{HelloReport, PathStatus};
    use crate::protocol::run::StartReport;
    use crate::session::{self, Mode};
    use crate::work;

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-launcher-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ControlPath expands to at most 103 bytes and macOS `temp_dir()` is
    /// most of that on its own, so sockets go straight under /tmp.
    fn control(test: &str) -> PathBuf {
        PathBuf::from("/tmp/ccnm-lt").join(format!("{}-{test}", std::process::id()))
    }

    /// What the work machine sends back when it has started a session.
    fn start_report_json() -> String {
        serde_json::to_string(&StartReport {
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

    /// What the home machine answers the work machine's handshake with.
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
    /// `ccnm xshun` typed at home tells the work machine the alias it
    /// should come *back* on, and the work machine writes that alias into
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
    fn the_alias_home_sends_is_the_one_the_session_comes_back_on() {
        let dir = temp("loop");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();

        // ---- hop 1: home asks work ---------------------------------
        let config = Config::parse(&format!(
            "version = 1\n\
             [hosts.work]\nssh = \"to-work\"\nccnm_bin = \"/opt/work/ccnm\"\nclaude_config_dir = \"/x/claude\"\n\
             [hosts.home]\nssh_from_work = \"to-home\"\nccnm_bin = \"/opt/home/ccnm\"\n\
             [workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nclaude_permission_mode = \"plan\"\n",
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
            line.contains("-T to-work /opt/work/ccnm internal work-start"),
            "hop 1 goes to the work machine, running the work machine's ccnm: {line}"
        );

        // The message itself, decoded exactly the way work decodes it.
        let wire = calls[0].args.last().unwrap().to_string_lossy().into_owned();
        let req: StartRequest = payload::decode(&wire).unwrap();
        assert_eq!(
            req.home_alias, "to-home",
            "work is told the alias to come back on, not the one home came in on"
        );
        assert_eq!(req.home_ccnm_bin, "/opt/home/ccnm");
        assert_eq!(req.root, root);
        assert_eq!(req.workspace, "xshun");
        // What the supervisor will hand Claude: `--permission-mode`,
        // `CLAUDE_CONFIG_DIR`, the opening line. A default arriving here
        // in place of the config's value is a session with more or less
        // permission than the person wrote down, and nothing on the work
        // machine can tell.
        assert_eq!(req.permission_mode, PermissionMode::Plan);
        assert_eq!(req.claude_config_dir, Some(PathBuf::from("/x/claude")));
        assert_eq!(req.prompt.as_deref(), Some("fix the failing test"));

        // ---- hop 2: work builds the session -------------------------
        // From here this is the work machine: a controller in a login
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
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
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

        let work_runner = FakeRunner::new();
        work_runner.push(Output::exited(1, "")); // has-session: nothing up yet
        work_runner.push(Output::exited(0, hello_json())); // the handshake home
        let tools = work::Tools {
            runner: &work_runner,
            state: state.clone(),
            control_dir: control("loop-work"),
            claude: None,
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
        let greeting = work_runner.calls()[1].display();
        assert!(
            greeting.contains("-T to-home /opt/home/ccnm internal hello"),
            "hop 2's check goes home, running home's ccnm: {greeting}"
        );

        // ---- hop 3: the session reaches back ------------------------
        // This file is what Claude Code will run. It is the only thing
        // the model has to touch the project with, and nothing later
        // rewrites it.
        let session_dir = session::Dir::at(rep.session_dir.clone().unwrap());
        let mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(session_dir.mcp_config()).unwrap()).unwrap();
        let server = &mcp["mcpServers"][crate::mcp::server::SERVER_NAME];
        assert_eq!(server["command"], session::SSH_BIN);
        let args: Vec<String> = server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "--payload")
            .expect("the transport carries a payload");
        assert_eq!(args[at - 4], "to-home", "the third hop goes home");
        assert_eq!(args[at - 3], "/opt/home/ccnm", "running home's ccnm");
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
        assert_eq!(spec.home_alias, "to-home");
        assert_eq!(spec.root, root);
        // The spec is what the supervisor reads to start Claude, so this
        // is where the three values from hop 1 have to have landed.
        assert_eq!(spec.permission_mode, PermissionMode::Plan);
        assert_eq!(spec.claude_config_dir, Some(PathBuf::from("/x/claude")));
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
    /// Print mode has its own request type, its own work-side entry and
    /// its own copy of the request-to-spec mapping, so the interactive
    /// loop passing says nothing about it. Two things are specific to it.
    /// The transport must say *nobody is watching*: `exec_command` asks
    /// before it runs, and a print session that waited for an answer
    /// would wait for its whole timeout and report nothing. And the
    /// report comes back through the same ssh that carried the request,
    /// so the last hop is home decoding what work really wrote -- not a
    /// document the test made up.
    #[test]
    fn a_print_run_comes_back_on_the_alias_home_sent_and_asks_nobody() {
        let dir = temp("print-loop");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let config = Config::parse(&format!(
            "version = 1\n\
             [hosts.work]\nssh = \"to-work\"\nccnm_bin = \"/opt/work/ccnm\"\nclaude_config_dir = \"/x/claude\"\n\
             [hosts.home]\nssh_from_work = \"to-home\"\nccnm_bin = \"/opt/home/ccnm\"\n\
             [workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nclaude_permission_mode = \"plan\"\n",
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
            line.contains("-T to-work /opt/work/ccnm internal work-run"),
            "{line}"
        );
        let wire = calls[0].args.last().unwrap().to_string_lossy().into_owned();
        let req: RunRequest = payload::decode(&wire).unwrap();
        assert_eq!(req.home_alias, "to-home");
        assert_eq!(
            req.home_ccnm_bin, "/opt/home/ccnm",
            "print mode names home's ccnm for the way back, like interactive does"
        );
        assert_eq!(req.root, root);
        assert_eq!(req.workspace, "xshun");
        assert_eq!(req.prompt, "say hi");
        assert_eq!(req.timeout_secs, 60);
        assert_eq!(req.permission_mode, PermissionMode::Plan);
        assert_eq!(req.claude_config_dir, Some(PathBuf::from("/x/claude")));

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
                    runner: &inner,
                    claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                    tmux: None,
                    exe: supervisor,
                };
                listener.serve_one(&tools).expect("hello");
                listener.serve_one(&tools).expect("start");
            }
        });

        let work_runner = FakeRunner::new();
        work_runner.push(Output::exited(0, hello_json())); // the handshake home
        let tools = work::Tools {
            runner: &work_runner,
            state: state.clone(),
            control_dir: control("print-loop-work"),
            claude: None,
            tmux: None,
            controller: socket.clone(),
        };
        let rep = work::run(&req, &tools).expect("work's half of the call");
        served.join().unwrap();
        let _ = std::fs::remove_file(&socket);

        assert!(rep.outcome.ok(), "{:?}", rep.outcome);
        assert_eq!(
            rep.result.as_ref().and_then(|r| r.result.as_deref()),
            Some("hi from claude")
        );
        let greeting = work_runner.calls()[0].display();
        assert!(
            greeting.contains("-T to-home /opt/home/ccnm internal hello"),
            "hop 2's check goes home, running home's ccnm: {greeting}"
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
        let at = args.iter().position(|a| a == "--payload").unwrap();
        assert_eq!(args[at - 4], "to-home", "the third hop goes home");
        assert_eq!(args[at - 3], "/opt/home/ccnm", "running home's ccnm");
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
        assert_eq!(spec.home_alias, "to-home");
        assert_eq!(spec.permission_mode, PermissionMode::Plan);
        assert_eq!(spec.claude_config_dir, Some(PathBuf::from("/x/claude")));
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
            back.result.and_then(|r| r.result),
            Some("hi from claude".to_string())
        );
    }

    /// Direction two: the same session, asked for from the work machine.
    ///
    /// The work machine has no workspace list, so it cannot build the
    /// start request itself -- it runs the *user-facing* command on the
    /// home machine and lets home do what it does when somebody types it
    /// there. That is the whole design: one definition of every
    /// workspace, and no second copy of the launching code.
    #[test]
    fn from_the_work_machine_the_entire_start_is_delegated_home() {
        let config = Config::parse(
            "[hosts.home]\nssh_from_work = \"to-home\"\nccnm_bin = \"/opt/home/ccnm\"\n",
        )
        .unwrap();
        let (alias, host) = config
            .home_from_work()
            .expect("a config with only a way home is the work machine's");

        let fake = FakeRunner::new();
        let mut started = Output::exited(0, "");
        started.stderr = b"ccnm-xshun (started, tmux server pid 22413)\n".to_vec();
        fake.push(started);
        let env = Env {
            runner: &fake,
            control_dir: control("delegate"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        start_from_work(alias, &host.ccnm_bin(), "xshun", None, &env).unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "one hop, and nothing decided on this side");
        let line = calls[0].display();
        assert!(
            line.contains("-T to-home /opt/home/ccnm run xshun --detached"),
            "{line}"
        );
        // --detached is not decoration. Without it the far side would sit
        // there waiting to attach a terminal that is on this machine.
        assert!(line.ends_with("--detached"), "{line}");
    }

    /// The opening line makes the trip, and it makes it on stdin.
    ///
    /// Two halves, and the second one is the point. That the prompt
    /// arrives is half: it used to be dropped without a word, so somebody
    /// typed a sentence and Claude opened with nothing. That it arrives
    /// *on stdin* is the other half, and it is not a style choice --
    /// remote command lines are unquoted (`ssh::is_remote_safe`), so a
    /// prompt with a quote or a newline in it either gets refused or,
    /// worse, gets taken apart by the far side's login shell. stdin is
    /// bytes. The prompt below has a quote, an apostrophe, a backtick and
    /// a newline in it for exactly that reason.
    #[test]
    fn an_opening_line_typed_at_work_rides_stdin_not_the_command_line() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, ""));
        let env = Env {
            runner: &fake,
            control_dir: control("prompt-over"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        let prompt = "fix the \"failing\" test\nit's in `mod tests`";
        start_from_work("to-home", "/opt/home/ccnm", "xshun", Some(prompt), &env).unwrap();

        let call = fake.calls().remove(0);
        let line = call.display();
        assert!(
            line.ends_with("/opt/home/ccnm run xshun --detached --prompt-stdin"),
            "the far side is told to read it from stdin: {line}"
        );
        assert!(
            !line.contains("failing"),
            "not one word of it on the command line: {line}"
        );
        assert_eq!(
            call.stdin.as_deref(),
            Some(prompt.as_bytes()),
            "byte for byte, newline included"
        );
        // The rule that forced the prompt onto stdin still holds for
        // everything that did stay on the remote line -- everything after
        // the alias, which is what the far side's login shell reads.
        let alias = call.args.iter().position(|a| a == "to-home").unwrap();
        for arg in call.args.iter().skip(alias + 1) {
            let arg = arg.to_string_lossy();
            assert!(
                crate::ssh::is_remote_safe(&arg),
                "{arg} would need quoting on the way over"
            );
        }
    }

    /// The home machine keeping ccnm somewhere other than the default is
    /// a supported config, and it is set on the work machine's own file.
    /// Ignoring it produced "command not found" for a path the person had
    /// spelled out correctly -- so this pins that the configured path is
    /// the one that gets run, and that being wrong says which path it
    /// tried and where to fix it.
    #[test]
    fn where_ccnm_lives_on_the_home_machine_is_read_and_named() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(127, ""));
        let env = Env {
            runner: &fake,
            control_dir: control("notfound"),
            current_exe: PathBuf::from("/opt/work/ccnm"),
        };
        let err =
            start_from_work("to-home", "/opt/homebrew/bin/ccnm", "xshun", None, &env).unwrap_err();

        assert!(
            fake.calls()[0]
                .display()
                .contains("/opt/homebrew/bin/ccnm run xshun"),
            "the configured path is the one that runs: {}",
            fake.calls()[0].display()
        );
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("/opt/homebrew/bin/ccnm"), "{err}");
        assert!(err.message().contains("ccnm_bin"), "{err}");
    }

    /// A home that never answered and a home that answered "no" are two
    /// different problems with two different fixes, and the work machine
    /// only ever sees an exit code. Reporting a refusal as unreachable
    /// sends somebody to debug their network over a typo'd workspace
    /// name.
    #[test]
    fn a_home_that_refused_is_not_reported_as_a_home_that_was_not_there() {
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
        let err = start_from_work("to-home", "/opt/home/ccnm", "xshun", None, &env_for(&down))
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message().contains("Operation timed out"), "{err}");

        // Home was reached, looked, and said no. Its own words are
        // relayed; the error says whose failure it was and with what.
        let refused = FakeRunner::new();
        let mut no = Output::exited(ErrorCode::Config.exit_code(), "");
        no.stderr =
            b"CCNM_E_CONFIG:\nworkspace 'xshun' is not defined; defined: fixture\n".to_vec();
        refused.push(no);
        let err = start_from_work(
            "to-home",
            "/opt/home/ccnm",
            "xshun",
            None,
            &env_for(&refused),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message().contains("exited 10"), "{err}");
        assert!(err.message().contains("xshun"), "{err}");
    }

    /// Both roles refuse before the network when the project is not on
    /// this machine, because this machine is the one that would serve it.
    /// The check is in `work_ssh`, which every home-side command goes
    /// through, so it is worth a test that names all of them.
    #[test]
    fn every_home_side_command_checks_the_project_is_here_first() {
        let dir = temp("no-root");
        let config = Config::parse(&format!(
            "version = 1\n\
             [hosts.work]\nssh = \"to-work\"\n\
             [hosts.home]\nssh_from_work = \"to-home\"\n\
             [workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\n",
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
    /// session's own timeout *and* the work side's grace on top of it. A
    /// timeout shorter than the thing it is waiting for turns every long
    /// run into "the work machine is unreachable".
    #[test]
    fn the_print_call_outlives_the_session_it_is_waiting_for() {
        let dir = temp("print");
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let config = Config::parse(&format!(
            "version = 1\n\
             [hosts.work]\nssh = \"to-work\"\n\
             [hosts.home]\nssh_from_work = \"to-home\"\n\
             [workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\n",
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
        assert_eq!(req.home_alias, "to-home");
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

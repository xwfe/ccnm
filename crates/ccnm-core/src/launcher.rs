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
pub fn start_from_work(
    home_alias: &str,
    home_ccnm_bin: &str,
    workspace: &str,
    env: &Env<'_>,
) -> Result<()> {
    let ssh = Ssh::new(home_alias, env.control_dir.clone())?.with_ccnm_bin(home_ccnm_bin);
    let out = env.runner.run(&ssh.remote_cmd(
        Master::Reuse,
        &[ssh.ccnm_bin(), "run", workspace, "--detached"],
        Duration::from_secs(180),
    )?)?;
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
    //! Starting a session from the work machine: what it sends home, and
    //! what it makes of the three ways that can fail.

    use super::*;
    use crate::config::Config;
    use crate::error::ErrorCode;
    use crate::process::{FakeRunner, Output};

    /// ControlPath expands to at most 103 bytes and macOS `temp_dir()` is
    /// most of that on its own, so sockets go straight under /tmp.
    fn control(test: &str) -> PathBuf {
        PathBuf::from("/tmp/ccnm-lt").join(format!("{}-{test}", std::process::id()))
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
        start_from_work(alias, &host.ccnm_bin(), "xshun", &env).unwrap();

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
        let err = start_from_work("to-home", "/opt/homebrew/bin/ccnm", "xshun", &env).unwrap_err();

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
        let err =
            start_from_work("to-home", "/opt/home/ccnm", "xshun", &env_for(&down)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message().contains("Operation timed out"), "{err}");

        // Home was reached, looked, and said no. Its own words are
        // relayed; the error says whose failure it was and with what.
        let refused = FakeRunner::new();
        let mut no = Output::exited(ErrorCode::Config.exit_code(), "");
        no.stderr =
            b"CCNM_E_CONFIG:\nworkspace 'xshun' is not defined; defined: fixture\n".to_vec();
        refused.push(no);
        let err =
            start_from_work("to-home", "/opt/home/ccnm", "xshun", &env_for(&refused)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message().contains("exited 10"), "{err}");
        assert!(err.message().contains("xshun"), "{err}");
    }

}

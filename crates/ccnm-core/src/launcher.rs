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
use crate::protocol::run::{RunReport, RunRequest};
use crate::ssh::{Master, Ssh};

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

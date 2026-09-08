//! Runs on Agent Node, before OpenSSH. The wire payload contains Runtime data only.
use crate::error::{Error, Result};
use crate::process::Cmd;
use crate::protocol::payload::{self, Protocol};
use crate::session::{self, Dir, Spec};
use crate::ssh::Ssh;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub session_dir: PathBuf,
}
impl Protocol for Request {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        2
    }
}

pub fn launcher(dir: &Dir, exe: &std::path::Path) -> Result<Cmd> {
    let request = Request {
        protocol: 2,
        session_dir: dir.path().to_path_buf(),
    };
    Ok(Cmd::new(exe)
        .args(["internal", "agent-transport", "--payload"])
        .arg(payload::encode(&request)?))
}

pub fn command(spec: &Spec) -> Result<Cmd> {
    let runtime = spec
        .runtime
        .as_ref()
        .ok_or_else(|| Error::invalid_args("Agent transport needs a remote Runtime"))?;
    let ssh = Ssh::new(&runtime.alias, "/unused")?
        .with_ccnm_bin(&runtime.ccnm_bin)
        .for_provider(spec.provider());
    let serve =
        crate::protocol::mcp::ServePayload::new(&spec.workspace, spec.root.clone(), &spec.id)
            .with_interactive(spec.mode.is_interactive())
            .with_provider(spec.provider());
    let mut cmd = ssh.mcp_transport_cmd(&payload::encode(&serve)?)?;
    // Claude's previous MCP JSON and measured Codex transport both pinned the
    // system OpenSSH; do not accidentally replace that with a PATH lookup.
    cmd.program = session::SSH_BIN.into();
    Ok(cmd)
}

pub fn exec(request: &Request) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let spec = session::load(&Dir::at(&request.session_dir))?;
    let cmd = command(&spec)?;
    let mut process = cmd.process();
    Err(Error::internal("cannot exec Agent-side SSH transport").with_source(process.exec()))
}

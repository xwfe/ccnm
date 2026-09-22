//! Runs on Agent Node, before OpenSSH. The wire payload contains Runtime data only.
use crate::error::{Error, Result};
use crate::process::Cmd;
use crate::protocol::payload::{self, Protocol};
use crate::session::{self, Dir, Spec};
use crate::ssh::Ssh;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<crate::instance::AgentIdentity>,
    pub session_dir: PathBuf,
}
impl Protocol for Request {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        if self.identity.is_some() { 3 } else { 2 }
    }
}

pub fn launcher(dir: &Dir, exe: &std::path::Path) -> Result<Cmd> {
    launcher_for(dir, exe, None)
}

pub fn launcher_for(
    dir: &Dir,
    exe: &std::path::Path,
    identity: Option<&crate::instance::AgentIdentity>,
) -> Result<Cmd> {
    launcher_named(dir, exe, identity, "agent-transport")
}

/// What Codex is told to spawn for an exec-server session (P23): the same
/// record as the MCP transport's, because it is the same question -- which
/// session -- and where that leads is the session's to say, not the
/// caller's. A different verb, so a build that reached the wrong one fails
/// on the name rather than halfway into the other protocol.
pub fn native_launcher_for(
    dir: &Dir,
    exe: &std::path::Path,
    identity: Option<&crate::instance::AgentIdentity>,
) -> Result<Cmd> {
    launcher_named(dir, exe, identity, "exec-transport")
}

fn launcher_named(
    dir: &Dir,
    exe: &std::path::Path,
    identity: Option<&crate::instance::AgentIdentity>,
    internal: &str,
) -> Result<Cmd> {
    let request = Request {
        protocol: if identity.is_some() { 3 } else { 2 },
        identity: identity.cloned(),
        session_dir: dir.path().to_path_buf(),
    };
    Ok(Cmd::new(exe)
        .args(["internal", internal, "--payload"])
        .arg(payload::encode(&request)?))
}

/// The one connection this session's tools ride: MCP for most sessions,
/// exec-server JSON-RPC for one on the chain. Same ssh either way.
pub fn command(spec: &Spec) -> Result<Cmd> {
    spec.validate_identity()?;
    if spec.codex_exec_server {
        return native_command(spec);
    }
    let runtime = remote(spec)?;
    let ssh = Ssh::new(&runtime.alias, "/unused")?
        .with_ccnm_bin(&runtime.ccnm_bin)
        .for_provider(spec.provider());
    // A bound session asks the Runtime to open the workspace and lets it
    // find the project itself (internal wire protocol 4). The root this
    // machine holds is a record of what the Runtime said, not an argument
    // it may send back: whoever names the directory names what every tool
    // call, the write guard and the safety verdict apply to.
    //
    // Legacy `agent_node` workspaces keep the old shape, which carries the
    // root. That is the migration boundary -- they have no identity to
    // resolve with, and they are on their way out either way.
    let wire = match spec.agent_identity.clone() {
        Some(identity) => {
            spec.workspace_binding()?;
            payload::encode(
                &crate::runtime::OpenPayload::new(&spec.workspace, identity, &spec.id)
                    .with_interactive(spec.mode.is_interactive()),
            )?
        }
        None => payload::encode(
            &crate::protocol::mcp::ServePayload::new(&spec.workspace, spec.root.clone(), &spec.id)
                .with_interactive(spec.mode.is_interactive())
                .with_provider(spec.provider()),
        )?,
    };
    let mut cmd = ssh.mcp_transport_cmd(&wire)?;
    // Claude's previous MCP JSON and measured Codex transport both pinned the
    // system OpenSSH; do not accidentally replace that with a PATH lookup.
    cmd.program = session::SSH_BIN.into();
    Ok(cmd)
}

/// The ssh an exec-server session rides (P23): the MCP transport's
/// connection, carrying `ccnm internal exec-serve` for the workspace this
/// session is bound to. The far side opens by workspace name and bound
/// identity (wire protocol 6) and decides root and binary itself; nothing
/// this machine holds crosses.
pub fn native_command(spec: &Spec) -> Result<Cmd> {
    spec.validate_identity()?;
    if !spec.codex_exec_server {
        return Err(Error::invalid_args(
            "this session's tools are served over MCP, not exec-server",
        ));
    }
    // validate_identity has already required an identity for the chain.
    let identity = spec
        .agent_identity
        .clone()
        .ok_or_else(|| Error::invalid_args("exec-server session has no bound Agent identity"))?;
    spec.workspace_binding()?;
    let runtime = remote(spec)?;
    let ssh = Ssh::new(&runtime.alias, "/unused")?
        .with_ccnm_bin(&runtime.ccnm_bin)
        .for_provider(spec.provider());
    let wire = payload::encode(&crate::runtime::NativeOpenPayload::new(
        &spec.workspace,
        identity,
        &spec.id,
    ))?;
    let mut cmd = ssh.exec_transport_cmd(&wire)?;
    cmd.program = session::SSH_BIN.into();
    Ok(cmd)
}

fn remote(spec: &Spec) -> Result<&session::RuntimeLink> {
    spec.runtime
        .as_ref()
        .ok_or_else(|| Error::invalid_args("Agent transport needs a remote Runtime"))
}

pub fn exec(request: &Request) -> Result<()> {
    let spec = load_for(request)?;
    if spec.codex_exec_server {
        return Err(Error::invalid_args(
            "this session's tools ride exec-transport, not agent-transport",
        ));
    }
    exec_ssh(command(&spec)?)
}

/// `ccnm internal exec-transport`: what Codex spawns from the session's
/// `environments.toml`. Becomes the ssh to the Runtime's `exec-serve`;
/// Codex holds both ends of the pipe from then on.
pub fn exec_native(request: &Request) -> Result<()> {
    let spec = load_for(request)?;
    exec_ssh(native_command(&spec)?)
}

fn load_for(request: &Request) -> Result<Spec> {
    let spec = session::load(&Dir::at(&request.session_dir))?;
    if spec.agent_identity != request.identity {
        return Err(Error::invalid_args(
            "Agent transport identity does not match session",
        ));
    }
    Ok(spec)
}

fn exec_ssh(cmd: Cmd) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut process = cmd.process();
    Err(Error::internal("cannot exec Agent-side SSH transport").with_source(process.exec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PermissionMode;
    use crate::instance::AgentIdentity;
    use crate::provider::AgentProvider;
    use crate::session::{Mode, RuntimeLink, Spec};
    use std::path::PathBuf;

    const ROOT: &str = "/Users/bing/ccnm-fixture";
    const ID: &str = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";

    fn spec(identity: Option<AgentIdentity>) -> Spec {
        Spec {
            protocol: if identity.is_some() {
                crate::instance::INSTANCE_SESSION_PROTOCOL
            } else {
                crate::protocol::payload::PROTOCOL
            },
            runtime_node: identity.as_ref().map(|_| "runtime".to_string()),
            provider: identity
                .as_ref()
                .map_or_else(AgentProvider::default, |i| i.provider),
            agent_identity: identity,
            id: ID.into(),
            workspace: "fixture".into(),
            root: PathBuf::from(ROOT),
            runtime: Some(RuntimeLink {
                alias: "runtime-alias".into(),
                ccnm_bin: "~/.local/bin/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: PermissionMode::default(),
            mode: Mode::Interactive { prompt: None },
            timeout_secs: 600,
            cwd: PathBuf::from("/Users/fodelf/.local/state/ccnm/workspaces/fixture"),
            codex_exec_server: false,
            agent_tools: Default::default(),
        }
    }

    fn identity() -> AgentIdentity {
        AgentIdentity {
            node: "agent".into(),
            instance: "claude-main".into(),
            provider: AgentProvider::Claude,
            profile_ref: "default".into(),
        }
    }

    fn codex() -> AgentIdentity {
        AgentIdentity {
            node: "agent".into(),
            instance: "codex-main".into(),
            provider: AgentProvider::Codex,
            profile_ref: "default".into(),
        }
    }

    fn native_spec() -> Spec {
        let mut spec = spec(Some(codex()));
        spec.provider = AgentProvider::Codex;
        spec.codex_exec_server = true;
        spec
    }

    fn wire(cmd: &Cmd) -> String {
        cmd.args.last().unwrap().to_string_lossy().into_owned()
    }

    /// A bound session names the workspace and who is asking. Where the
    /// project is stays the Runtime's answer: this side records a root but
    /// does not get to send one back, so nothing the Agent holds can decide
    /// what the tools, the write guard and the safety verdict apply to.
    #[test]
    fn a_bound_session_asks_the_runtime_to_open_the_workspace() {
        let spec = spec(Some(identity()));
        let cmd = command(&spec).unwrap();
        let sent: crate::runtime::OpenPayload = payload::decode(&wire(&cmd)).unwrap();
        assert_eq!(sent.protocol, crate::runtime::OPEN_PROTOCOL);
        assert_eq!(sent.workspace, "fixture");
        assert_eq!(sent.session, ID);
        assert_eq!(sent.agent, identity());
        assert!(sent.interactive, "somebody is at this terminal");
        // Not in the payload, and not anywhere else on the command line.
        for arg in &cmd.args {
            assert!(
                !arg.to_string_lossy().contains(ROOT),
                "the root must not cross: {arg:?}"
            );
        }
    }

    /// The migration boundary. A legacy `agent_node` workspace has no
    /// identity to resolve with, so it keeps the old shape -- root and all
    /// -- until it is retired.
    #[test]
    fn a_legacy_session_still_carries_its_own_root() {
        let cmd = command(&spec(None)).unwrap();
        let sent: crate::protocol::mcp::ServePayload = payload::decode(&wire(&cmd)).unwrap();
        assert_eq!(sent.root, PathBuf::from(ROOT));
        assert_eq!(sent.session, ID);
        assert!(sent.binding.is_none());
    }

    /// An exec-server session rides the identical ssh; what it carries is
    /// the protocol-6 open, which names the workspace and the bound identity
    /// and nothing this machine holds -- no root, no binary.
    #[test]
    fn an_exec_server_session_rides_the_same_ssh_to_exec_serve() {
        let native = native_spec();
        let cmd = native_command(&native).unwrap();
        assert_eq!(cmd.program, session::SSH_BIN);
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a == "exec-serve"), "{args:?}");
        assert!(!args.iter().any(|a| a == "mcp-serve"), "{args:?}");
        assert!(args.iter().any(|a| a == "ForwardAgent=no"), "{args:?}");
        let sent: crate::runtime::NativeOpenPayload = payload::decode(&wire(&cmd)).unwrap();
        assert_eq!(sent.protocol, crate::runtime::NATIVE_PROTOCOL);
        assert_eq!(sent.workspace, "fixture");
        assert_eq!(sent.session, ID);
        assert_eq!(sent.agent, codex());
        for arg in &args {
            assert!(!arg.contains(ROOT), "the root must not cross: {arg:?}");
        }
        // `command` is what status and the launcher ask; for a session on
        // the chain it answers with this same connection.
        assert_eq!(command(&native).unwrap(), cmd);
        // And a session that is not on the chain cannot be handed it.
        let err = native_command(&spec(Some(codex()))).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidArgs);
    }

    /// The record refuses to describe a session the chain cannot run:
    /// Codex's tools, opened by bound identity, in front of a person.
    #[test]
    fn the_chain_needs_a_bound_interactive_codex() {
        assert!(native_spec().validate_identity().is_ok());
        let mut claude = native_spec();
        claude.provider = AgentProvider::Claude;
        claude.agent_identity = Some(identity());
        assert!(claude.validate_identity().is_err());
        let mut print = native_spec();
        print.mode = Mode::Print {
            prompt: "fix it".into(),
        };
        assert!(print.validate_identity().is_err());
        let mut legacy = native_spec();
        legacy.agent_identity = None;
        legacy.runtime_node = None;
        legacy.protocol = 2;
        assert!(legacy.validate_identity().is_err());
    }

    /// The print half of the same decision: nobody is there to answer a
    /// permission prompt, and the far side has to be told so.
    #[test]
    fn a_print_session_says_nobody_is_at_the_terminal() {
        let mut spec = spec(Some(identity()));
        spec.mode = Mode::Print {
            prompt: "fix the failing test".into(),
        };
        let cmd = command(&spec).unwrap();
        let sent: crate::runtime::OpenPayload = payload::decode(&wire(&cmd)).unwrap();
        assert!(!sent.interactive);
    }
}

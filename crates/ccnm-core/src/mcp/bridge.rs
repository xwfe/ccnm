//! `ccnm mcp bridge <workspace>`: the client-machine half of the Remote
//! Workspace MCP entry (docs/protocol/remote-workspace-mcp-v1.md).
//!
//! It builds one command and then gets out of the way. An external MCP Host
//! starts this process, and what it talks to is the real server on the
//! Runtime Node — the same `internal mcp-serve`, the same seven tools, the
//! same path policy and the same write guard the managed path uses. Nothing
//! here reimplements a tool, and nothing here is an MCP server.
//!
//! The bridge holds no opinion about the project. It does not know the
//! root, cannot send one, and has no workspace list of its own: it names a
//! workspace and a mode, and the Runtime decides whether that workspace is
//! open to external clients at all.
//!
//! **There is no child process.** The CLI `exec`s the command this module
//! builds, so the bridge process *becomes* the ssh: stdin and stdout are
//! the MCP stream end to end, EOF and signals reach the transport directly,
//! and there is no supervisor left to leak an orphan if it dies.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::process::Cmd;
use crate::protocol::payload;
use crate::runtime::{ExternalMode, ExternalOpenPayload};
use crate::ssh::Ssh;

/// What the Host asked for on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub workspace: String,
    /// A node in *this* machine's config. Optional when the config leaves
    /// no room for doubt.
    pub node: Option<String>,
    pub mode: ExternalMode,
}

/// The node this bridge will dial.
///
/// Named explicitly, or inferred when there is exactly one candidate. It is
/// never guessed from more than one: a bridge that picks a machine for you
/// is a bridge that one day opens the wrong project.
pub fn node<'a>(
    config: &'a Config,
    request: &Request,
) -> Result<(&'a str, &'a crate::config::Node)> {
    if let Some(asked) = &request.node {
        let (name, node) = config.nodes.get_key_value(asked).ok_or_else(|| {
            Error::config(format!("no node named {asked} in this machine's config"))
        })?;
        if node.ssh.is_none() {
            return Err(Error::config(format!(
                "node {name} has no ssh alias, so this machine cannot open a transport to it"
            )));
        }
        return Ok((name.as_str(), node));
    }
    let mut dialable = config
        .nodes
        .iter()
        .filter(|(name, node)| node.ssh.is_some() && config.this.as_deref() != Some(name.as_str()));
    match (dialable.next(), dialable.next()) {
        (Some((name, node)), None) => Ok((name.as_str(), node)),
        (None, _) => Err(Error::config(
            "this machine's config has no node with an ssh alias to open a transport to",
        )),
        (Some(_), Some(_)) => Err(Error::config(
            "this machine's config has more than one node with an ssh alias; name one with --node",
        )),
    }
}

/// The one command an external MCP Host runs, ready to `exec`.
///
/// `session` names the retained-output directory on the Runtime side. It is
/// this connection's own: an `output_ref` never means anything outside it.
pub fn command(config: &Config, request: &Request, session: &str) -> Result<Cmd> {
    let (name, node) = node(config, request)?;
    let alias = node
        .ssh
        .as_deref()
        .ok_or_else(|| Error::config(format!("node {name} has no ssh alias")))?;
    let ccnm_bin = node
        .ccnm_bin
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::config::DEFAULT_CCNM_BIN.to_string());
    let wire = payload::encode(&ExternalOpenPayload::new(
        &request.workspace,
        session,
        request.mode,
    ))?;
    let mut cmd = Ssh::new(alias, "/unused")?
        .with_ccnm_bin(&ccnm_bin)
        .mcp_transport_cmd(&wire)?;
    // The measured system OpenSSH, like every other ccnm transport. A PATH
    // lookup here would let a different ssh answer for the same alias.
    cmd.program = crate::session::SSH_BIN.into();
    Ok(cmd)
}

/// A session id for one bridge connection.
pub fn session_id() -> String {
    format!("bridge-{}", uuid::Uuid::new_v4().hyphenated())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn config(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn request(node: Option<&str>, mode: ExternalMode) -> Request {
        Request {
            workspace: "demo".into(),
            node: node.map(str::to_string),
            mode,
        }
    }

    const ONE_NODE: &str = r#"
this = "laptop"
[nodes.laptop]
[nodes.runtime]
ssh = "runtime-alias"
"#;

    /// What crosses the wire is a workspace name, a session id and a mode.
    /// No root, no host, no key: the Runtime owns every one of those, and a
    /// payload that could carry them would make the bridge the authority.
    #[test]
    fn the_payload_names_a_workspace_and_nothing_about_the_machine() {
        let cmd = command(
            &config(ONE_NODE),
            &request(None, ExternalMode::Read),
            "bridge-1",
        )
        .unwrap();
        let wire = cmd.args.last().unwrap().to_string_lossy().into_owned();
        let sent: ExternalOpenPayload = payload::decode(&wire).unwrap();
        assert_eq!(sent.protocol, crate::runtime::EXTERNAL_PROTOCOL);
        assert_eq!(sent.workspace, "demo");
        assert_eq!(sent.session, "bridge-1");
        assert_eq!(sent.mode, ExternalMode::Read);
    }

    /// One ssh, running the same `internal mcp-serve` the managed path runs.
    #[test]
    fn the_command_is_one_ssh_into_the_runtime_server() {
        let cmd = command(
            &config(ONE_NODE),
            &request(None, ExternalMode::Coding),
            "bridge-2",
        )
        .unwrap();
        assert_eq!(
            cmd.program,
            std::path::PathBuf::from(crate::session::SSH_BIN)
        );
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"runtime-alias".to_string()), "{args:?}");
        assert!(args.contains(&"mcp-serve".to_string()), "{args:?}");
        assert_eq!(args.iter().filter(|a| *a == "-T").count(), 1);
    }

    #[test]
    fn a_named_node_wins_and_an_unknown_one_is_refused() {
        let config = config(
            r#"
this = "laptop"
[nodes.laptop]
[nodes.a]
ssh = "alias-a"
[nodes.b]
ssh = "alias-b"
"#,
        );
        let cmd = command(&config, &request(Some("b"), ExternalMode::Read), "s").unwrap();
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"alias-b".to_string()), "{args:?}");
        let err = command(&config, &request(Some("nope"), ExternalMode::Read), "s").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
    }

    /// Two candidates and no `--node` is a question, not a default.
    #[test]
    fn an_ambiguous_config_asks_rather_than_picks() {
        let config = config(
            r#"
this = "laptop"
[nodes.laptop]
[nodes.a]
ssh = "alias-a"
[nodes.b]
ssh = "alias-b"
"#,
        );
        let err = command(&config, &request(None, ExternalMode::Read), "s").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(format!("{err}").contains("--node"), "{err}");
    }

    #[test]
    fn a_node_without_an_alias_cannot_be_dialled() {
        let config = config(
            r#"
this = "runtime"
[nodes.runtime]
runtime_user = "ccrun"
"#,
        );
        let err = command(&config, &request(Some("runtime"), ExternalMode::Read), "s").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
    }

    /// What the transport must never bring along. A bridge runs on the
    /// client's machine, where an ssh agent and a user's own ssh config
    /// usually are; none of that may ride into the Runtime with it.
    #[test]
    fn the_transport_forwards_nothing_and_shares_no_connection() {
        let cmd = command(
            &config(ONE_NODE),
            &request(None, ExternalMode::Read),
            "bridge-hygiene",
        )
        .unwrap();
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for option in [
            // No LocalForward from the user's own config comes along.
            "ClearAllForwardings=yes",
            // Not shared with, and not killed by, any other ssh this
            // machine happens to be running.
            "ControlMaster=no",
            "ControlPath=none",
            // No password prompt in a process a Host started.
            "BatchMode=yes",
            // The environment does not travel.
            "SendEnv=-*",
        ] {
            assert!(
                args.contains(&option.to_string()),
                "{option} missing: {args:?}"
            );
        }
        // And no agent forwarding was asked for anywhere.
        assert!(
            !args.iter().any(|a| a.contains("ForwardAgent=yes")),
            "{args:?}"
        );
    }

    #[test]
    fn a_generated_session_id_is_a_valid_identifier() {
        crate::instance::identifier(&session_id()).unwrap();
    }
}

//! Boundary-specific environment rules. Inspect names, never log values.
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};

use crate::error::{Error, Result};
use crate::process::Cmd;
use crate::provider::AgentProvider;

pub fn agent_private(name: &OsStr) -> bool {
    AgentProvider::ALL
        .iter()
        .any(|p| p.credentials().is_environment_name(name))
}

/// Conservative unknown-credential classification, not a secret scanner.
/// Do not classify entire vendor namespaces such as GOOGLE_* as secrets.
pub fn authentication(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    matches!(upper.as_str(), "TOKEN" | "PASSWORD" | "SECRET" | "API_KEY")
        || [
            "_TOKEN",
            "_PASSWORD",
            "_SECRET",
            "_API_KEY",
            "_ACCESS_KEY",
            "_ACCESS_KEY_ID",
            "_SECRET_ACCESS_KEY",
            "_CREDENTIALS",
        ]
        .iter()
        .any(|suffix| upper.ends_with(suffix))
}

pub fn strip_names(names: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    names
        .into_iter()
        .filter(|name| agent_private(name) || authentication(name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Used both by the Agent-local official CLI adapter and before SSH exec.
/// SSH_AUTH_SOCK is local SSH authentication, not permission to forward it.
pub fn without_agent_auth(mut cmd: Cmd) -> Cmd {
    let mut names = strip_names(std::env::vars_os().map(|(k, _)| k));
    // Explicit entries are also stripped; otherwise env() would restore them
    // after env_remove() in the process runner.
    names.extend(strip_names(cmd.env.iter().map(|(k, _)| k.clone())));
    names.extend(
        [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]
        .into_iter()
        .map(OsString::from),
    );
    let names: BTreeSet<_> = names.into_iter().collect();
    cmd.env.retain(|(k, _)| !names.contains(k));
    for name in names {
        if !cmd.env_remove.contains(&name) {
            cmd = cmd.env_remove(name);
        }
    }
    cmd
}

pub fn validate_runtime_names(names: impl IntoIterator<Item = OsString>) -> Result<()> {
    if names
        .into_iter()
        .any(|name| authentication(&name) || name == "SSH_AUTH_SOCK")
    {
        // Even a name can contain injected secrets: keep diagnostics generic.
        return Err(Error::policy(
            "Runtime authentication environment has no project authorization; exec refused before spawn (values withheld)",
        ));
    }
    Ok(())
}

/// Child environment defense after Runtime authorization. This low-level
/// constructor does not grant permission to spawn; runtime_gate does that.
pub fn runtime_child(cmd: Cmd) -> Cmd {
    without_agent_auth(cmd).env_remove("SSH_AUTH_SOCK")
}

//! Official Codex 0.153.4 only. No credential content is read or copied.
use super::{AgentReport, Ask, AuthStatus};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output, ProcessRunner};
use crate::session::{Dir, Mode, Spec};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod context;
pub mod result;
pub use crate::session::transport;

pub const CREDENTIALS: super::CredentialMetadata = super::CredentialMetadata {
    env_prefixes: &["CODEX_", "OPENAI_"],
    config_env: "CODEX_HOME",
    default_directory: ".config/ccnm/agents/codex",
    additional_directories: &[".codex"],
    xdg_config_directory: Some("ccnm/agents/codex"),
    containers: &[],
    files: &["auth.json"],
    agent_name: "Codex",
    vendor_name: "OpenAI",
    login_command: "codex login",
    egress_host: "api.openai.com",
};
pub const VERSION: &str = "0.153.4";
const DISABLED: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "apps",
    "plugins",
    "hooks",
    "multi_agent",
    "multi_agent_v2",
    "browser_use",
    "computer_use",
    "image_generation",
    "memories",
    "workspace_dependencies",
    "skill_search",
    "shell_snapshot",
    "goals",
    "tool_suggest",
];

pub fn locate(path: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = path {
        candidates.extend(std::env::split_paths(path).map(|p| p.join("codex")));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/codex"));
        candidates.push(home.join(".cargo/bin/codex"));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ]);
    candidates
        .into_iter()
        .find(|p| crate::process::is_executable(p))
}

/// Deliberately not CCNM_CONFIG or inherited CODEX_HOME: those can identify a
/// Runtime config or the user's personal Agent setup. Resolve only on Agent.
pub fn home() -> Result<PathBuf> {
    Ok(crate::paths::config_path()?
        .parent()
        .ok_or_else(|| Error::internal("missing config parent"))?
        .join("agents/codex"))
}

pub fn validate_home(path: &Path) -> Result<()> {
    crate::safety::credentials::private_home(path, CREDENTIALS.files)
}

fn isolated(cmd: Cmd, home: &Path) -> Cmd {
    crate::safety::environment::without_agent_auth(cmd).env("CODEX_HOME", home)
}

pub fn parse_version(out: &Output) -> Result<String> {
    if !out.success() {
        return Err(Error::new(ErrorCode::Version, "codex --version failed"));
    }
    let text = out.stdout_lossy();
    let version = text
        .trim()
        .strip_prefix("codex-cli ")
        .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
        .ok_or_else(|| Error::new(ErrorCode::Version, "unexpected codex --version output"))?;
    if version != VERSION {
        return Err(Error::new(
            ErrorCode::Version,
            format!("Codex {version} has not been measured; this adapter requires {VERSION}"),
        ));
    }
    Ok(version.to_string())
}

pub fn parse_auth(out: &Output) -> Result<AuthStatus> {
    if out.timed_out {
        return Err(Error::new(ErrorCode::Auth, "codex login status timed out"));
    }
    let text = out.stderr_lossy();
    let text = text.trim();
    let method = if out.exit_code == Some(0) && text == "Logged in using ChatGPT" {
        Some("ChatGPT")
    } else if out.exit_code == Some(0) && text.starts_with("Logged in using an API key") {
        Some("API key")
    } else if out.exit_code == Some(1) && text == "Not logged in" {
        None
    } else {
        return Err(Error::new(
            ErrorCode::Auth,
            "unexpected codex login status; no authentication details retained",
        ));
    };
    Ok(AuthStatus {
        logged_in: method.is_some(),
        auth_method: method.map(str::to_owned),
        email: None,
        subscription_type: None,
    })
}

pub fn report(bin: Option<&Path>, runner: &dyn ProcessRunner, ask: Ask) -> AgentReport {
    let Some(bin) = bin else {
        let err = Error::new(
            ErrorCode::Version,
            "codex not found in the Agent Node environment",
        );
        return AgentReport {
            path: None,
            version: Err((&err).into()),
            auth: Err(err.into()),
        };
    };
    let version = runner
        .run(
            &Cmd::new(bin)
                .arg("--version")
                .timeout(Duration::from_secs(20)),
        )
        .and_then(|out| parse_version(&out))
        .map_err(Into::into);
    let auth = match ask {
        Ask::VersionOnly => Err(Error::new(
            ErrorCode::NotReady,
            "Codex login not checked outside the controller login session",
        )),
        Ask::Everything => home().and_then(|home| {
            validate_home(&home)?;
            runner
                .run(&isolated(
                    Cmd::new(bin)
                        .args(["login", "status"])
                        .timeout(Duration::from_secs(20)),
                    &home,
                ))
                .and_then(|out| parse_auth(&out))
        }),
    }
    .map_err(Into::into);
    AgentReport {
        path: Some(bin.to_path_buf()),
        version,
        auth,
    }
}

pub fn validate_spec(spec: &Spec) -> Result<()> {
    if spec.provider() != super::AgentProvider::Codex || spec.protocol != 2 {
        return Err(Error::new(
            ErrorCode::Version,
            "Codex sessions require protocol 2",
        ));
    }
    if spec.provider_config_dir.is_some() {
        return Err(Error::invalid_args(
            "Codex home is Agent-local; a caller must not supply claude_config_dir",
        ));
    }
    if spec.permission_mode != super::PermissionMode::default() {
        return Err(Error::invalid_args(
            "Claude permission modes cannot configure Codex",
        ));
    }
    if spec.runtime.is_none() {
        return Err(Error::new(
            ErrorCode::NotReady,
            "Codex colocated mode has not been measured; use the SSH MCP topology",
        ));
    }
    let home = home()?;
    if spec.cwd.starts_with(&home) {
        return Err(Error::invalid_args(
            "Codex workspace state cannot live in its private authentication directory",
        ));
    }
    validate_home(&home)
}

pub fn launch_cmd(bin: &Path, spec: &Spec, dir: &Dir) -> Result<Cmd> {
    validate_spec(spec)?;
    build_launch_cmd(bin, spec, dir, &home()?, &std::env::current_exe()?)
}

pub(crate) fn build_launch_cmd(
    bin: &Path,
    spec: &Spec,
    dir: &Dir,
    agent_home: &Path,
    exe: &Path,
) -> Result<Cmd> {
    let mut cmd = isolated(Cmd::new(bin), agent_home)
        .cwd(&spec.cwd)
        .timeout(Duration::from_secs(spec.timeout_secs));
    if !spec.mode.is_interactive() {
        cmd = cmd.args([
            "exec",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "--ephemeral",
            "--json",
            "--color",
            "never",
        ]);
    } else {
        cmd = cmd.arg("--no-alt-screen");
    }
    cmd = cmd.args([
        "--sandbox",
        "read-only",
        "-c",
        if spec.mode.is_interactive() {
            "approval_policy=\"on-request\""
        } else {
            "approval_policy=\"never\""
        },
        "-c",
        "web_search=\"disabled\"",
        "--enable",
        "code_mode_only",
        "-c",
        "agents.enabled=false",
        "-c",
        "features.code_mode.excluded_tool_namespaces=[\"functions\",\"collaboration\"]",
    ]);
    for feature in DISABLED {
        cmd = cmd.args(["--disable", feature]);
    }
    let transport = transport::launcher(dir, exe)?;
    let args: Vec<_> = transport
        .args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    // TOML quoted strings/arrays are JSON-compatible for these argv values.
    cmd = cmd
        .arg("-c")
        .arg(format!(
            "mcp_servers.ccnm.command={}",
            serde_json::json!(exe.to_string_lossy())
        ))
        .arg("-c")
        .arg(format!("mcp_servers.ccnm.args={}", serde_json::json!(args)))
        .args([
            "-c",
            "mcp_servers.ccnm.required=true",
            "-c",
            "mcp_servers.ccnm.default_tools_approval_mode=\"approve\"",
        ])
        .arg("-c")
        .arg(format!(
            "mcp_servers.ccnm.enabled_tools={}",
            serde_json::json!(crate::session::MCP_TOOLS)
        ));
    match &spec.mode {
        Mode::Print { prompt } => cmd = cmd.arg("-").stdin(prompt.as_bytes().to_vec()),
        Mode::Interactive {
            prompt: Some(prompt),
        } => cmd = cmd.arg("--").arg(prompt),
        Mode::Interactive { prompt: None } => {}
    }
    Ok(cmd)
}

pub fn check_inventory(bin: &Path, spec: &Spec, runner: &dyn ProcessRunner) -> Result<()> {
    let out = runner.run(&isolated(
        Cmd::new(bin)
            .args(["mcp", "list", "--json"])
            .cwd(&spec.cwd)
            .timeout(Duration::from_secs(20)),
        &home()?,
    ))?;
    let empty = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty));
    if !out.success() || !empty {
        return Err(Error::new(
            ErrorCode::NotReady,
            "dedicated Codex home must have no configured MCP servers; ccnm injects its own per session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

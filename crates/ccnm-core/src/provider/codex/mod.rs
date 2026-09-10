//! Official Codex 0.154.0 only. No credential content is read or copied.
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
pub const VERSION: &str = "0.154.0";
const DISABLED: &[&str] = &[
    "shell_tool",
    "unified_exec",
    // 0.154.0 新增，stable 且默认开启。它和 unified_exec 是同一类东西——
    // 一条不经 ccnm 七工具的执行路径——所以同样关掉。名字不一样，所以旧的
    // 列表拦不住它：这正是版本 pin 存在的理由。
    "unified_exec_tty",
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

/// Models measured to advertise Code Mode under the pinned CLI version.
///
/// `code_mode_only` is an under-development Codex feature, and a model is
/// allowed to refuse it. `gpt-5.3-codex-spark` does: Codex prints "model
/// `…` does not advertise Code Mode support" at startup, and in a measured
/// run the model then made one wrongly-shaped tool call, was refused by
/// name, and gave up — see `docs/research/p7-codex-parity-2026-09-10.md`.
///
/// **Nothing answers this before launch.** `codex doctor --json` reports
/// which feature flags are on, not whether the model supports them, and the
/// mismatch only surfaces as a warning item once the turn has started. So
/// this is a measured table, exactly like [`VERSION`], and it is empty on
/// purpose: every fixture under `tests/fixtures/` was captured **without**
/// `--model`, i.e. on the CLI default model, which is the one entry
/// [`code_mode`] treats as measured.
const CODE_MODE_MODELS: &[&str] = &[];

/// Whether this launch turns Code Mode on.
///
/// Naming no model means the CLI default, which every measured fixture was
/// captured with. Naming one ccnm has not measured with Code Mode means it
/// launches without it: forcing an under-development feature onto a model
/// that says it does not support it is how the parity run above failed.
///
/// The trade is real and documented in the support matrix: with Code Mode
/// on, `features.code_mode.excluded_tool_namespaces` was measured to remove
/// Codex's own `apply_patch` from the registry the model sees; with it off,
/// what keeps native tools from touching the Agent's disk is the read-only
/// sandbox and the disabled feature list, which is a weaker guarantee.
pub(crate) fn code_mode(model: Option<&str>) -> bool {
    model.is_none_or(|model| CODE_MODE_MODELS.contains(&model))
}

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
    crate::paths::codex_home()
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
    report_at(bin, None, runner, ask)
}

pub fn report_at(
    bin: Option<&Path>,
    profile_dir: Option<&Path>,
    runner: &dyn ProcessRunner,
    ask: Ask,
) -> AgentReport {
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
        Ask::Everything => profile_dir
            .map(Path::to_path_buf)
            .map_or_else(home, Ok)
            .and_then(|home| {
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
    if spec.provider() != super::AgentProvider::Codex
        || !matches!(
            spec.protocol,
            2 | crate::instance::INSTANCE_SESSION_PROTOCOL
        )
        || (spec.protocol == 2) != spec.agent_identity.is_none()
    {
        return Err(Error::new(
            ErrorCode::Version,
            "Codex legacy sessions require protocol 2; bound instance sessions require protocol 3",
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
    Ok(())
}

pub fn launch_cmd(bin: &Path, spec: &Spec, dir: &Dir) -> Result<Cmd> {
    launch_cmd_at(bin, spec, dir, None, None)
}

pub fn launch_cmd_at(
    bin: &Path,
    spec: &Spec,
    dir: &Dir,
    profile_dir: Option<&Path>,
    model: Option<&str>,
) -> Result<Cmd> {
    validate_spec(spec)?;
    let home = profile_dir.map(Path::to_path_buf).map_or_else(home, Ok)?;
    validate_home(&home)?;
    if spec.cwd.starts_with(&home) {
        return Err(Error::invalid_args(
            "Codex workspace state cannot live in its private authentication directory",
        ));
    }
    build_launch_cmd(bin, spec, dir, &home, &std::env::current_exe()?, model)
}

pub(crate) fn build_launch_cmd(
    bin: &Path,
    spec: &Spec,
    dir: &Dir,
    agent_home: &Path,
    exe: &Path,
    model: Option<&str>,
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
    // Only when the instance names one: without it the CLI picks its own
    // default, which is what every measured fixture was captured with.
    if let Some(model) = model {
        cmd = cmd.args(["--model", model]);
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
        // Not part of Code Mode: it keeps the model from spawning Codex's
        // own sub-agents whichever tool surface it is given.
        "-c",
        "agents.enabled=false",
    ]);
    if code_mode(model) {
        cmd = cmd.args([
            "--enable",
            "code_mode_only",
            "-c",
            "features.code_mode.excluded_tool_namespaces=[\"functions\",\"collaboration\"]",
        ]);
    }
    for feature in DISABLED {
        cmd = cmd.args(["--disable", feature]);
    }
    let transport = transport::launcher_for(dir, exe, spec.agent_identity.as_ref())?;
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
    check_inventory_at(bin, spec, None, runner)
}

pub fn check_inventory_at(
    bin: &Path,
    spec: &Spec,
    profile_dir: Option<&Path>,
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let home = profile_dir.map(Path::to_path_buf).map_or_else(home, Ok)?;
    validate_home(&home)?;
    let out = runner.run(&isolated(
        Cmd::new(bin)
            .args(["mcp", "list", "--json"])
            .cwd(&spec.cwd)
            .timeout(Duration::from_secs(20)),
        &home,
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

//! Asking the official Claude Code CLI about itself. ccnm only ever runs
//! `claude --version` and `claude auth status`; it never logs in (design
//! doc section 10).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use super::types::{
    AgentReport as ClaudeReport, Ask, AuthStatus, RunResult as PrintResult, Usage,
};
pub mod context;

pub const CREDENTIALS: super::CredentialMetadata = super::CredentialMetadata {
    env_prefixes: &["ANTHROPIC_", "CLAUDE_"],
    config_env: "CLAUDE_CONFIG_DIR",
    default_directory: ".claude",
    additional_directories: &[],
    xdg_config_directory: None,
    containers: &["Library/Keychains/login.keychain-db"],
    files: &[".credentials.json", "credentials.json"],
    agent_name: "Claude",
    vendor_name: "Anthropic",
    login_command: "claude auth login",
    egress_host: "api.anthropic.com",
};
mod policy;
pub(crate) use policy::write_session_files;
pub use policy::{NATIVE_TOOLS_DENIED, mcp_config, settings, transport_payload};

use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output, ProcessRunner};
use crate::session::{Dir, Mode, Spec};

/// Values accepted by `claude --permission-mode`, checked against Claude
/// Code 2.1.260 `--help`. Serialized in Claude's own camelCase so the config
/// file, the session spec and the CLI flag all read the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    AcceptEdits,
    Auto,
    BypassPermissions,
    Manual,
    DontAsk,
    Plan,
}

impl PermissionMode {
    /// The exact string to pass after `--permission-mode`.
    pub fn as_cli_value(self) -> &'static str {
        match self {
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Auto => "auto",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Manual => "manual",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }
}

pub fn parse_permission_mode(raw: &str) -> Result<PermissionMode> {
    use PermissionMode as M;
    // Spelled the way Claude Code spells them, and the way they appear in
    // the config file, so there is one spelling to remember.
    match raw {
        "acceptEdits" => Ok(M::AcceptEdits),
        "auto" => Ok(M::Auto),
        "bypassPermissions" => Ok(M::BypassPermissions),
        "manual" => Ok(M::Manual),
        "dontAsk" => Ok(M::DontAsk),
        "plan" => Ok(M::Plan),
        _ => Err(Error::invalid_args(format!(
            "unknown permission mode {raw}; one of acceptEdits, auto, bypassPermissions, manual, dontAsk, plan"
        ))),
    }
}

// Controller's error text historically included the Rust Debug type names.
// Keep that observable text when the internal types are renamed.
pub(crate) fn unexpected_probe_reply(report: &ClaudeReport) -> String {
    format!(
        "Claude(ClaudeReport {{ path: {:?}, version: {:?}, auth: {:?} }})",
        report.path, report.version, report.auth
    )
}

pub fn auth_hint(config_dir: Option<&Path>) -> String {
    const WHERE: &str = "asked from the Agent Node's login session, so this is Claude's real answer, not an artefact of ssh";
    match config_dir {
        Some(dir) => format!(
            "Claude is not authenticated in the configured CLAUDE_CONFIG_DIR ({WHERE})\nrun on the Agent Node, on its own screen: CLAUDE_CONFIG_DIR={} claude auth login",
            dir.display()
        ),
        None => format!(
            "Claude is not authenticated on the Agent Node ({WHERE})\nrun on the Agent Node, on its own screen: claude auth login\nan expired OAuth session looks the same as never having logged in; either way the fix is that command"
        ),
    }
}

/// Find the `claude` binary. Non-interactive ssh sessions often have a bare
/// PATH, so the usual install locations are tried after it.
pub fn locate(path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    locate_with_known_dirs(
        path_var,
        home,
        &[
            PathBuf::from("/usr/local/bin/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
        ],
    )
}

fn locate_with_known_dirs(
    path_var: Option<&OsStr>,
    home: Option<&Path>,
    known: &[PathBuf],
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = path_var {
        candidates.extend(std::env::split_paths(path).map(|dir| dir.join("claude")));
    }
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/claude"));
        candidates.push(home.join(".claude/local/claude"));
    }
    candidates.extend(known.iter().cloned());
    candidates.into_iter().find(|p| is_executable(p))
}

pub fn locate_from_env() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    locate(std::env::var_os("PATH").as_deref(), home.as_deref())
}

use crate::process::is_executable;

pub fn version_cmd(bin: &Path, config_dir: Option<&Path>) -> Cmd {
    with_config_dir(Cmd::new(bin).arg("--version"), config_dir).timeout(Duration::from_secs(20))
}

pub fn auth_status_cmd(bin: &Path, config_dir: Option<&Path>) -> Cmd {
    with_config_dir(Cmd::new(bin).args(["auth", "status", "--json"]), config_dir)
        .timeout(Duration::from_secs(20))
}

fn with_config_dir(cmd: Cmd, config_dir: Option<&Path>) -> Cmd {
    match config_dir {
        Some(dir) => cmd.env("CLAUDE_CONFIG_DIR", dir),
        None => cmd,
    }
}

/// `2.1.259 (Claude Code)` -> `2.1.259`.
pub fn parse_version(out: &Output) -> Result<String> {
    if !out.success() {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "claude --version failed (exit {:?}): {}",
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        ));
    }
    let stdout = out.stdout_lossy();
    stdout
        .split_whitespace()
        .next()
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Version,
                format!(
                    "claude --version printed something unexpected: {:?}",
                    stdout.trim()
                ),
            )
        })
}

/// Ask the local `claude` about itself.
///
/// Deliberately shallow: ccnm never looks at a credential itself, not even
/// to prove that it could. Everything it claims about the login is what
/// `claude auth status --json` said, in the context it was asked from.
pub fn report(
    bin: Option<&Path>,
    config_dir: Option<&Path>,
    runner: &dyn ProcessRunner,
    ask: Ask,
) -> ClaudeReport {
    let Some(bin) = bin else {
        let missing = Error::new(
            ErrorCode::Version,
            "claude not found: looked in PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
        );
        return ClaudeReport {
            path: None,
            version: Err((&missing).into()),
            auth: Err(missing.into()),
        };
    };
    ClaudeReport {
        path: Some(bin.to_path_buf()),
        version: runner
            .run(&version_cmd(bin, config_dir))
            .and_then(|out| parse_version(&out))
            .map_err(Into::into),
        auth: match ask {
            Ask::Everything => runner
                .run(&auth_status_cmd(bin, config_dir))
                .and_then(|out| parse_auth(&out))
                .map_err(Into::into),
            Ask::VersionOnly => Err(crate::error::ErrorReport::new(
                ErrorCode::NotReady,
                "not asked here: only a login session gets a true answer about the login",
            )),
        },
    }
}

/// The argv ccnm starts Claude Code with.
///
/// Every flag was checked against 2.1.260 `--help` and one real run on
/// 2026-09-04 (design doc section 13):
///
/// - `--tools ""` removes *every* built-in tool. Measured: the session's
///   tool list was exactly the seven `mcp__ccnm__*` names — no Read, no
///   Bash, no Agent, no WebFetch. Simpler and stronger than naming the
///   tools to deny, which is why that list is only the second lock (in
///   settings.json), not the first.
/// - `--strict-mcp-config` keeps out every other MCP server, including the
///   ones the user's enabled plugins would bring. Measured on a machine
///   with eight plugins enabled: none appeared.
/// - `--settings <file>` carries the allow-list; without it every MCP call
///   would want a permission prompt, and in print mode nobody answers.
/// - `--permission-prompts none` turns any prompt that would still happen
///   into a denial, which shows up in the result's `permission_denials`
///   rather than as a hang.
/// - `--setting-sources user,project,local` is the default, spelled out.
///   The user's own settings must load: on the real Agent Node that is
///   where the proxy Claude needs to reach the API is configured (section
///   24). "project" resolves against the cwd, a directory ccnm owns.
/// - `--session-id` makes Claude's id the ccnm session id, so one
///   identifier names the directory here, the output directory on the
///   Runtime Node, and Claude's own transcript.
/// - `--no-session-persistence` in print mode: a one-shot run leaves no
///   entry for `claude --resume` to find. Its record is the session
///   directory.
///
/// The prompt goes in on stdin, not argv: no length limit, no `-` prefix
/// misread as a flag, and Claude stops waiting for stdin the moment it
/// arrives instead of after a three-second timeout.
pub fn launch_cmd(bin: &Path, spec: &Spec, dir: &Dir) -> Cmd {
    let mut cmd = with_config_dir(Cmd::new(bin), spec.provider_config_dir.as_deref())
        .cwd(spec.cwd.clone())
        .timeout(Duration::from_secs(spec.timeout_secs));
    if spec.runtime.is_some() {
        // Remote sessions expose only ccnm. A colocated session needs Claude's
        // native tools and has no mcp.json; passing these flags made it start
        // in the state directory with no usable tools.
        cmd = cmd
            .args(["--tools", ""])
            .arg("--mcp-config")
            .arg(dir.mcp_config())
            .arg("--strict-mcp-config");
    }
    let cmd = cmd
        .arg("--settings")
        .arg(dir.settings())
        .args(["--setting-sources", "user,project,local"])
        .args(["--permission-mode", spec.permission_mode.as_cli_value()])
        .args(["--session-id", &spec.id]);
    match &spec.mode {
        Mode::Print { prompt } => cmd
            .args([
                "--print",
                "--output-format",
                "json",
                "--permission-prompts",
                "none",
                "--no-session-persistence",
            ])
            .stdin(prompt.as_bytes().to_vec()),
        // Nothing to add: plain `claude` is the terminal UI. No
        // `--permission-prompts none`, because now there *is* somebody to
        // answer one, and no `--no-session-persistence`, because a
        // terminal session that a dropped connection interrupted is worth
        // being able to resume. An opening prompt, if there is one, goes
        // on argv — stdin belongs to the terminal.
        Mode::Interactive { prompt: None } => cmd,
        Mode::Interactive {
            prompt: Some(prompt),
        } => cmd.arg(prompt),
    }
}

/// Parse what `claude -p --output-format json` wrote to stdout.
pub fn parse_print(stdout: &[u8]) -> Result<PrintResult> {
    serde_json::from_slice(stdout).map_err(|e| {
        Error::internal(format!(
            "claude did not print a result document; stdout begins: {:?}",
            String::from_utf8_lossy(&stdout[..stdout.len().min(200)])
        ))
        .with_source(e)
    })
}

/// `claude auth status` exits 0 when logged in and 1 when not, printing
/// JSON either way. Output that is not that JSON means a Claude Code too
/// old to have the command.
pub fn parse_auth(out: &Output) -> Result<AuthStatus> {
    if out.timed_out {
        return Err(Error::new(
            ErrorCode::Version,
            "claude auth status timed out",
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            format!(
                "claude auth status did not print its JSON (exit {:?}); Claude Code too old?\nstdout: {}\nstderr: {}",
                out.exit_code,
                out.stdout_lossy().trim(),
                out.stderr_lossy().trim()
            ),
        )
        .with_source(e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::RuntimeLink;

    #[test]
    fn commands_carry_config_dir_only_when_set() {
        let bin = Path::new("/usr/local/bin/claude");
        let cmd = version_cmd(bin, None);
        assert_eq!(cmd.display(), "/usr/local/bin/claude --version");
        assert!(cmd.env.is_empty());

        let cmd = auth_status_cmd(bin, Some(Path::new("/x/claude")));
        assert_eq!(cmd.display(), "/usr/local/bin/claude auth status --json");
        assert_eq!(cmd.env.len(), 1);
        assert_eq!(cmd.env[0].0, "CLAUDE_CONFIG_DIR");
        assert_eq!(cmd.env[0].1, "/x/claude");
    }

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version(&Output::exited(0, "2.1.259 (Claude Code)\n")).unwrap(),
            "2.1.259"
        );
        let err = parse_version(&Output::exited(0, "Claude Code 2.1\n")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        let err = parse_version(&Output::exited(1, "")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
    }

    /// Captured from Claude Code 2.1.259 on 2026-09-03 (ids shortened);
    /// 2.1.260 prints the same fields.
    const LOGGED_IN: &str = r#"{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "apiProvider": "firstParty",
  "analyticsDisabled": false,
  "projectsDirectory": "/Users/me/.claude/projects",
  "email": "me@example.com",
  "orgId": "1e6d91f7",
  "orgName": "me's Organization",
  "subscriptionType": "max"
}"#;

    const LOGGED_OUT: &str = r#"{
  "loggedIn": false,
  "authMethod": "none",
  "apiProvider": "firstParty",
  "analyticsDisabled": false,
  "projectsDirectory": "/tmp/fresh/projects"
}"#;

    #[test]
    fn auth_parsing() {
        let status = parse_auth(&Output::exited(0, LOGGED_IN)).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.describe(), "me@example.com via claude.ai (max)");

        let status = parse_auth(&Output::exited(1, LOGGED_OUT)).unwrap();
        assert!(!status.logged_in);
        assert_eq!(status.email, None);

        let err = parse_auth(&Output::exited(1, "error: unknown command 'auth'")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("too old"), "{err}");
    }

    fn spec() -> Spec {
        Spec {
            runtime_node: None,
            agent_identity: None,
            provider: Default::default(),
            protocol: crate::protocol::PROTOCOL,
            id: "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into(),
            workspace: "fixture".into(),
            root: PathBuf::from("/Users/bing/ccnm-fixture"),
            runtime: Some(RuntimeLink {
                alias: "xdwmbp".into(),
                ccnm_bin: "~/.local/bin/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: crate::config::PermissionMode::AcceptEdits,
            mode: Mode::Print {
                prompt: "-p looks like a flag\nand spans lines".into(),
            },
            timeout_secs: 600,
            cwd: PathBuf::from("/Users/me/.local/state/ccnm/workspaces/fixture"),
            codex_exec_server: false,
        }
    }

    /// The exact argv that was proven on the real machine, and nothing on
    /// it that was not.
    #[test]
    fn launch_argv_is_the_proven_shape() {
        let dir =
            Dir::at("/Users/me/.local/state/ccnm/sessions/0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d");
        let cmd = launch_cmd(Path::new("/opt/homebrew/bin/claude"), &spec(), &dir);
        assert_eq!(
            cmd.display(),
            "/opt/homebrew/bin/claude --tools  --mcp-config /Users/me/.local/state/ccnm/sessions/0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d/mcp.json --strict-mcp-config --settings /Users/me/.local/state/ccnm/sessions/0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d/settings.json --setting-sources user,project,local --permission-mode acceptEdits --session-id 0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d --print --output-format json --permission-prompts none --no-session-persistence"
        );
        // `--tools ""` is a real empty argument, not a dropped one.
        let tools_at = cmd.args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(cmd.args[tools_at + 1], "");
        // The prompt is on stdin and nowhere on the command line.
        assert_eq!(
            cmd.stdin.as_deref(),
            Some("-p looks like a flag\nand spans lines".as_bytes())
        );
        assert!(!cmd.display().contains("looks like a flag"));
        assert_eq!(
            cmd.cwd.as_deref(),
            Some(Path::new("/Users/me/.local/state/ccnm/workspaces/fixture"))
        );
        assert_eq!(cmd.timeout, Duration::from_secs(600));
        assert!(cmd.env.is_empty());

        let mut with_dir = spec();
        with_dir.provider_config_dir = Some(PathBuf::from("/x/claude"));
        let cmd = launch_cmd(Path::new("/c"), &with_dir, &dir);
        assert_eq!(cmd.env[0].0, "CLAUDE_CONFIG_DIR");
    }

    /// The real document from 2.1.260, so a field rename upstream shows up
    /// here rather than as a zero in someone's report.
    #[test]
    fn print_result_parses_the_real_document() {
        let doc = include_bytes!("../../../../../tests/fixtures/claude-print-2.1.260.json");
        let r = parse_print(doc).unwrap();
        assert!(!r.is_error);
        assert_eq!(r.subtype.as_deref(), Some("success"));
        assert_eq!(r.num_turns, 2);
        assert_eq!(r.usage.input_tokens, 34);
        assert_eq!(r.usage.output_tokens, 137);
        assert_eq!(r.usage.cache_creation_input_tokens, 9205);
        assert_eq!(r.usage.cache_read_input_tokens, 9093);
        assert!(
            r.total_cost_usd > 0.19 && r.total_cost_usd < 0.20,
            "{}",
            r.total_cost_usd
        );
        assert!(r.permission_denials.is_empty());
        assert!(
            r.result
                .as_deref()
                .unwrap()
                .contains("mcp__ccnm__apply_patch")
        );
        assert_eq!(
            r.summary(),
            "2 turns in 7.9 s (api 11.0 s); tokens in 34 out 137 cache-write 9205 cache-read 9093; $0.19; 0 permission denials"
        );

        // The roundtrip that carries it home over ssh.
        let json = serde_json::to_string(&r).unwrap();
        let back: PrintResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn a_result_with_fields_missing_still_parses() {
        let r = parse_print(br#"{"is_error":true,"result":"Not logged in"}"#).unwrap();
        assert!(r.is_error);
        assert_eq!(r.num_turns, 0);
        assert_eq!(r.usage, Usage::default());
        let err = parse_print(b"Warning: something went wrong\n").unwrap_err();
        assert!(err.message().contains("stdout begins"), "{err}");
    }

    #[test]
    fn locate_prefers_path_then_known_dirs() {
        let dir = std::env::temp_dir().join(format!("ccnm-claude-locate-{}", std::process::id()));
        let _cleanup = ccnm_testdir::TestDir::adopt(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fake = bin_dir.join("claude");
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var = std::env::join_paths([dir.join("nowhere"), bin_dir.clone()]).unwrap();
        assert_eq!(locate(Some(&path_var), None), Some(fake.clone()));

        // Not on PATH, but in ~/.local/bin.
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::rename(&fake, home.join(".local/bin/claude")).unwrap();
        assert_eq!(
            locate(Some(&path_var), Some(&home)),
            Some(home.join(".local/bin/claude"))
        );

        // A non-executable file does not count.
        std::fs::write(&fake, "").unwrap();
        assert_eq!(locate_with_known_dirs(Some(&path_var), None, &[]), None);
    }
}

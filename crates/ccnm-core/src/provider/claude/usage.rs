//! `claude -p /usage` (P105): Claude Code's own report of the login's
//! subscription limits.
//!
//! Measured (docs/research/2026-10-19-p105-usage.md): in print mode
//! `/usage` is a local command. The result document says
//! `local_command: "usage"` and `num_turns: 0`, and against a fake API
//! (2.1.300) no `/v1/messages` request was made. On a subscription login
//! (2.1.280 and 2.1.303, from the Agent's login session) its text starts:
//!
//! ```text
//! You are currently using your subscription to power your Claude Code usage
//!
//! Current session: 17% used · resets Oct 19 at 6pm (Asia/Singapore)
//! Current week (all models): 38% used · resets Oct 24 at 11am (Asia/Singapore)
//! ```
//!
//! and goes on with "What's contributing to your limits usage?", a local
//! estimate from this machine's transcripts that is not brought back. An
//! API key, and no login at all, give only the session's cost lines.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output};
use crate::quota::{Note, Quota, Window};

/// The oldest Claude Code `claude -p /usage` was measured on.
///
/// Older ones are not asked. One that did not know `/usage` as a local
/// command could send it to the model as a prompt -- spending the very
/// quota this is asking about -- and nothing before the run can tell.
pub const MIN_VERSION: &str = "2.1.280";

/// Where the section ccnm does not bring back begins.
const CONTRIBUTING: &str = "What's contributing";

/// `claude --print /usage`, the way it was measured.
///
/// - `--no-session-persistence` keeps the question out of `claude
///   --resume`; measured, it still leaves one empty `memory/` directory
///   under `~/.claude/projects/` for the working directory, which is why
///   the caller passes the same directory every time.
/// - `--strict-mcp-config` with no `--mcp-config`: no MCP server is started
///   to answer a question that needs none.
/// - Setting sources are left at the default for the reason
///   [`super::launch_cmd`] spells them out: the user's settings are where
///   the proxy Claude needs may be configured.
/// - No stdin, so Claude does not wait three seconds for a prompt there.
pub fn cmd(bin: &Path, config_dir: Option<&Path>, cwd: &Path) -> Cmd {
    super::with_config_dir(
        Cmd::new(bin).args([
            "--print",
            "/usage",
            "--output-format",
            "json",
            "--no-session-persistence",
            "--strict-mcp-config",
        ]),
        config_dir,
    )
    .cwd(cwd)
    .timeout(Duration::from_secs(60))
}

/// Whether `version` (`2.1.303`) is at least [`MIN_VERSION`].
pub fn measured(version: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parts(version), parts(MIN_VERSION)) {
        (Some(have), Some(need)) => have >= need,
        _ => false,
    }
}

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    local_command: Option<String>,
    #[serde(default)]
    num_turns: u64,
    #[serde(default)]
    result: String,
}

/// What `claude --print /usage --output-format json` printed, as limits.
///
/// Anything but the local command's own result is refused rather than
/// read: a document without `local_command: "usage"` came from a turn,
/// and its text is a model's answer, not Claude Code's numbers.
pub fn parse(out: &Output, version: &str, plan: Option<String>) -> Result<Quota> {
    if out.timed_out {
        return Err(Error::new(
            ErrorCode::NotReady,
            "claude --print /usage did not answer within 60 seconds",
        ));
    }
    let doc: Document = serde_json::from_slice(&out.stdout).map_err(|e| {
        Error::new(
            ErrorCode::Version,
            format!(
                "claude --print /usage did not print its JSON result (exit {:?})\nstderr: {}",
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        )
        .with_source(e)
    })?;
    if doc.local_command.as_deref() != Some("usage") {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "Claude Code {version} did not answer /usage as its local command (turns: {}); its answer is not read as limits",
                doc.num_turns
            ),
        ));
    }
    if doc.is_error {
        return Err(Error::new(
            ErrorCode::Version,
            format!("claude --print /usage failed: {}", doc.result.trim()),
        ));
    }
    let (windows, notes) = read_text(&doc.result);
    Ok(Quota {
        provider: crate::provider::AgentProvider::Claude,
        version: version.to_string(),
        plan,
        windows,
        notes,
    })
}

/// The windows and the account lines, from the text before
/// [`CONTRIBUTING`].
///
/// Account lines are kept only when there is a window: without one, the
/// text is the cost of this very `--print` run (all zeros), which says
/// nothing about the login.
fn read_text(text: &str) -> (Vec<Window>, Vec<Note>) {
    let mut windows = Vec::new();
    let mut said = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(CONTRIBUTING) {
            break;
        }
        if line.is_empty() {
            continue;
        }
        match window(line) {
            Some(w) => windows.push(w),
            None => said.push(Note::Said {
                text: line.to_string(),
            }),
        }
    }
    if windows.is_empty() {
        said.clear();
    }
    (windows, said)
}

/// `Current week (all models): 38% used · resets Oct 24 at 11am (Asia/Singapore)`.
fn window(line: &str) -> Option<Window> {
    let (label, rest) = line.split_once(": ")?;
    let (percent, rest) = rest.split_once("% used")?;
    let used: f64 = percent.trim().parse().ok()?;
    let resets = rest
        .trim_start()
        .strip_prefix('·')
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix("resets "))
        .map(|when| when.trim().to_string())
        .filter(|when| !when.is_empty());
    Some(Window {
        label: Some(label.trim().to_string()),
        limit: None,
        minutes: None,
        used_percent: used.round().max(0.0) as u32,
        resets_at: None,
        resets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Output {
        let path = format!(
            "{}/../../tests/fixtures/usage/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        Output::exited(0, std::fs::read(&path).unwrap())
    }

    #[test]
    fn a_subscription_login_gives_its_windows_and_not_the_estimate() {
        for version in ["2.1.280", "2.1.303"] {
            let quota = parse(
                &fixture(&format!("claude-usage-subscription-{version}.json")),
                version,
                Some("max".into()),
            )
            .unwrap();
            let rows: Vec<(String, u32, Option<String>)> = quota
                .windows
                .iter()
                .map(|w| (w.label.clone().unwrap(), w.used_percent, w.resets.clone()))
                .collect();
            assert_eq!(
                rows,
                [
                    (
                        "Current session".to_string(),
                        17,
                        Some("Oct 19 at 6pm (Asia/Singapore)".to_string())
                    ),
                    (
                        "Current week (all models)".to_string(),
                        38,
                        Some("Oct 24 at 11am (Asia/Singapore)".to_string())
                    ),
                    (
                        "Current week (Fable)".to_string(),
                        36,
                        Some("Oct 24 at 11am (Asia/Singapore)".to_string())
                    ),
                ],
                "{version}"
            );
            // The account line comes back; the local estimate after it,
            // with its skill and MCP server names, does not.
            assert_eq!(
                quota.notes,
                [Note::Said {
                    text:
                        "You are currently using your subscription to power your Claude Code usage"
                            .into()
                }]
            );
            assert_eq!(quota.plan.as_deref(), Some("max"));
        }
    }

    #[test]
    fn an_api_key_login_has_no_window_and_no_cost_lines() {
        let quota = parse(
            &fixture("claude-usage-api-key-2.1.300.json"),
            "2.1.300",
            None,
        )
        .unwrap();
        assert!(quota.windows.is_empty());
        assert!(quota.notes.is_empty(), "{:?}", quota.notes);
    }

    #[test]
    fn an_answer_from_a_turn_is_refused_not_read() {
        let out = Output::exited(
            0,
            r#"{"type":"result","is_error":false,"num_turns":1,"result":"Current session: 5% used"}"#,
        );
        let err = parse(&out, "2.1.303", None).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("turns: 1"), "{err}");
    }

    #[test]
    fn output_that_is_not_the_document_is_a_version_problem() {
        let out = Output::exited(1, "Error: unknown option '--no-session-persistence'");
        assert_eq!(
            parse(&out, "2.1.100", None).unwrap_err().code(),
            ErrorCode::Version
        );
        let mut slow = Output::exited(0, "");
        slow.timed_out = true;
        assert_eq!(
            parse(&slow, "2.1.303", None).unwrap_err().code(),
            ErrorCode::NotReady
        );
    }

    #[test]
    fn a_window_line_without_a_reset_or_with_a_fraction_still_reads() {
        let w = window("Current week (Sonnet only): 4.6% used").unwrap();
        assert_eq!((w.used_percent, w.resets), (5, None));
        assert!(window("Total cost:            $0.0000").is_none());
        assert!(window("Usage:                 0 input, 0 output").is_none());
    }

    #[test]
    fn only_measured_versions_are_asked() {
        assert!(measured("2.1.280"));
        assert!(measured("2.1.303"));
        assert!(measured("2.2.0"));
        assert!(measured("10.0.0"));
        assert!(!measured("2.1.279"));
        assert!(!measured("2.0.999"));
        assert!(!measured("2.1.300-beta"));
        assert!(!measured(""));
    }

    use crate::process::FakeRunner;
    use crate::provider::AgentProvider;

    fn quota_at(fake: &FakeRunner, bin: Option<&str>) -> Result<Quota> {
        AgentProvider::Claude.quota_at(
            bin.map(Path::new),
            Some(Path::new("/x/claude")),
            fake,
            Path::new("/state/usage"),
        )
    }

    #[test]
    fn version_and_login_come_first_then_usage_in_the_same_profile() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "2.1.303 (Claude Code)\n"));
        fake.push(Output::exited(
            0,
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}"#,
        ));
        fake.push(fixture("claude-usage-subscription-2.1.303.json"));
        let quota = quota_at(&fake, Some("/opt/claude")).unwrap();
        assert_eq!(quota.version, "2.1.303");
        assert_eq!(quota.plan.as_deref(), Some("max"));
        assert_eq!(quota.windows.len(), 3);
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(
            calls[1].display().contains("auth status"),
            "{}",
            calls[1].display()
        );
        assert!(
            calls[2].display().contains("/usage"),
            "{}",
            calls[2].display()
        );
        assert_eq!(calls[2].cwd.as_deref(), Some(Path::new("/state/usage")));
        for call in &calls {
            assert!(
                call.env
                    .iter()
                    .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude"),
                "{}",
                call.display()
            );
        }
    }

    #[test]
    fn no_login_is_said_as_such_and_usage_is_not_run() {
        // Measured on 2.1.303: without a login /usage prints the same cost
        // lines an API key gets, which would read as "no limits".
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "2.1.303 (Claude Code)\n"));
        fake.push(Output::exited(
            1,
            r#"{"loggedIn":false,"authMethod":"none"}"#,
        ));
        let err = quota_at(&fake, Some("/opt/claude")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Auth);
        assert_eq!(fake.calls().len(), 2);
    }

    #[test]
    fn an_unmeasured_claude_is_not_asked() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "2.1.279 (Claude Code)\n"));
        fake.push(Output::exited(0, r#"{"loggedIn":true}"#));
        let err = quota_at(&fake, Some("/opt/claude")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("2.1.280"), "{err}");
        assert_eq!(fake.calls().len(), 2, "/usage was not run");

        let fake = FakeRunner::new();
        let err = quota_at(&fake, None).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn codex_other_than_the_pinned_release_is_not_asked() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "codex-cli 0.162.1\n"));
        let err = AgentProvider::Codex
            .quota_at(
                Some(Path::new("/opt/codex")),
                Some(Path::new("/profile")),
                &fake,
                Path::new("/state/usage"),
            )
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message().contains("0.154.0"), "{err}");
        assert_eq!(fake.calls().len(), 1, "only --version ran");
    }

    #[test]
    fn the_command_is_the_measured_one() {
        let cmd = cmd(
            Path::new("/bin/claude"),
            Some(Path::new("/p")),
            Path::new("/state/usage"),
        );
        assert_eq!(
            cmd.args,
            [
                "--print",
                "/usage",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--strict-mcp-config"
            ]
            .map(std::ffi::OsString::from)
        );
        assert_eq!(cmd.env[0].0, "CLAUDE_CONFIG_DIR");
        assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/state/usage")));
        assert!(cmd.stdin.is_none());
    }
}

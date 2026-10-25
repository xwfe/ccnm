//! `codex app-server`'s `account/rateLimits/read` (P105): Codex's own
//! report of the login's limits, as JSON-RPC over stdio.
//!
//! Measured (docs/research/2026-10-19-p105-usage.md): the response schema
//! that 0.154.0 and 0.162.1 each generate (`app-server
//! generate-json-schema`) has the same fields; a real ChatGPT login
//! answered on 0.162.1 with `tests/fixtures/usage/codex-rate-limits-0.162.1.json`.
//! The server drops a request still in flight when its input ends, so the
//! requests go in and the input stays open until the answer is out
//! ([`crate::process::stream_lines`]). No model request is involved.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Flow, stream_lines};
use crate::quota::{Note, Quota, Window};

const INITIALIZE: u64 = 1;
const READ: u64 = 2;

/// `codex app-server` in the profile, with the three requests on stdin.
pub fn cmd(bin: &Path, home: &Path) -> Cmd {
    let lines = [
        json!({"id": INITIALIZE, "method": "initialize", "params": {
            "clientInfo": {"name": "ccnm", "version": crate::VERSION}}}),
        json!({"method": "initialized"}),
        json!({"id": READ, "method": "account/rateLimits/read", "params": null}),
    ];
    let stdin: String = lines.iter().map(|line| format!("{line}\n")).collect();
    super::isolated(Cmd::new(bin).arg("app-server"), home)
        .stdin(stdin.into_bytes())
        .timeout(Duration::from_secs(30))
}

/// Ask the profile's Codex for its limits. `version` is what `codex
/// --version` already said, checked by the caller.
pub fn ask(bin: &Path, home: &Path, version: &str) -> Result<Quota> {
    let mut answer: Option<Value> = None;
    let streamed = stream_lines(&cmd(bin, home), |line| {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return Flow::Continue;
        };
        match message.get("id").and_then(Value::as_u64) {
            Some(INITIALIZE) if message.get("error").is_some() => {
                answer = Some(message);
                Flow::Stop
            }
            Some(READ) => {
                answer = Some(message);
                Flow::Close
            }
            _ => Flow::Continue,
        }
    })?;
    match answer {
        Some(message) => parse(&message, version),
        None => {
            let stderr = String::from_utf8_lossy(&streamed.stderr);
            let how = if streamed.timed_out {
                "did not answer within 30 seconds".to_string()
            } else {
                format!("ended without answering (exit {:?})", streamed.exit_code)
            };
            Err(Error::new(
                ErrorCode::NotReady,
                format!(
                    "codex app-server {how} account/rateLimits/read\nstderr: {}",
                    stderr.trim()
                ),
            ))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    rate_limits: Snapshot,
    #[serde(default)]
    rate_limits_by_limit_id: Option<BTreeMap<String, Snapshot>>,
    #[serde(default)]
    rate_limit_reset_credits: Option<ResetCredits>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    #[serde(default)]
    limit_id: Option<String>,
    #[serde(default)]
    limit_name: Option<String>,
    #[serde(default)]
    primary: Option<RateWindow>,
    #[serde(default)]
    secondary: Option<RateWindow>,
    #[serde(default)]
    credits: Option<Credits>,
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit_reached_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    used_percent: i64,
    #[serde(default)]
    window_duration_mins: Option<u64>,
    #[serde(default)]
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credits {
    has_credits: bool,
    unlimited: bool,
    #[serde(default)]
    balance: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCredits {
    available_count: u64,
}

/// One JSON-RPC response from the server, as limits.
pub fn parse(message: &Value, version: &str) -> Result<Quota> {
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        return Err(Error::new(
            ErrorCode::NotReady,
            format!("Codex did not report its limits: {text}"),
        ));
    }
    let reply: Reply = message
        .get("result")
        .cloned()
        .ok_or_else(|| Error::new(ErrorCode::Version, "account/rateLimits/read had no result"))
        .and_then(|result| {
            serde_json::from_value(result).map_err(|e| {
                Error::new(
                    ErrorCode::Version,
                    format!(
                        "Codex {version} answered account/rateLimits/read in a shape ccnm has not measured"
                    ),
                )
                .with_source(e)
            })
        })?;
    let main = &reply.rate_limits;
    let mut windows: Vec<Window> = windows_of(main, None);
    for (id, other) in reply.rate_limits_by_limit_id.iter().flatten() {
        if other.limit_id.as_deref().unwrap_or(id) == main.limit_id.as_deref().unwrap_or("") {
            continue;
        }
        let name = other.limit_name.clone().unwrap_or_else(|| id.clone());
        windows.extend(windows_of(other, Some(name)));
    }
    let mut notes = Vec::new();
    if let Some(reason) = &main.rate_limit_reached_type {
        notes.push(Note::Reached {
            reason: reason.clone(),
        });
    }
    if let Some(credits) = main
        .credits
        .as_ref()
        .filter(|c| c.has_credits || c.unlimited)
    {
        notes.push(Note::Credits {
            balance: credits.balance.clone(),
            unlimited: credits.unlimited,
        });
    }
    if let Some(available) = reply
        .rate_limit_reset_credits
        .map(|r| r.available_count)
        .filter(|n| *n > 0)
    {
        notes.push(Note::ResetCredits { available });
    }
    Ok(Quota {
        provider: crate::provider::AgentProvider::Codex,
        version: version.to_string(),
        plan: main.plan_type.clone().filter(|plan| plan != "unknown"),
        windows,
        notes,
    })
}

fn windows_of(snapshot: &Snapshot, limit: Option<String>) -> Vec<Window> {
    [&snapshot.primary, &snapshot.secondary]
        .into_iter()
        .flatten()
        .map(|w| Window {
            label: None,
            limit: limit.clone(),
            minutes: w.window_duration_mins,
            used_percent: u32::try_from(w.used_percent.max(0)).unwrap_or(u32::MAX),
            resets_at: w.resets_at.and_then(|at| u64::try_from(at).ok()),
            resets: None,
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;

    fn fixture() -> Value {
        let path = format!(
            "{}/../../tests/fixtures/usage/codex-rate-limits-0.162.1.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn the_measured_reply_reads_as_one_used_up_weekly_window() {
        let quota = parse(&fixture(), "0.154.0").unwrap();
        assert_eq!(quota.plan.as_deref(), Some("prolite"));
        assert_eq!(
            quota.windows,
            [Window {
                label: None,
                limit: None,
                minutes: Some(10080),
                used_percent: 100,
                resets_at: Some(1_792_906_538),
                resets: None,
            }]
        );
        // The by-limit copy of the main limit is not listed twice, and a
        // zero balance with no credits is not worth a line.
        assert_eq!(
            quota.notes,
            [
                Note::Reached {
                    reason: "rate_limit_reached".into()
                },
                Note::ResetCredits { available: 3 },
            ]
        );
    }

    #[test]
    fn both_windows_and_another_limit_are_listed() {
        let reply = json!({"id": 2, "result": {
            "rateLimits": {"limitId": "codex", "planType": "unknown",
                "primary": {"usedPercent": 12, "windowDurationMins": 300, "resetsAt": 10},
                "secondary": {"usedPercent": 40, "windowDurationMins": 10080},
                "credits": {"hasCredits": true, "unlimited": false, "balance": "4.20"}},
            "rateLimitsByLimitId": {
                "codex": {"limitId": "codex", "primary": {"usedPercent": 12}},
                "codex_other": {"limitId": "codex_other", "limitName": "GPT-5.5",
                    "primary": {"usedPercent": 70, "windowDurationMins": 300}}}}});
        let quota = parse(&reply, "0.154.0").unwrap();
        assert_eq!(quota.plan, None);
        let rows: Vec<(Option<&str>, Option<u64>, u32)> = quota
            .windows
            .iter()
            .map(|w| (w.limit.as_deref(), w.minutes, w.used_percent))
            .collect();
        assert_eq!(
            rows,
            [
                (None, Some(300), 12),
                (None, Some(10080), 40),
                (Some("GPT-5.5"), Some(300), 70)
            ]
        );
        assert_eq!(
            quota.notes,
            [Note::Credits {
                balance: Some("4.20".into()),
                unlimited: false
            }]
        );
    }

    #[test]
    fn an_error_reply_or_an_unknown_shape_is_not_limits() {
        let err = parse(
            &json!({"id": 2, "error": {"code": -32600, "message": "authentication required"}}),
            "0.154.0",
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("authentication required"), "{err}");
        let err = parse(&json!({"id": 2, "result": {"limits": []}}), "0.154.0").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
    }

    /// A stand-in for `codex app-server` that answers like the real one --
    /// including dropping the request when its input ends early -- so the
    /// conversation is tested over a real pipe.
    fn scratch(tag: &str) -> TestDir {
        let dir =
            std::env::temp_dir().join(format!("ccnm-codex-usage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    fn fake_server(dir: &TestDir, reply: &str) -> std::path::PathBuf {
        let path = dir.path().join("codex");
        std::fs::write(dir.path().join("reply.json"), format!("{reply}\n")).unwrap();
        // fd 3, because a background command's stdin is /dev/null in a
        // non-interactive sh unless redirected. The reader still waiting
        // after 0.2 s is the input still being open.
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             [ \"$1\" = app-server ] || exit 64\n\
             [ -n \"$CODEX_HOME\" ] || exit 65\n\
             read init; echo '{\"id\":1,\"result\":{}}'\n\
             read note; echo '{\"method\":\"configWarning\",\"params\":{}}'\n\
             read ask\n\
             exec 3<&0\n\
             ( read -r more <&3 ) &\n\
             reader=$!\n\
             sleep 0.2\n\
             kill -0 $reader 2>/dev/null || exit 0\n\
             cat \"$(dirname \"$0\")/reply.json\"\n\
             wait $reader\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_answer_is_read_before_the_input_is_closed() {
        let dir = scratch("answer");
        let reply = serde_json::to_string(&fixture()).unwrap();
        let bin = fake_server(&dir, &reply);
        let quota = ask(&bin, dir.path(), "0.154.0").unwrap();
        assert_eq!(quota.windows.len(), 1);
        assert_eq!(quota.windows[0].used_percent, 100);
    }

    #[test]
    fn a_server_that_ends_without_answering_says_so() {
        let dir = scratch("silent");
        let path = dir.path().join("codex");
        std::fs::write(
            &path,
            "#!/bin/sh\necho 'config.toml is broken' >&2\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = ask(&path, dir.path(), "0.154.0").unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("exit Some(1)"), "{err}");
        assert!(err.message().contains("config.toml is broken"), "{err}");
    }

    #[test]
    fn the_server_runs_in_the_profile_without_inherited_logins() {
        let cmd = cmd(Path::new("/bin/codex"), Path::new("/profile"));
        assert_eq!(cmd.args, ["app-server"].map(std::ffi::OsString::from));
        assert!(
            cmd.env
                .iter()
                .any(|(k, v)| k == "CODEX_HOME" && v == "/profile")
        );
        let stdin = String::from_utf8(cmd.stdin.clone().unwrap()).unwrap();
        let methods: Vec<String> = stdin
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["method"].to_string())
            .collect();
        assert_eq!(
            methods,
            [
                "\"initialize\"",
                "\"initialized\"",
                "\"account/rateLimits/read\""
            ]
        );
    }
}

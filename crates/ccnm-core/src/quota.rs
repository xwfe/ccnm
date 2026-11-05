//! `ccnm usage` (P105): how much of an Agent login's subscription limits
//! is used, as that login's own official CLI reports it.
//!
//! Only through Claude Code's own entry point, `claude -p /usage`, and only
//! read. ccnm never opens a credential and never calls a vendor's HTTP API
//! itself, so everything here is what that said, in the shape measured in
//! docs/research/2026-10-19-p105-usage.md. A limit belongs to a login, not
//! to a project: every workspace on the same login shares it.

use serde::{Deserialize, Serialize};

use crate::Lang;
use crate::overview::table;
use crate::provider::AgentProvider;

/// One login's limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub provider: AgentProvider,
    /// The CLI that answered, `2.1.303`.
    pub version: String,
    /// The plan's name when the CLI gives one: `subscriptionType` from
    /// `auth status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

/// One limit window: how much of it is used and when it starts over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Claude's own name for it (`Current week (all models)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Can pass 100: Claude says so for a spend limit that has been run
    /// past.
    pub used_percent: u32,
    /// The CLI's own words, already in the Agent's time zone
    /// (`Oct 24 at 11am (Asia/Singapore)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets: Option<String>,
}

/// Something the CLI said about the account as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Note {
    /// A limit is used up; `reason` is the CLI's own code
    /// (`rate_limit_reached`, ...).
    Reached { reason: String },
    /// A line the CLI printed about the account, kept as it was.
    Said { text: String },
}

/// One login's answer the way a person reads it: a heading naming who and
/// which CLI, then a row per window, then the notes.
///
/// `who` is the caller's name for the login (the Agent's ssh alias, and
/// the instance when there is one).
pub fn render(who: &str, quota: &Quota, lang: Lang) -> String {
    let mut heading = format!(
        "{who} · {} {}",
        quota.provider.display_name(),
        quota.version
    );
    if let Some(plan) = &quota.plan {
        heading.push_str(&format!(" · {plan}"));
    }
    let mut out = format!("{heading}\n");
    if quota.windows.is_empty() {
        out.push_str(&format!("  {}\n", no_windows(lang)));
    } else {
        let rows: Vec<Vec<String>> = quota
            .windows
            .iter()
            .map(|w| {
                let left = 100u32.saturating_sub(w.used_percent);
                vec![
                    format!("  {}", window_name(w, lang)),
                    lang.pick(
                        format!("已用 {}%", w.used_percent),
                        format!("used {}%", w.used_percent),
                    ),
                    lang.pick(format!("剩 {left}%"), format!("left {left}%")),
                    resets(w, lang),
                ]
            })
            .collect();
        out.push_str(&table(&rows));
    }
    for note in &quota.notes {
        out.push_str(&format!("  {}\n", note_text(note, lang)));
    }
    out
}

/// Why a login reported no window, said so the person knows what to try.
///
/// Claude's `/usage` prints the same few cost lines for an API key, a
/// third-party provider and -- measured on 2.1.303 from an ssh session --
/// no login at all. The last is ruled out before `/usage` runs (`auth
/// status` comes first), so what is left is a login without a
/// subscription, or a Claude Code that prints its limits some other way.
fn no_windows(lang: Lang) -> String {
    lang.pick(
        "没有额度可报：/usage 里没有已用百分比那几行。API key 或第三方平台登录本来就没有 5 小时 / 每周额度；订阅登录还这样，就是 Claude Code 改了输出，在那台机器上开 claude 敲 /usage 看原文",
        "no limits to report: /usage printed no percentage lines. An API key or third-party login has no 5-hour or weekly limit; on a subscription login this means Claude Code changed its output, so open claude on that machine and type /usage to see it",
    )
    .to_string()
}

/// A window's name: Claude's label, in Chinese where it is one ccnm knows.
fn window_name(window: &Window, lang: Lang) -> String {
    match &window.label {
        Some(label) => claude_label(label, lang),
        None => lang.pick("窗口", "window").to_string(),
    }
}

/// Claude's labels as measured on 2.1.280 and 2.1.303. "Current session"
/// is the 5-hour limit (`five_hour` in the status line's `rate_limits`),
/// not a ccnm session, which is why the Chinese does not say 会话. Anything
/// unknown stays in Claude's words.
fn claude_label(label: &str, lang: Lang) -> String {
    if lang == Lang::En {
        return label.to_string();
    }
    if label == "Current session" {
        return "当前 5 小时".to_string();
    }
    match label
        .strip_prefix("Current week (")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some("all models") => "本周（全部模型）".to_string(),
        Some(which) => match which.strip_suffix(" only") {
            Some(model) => format!("本周（仅 {model}）"),
            None => format!("本周（{which}）"),
        },
        None => label.to_string(),
    }
}

fn resets(window: &Window, lang: Lang) -> String {
    match &window.resets {
        Some(text) => lang.pick(format!("{text} 重置"), format!("resets {text}")),
        None => String::new(),
    }
}

fn note_text(note: &Note, lang: Lang) -> String {
    match note {
        Note::Reached { reason } => lang.pick(
            format!("! 额度用完了（{reason}），到上面的重置时间才恢复"),
            format!("! limit reached ({reason}); it comes back at the reset time above"),
        ),
        Note::Said { text } => text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Quota {
        Quota {
            provider: AgentProvider::Claude,
            version: "2.1.303".into(),
            plan: Some("max".into()),
            windows: vec![
                Window {
                    label: Some("Current session".into()),
                    used_percent: 17,
                    resets: Some("Oct 19 at 6pm (Asia/Singapore)".into()),
                },
                Window {
                    label: Some("Current week (all models)".into()),
                    used_percent: 38,
                    resets: Some("Oct 24 at 11am (Asia/Singapore)".into()),
                },
                Window {
                    label: Some("Current week (Sonnet only)".into()),
                    used_percent: 4,
                    resets: None,
                },
            ],
            notes: vec![Note::Said {
                text: "You are currently using your subscription to power your Claude Code usage"
                    .into(),
            }],
        }
    }

    #[test]
    fn a_claude_login_reads_as_used_left_and_when() {
        let text = render("fodelf", &claude(), Lang::Zh);
        assert_eq!(
            text,
            "fodelf · Claude Code 2.1.303 · max\n\
             \x20 当前 5 小时        已用 17%  剩 83%  Oct 19 at 6pm (Asia/Singapore) 重置\n\
             \x20 本周（全部模型）   已用 38%  剩 62%  Oct 24 at 11am (Asia/Singapore) 重置\n\
             \x20 本周（仅 Sonnet）  已用 4%   剩 96%\n\
             \x20 You are currently using your subscription to power your Claude Code usage\n"
        );
        let text = render("fodelf", &claude(), Lang::En);
        assert!(
            text.contains("Current week (all models)   used 38%  left 62%  resets Oct 24"),
            "{text}"
        );
    }

    #[test]
    fn no_window_says_what_to_try() {
        let quota = Quota {
            windows: vec![],
            notes: vec![],
            plan: None,
            ..claude()
        };
        let text = render("fodelf", &quota, Lang::Zh);
        assert!(text.contains("API key"), "{text}");
        assert!(text.contains("敲 /usage"), "{text}");
    }

    #[test]
    fn a_label_ccnm_does_not_know_stays_in_claudes_words() {
        assert_eq!(claude_label("Spend limit", Lang::Zh), "Spend limit");
        assert_eq!(
            claude_label("Current week (Fable)", Lang::Zh),
            "本周（Fable）"
        );
    }
}

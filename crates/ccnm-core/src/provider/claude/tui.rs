//! Reading Claude Code's terminal screen, for the one thing ccnm types into
//! it: `/mcp reconnect ccnm`, when someone enters a session whose tools are
//! down (P100).
//!
//! Everything here was measured on Claude Code 2.1.300 with
//! `tmux capture-pane -p` (docs/research/probes/p100-session-hooks.py). The
//! screen is Claude Code's, not a contract, so every reading fails towards
//! "do not type": a layout ccnm does not recognise is left for the person.

use crate::mcp::server::SERVER_NAME;

/// What ccnm types: Claude Code's own inline command, the same thing
/// `/mcp → ccnm → Reconnect` does in the panel. It runs at once even while
/// the model is answering (measured), so a busy session needs no waiting.
pub fn reconnect_command() -> String {
    format!("/mcp reconnect {SERVER_NAME}")
}

/// The prompt's input box, as far as typing into it is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBox {
    /// `❯` alone between the two rules: nothing typed. The only state ccnm
    /// types into.
    Empty,
    /// Something unsent is in it. Typing would add to it, and clearing it
    /// first would throw away somebody's words.
    Holding,
    /// No input box ccnm recognises: a permission question, a menu, a
    /// multi-line draft, another version's layout. Typing into a question
    /// could answer it, so nothing is typed.
    Unknown,
}

/// Read the input box off a captured screen.
///
/// Claude Code 2.1.300 draws it as one line starting `❯` (and a no-break
/// space) with a rule of `─` right above and right below; the status lines
/// come after. Vim mode's `-- INSERT --` / `-- VISUAL --` means the keys
/// would not necessarily be text, so that is unknown too.
pub fn input_box(screen: &str) -> InputBox {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    if lines
        .iter()
        .any(|line| line.contains("-- INSERT --") || line.contains("-- VISUAL --"))
    {
        return InputBox::Unknown;
    }
    let is_rule = |line: &str| {
        let line = line.trim();
        !line.is_empty() && line.chars().all(|c| c == '─')
    };
    let Some(at) = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('❯'))
    else {
        return InputBox::Unknown;
    };
    let framed =
        at > 0 && is_rule(lines[at - 1]) && lines.get(at + 1).is_some_and(|line| is_rule(line));
    if !framed {
        return InputBox::Unknown;
    }
    let typed = lines[at].trim_start().trim_start_matches('❯');
    if typed.trim().is_empty() {
        InputBox::Empty
    } else {
        InputBox::Holding
    }
}

/// How many times the screen shows Claude Code's answer to a reconnect,
/// either way. Counted rather than looked for: an earlier reconnect can
/// still be on screen, so it is a *new* line that answers this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconnectAnswers {
    pub succeeded: usize,
    pub failed: usize,
}

/// `Successfully reconnected to ccnm` / `Failed to reconnect to ccnm`
/// (2.1.300). The success can be cut short: typed while the model was
/// answering, it showed on the right edge as `Successfully reconnected`.
pub fn reconnect_answers(screen: &str) -> ReconnectAnswers {
    let failed = format!("Failed to reconnect to {SERVER_NAME}");
    ReconnectAnswers {
        succeeded: screen.matches("Successfully reconnected").count(),
        failed: screen.matches(failed.as_str()).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────";

    /// The bottom of a 2.1.300 screen around the input box, as
    /// `capture-pane -p` gives it: `❯` is followed by a no-break space.
    fn screen(above: &[&str], input: &str, footer: &str) -> String {
        let mut lines: Vec<String> = above.iter().map(|s| s.to_string()).collect();
        lines.push(RULE.into());
        lines.push(format!("❯\u{a0}{input}"));
        lines.push(RULE.into());
        lines.push(format!("  {footer}"));
        lines.join("\n") + "\n\n\n"
    }

    const IDLE: &str = "⏸ manual mode on · ? for shortcuts · ← for agents";
    const BUSY: &str = "⏸ manual mode on · esc to interrupt · ← for agents";

    #[test]
    fn an_empty_box_is_empty_idle_or_busy() {
        let idle = screen(&["❯ hello one", "⏺ reply 0", "✻ Crunched for 0s"], "", IDLE);
        assert_eq!(input_box(&idle), InputBox::Empty);
        // The model answering: the box is drawn the same, and the command
        // was measured to run at once.
        let busy = screen(&["❯ SLOW one", "✽ Flibbertigibbeting…"], "", BUSY);
        assert_eq!(input_box(&busy), InputBox::Empty);
    }

    #[test]
    fn a_draft_is_never_typed_into() {
        let draft = screen(
            &["⏺ reply 0"],
            "a draft that is never sent",
            "⏸ manual mode on",
        );
        assert_eq!(input_box(&draft), InputBox::Holding);
    }

    /// The prompts in the conversation above start with `❯` too; only the
    /// framed one at the bottom is the input box.
    #[test]
    fn only_the_framed_prompt_counts() {
        let no_box = "❯ hello one\n⏺ reply 0\n\n  Do you want to proceed?\n  ❯ 1. Yes\n    2. No\n";
        assert_eq!(input_box(no_box), InputBox::Unknown, "a question is open");
        assert_eq!(input_box(""), InputBox::Unknown);
        // A draft over two lines: the second rule is not right below `❯`.
        let two = format!("{RULE}\n❯\u{a0}first line\n  second line\n{RULE}\n  {IDLE}\n");
        assert_eq!(input_box(&two), InputBox::Unknown);
    }

    #[test]
    fn vim_mode_is_left_alone() {
        let vim = screen(&["⏺ reply 0"], "", "-- INSERT -- ⏸ manual mode on");
        assert_eq!(input_box(&vim), InputBox::Unknown);
    }

    /// The real thing: screens captured from Claude Code 2.1.300 in the P100
    /// probe, only the working directory line rewritten.
    #[test]
    fn real_2_1_300_screens_read_as_measured() {
        macro_rules! capture {
            ($name:literal) => {
                include_str!(concat!(
                    "../../../../../tests/fixtures/claude-tui-2.1.300/",
                    $name,
                    ".txt"
                ))
            };
        }
        let none = ReconnectAnswers::default();
        for (name, screen, input, answers) in [
            ("idle", capture!("1-idle-empty"), InputBox::Empty, none),
            ("draft", capture!("2-idle-draft"), InputBox::Holding, none),
            ("busy", capture!("4-busy"), InputBox::Empty, none),
            (
                "dead",
                capture!("5b-reconnect-while-dead"),
                InputBox::Empty,
                ReconnectAnswers {
                    succeeded: 0,
                    failed: 1,
                },
            ),
            (
                "typed while busy",
                capture!("5c-busy-typed"),
                InputBox::Empty,
                ReconnectAnswers {
                    succeeded: 1,
                    failed: 1,
                },
            ),
            // The failure from two attempts back is still on screen: why
            // a reconnect is answered by a *new* line, not by any line.
            (
                "back",
                capture!("6-after-reconnect"),
                InputBox::Empty,
                ReconnectAnswers {
                    succeeded: 1,
                    failed: 1,
                },
            ),
        ] {
            assert_eq!(input_box(screen), input, "{name}");
            assert_eq!(reconnect_answers(screen), answers, "{name}");
        }
    }

    #[test]
    fn reconnect_answers_are_counted() {
        let ok = screen(
            &[
                "❯ /mcp reconnect ccnm",
                "  ⎿  Successfully reconnected to ccnm",
            ],
            "",
            IDLE,
        );
        assert_eq!(
            reconnect_answers(&ok),
            ReconnectAnswers {
                succeeded: 1,
                failed: 0
            }
        );
        let both = screen(
            &[
                "❯ /mcp reconnect ccnm",
                "  ⎿  Failed to reconnect to ccnm",
                "❯ SLOW two",
                "✶ Ebbing…                                      Successfully reconnected",
            ],
            "",
            BUSY,
        );
        assert_eq!(
            reconnect_answers(&both),
            ReconnectAnswers {
                succeeded: 1,
                failed: 1
            }
        );
        assert_eq!(reconnect_command(), "/mcp reconnect ccnm");
    }
}

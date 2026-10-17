//! Reading Claude Code's terminal screen, for the one thing ccnm types into
//! it: `/mcp reconnect ccnm`, when someone enters a session whose tools are
//! down (P100).
//!
//! Everything here was measured on Claude Code 2.1.300 with
//! `tmux capture-pane -p` (docs/research/probes/p100-session-hooks.py), and
//! the placeholder on 2.1.302 with `-e` on fodelf (v0.19.0 release check).
//! The screen is Claude Code's, not a contract, so every reading fails
//! towards "do not type": a layout ccnm does not recognise is left for the
//! person.
//!
//! The screen comes with its colour codes (`capture-pane -e`), for one
//! reason: a session nobody has said anything in shows a suggestion in the
//! empty input box -- `❯ Try "write a test for <filepath>"` -- drawn dim.
//! As plain text it is indistinguishable from somebody's unsent words.

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
/// come after. Text in it that is all drawn dim is the suggestion an empty
/// box shows, not something typed. Vim mode's `-- INSERT --` /
/// `-- VISUAL --` means the keys would not necessarily be text, so that is
/// unknown too.
pub fn input_box(screen: &str) -> InputBox {
    let lines: Vec<Line> = screen.lines().map(Line::read).collect();
    if lines
        .iter()
        .any(|line| line.plain.contains("-- INSERT --") || line.plain.contains("-- VISUAL --"))
    {
        return InputBox::Unknown;
    }
    let is_rule = |line: &Line| {
        let line = line.plain.trim();
        !line.is_empty() && line.chars().all(|c| c == '─')
    };
    let Some(at) = lines
        .iter()
        .rposition(|line| line.plain.trim_start().starts_with('❯'))
    else {
        return InputBox::Unknown;
    };
    let framed = at > 0 && is_rule(&lines[at - 1]) && lines.get(at + 1).is_some_and(is_rule);
    if !framed {
        return InputBox::Unknown;
    }
    let after_prompt = |text: &str| -> bool {
        text.split_once('❯')
            .is_some_and(|(_, typed)| !typed.trim().is_empty())
    };
    // Typed text is drawn in the normal weight; a box whose only text is
    // dim holds the suggestion and nothing of anyone's.
    if after_prompt(&lines[at].bright) {
        InputBox::Holding
    } else {
        InputBox::Empty
    }
}

/// One screen line from `capture-pane -e`: the text, and the part of it not
/// drawn dim. Escape sequences are dropped; a screen without any (`-p`
/// alone, as the P100 fixtures are) reads as all bright.
struct Line {
    plain: String,
    bright: String,
}

impl Line {
    fn read(raw: &str) -> Line {
        let (mut plain, mut bright, mut dim) = (String::new(), String::new(), false);
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                plain.push(c);
                if !dim {
                    bright.push(c);
                }
                continue;
            }
            match chars.next() {
                // CSI: parameters up to a final byte; only SGR (`m`) matters.
                Some('[') => {
                    let mut params = String::new();
                    for p in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&p) {
                            if p == 'm' {
                                dim = sgr_dim(&params, dim);
                            }
                            break;
                        }
                        params.push(p);
                    }
                }
                // OSC (a hyperlink, say): up to BEL or ESC \.
                Some(']') => {
                    while let Some(p) = chars.next() {
                        if p == '\x07' || (p == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        Line {
            plain: plain.trim_end().to_string(),
            bright,
        }
    }
}

/// Whether text is dim after one SGR sequence: `2` turns it on, `22`, `0`
/// and an empty list off. The numbers inside a colour (`38;5;244`,
/// `48;2;r;g;b`) are values, not attributes, and are skipped.
fn sgr_dim(params: &str, mut dim: bool) -> bool {
    if params.is_empty() {
        return false;
    }
    let mut it = params.split(';');
    while let Some(p) = it.next() {
        match p {
            "" | "0" | "22" => dim = false,
            "2" => dim = true,
            "38" | "48" | "58" => match it.next() {
                Some("5") => {
                    it.next();
                }
                Some("2") => {
                    it.nth(2);
                }
                _ => {}
            },
            _ => {}
        }
    }
    dim
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
    let text: String = screen
        .lines()
        .map(|line| Line::read(line).plain + "\n")
        .collect();
    let failed = format!("Failed to reconnect to {SERVER_NAME}");
    ReconnectAnswers {
        succeeded: text.matches("Successfully reconnected").count(),
        failed: text.matches(failed.as_str()).count(),
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

    /// Found in the v0.19.0 release check on fodelf (Claude Code 2.1.302,
    /// `capture-pane -e`): a fresh session's empty box shows a dim
    /// suggestion, which plain text could not tell from a draft, so a
    /// session nobody had said anything in was never reconnected. A draft
    /// is drawn in the normal weight. Status line usage figures removed.
    #[test]
    fn a_dim_suggestion_is_an_empty_box_and_a_draft_is_not() {
        let fresh = include_str!(
            "../../../../../tests/fixtures/claude-tui-2.1.302/1-fresh-placeholder.txt"
        );
        assert!(
            fresh.contains("\x1b[2mTry \""),
            "the fixture keeps its colour codes"
        );
        assert_eq!(input_box(fresh), InputBox::Empty);
        let draft = include_str!("../../../../../tests/fixtures/claude-tui-2.1.302/2-draft.txt");
        assert_eq!(input_box(draft), InputBox::Holding);
        // The rule above and below are coloured; still rules.
        assert!(draft.contains("\x1b[38;5;244m─"), "{draft}");
    }

    #[test]
    fn colour_codes_are_read_not_matched() {
        let dim = Line::read("\x1b[39m❯\u{a0}\x1b[2mTry \"x\"\x1b[0m");
        assert_eq!(dim.plain, "❯\u{a0}Try \"x\"");
        assert_eq!(dim.bright, "❯\u{a0}");
        // 2 inside a colour is a value, not dim; 22 turns dim off.
        let colour = Line::read("\x1b[38;5;2mA\x1b[2mB\x1b[22mC\x1b[38;2;2;2;2mD");
        assert_eq!(
            (colour.plain.as_str(), colour.bright.as_str()),
            ("ABCD", "ACD")
        );
        // A hyperlink's OSC is dropped whole, with either terminator.
        let link = Line::read("\x1b]8;;https://x\x07go\x1b]8;;\x1b\\!");
        assert_eq!(link.plain, "go!");
        let answers = reconnect_answers("\x1b[2m  ⎿  \x1b[0mSuccessfully reconnected to ccnm\n");
        assert_eq!(answers.succeeded, 1);
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

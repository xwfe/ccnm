//! `ccnm internal session-hook`: what a remote Claude session tells ccnm
//! about its conversations, through Claude Code's own hooks.
//!
//! Two things need it:
//!
//! - **`--continue` (P101).** Every `/clear` starts a new conversation with
//!   a new id, and ccnm only ever gave Claude the first one. `--continue`
//!   used to carry on that first one, skipping everything after the last
//!   `/clear` -- on 2026-10-17 a whole day of it. `SessionStart` says which
//!   conversation is current, so each id is written down here as it begins.
//! - **Handing off to a fresh conversation (P102).** Compaction keeps a
//!   session going past its window, and each one summarises the summary
//!   before it. After each, the model is told how many there have been and
//!   when to suggest a fresh conversation; a handoff it ends a reply with is
//!   kept, and given to the next conversation when the person types
//!   `/clear`. ccnm never clears anything itself: whether the work is done
//!   is the person's call, and a cleared context does not come back.
//!
//! Measured on Claude Code 2.1.300 (docs/research/probes/p100-session-hooks.py):
//! `SessionStart` fires with `source` `startup`, `clear` (new `session_id`),
//! `compact` and, for `--resume <id> --fork-session --session-id <ours>`,
//! `fork`; its stdout reaches the model in the next request. `Stop` carries
//! the last reply as `last_assistant_message`.
//!
//! A hook that fails puts its stderr in front of the person, and nothing
//! here is worth that: every failure is silent, and the worst it costs is a
//! `--continue` that falls back to the session's first conversation, or a
//! handoff that is not carried over.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::protocol::payload::{self, Protocol};
use crate::session::Dir;

pub const PROTOCOL: u32 = 1;

/// The `--payload`: which session the hook belongs to. Claude Code passes
/// its own ids on stdin; this is ccnm's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    pub session_dir: PathBuf,
}

impl Protocol for Request {
    fn protocol(&self) -> u32 {
        self.protocol
    }
    fn expected_protocol(&self) -> u32 {
        PROTOCOL
    }
}

/// The command a settings.json hook runs: this binary, by absolute path
/// (the same one `mcp.json` starts the transport with), so no PATH is
/// involved, and the payload, which needs no quoting.
pub fn command(exe: &Path, dir: &Dir) -> Result<String> {
    let request = Request {
        protocol: PROTOCOL,
        session_dir: dir.path().to_path_buf(),
    };
    Ok(format!(
        "{} internal session-hook --payload {}",
        crate::mcp::hooks::shell_quote(&exe.to_string_lossy()),
        payload::encode(&request)?
    ))
}

/// From this many compactions on, the model is asked not to wait for the
/// task to be finished before suggesting a fresh conversation. Each one
/// summarises the one before; on 2026-10-15–17 a busy session compacted
/// every one to two and a half hours, so three is most of a working day in
/// one window. Lower means more interruptions, higher means more of the
/// conversation known only as a summary of summaries.
pub const HAND_OFF_AFTER: u32 = 3;

/// The line a handoff starts with. Plain text, so it reads naturally to the
/// person, and unlikely to start a line by accident.
pub const MARKER: &str = "[ccnm handoff]";

/// A handoff longer than this is cut: it goes into the next conversation's
/// first request, and a handoff is a page, not the conversation again.
const HANDOFF_MAX: usize = 32 * 1024;

/// Kept ids, oldest dropped first: one per `/clear`, so this is a lot of
/// clearing, and `--continue` only ever wants the last few.
const IDS_MAX: usize = 256;

/// Claude's conversations in one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversations {
    /// In the order they began: the one ccnm started (or forked into, for
    /// `--continue`), then one more after each `/clear`.
    #[serde(default)]
    pub ids: Vec<String>,
    /// How often the current one has been compacted. For a `--continue`
    /// this starts at the count of the conversation it carries on.
    #[serde(default)]
    pub compactions: u32,
}

pub fn read(dir: &Dir) -> Option<Conversations> {
    serde_json::from_slice(&fs::read(dir.conversations()).ok()?).ok()
}

fn write(dir: &Dir, conversations: &Conversations) -> Result<()> {
    let path = dir.conversations();
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, crate::session::pretty(conversations)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// A `--continue` session starts counting where the conversation it
/// carries on left off: the compactions are in what it inherits.
pub fn seed(dir: &Dir, compactions: u32) -> Result<()> {
    write(
        dir,
        &Conversations {
            ids: Vec::new(),
            compactions,
        },
    )
}

/// Claude Code's conversation ids are UUIDs; anything else is not written
/// down, since `--continue` will put it on a command line.
fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// What one hook call has to say to Claude Code on stdout. Never an error:
/// see the module comment.
pub fn handle(dir: &Dir, input: &str) -> String {
    // A Claude Code background agent is relaunched with this session's
    // --settings, so it runs these hooks too (P117). Its conversation is not
    // the session's: written down, it is what `-c` and the next machine
    // would pick, and its last reply would replace the session's handoff,
    // or delete it.
    if !crate::session::from_own_agent(dir) {
        return String::new();
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(input) else {
        return String::new();
    };
    let text = |key: &str| event.get(key).and_then(serde_json::Value::as_str);
    match text("hook_event_name") {
        Some("SessionStart") => session_start(dir, text("session_id"), text("source")),
        Some("Stop") => {
            if let Some(reply) = text("last_assistant_message") {
                keep_handoff(dir, reply);
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn session_start(dir: &Dir, id: Option<&str>, source: Option<&str>) -> String {
    let mut conversations = read(dir).unwrap_or_default();
    if let Some(id) = id.filter(|id| is_uuid(id))
        && conversations.ids.last().map(String::as_str) != Some(id)
    {
        conversations.ids.push(id.to_string());
        if conversations.ids.len() > IDS_MAX {
            conversations.ids.drain(..conversations.ids.len() - IDS_MAX);
        }
    }
    let mut say = Vec::new();
    match source {
        Some("compact") => {
            conversations.compactions = conversations.compactions.saturating_add(1);
            say.push(compacted(conversations.compactions));
        }
        // Carries on a conversation ccnm seeded the count of.
        Some("fork") => {}
        Some("clear") => {
            conversations.compactions = 0;
            if let Some(handoff) = take_handoff(dir) {
                say.push(format!(
                    "ccnm: the user typed /clear, and the conversation before it in this terminal ended with the handoff below. Pick the work up from it. If the user's next message asks for something else, follow the user.\n\n{handoff}"
                ));
            }
        }
        // `startup`, and `resume` into a conversation whose count nobody
        // kept. Only a session's very first conversation is handed what the
        // project's previous session left (P115).
        _ => {
            conversations.compactions = 0;
            if source == Some("startup")
                && let Some(carried) = crate::session::carry::take(dir)
            {
                say.push(crate::session::carry::message(&carried));
            }
        }
    }
    let _ = write(dir, &conversations);
    say.push(how_to_hand_off());
    say.join("\n\n")
}

fn compacted(times: u32) -> String {
    if times >= HAND_OFF_AFTER {
        format!(
            "ccnm: this conversation has now been compacted {times} times; each compaction summarised the one before, and detail from early on is gone. Do not wait for the task to be finished: at the next point where the work can be left safely (a check run, the changes listed or committed), stop, write the handoff below and ask the user to type /clear to go on in a fresh conversation. Tell them why in one sentence."
        )
    } else {
        let times = if times == 1 {
            "once".to_string()
        } else {
            format!("{times} times")
        };
        format!(
            "ccnm: this conversation has now been compacted {times}, so what came before is a summary. When the task the user gave you is finished and they move on to something else, do not start it here: write the handoff below and suggest they type /clear to go on in a fresh conversation."
        )
    }
}

/// Said at every start, and again after each compaction (the summary may
/// not keep it): what a handoff is and how it gets to the next
/// conversation.
fn how_to_hand_off() -> String {
    format!(
        "ccnm: when the user wants to move on to a fresh conversation, or you suggest one, end your reply with a handoff: a line that is exactly {MARKER}, then the goal; what is done (files changed, by workspace-relative path, and commits); what is not done or not verified; the exact commands that check the work; decisions and limits the user set; the next step. Then ask the user to type /clear, and ccnm gives the handoff to the new conversation. Only a handoff in your last reply is carried over."
    )
}

/// Keep the handoff a reply ends with, or forget an older one when the
/// last reply has none: only a handoff that was the last word is carried.
fn keep_handoff(dir: &Dir, reply: &str) {
    let Some(start) = reply
        .match_indices(MARKER)
        .map(|(at, _)| at)
        .find(|&at| at == 0 || reply[..at].ends_with('\n'))
    else {
        let _ = fs::remove_file(dir.handoff());
        return;
    };
    let mut handoff = reply[start..].trim_end().to_string();
    if handoff.len() > HANDOFF_MAX {
        let mut end = HANDOFF_MAX;
        while !handoff.is_char_boundary(end) {
            end -= 1;
        }
        handoff.truncate(end);
        handoff.push_str("\n[cut by ccnm at 32 KiB]");
    }
    let path = dir.handoff();
    let tmp = path.with_extension("md.tmp");
    if fs::write(&tmp, handoff).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

/// The kept handoff, once: given to one conversation and gone.
fn take_handoff(dir: &Dir) -> Option<String> {
    let text = fs::read_to_string(dir.handoff()).ok()?;
    let _ = fs::remove_file(dir.handoff());
    Some(text).filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;

    const FIRST: &str = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
    const SECOND: &str = "96654ec6-d565-4d6f-baf0-288f5b345639";

    fn dir(test: &str) -> (TestDir, Dir) {
        let path = std::env::temp_dir().join(format!("ccnm-hook-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        (TestDir::adopt(path.clone()), Dir::at(path))
    }

    /// The shape 2.1.300 sends, trimmed to what is read.
    fn start(id: &str, source: &str) -> String {
        serde_json::json!({"hook_event_name": "SessionStart", "session_id": id, "source": source, "cwd": "/x"})
            .to_string()
    }

    fn stop(reply: &str) -> String {
        serde_json::json!({"hook_event_name": "Stop", "session_id": FIRST, "stop_hook_active": false, "last_assistant_message": reply})
            .to_string()
    }

    /// P115: what the project's previous session left goes to the new
    /// session's first conversation, once -- not to one after `/clear`, nor
    /// to a resumed one, which already has its own history.
    #[test]
    fn what_the_previous_session_left_reaches_only_the_first_conversation() {
        use crate::protocol::run::{Carried, CarriedKind};
        let (_t, dir) = dir("carried");
        crate::session::carry::leave(
            &dir,
            &Carried {
                kind: CarriedKind::Handoff,
                from: "hpsrv".into(),
                session: "old".into(),
                written: 1,
                text: "[ccnm handoff]\nfinish the parser".into(),
            },
        );
        let resumed = handle(&dir, &start(FIRST, "resume"));
        assert!(!resumed.contains("finish the parser"), "{resumed}");
        let first = handle(&dir, &start(FIRST, "startup"));
        assert!(
            first.contains("previous session, on the AI machine hpsrv"),
            "{first}"
        );
        assert!(first.contains("finish the parser"), "{first}");
        assert!(
            first.contains(MARKER),
            "how to hand off is still said: {first}"
        );
        let again = handle(&dir, &start(SECOND, "clear"));
        assert!(!again.contains("finish the parser"), "{again}");
    }

    /// P117: a Claude Code background agent runs the session's hooks too.
    /// Its events change nothing and say nothing; the session's own still
    /// work. Told apart by who started the hook: the recorded Agent pid is
    /// this test's own pid, which is neither its parent nor its grandparent.
    #[cfg(unix)]
    #[test]
    fn a_background_agents_hooks_leave_the_session_alone() {
        use crate::protocol::run::{Carried, CarriedKind};
        let (_t, dir) = dir("background");
        handle(&dir, &start(FIRST, "startup"));
        handle(&dir, &stop("[ccnm handoff]\nGoal: the session's own."));
        crate::session::carry::leave(
            &dir,
            &Carried {
                kind: CarriedKind::Handoff,
                from: "a".into(),
                session: "old".into(),
                written: 1,
                text: "[ccnm handoff]\nnot for a background agent".into(),
            },
        );

        crate::session::write_agent_pid(&dir, std::process::id()).unwrap();
        assert_eq!(handle(&dir, &start(SECOND, "startup")), "");
        assert_eq!(handle(&dir, &stop("Background work done.")), "");
        handle(&dir, &stop("[ccnm handoff]\nGoal: the background agent's."));
        assert_eq!(read(&dir).unwrap().ids, [FIRST]);
        assert_eq!(
            fs::read_to_string(dir.handoff()).unwrap(),
            "[ccnm handoff]\nGoal: the session's own."
        );
        assert!(dir.carried().exists(), "left for the session, not taken");

        // The session's own Claude is this test's parent here.
        crate::session::write_agent_pid(&dir, std::os::unix::process::parent_id()).unwrap();
        handle(&dir, &start(SECOND, "clear"));
        assert_eq!(read(&dir).unwrap().ids, [FIRST, SECOND]);
    }

    /// P101: each conversation is written down as it begins, so
    /// `--continue` can find the last one rather than the first.
    #[test]
    fn every_conversation_is_written_down_as_it_begins() {
        let (_t, dir) = dir("ids");
        handle(&dir, &start(FIRST, "startup"));
        handle(&dir, &start(FIRST, "compact"));
        handle(&dir, &start(SECOND, "clear"));
        let kept = read(&dir).unwrap();
        assert_eq!(kept.ids, [FIRST, SECOND]);
        assert_eq!(kept.compactions, 0, "a new conversation after /clear");
    }

    /// What is not a UUID never reaches a command line, and input that is
    /// not Claude Code's changes nothing and says nothing.
    #[test]
    fn bad_input_is_ignored_silently() {
        let (_t, dir) = dir("bad");
        assert_eq!(handle(&dir, "not json"), "");
        assert_eq!(handle(&dir, "{}"), "");
        handle(&dir, &start("../../etc/passwd", "startup"));
        handle(
            &dir,
            &start("0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2dX", "clear"),
        );
        assert!(read(&dir).unwrap().ids.is_empty());
    }

    /// P102: after each compaction the model hears how many there have
    /// been, and from the third on, not to wait for the task to finish.
    #[test]
    fn compactions_are_counted_and_said() {
        let (_t, dir) = dir("compact");
        let first = handle(&dir, &start(FIRST, "startup"));
        assert!(first.contains(MARKER), "{first}");
        assert!(!first.contains("compacted"), "{first}");
        let once = handle(&dir, &start(FIRST, "compact"));
        assert!(once.contains("compacted once"), "{once}");
        assert!(
            once.contains("When the task the user gave you is finished"),
            "{once}"
        );
        assert!(once.contains(MARKER), "said again: the summary may drop it");
        handle(&dir, &start(FIRST, "compact"));
        let third = handle(&dir, &start(FIRST, "compact"));
        assert!(third.contains("compacted 3 times"), "{third}");
        assert!(
            third.contains("Do not wait for the task to be finished"),
            "{third}"
        );
        assert_eq!(read(&dir).unwrap().compactions, 3);
    }

    /// A `--continue` session carries on the count it was seeded with.
    #[test]
    fn a_fork_keeps_the_seeded_count() {
        let (_t, dir) = dir("fork");
        seed(&dir, 2).unwrap();
        handle(&dir, &start(FIRST, "fork"));
        let next = handle(&dir, &start(FIRST, "compact"));
        assert!(next.contains("compacted 3 times"), "{next}");
        assert_eq!(read(&dir).unwrap().ids, [FIRST]);
    }

    /// The handoff a conversation ends with goes to the next one, once.
    #[test]
    fn a_handoff_that_was_the_last_word_is_carried_over_once() {
        let (_t, dir) = dir("handoff");
        handle(&dir, &start(FIRST, "startup"));
        handle(
            &dir,
            &stop("Done with this part.\n\n[ccnm handoff]\nGoal: P102.\nNext: run the probe."),
        );
        let next = handle(&dir, &start(SECOND, "clear"));
        assert!(next.contains("the user typed /clear"), "{next}");
        assert!(
            next.contains("[ccnm handoff]\nGoal: P102.\nNext: run the probe."),
            "{next}"
        );
        assert!(!next.contains("Done with this part"), "{next}");
        assert!(!dir.handoff().exists(), "given once");
        let again = handle(&dir, &start(FIRST, "clear"));
        assert!(!again.contains("the user typed /clear"), "{again}");
    }

    /// A reply after the handoff replaces it; a marker inside a line is not
    /// a handoff.
    #[test]
    fn only_a_handoff_in_the_last_reply_counts() {
        let (_t, dir) = dir("stale");
        handle(&dir, &stop("[ccnm handoff]\nGoal: old."));
        assert!(dir.handoff().exists());
        handle(&dir, &stop("Sure, the test command is cargo test."));
        assert!(!dir.handoff().exists());
        handle(&dir, &stop("I would write [ccnm handoff] at the end."));
        assert!(!dir.handoff().exists());
        let next = handle(&dir, &start(SECOND, "clear"));
        assert!(!next.contains("the user typed /clear"), "{next}");
    }

    #[test]
    fn a_long_handoff_is_cut_on_a_character() {
        let (_t, dir) = dir("long");
        let body = "交".repeat(HANDOFF_MAX);
        handle(&dir, &stop(&format!("{MARKER}\n{body}")));
        let kept = fs::read_to_string(dir.handoff()).unwrap();
        assert!(kept.len() <= HANDOFF_MAX + 40, "{}", kept.len());
        assert!(kept.ends_with("[cut by ccnm at 32 KiB]"));
    }

    /// The command runs through `sh -c`, as Claude Code runs it: a path
    /// with a space and a quote still names the binary, and the payload
    /// decodes to this session.
    #[test]
    fn the_command_survives_a_shell() {
        let (_t, dir) = dir("command");
        let exe = Path::new("/Users/o'brien/my bin/ccnm");
        let line = command(exe, &dir).unwrap();
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("set -- {line}; printf '%s\\n' \"$@\"")])
            .output()
            .unwrap();
        let words: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(words[0], "/Users/o'brien/my bin/ccnm");
        assert_eq!(words[1..4], ["internal", "session-hook", "--payload"]);
        let request: Request = payload::decode(&words[4]).unwrap();
        assert_eq!(request.session_dir, dir.path());
    }
}

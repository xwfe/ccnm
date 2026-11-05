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
//! - **Handing off to a fresh conversation (P102, P120).** Compaction keeps
//!   a session going past its window, and each one summarises the summary
//!   before it. From the [`HAND_OFF_AFTER`]th on, ccnm tells the *person*,
//!   once per compaction, that a fresh conversation would help (P120). Until
//!   P120 the model was told to suggest it, and from the third compaction on
//!   it did so at every point the work could be left -- with a person who
//!   commits every small change, at every commit. A handoff the model ends
//!   a reply with (when asked for one) is kept, and given to the next
//!   conversation when the person types `/clear`; without one, a
//!   conversation compacted that often hands over its end. ccnm never
//!   clears anything itself: whether the work is done is the person's call,
//!   and a cleared context does not come back.
//!
//! Measured on Claude Code 2.1.300 (docs/research/probes/p100-session-hooks.py):
//! `SessionStart` fires with `source` `startup`, `clear` (new `session_id`),
//! `compact` and, for `--resume <id> --fork-session --session-id <ours>`,
//! `fork`; its stdout reaches the model in the next request. `Stop` carries
//! the last reply as `last_assistant_message`. On 2.1.318
//! (docs/research/probes/p120-hook-notices.py): stdout that is JSON with a
//! `systemMessage` shows that line to the person, as `SessionStart:compact
//! says: …` or `Stop says: …`, from both hooks; `SessionStart`'s
//! `hookSpecificOutput.additionalContext` reaches the model like plain
//! stdout; both hooks get `transcript_path`, and after `/clear` the new
//! conversation's transcript sits beside the old one's.
//!
//! A hook that fails puts its stderr in front of the person, and nothing
//! here is worth that: every failure is silent, and the worst it costs is a
//! `--continue` that falls back to the session's first conversation, or a
//! handoff that is not carried over.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Lang;
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

/// From this many compactions on, ccnm tells the person after each one that
/// a fresh conversation would help, and a `/clear` with no handoff hands the
/// new conversation the end of this one (P120). Each compaction summarises
/// the one before; on 2026-10-15–17 a busy session compacted every one to
/// two and a half hours, so three is most of a working day in one window.
/// Lower means more of those lines and more `/clear`s carrying text; higher
/// means more of the conversation known only as a summary of summaries.
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
    handle_in(dir, input, &person_lang)
}

/// [`handle`], with the language of what ccnm tells the person asked for
/// only when there is something to tell: reading the config is not worth
/// doing on every reply.
pub fn handle_in(dir: &Dir, input: &str, lang: &dyn Fn() -> Lang) -> String {
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
        Some("SessionStart") => session_start(
            dir,
            text("session_id"),
            text("source"),
            text("transcript_path"),
            lang,
        ),
        Some("Stop") => match text("last_assistant_message") {
            Some(reply) if keep_handoff(dir, reply) => {
                serde_json::json!({ "systemMessage": handoff_kept(lang()) }).to_string()
            }
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The language ccnm speaks to people on this machine: the config's
/// `[ui] lang`, as the CLI reads it, else Chinese. A hook gets no
/// `--lang`, and a config that cannot be read is no reason to stay silent.
fn person_lang() -> Lang {
    crate::paths::effective_config_path()
        .ok()
        .and_then(|path| crate::Config::load(&path).ok())
        .and_then(|config| config.ui.lang)
        .and_then(|text| Lang::parse(&text))
        .unwrap_or(Lang::Zh)
}

fn session_start(
    dir: &Dir,
    id: Option<&str>,
    source: Option<&str>,
    transcript: Option<&str>,
    lang: &dyn Fn() -> Lang,
) -> String {
    let mut conversations = read(dir).unwrap_or_default();
    // The conversation a `/clear` leaves: the last one written down.
    let previous = conversations.ids.last().cloned();
    if let Some(id) = id.filter(|id| is_uuid(id))
        && conversations.ids.last().map(String::as_str) != Some(id)
    {
        conversations.ids.push(id.to_string());
        if conversations.ids.len() > IDS_MAX {
            conversations.ids.drain(..conversations.ids.len() - IDS_MAX);
        }
    }
    let mut say = Vec::new();
    let mut notice = None;
    match source {
        Some("compact") => {
            conversations.compactions = conversations.compactions.saturating_add(1);
            say.push(compacted(conversations.compactions));
            if conversations.compactions >= HAND_OFF_AFTER {
                notice = Some(compacted_notice(conversations.compactions, lang()));
            }
        }
        // Carries on a conversation ccnm seeded the count of.
        Some("fork") => {}
        Some("clear") => {
            let compacted = std::mem::take(&mut conversations.compactions);
            if let Some(handoff) = take_handoff(dir) {
                say.push(format!(
                    "ccnm: the user typed /clear, and the conversation before it in this terminal ended with the handoff below. Pick the work up from it. If the user's next message asks for something else, follow the user.\n\n{handoff}"
                ));
                notice = Some(person_line(
                    Tone::Done,
                    lang().pick(
                        "交接已交给这段新对话",
                        "The handoff went to this new conversation",
                    ),
                    None,
                ));
            } else if compacted >= HAND_OFF_AFTER
                && let Some(tail) = previous_tail(transcript, previous.as_deref())
            {
                say.push(format!(
                    "ccnm: the user typed /clear after the conversation before it in this terminal had been compacted {compacted} times, and it ended without a handoff. Below is the end of that conversation: what the user and the AI said, without tool calls or their output. It is raw, not a summary: use it to learn where the work stood, check the project itself before relying on any of it (git status, the files it names), then carry on. If the user's next message asks for something else, follow the user.\n\n{tail}"
                ));
                let lang = lang();
                notice = Some(person_line(
                    Tone::Done,
                    lang.pick(
                        "上一段没写交接，已把它的结尾交给这段新对话",
                        "No handoff was written; the end of the last conversation went to this one",
                    ),
                    Some(lang.pick(
                        "那是原始记录，模型会先核对项目再用",
                        "It is the raw record; the model checks the project before relying on it",
                    )),
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
    let context = say.join("\n\n");
    match notice {
        // JSON only when there is a line for the person: plain stdout is
        // what every build before P120 printed, measured back to 2.1.300.
        Some(notice) => serde_json::json!({
            "systemMessage": notice,
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": context,
            },
        })
        .to_string(),
        None => context,
    }
}

/// What the model is told after a compaction: how many there have been,
/// and nothing about what to do with that. Until P120 it was also told
/// when to suggest a fresh conversation, and from the third on it did at
/// every stopping point; the person hears it from ccnm instead.
fn compacted(times: u32) -> String {
    if times == 1 {
        "ccnm: this conversation has now been compacted once, so what came before is a summary."
            .to_string()
    } else {
        format!(
            "ccnm: this conversation has now been compacted {times} times; each compaction summarised the one before, and detail from early on is gone."
        )
    }
}

/// The kind of line ccnm shows the person, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// Something for the person to do: yellow.
    Act,
    /// Something done for them: green.
    Done,
}

/// A line for the person, made to stand out (P121). Claude Code draws a
/// hook's `systemMessage` in grey (colour 246 on 2.1.318) after `… says:`,
/// which is easy to read past; ANSI codes in it are drawn as they are, and
/// the lines after the first stay grey. So: a coloured `ccnm` tag, the part
/// that matters in bold colour, the explanation under it in Claude Code's
/// own grey. 16-colour codes only, which every terminal tmux runs in has.
fn person_line(tone: Tone, head: &str, detail: Option<&str>) -> String {
    let (tag, text) = match tone {
        Tone::Act => ("\x1b[1;30;43m", "\x1b[1;33m"),
        Tone::Done => ("\x1b[1;30;42m", "\x1b[1;32m"),
    };
    let mut line = format!("{tag} ccnm \x1b[0m {text}{head}\x1b[0m");
    if let Some(detail) = detail {
        line.push('\n');
        line.push_str(detail);
    }
    line
}

/// What the person sees after a compaction, from the [`HAND_OFF_AFTER`]th
/// on.
fn compacted_notice(times: u32, lang: Lang) -> String {
    person_line(
        Tone::Act,
        &lang.pick(
            format!("这段对话已压缩 {times} 次，建议敲 /clear 开新对话"),
            format!("Compacted {times} times: type /clear for a fresh conversation"),
        ),
        Some(lang.pick(
            "早期细节只剩摘要。/clear 会把这段对话的结尾带过去；要完整交接，先说“写交接”",
            "Early detail is only a summary now. /clear hands the new one the end of this conversation; for a full handoff, ask for one first",
        )),
    )
}

/// What the person sees when the last reply ended with a handoff.
fn handoff_kept(lang: Lang) -> String {
    person_line(
        Tone::Done,
        lang.pick(
            "交接已记下，敲 /clear 开新对话接着做",
            "Handoff kept: type /clear to go on in a fresh conversation",
        ),
        Some(lang.pick(
            "在这里接着聊的话，这份交接就作废",
            "Carry on here instead and it is dropped",
        )),
    )
}

/// Said at every start, and again after each compaction (the summary may
/// not keep it): when to write a handoff, its one shape, and how it gets to
/// the next conversation.
///
/// The fields are a compaction summary's Status block (goal, done, in
/// progress, blocked, next; see `COMPACT_INSTRUCTIONS`) plus the two things
/// a fresh conversation cannot find out on its own: how to check the work,
/// and what the user decided. One shape, so the next model knows where to
/// look and the person can skim it; lines, not prose, so it stays short.
fn how_to_hand_off() -> String {
    format!(
        "ccnm: write a handoff only when the user asks for one or asks to go on in a fresh conversation. Do not suggest either yourself: ccnm tells the user when the conversation has been compacted often enough for it to help. A handoff ends your reply, in exactly this shape:

{MARKER}
Goal: the task, in one sentence.
Done: what changed, by workspace-relative path; commits (short hash and subject); what is still uncommitted.
In progress: what you were doing and how far it got.
Blocked: what waits on the user or on something else, and what would unblock it.
Check: the exact commands that verify the work, and what still fails (test names, the decisive error line).
Keep: the user's decisions and limits that still hold, close to their words.
Next: the first step, as a command or tool call.

Each field on its own line, a list one item per line under it. Facts, not narrative; paths and commands exactly as they are; no code or long output, the files are in the project. Write none for an empty field. At most 40 lines, in the language the user writes in. Only a handoff in your last reply is carried over: ccnm keeps it and gives it to the new conversation when the user types /clear."
    )
}

/// Keep the handoff a reply ends with, or forget an older one when the
/// last reply has none: only a handoff that was the last word is carried.
/// Whether one was kept.
fn keep_handoff(dir: &Dir, reply: &str) -> bool {
    let Some(start) = reply
        .match_indices(MARKER)
        .map(|(at, _)| at)
        .find(|&at| at == 0 || reply[..at].ends_with('\n'))
    else {
        let _ = fs::remove_file(dir.handoff());
        return false;
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
    fs::write(&tmp, handoff).is_ok() && fs::rename(&tmp, &path).is_ok()
}

/// The kept handoff, once: given to one conversation and gone.
fn take_handoff(dir: &Dir) -> Option<String> {
    let text = fs::read_to_string(dir.handoff()).ok()?;
    let _ = fs::remove_file(dir.handoff());
    Some(text).filter(|text| !text.trim().is_empty())
}

/// The end of the conversation a `/clear` left, from its transcript: the
/// one named by its id, beside the new conversation's (2.1.318 keeps both
/// in the directory of the session's cwd). An id that is not a UUID never
/// becomes a path.
fn previous_tail(transcript: Option<&str>, previous: Option<&str>) -> Option<String> {
    let previous = previous.filter(|id| is_uuid(id))?;
    let path = Path::new(transcript?)
        .parent()?
        .join(format!("{previous}.jsonl"));
    let file = fs::File::open(path).ok()?;
    crate::session::carry::conversation_tail(std::io::BufReader::new(file))
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

    /// A `SessionStart` as 2.1.318 sends it, with the transcript of the
    /// conversation that is starting.
    fn start_at(id: &str, source: &str, transcript: &Path) -> String {
        serde_json::json!({"hook_event_name": "SessionStart", "session_id": id, "source": source, "cwd": "/x", "transcript_path": transcript})
            .to_string()
    }

    /// The hook, with the person's language fixed: [`handle`] would read
    /// this machine's config for it.
    fn hook(dir: &Dir, input: &str) -> String {
        handle_in(dir, input, &|| Lang::En)
    }

    /// What reaches the model: plain stdout, or the JSON's context.
    fn model_text(out: &str) -> String {
        match serde_json::from_str::<serde_json::Value>(out) {
            Ok(v) if v.is_object() => v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            _ => out.to_string(),
        }
    }

    /// The line the person sees, if any.
    fn notice(out: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(out)
            .ok()?
            .get("systemMessage")?
            .as_str()
            .map(str::to_string)
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
        let resumed = hook(&dir, &start(FIRST, "resume"));
        assert!(!resumed.contains("finish the parser"), "{resumed}");
        let first = hook(&dir, &start(FIRST, "startup"));
        assert!(
            first.contains("previous session, on the AI machine hpsrv"),
            "{first}"
        );
        assert!(first.contains("finish the parser"), "{first}");
        assert!(
            first.contains(MARKER),
            "how to hand off is still said: {first}"
        );
        let again = hook(&dir, &start(SECOND, "clear"));
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
        hook(&dir, &start(FIRST, "startup"));
        hook(&dir, &stop("[ccnm handoff]\nGoal: the session's own."));
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
        assert_eq!(hook(&dir, &start(SECOND, "startup")), "");
        assert_eq!(hook(&dir, &stop("Background work done.")), "");
        hook(&dir, &stop("[ccnm handoff]\nGoal: the background agent's."));
        assert_eq!(read(&dir).unwrap().ids, [FIRST]);
        assert_eq!(
            fs::read_to_string(dir.handoff()).unwrap(),
            "[ccnm handoff]\nGoal: the session's own."
        );
        assert!(dir.carried().exists(), "left for the session, not taken");

        // The session's own Claude is this test's parent here.
        crate::session::write_agent_pid(&dir, std::os::unix::process::parent_id()).unwrap();
        hook(&dir, &start(SECOND, "clear"));
        assert_eq!(read(&dir).unwrap().ids, [FIRST, SECOND]);
    }

    /// P101: each conversation is written down as it begins, so
    /// `--continue` can find the last one rather than the first.
    #[test]
    fn every_conversation_is_written_down_as_it_begins() {
        let (_t, dir) = dir("ids");
        hook(&dir, &start(FIRST, "startup"));
        hook(&dir, &start(FIRST, "compact"));
        hook(&dir, &start(SECOND, "clear"));
        let kept = read(&dir).unwrap();
        assert_eq!(kept.ids, [FIRST, SECOND]);
        assert_eq!(kept.compactions, 0, "a new conversation after /clear");
    }

    /// What is not a UUID never reaches a command line, and input that is
    /// not Claude Code's changes nothing and says nothing.
    #[test]
    fn bad_input_is_ignored_silently() {
        let (_t, dir) = dir("bad");
        assert_eq!(hook(&dir, "not json"), "");
        assert_eq!(hook(&dir, "{}"), "");
        hook(&dir, &start("../../etc/passwd", "startup"));
        handle(
            &dir,
            &start("0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2dX", "clear"),
        );
        assert!(read(&dir).unwrap().ids.is_empty());
    }

    /// P102, P120: after each compaction the model hears how many there
    /// have been; from the third on the person gets one line, once per
    /// compaction, and stdout turns into JSON to carry it.
    #[test]
    fn compactions_are_counted_and_said() {
        let (_t, dir) = dir("compact");
        let first = hook(&dir, &start(FIRST, "startup"));
        assert!(first.contains(MARKER), "{first}");
        assert!(!first.contains("has now been compacted"), "{first}");
        let once = hook(&dir, &start(FIRST, "compact"));
        assert!(once.contains("compacted once"), "{once}");
        assert_eq!(notice(&once), None, "plain stdout, as before P120: {once}");
        assert!(once.contains(MARKER), "said again: the summary may drop it");
        let twice = hook(&dir, &start(FIRST, "compact"));
        assert_eq!(notice(&twice), None, "{twice}");
        for times in [3, 4] {
            let out = handle_in(&dir, &start(FIRST, "compact"), &|| Lang::Zh);
            let said = notice(&out).expect("a line for the person");
            assert!(
                said.contains(&format!("压缩 {times} 次")) && said.contains("/clear"),
                "{said}"
            );
            let model = model_text(&out);
            assert!(
                model.contains(&format!("compacted {times} times")),
                "{model}"
            );
            assert!(model.contains(MARKER), "{model}");
        }
        assert_eq!(read(&dir).unwrap().compactions, 4);
    }

    /// P120: nothing the model hears tells it to suggest a fresh
    /// conversation on its own. Before P120 the first compaction told it to
    /// at the end of every task, and the third at every stopping point.
    #[test]
    fn the_model_is_not_told_to_suggest_a_fresh_conversation() {
        let (_t, dir) = dir("no-nagging");
        let first = handle(&dir, &start(FIRST, "startup"));
        let once = handle(&dir, &start(FIRST, "compact"));
        for said in [&first, &once] {
            assert!(!said.contains("suggest they type /clear"), "{said}");
            assert!(
                !said.contains("When the task the user gave you is finished"),
                "{said}"
            );
            assert!(said.contains("only when the user asks"), "{said}");
        }
    }

    /// P120: a conversation compacted often enough, cleared without a
    /// handoff, hands the new one its end -- read from its own transcript,
    /// beside the new one's -- and the person is told; one compacted less
    /// hands over nothing, as before.
    #[test]
    fn a_clear_hands_over_the_end_of_a_long_conversation() {
        let (_t, dir) = dir("tail");
        let projects = dir.path().join("projects/-x");
        fs::create_dir_all(&projects).unwrap();
        let line = |v: serde_json::Value| v.to_string();
        let transcript = [
            line(serde_json::json!({"type": "user", "message": {"content": "fix the parser"}})),
            line(
                serde_json::json!({"type": "assistant", "message": {"content": [
                    {"type": "text", "text": "Parser fixed; tests 4 and 5 still fail."}
                ]}}),
            ),
        ]
        .join("\n");
        fs::write(projects.join(format!("{FIRST}.jsonl")), transcript).unwrap();
        let cleared = projects.join(format!("{SECOND}.jsonl"));

        hook(&dir, &start(FIRST, "startup"));
        hook(&dir, &start(FIRST, "compact"));
        hook(&dir, &start(FIRST, "compact"));
        let short = hook(&dir, &start_at(SECOND, "clear", &cleared));
        assert!(!short.contains("tests 4 and 5"), "{short}");
        assert_eq!(notice(&short), None, "{short}");

        hook(
            &dir,
            &start_at(FIRST, "startup", &projects.join(format!("{FIRST}.jsonl"))),
        );
        for _ in 0..3 {
            hook(&dir, &start(FIRST, "compact"));
        }
        let out = hook(&dir, &start_at(SECOND, "clear", &cleared));
        let model = model_text(&out);
        assert!(model.contains("compacted 3 times"), "{model}");
        assert!(
            model.contains("[AI]\nParser fixed; tests 4 and 5 still fail."),
            "{model}"
        );
        assert!(model.contains("raw, not a summary"), "{model}");
        assert!(
            notice(&out).is_some_and(|n| n.contains("end of the last conversation")),
            "{out}"
        );
        assert_eq!(read(&dir).unwrap().compactions, 0);
    }

    /// P121: a handoff has one shape, in the terms a compaction summary's
    /// Status block uses, and it is asked for, not offered.
    #[test]
    fn the_handoff_has_one_shape() {
        let said = how_to_hand_off();
        assert!(said.contains(&format!("\n{MARKER}\nGoal:")), "{said}");
        for field in [
            "\nGoal: ",
            "\nDone: ",
            "\nIn progress: ",
            "\nBlocked: ",
            "\nCheck: ",
            "\nKeep: ",
            "\nNext: ",
        ] {
            assert!(said.contains(field), "{field:?} missing: {said}");
        }
        assert!(said.contains("At most 40 lines"), "{said}");
        assert!(said.contains("only when the user asks"), "{said}");
    }

    /// P121: what ccnm tells the person stands out from Claude Code's grey:
    /// a coloured tag, the part that matters in bold colour, the reason on
    /// a line of its own.
    #[test]
    fn lines_for_the_person_stand_out() {
        let compacted = compacted_notice(3, Lang::Zh);
        assert!(
            compacted.starts_with("\x1b[1;30;43m ccnm \x1b[0m \x1b[1;33m这段对话已压缩 3 次"),
            "{compacted:?}"
        );
        let (head, detail) = compacted.split_once('\n').expect("two lines");
        assert!(
            head.ends_with("\x1b[0m") && head.contains("/clear"),
            "{head:?}"
        );
        assert!(
            detail.contains("写交接") && !detail.contains('\x1b'),
            "{detail:?}"
        );
        let kept = handoff_kept(Lang::En);
        assert!(kept.starts_with("\x1b[1;30;42m ccnm"), "{kept:?}");
        assert!(kept.contains("Handoff kept"), "{kept:?}");
        assert_eq!(kept.lines().count(), 2, "{kept:?}");
        let done = person_line(Tone::Done, "x", None);
        assert_eq!(done, "\x1b[1;30;42m ccnm \x1b[0m \x1b[1;32mx\x1b[0m");
    }

    /// P120: the person sees one line when a reply ends with a handoff,
    /// and nothing otherwise.
    #[test]
    fn a_kept_handoff_is_announced() {
        let (_t, dir) = dir("announced");
        let out = handle_in(&dir, &stop("Done.\n\n[ccnm handoff]\nGoal: x."), &|| {
            Lang::Zh
        });
        assert!(
            notice(&out).is_some_and(|n| n.contains("交接已记下") && n.contains("/clear")),
            "{out}"
        );
        assert_eq!(hook(&dir, &stop("Sure.")), "");
        assert!(!dir.handoff().exists());
    }

    /// A `--continue` session carries on the count it was seeded with.
    #[test]
    fn a_fork_keeps_the_seeded_count() {
        let (_t, dir) = dir("fork");
        seed(&dir, 2).unwrap();
        hook(&dir, &start(FIRST, "fork"));
        let next = hook(&dir, &start(FIRST, "compact"));
        assert!(model_text(&next).contains("compacted 3 times"), "{next}");
        assert_eq!(read(&dir).unwrap().ids, [FIRST]);
    }

    /// The handoff a conversation ends with goes to the next one, once.
    #[test]
    fn a_handoff_that_was_the_last_word_is_carried_over_once() {
        let (_t, dir) = dir("handoff");
        hook(&dir, &start(FIRST, "startup"));
        handle(
            &dir,
            &stop("Done with this part.\n\n[ccnm handoff]\nGoal: P102.\nNext: run the probe."),
        );
        let out = hook(&dir, &start(SECOND, "clear"));
        let next = model_text(&out);
        assert!(next.contains("the user typed /clear"), "{next}");
        assert!(
            next.contains("[ccnm handoff]\nGoal: P102.\nNext: run the probe."),
            "{next}"
        );
        assert!(!next.contains("Done with this part"), "{next}");
        assert!(
            notice(&out).is_some_and(|n| n.contains("handoff went")),
            "{out}"
        );
        assert!(!dir.handoff().exists(), "given once");
        let again = hook(&dir, &start(FIRST, "clear"));
        assert!(!again.contains("the user typed /clear"), "{again}");
    }

    /// A reply after the handoff replaces it; a marker inside a line is not
    /// a handoff.
    #[test]
    fn only_a_handoff_in_the_last_reply_counts() {
        let (_t, dir) = dir("stale");
        hook(&dir, &stop("[ccnm handoff]\nGoal: old."));
        assert!(dir.handoff().exists());
        hook(&dir, &stop("Sure, the test command is cargo test."));
        assert!(!dir.handoff().exists());
        hook(&dir, &stop("I would write [ccnm handoff] at the end."));
        assert!(!dir.handoff().exists());
        let next = hook(&dir, &start(SECOND, "clear"));
        assert!(!next.contains("the user typed /clear"), "{next}");
    }

    #[test]
    fn a_long_handoff_is_cut_on_a_character() {
        let (_t, dir) = dir("long");
        let body = "交".repeat(HANDOFF_MAX);
        hook(&dir, &stop(&format!("{MARKER}\n{body}")));
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

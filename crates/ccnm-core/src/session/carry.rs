//! What a new session is handed from the project's previous one (P115).
//!
//! The previous session may be on another AI machine -- the point of `--on`
//! is to move to one when a login runs out -- so the conversation itself
//! cannot follow: another login has no cache of it, and Claude and Codex
//! cannot read each other's. What follows is text. Best is the handoff the
//! last reply ended with (P102); a session whose login ran out never got to
//! write one, and then the tail of what the person and the AI said is the
//! next best thing, marked as raw so the model checks before it trusts it.

use std::fs;

use crate::protocol::run::{Carried, CarriedKind};
use crate::session::Dir;

/// The most text carried, the same bound a handoff has (P102): it goes
/// into the new session's first request, and is a page, not a history.
pub const MAX: usize = 32 * 1024;

/// What the person and the AI said in a Claude transcript, oldest first,
/// cut from the front to [`MAX`]. Tool calls, their output and the model's
/// thinking are left out: they are most of a transcript, and the files they
/// were about are still in the project to look at.
///
/// Claude Code's own format, read as measured on 2.1.303 (P115): a typed
/// prompt is a `user` entry whose content is a string (or `text` blocks);
/// a reply is the `text` blocks of an `assistant` entry. Lines that do not
/// parse, and entries of any other type, are skipped.
///
/// Read a line at a time, keeping only as many turns as fit: a transcript
/// also holds every request Claude sent (`api-request-blob`), and a long
/// session's runs to tens of megabytes.
pub fn conversation_tail(transcript: impl std::io::BufRead) -> Option<String> {
    let mut turns = std::collections::VecDeque::new();
    let mut size = 0;
    let mut cut = false;
    for line in transcript.lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if entry
            .get("isSidechain")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            || entry.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true)
        {
            continue;
        }
        let who = match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "AI",
            _ => continue,
        };
        let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let text = match content {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let text = text.trim();
        // Claude Code's own bookkeeping, typed as the user: slash commands
        // and what they printed.
        if text.is_empty() || text.starts_with("<command-") || text.starts_with("<local-command-") {
            continue;
        }
        let turn = format!("[{who}]\n{text}");
        size += turn.len() + 2;
        turns.push_back(turn);
        // Whole turns go while the rest still fills the bound; the one
        // left at the front is cut inside below.
        while turns.len() > 1 && size - turns[0].len() - 2 >= MAX {
            size -= turns.pop_front().map_or(0, |t| t.len() + 2);
            cut = true;
        }
    }
    if turns.is_empty() {
        return None;
    }
    let all = Vec::from(turns).join("\n\n");
    if all.len() <= MAX && !cut {
        return Some(all);
    }
    let mut start = all.len().saturating_sub(MAX);
    while !all.is_char_boundary(start) {
        start += 1;
    }
    Some(format!("[earlier part cut by ccnm]\n{}", &all[start..]))
}

/// What the new session's model is told, with the text.
pub fn message(carried: &Carried) -> String {
    let from = if carried.from.is_empty() {
        "an AI machine".to_string()
    } else {
        format!("the AI machine {}", carried.from)
    };
    match carried.kind {
        CarriedKind::Handoff => format!(
            "ccnm: this project's previous session, on {from}, ended with the handoff below. Pick the work up from it. If the user's first message asks for something else, follow the user.\n\n{}",
            carried.text
        ),
        CarriedKind::Conversation => format!(
            "ccnm: this project's previous session, on {from}, ended without a handoff -- its login may have run out. Below is the end of that conversation: what the user and the AI said, without tool calls or their output. It is raw, not a summary: use it to learn where the work stood, check the project itself before relying on any of it (git status, the files it names), then carry on. If the user's first message asks for something else, follow the user.\n\n{}",
            carried.text
        ),
    }
}

/// Leave `carried` for this session's first conversation. A session that
/// cannot be given it starts without it, which is not worth failing over.
pub fn leave(dir: &Dir, carried: &Carried) {
    let path = dir.carried();
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(carried)
        && fs::write(&tmp, text).is_ok()
    {
        let _ = fs::rename(&tmp, &path);
    }
}

/// What this session was left, once: the first conversation takes it.
pub fn take(dir: &Dir) -> Option<Carried> {
    let text = fs::read_to_string(dir.carried()).ok()?;
    let _ = fs::remove_file(dir.carried());
    serde_json::from_str::<Carried>(&text)
        .ok()
        .filter(|c| !c.text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: serde_json::Value) -> String {
        value.to_string()
    }

    #[test]
    fn only_what_the_person_and_the_ai_said_is_kept() {
        let transcript = [
            line(serde_json::json!({"type": "user", "message": {"content": "fix the parser test"}})),
            line(serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "secret plan"},
                {"type": "text", "text": "Looking at it."},
                {"type": "tool_use", "name": "read_file", "input": {}}
            ]}})),
            line(serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "content": "file body"}
            ]}})),
            line(serde_json::json!({"type": "user", "message": {"content": "<command-name>/clear</command-name>"}})),
            line(serde_json::json!({"type": "user", "isSidechain": true, "message": {"content": "sub-agent prompt"}})),
            line(serde_json::json!({"type": "api-request-blob", "message": {"content": "request"}})),
            "not json".to_string(),
            line(serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "Fixed; cargo test passes."}
            ]}})),
        ]
        .join("\n");
        assert_eq!(
            conversation_tail(transcript.as_bytes()).unwrap(),
            "[user]\nfix the parser test\n\n[AI]\nLooking at it.\n\n[AI]\nFixed; cargo test passes."
        );
    }

    #[test]
    fn nothing_said_is_nothing_to_carry() {
        let transcript = line(
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "x", "input": {}}
            ]}}),
        );
        assert_eq!(conversation_tail(transcript.as_bytes()), None);
        assert_eq!(conversation_tail("".as_bytes()), None);
    }

    /// The end is what matters -- where the work stood -- so a long
    /// conversation loses its beginning, at a character, and says so.
    #[test]
    fn a_long_conversation_keeps_its_end() {
        let said = "改".repeat(20_000);
        let transcript = [
            line(serde_json::json!({"type": "user", "message": {"content": said}})),
            line(serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "the last word"}]}})),
        ]
        .join("\n");
        let tail = conversation_tail(transcript.as_bytes()).unwrap();
        assert!(tail.starts_with("[earlier part cut by ccnm]\n"));
        assert!(tail.ends_with("[AI]\nthe last word"));
        assert!(tail.len() <= MAX + "[earlier part cut by ccnm]\n".len());
    }

    #[test]
    fn many_turns_keep_the_newest_whole_ones() {
        let transcript = (0..5000)
            .map(|i| line(serde_json::json!({"type": "user", "message": {"content": format!("turn {i:05} {}", "x".repeat(80))}})))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = conversation_tail(transcript.as_bytes()).unwrap();
        assert!(
            tail.starts_with("[earlier part cut by ccnm]\n"),
            "{}",
            &tail[..80]
        );
        assert!(tail.ends_with(&format!("turn 04999 {}", "x".repeat(80))));
        assert!(!tail.contains("turn 00000"));
        assert!(tail.len() <= MAX + "[earlier part cut by ccnm]\n".len());
    }

    #[test]
    fn the_model_is_told_which_kind_it_got() {
        let mut carried = Carried {
            kind: CarriedKind::Handoff,
            from: "hpsrv".into(),
            session: "s".into(),
            written: 0,
            text: "[ccnm handoff]\ngoal".into(),
        };
        let said = message(&carried);
        assert!(said.contains("on the AI machine hpsrv, ended with the handoff below"));
        assert!(said.ends_with("[ccnm handoff]\ngoal"));
        carried.kind = CarriedKind::Conversation;
        let said = message(&carried);
        assert!(said.contains("ended without a handoff"));
        assert!(said.contains("check the project itself"));
    }

    #[test]
    fn what_is_left_is_taken_once() {
        let path = std::env::temp_dir().join(format!("ccnm-carry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        let _cleanup = ccnm_testdir::TestDir::adopt(path.clone());
        let dir = Dir::at(&path);
        let carried = Carried {
            kind: CarriedKind::Conversation,
            from: "agent".into(),
            session: "s1".into(),
            written: 5,
            text: "[user]\nhi".into(),
        };
        leave(&dir, &carried);
        assert_eq!(take(&dir), Some(carried));
        assert_eq!(take(&dir), None);
    }
}

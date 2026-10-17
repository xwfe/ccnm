//! `read_output`: page through what a command wrote.
//!
//! The last of the seven core tools. `exec_command` returns the head and
//! the tail of its output and keeps all of it on the Runtime Node; this is
//! how the middle is reached.
//!
//! Offsets are byte offsets into the retained file and they are stable,
//! because the file is only ever appended to: offset 4096 means the same
//! thing an hour later. That property is what makes paging cheap — no
//! cursor to keep, nothing re-sent.
//!
//! A foreground command's file is finished before its reference exists. A
//! background command's (P41) is not: reading up to the end of it while it
//! runs is "so far", not the end, and the page says which, with a line on
//! how the command is doing from its [`jobs::Status`].
//!
//! The reference is not a path. It is matched against the shape
//! `exec_command` generates and then joined to *this session's* retention
//! directory, so there is nothing to traverse with: a `..` or a slash
//! fails the shape check before it reaches the filesystem.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mcp::jobs;
use crate::mcp::retention;

/// Bytes returned when the caller does not say.
pub const DEFAULT_LIMIT: usize = 16 * 1024;
/// Ceiling on `limit` (docs/protocol/remote-workspace-mcp-v1.md section 8).
pub const MAX_LIMIT: usize = 32 * 1024;
/// Ceiling on `wait_ms`: the same ten minutes a command may take in the
/// foreground. Claude Code's MCP idle timeout for a stdio server is 30
/// minutes (2.1.273). Codex 0.154.0 gives up on any call after 300 s unless
/// the server's `tool_timeout_sec` says longer, which a managed session's
/// launch does (`provider::codex::TOOL_TIMEOUT`, P80); a Codex Host on
/// `ccnm mcp bridge` has to set it itself.
pub const MAX_WAIT_MS: u64 = 600_000;

/// Arguments of `read_output`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadOutputArgs {
    /// The `output_ref` exec_command returned.
    pub output_ref: String,
    /// Which stream. Default "stdout".
    #[serde(default)]
    pub stream: Option<Stream>,
    /// Byte offset to start at. Default 0. Offsets are stable: a finished
    /// command's output never changes.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Bytes to return at most. Default 16384, max 32768.
    #[serde(default)]
    #[schemars(range(min = 1, max = 32_768))]
    pub limit: Option<u32>,
    /// For a command running in the background: wait up to this long for it
    /// to finish before reading. Default 0, max 600000.
    #[serde(default)]
    #[schemars(range(min = 0, max = 600_000))]
    pub wait_ms: Option<u64>,
    /// Anything this tool does not declare: reported back, not obeyed. See
    /// [`crate::mcp::Ignored`].
    #[serde(flatten)]
    pub ignored: crate::mcp::Ignored,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    #[default]
    Stdout,
    Stderr,
}

impl Stream {
    fn file(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// One page of a command's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPage {
    #[serde(skip)]
    pub text: String,
    pub output_ref: String,
    pub stream: Stream,
    pub offset: u64,
    /// Bytes of output in `text`.
    pub bytes: usize,
    pub total_bytes: u64,
    /// Where to continue. Absent at the end of the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
    /// At the end of the stream, and nothing more will be written to it.
    pub eof: bool,
    /// A background command that is still running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Read a slice of a retained stream.
pub fn read_output(session_dir: &Path, args: &ReadOutputArgs) -> Result<OutputPage> {
    let reference = validate_ref(&args.output_ref)?;
    let stream = args.stream.unwrap_or_default();
    let limit = match args.limit {
        Some(0) => return Err(Error::invalid_args("limit must be at least 1")),
        Some(n) => (n as usize).min(MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let offset = args.offset.unwrap_or(0);

    let run = session_dir.join(&reference);
    let mut status = jobs::Status::read(&run);
    let mut running = false;
    if status.as_ref().is_some_and(|status| status.ended.is_none()) {
        running = retention::in_progress(&run);
        if !running {
            // It may have ended between the two looks; its final status is
            // written before it lets go of the run.
            status = jobs::Status::read(&run);
        }
    }

    let path = run.join(stream.file());
    let meta = std::fs::metadata(&path).map_err(|e| {
        Error::invalid_args(format!(
            "no output kept for {reference}; a command's output is kept for a while, not forever"
        ))
        .with_source(e)
    })?;
    let total_bytes = meta.len();
    if offset > total_bytes {
        return Err(Error::invalid_args(format!(
            "offset {offset} is past the end of {} ({total_bytes} bytes)",
            stream.file()
        )));
    }

    // Read from up to three bytes before `offset`, to see whether it is
    // inside a character, and three past the page, so that moving the
    // start forward still leaves a page of `limit` bytes.
    let back = offset.min(MAX_CONTINUATION as u64);
    let mut file = std::fs::File::open(&path)
        .map_err(|e| Error::internal("cannot open retained output").with_source(e))?;
    file.seek(SeekFrom::Start(offset - back))
        .map_err(|e| Error::internal("cannot seek retained output").with_source(e))?;
    let mut buf = vec![0u8; back as usize + MAX_CONTINUATION + limit];
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(Error::internal("cannot read retained output").with_source(e)),
        }
    }
    buf.truncate(filled);
    let (before, rest) = buf.split_at((back as usize).min(filled));

    let mut notes: Vec<String> = Vec::new();
    let skip = inside_character(before, rest);
    let start = offset + skip as u64;
    if skip > 0 {
        notes.push(format!(
            "offset {offset} is inside a character; this page starts at offset {start}, where the next one begins"
        ));
    }
    let page = &rest[skip..rest.len().min(skip + limit)];

    let at_end = start + page.len() as u64 >= total_bytes;
    // A finished stream is shown to its last byte. Anywhere else, a
    // character the page cuts in half -- or a running command is halfway
    // through writing -- is left whole for the next read, so every page
    // seam is a character boundary and the next offset is never inside one.
    let end = if at_end && !running {
        page.len()
    } else {
        page.len() - unfinished_tail(page)
    };
    if end == 0 && !page.is_empty() && !(at_end && running) {
        return Err(Error::invalid_args(format!(
            "limit {limit} is too small to hold one character at this offset"
        )));
    }
    let text = match std::str::from_utf8(&page[..end]) {
        Ok(text) => text.to_string(),
        Err(_) => {
            notes.push("this stream is not valid UTF-8; invalid bytes were replaced".into());
            String::from_utf8_lossy(&page[..end]).into_owned()
        }
    };
    let next = start + end as u64;
    let eof = next >= total_bytes && !running;

    let footer = if eof {
        format!("[end of {} at {total_bytes} bytes]", stream.file())
    } else if next >= total_bytes {
        format!(
            "[{total_bytes} bytes so far and the command is still running; read again from offset={next}, with wait_ms to wait for it]"
        )
    } else {
        format!("[{next} of {total_bytes} bytes; continue with offset={next}]")
    };
    let mut rendered = text.clone();
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str(&footer);
    if let Some(status) = &status {
        rendered.push_str(&format!("\n[{}]", status.describe(running)));
    }
    for note in &notes {
        rendered.push_str("\n[");
        rendered.push_str(note);
        rendered.push(']');
    }

    Ok(OutputPage {
        text: rendered,
        output_ref: reference,
        stream,
        offset: start,
        bytes: text.len(),
        total_bytes,
        next_offset: (!eof).then_some(next),
        eof,
        running,
        notes,
    })
}

/// The reference must be exactly what `exec_command` produces. Matching
/// the shape rather than sanitizing means there is no path to traverse:
/// a slash, a dot or a `..` never gets as far as being joined to
/// anything.
pub(crate) fn validate_ref(raw: &str) -> Result<String> {
    let reference = raw.trim();
    let ok = reference.len() == 18
        && reference.starts_with("r-")
        && reference[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if !ok {
        return Err(Error::invalid_args(format!(
            "{reference} is not an output_ref; pass the one exec_command returned"
        )));
    }
    Ok(reference.to_string())
}

/// The most continuation bytes a UTF-8 character has.
const MAX_CONTINUATION: usize = 3;

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// How many bytes a character starting with `lead` has; 0 for a byte no
/// character starts with.
fn width(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}

/// How many bytes of `after` to skip so a page starts on a character:
/// non-zero only when the offset between `before` and `after` is inside a
/// character whose lead byte is in `before`.
///
/// Before P103 the page started where it was asked to. Inside a character
/// its first byte is a continuation byte, and the tail check below found no
/// valid prefix at all and refused a 4000-byte page as too small. A
/// continuation byte with no lead within reach is skipped by nothing: it is
/// a bad byte, and the page shows it as U+FFFD rather than dropping it.
fn inside_character(before: &[u8], after: &[u8]) -> usize {
    if !after.first().copied().is_some_and(is_continuation) {
        return 0;
    }
    for (distance, &byte) in before.iter().rev().enumerate().map(|(i, b)| (i + 1, b)) {
        if is_continuation(byte) {
            continue;
        }
        let missing = width(byte).saturating_sub(distance);
        return after
            .iter()
            .take(missing)
            .take_while(|b| is_continuation(**b))
            .count();
    }
    0
}

/// Bytes at the end of `page` that begin a character without finishing
/// it: the ones to leave for the next read.
///
/// Only a true prefix counts. Before P103 this was "everything from the
/// first invalid byte on", so one byte that was not UTF-8 ended every page
/// before it and the next page, starting on it, had nothing to show.
fn unfinished_tail(page: &[u8]) -> usize {
    let lead = page
        .iter()
        .rev()
        .take(MAX_CONTINUATION)
        .position(|b| !is_continuation(*b))
        .map(|from_end| page.len() - 1 - from_end);
    match lead.map(|at| (at, std::str::from_utf8(&page[at..]))) {
        Some((at, Err(e))) if e.valid_up_to() == 0 && e.error_len().is_none() => page.len() - at,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn session(name: &str, stdout: &[u8], stderr: &[u8]) -> (TestDir, String) {
        let dir = std::env::temp_dir().join(format!("ccnm-output-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let reference = "r-0123456789abcdef".to_string();
        let run = dir.join(&reference);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("stdout"), stdout).unwrap();
        fs::write(run.join("stderr"), stderr).unwrap();
        (TestDir::adopt(dir), reference)
    }

    fn read(dir: &Path, args: ReadOutputArgs) -> OutputPage {
        read_output(dir, &args).unwrap()
    }

    fn fails(dir: &Path, args: ReadOutputArgs) -> Error {
        match read_output(dir, &args) {
            Err(e) => e,
            Ok(p) => panic!("expected a refusal, got {} bytes", p.bytes),
        }
    }

    #[test]
    fn a_short_stream_comes_back_whole() {
        let (dir, r) = session("short", b"hello\n", b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r.clone(),
                ..Default::default()
            },
        );
        assert_eq!(page.bytes, 6);
        assert_eq!(page.total_bytes, 6);
        assert!(page.eof);
        assert_eq!(page.next_offset, None);
        assert_eq!(page.stream, Stream::Stdout);
        assert_eq!(page.text, "hello\n[end of stdout at 6 bytes]");
    }

    #[test]
    fn paging_covers_the_stream_exactly_once_and_offsets_are_stable() {
        let body: Vec<u8> = (0..2000)
            .flat_map(|n| format!("line {n}\n").into_bytes())
            .collect();
        let (dir, r) = session("paging", &body, b"");
        let mut offset = 0u64;
        let mut seen = Vec::new();
        let mut pages = 0;
        loop {
            let page = read(
                &dir,
                ReadOutputArgs {
                    output_ref: r.clone(),
                    offset: Some(offset),
                    limit: Some(1024),
                    ..Default::default()
                },
            );
            pages += 1;
            // The content is exactly the first `bytes` bytes of the text;
            // the footer is appended after it.
            seen.extend_from_slice(&page.text.as_bytes()[..page.bytes]);
            match page.next_offset {
                Some(next) => {
                    assert!(next > offset, "paging must make progress");
                    offset = next;
                }
                None => break,
            }
            assert!(pages < 100, "too many pages");
        }
        assert!(pages > 10, "only {pages} pages");
        // Every byte, once, in order.
        assert_eq!(seen, body);

        // Stable: the same offset gives the same bytes on a second read.
        let a = read(
            &dir,
            ReadOutputArgs {
                output_ref: r.clone(),
                offset: Some(4096),
                limit: Some(64),
                ..Default::default()
            },
        );
        let b = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                offset: Some(4096),
                limit: Some(64),
                ..Default::default()
            },
        );
        assert_eq!(a.text, b.text);
    }

    #[test]
    fn a_page_seam_never_splits_a_character() {
        // Every character is three bytes, so no limit that is not a
        // multiple of three lands on a boundary by luck.
        let body = "中".repeat(500).into_bytes();
        let (dir, r) = session("utf8", &body, b"");
        for limit in [4u32, 5, 7, 100, 101] {
            let mut offset = 0u64;
            let mut collected = String::new();
            loop {
                let page = read(
                    &dir,
                    ReadOutputArgs {
                        output_ref: r.clone(),
                        offset: Some(offset),
                        limit: Some(limit),
                        ..Default::default()
                    },
                );
                assert!(page.notes.is_empty(), "limit {limit}: {:?}", page.notes);
                assert_eq!(page.offset % 3, 0, "limit {limit} left a ragged offset");
                collected.push_str(&page.text[..page.bytes]);
                match page.next_offset {
                    Some(next) => offset = next,
                    None => break,
                }
            }
            assert_eq!(collected, "中".repeat(500), "limit {limit}");
        }
    }

    #[test]
    fn a_limit_too_small_for_one_character_says_so() {
        let body = "中".repeat(10).into_bytes();
        let (dir, r) = session("tiny", &body, b"");
        // From a character's start, and from inside one: the page then
        // starts at the next character, which still does not fit.
        for (limit, offset) in [(2, 0), (1, 0), (2, 1)] {
            let e = fails(
                &dir,
                ReadOutputArgs {
                    output_ref: r.clone(),
                    limit: Some(limit),
                    offset: Some(offset),
                    ..Default::default()
                },
            );
            assert_eq!(e.code(), ErrorCode::InvalidArgs);
            assert!(e.message().contains("too small"), "{limit}@{offset}: {e}");
        }
        // Three bytes do hold one.
        assert_eq!(
            read(
                &dir,
                ReadOutputArgs {
                    output_ref: r,
                    limit: Some(3),
                    offset: Some(1),
                    ..Default::default()
                },
            )
            .bytes,
            3
        );
    }

    /// The P103 defect, first half. A model reading the tail works out an
    /// offset from `total_bytes`, and with any CJK or `✓` in the output it
    /// lands inside a character about two times in three. The first byte
    /// read was then a continuation byte, the "largest valid prefix" was
    /// empty, and a 4000-byte page was refused as too small.
    #[test]
    fn an_offset_inside_a_character_starts_at_the_next_one() {
        let body = "中".repeat(3000).into_bytes();
        let (dir, r) = session("midchar", &body, b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                offset: Some(4000),
                limit: Some(4000),
                ..Default::default()
            },
        );
        // 4000 is one byte into the character at 3999.
        assert_eq!(page.offset, 4002);
        assert_eq!(page.bytes, 3999);
        assert_eq!(page.next_offset, Some(8001));
        assert!(
            page.notes
                .iter()
                .any(|n| n.contains("offset 4000 is inside a character")
                    && n.contains("starts at offset 4002")),
            "{:?}",
            page.notes
        );
        assert!(
            page.text.contains("[offset 4000 is inside"),
            "{}",
            page.text
        );
    }

    /// Every offset of a stream with one-, two-, three- and four-byte
    /// characters: the page starts at the first character boundary at or
    /// after it and holds exactly the text from there.
    #[test]
    fn any_offset_reads_from_the_next_character_boundary() {
        let s = "a\u{e9}中😀z ✓ done\n";
        let (dir, r) = session("everyoffset", s.as_bytes(), b"");
        for requested in 0..=s.len() {
            let page = read(
                &dir,
                ReadOutputArgs {
                    output_ref: r.clone(),
                    offset: Some(requested as u64),
                    ..Default::default()
                },
            );
            let start = (requested..=s.len())
                .find(|&i| s.is_char_boundary(i))
                .unwrap();
            assert_eq!(page.offset, start as u64, "offset {requested}");
            assert_eq!(&page.text[..page.bytes], &s[start..], "offset {requested}");
            assert!(
                page.notes.iter().all(|n| !n.contains("UTF-8")),
                "offset {requested}: {:?}",
                page.notes
            );
            assert_eq!(
                page.notes.iter().any(|n| n.contains("inside a character")),
                start != requested,
                "offset {requested}: {:?}",
                page.notes
            );
        }
    }

    /// The P103 defect, second half. One byte that is not UTF-8 in the
    /// middle of a long output: the page ended just before it, the next
    /// page started on it, and from there every page was refused.
    #[test]
    fn an_invalid_byte_does_not_stop_the_paging() {
        let mut body = Vec::new();
        for n in 0..300 {
            body.extend_from_slice(format!("行 {n}\n").as_bytes());
        }
        body.push(0xff);
        for n in 0..300 {
            body.extend_from_slice(format!("✓ {n}\n").as_bytes());
        }
        let (dir, r) = session("invalid", &body, b"");
        for limit in [64u32, 1000, 1024] {
            let mut offset = 0u64;
            let mut text = String::new();
            let mut flagged = false;
            let mut pages = 0;
            loop {
                let page = read(
                    &dir,
                    ReadOutputArgs {
                        output_ref: r.clone(),
                        offset: Some(offset),
                        limit: Some(limit),
                        ..Default::default()
                    },
                );
                // Following next_offset never lands inside a character.
                assert_eq!(page.offset, offset, "limit {limit}");
                text.push_str(&page.text[..page.bytes]);
                flagged |= page.notes.iter().any(|n| n.contains("not valid UTF-8"));
                pages += 1;
                assert!(pages < 1000, "limit {limit}: no progress");
                match page.next_offset {
                    Some(next) => {
                        assert!(next > offset, "limit {limit}: stuck at {offset}");
                        offset = next;
                    }
                    None => break,
                }
            }
            // Every byte once, in order, the bad one as U+FFFD.
            assert_eq!(text, String::from_utf8_lossy(&body), "limit {limit}");
            assert!(flagged, "limit {limit}: the replacement went unmentioned");
        }
    }

    /// Moving forward is for an offset inside a *valid* character. A
    /// continuation byte with no lead before it is just a bad byte, and
    /// skipping it would lose it without a word.
    #[test]
    fn a_stray_continuation_byte_is_not_taken_for_the_middle_of_a_character() {
        let mut body = b"a\x80".to_vec();
        body.extend_from_slice(&[b'b'; 100]);
        let (dir, r) = session("stray", &body, b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                offset: Some(1),
                limit: Some(10),
                ..Default::default()
            },
        );
        assert_eq!(page.offset, 1);
        assert_eq!(&page.text[..page.bytes], "\u{fffd}bbbbbbbbb");
        assert_eq!(page.next_offset, Some(11));
        assert!(
            page.notes.iter().any(|n| n.contains("not valid UTF-8")),
            "{:?}",
            page.notes
        );
        assert!(
            page.notes.iter().all(|n| !n.contains("inside a character")),
            "{:?}",
            page.notes
        );
    }

    /// A command still running may be halfway through writing a character:
    /// that one is left for the next read, while a bad byte before it is
    /// replaced, not a wall.
    #[test]
    fn a_running_command_s_unfinished_character_waits_for_the_next_read() {
        let mut body = b"a\xffb\n".to_vec();
        body.extend_from_slice(&"中".as_bytes()[..2]);
        let (dir, r) = session("running", &body, b"");
        let run = dir.join(&r);
        jobs::Status::started("x", None).write(&run).unwrap();
        let running = std::fs::File::create(run.join("running")).unwrap();
        running.lock().unwrap();

        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r.clone(),
                ..Default::default()
            },
        );
        assert!(page.running, "{}", page.text);
        assert_eq!(&page.text[..page.bytes], "a\u{fffd}b\n");
        assert_eq!(page.next_offset, Some(4));
        // Nothing whole to read yet is "so far", not "too small".
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                offset: Some(4),
                ..Default::default()
            },
        );
        assert_eq!(page.bytes, 0);
        assert_eq!(page.next_offset, Some(4));
        drop(running);
    }

    #[test]
    fn stderr_is_a_separate_stream() {
        let (dir, r) = session("streams", b"out\n", b"err\n");
        let out = read(
            &dir,
            ReadOutputArgs {
                output_ref: r.clone(),
                ..Default::default()
            },
        );
        assert!(out.text.starts_with("out\n"), "{}", out.text);
        let err = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                stream: Some(Stream::Stderr),
                ..Default::default()
            },
        );
        assert!(err.text.starts_with("err\n"), "{}", err.text);
        assert_eq!(err.stream, Stream::Stderr);
    }

    #[test]
    fn an_empty_stream_is_an_answer() {
        let (dir, r) = session("empty", b"", b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                ..Default::default()
            },
        );
        assert_eq!(page.bytes, 0);
        assert_eq!(page.total_bytes, 0);
        assert!(page.eof);
        assert_eq!(page.text, "[end of stdout at 0 bytes]");
    }

    #[test]
    fn an_output_ref_is_matched_by_shape_so_there_is_nothing_to_traverse() {
        let (dir, r) = session("refs", b"kept\n", b"");
        // A real secret one directory up, reachable only if the reference
        // were ever treated as a path.
        fs::write(dir.join("stdout"), "SECRET\n").unwrap();
        for bad in [
            "..",
            "../stdout",
            "r-0123456789abcdef/../..",
            "/etc/passwd",
            "r-0123456789abcdeg",
            "r-0123456789abcde",
            "r-0123456789abcdef0",
            "",
            "stdout",
        ] {
            let e = fails(
                &dir,
                ReadOutputArgs {
                    output_ref: bad.to_string(),
                    ..Default::default()
                },
            );
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{bad}");
            assert!(e.message().contains("not an output_ref"), "{bad} -> {e}");
        }
        // The well-formed one still works.
        assert_eq!(
            read(
                &dir,
                ReadOutputArgs {
                    output_ref: r,
                    ..Default::default()
                }
            )
            .bytes,
            5
        );
    }

    #[test]
    fn a_reference_that_was_pruned_says_so_rather_than_crashing() {
        let (dir, _) = session("gone", b"x", b"");
        let e = fails(
            &dir,
            ReadOutputArgs {
                output_ref: "r-ffffffffffffffff".into(),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(
            e.message().contains("not kept forever") || e.message().contains("no output kept"),
            "{e}"
        );
    }

    #[test]
    fn an_offset_past_the_end_is_refused_and_the_end_itself_is_not() {
        let (dir, r) = session("offsets", b"abc", b"");
        let at_end = read(
            &dir,
            ReadOutputArgs {
                output_ref: r.clone(),
                offset: Some(3),
                ..Default::default()
            },
        );
        assert_eq!(at_end.bytes, 0);
        assert!(at_end.eof);

        let e = fails(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                offset: Some(4),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("past the end"), "{e}");
    }

    #[test]
    fn non_utf8_output_is_flagged_not_refused() {
        let (dir, r) = session("binary", b"ok \xff\xfe done\n", b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                ..Default::default()
            },
        );
        assert!(page.text.contains('\u{fffd}'), "{}", page.text);
        assert!(
            page.notes.iter().any(|n| n.contains("not valid UTF-8")),
            "{:?}",
            page.notes
        );
    }

    #[test]
    fn structured_content_carries_no_output() {
        let (dir, r) = session("bounded", b"the actual bytes\n", b"");
        let page = read(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                ..Default::default()
            },
        );
        let json = serde_json::to_string(&page).unwrap();
        assert!(!json.contains("the actual bytes"), "{json}");
        assert!(json.contains("\"total_bytes\":17"), "{json}");
    }
}

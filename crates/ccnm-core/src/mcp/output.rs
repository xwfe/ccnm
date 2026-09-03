//! `read_output`: page through what a command wrote.
//!
//! The seventh and last tool of the set (design doc section 14).
//! `exec_command` returns the head and the tail of its output and keeps
//! all of it on the workspace machine; this is how the middle is reached.
//!
//! Offsets are byte offsets into the retained file and they are stable,
//! because the file is finished before the reference exists: a run's
//! output never changes, so offset 4096 means the same thing an hour
//! later. That is the property the design doc asks for, and it is what
//! makes paging cheap — no cursor to keep, nothing re-sent.
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

/// Bytes returned when the caller does not say.
pub const DEFAULT_LIMIT: usize = 16 * 1024;
/// Ceiling on `limit` (design doc section 15).
pub const MAX_LIMIT: usize = 32 * 1024;

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
    pub eof: bool,
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

    let path = session_dir.join(&reference).join(stream.file());
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

    let mut file = std::fs::File::open(&path)
        .map_err(|e| Error::internal("cannot open retained output").with_source(e))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| Error::internal("cannot seek retained output").with_source(e))?;
    let mut buf = vec![0u8; limit];
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(Error::internal("cannot read retained output").with_source(e)),
        }
    }
    buf.truncate(filled);

    // Cut the tail back to a character boundary so the next offset starts
    // on one too. Without this, paging a file with any non-ASCII in it
    // puts a replacement character at every page seam.
    let mut notes: Vec<String> = Vec::new();
    let end = if offset + filled as u64 >= total_bytes {
        filled
    } else {
        boundary_before(&buf)
    };
    if end == 0 && filled > 0 {
        return Err(Error::invalid_args(format!(
            "limit {limit} is too small to hold one character at this offset"
        )));
    }
    let text = match std::str::from_utf8(&buf[..end]) {
        Ok(text) => text.to_string(),
        Err(_) => {
            notes.push("this stream is not valid UTF-8; invalid bytes were replaced".into());
            String::from_utf8_lossy(&buf[..end]).into_owned()
        }
    };
    let next = offset + end as u64;
    let eof = next >= total_bytes;

    let footer = if eof {
        format!("[end of {} at {total_bytes} bytes]", stream.file())
    } else {
        format!("[{next} of {total_bytes} bytes; continue with offset={next}]")
    };
    let mut rendered = text.clone();
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str(&footer);
    for note in &notes {
        rendered.push_str("\n[");
        rendered.push_str(note);
        rendered.push(']');
    }

    Ok(OutputPage {
        text: rendered,
        output_ref: reference,
        stream,
        offset,
        bytes: text.len(),
        total_bytes,
        next_offset: (!eof).then_some(next),
        eof,
        notes,
    })
}

/// The reference must be exactly what `exec_command` produces. Matching
/// the shape rather than sanitizing means there is no path to traverse:
/// a slash, a dot or a `..` never gets as far as being joined to
/// anything.
fn validate_ref(raw: &str) -> Result<String> {
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

/// The largest prefix of `buf` that is a whole number of UTF-8
/// characters, looking back at most four bytes.
fn boundary_before(buf: &[u8]) -> usize {
    match std::str::from_utf8(buf) {
        Ok(_) => buf.len(),
        Err(e) => e.valid_up_to(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::fs;
    use std::path::PathBuf;

    fn session(name: &str, stdout: &[u8], stderr: &[u8]) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("ccnm-output-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let reference = "r-0123456789abcdef".to_string();
        let run = dir.join(&reference);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("stdout"), stdout).unwrap();
        fs::write(run.join("stderr"), stderr).unwrap();
        (dir, reference)
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
        let e = fails(
            &dir,
            ReadOutputArgs {
                output_ref: r,
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("too small"), "{e}");
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

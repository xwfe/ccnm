//! `read_file`: the first of the six coding tools (design doc sections 14
//! and 15).
//!
//! It streams. The file is read one line at a time and the loop stops at
//! the first limit it hits, so `read_file` on a 2 GB log costs the same as
//! on a 2 KB one. coding-tools-mcp reads the whole file into memory and
//! then trims to `max_bytes` (`docs/research/coding-tools-mcp.md`, item 7
//! of section m); on a home machine shared with the user's real work that
//! is a memory spike nobody asked for.
//!
//! Everything it returns is bounded (section 16), and all of it is in
//! `content[0].text`: the numbered lines, then one footer with the
//! version and where to continue. Nothing goes in `structuredContent`,
//! because Claude Code shows the model that *instead of* the text when
//! both are present (measured 2026-09-04; see `text_only` in the server).
//!
//! What tends to go wrong in practice, and what happens instead:
//!
//! ```text
//! a fifo or device in the workspace   refused after `stat`, before `open`,
//!                                     because opening a fifo blocks forever
//!                                     and would freeze the whole session
//! a minified 5 MB single line         cut at max_bytes on a char boundary,
//!                                     and the reply says which line was cut
//! a binary file                       refused by a NUL scan of the first 8 KiB
//!                                     rather than spending context on garbage
//! latin-1 or other non-UTF-8 text     decoded lossily and flagged, not refused
//! CRLF, a BOM, no final newline       normalized for display and reported, so
//!                                     apply_patch later knows what it is editing
//! start_line past the end             an explicit "file has N lines", not an
//!                                     empty answer the model has to guess at
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::path;

/// Lines returned when the caller does not say (design doc section 15).
pub const DEFAULT_MAX_LINES: u32 = 200;
/// Ceiling on `max_lines`. A request above this is clamped, not refused:
/// `max_lines` is "give me at most N", so a big N is a preference, not a
/// mistake.
pub const MAX_MAX_LINES: u32 = 2_000;
/// Bytes of file content returned when the caller does not say.
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024;
/// Ceiling on `max_bytes`, likewise clamped. Twice the default is enough
/// for a deliberate wide read and still far from filling a context window.
pub const MAX_MAX_BYTES: usize = 64 * 1024;

/// How much of the file is sniffed for NUL bytes before deciding it is
/// binary. Matches what `git` and `grep` look at.
const BINARY_PEEK_BYTES: u64 = 8 * 1024;

/// Ceiling on bytes walked while seeking `start_line`. Reaching line
/// 1_000_000 of a huge log is not what this tool is for, and without a
/// bound one call can hold the runtime for minutes.
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// Arguments of `read_file`. Doc comments become the JSON schema
/// descriptions the model reads, and every one of them is charged to the
/// 16 KiB `tools/list` budget, so they are terse on purpose.
///
/// `range(min = 1)` is not decoration. Without it schemars derives
/// `minimum: 0` from `u32` and the schema tells the model that
/// `start_line: 0` is legal, which the code then refuses — a wasted round
/// trip caused by ccnm's own documentation.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadFileArgs {
    /// Path relative to the workspace root, e.g. `src/main.rs`.
    pub path: String,
    /// First line to return, 1-based. Default 1.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub start_line: Option<u32>,
    /// Last line to return, inclusive. Default: end of file.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub end_line: Option<u32>,
    /// Maximum lines to return. Default 200, capped at 2000.
    #[serde(default)]
    #[schemars(range(min = 1, max = 2000))]
    pub max_lines: Option<u32>,
    /// Maximum bytes of file content to return. Default 32768, capped at 65536.
    #[serde(default)]
    #[schemars(range(min = 1, max = 65536))]
    pub max_bytes: Option<u32>,
}

/// Why a read stopped early. Absent when the caller's own range or the end
/// of the file ended it, because neither is a truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    MaxLines,
    MaxBytes,
}

/// How the lines that were read end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    Lf,
    Crlf,
    /// Both appear. Worth saying: a patch that assumes one of them
    /// corrupts the other half of the file.
    Mixed,
    /// A single line with no terminator, or no lines at all.
    None,
}

/// The result of one `read_file`. What the model sees is
/// [`text`](Self::text) alone; the other fields are what the text was
/// rendered from, kept for tests and for callers inside ccnm, and are not
/// sent on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    /// Numbered lines plus one footer line. Goes to `content[0].text`.
    #[serde(skip)]
    pub text: String,
    /// The normalized workspace-relative path that was read.
    pub path: String,
    /// First line actually returned; 0 when no line was.
    pub start_line: u32,
    /// Last line actually returned; 0 when no line was.
    pub end_line: u32,
    pub lines: u32,
    /// Bytes of file content in `text`, not counting line numbers or the
    /// footer.
    pub bytes: usize,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_by: Option<Truncation>,
    /// Where a following call should start. Absent only when the read
    /// reached the end of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_line: Option<u32>,
    /// Only known when the read reached the end of the file. Counting the
    /// lines of a file that was not fully read would mean a second pass,
    /// which is exactly the cost this tool is built to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u32>,
    pub file_bytes: u64,
    /// What the file was when it was read. Hand it back to `apply_patch`
    /// so an edit built on this content is refused if the file has since
    /// changed (see [`crate::mcp::version_of`]).
    pub version: String,
    pub line_ending: LineEnding,
    /// Whether the file's last line ends with a newline. Absent unless the
    /// end of the file was reached. `apply_patch` will need it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_newline: Option<bool>,
    /// Anything the caller should know about the bytes themselves: a
    /// stripped BOM, invalid UTF-8, a line cut in half. A handful of short
    /// strings at most.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Read a slice of a text file inside `root`.
///
/// `root` must already be canonical; [`crate::mcp::server::Server`]
/// canonicalizes it once at startup and never revisits it.
pub fn read_file(root: &Path, args: &ReadFileArgs) -> Result<FileChunk> {
    let limits = Limits::new(args)?;
    let target = path::resolve_read(root, &args.path)?;
    let rel = target.rel().to_string();

    // `stat` first. `File::open` on a fifo blocks until a writer shows up,
    // which on a current-thread runtime hangs every later tool call in the
    // session, so the type check has to happen before the open, not after.
    // (A file swapped for a fifo in between would still block; that is a
    // race against someone with write access to the workspace, which is a
    // bigger problem than this tool.)
    let meta = std::fs::metadata(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot stat {rel}")).with_source(e))?;
    if meta.is_dir() {
        return Err(Error::invalid_args(format!(
            "{rel} is a directory, not a file"
        )));
    }
    if !meta.is_file() {
        return Err(Error::invalid_args(format!(
            "{rel} is not a regular file (fifo, socket or device); ccnm will not open it"
        )));
    }
    let file_bytes = meta.len();
    let version = crate::mcp::version_of(&meta);

    let mut file = File::open(target.abs()).map_err(|e| open_error(&rel, e))?;
    if let Some(offset) = binary_offset(&mut file)? {
        return Err(Error::invalid_args(format!(
            "{rel} looks like a binary file (NUL byte at offset {offset}); ccnm only reads text"
        )));
    }
    file.rewind()
        .map_err(|e| Error::internal(format!("cannot rewind {rel}")).with_source(e))?;

    let mut scan = Scan::new(limits);
    scan.run(BufReader::new(file), &rel)?;
    Ok(scan.finish(rel, file_bytes, version))
}

/// Validated, clamped arguments.
#[derive(Debug, Clone, Copy)]
struct Limits {
    start: u32,
    end: Option<u32>,
    max_lines: u32,
    max_bytes: usize,
}

impl Limits {
    fn new(args: &ReadFileArgs) -> Result<Self> {
        let start = args.start_line.unwrap_or(1);
        if start == 0 {
            return Err(Error::invalid_args(
                "start_line is 1-based; the first line is 1, not 0",
            ));
        }
        if let Some(end) = args.end_line {
            if end == 0 {
                return Err(Error::invalid_args(
                    "end_line is 1-based; the first line is 1, not 0",
                ));
            }
            if end < start {
                return Err(Error::invalid_args(format!(
                    "end_line {end} is before start_line {start}"
                )));
            }
        }
        let max_lines = match args.max_lines {
            Some(0) => return Err(Error::invalid_args("max_lines must be at least 1")),
            Some(n) => n.min(MAX_MAX_LINES),
            None => DEFAULT_MAX_LINES,
        };
        let max_bytes = match args.max_bytes {
            Some(0) => return Err(Error::invalid_args("max_bytes must be at least 1")),
            Some(n) => (n as usize).min(MAX_MAX_BYTES),
            None => DEFAULT_MAX_BYTES,
        };
        Ok(Limits {
            start,
            end: args.end_line,
            max_lines,
            max_bytes,
        })
    }
}

/// The streaming read, kept apart from the I/O setup so the loop can be
/// tested against an in-memory reader.
struct Scan {
    limits: Limits,
    lines: Vec<(u32, String)>,
    bytes: usize,
    crlf: bool,
    lf: bool,
    last_line_terminated: bool,
    total_lines: Option<u32>,
    next_start_line: Option<u32>,
    truncated_by: Option<Truncation>,
    partial_line: Option<u32>,
    lossy: bool,
    bom: bool,
}

impl Scan {
    fn new(limits: Limits) -> Self {
        Scan {
            limits,
            lines: Vec::new(),
            bytes: 0,
            crlf: false,
            lf: false,
            last_line_terminated: true,
            total_lines: None,
            next_start_line: None,
            truncated_by: None,
            partial_line: None,
            lossy: false,
            bom: false,
        }
    }

    fn run<R: BufRead>(&mut self, mut reader: R, rel: &str) -> Result<()> {
        let mut raw = Vec::new();
        let mut line_no: u32 = 0;
        let mut scanned: u64 = 0;

        loop {
            raw.clear();
            let read = reader
                .read_until(b'\n', &mut raw)
                .map_err(|e| open_error(rel, e))?;
            if read == 0 {
                self.total_lines = Some(line_no);
                break;
            }
            scanned += read as u64;
            if scanned > MAX_SCAN_BYTES {
                return Err(Error::invalid_args(format!(
                    "{rel}: reading line {} would mean walking more than {} MiB; use search_text to find the part you want",
                    self.limits.start,
                    MAX_SCAN_BYTES / (1024 * 1024)
                )));
            }
            line_no = line_no.saturating_add(1);

            let body = self.classify(&raw);
            if line_no < self.limits.start {
                continue;
            }
            if self.limits.end.is_some_and(|end| line_no > end) {
                // The caller's own range ended the read. Not a truncation,
                // but the file does continue.
                self.next_start_line = Some(line_no);
                return Ok(());
            }
            if self.lines.len() as u32 >= self.limits.max_lines {
                self.truncated_by = Some(Truncation::MaxLines);
                self.next_start_line = Some(line_no);
                return Ok(());
            }
            if !self.push(line_no, body) {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Strip the line terminator, remembering which kind it was.
    fn classify<'a>(&mut self, raw: &'a [u8]) -> &'a [u8] {
        if let Some(body) = raw.strip_suffix(b"\r\n") {
            self.crlf = true;
            self.last_line_terminated = true;
            body
        } else if let Some(body) = raw.strip_suffix(b"\n") {
            self.lf = true;
            self.last_line_terminated = true;
            body
        } else {
            self.last_line_terminated = false;
            raw
        }
    }

    /// Add one line to the answer. Returns false when the byte budget ran
    /// out and the scan should stop.
    fn push(&mut self, line_no: u32, body: &[u8]) -> bool {
        let mut text = match std::str::from_utf8(body) {
            Ok(s) => s.to_string(),
            Err(_) => {
                self.lossy = true;
                String::from_utf8_lossy(body).into_owned()
            }
        };
        if line_no == 1
            && let Some(rest) = text.strip_prefix('\u{feff}')
        {
            self.bom = true;
            text = rest.to_string();
        }

        let room = self.limits.max_bytes - self.bytes;
        if text.len() <= room {
            self.bytes += text.len();
            self.lines.push((line_no, text));
            return true;
        }
        self.truncated_by = Some(Truncation::MaxBytes);
        if self.lines.is_empty() {
            // One line longer than the entire budget: a minified bundle, a
            // one-line JSON dump. Returning nothing would make the file
            // permanently unreadable, so return the prefix that fits and
            // say so. The rest of this line is not reachable through
            // read_file; next_start_line points past it so a caller that
            // loops on next_start_line terminates.
            let cut = crate::mcp::truncate_bytes(&text, room).len();
            text.truncate(cut);
            self.bytes += text.len();
            self.lines.push((line_no, text));
            self.partial_line = Some(line_no);
            self.next_start_line = Some(line_no.saturating_add(1));
        } else {
            self.next_start_line = Some(line_no);
        }
        false
    }

    fn finish(self, path: String, file_bytes: u64, version: String) -> FileChunk {
        let start_line = self.lines.first().map_or(0, |(n, _)| *n);
        let end_line = self.lines.last().map_or(0, |(n, _)| *n);
        let line_ending = match (self.crlf, self.lf) {
            (true, true) => LineEnding::Mixed,
            (true, false) => LineEnding::Crlf,
            (false, true) => LineEnding::Lf,
            (false, false) => LineEnding::None,
        };
        let final_newline = match self.total_lines {
            Some(0) => None,
            Some(_) => Some(self.last_line_terminated),
            None => None,
        };
        let mut notes = Vec::new();
        if self.bom {
            notes.push("a UTF-8 BOM was stripped from line 1".to_string());
        }
        if self.lossy {
            notes.push(
                "the file is not valid UTF-8; invalid bytes were replaced with U+FFFD".to_string(),
            );
        }
        if let Some(line) = self.partial_line {
            notes.push(format!(
                "line {line} is longer than max_bytes and was cut; the rest of it is not returned"
            ));
        }

        let text = render(
            &self.lines,
            &path,
            Stop {
                truncated_by: self.truncated_by,
                next_start_line: self.next_start_line,
                total_lines: self.total_lines,
            },
            self.limits,
            &version,
            &notes,
        );
        FileChunk {
            text,
            path,
            start_line,
            end_line,
            lines: self.lines.len() as u32,
            bytes: self.bytes,
            truncated: self.truncated_by.is_some(),
            truncated_by: self.truncated_by,
            next_start_line: self.next_start_line,
            total_lines: self.total_lines,
            file_bytes,
            version,
            line_ending,
            final_newline,
            notes,
        }
    }
}

/// Numbered lines and a footer that says what to do next.
///
/// Where a read stopped, for the footer.
struct Stop {
    truncated_by: Option<Truncation>,
    next_start_line: Option<u32>,
    total_lines: Option<u32>,
}

/// The text is the whole answer: the numbered lines, then one bracketed
/// footer that says whether there is more and carries the `version` an
/// `apply_patch` has to hand back. Nothing the model needs is anywhere
/// else — Claude Code shows the model exactly one channel (see
/// `text_only` in the server), and a model that cannot see the footer
/// stops after 200 lines believing it read the whole file, or cannot
/// patch what it read.
fn render(
    lines: &[(u32, String)],
    path: &str,
    stop: Stop,
    limits: Limits,
    version: &str,
    notes: &[String],
) -> String {
    let Stop {
        truncated_by,
        next_start_line,
        total_lines,
    } = stop;
    let mut out = String::new();
    if lines.is_empty() {
        match total_lines {
            Some(0) => out.push_str(&format!("[{path} is empty; version {version}]")),
            Some(total) => out.push_str(&format!(
                "[no lines returned: {path} has {total} line{}, and start_line was {}; version {version}]",
                if total == 1 { "" } else { "s" },
                limits.start
            )),
            None => out.push_str(&format!(
                "[no lines returned from {path}; version {version}]"
            )),
        }
        return out;
    }

    let width = lines.last().map_or(1, |(n, _)| n.to_string().len()).max(1);
    for (n, text) in lines {
        out.push_str(&format!("{n:>width$}\u{2192}{text}\n"));
    }

    let footer = match (truncated_by, next_start_line, total_lines) {
        (Some(Truncation::MaxLines), Some(next), _) => format!(
            "stopped at max_lines={}; continue with start_line={next}",
            limits.max_lines
        ),
        (Some(Truncation::MaxBytes), Some(next), _) => format!(
            "stopped at max_bytes={}; continue with start_line={next}",
            limits.max_bytes
        ),
        (None, Some(next), _) => format!("file continues at line {next}"),
        (_, None, Some(total)) => format!(
            "end of file, {total} line{}",
            if total == 1 { "" } else { "s" }
        ),
        (_, None, None) => "end of range".to_string(),
    };
    out.push_str(&format!("[{footer}; version {version}]"));
    for note in notes {
        out.push_str("\n[");
        out.push_str(note);
        out.push(']');
    }
    out
}

/// Does the head of the file contain a NUL? Cheaper and more reliable than
/// guessing from the extension, and it is what `git` does.
fn binary_offset(file: &mut File) -> Result<Option<usize>> {
    let mut head = Vec::new();
    file.take(BINARY_PEEK_BYTES)
        .read_to_end(&mut head)
        .map_err(|e| Error::internal("cannot read file head").with_source(e))?;
    Ok(head.iter().position(|b| *b == 0))
}

/// A failed open is almost always the caller's problem, not a bug: the
/// file went away between `stat` and `open`, or it is not readable by the
/// runtime user. Anything else keeps `CCNM_E_INTERNAL` so it stays visible.
fn open_error(rel: &str, err: std::io::Error) -> Error {
    let code = match err.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            ErrorCode::InvalidArgs
        }
        _ => ErrorCode::Internal,
    };
    Error::new(code, format!("cannot read {rel}")).with_source(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// The footer's `; version <size-mtime>` is different on every run, so
    /// tests that compare whole texts compare them without it -- after
    /// checking it was there, because a footer without the version is the
    /// bug that made apply_patch impossible.
    fn strip_version(text: &str) -> String {
        let start = text
            .rfind("; version ")
            .unwrap_or_else(|| panic!("no version in the footer of {text:?}"));
        assert!(text.ends_with(']'), "{text:?}");
        format!("{}]", &text[..start])
    }

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-read-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn write(root: &Path, name: &str, bytes: impl AsRef<[u8]>) {
        fs::write(root.join(name), bytes).unwrap();
    }

    fn args(path: &str) -> ReadFileArgs {
        ReadFileArgs {
            path: path.to_string(),
            ..Default::default()
        }
    }

    fn read(root: &Path, a: &ReadFileArgs) -> FileChunk {
        read_file(root, a).unwrap()
    }

    fn err(root: &Path, a: &ReadFileArgs) -> Error {
        match read_file(root, a) {
            Err(e) => e,
            Ok(c) => panic!("expected a refusal, got {} lines", c.lines),
        }
    }

    #[test]
    fn a_small_file_comes_back_whole_and_numbered() {
        let root = workspace("small");
        write(&root, "a.txt", "one\ntwo\nthree\n");
        let c = read(&root, &args("a.txt"));
        assert_eq!(
            strip_version(&c.text),
            "1\u{2192}one\n2\u{2192}two\n3\u{2192}three\n[end of file, 3 lines]"
        );
        // The version the model must hand back to apply_patch is in the
        // text, verbatim.
        assert!(
            c.text.ends_with(&format!("; version {}]", c.version)),
            "{}",
            c.text
        );
        assert_eq!((c.start_line, c.end_line, c.lines), (1, 3, 3));
        assert_eq!(c.total_lines, Some(3));
        assert_eq!(c.next_start_line, None);
        assert!(!c.truncated);
        assert_eq!(c.line_ending, LineEnding::Lf);
        assert_eq!(c.final_newline, Some(true));
        assert_eq!(c.bytes, "onetwothree".len());
        assert_eq!(c.file_bytes, 14);
        assert!(c.notes.is_empty());
    }

    #[test]
    fn line_numbers_are_right_aligned_to_the_widest_one() {
        let root = workspace("width");
        let body: String = (1..=12).map(|n| format!("l{n}\n")).collect();
        write(&root, "a.txt", &body);
        let c = read(&root, &args("a.txt"));
        assert!(
            c.text.starts_with(" 1\u{2192}l1\n 2\u{2192}l2\n"),
            "{}",
            c.text
        );
        assert!(c.text.contains("\n12\u{2192}l12\n"), "{}", c.text);
    }

    /// The struct is no longer sent as `structuredContent` (the text is
    /// the whole answer), but its serialized form is still what tests and
    /// ccnm's own callers see, and it must not quietly grow the body.
    #[test]
    fn the_chunk_serializes_without_the_file_body() {
        let root = workspace("nodup");
        write(&root, "a.txt", "hello world\n");
        let c = read(&root, &args("a.txt"));
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("hello world"), "{json}");
        assert!(json.contains("\"path\":\"a.txt\""), "{json}");
    }

    #[test]
    fn a_range_is_honoured_and_says_the_file_continues() {
        let root = workspace("range");
        let body: String = (1..=10).map(|n| format!("l{n}\n")).collect();
        write(&root, "a.txt", &body);
        let c = read(
            &root,
            &ReadFileArgs {
                start_line: Some(3),
                end_line: Some(5),
                ..args("a.txt")
            },
        );
        assert_eq!((c.start_line, c.end_line, c.lines), (3, 5, 3));
        assert!(!c.truncated, "an honoured range is not a truncation");
        assert_eq!(c.next_start_line, Some(6));
        assert_eq!(c.total_lines, None);
        assert!(
            c.text.contains("[file continues at line 6; version "),
            "{}",
            c.text
        );
    }

    #[test]
    fn a_range_that_reaches_the_end_reports_the_end() {
        let root = workspace("range-eof");
        write(&root, "a.txt", "l1\nl2\nl3\n");
        let c = read(
            &root,
            &ReadFileArgs {
                start_line: Some(2),
                end_line: Some(9),
                ..args("a.txt")
            },
        );
        assert_eq!((c.start_line, c.end_line), (2, 3));
        assert_eq!(c.next_start_line, None);
        assert_eq!(c.total_lines, Some(3));
    }

    #[test]
    fn start_line_past_the_end_says_how_long_the_file_is() {
        let root = workspace("past");
        write(&root, "a.txt", "l1\nl2\n");
        let c = read(
            &root,
            &ReadFileArgs {
                start_line: Some(99),
                ..args("a.txt")
            },
        );
        assert_eq!(c.lines, 0);
        assert_eq!(c.total_lines, Some(2));
        assert_eq!(
            strip_version(&c.text),
            "[no lines returned: a.txt has 2 lines, and start_line was 99]"
        );
    }

    #[test]
    fn an_empty_file_says_it_is_empty() {
        let root = workspace("empty");
        write(&root, "a.txt", "");
        let c = read(&root, &args("a.txt"));
        assert_eq!(c.lines, 0);
        assert_eq!(c.total_lines, Some(0));
        assert_eq!(c.file_bytes, 0);
        assert_eq!(c.line_ending, LineEnding::None);
        assert_eq!(c.final_newline, None);
        // An empty file still has a version: it can be patched into.
        assert_eq!(strip_version(&c.text), "[a.txt is empty]");
    }

    #[test]
    fn bad_line_arguments_are_invalid_args_not_policy() {
        let root = workspace("badargs");
        write(&root, "a.txt", "l1\n");
        let cases: [(ReadFileArgs, &str); 5] = [
            (
                ReadFileArgs {
                    start_line: Some(0),
                    ..args("a.txt")
                },
                "1-based",
            ),
            (
                ReadFileArgs {
                    end_line: Some(0),
                    ..args("a.txt")
                },
                "1-based",
            ),
            (
                ReadFileArgs {
                    start_line: Some(5),
                    end_line: Some(2),
                    ..args("a.txt")
                },
                "before start_line",
            ),
            (
                ReadFileArgs {
                    max_lines: Some(0),
                    ..args("a.txt")
                },
                "max_lines",
            ),
            (
                ReadFileArgs {
                    max_bytes: Some(0),
                    ..args("a.txt")
                },
                "max_bytes",
            ),
        ];
        for (a, needle) in cases {
            let e = err(&root, &a);
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{e}");
            assert!(e.message().contains(needle), "{e}");
        }
    }

    #[test]
    fn max_lines_truncates_and_points_at_the_next_line() {
        let root = workspace("maxlines");
        let body: String = (1..=500).map(|n| format!("l{n}\n")).collect();
        write(&root, "a.txt", &body);
        let c = read(&root, &args("a.txt"));
        assert_eq!(c.lines, DEFAULT_MAX_LINES);
        assert!(c.truncated);
        assert_eq!(c.truncated_by, Some(Truncation::MaxLines));
        assert_eq!(c.next_start_line, Some(201));
        assert_eq!(c.total_lines, None, "the file was not read to the end");
        assert_eq!(c.final_newline, None);
        assert!(
            strip_version(&c.text)
                .ends_with("[stopped at max_lines=200; continue with start_line=201]"),
            "{}",
            &c.text[c.text.len() - 80..]
        );

        // And the next call carries on from exactly there.
        let next = read(
            &root,
            &ReadFileArgs {
                start_line: c.next_start_line,
                ..args("a.txt")
            },
        );
        assert_eq!(next.start_line, 201);
        assert!(next.text.starts_with("201\u{2192}l201\n"), "{}", next.text);
    }

    #[test]
    fn max_lines_is_clamped_rather_than_refused() {
        let root = workspace("clamp");
        let body: String = (1..=3000).map(|n| format!("l{n}\n")).collect();
        write(&root, "a.txt", &body);
        let c = read(
            &root,
            &ReadFileArgs {
                max_lines: Some(999_999),
                max_bytes: Some(u32::MAX),
                ..args("a.txt")
            },
        );
        assert_eq!(c.lines, MAX_MAX_LINES);
        assert!(
            c.text.contains(&format!("max_lines={MAX_MAX_LINES}")),
            "clamped value must be visible"
        );
    }

    #[test]
    fn max_bytes_stops_between_lines() {
        let root = workspace("maxbytes");
        // 40 lines of 100 bytes each; 1 KiB of budget fits ten of them.
        let body: String = (1..=40).map(|n| format!("{n:0>99}\n")).collect();
        write(&root, "a.txt", &body);
        let c = read(
            &root,
            &ReadFileArgs {
                max_bytes: Some(1000),
                ..args("a.txt")
            },
        );
        assert_eq!(c.truncated_by, Some(Truncation::MaxBytes));
        assert!(c.bytes <= 1000, "{} bytes", c.bytes);
        assert_eq!(c.next_start_line, Some(c.end_line + 1));
        assert!(c.notes.is_empty(), "no line was cut in half: {:?}", c.notes);
        assert!(c.text.contains("[stopped at max_bytes=1000"), "{}", c.text);
    }

    #[test]
    fn one_line_longer_than_the_budget_comes_back_partial_and_terminates() {
        let root = workspace("longline");
        write(&root, "min.js", format!("{}\nafter\n", "x".repeat(200_000)));
        let c = read(
            &root,
            &ReadFileArgs {
                max_bytes: Some(64),
                ..args("min.js")
            },
        );
        assert_eq!(c.lines, 1);
        assert_eq!(c.bytes, 64);
        assert_eq!(c.truncated_by, Some(Truncation::MaxBytes));
        // Past the cut line, not back onto it: a caller looping on
        // next_start_line has to make progress.
        assert_eq!(c.next_start_line, Some(2));
        assert!(
            c.notes.iter().any(|n| n.contains("line 1 is longer")),
            "{:?}",
            c.notes
        );
        let next = read(
            &root,
            &ReadFileArgs {
                start_line: Some(2),
                ..args("min.js")
            },
        );
        assert!(next.text.starts_with("2\u{2192}after"), "{}", next.text);
    }

    #[test]
    fn cutting_a_long_line_never_splits_a_character() {
        let root = workspace("utf8cut");
        // Every character is 3 bytes, so no budget below the line length
        // lands on a boundary by luck.
        write(&root, "cjk.txt", format!("{}\n", "中".repeat(100)));
        for budget in 1..=40u32 {
            let c = read(
                &root,
                &ReadFileArgs {
                    max_bytes: Some(budget),
                    ..args("cjk.txt")
                },
            );
            assert!(c.bytes <= budget as usize);
            assert_eq!(c.bytes % 3, 0, "budget {budget} cut mid-character");
            // The rendered text must still be valid UTF-8 by construction.
            assert!(c.text.contains('\u{2192}'));
        }
    }

    #[test]
    fn crlf_is_reported_and_stripped_from_the_numbered_output() {
        let root = workspace("crlf");
        write(&root, "a.txt", "one\r\ntwo\r\n");
        let c = read(&root, &args("a.txt"));
        assert_eq!(c.line_ending, LineEnding::Crlf);
        assert_eq!(
            strip_version(&c.text),
            "1\u{2192}one\n2\u{2192}two\n[end of file, 2 lines]"
        );
        assert!(
            !c.text.contains('\r'),
            "a stray CR would be invisible noise"
        );
    }

    #[test]
    fn mixed_line_endings_are_called_mixed() {
        let root = workspace("mixed");
        write(&root, "a.txt", "one\r\ntwo\nthree\r\n");
        assert_eq!(read(&root, &args("a.txt")).line_ending, LineEnding::Mixed);
    }

    #[test]
    fn a_missing_final_newline_is_reported() {
        let root = workspace("nonl");
        write(&root, "a.txt", "one\ntwo");
        let c = read(&root, &args("a.txt"));
        assert_eq!(c.lines, 2);
        assert_eq!(c.final_newline, Some(false));
        assert_eq!(c.total_lines, Some(2));
        // A file that is one unterminated line has no line ending at all.
        write(&root, "b.txt", "solo");
        let c = read(&root, &args("b.txt"));
        assert_eq!(c.line_ending, LineEnding::None);
        assert_eq!(c.final_newline, Some(false));
    }

    #[test]
    fn a_bom_is_stripped_and_flagged() {
        let root = workspace("bom");
        write(
            &root,
            "a.txt",
            b"\xef\xbb\xbfuse std;\nfn main() {}\n".as_slice(),
        );
        let c = read(&root, &args("a.txt"));
        assert!(c.text.starts_with("1\u{2192}use std;"), "{}", c.text);
        assert!(c.notes.iter().any(|n| n.contains("BOM")), "{:?}", c.notes);
    }

    #[test]
    fn a_binary_file_is_refused_before_it_costs_context() {
        let root = workspace("binary");
        write(
            &root,
            "a.bin",
            b"\x7fELF\x02\x01\x01\x00\x00\x00rest".as_slice(),
        );
        let e = err(&root, &args("a.bin"));
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("binary"), "{e}");
        assert!(e.message().contains("offset 7"), "{e}");
    }

    #[test]
    fn non_utf8_text_is_read_lossily_rather_than_refused() {
        let root = workspace("latin1");
        // "café" in latin-1: 0xe9 is not valid UTF-8 but is not binary.
        write(&root, "a.txt", b"caf\xe9\nnext\n".as_slice());
        let c = read(&root, &args("a.txt"));
        assert_eq!(c.lines, 2);
        assert!(c.text.contains('\u{fffd}'), "{}", c.text);
        assert!(
            c.notes.iter().any(|n| n.contains("not valid UTF-8")),
            "{:?}",
            c.notes
        );
    }

    #[test]
    fn a_directory_and_a_fifo_are_both_refused_without_blocking() {
        let root = workspace("types");
        fs::create_dir(root.join("sub")).unwrap();
        let e = err(&root, &args("sub"));
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("is a directory"), "{e}");

        // Opening a fifo with no writer blocks forever. If this test hangs,
        // the type check moved after the open and every later tool call in
        // a real session would hang with it.
        let fifo = root.join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(made, "mkfifo is needed for this test");
        let e = err(&root, &args("pipe"));
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("not a regular file"), "{e}");
    }

    #[test]
    fn an_unreadable_file_is_the_callers_problem_not_an_internal_error() {
        let root = workspace("perm");
        write(&root, "a.txt", "secret\n");
        let path = root.join("a.txt");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        fs::set_permissions(&path, perms).unwrap();
        // Running as root would read it anyway; then there is nothing to
        // assert and skipping is honest.
        if File::open(&path).is_ok() {
            return;
        }
        let e = err(&root, &args("a.txt"));
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("cannot read a.txt"), "{e}");
    }

    #[test]
    fn path_policy_errors_reach_the_caller_unchanged() {
        let root = workspace("policy");
        let e = err(&root, &args("../../etc/passwd"));
        assert_eq!(e.code(), ErrorCode::Policy);
        let e = err(&root, &args("/etc/passwd"));
        assert_eq!(e.code(), ErrorCode::Policy);
        let e = err(&root, &args("nope.txt"));
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn a_large_file_is_not_read_into_memory() {
        let root = workspace("large");
        // 4 MiB across 40k lines. The point is not the size but that the
        // read costs a constant amount: only 200 lines come back and the
        // scan stops there.
        let body: String = (1..=40_000).map(|n| format!("{n:0>99}\n")).collect();
        write(&root, "big.txt", &body);
        let started = std::time::Instant::now();
        let c = read(&root, &args("big.txt"));
        assert_eq!(c.lines, DEFAULT_MAX_LINES);
        assert!(c.bytes <= DEFAULT_MAX_BYTES);
        assert_eq!(c.file_bytes, 4_000_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "reading the head of a 4 MiB file took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn seeking_absurdly_far_into_a_file_is_refused_instead_of_grinding() {
        let root = workspace("scan");
        write(&root, "a.txt", "l1\nl2\n");
        // The bound is on bytes walked, not on the line number, so a small
        // file with a huge start_line is fine.
        let c = read(
            &root,
            &ReadFileArgs {
                start_line: Some(u32::MAX),
                ..args("a.txt")
            },
        );
        assert_eq!(c.lines, 0);
        assert_eq!(c.total_lines, Some(2));
    }
}

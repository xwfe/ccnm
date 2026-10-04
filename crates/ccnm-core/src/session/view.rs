//! The retained view of one output stream of a finished print session (P59).
//!
//! What leaves the Agent is never the raw file. It is a copy made once, the
//! first time anyone asks after the session ended:
//!
//! - **redacted as a whole**, with [`AgentProvider::output_redaction`] -- the
//!   same rule the short tails in a report use. Doing it per page would let a
//!   private path that straddles two pages out half at a time.
//! - **valid UTF-8**: bytes that are not become U+FFFD, exactly as
//!   `String::from_utf8_lossy` would, so every page can be a JSON string and
//!   byte offsets into the view never cut a character.
//! - **bounded**: only the last [`RAW_CAP`] bytes of the stream are kept.
//!   The head is dropped at a character boundary and the view says so.
//! - **frozen**: built from the length the file had when it was built. A
//!   stray process still appending afterwards changes nothing, so every page
//!   read later comes from one fixed document.
//!
//! Everything streams through fixed buffers: memory stays at a buffer or two
//! whatever the size of the output.
//!
//! [`AgentProvider::output_redaction`]: crate::provider::AgentProvider::output_redaction

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Control, Dir};
use crate::error::{Error, Result};
use crate::provider::{REDACTED, Redaction};

/// How much of a stream is kept: its last 32 MiB. Enough for any report a
/// person reads; a build log larger than that loses its head, and says so.
pub const RAW_CAP: u64 = 32 * 1024 * 1024;

const BUFFER: usize = 64 * 1024;
const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub fn name(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// What a view is, recorded next to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Changes whenever a view is built, so a reader can tell "the same
    /// document" from "a document of the same length".
    pub generation: String,
    pub view_bytes: u64,
    /// How many bytes the process had written to the stream.
    pub source_bytes: u64,
    /// Part of what was written is not in the view, for good.
    pub source_truncated: bool,
}

fn view_path(dir: &Dir, stream: Stream) -> PathBuf {
    dir.path().join(format!("{}.view", stream.name()))
}

fn meta_path(dir: &Dir, stream: Stream) -> PathBuf {
    dir.path().join(format!("{}.view.json", stream.name()))
}

fn read_meta(dir: &Dir, stream: Stream) -> Option<Meta> {
    let bytes = fs::read(meta_path(dir, stream)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The view of `stream`, built now if it does not exist yet.
///
/// `ran`: the Agent was started. The supervisor creates both streams before
/// it starts the Agent, so for such a session a stream that is not there was
/// lost, and building a view of it is refused (F23): read as 0 bytes, it
/// reached the caller as "empty and complete". A session that never started
/// never had one; its view is empty, because that is what it printed.
pub fn ensure(dir: &Dir, stream: Stream, redaction: &Redaction, ran: bool) -> Result<Meta> {
    ensure_with_cap(dir, stream, redaction, ran, RAW_CAP)
}

pub(crate) fn ensure_with_cap(
    dir: &Dir,
    stream: Stream,
    redaction: &Redaction,
    ran: bool,
    cap: u64,
) -> Result<Meta> {
    if let Some(meta) = read_meta(dir, stream) {
        return Ok(meta);
    }
    // Two readers asking at once build it once: the second one waits here
    // and then finds the first one's.
    let _control = Control::lock(dir)?;
    if let Some(meta) = read_meta(dir, stream) {
        return Ok(meta);
    }
    let raw_path = match stream {
        Stream::Stdout => dir.stdout(),
        Stream::Stderr => dir.stderr(),
    };
    let mut raw = match fs::File::open(&raw_path) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && ran => {
            return Err(Error::internal(format!(
                "the session's {} is gone: it was created when the Agent started and is no longer at {}, so there is no complete output to serve",
                stream.name(),
                raw_path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let source_bytes = match &raw {
        Some(file) => file.metadata()?.len(),
        None => 0,
    };
    let tmp = dir.path().join(format!(
        ".{}.view.{}.tmp",
        stream.name(),
        uuid::Uuid::new_v4().simple()
    ));
    let built = (|| -> Result<(u64, bool)> {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let made = match raw.as_mut() {
            Some(file) => build(file, source_bytes, redaction, cap, &mut out)?,
            None => build(&mut std::io::empty(), 0, redaction, cap, &mut out)?,
        };
        out.sync_all()?;
        Ok(made)
    })();
    let (view_bytes, source_truncated) = match built {
        Ok(made) => made,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    fs::rename(&tmp, view_path(dir, stream))?;
    let meta = Meta {
        generation: uuid::Uuid::new_v4().simple().to_string(),
        view_bytes,
        source_bytes,
        source_truncated,
    };
    // The meta is the commit point: a view file without one is rebuilt.
    let meta_tmp = dir.path().join(format!(".{}.view.json.tmp", stream.name()));
    fs::write(
        &meta_tmp,
        serde_json::to_vec(&meta)
            .map_err(|e| Error::internal("cannot encode a view record").with_source(e))?,
    )?;
    fs::rename(&meta_tmp, meta_path(dir, stream))?;
    Ok(meta)
}

/// Up to `limit` bytes of the view from `offset`. The caller asked for this
/// exact generation; a view that does not match it is an error, not a
/// quietly different document.
pub fn read(dir: &Dir, stream: Stream, meta: &Meta, offset: u64, limit: u64) -> Result<Vec<u8>> {
    let mut file = fs::File::open(view_path(dir, stream))?;
    if file.metadata()?.len() != meta.view_bytes {
        return Err(Error::internal(
            "the retained output changed under its record",
        ));
    }
    if offset > meta.view_bytes {
        return Err(Error::invalid_args(
            "offset is past the end of the retained output",
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.take(limit.min(meta.view_bytes - offset))
        .read_to_end(&mut data)?;
    Ok(data)
}

/// Write the view of the last `cap` bytes of `raw` (which is `raw_len`
/// long) to `out`. Returns the view's length and whether anything written
/// to the stream is missing from it.
pub(crate) fn build<R: Read + Seek, W: Write>(
    raw: &mut R,
    raw_len: u64,
    redaction: &Redaction,
    cap: u64,
    out: &mut W,
) -> Result<(u64, bool)> {
    let pattern = match redaction {
        Redaction::Withhold(why) => {
            out.write_all(why.as_bytes())?;
            return Ok((why.len() as u64, true));
        }
        Redaction::Keep => None,
        Redaction::Replace(private) => Some(private.as_bytes().to_vec()),
    };
    let mut cut = raw_len.saturating_sub(cap);
    let truncated = cut > 0;
    if truncated {
        // Start on a character, not in the middle of one.
        raw.seek(SeekFrom::Start(cut))?;
        let mut probe = [0u8; 3];
        let got = raw.read(&mut probe)?;
        cut += probe[..got]
            .iter()
            .take_while(|b| is_continuation(**b))
            .count() as u64;
    }
    // Begin a pattern's length early, so a private path that straddles the
    // cut is still recognised and replaced whole instead of leaving its
    // second half in the view.
    let lead = pattern.as_ref().map_or(0, |p| p.len().saturating_sub(1)) as u64;
    let from = cut.saturating_sub(lead);
    raw.seek(SeekFrom::Start(from))?;
    let mut redactor = Redactor::new(pattern, from, cut);
    let mut utf8 = Utf8Normalizer::default();
    let (mut redacted, mut clean) = (Vec::new(), Vec::new());
    let mut written = 0u64;
    let mut source = raw.take(raw_len - from);
    let mut buffer = vec![0u8; BUFFER];
    loop {
        let n = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        redactor.feed(&buffer[..n], &mut redacted);
        utf8.feed(&redacted, &mut clean);
        redacted.clear();
        out.write_all(&clean)?;
        written += clean.len() as u64;
        clean.clear();
    }
    redactor.finish(&mut redacted);
    utf8.feed(&redacted, &mut clean);
    utf8.finish(&mut clean);
    out.write_all(&clean)?;
    written += clean.len() as u64;
    Ok((written, truncated))
}

fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// `str::replace` of one pattern, fed a chunk at a time.
///
/// Holds back the last `pattern.len() - 1` bytes of each chunk, because a
/// match may start there and end in the next one. Input before
/// `skip_before` is not emitted, unless it is part of a match that ends
/// after it -- then the whole placeholder is.
struct Redactor {
    pattern: Option<Vec<u8>>,
    pending: Vec<u8>,
    /// Input offset of `pending[0]`.
    at: u64,
    skip_before: u64,
}

impl Redactor {
    fn new(pattern: Option<Vec<u8>>, at: u64, skip_before: u64) -> Self {
        Redactor {
            pattern: pattern.filter(|p| !p.is_empty()),
            pending: Vec::new(),
            at,
            skip_before,
        }
    }

    fn feed(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        self.pending.extend_from_slice(chunk);
        self.drain(false, out);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        self.drain(true, out);
    }

    fn drain(&mut self, last: bool, out: &mut Vec<u8>) {
        let n = self.pending.len();
        let mut i = 0;
        match &self.pattern {
            None => {
                let keep_from = self.skip_before.saturating_sub(self.at).min(n as u64) as usize;
                out.extend_from_slice(&self.pending[keep_from..]);
                i = n;
            }
            Some(pattern) => {
                while i < n {
                    if !last && n - i < pattern.len() {
                        break;
                    }
                    if self.pending[i..].starts_with(pattern) {
                        if self.at + (i + pattern.len()) as u64 > self.skip_before {
                            out.extend_from_slice(REDACTED.as_bytes());
                        }
                        i += pattern.len();
                    } else {
                        if self.at + i as u64 >= self.skip_before {
                            out.push(self.pending[i]);
                        }
                        i += 1;
                    }
                }
            }
        }
        self.pending.drain(..i);
        self.at += i as u64;
    }
}

/// `String::from_utf8_lossy`, fed a chunk at a time: an incomplete sequence
/// at the end of a chunk waits for the next one instead of being replaced.
#[derive(Default)]
struct Utf8Normalizer {
    carry: Vec<u8>,
}

impl Utf8Normalizer {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        self.carry.extend_from_slice(bytes);
        let mut used = 0;
        loop {
            let rest = &self.carry[used..];
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.extend_from_slice(text.as_bytes());
                    used += rest.len();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.extend_from_slice(&rest[..valid]);
                    match e.error_len() {
                        Some(bad) => {
                            out.extend_from_slice(REPLACEMENT);
                            used += valid + bad;
                        }
                        // Cut off, not invalid: maybe the next chunk finishes it.
                        None => {
                            used += valid;
                            break;
                        }
                    }
                }
            }
        }
        self.carry.drain(..used);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.carry.is_empty() {
            out.extend_from_slice(REPLACEMENT);
            self.carry.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Deterministic bytes that include every kind of trouble: ASCII,
    /// multi-byte characters, stray continuation bytes, truncated lead
    /// bytes, 0xFF.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let alphabet: [&[u8]; 8] = [
            b"a",
            b"\n",
            "中".as_bytes(),
            "😀".as_bytes(),
            &[0x80],
            &[0xE4, 0xB8],
            &[0xFF],
            b"/private/dir",
        ];
        let mut state = seed;
        let mut out = Vec::new();
        while out.len() < len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.extend_from_slice(alphabet[(state >> 60) as usize % alphabet.len()]);
        }
        out
    }

    fn in_chunks(input: &[u8], size: usize, redaction: &Redaction) -> Vec<u8> {
        let pattern = match redaction {
            Redaction::Replace(p) => Some(p.as_bytes().to_vec()),
            _ => None,
        };
        let mut redactor = Redactor::new(pattern, 0, 0);
        let mut utf8 = Utf8Normalizer::default();
        let (mut mid, mut out) = (Vec::new(), Vec::new());
        for chunk in input.chunks(size.max(1)) {
            redactor.feed(chunk, &mut mid);
            utf8.feed(&mid, &mut out);
            mid.clear();
        }
        redactor.finish(&mut mid);
        utf8.feed(&mid, &mut out);
        utf8.finish(&mut out);
        out
    }

    /// Streaming gives exactly what redacting and decoding the whole thing
    /// at once gives, however the input is cut into chunks -- including a
    /// private path or a character cut in half at a chunk edge.
    #[test]
    fn streaming_matches_the_whole_document_at_any_chunk_size() {
        let redaction = Redaction::Replace("/private/dir".into());
        for seed in 0..20 {
            let input = noise(4000, seed);
            let whole = String::from_utf8_lossy(&input).replace("/private/dir", REDACTED);
            for size in [1, 2, 3, 5, 7, 11, 64, 4096] {
                let got = in_chunks(&input, size, &redaction);
                assert_eq!(got, whole.as_bytes(), "seed {seed}, chunk {size}");
            }
        }
    }

    fn view_of(raw: &[u8], redaction: &Redaction, cap: u64) -> (String, bool) {
        let mut out = Vec::new();
        let (len, truncated) = build(
            &mut Cursor::new(raw),
            raw.len() as u64,
            redaction,
            cap,
            &mut out,
        )
        .unwrap();
        assert_eq!(len, out.len() as u64);
        (
            String::from_utf8(out).expect("a view is always valid UTF-8"),
            truncated,
        )
    }

    #[test]
    fn a_small_stream_is_kept_whole() {
        let (view, truncated) = view_of("中文 and ascii\n".as_bytes(), &Redaction::Keep, 1024);
        assert_eq!(view, "中文 and ascii\n");
        assert!(!truncated);
        assert_eq!(view_of(b"", &Redaction::Keep, 1024), (String::new(), false));
    }

    /// Over the cap, the head goes; the cut lands on a character, and a
    /// private path straddling it is replaced whole, not left half visible.
    #[test]
    fn over_the_cap_the_head_goes_at_a_character_and_nothing_private_survives() {
        let private = "/Users/someone/.claude-private";
        let raw = format!("{}{private}中文tail", "x".repeat(100));
        let cap_inside_path = (raw.len() - raw.find(private).unwrap() - 5) as u64;
        let (view, truncated) = view_of(
            raw.as_bytes(),
            &Redaction::Replace(private.into()),
            cap_inside_path,
        );
        assert!(truncated);
        assert!(
            !view.contains("claude-private") && !view.contains("someone"),
            "{view}"
        );
        assert!(view.starts_with(REDACTED), "{view}");
        assert!(view.ends_with("中文tail"));

        // A cut in the middle of 中: the view starts at the next character.
        let raw = "ab中文".as_bytes();
        let (view, truncated) = view_of(raw, &Redaction::Keep, 5);
        assert!(truncated);
        assert_eq!(view, "文");
    }

    #[test]
    fn a_withheld_stream_says_why_and_counts_as_incomplete() {
        let (view, truncated) = view_of(b"secret", &Redaction::Withhold("withheld: reason"), 1024);
        assert_eq!(view, "withheld: reason");
        assert!(truncated);
    }

    fn session_dir(test: &str) -> ccnm_testdir::TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-view-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        ccnm_testdir::TestDir::adopt(dir)
    }

    /// Built once: a second call returns the same generation, and bytes
    /// appended to the stream afterwards are not in it.
    #[test]
    fn a_view_is_built_once_and_frozen() {
        let root = session_dir("frozen");
        let dir = Dir::at(root.to_path_buf());
        fs::write(dir.stdout(), "first\n").unwrap();
        let meta = ensure(&dir, Stream::Stdout, &Redaction::Keep, true).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(dir.stdout())
            .unwrap()
            .write_all(b"late\n")
            .unwrap();
        let again = ensure(&dir, Stream::Stdout, &Redaction::Keep, true).unwrap();
        assert_eq!(meta, again);
        assert_eq!(
            read(&dir, Stream::Stdout, &meta, 0, 100).unwrap(),
            b"first\n"
        );
        assert_eq!(meta.source_bytes, 6);
        assert_eq!(read(&dir, Stream::Stdout, &meta, 2, 2).unwrap(), b"rs");
        assert!(read(&dir, Stream::Stdout, &meta, 7, 1).is_err());

        // Built once, it no longer needs the raw file.
        fs::remove_file(dir.stdout()).unwrap();
        assert_eq!(
            ensure(&dir, Stream::Stdout, &Redaction::Keep, true).unwrap(),
            meta
        );
    }

    /// A stream with no file: lost if the Agent ran (F23), nothing at all
    /// if it never started. Only the second is an empty view.
    #[test]
    fn a_missing_stream_is_empty_only_for_a_session_that_never_ran() {
        let root = session_dir("missing");
        let dir = Dir::at(root.to_path_buf());
        let err = ensure(&dir, Stream::Stderr, &Redaction::Keep, true).unwrap_err();
        assert!(err.message().contains("stderr is gone"), "{err}");
        assert!(read_meta(&dir, Stream::Stderr).is_none());
        assert!(!view_path(&dir, Stream::Stderr).exists());

        let never = ensure(&dir, Stream::Stderr, &Redaction::Keep, false).unwrap();
        assert_eq!(
            (never.view_bytes, never.source_bytes, never.source_truncated),
            (0, 0, false)
        );

        // An empty file is a stream the Agent wrote nothing to: empty, whole.
        fs::write(dir.stdout(), "").unwrap();
        let empty = ensure(&dir, Stream::Stdout, &Redaction::Keep, true).unwrap();
        assert_eq!(
            (empty.view_bytes, empty.source_bytes, empty.source_truncated),
            (0, 0, false)
        );
    }

    #[test]
    fn a_view_built_under_a_smaller_cap_reports_what_it_dropped() {
        let root = session_dir("cap");
        let dir = Dir::at(root.to_path_buf());
        fs::write(dir.stderr(), "0123456789").unwrap();
        let meta = ensure_with_cap(&dir, Stream::Stderr, &Redaction::Keep, true, 4).unwrap();
        assert_eq!(
            (meta.view_bytes, meta.source_bytes, meta.source_truncated),
            (4, 10, true)
        );
        assert_eq!(read(&dir, Stream::Stderr, &meta, 0, 10).unwrap(), b"6789");
    }
}

//! The MCP side of ccnm: the stdio server that runs on the home machine
//! (`ccnm internal mcp-serve`) and the client used to probe it.
//!
//! This is the only async code in the binary. Both entry points build a
//! current-thread tokio runtime, do their work inside one `block_on`, and
//! hand a plain `Result` back to synchronous callers (design doc section
//! 25). MCP JSON-RPC goes straight over stdin/stdout; the control
//! protocol's base64 payload is consumed once, before the first byte of
//! MCP (section 9).

pub mod glob;
pub mod list;
pub mod patch;
pub mod path;
pub mod probe;
pub mod read;
pub mod search;
pub mod server;

/// The longest prefix of `s` that fits in `max` bytes without splitting a
/// character.
///
/// Every tool that returns text needs this, and the reason is the same each
/// time: `&s[..max]` panics when `max` lands inside a multi-byte character,
/// and that is the ordinary case, not the exotic one. A file with an
/// accented word, a comment in Chinese or an emoji in a string hits it the
/// moment a byte budget runs out mid-line. `str::floor_char_boundary` would
/// do this, but it is still unstable.
/// An opaque token that changes whenever a file is written.
///
/// `read_file` returns it and `apply_patch` requires it back, which is how
/// a patch built on content the user has since changed is refused instead
/// of applied (design doc section 15).
///
/// It is size and modification time, **not** a hash of the content, and the
/// difference is deliberate. `read_file` streams: it can answer about the
/// first 200 lines of a 2 GB file without reading the rest, and hashing
/// would throw that away for every call. Size and mtime come out of the
/// `stat` the tool already does, so staleness detection is free.
///
/// What that buys and what it costs: every write to the file changes its
/// mtime, so no real edit slips past. A file restored from a backup, or
/// copied with its timestamps, can look changed when its content is not —
/// a false alarm, which is the safe direction. Two writes inside the same
/// nanosecond that leave the size identical would slip past, which on a
/// filesystem with nanosecond timestamps is not a thing that happens.
pub(crate) fn version_of(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{mtime:x}", meta.len())
}

pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::truncate_bytes;

    #[test]
    fn truncate_bytes_stops_on_a_character_boundary() {
        let s = "a中b";
        assert_eq!(truncate_bytes(s, 0), "");
        assert_eq!(truncate_bytes(s, 1), "a");
        // 2 and 3 land inside the three-byte character, so both give "a".
        assert_eq!(truncate_bytes(s, 2), "a");
        assert_eq!(truncate_bytes(s, 3), "a");
        assert_eq!(truncate_bytes(s, 4), "a中");
        assert_eq!(truncate_bytes(s, 99), s);
        assert_eq!(truncate_bytes("", 5), "");
    }
}

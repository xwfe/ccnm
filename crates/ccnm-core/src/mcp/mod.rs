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

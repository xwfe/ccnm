//! The glob syntax `list_files` and `search_text` accept.
//!
//! Written here rather than pulled in as a crate because it is a small
//! pure function with a large blast radius, and because the alternative
//! that needs no dependency — handing the pattern to `git ls-files` as a
//! pathspec — would give one behaviour inside a git repository and
//! another outside it.
//!
//! Supported, and nothing else:
//!
//! ```text
//! *        any run of characters inside one path segment
//! **       any run of segments, including none
//! ?        exactly one character inside one path segment
//! {a,b}    alternation, nestable; expanded before matching
//! ```
//!
//! Character classes (`[a-z]`) are refused rather than treated as
//! literals. A pattern that silently matches nothing is the worst
//! outcome: the model concludes the files do not exist.
//!
//! Matching is a table, not recursion. `a/**/**/**/**/b` against a deep
//! path is the classic way to make a backtracking matcher hang, and this
//! runtime answers a model that can send any pattern it likes.

use crate::error::{Error, Result};

/// Ceiling on what one `{a,b}` pattern may expand to. `{a,b}{c,d}{e,f}…`
/// doubles per group, so without a bound a short pattern can ask for
/// millions of matches.
const MAX_ALTERNATIVES: usize = 64;

/// Ceiling on pattern segments, so the match table stays small.
const MAX_SEGMENTS: usize = 64;

/// A compiled glob: one or more alternatives, each a list of segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    source: String,
    alternatives: Vec<Vec<String>>,
    /// The pattern has a `/` other than a trailing one. See
    /// [`Glob::matches_file_as_rg`].
    rooted: bool,
    /// The pattern ends with `/`: under ripgrep's rules it names
    /// directories only.
    dir_only: bool,
}

impl Glob {
    /// Compile `pattern`, or explain why it cannot be used.
    pub fn new(pattern: &str) -> Result<Glob> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Err(Error::invalid_args("glob is empty"));
        }
        if pattern.contains('\0') {
            return Err(Error::invalid_args("glob contains a NUL byte"));
        }
        if pattern.contains('[') || pattern.contains(']') {
            return Err(Error::invalid_args(format!(
                "glob {pattern} uses a character class; ccnm supports *, **, ? and {{a,b}} only"
            )));
        }
        if pattern.starts_with('/') {
            return Err(Error::invalid_args(format!(
                "glob {pattern} starts with /; globs are relative to the listed directory"
            )));
        }
        if pattern.contains("..") {
            return Err(Error::invalid_args(format!(
                "glob {pattern} contains `..`; globs cannot leave the workspace"
            )));
        }

        let mut alternatives = Vec::new();
        for expanded in expand_braces(pattern)? {
            let segments: Vec<String> = expanded
                .split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .map(str::to_string)
                .collect();
            if segments.is_empty() {
                return Err(Error::invalid_args(format!(
                    "glob {pattern} does not name anything"
                )));
            }
            if segments.len() > MAX_SEGMENTS {
                return Err(Error::invalid_args(format!(
                    "glob {pattern} has more than {MAX_SEGMENTS} path segments"
                )));
            }
            alternatives.push(segments);
        }
        Ok(Glob {
            source: pattern.to_string(),
            alternatives,
            rooted: pattern.trim_end_matches('/').contains('/'),
            dir_only: pattern.ends_with('/'),
        })
    }

    /// The pattern as the caller wrote it, for error and footer text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Does `path` match? `path` is a `/`-separated relative path.
    pub fn matches(&self, path: &str) -> bool {
        let text: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        self.alternatives
            .iter()
            .any(|segments| match_segments(segments, &text))
    }

    /// Does the file at `path` (relative to the workspace root) match the
    /// way ripgrep's `--glob` would have matched it? Those are gitignore's
    /// rules, and `search_text` promised them before it stopped handing
    /// the glob to rg (P38):
    ///
    /// ```text
    /// *.rs         no `/`: the file name, at any depth
    /// src/*.rs     a `/`: the whole path from the root
    /// src/         a trailing `/`: directories only, so never a file
    /// ```
    ///
    /// Whether a pattern is rooted is decided on the pattern as written,
    /// before braces are expanded, as gitignore decides it: in
    /// `{*.rs,src/*.py}` the `*.rs` is rooted too.
    pub fn matches_file_as_rg(&self, path: &str) -> bool {
        if self.dir_only {
            return false;
        }
        if self.rooted {
            return self.matches(path);
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        self.alternatives
            .iter()
            .any(|segments| match_segments(segments, &[name]))
    }

    /// The last segment of every alternative, deduplicated: what a file's
    /// name has to match, whatever directory it is in.
    pub fn file_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for segments in &self.alternatives {
            if let Some(last) = segments.last()
                && !names.contains(&last.as_str())
            {
                names.push(last);
            }
        }
        names
    }
}

/// `src/{a,b}/*.rs` -> `src/a/*.rs`, `src/b/*.rs`. Nesting works because
/// the function recurses on the remainder after each substitution.
fn expand_braces(pattern: &str) -> Result<Vec<String>> {
    let Some(open) = pattern.find('{') else {
        if pattern.contains('}') {
            return Err(Error::invalid_args(format!(
                "glob {pattern} has a `}}` with no `{{`"
            )));
        }
        return Ok(vec![pattern.to_string()]);
    };
    // Find the `}` that closes this `{`, allowing nested groups.
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in pattern.char_indices().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return Err(Error::invalid_args(format!(
            "glob {pattern} has a `{{` with no `}}`"
        )));
    };

    let (head, rest) = (&pattern[..open], &pattern[close + 1..]);
    let body = &pattern[open + 1..close];
    let mut out = Vec::new();
    for choice in split_alternatives(body) {
        for tail in expand_braces(&format!("{choice}{rest}"))? {
            out.push(format!("{head}{tail}"));
            if out.len() > MAX_ALTERNATIVES {
                return Err(Error::invalid_args(format!(
                    "glob {pattern} expands to more than {MAX_ALTERNATIVES} patterns"
                )));
            }
        }
    }
    Ok(out)
}

/// Split `a,b,{c,d}` on the commas that belong to this level.
fn split_alternatives(body: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&body[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&body[start..]);
    parts
}

/// Do `pattern` segments match `text` segments? A table over
/// (pattern index, text index), filled from the end, so `**` costs a cell
/// rather than a branch and no pattern can make this run long.
fn match_segments(pattern: &[String], text: &[&str]) -> bool {
    let (n, m) = (pattern.len(), text.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[n][m] = true;
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            if i == n {
                // Pattern exhausted: only an exhausted path matches.
                continue;
            }
            dp[i][j] = if pattern[i] == "**" {
                // Skip this `**`, or let it swallow one more segment.
                dp[i + 1][j] || (j < m && dp[i][j + 1])
            } else {
                j < m && match_one(&pattern[i], text[j]) && dp[i + 1][j + 1]
            };
        }
    }
    dp[0][0]
}

/// `*` and `?` inside a single segment. The standard linear-space
/// wildcard match: remember the last `*` and restart one character later
/// when the tail stops matching, which is O(pattern × text) and cannot
/// blow up.
fn match_one(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut resume = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            resume = ti;
            pi += 1;
        } else if let Some(s) = star {
            resume += 1;
            pi = s + 1;
            ti = resume;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn g(pattern: &str) -> Glob {
        Glob::new(pattern).unwrap()
    }

    fn refused(pattern: &str) -> Error {
        match Glob::new(pattern) {
            Err(e) => e,
            Ok(_) => panic!("{pattern} should have been refused"),
        }
    }

    #[test]
    fn star_stays_inside_one_segment() {
        let p = g("*.rs");
        assert!(p.matches("main.rs"));
        assert!(!p.matches("src/main.rs"), "* must not cross a slash");
        assert!(!p.matches("main.rs.bak"));
        assert!(g("src/*.rs").matches("src/main.rs"));
        assert!(!g("src/*.rs").matches("src/mcp/main.rs"));
    }

    #[test]
    fn double_star_crosses_segments_and_may_match_none() {
        let p = g("**/*.rs");
        assert!(p.matches("main.rs"), "**/ must be allowed to match nothing");
        assert!(p.matches("src/main.rs"));
        assert!(p.matches("crates/core/src/mcp/read.rs"));
        assert!(!p.matches("src/main.toml"));

        assert!(g("src/**").matches("src/a"));
        assert!(g("src/**").matches("src/a/b/c"));
        assert!(!g("src/**").matches("tests/a"));
        assert!(g("**").matches("anything/at/all"));
    }

    #[test]
    fn question_mark_is_exactly_one_character() {
        assert!(g("a?c.rs").matches("abc.rs"));
        assert!(!g("a?c.rs").matches("ac.rs"));
        assert!(!g("a?c.rs").matches("abbc.rs"));
        assert!(!g("?").matches("a/b"), "? must not match a slash");
    }

    #[test]
    fn braces_expand_and_nest() {
        let p = g("**/*.{rs,toml}");
        assert!(p.matches("Cargo.toml"));
        assert!(p.matches("src/main.rs"));
        assert!(!p.matches("README.md"));

        let p = g("{src,tests}/{a,b}.rs");
        for ok in ["src/a.rs", "src/b.rs", "tests/a.rs", "tests/b.rs"] {
            assert!(p.matches(ok), "{ok}");
        }
        assert!(!p.matches("src/c.rs"));

        assert!(g("x/{a,{b,c}}.rs").matches("x/c.rs"), "nested braces");
    }

    #[test]
    fn unsupported_or_dangerous_syntax_is_refused_not_ignored() {
        // Treating these as literals would match nothing and the model
        // would conclude the files are not there.
        for (pattern, needle) in [
            ("src/[abc].rs", "character class"),
            ("/etc/*.conf", "starts with /"),
            ("../*.rs", "`..`"),
            ("src/{a,b.rs", "no `}`"),
            ("src/a,b}.rs", "no `{`"),
            ("", "empty"),
            ("{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}", "more than 64"),
        ] {
            let e = refused(pattern);
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{pattern}");
            assert!(e.message().contains(needle), "{pattern} -> {e}");
        }
    }

    #[test]
    fn a_pattern_built_to_backtrack_still_returns() {
        // A recursive matcher takes exponential time on this. The table
        // does not, and a model can send whatever it likes.
        let p = g("a/**/**/**/**/**/**/**/**/b");
        let path = format!("a/{}/c", vec!["x"; 200].join("/"));
        let started = std::time::Instant::now();
        assert!(!p.matches(&path));
        assert!(p.matches("a/x/x/x/b"));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_segment_of_only_stars_still_terminates() {
        let p = g("**/*******x");
        assert!(p.matches("a/b/yyyyx"));
        assert!(!p.matches("a/b/yyyy"));
    }

    #[test]
    fn redundant_syntax_is_normalized() {
        assert!(g("./src/*.rs").matches("src/main.rs"));
        assert!(g("src//*.rs").matches("src/main.rs"));
        assert_eq!(g("  src/*.rs  ").source(), "src/*.rs");
    }

    #[test]
    fn matching_is_case_sensitive_and_dot_is_not_special() {
        assert!(!g("*.RS").matches("main.rs"));
        // Unlike a shell, a leading dot is matched by `*`; hiding dotfiles
        // is list_files's job, not the pattern's.
        assert!(g("*").matches(".gitignore"));
    }

    /// The rules ripgrep's `--glob` applied, measured on rg 15.2.0 (P38
    /// record): no `/` is the file name anywhere, a `/` anchors at the
    /// root, a trailing `/` is directories only.
    #[test]
    fn matching_a_file_as_rg_would() {
        let rs = g("*.rs");
        assert!(rs.matches_file_as_rg("main.rs"));
        assert!(rs.matches_file_as_rg("src/mcp/read.rs"), "any depth");
        assert!(!rs.matches_file_as_rg("src/main.toml"));

        let rooted = g("src/*.rs");
        assert!(rooted.matches_file_as_rg("src/a.rs"));
        assert!(!rooted.matches_file_as_rg("src/nested/b.rs"));
        assert!(
            !rooted.matches_file_as_rg("a/src/x.rs"),
            "anchored at the root"
        );
        assert!(g("**/src/*.rs").matches_file_as_rg("a/src/x.rs"));
        assert!(g("src/**").matches_file_as_rg("src/nested/b.rs"));

        assert!(!g("src/").matches_file_as_rg("src"), "directories only");
        assert!(!g("src/").matches_file_as_rg("src/a.rs"));

        // Rooted is a property of the pattern as written.
        let mixed = g("{*.rs,src/*.py}");
        assert!(mixed.matches_file_as_rg("a.rs"));
        assert!(!mixed.matches_file_as_rg("src/a.rs"));
        assert!(mixed.matches_file_as_rg("src/b.py"));

        assert!(g("*").matches_file_as_rg("deep/down/file"));
        assert!(g("**").matches_file_as_rg("deep/down/file"));
    }

    #[test]
    fn file_names_are_the_last_segments_once_each() {
        assert_eq!(g("**/*.{rs,toml}").file_names(), ["*.rs", "*.toml"]);
        assert_eq!(g("{src,tests}/*.rs").file_names(), ["*.rs"]);
        assert_eq!(g("src/**").file_names(), ["**"]);
    }

    #[test]
    fn non_ascii_names_match_by_character_not_byte() {
        assert!(g("文档/*.md").matches("文档/说明.md"));
        assert!(
            g("?.md").matches("中.md"),
            "? is one character, not one byte"
        );
    }
}

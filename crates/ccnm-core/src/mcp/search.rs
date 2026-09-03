//! `search_text`: the third phase 2 tool (design doc section 15).
//!
//! The search runs where the files are. Nothing is shipped to the work
//! machine to be searched there — only the hits come back, which is the
//! whole reason the runtime lives on the home machine at all.
//!
//! `rg` does the scanning. ccnm does not implement a text scanner: matching
//! semantics, encoding detection, ignore-file precedence and multiline
//! handling are years of work that already exist, and a hand-rolled scanner
//! would be both slower and differently wrong.
//!
//! What ccnm does own is every constraint. rg's defaults happen to be safe
//! today, and that is not a reason to depend on them:
//!
//! ```text
//! --no-config     a RIPGREP_CONFIG_PATH in the environment must not be able
//!                 to change what ccnm searches or how
//! --no-follow     a symlink is how a search leaves the workspace
//! --no-hidden     dotfiles stay out, .git among them
//! -g !.git        and .git explicitly again, because a future flag that
//!                 turns hidden files back on must not turn this off
//! cwd = root      rg is given a relative scope from the workspace root, so
//!                 the paths it prints are relative and no absolute path of
//!                 the home machine can reach the model
//! ```
//!
//! and then checks rg's output anyway: any hit whose path is absolute, has
//! a `..`, or is under `.git/` is dropped. rg is a fast scanner, not the
//! security boundary.
//!
//! Both limits bound the *work*, not just the answer. `stream_lines` reads
//! rg's JSON as it arrives and kills it the moment `max_results` or the byte
//! budget is reached, so searching a monorepo for `e` costs fifty matches,
//! not a full scan followed by a truncation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::glob::Glob;
use crate::mcp::path;
use crate::mcp::truncate_bytes;
use crate::process::{Cmd, Flow, stream_lines};

/// Matches returned when the caller does not say (design doc section 15).
pub const DEFAULT_MAX_RESULTS: u32 = 50;
/// Ceiling on `max_results`. Also bounds `hits`, the one part of
/// `structuredContent` that grows with the answer.
pub const MAX_MAX_RESULTS: u32 = 200;
/// Context lines each side when the caller does not say.
pub const DEFAULT_CONTEXT_LINES: u32 = 2;
/// Ceiling on `context_lines`. Ten each side of fifty matches is already a
/// thousand lines; past that a `read_file` is the better call.
pub const MAX_CONTEXT_LINES: u32 = 10;

/// Total bytes of matched and context text returned. Not a parameter: it
/// is a property of the context window, not of the question being asked,
/// and a caller that could raise it would.
const MAX_RESPONSE_BYTES: usize = 32 * 1024;

/// Per-line cap. One minified bundle would otherwise spend the entire
/// budget on a single hit.
const MAX_LINE_BYTES: usize = 512;

/// Wall clock for the whole search. rg is fast; this is for a pathological
/// regex on a huge tree, and the early stop usually ends things first.
const RG_TIMEOUT: Duration = Duration::from_secs(60);

/// Arguments of `search_text`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct SearchTextArgs {
    /// What to look for. A literal string unless `regex` is true.
    pub query: String,
    /// Directory to search, relative to the workspace root. Default: the root.
    #[serde(default)]
    pub path: Option<String>,
    /// Only search files matching this glob, e.g. `**/*.rs`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Treat `query` as a regular expression. Default false.
    #[serde(default)]
    pub regex: Option<bool>,
    /// Match case. Default true.
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    /// Lines of context each side of a match. Default 2, capped at 10.
    #[serde(default)]
    #[schemars(range(min = 0, max = 10))]
    pub context_lines: Option<u32>,
    /// Maximum matches to return. Default 50, capped at 200.
    #[serde(default)]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: Option<u32>,
}

/// Why a search stopped before rg ran out of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    MaxResults,
    MaxBytes,
}

/// Where one match is. Deliberately without the matched text: that is the
/// expensive part and it is already in `content[0].text`, one line up from
/// its own `path:line` prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    /// 1-based character column of the first match on the line.
    pub column: u32,
}

/// The result of one `search_text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Hits grouped by file with context, plus a footer. Goes to
    /// `content[0].text`.
    #[serde(skip)]
    pub text: String,
    pub query: String,
    /// The directory that was searched; `.` for the workspace root.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    pub regex: bool,
    pub matches: u32,
    pub files: u32,
    /// Bytes of matched and context text in `text`, excluding line numbers.
    pub bytes: usize,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_by: Option<Truncation>,
    pub hits: Vec<Hit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Search `root` for `args.query`.
pub fn search_text(root: &Path, args: &SearchTextArgs) -> Result<SearchResult> {
    let plan = Plan::new(root, args)?;
    let rg = locate_rg().ok_or_else(|| {
        Error::dependency(
            "ripgrep is not installed on the workspace machine, and ccnm searches with it rather than scanning files itself; install it (`brew install ripgrep`) and try again",
        )
    })?;

    let mut collector = Collector::new(&plan);
    let outcome = stream_lines(&plan.command(&rg), |line| collector.event(line))?;
    collector.check(&outcome, root)?;
    Ok(collector.finish(plan))
}

/// The validated question, separate from the answering so the argv can be
/// asserted without running anything.
struct Plan {
    root: PathBuf,
    query: String,
    rel_dir: String,
    glob: Option<Glob>,
    regex: bool,
    case_sensitive: bool,
    context_lines: u32,
    max_results: u32,
}

impl Plan {
    fn new(root: &Path, args: &SearchTextArgs) -> Result<Plan> {
        if args.query.is_empty() {
            return Err(Error::invalid_args("query is empty"));
        }
        if args.query.contains('\0') {
            return Err(Error::invalid_args("query contains a NUL byte"));
        }
        let max_results = match args.max_results {
            Some(0) => return Err(Error::invalid_args("max_results must be at least 1")),
            Some(n) => n.min(MAX_MAX_RESULTS),
            None => DEFAULT_MAX_RESULTS,
        };
        let context_lines = args
            .context_lines
            .unwrap_or(DEFAULT_CONTEXT_LINES)
            .min(MAX_CONTEXT_LINES);
        // Compiled with ccnm's own matcher even though rg is what filters,
        // so an unsupported pattern is refused the same way list_files
        // refuses it instead of quietly meaning something else here.
        let glob = args.glob.as_deref().map(Glob::new).transpose()?;

        let rel_dir = match args.path.as_deref().map(str::trim) {
            None | Some("") | Some(".") | Some("./") => String::new(),
            Some(raw) => {
                let resolved = path::resolve_read(root, raw)?;
                if !resolved.abs().is_dir() {
                    return Err(Error::invalid_args(format!(
                        "{} is not a directory; search_text searches a tree",
                        resolved.rel()
                    )));
                }
                resolved.rel().to_string()
            }
        };
        Ok(Plan {
            root: root.to_path_buf(),
            query: args.query.clone(),
            rel_dir,
            glob,
            regex: args.regex.unwrap_or(false),
            case_sensitive: args.case_sensitive.unwrap_or(true),
            context_lines,
            max_results,
        })
    }

    /// The argv. Every constraint is stated, none inherited; `query` and the
    /// glob are their own arguments and no shell ever sees them.
    fn command(&self, rg: &Path) -> Cmd {
        let mut cmd = Cmd::new(rg)
            .args([
                "--json",
                "--no-config",
                "--no-follow",
                "--no-hidden",
                "--glob",
                "!.git",
            ])
            .cwd(&self.root)
            .timeout(RG_TIMEOUT);
        cmd = cmd.arg(if self.case_sensitive {
            "--case-sensitive"
        } else {
            "--ignore-case"
        });
        if !self.regex {
            cmd = cmd.arg("--fixed-strings");
        }
        if self.context_lines > 0 {
            cmd = cmd.args(["--context", &self.context_lines.to_string()]);
        }
        if let Some(glob) = &self.glob {
            cmd = cmd.args(["--glob", glob.source()]);
        }
        // `--` first: a query of `-i` is a query, not a flag.
        cmd.arg("--")
            .arg(&self.query)
            .arg(if self.rel_dir.is_empty() {
                "./"
            } else {
                &self.rel_dir
            })
    }

    fn scope(&self) -> &str {
        if self.rel_dir.is_empty() {
            "."
        } else {
            &self.rel_dir
        }
    }
}

/// One line of the rendered answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    number: u32,
    is_match: bool,
    text: String,
}

/// Reads rg's JSON stream and decides when to stop it.
struct Collector {
    max_results: usize,
    scope: String,
    /// `path -> lines`, in the order rg found them.
    groups: Vec<(String, Vec<Line>)>,
    hits: Vec<Hit>,
    bytes: usize,
    truncated_by: Option<Truncation>,
    notes: BTreeSet<String>,
    /// Enough matches; still accepting the trailing context of the last one.
    full: bool,
}

impl Collector {
    fn new(plan: &Plan) -> Collector {
        Collector {
            max_results: plan.max_results as usize,
            scope: plan.scope().to_string(),
            groups: Vec::new(),
            hits: Vec::new(),
            bytes: 0,
            truncated_by: None,
            notes: BTreeSet::new(),
            full: false,
        }
    }

    fn event(&mut self, raw: &[u8]) -> Flow {
        let Ok(event) = serde_json::from_slice::<Value>(raw) else {
            // rg speaks JSON on stdout and nothing else. A line that is not
            // JSON means a version whose format ccnm does not know, and
            // guessing at it would be worse than saying so.
            self.notes
                .insert("ripgrep produced output ccnm could not read".into());
            return Flow::Stop;
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let data = event.get("data").unwrap_or(&Value::Null);

        match kind {
            "begin" => {
                // A new file after the budget is spent ends the search; the
                // trailing context of the last match has been collected.
                if self.full {
                    return Flow::Stop;
                }
                Flow::Continue
            }
            "end" => {
                if !data.get("binary_offset").unwrap_or(&Value::Null).is_null() {
                    self.notes.insert(
                        "some files are binary and were not searched past the first NUL".into(),
                    );
                }
                Flow::Continue
            }
            "match" | "context" => self.line(kind == "match", data),
            _ => Flow::Continue,
        }
    }

    fn line(&mut self, is_match: bool, data: &Value) -> Flow {
        if is_match && self.full {
            return Flow::Stop;
        }
        let Some(path) = text_field(data.get("path")) else {
            // A path that is not UTF-8 cannot be sent to a JSON client, and
            // ccnm will not invent a name for it.
            self.notes
                .insert("a file whose name is not valid UTF-8 was skipped".into());
            return Flow::Continue;
        };
        let Some(path) = self.safe_path(&path) else {
            return Flow::Continue;
        };
        let Some(raw_line) = text_field(data.get("lines")) else {
            self.notes
                .insert("a matching line is not valid UTF-8 and was skipped".into());
            return Flow::Continue;
        };
        if raw_line.contains('\0') {
            self.notes
                .insert("a matching line contains binary data and was skipped".into());
            return Flow::Continue;
        }
        let number = data.get("line_number").and_then(Value::as_u64).unwrap_or(0) as u32;

        let trimmed = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        let text = truncate_bytes(trimmed, MAX_LINE_BYTES);
        let cut = text.len() < trimmed.len();
        let mut text = text.to_string();
        if cut {
            text.push('…');
            self.notes
                .insert(format!("lines longer than {MAX_LINE_BYTES} bytes are cut"));
        }

        if self.bytes + text.len() > MAX_RESPONSE_BYTES {
            self.truncated_by = Some(Truncation::MaxBytes);
            return Flow::Stop;
        }
        self.bytes += text.len();

        if is_match {
            self.hits.push(Hit {
                path: path.clone(),
                line: number,
                column: column_of(data, trimmed),
            });
            if self.hits.len() >= self.max_results {
                // Not a stop yet: the trailing context of this match is
                // still coming, and cutting it off makes the last hit look
                // like the end of the file.
                self.truncated_by = Some(Truncation::MaxResults);
                self.full = true;
            }
        }

        match self.groups.last_mut() {
            Some((last, lines)) if *last == path => lines.push(Line {
                number,
                is_match,
                text,
            }),
            _ => self.groups.push((
                path,
                vec![Line {
                    number,
                    is_match,
                    text,
                }],
            )),
        }
        Flow::Continue
    }

    /// rg is a scanner, not the boundary. Whatever flags it was given, a
    /// path that is absolute, climbs out, or is under `.git/` does not go
    /// back to the model.
    ///
    /// `.git` is defended three times over: `--no-hidden` keeps rg out of
    /// it, `-g !.git` says so again, and this drops it if the first two
    /// ever stop being true. Only the first and third have behavioural
    /// tests -- with `--no-hidden` in place the glob makes no observable
    /// difference, which is the point of having it.
    fn safe_path(&mut self, raw: &str) -> Option<String> {
        let path = raw.strip_prefix("./").unwrap_or(raw);
        let rejected = path.is_empty()
            || path.starts_with('/')
            || path.split('/').any(|s| s == ".." || s == ".git");
        if rejected {
            self.notes
                .insert("ripgrep offered a path outside the workspace and it was dropped".into());
            return None;
        }
        Some(path.to_string())
    }

    /// rg exit 1 means no matches, which is an answer, not a failure. Only
    /// 2 and above are errors, and a child ccnm killed on purpose has no
    /// code at all.
    fn check(&mut self, outcome: &crate::process::Streamed, root: &Path) -> Result<()> {
        if outcome.timed_out {
            return Err(Error::invalid_args(
                "the search took too long; narrow it with path or glob",
            ));
        }
        if outcome.stopped_early {
            return Ok(());
        }
        match outcome.exit_code {
            Some(0) | Some(1) => Ok(()),
            _ => {
                let detail = sanitize(&String::from_utf8_lossy(&outcome.stderr), root);
                // A bad regex is the caller's to fix; anything else is the
                // machine's problem and keeps a code that stays visible.
                let code =
                    if detail.contains("regex parse error") || detail.contains("error parsing") {
                        ErrorCode::InvalidArgs
                    } else {
                        ErrorCode::Internal
                    };
                Err(Error::new(code, format!("ripgrep failed: {detail}")))
            }
        }
    }

    fn finish(self, plan: Plan) -> SearchResult {
        let matches = self.hits.len() as u32;
        let files = self.groups.len() as u32;
        let mut text = String::new();
        for (path, lines) in &self.groups {
            text.push_str(path);
            text.push('\n');
            let mut previous: Option<u32> = None;
            for line in lines {
                if previous.is_some_and(|p| line.number > p + 1) {
                    text.push_str("--\n");
                }
                previous = Some(line.number);
                text.push_str(&format!(
                    "{}{}{}\n",
                    line.number,
                    if line.is_match { ':' } else { '-' },
                    line.text
                ));
            }
        }
        let footer = if matches == 0 {
            format!("[no matches for {} under {}]", plan.query, self.scope)
        } else {
            match self.truncated_by {
                Some(Truncation::MaxResults) => format!(
                    "[stopped at max_results={}; narrow it with path, glob or a longer query]",
                    plan.max_results
                ),
                Some(Truncation::MaxBytes) => format!(
                    "[stopped at {} bytes of output; narrow it with path, glob or a longer query]",
                    MAX_RESPONSE_BYTES
                ),
                None => format!(
                    "[{matches} match{} in {files} file{}]",
                    if matches == 1 { "" } else { "es" },
                    if files == 1 { "" } else { "s" }
                ),
            }
        };
        text.push_str(&footer);
        let notes: Vec<String> = self.notes.into_iter().collect();
        for note in &notes {
            text.push_str("\n[");
            text.push_str(note);
            text.push(']');
        }

        SearchResult {
            text,
            query: plan.query,
            path: self.scope,
            glob: plan.glob.map(|g| g.source().to_string()),
            regex: plan.regex,
            matches,
            files,
            bytes: self.bytes,
            truncated: self.truncated_by.is_some(),
            truncated_by: self.truncated_by,
            hits: self.hits,
            notes,
        }
    }
}

/// rg writes `{"text": "..."}` for valid UTF-8 and `{"bytes": "<base64>"}`
/// otherwise. Only the first is usable.
fn text_field(value: Option<&Value>) -> Option<String> {
    value?.get("text")?.as_str().map(str::to_string)
}

/// 1-based column of the first submatch, counted in characters so a line of
/// CJK does not report a column past its own length.
fn column_of(data: &Value, line: &str) -> u32 {
    let start = data
        .get("submatches")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(|m| m.get("start"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let prefix = line.get(..start.min(line.len())).unwrap_or("");
    prefix.chars().count() as u32 + 1
}

/// Never let the home machine's absolute paths reach the model, even
/// through an error message.
fn sanitize(message: &str, root: &Path) -> String {
    let root = root.display().to_string();
    let cleaned = message.replace(&root, "<workspace>");
    cleaned.trim().to_string()
}

/// Where `rg` might be. PATH first, then the two places Homebrew puts it,
/// because a non-interactive ssh session gets a short PATH.
fn locate_rg() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("rg")));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/rg"));
    candidates.push(PathBuf::from("/usr/local/bin/rg"));
    candidates
        .into_iter()
        .find(|p| crate::claude::is_executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-search-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir_all(dir.join("ignored")).unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    let needle = 1;\n    println!(\"{needle}\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "// needle in a comment\npub fn f() {}\n// NEEDLE shouting\n",
        )
        .unwrap();
        fs::write(dir.join("README.md"), "no match here\n").unwrap();
        fs::write(dir.join(".git/config"), "needle in the git database\n").unwrap();
        fs::write(dir.join(".hidden"), "needle in a dotfile\n").unwrap();
        fs::write(dir.join("ignored/x.rs"), "needle in an ignored file\n").unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn args(query: &str) -> SearchTextArgs {
        SearchTextArgs {
            query: query.to_string(),
            ..Default::default()
        }
    }

    fn search(root: &Path, a: &SearchTextArgs) -> SearchResult {
        search_text(root, a).unwrap()
    }

    fn err(root: &Path, a: &SearchTextArgs) -> Error {
        match search_text(root, a) {
            Err(e) => e,
            Ok(r) => panic!("expected a refusal, got {} matches", r.matches),
        }
    }

    #[test]
    fn a_literal_search_reports_hits_with_context() {
        let root = workspace("basic");
        let r = search(
            &root,
            &SearchTextArgs {
                context_lines: Some(1),
                ..args("needle")
            },
        );
        assert_eq!(r.matches, 3, "{}", r.text);
        assert_eq!(r.files, 2);
        assert!(!r.truncated);
        assert!(
            r.text
                .contains("src/main.rs\n1-fn main() {\n2:    let needle = 1;\n"),
            "{}",
            r.text
        );
        assert!(r.text.ends_with("[3 matches in 2 files]"), "{}", r.text);
        // Context lines are marked with `-`, matches with `:`.
        assert!(r.text.contains("1-fn main() {"), "{}", r.text);
        // Column is 1-based and points at the match.
        let hit = r.hits.iter().find(|h| h.path == "src/main.rs").unwrap();
        assert_eq!((hit.line, hit.column), (2, 9));
    }

    #[test]
    fn the_git_database_dotfiles_and_ignored_files_are_never_searched() {
        let root = workspace("excluded");
        let r = search(&root, &args("needle"));
        for forbidden in [".git", ".hidden", "ignored/"] {
            assert!(
                !r.text.contains(forbidden),
                "{forbidden} reached the model:\n{}",
                r.text
            );
        }
        assert!(
            r.hits.iter().all(|h| !h.path.starts_with('.')),
            "{:?}",
            r.hits
        );
    }

    #[test]
    fn no_match_is_an_answer_not_a_failure() {
        let root = workspace("nomatch");
        let r = search(&root, &args("haystack"));
        assert_eq!(r.matches, 0);
        assert_eq!(r.files, 0);
        assert!(!r.truncated);
        assert_eq!(r.text, "[no matches for haystack under .]");
    }

    #[test]
    fn literal_is_literal_and_regex_is_opt_in() {
        let root = workspace("literal");
        fs::write(root.join("src/re.rs"), "let a = b.c;\nlet x = 1;\n").unwrap();
        // `b.c` as a literal must not match `b_c`, and `.` must not be a
        // wildcard: a user searching for a literal string that happens to be
        // a valid regex would otherwise get silently wrong results.
        fs::write(root.join("src/re2.rs"), "let a = bXc;\n").unwrap();
        let literal = search(&root, &args("b.c"));
        assert_eq!(literal.matches, 1, "{}", literal.text);
        assert!(literal.text.contains("b.c"), "{}", literal.text);

        let regex = search(
            &root,
            &SearchTextArgs {
                regex: Some(true),
                ..args("b.c")
            },
        );
        assert_eq!(regex.matches, 2, "{}", regex.text);
        assert!(regex.regex);
    }

    #[test]
    fn a_query_that_looks_like_a_flag_is_still_a_query() {
        let root = workspace("flaglike");
        fs::write(root.join("src/flags.rs"), "// --ignore-case here\n").unwrap();
        let r = search(&root, &args("--ignore-case"));
        assert_eq!(r.matches, 1, "{}", r.text);
    }

    #[test]
    fn case_sensitivity_is_explicit() {
        let root = workspace("case");
        assert_eq!(search(&root, &args("NEEDLE")).matches, 1);
        assert_eq!(
            search(
                &root,
                &SearchTextArgs {
                    case_sensitive: Some(false),
                    ..args("NEEDLE")
                }
            )
            .matches,
            4,
            "two in main.rs plus both spellings in lib.rs"
        );
    }

    #[test]
    fn scope_and_glob_narrow_the_search() {
        let root = workspace("scope");
        let scoped = search(
            &root,
            &SearchTextArgs {
                path: Some("src".into()),
                ..args("needle")
            },
        );
        assert_eq!(scoped.path, "src");
        assert!(
            scoped.hits.iter().all(|h| h.path.starts_with("src/")),
            "{:?}",
            scoped.hits
        );

        let globbed = search(
            &root,
            &SearchTextArgs {
                glob: Some("**/*.rs".into()),
                ..args("needle")
            },
        );
        assert!(globbed.matches > 0);
        assert!(
            globbed.hits.iter().all(|h| h.path.ends_with(".rs")),
            "{:?}",
            globbed.hits
        );
    }

    #[test]
    fn the_path_policy_is_the_same_one_the_other_tools_use() {
        let root = workspace("policy");
        for (path, code) in [
            ("../", ErrorCode::Policy),
            ("/etc", ErrorCode::Policy),
            ("~/", ErrorCode::Policy),
            ("nope", ErrorCode::InvalidArgs),
            ("README.md", ErrorCode::InvalidArgs),
        ] {
            let e = err(
                &root,
                &SearchTextArgs {
                    path: Some(path.into()),
                    ..args("needle")
                },
            );
            assert_eq!(e.code(), code, "{path} -> {e}");
        }
        // And the glob goes through the same compiler list_files uses.
        let e = err(
            &root,
            &SearchTextArgs {
                glob: Some("../*".into()),
                ..args("needle")
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn bad_arguments_are_refused_before_ripgrep_runs() {
        let root = workspace("badargs");
        assert_eq!(err(&root, &args("")).code(), ErrorCode::InvalidArgs);
        assert_eq!(err(&root, &args("a\0b")).code(), ErrorCode::InvalidArgs);
        let e = err(
            &root,
            &SearchTextArgs {
                max_results: Some(0),
                ..args("needle")
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn a_broken_regex_is_the_callers_problem_and_leaks_no_paths() {
        let root = workspace("badregex");
        let e = err(
            &root,
            &SearchTextArgs {
                regex: Some(true),
                ..args("(unclosed")
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs, "{e}");
        assert!(
            !e.message().contains(&root.display().to_string()),
            "the workspace path reached the model: {e}"
        );
    }

    #[test]
    fn max_results_stops_ripgrep_rather_than_truncating_its_output() {
        let root = workspace("many");
        let body: String = (1..=5_000).map(|n| format!("needle {n}\n")).collect();
        fs::write(root.join("src/many.rs"), body).unwrap();
        let r = search(
            &root,
            &SearchTextArgs {
                max_results: Some(10),
                context_lines: Some(0),
                ..args("needle")
            },
        );
        assert_eq!(r.matches, 10);
        assert_eq!(r.hits.len(), 10);
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(Truncation::MaxResults));
        assert!(r.text.contains("[stopped at max_results=10"), "{}", r.text);
    }

    #[test]
    fn the_last_match_still_gets_its_trailing_context() {
        let root = workspace("trailing");
        let body: String = (1..=100)
            .map(|n| format!("ctxmark {n}\nafter {n}\nfiller {n}\n"))
            .collect();
        fs::write(root.join("src/ctx.rs"), body).unwrap();
        // A term only this file has. The fixture also contains "needle" and
        // rg walks files in an order it does not promise, so a shared term
        // plus a small max_results makes the test depend on that order.
        let r = search(
            &root,
            &SearchTextArgs {
                max_results: Some(2),
                context_lines: Some(1),
                ..args("ctxmark")
            },
        );
        assert_eq!(r.matches, 2);
        // Without the "full but still collecting context" state the answer
        // would end on the match line and look like the end of the file.
        assert!(r.text.contains("after 2"), "{}", r.text);
    }

    #[test]
    fn the_byte_budget_holds_even_against_one_enormous_line() {
        let root = workspace("bytes");
        // Lines far longer than the per-line cap, and enough of them to pass
        // the response budget before max_results would.
        let body: String = (1..=300)
            .map(|n| format!("widemark {}\n", "x".repeat(4_000) + &n.to_string()))
            .collect();
        fs::write(root.join("src/wide.rs"), body).unwrap();
        let r = search(
            &root,
            &SearchTextArgs {
                max_results: Some(200),
                context_lines: Some(0),
                ..args("widemark")
            },
        );
        assert!(r.bytes <= MAX_RESPONSE_BYTES, "{} bytes", r.bytes);
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(Truncation::MaxBytes));
        assert!(
            r.notes.iter().any(|n| n.contains("longer than")),
            "{:?}",
            r.notes
        );
        // Every returned line is capped, so no single hit can dominate.
        for line in r.text.lines().filter(|l| l.contains(':')) {
            assert!(line.len() < MAX_LINE_BYTES + 64, "{}", line.len());
        }
    }

    #[test]
    fn cutting_a_long_line_never_splits_a_character() {
        let root = workspace("utf8");
        fs::write(
            root.join("src/cjk.rs"),
            format!("// wideline {}\n", "中".repeat(1_000)),
        )
        .unwrap();
        let r = search(
            &root,
            &SearchTextArgs {
                context_lines: Some(0),
                ..args("wideline")
            },
        );
        assert_eq!(r.matches, 1);
        // The rendered text is a String, so a split character would have
        // panicked before we got here; assert the cut happened at all.
        assert!(r.text.contains('…'), "{}", &r.text[..80.min(r.text.len())]);
    }

    #[test]
    fn a_binary_file_never_sends_its_contents() {
        let root = workspace("binary");
        let mut blob = b"needle".to_vec();
        blob.extend_from_slice(&[0u8; 64]);
        blob.extend_from_slice(b"needle secret");
        fs::write(root.join("src/blob.bin"), blob).unwrap();
        let r = search(&root, &args("needle"));
        assert!(!r.text.contains("secret"), "{}", r.text);
        assert!(
            r.hits.iter().all(|h| h.path != "src/blob.bin"),
            "{:?}",
            r.hits
        );
    }

    #[test]
    fn structured_content_carries_locations_but_not_the_matched_text() {
        let root = workspace("nodup");
        let r = search(&root, &args("needle"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"line\":2"), "{json}");
        assert!(
            !json.contains("let needle = 1"),
            "matched text duplicated: {json}"
        );
        assert!(!json.contains(&root.display().to_string()), "{json}");
    }

    #[test]
    fn the_argv_states_every_constraint_and_quotes_nothing() {
        let root = workspace("argv");
        let plan = Plan::new(
            &root,
            &SearchTextArgs {
                path: Some("src".into()),
                glob: Some("**/*.rs".into()),
                context_lines: Some(3),
                ..args("-i --danger")
            },
        )
        .unwrap();
        let cmd = plan.command(Path::new("/opt/homebrew/bin/rg"));
        let argv: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for expected in [
            "--json",
            "--no-config",
            "--no-follow",
            "--no-hidden",
            "!.git",
            "--fixed-strings",
            "--case-sensitive",
        ] {
            assert!(
                argv.contains(&expected.to_string()),
                "{expected} missing: {argv:?}"
            );
        }
        // The query is one argument, after `--`, and never spliced into a
        // string a shell could reinterpret.
        let end = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[end + 1], "-i --danger");
        assert_eq!(argv[end + 2], "src");
        assert_eq!(cmd.cwd.as_deref(), Some(root.as_path()));
        assert!(!cmd.program.to_string_lossy().contains("sh"));
    }

    #[test]
    fn sanitize_replaces_the_workspace_path() {
        let root = Path::new("/Users/someone/secret-project");
        let message = "rg: /Users/someone/secret-project/src: No such file or directory";
        assert_eq!(
            sanitize(message, root),
            "rg: <workspace>/src: No such file or directory"
        );
    }

    #[test]
    fn safe_path_drops_anything_that_left_the_workspace() {
        let root = workspace("safepath");
        let plan = Plan::new(&root, &args("x")).unwrap();
        let mut collector = Collector::new(&plan);
        assert_eq!(
            collector.safe_path("src/main.rs"),
            Some("src/main.rs".into())
        );
        assert_eq!(
            collector.safe_path("./src/main.rs"),
            Some("src/main.rs".into())
        );
        for bad in [
            "/etc/passwd",
            "../outside",
            "src/../../x",
            ".git/config",
            "",
        ] {
            assert_eq!(collector.safe_path(bad), None, "{bad}");
        }
        assert!(!collector.notes.is_empty());
    }
}

//! `list_files`: the second phase 2 tool (design doc section 15).
//!
//! The whole job is navigation, so the thing that decides whether it is
//! useful is not the listing code but what it leaves out. A plain
//! recursive walk of a real project returns `node_modules` and `target`
//! and nothing a model wanted, and the 200-entry budget is gone before
//! the first source file.
//!
//! So in a git workspace the file list comes from git:
//!
//! ```text
//! git ls-files --cached --others --exclude-standard -z -- <scope>
//! ```
//!
//! which is the project's own definition of what matters: tracked files
//! plus new ones, minus everything `.gitignore`, the global ignore file
//! and `.git/info/exclude` rule out. That
//! beats a hard-coded skip list, which is what coding-tools-mcp uses
//! (thirteen names, no way to turn it off — `docs/research/`, section c).
//! Outside a git repository there is nothing to ask, so a bounded walk
//! with a short documented skip list is the fallback, and the answer says
//! which of the two produced it.
//!
//! Two shapes, decided by whether a glob was given:
//!
//! ```text
//! no glob    the immediate children of `path`, directories marked with /
//! glob       everything under `path` matching it, at any depth
//! ```
//!
//! One parameter fewer than a `recursive` flag, and it is how a person
//! already thinks: you either open a directory or you go looking.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mcp::glob::Glob;
use crate::mcp::path;
use crate::process::{Cmd, ProcessRunner};

/// Entries returned when the caller does not say.
pub const DEFAULT_MAX_ENTRIES: u32 = 200;
/// Ceiling on `max_entries`, clamped rather than refused for the same
/// reason as `read_file`'s: a large "at most N" is a preference.
pub const MAX_MAX_ENTRIES: u32 = 1_000;

/// How long `git ls-files` gets. It reads the index, so this is generous.
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// Ceiling on paths accepted from git before giving up and asking the
/// caller to narrow the search. A monorepo index can be hundreds of
/// thousands of lines and none of it helps at that size.
const MAX_CANDIDATES: usize = 200_000;

/// Ceiling on directory entries examined during the fallback walk.
const MAX_VISITED: usize = 50_000;

/// Directories the fallback walk skips. Only used outside a git
/// workspace, where there is nothing to ask what matters. Dotted names
/// are already covered by `include_hidden`, so this list is short on
/// purpose; it is a guess, and the answer says so.
const SKIP_DIRS: [&str; 6] = ["node_modules", "target", "dist", "build", "venv", "vendor"];

/// Arguments of `list_files`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ListFilesArgs {
    /// Directory to list, relative to the workspace root. Default: the root.
    #[serde(default)]
    pub path: Option<String>,
    /// Match recursively under `path` instead of listing its children.
    /// Supports `*`, `**`, `?` and `{a,b}`, e.g. `**/*.{rs,toml}`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Maximum entries to return. Default 200, capped at 1000.
    #[serde(default)]
    #[schemars(range(min = 1, max = 1000))]
    pub max_entries: Option<u32>,
    /// Include names starting with a dot. Default false.
    #[serde(default)]
    pub include_hidden: Option<bool>,
}

/// Where the file list came from. Worth reporting: the two sources leave
/// out different things, and a model that sees no `target/` should be
/// able to tell "git ignores it" from "ccnm guessed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `git ls-files`: the project's own ignore rules applied.
    Git,
    /// A filesystem walk with ccnm's short skip list.
    Walk,
}

/// The result of one `list_files`. As in `read_file`, the listing itself
/// lives in [`text`](Self::text) and is left out of the serialized form
/// so a client that forwards both does not pay for it twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    /// The entries, one per line, plus a footer. Goes to `content[0].text`.
    #[serde(skip)]
    pub text: String,
    /// The directory that was listed, workspace-relative; `.` for the root.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    pub entries: u32,
    pub files: u32,
    pub dirs: u32,
    /// True when `max_entries` cut the answer short. There is no cursor:
    /// narrowing with `path` or `glob` gives a better answer than paging
    /// through a directory in an order nobody chose.
    pub truncated: bool,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// List files under `root`.
pub fn list_files(
    root: &Path,
    args: &ListFilesArgs,
    runner: &dyn ProcessRunner,
) -> Result<Listing> {
    let max_entries = match args.max_entries {
        Some(0) => return Err(Error::invalid_args("max_entries must be at least 1")),
        Some(n) => n.min(MAX_MAX_ENTRIES),
        None => DEFAULT_MAX_ENTRIES,
    };
    let include_hidden = args.include_hidden.unwrap_or(false);
    let pattern = args.glob.as_deref().map(Glob::new).transpose()?;

    // An absent path means the root, which `resolve_read` refuses by
    // design: it exists to keep callers inside the workspace, and the
    // root is the one path that is not inside anything.
    let (rel_dir, abs_dir) = match args.path.as_deref().map(str::trim) {
        None | Some("") | Some(".") | Some("./") => (String::new(), root.to_path_buf()),
        Some(raw) => {
            let resolved = path::resolve_read(root, raw)?;
            (resolved.rel().to_string(), resolved.abs().to_path_buf())
        }
    };
    if !abs_dir.is_dir() {
        return Err(Error::invalid_args(format!(
            "{} is not a directory; use read_file to read it",
            display_dir(&rel_dir)
        )));
    }

    let mut notes = Vec::new();
    let (candidates, source) = match git_candidates(root, &rel_dir, runner)? {
        Some(paths) => (paths, Source::Git),
        None => {
            notes.push(format!(
                "not a git workspace, so ccnm walked the directory and skipped {}",
                SKIP_DIRS.join(", ")
            ));
            (
                walk(&abs_dir, &rel_dir, pattern.is_some(), &mut notes)?,
                Source::Walk,
            )
        }
    };

    let selected = select(
        &candidates,
        &rel_dir,
        pattern.as_ref(),
        include_hidden,
        max_entries,
    );
    Ok(finish(
        selected,
        rel_dir,
        pattern,
        source,
        notes,
        max_entries,
    ))
}

/// One entry of the answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    /// Workspace-relative, with a trailing `/` on a directory so the
    /// difference costs one byte instead of a `type` field.
    path: String,
    is_dir: bool,
}

/// What the two sources both produce: workspace-relative paths, with a
/// trailing `/` marking a directory.
type Candidates = Vec<String>;

/// Ask git for the file list, or `None` when this is not a git workspace.
fn git_candidates(
    root: &Path,
    rel_dir: &str,
    runner: &dyn ProcessRunner,
) -> Result<Option<Candidates>> {
    let mut cmd = Cmd::new("git")
        // No --directory. It collapses an entirely untracked directory
        // into one entry, which sounds like a saving and is a bug: asking
        // for `src` in a repository with nothing committed yet answers
        // `src/`, which is not inside `src`, so the listing comes back
        // empty. A glob over an untracked tree loses the same way. The
        // depth-1 collapse below already produces the shape --directory
        // was for, and --exclude-standard is what keeps node_modules out.
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .cwd(root)
        .timeout(GIT_TIMEOUT);
    // Scope to the subtree, so listing one directory of a large repository
    // does not read out the whole index.
    if !rel_dir.is_empty() {
        cmd = cmd.args(["--", rel_dir]);
    }
    let Ok(out) = runner.run(&cmd) else {
        // No git binary at all. Not an error: the walk is the fallback.
        return Ok(None);
    };
    if !out.success() {
        // Not a repository, or git refused the pathspec. Either way there
        // is nothing to learn from it.
        return Ok(None);
    }
    let mut paths: Candidates = Vec::new();
    for chunk in out.stdout.split(|b| *b == 0) {
        if chunk.is_empty() {
            continue;
        }
        // git writes bytes; a path that is not UTF-8 cannot be sent to a
        // JSON client anyway, so skip it rather than mangle it.
        let Ok(text) = std::str::from_utf8(chunk) else {
            continue;
        };
        paths.push(text.to_string());
        if paths.len() > MAX_CANDIDATES {
            return Err(Error::invalid_args(format!(
                "more than {MAX_CANDIDATES} files here; narrow it with path or glob"
            )));
        }
    }
    Ok(Some(paths))
}

/// The fallback for a workspace with no git. Depth 1 unless a glob was
/// given, and directory symlinks are listed but never followed: following
/// them is both how a walk leaves the workspace and how it finds a loop
/// it can never finish.
fn walk(
    abs_dir: &Path,
    rel_dir: &str,
    recursive: bool,
    notes: &mut Vec<String>,
) -> Result<Candidates> {
    let mut out: Candidates = Vec::new();
    let mut queue = vec![(abs_dir.to_path_buf(), rel_dir.to_string())];
    let mut visited = 0usize;

    while let Some((dir, rel)) = queue.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A directory that cannot be read is a gap in the answer, not
            // a failure of the call.
            Err(_) => {
                notes.push(format!("{} could not be read", display_dir(&rel)));
                continue;
            }
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_VISITED {
                notes.push(format!(
                    "stopped after looking at {MAX_VISITED} entries; narrow it with path or glob"
                ));
                return Ok(out);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            // Two separate questions, and answering them with one call is
            // the bug. What the entry *is* decides how it is listed, and
            // for that a symlink to a directory is a directory: calling it
            // a file would send the model to read_file, which refuses it.
            // Whether to *descend* is decided by the link itself, and a
            // symlink is never followed -- that is both how a walk leaves
            // the workspace and how it finds a loop it cannot finish.
            let link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            let is_dir = if link {
                std::fs::metadata(entry.path())
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            } else {
                entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            };
            if is_dir {
                out.push(format!("{child_rel}/"));
                let prune = SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.');
                if recursive && !link && !prune {
                    queue.push((entry.path(), child_rel));
                }
            } else {
                out.push(child_rel);
            }
        }
        if !recursive {
            break;
        }
    }
    Ok(out)
}

/// Turn the candidate paths into the entries the caller asked for.
///
/// Without a glob this collapses everything deeper than one level into
/// the directory that holds it, which is what makes `git ls-files` — a
/// flat list of files — answer a question about a directory.
fn select(
    candidates: &Candidates,
    rel_dir: &str,
    pattern: Option<&Glob>,
    include_hidden: bool,
    max_entries: u32,
) -> (Vec<Entry>, bool) {
    let mut seen: BTreeSet<Entry> = BTreeSet::new();
    let mut truncated = false;

    for candidate in candidates {
        let trimmed = candidate.trim_end_matches('/');
        let Some(inside) = strip_dir(trimmed, rel_dir) else {
            continue;
        };
        if inside.is_empty() {
            continue;
        }
        // ccnm's own staging files are never part of the project, so they
        // are hidden even from `include_hidden`. They exist for the length
        // of one `apply_patch`, or until the next one sweeps up after a
        // kill; either way nobody should be offered one to read or patch.
        if inside
            .split('/')
            .any(|s| s.starts_with(crate::mcp::patch::TEMP_PREFIX))
        {
            continue;
        }
        if !include_hidden && inside.split('/').any(|s| s.starts_with('.')) {
            continue;
        }

        let entry = match pattern {
            Some(glob) => {
                if !glob.matches(inside) {
                    continue;
                }
                Entry {
                    path: join(rel_dir, inside),
                    is_dir: candidate.ends_with('/'),
                }
            }
            None => match inside.split_once('/') {
                // Deeper than one level: report the directory that holds it.
                Some((head, _)) => Entry {
                    path: join(rel_dir, head),
                    is_dir: true,
                },
                None => Entry {
                    path: join(rel_dir, inside),
                    is_dir: candidate.ends_with('/'),
                },
            },
        };
        seen.insert(entry);
        // One over the limit is what proves there was more.
        if seen.len() > max_entries as usize {
            truncated = true;
            seen.pop_last();
            break;
        }
    }
    (seen.into_iter().collect(), truncated)
}

/// `path` relative to `dir`, or `None` when it is not under it.
fn strip_dir<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// `.` reads better than an empty string in a message.
fn display_dir(rel: &str) -> &str {
    if rel.is_empty() { "." } else { rel }
}

fn finish(
    (entries, truncated): (Vec<Entry>, bool),
    rel_dir: String,
    pattern: Option<Glob>,
    source: Source,
    notes: Vec<String>,
    max_entries: u32,
) -> Listing {
    let dirs = entries.iter().filter(|e| e.is_dir).count() as u32;
    let files = entries.len() as u32 - dirs;

    let mut text = String::new();
    for entry in &entries {
        text.push_str(&entry.path);
        if entry.is_dir {
            text.push('/');
        }
        text.push('\n');
    }
    let footer = if entries.is_empty() {
        match &pattern {
            Some(glob) => format!(
                "[nothing under {} matches {}]",
                display_dir(&rel_dir),
                glob.source()
            ),
            None => format!("[{} is empty]", display_dir(&rel_dir)),
        }
    } else if truncated {
        format!("[stopped at max_entries={max_entries}; narrow it with path or glob]")
    } else {
        match &pattern {
            Some(glob) => format!(
                "[{} match{} for {} under {}]",
                entries.len(),
                if entries.len() == 1 { "" } else { "es" },
                glob.source(),
                display_dir(&rel_dir)
            ),
            None => format!(
                "[{} entr{} in {}, {dirs} director{}]",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" },
                display_dir(&rel_dir),
                if dirs == 1 { "y" } else { "ies" }
            ),
        }
    };
    text.push_str(&footer);
    for note in &notes {
        text.push_str("\n[");
        text.push_str(note);
        text.push(']');
    }

    Listing {
        text,
        path: display_dir(&rel_dir).to_string(),
        glob: pattern.map(|g| g.source().to_string()),
        entries: entries.len() as u32,
        files,
        dirs,
        truncated,
        source,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::process::SystemRunner;
    use std::fs;
    use std::path::PathBuf;

    /// A workspace that looks like a real project: sources, a build
    /// directory that must not show up, a dotfile, and a symlink.
    fn workspace(name: &str, git: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-list-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/mcp")).unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::create_dir_all(dir.join("node_modules/left-pad")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(dir.join("README.md"), "# hi\n").unwrap();
        fs::write(dir.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/lib.rs"), "\n").unwrap();
        fs::write(dir.join("src/mcp/read.rs"), "\n").unwrap();
        fs::write(dir.join("tests/cli.rs"), "\n").unwrap();
        fs::write(dir.join("target/debug/huge.bin"), "\n").unwrap();
        fs::write(dir.join("node_modules/left-pad/index.js"), "\n").unwrap();
        if git {
            let runner = SystemRunner;
            for args in [
                vec!["init", "-q"],
                vec!["add", "-A"],
                vec![
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-qm",
                    "x",
                ],
            ] {
                let out = runner.run(&Cmd::new("git").args(args).cwd(&dir)).unwrap();
                assert!(out.success(), "{}", out.stderr_lossy());
            }
        }
        fs::canonicalize(&dir).unwrap()
    }

    fn list(root: &Path, args: &ListFilesArgs) -> Listing {
        list_files(root, args, &SystemRunner).unwrap()
    }

    fn lines(listing: &Listing) -> Vec<&str> {
        listing
            .text
            .lines()
            .filter(|l| !l.starts_with('['))
            .collect()
    }

    #[test]
    fn the_root_of_a_git_workspace_hides_what_gitignore_hides() {
        let root = workspace("git-root", true);
        let listing = list(&root, &ListFilesArgs::default());
        assert_eq!(listing.source, Source::Git);
        assert_eq!(
            lines(&listing),
            ["Cargo.toml", "README.md", "src/", "tests/"]
        );
        assert_eq!((listing.files, listing.dirs), (2, 2));
        assert!(
            !listing.text.contains("target"),
            "gitignored: {}",
            listing.text
        );
        assert!(!listing.text.contains("node_modules"), "{}", listing.text);
        assert!(
            !listing.text.contains(".gitignore"),
            "hidden by default: {}",
            listing.text
        );
        assert!(listing.text.ends_with("[4 entries in ., 2 directories]"));
    }

    #[test]
    fn without_git_the_walk_says_it_guessed() {
        let root = workspace("nogit-root", false);
        let listing = list(&root, &ListFilesArgs::default());
        assert_eq!(listing.source, Source::Walk);
        // The walk's skip list only prunes recursion, so a skipped
        // directory is still listed at depth 1 -- it exists, and saying so
        // is honest. What it must not do is spend the budget inside it.
        assert!(lines(&listing).contains(&"src/"));
        assert!(!listing.text.contains("left-pad"), "{}", listing.text);
        assert!(
            listing
                .notes
                .iter()
                .any(|n| n.contains("not a git workspace")),
            "{:?}",
            listing.notes
        );
    }

    #[test]
    fn a_repository_with_nothing_committed_yet_still_lists() {
        // Found on a real machine, not here: the fixture commits
        // everything, so `git ls-files --directory` had nothing to
        // collapse. In a repository where the files are still untracked it
        // collapses `src` into a single `src/` entry, which is not *inside*
        // `src`, and the listing came back empty.
        let dir = std::env::temp_dir().join(format!("ccnm-list-{}-fresh", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/mcp")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/mcp/read.rs"), "\n").unwrap();
        fs::write(dir.join("README.md"), "\n").unwrap();
        let out = SystemRunner
            .run(&Cmd::new("git").args(["init", "-q"]).cwd(&dir))
            .unwrap();
        assert!(out.success(), "{}", out.stderr_lossy());
        let root = fs::canonicalize(&dir).unwrap();

        let scoped = list(
            &root,
            &ListFilesArgs {
                path: Some("src".into()),
                ..Default::default()
            },
        );
        assert_eq!(scoped.source, Source::Git);
        assert_eq!(lines(&scoped), ["src/main.rs", "src/mcp/"]);

        let globbed = list(
            &root,
            &ListFilesArgs {
                glob: Some("**/*.rs".into()),
                ..Default::default()
            },
        );
        assert_eq!(lines(&globbed), ["src/main.rs", "src/mcp/read.rs"]);
    }

    #[test]
    fn a_subdirectory_lists_only_its_own_children() {
        let root = workspace("subdir", true);
        let listing = list(
            &root,
            &ListFilesArgs {
                path: Some("src".into()),
                ..Default::default()
            },
        );
        assert_eq!(lines(&listing), ["src/lib.rs", "src/main.rs", "src/mcp/"]);
        assert_eq!(listing.path, "src");
        // Paths are workspace-relative so they can be pasted straight into
        // read_file, which is the next thing the model does.
        assert!(lines(&listing).iter().all(|l| l.starts_with("src/")));
    }

    #[test]
    fn a_glob_searches_at_any_depth_and_a_missing_one_lists_one_level() {
        let root = workspace("glob", true);
        let deep = list(
            &root,
            &ListFilesArgs {
                glob: Some("**/*.rs".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            lines(&deep),
            [
                "src/lib.rs",
                "src/main.rs",
                "src/mcp/read.rs",
                "tests/cli.rs"
            ]
        );
        assert_eq!(deep.glob.as_deref(), Some("**/*.rs"));
        assert!(deep.text.ends_with("[4 matches for **/*.rs under .]"));

        // The same glob under a subdirectory is relative to it.
        let scoped = list(
            &root,
            &ListFilesArgs {
                path: Some("src".into()),
                glob: Some("**/*.rs".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            lines(&scoped),
            ["src/lib.rs", "src/main.rs", "src/mcp/read.rs"]
        );

        // And a glob that matches nothing says so rather than looking empty.
        let none = list(
            &root,
            &ListFilesArgs {
                glob: Some("**/*.py".into()),
                ..Default::default()
            },
        );
        assert_eq!(none.entries, 0);
        assert_eq!(none.text, "[nothing under . matches **/*.py]");
    }

    #[test]
    fn a_glob_cannot_reach_outside_the_listed_directory() {
        let root = workspace("glob-escape", true);
        for pattern in ["../*", "**/../../etc/*", "/etc/*"] {
            let e = list_files(
                &root,
                &ListFilesArgs {
                    glob: Some(pattern.into()),
                    ..Default::default()
                },
                &SystemRunner,
            )
            .unwrap_err();
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{pattern}");
        }
        // And a glob is not a way around the path policy either.
        let e = list_files(
            &root,
            &ListFilesArgs {
                path: Some("../".into()),
                ..Default::default()
            },
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Policy);
    }

    #[test]
    fn hidden_entries_appear_only_when_asked() {
        let root = workspace("hidden", true);
        let shown = list(
            &root,
            &ListFilesArgs {
                include_hidden: Some(true),
                ..Default::default()
            },
        );
        assert!(lines(&shown).contains(&".gitignore"), "{}", shown.text);
        // Including hidden entries must not include the git database.
        assert!(!shown.text.contains(".git/"), "{}", shown.text);
    }

    /// ccnm's own staging files belong to no project and are never shown,
    /// not even to a caller that asked for hidden entries.
    #[test]
    fn a_patch_temp_file_is_never_listed() {
        let root = workspace("temps", true);
        fs::write(root.join("src/.ccnm-abcdef123456-main.rs"), "staged\n").unwrap();
        for include_hidden in [false, true] {
            let shown = list(
                &root,
                &ListFilesArgs {
                    path: Some("src".into()),
                    include_hidden: Some(include_hidden),
                    ..Default::default()
                },
            );
            assert!(!shown.text.contains(".ccnm-"), "{}", shown.text);
        }
    }

    #[test]
    fn max_entries_truncates_and_says_how_to_narrow() {
        let root = workspace("many", true);
        for n in 0..50 {
            fs::write(root.join(format!("src/f{n:02}.rs")), "\n").unwrap();
        }
        let listing = list(
            &root,
            &ListFilesArgs {
                path: Some("src".into()),
                max_entries: Some(10),
                ..Default::default()
            },
        );
        assert_eq!(listing.entries, 10);
        assert!(listing.truncated);
        assert!(
            listing
                .text
                .ends_with("[stopped at max_entries=10; narrow it with path or glob]"),
            "{}",
            listing.text
        );

        // Over the cap is clamped, not refused; zero is refused.
        let clamped = list(
            &root,
            &ListFilesArgs {
                max_entries: Some(999_999),
                ..Default::default()
            },
        );
        assert!(!clamped.truncated);
        let e = list_files(
            &root,
            &ListFilesArgs {
                max_entries: Some(0),
                ..Default::default()
            },
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn a_file_is_not_a_directory_and_a_missing_path_says_so() {
        let root = workspace("types", true);
        let e = list_files(
            &root,
            &ListFilesArgs {
                path: Some("Cargo.toml".into()),
                ..Default::default()
            },
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("not a directory"), "{e}");

        let e = list_files(
            &root,
            &ListFilesArgs {
                path: Some("nope".into()),
                ..Default::default()
            },
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn a_directory_symlink_is_listed_but_never_walked_into() {
        // Without git there is no ignore file to lean on, so this is the
        // path where a walk could leave the workspace or loop forever.
        let root = workspace("symlink", false);
        std::os::unix::fs::symlink("/etc", root.join("out")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();

        // Listed as directories, because that is what they are; a model
        // told `out` is a file would call read_file and get a refusal it
        // cannot act on.
        let shallow = list(&root, &ListFilesArgs::default());
        assert!(lines(&shallow).contains(&"out/"), "{}", shallow.text);
        assert!(lines(&shallow).contains(&"loop/"), "{}", shallow.text);

        let started = std::time::Instant::now();
        let deep = list(
            &root,
            &ListFilesArgs {
                glob: Some("**".into()),
                ..Default::default()
            },
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the walk followed the loop"
        );
        assert!(
            !deep.text.contains("out/passwd"),
            "the walk left the workspace: {}",
            deep.text
        );
        assert!(!deep.text.contains("loop/src"), "{}", deep.text);
    }

    #[test]
    fn structured_content_never_carries_the_listing() {
        let root = workspace("nodup", true);
        let listing = list(&root, &ListFilesArgs::default());
        let json = serde_json::to_string(&listing).unwrap();
        assert!(!json.contains("Cargo.toml"), "{json}");
        assert!(json.contains("\"entries\":4"), "{json}");
        assert!(json.contains("\"source\":\"git\""), "{json}");
        assert!(!json.contains(&root.display().to_string()), "{json}");
    }

    #[test]
    fn strip_dir_only_matches_whole_segments() {
        assert_eq!(strip_dir("src/main.rs", ""), Some("src/main.rs"));
        assert_eq!(strip_dir("src/main.rs", "src"), Some("main.rs"));
        // `src` must not swallow `srcx`, which a plain starts_with would.
        assert_eq!(strip_dir("srcx/main.rs", "src"), None);
        assert_eq!(strip_dir("src", "src"), None);
        assert_eq!(strip_dir("tests/a.rs", "src"), None);
    }
}

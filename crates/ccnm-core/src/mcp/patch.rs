//! `apply_patch`: the only way source in the workspace changes.
//!
//! There is deliberately no `write_file(full_content)`. A whole-file write
//! costs the size of the file on every edit, which is the opposite of what
//! this architecture is for, and it silently discards anything that changed
//! since the model last looked.
//!
//! # The patch is a list of exact replacements
//!
//! Not a unified diff. A diff needs a parser and, in practice, fuzzy
//! matching to survive the hunk headers a model gets wrong; an exact
//! `old` -> `new` replacement is unambiguous, costs the size of the change
//! rather than the file, and gets one property for free that a diff has to
//! work for: **everything outside the replaced span is byte-identical**, so
//! a BOM, CRLF endings and a missing final newline all survive without any
//! code deciding to preserve them.
//!
//! An `old` that appears more than once is refused rather than guessed at,
//! unless the caller says `replace_all`.
//!
//! # Stale baselines
//!
//! Every change to an existing file must carry the `version` that
//! `read_file` returned. If the file has been written since, the patch is
//! refused and nothing happens. Matching `old` alone is not enough: it
//! proves the edited span is unchanged, not that the model's understanding
//! of the rest of the file still holds.
//!
//! # Three phases, and why
//!
//! ```text
//! plan     resolve every path, check every version, read every original,
//!          compute every new content. Nothing has touched the disk. Any
//!          problem here fails the whole call with nothing written.
//! stage    write each new content to a temp file beside its target, and
//!          each original to a backup temp. Still nothing user-visible.
//!          A full disk fails here, not half way through the commit.
//! commit   renames and unlinks only. A rename within a directory is
//!          atomic: a reader sees the old file or the new one, never a
//!          half-written one, and there is no window where the file is
//!          missing.
//! ```
//!
//! If a commit fails part way — which after a successful stage means the
//! filesystem is failing under us — the backups are renamed back and the
//! call reports failure. What it must never do is report success: "some of
//! your files were changed" has to be visible, so a rollback that itself
//! fails is reported louder still, naming every file involved.
//!
//! # The one thing rollback cannot cover
//!
//! Rollback runs in this process. If the process is not there any more —
//! `kill -9`, the ssh transport dropping, the machine losing power — and
//! it happened inside the commit loop, then some files are renamed and
//! some are not, and nothing has said so. Each file is still intact,
//! because each is one atomic rename; what is broken is the agreement
//! between them, which is how a rename lands in one file and not in its
//! caller.
//!
//! So the commit loop is bracketed by a journal ([`Journal`]) in ccnm's
//! own state directory, listing what is about to be renamed and where the
//! backups are. Written and fsynced before the first rename, removed
//! after the last. A journal still on disk that no live process owns means
//! exactly one thing: a patch was interrupted mid-commit. The next patch
//! finds it and refuses, naming which files were changed and which were
//! not.
//!
//! Refuses rather than rolls back, deliberately. By the time anyone looks,
//! the person may have fixed it by hand or committed it to git, and
//! silently reverting their work to satisfy a transaction from an hour ago
//! is a worse outcome than the interruption was. The requirement is that
//! the inconsistency cannot be silent, not that a machine resolves it.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mcp::path::{self, WriteTarget};
use crate::mcp::version_of;

/// Files one call may touch. A patch bigger than this is a refactor that
/// wants `exec_command`, and the transaction gets harder to reason about
/// the longer it is.
pub const MAX_FILES: usize = 50;

/// Total bytes of new content one call may carry, across every file. The
/// arguments travel through the model's context, so this is a limit on the
/// request as much as on the disk.
pub const MAX_CONTENT_BYTES: usize = 1024 * 1024;

/// How many places to look for `old` spelled differently, and how many
/// lines of the answer to print. Both exist because `near_miss` runs on
/// a *failed* edit, where nothing about the input has been vouched for:
/// the file may be 16 MiB and `old` may be a megabyte of it. Measured
/// without the first cap, one failed edit took **57.7 seconds**.
const MAX_NEAR_MISS_CANDIDATES: usize = 64;
const MAX_NEAR_MISS_SHOWN: usize = 20;

/// Files this size and above are refused for editing. `apply_patch` holds
/// the whole file in memory to replace inside it; a source file is never
/// close to this and a data file has no business being patched.
pub const MAX_EDIT_BYTES: u64 = 16 * 1024 * 1024;

/// What to do with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Create a file that does not exist. Missing parent directories are
    /// created with it.
    Add,
    /// Replace exact strings inside an existing file.
    Update,
    Delete,
    /// Rename. The content is untouched.
    Move,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Add => "add",
            Op::Update => "update",
            Op::Delete => "delete",
            Op::Move => "move",
        }
    }
}

/// One exact replacement inside a file.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct Edit {
    /// Text to find. Must appear exactly once unless `replace_all` is set.
    pub old: String,
    /// Text to put in its place. Empty deletes the old text.
    pub new: String,
    /// Replace every occurrence instead of requiring exactly one.
    #[serde(default)]
    pub replace_all: Option<bool>,
}

/// One file's worth of the patch.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct FilePatch {
    pub op: Option<Op>,
    /// Path relative to the workspace root.
    pub path: String,
    /// Destination, for `move`.
    #[serde(default)]
    pub to: Option<String>,
    /// The `version` `read_file` returned. Required for update, delete and
    /// move; the patch is refused if the file has changed since.
    #[serde(default)]
    pub version: Option<String>,
    /// Whole content, for `add`.
    #[serde(default)]
    pub content: Option<String>,
    /// Replacements, for `update`. Order does not matter when each one
    /// matches the file you read; it does when one edit's `new` text is
    /// what a later edit's `old` looks for.
    #[serde(default)]
    pub edits: Option<Vec<Edit>>,
}

/// Arguments of `apply_patch`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ApplyPatchArgs {
    /// The files to change. Either all of them are applied or none are.
    pub files: Vec<FilePatch>,
    /// Check everything and report what would happen, writing nothing.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub op: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub edits: u32,
    pub before_bytes: u64,
    pub after_bytes: u64,
    /// The file's new version, to pass to the next `apply_patch`. Absent
    /// for a delete and for a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The result of one `apply_patch`. No file content: the caller already has
/// what it sent, and what it needs back is the new version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchResult {
    #[serde(skip)]
    pub text: String,
    pub dry_run: bool,
    pub files_changed: u32,
    pub files: Vec<FileChange>,
}

/// Apply `args` to the workspace at `root`, all of it or none of it.
///
/// `journal_dir` is [`crate::paths::patches_dir`]. `None` disables the
/// mid-commit journal, which is what a caller with no state directory
/// gets; everything else works the same.
pub fn apply_patch(
    root: &Path,
    journal_dir: Option<&Path>,
    args: &ApplyPatchArgs,
) -> Result<PatchResult> {
    let dry_run = args.dry_run.unwrap_or(false);
    // Before anything, including a dry run: an abandoned journal means the
    // files in it may disagree with each other, and planning the next
    // patch on top of that is how a small inconsistency becomes a large
    // one.
    if let Some(dir) = journal_dir {
        Journal::check_abandoned(dir, root)?;
    }
    let plan = plan(root, args)?;
    if dry_run {
        return Ok(report(&plan, true, Vec::new()));
    }
    sweep_stale_temps(&plan);
    let staged = stage(plan)?;
    let journal = journal_dir
        .map(|dir| Journal::open(dir, root, &staged))
        .transpose()?;
    // The journal lives until this call returns: dropping it is what
    // removes it, and until then an interrupted process leaves it behind
    // for the next patch to find.
    let versions = commit(staged.as_slice(), journal.as_ref())?;
    // Borrowed, not moved: `Staged` owns the cleanup of its temp files, so
    // it must live until the end of the call rather than be taken apart.
    Ok(report(staged.iter().map(|s| &s.planned), false, versions))
}

/// A change worked out in full, with nothing written yet.
struct Planned {
    op: Op,
    rel: String,
    abs: PathBuf,
    to_rel: Option<String>,
    to_abs: Option<PathBuf>,
    /// New content for add and update.
    new_content: Option<Vec<u8>>,
    /// Original content, kept for the backup that makes rollback possible.
    original: Option<Vec<u8>>,
    /// Permissions to carry over, so patching a script does not
    /// un-executable it.
    mode: Option<std::fs::Permissions>,
    edits: u32,
    before_bytes: u64,
    after_bytes: u64,
    /// Directories this change will have to create, outermost first.
    new_dirs: Vec<PathBuf>,
}

/// Phase one. Every check that can be made without writing is made here,
/// so a patch that is going to fail fails before the disk is touched.
fn plan(root: &Path, args: &ApplyPatchArgs) -> Result<Vec<Planned>> {
    if args.files.is_empty() {
        return Err(Error::invalid_args("files is empty; nothing to apply"));
    }
    if args.files.len() > MAX_FILES {
        return Err(Error::invalid_args(format!(
            "{} files in one patch; the limit is {MAX_FILES}",
            args.files.len()
        )));
    }
    let content_bytes: usize = args
        .files
        .iter()
        .map(|f| {
            f.content.as_ref().map_or(0, String::len)
                + f.edits.as_ref().map_or(0, |edits| {
                    edits.iter().map(|e| e.old.len() + e.new.len()).sum()
                })
        })
        .sum();
    if content_bytes > MAX_CONTENT_BYTES {
        return Err(Error::invalid_args(format!(
            "the patch carries {content_bytes} bytes of content; the limit is {MAX_CONTENT_BYTES}"
        )));
    }

    let mut planned = Vec::with_capacity(args.files.len());
    // Two files that resolve to the same place would race each other during
    // the commit and leave whichever lost. Refused rather than ordered.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for file in &args.files {
        let one = plan_one(root, file)?;
        for touched in [Some(&one.rel), one.to_rel.as_ref()].into_iter().flatten() {
            if !seen.insert(touched.clone()) {
                return Err(Error::invalid_args(format!(
                    "{touched} appears twice in one patch; combine the changes into one entry"
                )));
            }
        }
        planned.push(one);
    }
    Ok(planned)
}

fn plan_one(root: &Path, file: &FilePatch) -> Result<Planned> {
    let op = file
        .op
        .ok_or_else(|| Error::invalid_args(format!("{}: op is required", file.path)))?;
    let target = path::resolve_write(root, &file.path)?;
    let rel = target.rel().to_string();

    match op {
        Op::Add => plan_add(file, target, rel),
        Op::Update => plan_update(file, target, rel),
        Op::Delete => plan_delete(file, target, rel),
        Op::Move => plan_move(root, file, target, rel),
    }
}

fn plan_add(file: &FilePatch, target: WriteTarget, rel: String) -> Result<Planned> {
    if target.exists() {
        return Err(Error::invalid_args(format!(
            "{rel} already exists; use op \"update\" to change it"
        )));
    }
    let content = file
        .content
        .as_ref()
        .ok_or_else(|| Error::invalid_args(format!("{rel}: op \"add\" needs content")))?;
    if file.edits.is_some() {
        return Err(Error::invalid_args(format!(
            "{rel}: op \"add\" takes content, not edits"
        )));
    }
    let bytes = content.as_bytes().to_vec();
    let after_bytes = bytes.len() as u64;
    Ok(Planned {
        op: Op::Add,
        new_dirs: missing_dirs(target.abs()),
        rel,
        abs: target.abs().to_path_buf(),
        to_rel: None,
        to_abs: None,
        new_content: Some(bytes),
        original: None,
        mode: None,
        edits: 0,
        before_bytes: 0,
        after_bytes,
    })
}

fn plan_update(file: &FilePatch, target: WriteTarget, rel: String) -> Result<Planned> {
    let (original, meta) = read_existing(&target, &rel)?;
    check_version(file, &meta, &rel)?;
    let edits = file.edits.as_deref().unwrap_or_default();
    if edits.is_empty() {
        return Err(Error::invalid_args(format!(
            "{rel}: op \"update\" needs at least one edit"
        )));
    }
    if file.content.is_some() {
        return Err(Error::invalid_args(format!(
            "{rel}: op \"update\" takes edits, not content; apply_patch never writes a whole file"
        )));
    }
    let text = String::from_utf8(original.clone()).map_err(|_| {
        Error::invalid_args(format!(
            "{rel} is not valid UTF-8; apply_patch only edits text"
        ))
    })?;
    let (new_text, applied) = apply_edits(&text, edits, &rel)?;
    let before_bytes = original.len() as u64;
    let after_bytes = new_text.len() as u64;
    Ok(Planned {
        op: Op::Update,
        rel,
        abs: target.abs().to_path_buf(),
        to_rel: None,
        to_abs: None,
        new_content: Some(new_text.into_bytes()),
        original: Some(original),
        mode: Some(meta.permissions()),
        edits: applied,
        before_bytes,
        after_bytes,
        new_dirs: Vec::new(),
    })
}

fn plan_delete(file: &FilePatch, target: WriteTarget, rel: String) -> Result<Planned> {
    let (original, meta) = read_existing(&target, &rel)?;
    check_version(file, &meta, &rel)?;
    let before_bytes = original.len() as u64;
    Ok(Planned {
        op: Op::Delete,
        rel,
        abs: target.abs().to_path_buf(),
        to_rel: None,
        to_abs: None,
        new_content: None,
        original: Some(original),
        mode: Some(meta.permissions()),
        edits: 0,
        before_bytes,
        after_bytes: 0,
        new_dirs: Vec::new(),
    })
}

fn plan_move(root: &Path, file: &FilePatch, target: WriteTarget, rel: String) -> Result<Planned> {
    let to = file
        .to
        .as_deref()
        .ok_or_else(|| Error::invalid_args(format!("{rel}: op \"move\" needs to")))?;
    // The destination goes through exactly the same policy as the source.
    let destination = path::resolve_write(root, to)?;
    if destination.exists() {
        return Err(Error::invalid_args(format!(
            "{} already exists; move will not overwrite it",
            destination.rel()
        )));
    }
    if destination.rel() == rel {
        return Err(Error::invalid_args(format!("{rel}: move to itself")));
    }
    if !target.exists() {
        return Err(Error::invalid_args(format!("{rel} does not exist")));
    }
    let meta = std::fs::metadata(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot stat {rel}")).with_source(e))?;
    if !meta.is_file() {
        return Err(Error::invalid_args(format!("{rel} is not a regular file")));
    }
    check_version(file, &meta, &rel)?;
    let before_bytes = meta.len();
    Ok(Planned {
        op: Op::Move,
        new_dirs: missing_dirs(destination.abs()),
        rel,
        abs: target.abs().to_path_buf(),
        to_rel: Some(destination.rel().to_string()),
        to_abs: Some(destination.abs().to_path_buf()),
        new_content: None,
        original: None,
        mode: None,
        edits: 0,
        before_bytes,
        after_bytes: before_bytes,
    })
}

fn read_existing(target: &WriteTarget, rel: &str) -> Result<(Vec<u8>, std::fs::Metadata)> {
    if !target.exists() {
        return Err(Error::invalid_args(format!("{rel} does not exist")));
    }
    let meta = std::fs::metadata(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot stat {rel}")).with_source(e))?;
    if meta.is_dir() {
        return Err(Error::invalid_args(format!("{rel} is a directory")));
    }
    if !meta.is_file() {
        return Err(Error::invalid_args(format!(
            "{rel} is not a regular file; ccnm will not open it"
        )));
    }
    if meta.len() > MAX_EDIT_BYTES {
        return Err(Error::invalid_args(format!(
            "{rel} is {} bytes; apply_patch edits files under {MAX_EDIT_BYTES}",
            meta.len()
        )));
    }
    let bytes = std::fs::read(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot read {rel}")).with_source(e))?;
    Ok((bytes, meta))
}

/// The stale-baseline check. Refusing a patch with no version at all is
/// the point: it means the model never read the file, so it is editing
/// from memory or from a guess.
fn check_version(file: &FilePatch, meta: &std::fs::Metadata, rel: &str) -> Result<()> {
    let Some(claimed) = file.version.as_deref() else {
        return Err(Error::invalid_args(format!(
            "{rel}: version is required for this op; call read_file first and pass the version it returns"
        )));
    };
    let actual = version_of(meta);
    if claimed != actual {
        return Err(Error::new(
            crate::error::ErrorCode::StaleEpoch,
            format!(
                "{rel} has changed since you read it (version {claimed} is now {actual}); read it again before patching, or your edit would overwrite whatever changed"
            ),
        ));
    }
    Ok(())
}

/// Directories on the way to `target` that do not exist yet, outermost
/// first, so they can be created in order and removed in reverse.
fn missing_dirs(target: &Path) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut dir = target.parent();
    while let Some(current) = dir {
        if current.is_dir() {
            break;
        }
        missing.push(current.to_path_buf());
        dir = current.parent();
    }
    missing.reverse();
    missing
}

/// Apply a file's edits.
///
/// Two passes, and which one runs is not the caller's business.
///
/// The first resolves every edit against the file as it was read. If all
/// of them land there, exactly once each and without overlapping, they
/// are applied by position.
///
/// What that buys is not "any order works" — two edits on unrelated
/// text already worked in either order. It is that **edits stop
/// interfering with each other**. Applied in sequence, an edit whose
/// replacement text contains a later edit's `old` leaves two copies of
/// it, and the later edit is refused as ambiguous although the patch was
/// perfectly well defined against the file as read. Swapping two names
/// is the smallest case of it and was simply impossible before.
///
/// Cline reports roughly 10% more successful diff edits after making
/// their apply order-invariant, nearly 25% on one model, noting that
/// models "frequently return diffs out of order" even when told not to.
///
/// Anything else falls through to the second pass, which applies the
/// edits in order, each to the result of the last. That is what makes a
/// chain work — an edit whose `old` only exists because an earlier edit
/// created it — and it is where every refusal comes from, so the errors
/// a caller sees are unchanged.
fn apply_edits(text: &str, edits: &[Edit], rel: &str) -> Result<(String, u32)> {
    if let Some(done) = apply_by_position(text, edits) {
        return Ok(done);
    }
    apply_in_order(text, edits, rel)
}

/// Resolve every edit against the original text and apply them by
/// position, or decline. Declining is not an error: the caller falls back
/// to ordered application, which is where refusals are worded.
///
/// `None` means "these edits are not independent of each other" — one of
/// them does not match the original at all, one is ambiguous, or two want
/// overlapping text.
fn apply_by_position(text: &str, edits: &[Edit]) -> Option<(String, u32)> {
    let crlf = text.contains("\r\n");
    // (start, end, replacement) for every occurrence every edit claims.
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for edit in edits {
        if edit.old.is_empty() {
            return None;
        }
        let (old, new) = match_form(text, &edit.old, &edit.new, crlf);
        let found: Vec<usize> = text.match_indices(old.as_str()).map(|(at, _)| at).collect();
        match (found.len(), edit.replace_all.unwrap_or(false)) {
            (0, _) => return None,
            (1, _) => {}
            (_, true) => {}
            // Ambiguous. Ordered application says so, with its own wording.
            (_, false) => return None,
        }
        for at in found {
            spans.push((at, at + old.len(), new.clone()));
        }
    }
    spans.sort_by_key(|(start, _, _)| *start);
    // Two edits reaching for the same bytes cannot both be honoured, and
    // guessing which one wins is exactly the silent mis-edit this whole
    // module exists to avoid.
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return None;
    }
    // Back to front, so the offsets still ahead are unaffected.
    let mut out = text.to_string();
    for (start, end, new) in spans.iter().rev() {
        out.replace_range(start..end, new);
    }
    Some((out, edits.len() as u32))
}

/// Apply the edits in order, each to the result of the last.
fn apply_in_order(text: &str, edits: &[Edit], rel: &str) -> Result<(String, u32)> {
    // A file written on Windows has CRLF endings, but read_file shows the
    // model LF, so its `old` will not match the bytes. Translate the
    // caller's strings into the file's own convention before matching, and
    // fall back to what was sent for a file with mixed endings.
    let crlf = text.contains("\r\n");
    let mut current = text.to_string();
    let mut applied = 0u32;
    for (index, edit) in edits.iter().enumerate() {
        if edit.old.is_empty() {
            return Err(Error::invalid_args(format!(
                "{rel} edit {}: old is empty; apply_patch does not insert at a guessed position",
                index + 1
            )));
        }
        let (old, new) = match_form(&current, &edit.old, &edit.new, crlf);
        let count = current.matches(old.as_str()).count();
        let replace_all = edit.replace_all.unwrap_or(false);
        if count == 0 {
            return Err(Error::invalid_args(format!(
                "{rel} edit {}: old does not appear in the file{}{}",
                index + 1,
                if index > 0 {
                    "; note that edits apply in order, so an earlier edit may have changed it"
                } else {
                    ""
                },
                near_miss(&current, &old)
            )));
        }
        if count > 1 && !replace_all {
            return Err(Error::invalid_args(format!(
                "{rel} edit {}: old appears {count} times; include more surrounding text to make it unique, or set replace_all",
                index + 1
            )));
        }
        current = if replace_all {
            current.replace(old.as_str(), &new)
        } else {
            current.replacen(old.as_str(), &new, 1)
        };
        applied += 1;
    }
    Ok((current, applied))
}

/// What to add to "old does not appear in the file" so the caller can do
/// something about it.
///
/// "No match" on its own sends a model round the loop it was already in:
/// read the file again, send almost the same string, fail again. The
/// documented ways these edits go wrong are boring and specific -- line
/// endings, a tab against four spaces, indentation that drifted a level --
/// so the useful answer names which one it is and shows the file's own
/// bytes to copy.
///
/// Deliberately diagnosis and not repair. A tool that quietly edits the
/// nearest similar thing is the failure mode with the worst tail: in a
/// large repository a near match is often a *different* function, the
/// edit lands there, and nobody finds out for weeks.
///
/// Returns "" when there is nothing useful to say, so it can be appended
/// unconditionally.
fn near_miss(text: &str, old: &str) -> String {
    let squeezed: Vec<String> = old.lines().map(squeeze).collect();
    // A trailing newline in `old` produces no final line; either way the
    // comparison is over the lines that carry content.
    let wanted: Vec<&String> = squeezed.iter().filter(|line| !line.is_empty()).collect();
    if wanted.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();

    // Every line that could start the block. Bounded on both sides: this
    // runs on a *failed* edit, where the file can be 16 MiB and `old` can
    // be a megabyte, and the unbounded version of this loop is
    // lines x wanted -- around 10^10 comparisons on those numbers, which
    // is a tool call that never comes back. A whitespace difference that
    // occurs more than MAX_CANDIDATES times is diagnosed from the first
    // one just as well.
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| squeeze(line) == *wanted[0])
        .map(|(at, _)| at)
        .take(MAX_NEAR_MISS_CANDIDATES)
        .collect();

    for start in &starts {
        if let Some(last) = block_ends_at(&lines, *start, &wanted) {
            return format!(
                "\nthe same text is at line {}, written with different whitespace; copy these bytes exactly as read_file returned them:\n{}",
                start + 1,
                shown(&lines, *start, last)
            );
        }
    }

    // Not the whole block, but its first line is somewhere.
    match starts.first() {
        Some(at) => format!(
            "\nits first line is at line {}, so the rest is what differs:\n    {}",
            at + 1,
            lines[*at]
        ),
        None => String::new(),
    }
}

/// The line `wanted` ends on if it starts at `start`, ignoring how each
/// line is spaced and skipping blank lines in the file, or `None` if it
/// does not match all the way through.
fn block_ends_at(lines: &[&str], start: usize, wanted: &[&String]) -> Option<usize> {
    let mut cursor = start;
    let mut last = start;
    for want in wanted {
        while cursor < lines.len() && squeeze(lines[cursor]).is_empty() {
            cursor += 1;
        }
        if cursor >= lines.len() || squeeze(lines[cursor]) != **want {
            return None;
        }
        last = cursor;
        cursor += 1;
    }
    Some(last)
}

/// The file's own bytes for lines `first..=last`, cut in the middle if
/// there are a lot of them. This text goes into the model's context, so
/// it is as bounded as every other tool result: the point is to show the
/// spacing, and the first and last few lines show it.
fn shown(lines: &[&str], first: usize, last: usize) -> String {
    let count = last - first + 1;
    if count <= MAX_NEAR_MISS_SHOWN {
        return lines[first..=last]
            .iter()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let half = MAX_NEAR_MISS_SHOWN / 2;
    let head = lines[first..first + half].iter();
    let tail = lines[last + 1 - half..=last].iter();
    head.map(|line| format!("    {line}"))
        .chain(std::iter::once(format!(
            "    ... {} more lines ...",
            count - 2 * half
        )))
        .chain(tail.map(|line| format!("    {line}")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A line reduced to what it says, with every run of whitespace collapsed
/// to one space. Two lines with the same squeeze differ only in spacing,
/// which covers tabs against spaces, a changed indent level, trailing
/// blanks and a stray carriage return in one go.
fn squeeze(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pick the form of `old`/`new` that matches how the file is written.
fn match_form(text: &str, old: &str, new: &str, crlf: bool) -> (String, String) {
    if crlf && !old.contains("\r\n") && old.contains('\n') {
        let translated = to_crlf(old);
        if text.contains(&translated) {
            return (translated, to_crlf(new));
        }
    }
    (old.to_string(), new.to_string())
}

fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// A change with its bytes already on disk, waiting for a rename.
struct Staged {
    planned: Planned,
    /// Temp holding the new content, for add and update.
    new_temp: Option<PathBuf>,
    /// Temp holding the original, so a failed commit can be undone.
    backup: Option<PathBuf>,
    /// Directories created during staging, outermost first.
    created_dirs: Vec<PathBuf>,
}

/// Temp files are removed when the staging goes out of scope, whatever the
/// reason it went out of scope.
///
/// The explicit paths already cover the ordinary ones: a failed stage
/// discards, a successful commit renames the new content into place and
/// unlinks the backups. This is for the rest — an early return added
/// later, a panic — because the cost of missing one is a `.ccnm-…` file
/// left in someone's source tree, which `git status` then shows as
/// untracked and a later `git add -A` can commit.
///
/// After a commit the new temp has been renamed away and the backup is
/// already gone, so both removals are no-ops. Directories are deliberately
/// *not* touched here: a committed patch's new directories must stay.
impl Drop for Staged {
    fn drop(&mut self) {
        for temp in [&self.new_temp, &self.backup].into_iter().flatten() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// The prefix of every temp file `apply_patch` writes.
pub const TEMP_PREFIX: &str = ".ccnm-";

/// A temp file this old is not one somebody is still working on.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

/// Remove temp files an earlier run was killed before it could clean up.
///
/// The one hole [`Drop`] cannot cover is the process dying without
/// unwinding: `kill -9`, the ssh transport dropping, the machine losing
/// power mid-patch. What that leaves is a `.ccnm-…` file beside a real
/// one, and nothing else would ever remove it.
///
/// Only the directories this patch is about to write to are swept, so the
/// cost is one `read_dir` per directory already being written, not a walk
/// of the workspace. Only files older than [`STALE_TEMP`] go, so a patch
/// running concurrently in another session is never robbed of its staging.
fn sweep_stale_temps(plan: &[Planned]) {
    let mut swept: BTreeSet<PathBuf> = BTreeSet::new();
    let dirs = plan
        .iter()
        .flat_map(|p| [Some(&p.abs), p.to_abs.as_ref()].into_iter().flatten())
        .filter_map(|path| path.parent().map(Path::to_path_buf));
    for dir in dirs {
        if !swept.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(TEMP_PREFIX) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().is_ok_and(|age| age > STALE_TEMP))
                .unwrap_or(false);
            if stale {
                tracing::warn!(path = %entry.path().display(), "removing a leftover patch temp file");
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Phase two. Everything that can fail for a boring reason — no space, no
/// permission — fails here, where nothing user-visible has changed yet.
fn stage(plan: Vec<Planned>) -> Result<Vec<Staged>> {
    let mut staged: Vec<Staged> = Vec::with_capacity(plan.len());
    for planned in plan {
        match stage_one(planned) {
            Ok(one) => staged.push(one),
            Err(e) => {
                discard(&staged);
                return Err(e);
            }
        }
    }
    Ok(staged)
}

fn stage_one(planned: Planned) -> Result<Staged> {
    let mut created_dirs = Vec::new();
    for dir in &planned.new_dirs {
        match std::fs::create_dir(dir) {
            Ok(()) => created_dirs.push(dir.clone()),
            // Another file in this same patch already made it. Two files
            // into one new directory is an ordinary patch — `add
            // tests/a.py` and `tests/b.py` with no tests/ yet — and each
            // was planned before either was staged, so both carry the
            // directory in their list. It is not recorded as created here:
            // whoever made it is the one that should remove it if this
            // patch is rolled back.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
            Err(e) => {
                return Err(Error::invalid_args(format!(
                    "cannot create a directory for {}",
                    planned.rel
                ))
                .with_source(e));
            }
        }
    }

    let new_temp = match &planned.new_content {
        Some(bytes) => {
            let temp = temp_beside(&planned.abs)?;
            write_atomic_temp(&temp, bytes, planned.mode.as_ref(), &planned.rel)?;
            Some(temp)
        }
        None => None,
    };
    let backup = match &planned.original {
        Some(bytes) => {
            let temp = temp_beside(&planned.abs)?;
            write_atomic_temp(&temp, bytes, planned.mode.as_ref(), &planned.rel)?;
            Some(temp)
        }
        None => None,
    };
    Ok(Staged {
        planned,
        new_temp,
        backup,
        created_dirs,
    })
}

/// How long a journal can exist before it is certainly not a commit in
/// progress. A commit is a handful of renames; a minute is four orders of
/// magnitude more than it takes.
const JOURNAL_STALE: Duration = Duration::from_secs(60);

/// One file's line in the journal, in the form a person needs when they
/// are looking at a workspace somebody's patch was interrupted in.
#[derive(Serialize, Deserialize)]
struct JournalLine {
    op: Op,
    rel: String,
    abs: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_rel: Option<String>,
    /// Where the original is, for anyone who wants to put it back by
    /// hand. Stored absolute because a person reads this file directly;
    /// shown relative, because the model reads the message.
    #[serde(skip_serializing_if = "Option::is_none")]
    backup: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct JournalFile {
    /// The process that was doing the renaming, so the reader knows which
    /// process to go looking for.
    pid: u32,
    /// The workspace this patch was in. Only a patch in the *same*
    /// workspace is blocked by it: one project's interrupted commit says
    /// nothing about another project's files, and two workspaces share
    /// one state directory.
    root: PathBuf,
    files: Vec<JournalLine>,
}

/// The record of a commit in progress: what is about to be renamed, and
/// where the originals are.
///
/// Written and fsynced before the first rename, removed when the call
/// leaves — by `Drop`, so an ordinary failure and a panic both clean up.
/// The one thing that does not run `Drop` is the one thing this is for: a
/// process killed outright, part way through the renames.
struct Journal {
    path: PathBuf,
    /// Set when the workspace is known to be inconsistent, so the record
    /// outlives this process on purpose.
    keep: std::cell::Cell<bool>,
}

impl Journal {
    fn open(dir: &Path, root: &Path, staged: &[Staged]) -> Result<Journal> {
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::internal(format!("cannot create {}", dir.display())).with_source(e)
        })?;
        let root = root.to_path_buf();
        let record = JournalFile {
            pid: std::process::id(),
            root,
            files: staged
                .iter()
                .map(|one| JournalLine {
                    op: one.planned.op,
                    rel: one.planned.rel.clone(),
                    abs: one.planned.abs.clone(),
                    to_rel: one.planned.to_rel.clone(),
                    backup: one.backup.clone(),
                })
                .collect(),
        };
        let body = serde_json::to_vec_pretty(&record)
            .map_err(|e| Error::internal("cannot describe the patch").with_source(e))?;
        let path = dir.join(format!(
            "{}-{}.json",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        // Same discipline as the content temps: durable before it is
        // relied on, or a power cut leaves a journal that describes
        // nothing.
        let mut file = std::fs::File::create(&path).map_err(|e| {
            Error::internal(format!("cannot write {}", path.display())).with_source(e)
        })?;
        file.write_all(&body)
            .map_err(|e| Error::internal("cannot write the patch journal").with_source(e))?;
        file.sync_all()
            .map_err(|e| Error::internal("cannot flush the patch journal").with_source(e))?;
        Ok(Journal {
            path,
            keep: std::cell::Cell::new(false),
        })
    }

    /// Leave this journal on disk. Called when a rollback failed, which
    /// means the files really do disagree with each other and the next
    /// patch must not proceed as if they did not.
    fn abandon(&self) {
        self.keep.set(true);
    }

    /// Refuse if an earlier patch was interrupted between renames.
    ///
    /// A journal older than [`JOURNAL_STALE`] belongs to no commit that is
    /// still happening. Age rather than asking whether the pid is alive:
    /// this crate forbids `unsafe`, so liveness would mean spawning a
    /// process on a path that is taken on every patch, and a commit is so
    /// much shorter than a minute that the answer is the same either way.
    /// The cost is that an interruption is invisible for up to a minute —
    /// during which the transport it died with is down anyway.
    fn check_abandoned(dir: &Path, root: &Path) -> Result<()> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let fresh = meta
                .modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age < JOURNAL_STALE);
            if fresh {
                continue;
            }
            // A journal that cannot be read is still evidence that a
            // patch was interrupted; the whole point of this file is that
            // an inconsistency is never silent, so an unreadable one says
            // so rather than being skipped.
            let record = match std::fs::read(&path)
                .ok()
                .and_then(|body| serde_json::from_slice::<JournalFile>(&body).ok())
            {
                Some(record) => record,
                None => {
                    return Err(Error::internal(format!(
                        "{} records an apply_patch that was interrupted, and cannot be read to say which files it was changing.\ncheck the workspace with git status before changing anything else, then delete it.",
                        path.display()
                    )));
                }
            };
            // Another project's interrupted commit says nothing about
            // this project's files.
            if record.root != root {
                continue;
            }
            return Err(Error::internal(interrupted_report(&record, &path)));
        }
        Ok(())
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if !self.keep.get() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What to say when a previous patch was interrupted mid-commit.
///
/// Names every file it was renaming, because the only question worth
/// answering is "which of these actually changed", and only a person
/// looking at the workspace can answer it. `git status` is named because
/// in a git workspace it answers it in one command.
/// The one place ccnm shows the model a path outside the workspace.
///
/// Everything else the server says is workspace-relative on purpose
/// (`server.rs`, design doc section 17): absolute paths tell the model
/// about a machine it has no business knowing the shape of, and they
/// travel back to Anthropic in the transcript. This message is the
/// deliberate exception, and only for the journal file: it is a recovery
/// instruction, "delete this and patching works again" is worthless
/// without saying which file, and the person who has to act on it is
/// reading it through the model. The backups are shown relative, because
/// those are inside the workspace and relative is all anyone needs.
fn interrupted_report(record: &JournalFile, journal: &Path) -> String {
    let mut out = String::from(
        "a previous apply_patch was interrupted while it was renaming files, so these may not agree with each other:\n",
    );
    for line in &record.files {
        out.push_str(&format!("  {} {}", line.op.name(), line.rel));
        if let Some(to) = &line.to_rel {
            out.push_str(&format!(" -> {to}"));
        }
        if let Some(backup) = &line.backup {
            let shown = backup
                .strip_prefix(&record.root)
                .unwrap_or(backup.as_path());
            out.push_str(&format!("   original kept at {}", shown.display()));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "check them before changing anything else -- git status and git diff will show which ones landed.\nnothing has been rolled back: by now the change may be wanted, or already committed.\nwhen you have looked, delete {} and patching works again.\n(process {} was doing the renaming and is gone.)",
        journal.display(),
        record.pid
    ));
    out
}

/// A temp file in the same directory as its target, so the commit rename
/// stays within one filesystem and is therefore atomic.
fn temp_beside(target: &Path) -> Result<PathBuf> {
    let dir = target
        .parent()
        .ok_or_else(|| Error::internal("a workspace path with no parent"))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unique = uuid::Uuid::new_v4().simple().to_string();
    Ok(dir.join(format!("{TEMP_PREFIX}{}-{name}", &unique[..12])))
}

fn write_atomic_temp(
    temp: &Path,
    bytes: &[u8],
    mode: Option<&std::fs::Permissions>,
    rel: &str,
) -> Result<()> {
    let mut file = std::fs::File::create(temp)
        .map_err(|e| Error::invalid_args(format!("cannot write beside {rel}")).with_source(e))?;
    file.write_all(bytes)
        .map_err(|e| Error::invalid_args(format!("cannot write {rel}")).with_source(e))?;
    // Without this the rename can be durable while the contents are not,
    // which after a crash is a file of the right name and the wrong length.
    file.sync_all()
        .map_err(|e| Error::internal(format!("cannot flush {rel}")).with_source(e))?;
    drop(file);
    if let Some(mode) = mode {
        // Carry the original permissions over: patching a script must not
        // stop it being executable.
        std::fs::set_permissions(temp, mode.clone()).map_err(|e| {
            Error::internal(format!("cannot set permissions on {rel}")).with_source(e)
        })?;
    }
    Ok(())
}

/// Throw away staged work that was never committed. The temp files go with
/// [`Drop`]; what needs saying explicitly is the directories, which a
/// committed patch has to keep.
fn discard(staged: &[Staged]) {
    for one in staged {
        for temp in [&one.new_temp, &one.backup].into_iter().flatten() {
            let _ = std::fs::remove_file(temp);
        }
        for dir in one.created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Phase three: renames and unlinks. Returns each file's new version.
///
/// `journal` is the record of this commit, kept on disk when a rollback
/// fails: at that point the files really do disagree with each other, and
/// the next patch must find that out rather than plan on top of it.
fn commit(staged: &[Staged], journal: Option<&Journal>) -> Result<Vec<Option<String>>> {
    let mut versions = Vec::with_capacity(staged.len());
    for (index, one) in staged.iter().enumerate() {
        match commit_one(one) {
            Ok(version) => versions.push(version),
            Err(e) => {
                // Everything before this succeeded. Put it back.
                let undone = rollback(&staged[..index]);
                discard(&staged[index..]);
                return Err(match undone {
                    Ok(()) => Error::internal(format!(
                        "{}: {}. The other files in this patch were rolled back and the workspace is unchanged.",
                        one.planned.rel,
                        e.message()
                    )),
                    Err(failed) => {
                        if let Some(journal) = journal {
                            journal.abandon();
                        }
                        Error::internal(format!(
                            "{}: {}. Rolling back failed too: {failed}. The workspace is PARTIALLY CHANGED; check it before doing anything else.",
                            one.planned.rel,
                            e.message()
                        ))
                    }
                });
            }
        }
    }
    // Committed. The backups have no further use.
    for one in staged {
        if let Some(backup) = &one.backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    Ok(versions)
}

fn commit_one(one: &Staged) -> Result<Option<String>> {
    let planned = &one.planned;
    match planned.op {
        Op::Add | Op::Update => {
            let temp = one
                .new_temp
                .as_ref()
                .ok_or_else(|| Error::internal("staged content is missing"))?;
            // One rename, so there is never a moment when the file is
            // absent and never a partly written file at its name.
            std::fs::rename(temp, &planned.abs).map_err(|e| {
                Error::internal(format!("cannot replace {}", planned.rel)).with_source(e)
            })?;
            let meta = std::fs::metadata(&planned.abs)
                .map_err(|e| Error::internal("cannot stat what was just written").with_source(e))?;
            Ok(Some(version_of(&meta)))
        }
        Op::Delete => {
            std::fs::remove_file(&planned.abs).map_err(|e| {
                Error::internal(format!("cannot remove {}", planned.rel)).with_source(e)
            })?;
            Ok(None)
        }
        Op::Move => {
            let to = planned
                .to_abs
                .as_ref()
                .ok_or_else(|| Error::internal("a move with no destination"))?;
            std::fs::rename(&planned.abs, to).map_err(|e| {
                Error::internal(format!("cannot move {}", planned.rel)).with_source(e)
            })?;
            let meta = std::fs::metadata(to)
                .map_err(|e| Error::internal("cannot stat what was just moved").with_source(e))?;
            Ok(Some(version_of(&meta)))
        }
    }
}

/// Undo committed changes, newest first.
fn rollback(committed: &[Staged]) -> std::result::Result<(), String> {
    let mut failures = Vec::new();
    for one in committed.iter().rev() {
        let planned = &one.planned;
        let outcome = match planned.op {
            // The file did not exist before; removing it restores that.
            Op::Add => std::fs::remove_file(&planned.abs).map_err(|e| e.to_string()),
            Op::Update | Op::Delete => match &one.backup {
                Some(backup) => std::fs::rename(backup, &planned.abs).map_err(|e| e.to_string()),
                None => Err("no backup was staged".to_string()),
            },
            Op::Move => match &planned.to_abs {
                Some(to) => std::fs::rename(to, &planned.abs).map_err(|e| e.to_string()),
                None => Err("a move with no destination".to_string()),
            },
        };
        if let Err(e) = outcome {
            failures.push(format!("{}: {e}", planned.rel));
        }
        for dir in one.created_dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn report<'a>(
    plan: impl IntoIterator<Item = &'a Planned>,
    dry_run: bool,
    versions: Vec<Option<String>>,
) -> PatchResult {
    let files: Vec<FileChange> = plan
        .into_iter()
        .enumerate()
        .map(|(index, planned)| FileChange {
            op: planned.op.name().to_string(),
            path: planned.rel.clone(),
            to: planned.to_rel.clone(),
            edits: planned.edits,
            before_bytes: planned.before_bytes,
            after_bytes: planned.after_bytes,
            version: versions.get(index).cloned().flatten(),
        })
        .collect();

    let mut text = String::new();
    for change in &files {
        text.push_str(&match change.op.as_str() {
            "add" => format!("add    {} ({} bytes)", change.path, change.after_bytes),
            "update" => format!(
                "update {} ({} edit{}, {} -> {} bytes)",
                change.path,
                change.edits,
                if change.edits == 1 { "" } else { "s" },
                change.before_bytes,
                change.after_bytes
            ),
            "delete" => format!("delete {}", change.path),
            _ => format!(
                "move   {} -> {}",
                change.path,
                change.to.as_deref().unwrap_or("?")
            ),
        });
        if let Some(version) = &change.version {
            text.push_str(&format!(" version {version}"));
        }
        text.push('\n');
    }
    text.push_str(&if dry_run {
        format!(
            "[dry run: {} file{} would change, nothing was written]",
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "[{} file{} changed]",
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        )
    });

    PatchResult {
        text,
        dry_run,
        files_changed: files.len() as u32,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::mcp::read::{self, ReadFileArgs};
    use std::fs;

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-patch-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    /// The version a model would have: read the file, keep what came back.
    fn version(root: &Path, path: &str) -> String {
        read::read_file(
            root,
            &ReadFileArgs {
                path: path.to_string(),
                ..Default::default()
            },
        )
        .unwrap()
        .version
    }

    fn update(path: &str, version: &str, old: &str, new: &str) -> FilePatch {
        FilePatch {
            op: Some(Op::Update),
            path: path.to_string(),
            version: Some(version.to_string()),
            edits: Some(vec![Edit {
                old: old.to_string(),
                new: new.to_string(),
                replace_all: None,
            }]),
            ..Default::default()
        }
    }

    fn apply(root: &Path, files: Vec<FilePatch>) -> PatchResult {
        apply_patch(
            root,
            None,
            &ApplyPatchArgs {
                files,
                dry_run: None,
            },
        )
        .unwrap()
    }

    fn fails(root: &Path, files: Vec<FilePatch>) -> Error {
        match apply_patch(
            root,
            None,
            &ApplyPatchArgs {
                files,
                dry_run: None,
            },
        ) {
            Err(e) => e,
            Ok(r) => panic!("expected a refusal, got {} files changed", r.files_changed),
        }
    }

    fn text(root: &Path, path: &str) -> String {
        fs::read_to_string(root.join(path)).unwrap()
    }

    /// Two new files under one new directory is an ordinary patch, and it
    /// used to fail: both were planned before either was staged, so both
    /// carried `tests/` in their list of directories to create and the
    /// second `create_dir` hit AlreadyExists. Nothing was written and the
    /// model got "cannot create a directory for tests/b.py".
    #[test]
    fn two_new_files_can_share_one_new_directory() {
        let root = workspace("shared-dir");
        let add = |path: &str, content: &str| FilePatch {
            op: Some(Op::Add),
            path: path.to_string(),
            content: Some(content.to_string()),
            ..Default::default()
        };
        let out = apply(
            &root,
            vec![add("tests/deep/a.py", "a\n"), add("tests/deep/b.py", "b\n")],
        );
        assert_eq!(out.files_changed, 2);
        assert_eq!(text(&root, "tests/deep/a.py"), "a\n");
        assert_eq!(text(&root, "tests/deep/b.py"), "b\n");
    }

    /// A patch that fails after its directories exist must not leave them
    /// behind, and must not remove a directory an earlier file in the same
    /// patch made and still needs.
    #[test]
    fn a_shared_new_directory_is_removed_when_the_patch_fails() {
        let root = workspace("shared-dir-fail");
        let add = |path: &str, content: &str| FilePatch {
            op: Some(Op::Add),
            path: path.to_string(),
            content: Some(content.to_string()),
            ..Default::default()
        };
        let err = fails(
            &root,
            vec![
                add("tests/deep/a.py", "a\n"),
                // Refused in the planning phase: no version.
                update("src/main.rs", "", "let x = 1;", "let x = 2;"),
            ],
        );
        assert_eq!(err.code(), ErrorCode::StaleEpoch);
        assert!(!root.join("tests").exists(), "no directory may be left");
        assert!(!root.join("tests/deep/a.py").exists());
    }

    /// The one thing a killed patch can leave in someone's source tree is
    /// a `.ccnm-…` temp file, which `git status` then shows as untracked
    /// and a later `git add -A` can commit. The next patch in that
    /// directory clears it out.
    #[test]
    fn a_leftover_temp_file_is_swept_by_the_next_patch() {
        let root = workspace("sweep");
        let old = root.join("src/.ccnm-deadbeef1234-main.rs");
        fs::write(&old, "half-written").unwrap();
        // Two hours old: nobody is still working on it.
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        filetime(&old, long_ago);
        // A fresh one, which could belong to a patch running right now.
        let fresh = root.join("src/.ccnm-abcdef123456-lib.rs");
        fs::write(&fresh, "in flight").unwrap();

        let v = version(&root, "src/main.rs");
        apply(
            &root,
            vec![update("src/main.rs", &v, "let x = 1;", "let x = 2;")],
        );
        assert!(!old.exists(), "the stale temp must be gone");
        assert!(fresh.exists(), "a fresh temp may belong to another patch");
    }

    /// Every temp file is gone when the call returns, however it returned.
    #[test]
    fn no_temp_files_survive_a_patch_that_worked_or_one_that_failed() {
        let root = workspace("no-temps");
        let v = version(&root, "src/main.rs");
        apply(
            &root,
            vec![update("src/main.rs", &v, "let x = 1;", "let x = 2;")],
        );
        assert_eq!(temps(&root.join("src")), Vec::<String>::new());

        // A patch whose second file is refused in planning.
        let v = version(&root, "src/main.rs");
        let _ = fails(
            &root,
            vec![
                update("src/main.rs", &v, "let x = 2;", "let x = 3;"),
                update("src/lib.rs", "stale", "pub fn a() {}", "pub fn c() {}"),
            ],
        );
        assert_eq!(temps(&root.join("src")), Vec::<String>::new());
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 2;\n}\n"
        );
    }

    fn temps(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TEMP_PREFIX))
            .collect();
        found.sort();
        found
    }

    /// Set a file's modification time, so "old enough to sweep" can be
    /// tested without waiting an hour.
    fn filetime(path: &Path, when: std::time::SystemTime) {
        let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let stamp = std::process::Command::new("/usr/bin/touch")
            .arg("-t")
            .arg(unix_to_touch(secs))
            .arg(path)
            .status()
            .unwrap();
        assert!(stamp.success(), "touch failed");
    }

    /// Unix seconds to touch(1)'s `[[CC]YY]MMDDhhmm[.SS]`, via date(1) so
    /// this test carries no calendar arithmetic of its own.
    fn unix_to_touch(secs: u64) -> String {
        let out = std::process::Command::new("/bin/date")
            .args(["-r", &secs.to_string(), "+%Y%m%d%H%M.%S"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn read_patch_read_shows_the_new_content_immediately() {
        let root = workspace("roundtrip");
        let before = read::read_file(
            &root,
            &ReadFileArgs {
                path: "src/main.rs".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(before.text.contains("let x = 1;"));

        let result = apply(
            &root,
            vec![update(
                "src/main.rs",
                &before.version,
                "let x = 1;",
                "let x = 42;",
            )],
        );
        assert_eq!(result.files_changed, 1);
        let new_version = result.files[0].version.clone().unwrap();
        assert_ne!(new_version, before.version, "the version must move");

        let after = read::read_file(
            &root,
            &ReadFileArgs {
                path: "src/main.rs".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(after.text.contains("let x = 42;"), "{}", after.text);
        assert!(!after.text.contains("let x = 1;"));
        // And the version read_file now reports is the one apply_patch
        // handed back, so the next patch can chain off it without a re-read.
        assert_eq!(after.version, new_version);
        let again = apply(&root, vec![update("src/main.rs", &new_version, "42", "43")]);
        assert_eq!(again.files_changed, 1);
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 43;\n}\n"
        );
    }

    #[test]
    fn a_stale_baseline_is_refused_and_writes_nothing() {
        let root = workspace("stale");
        let stale = version(&root, "src/main.rs");
        // The user edits the file after the model read it.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    let x = 1; // mine\n}\n",
        )
        .unwrap();

        let err = fails(
            &root,
            vec![update("src/main.rs", &stale, "let x = 1;", "let x = 42;")],
        );
        assert_eq!(err.code(), ErrorCode::StaleEpoch);
        assert!(
            err.message().contains("has changed since you read it"),
            "{err}"
        );
        // The user's line is still there, untouched.
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 1; // mine\n}\n"
        );
    }

    #[test]
    fn a_change_without_a_version_is_refused() {
        let root = workspace("noversion");
        let err = fails(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/main.rs".into(),
                edits: Some(vec![Edit {
                    old: "let x = 1;".into(),
                    new: "y".into(),
                    replace_all: None,
                }]),
                ..Default::default()
            }],
        );
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("read_file first"), "{err}");
    }

    #[test]
    fn a_multi_file_patch_that_fails_changes_nothing() {
        let root = workspace("atomic");
        let main_before = text(&root, "src/main.rs");
        let lib_before = text(&root, "src/lib.rs");
        let main_version = version(&root, "src/main.rs");

        // The first file is fine; the second names text that is not there.
        let err = fails(
            &root,
            vec![
                update("src/main.rs", &main_version, "let x = 1;", "let x = 99;"),
                update(
                    "src/lib.rs",
                    &version(&root, "src/lib.rs"),
                    "pub fn zzz()",
                    "x",
                ),
            ],
        );
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert_eq!(
            text(&root, "src/main.rs"),
            main_before,
            "the first file was written anyway"
        );
        assert_eq!(text(&root, "src/lib.rs"), lib_before);

        // And the same when the failure is a stale version on the second file.
        let lib_stale = version(&root, "src/lib.rs");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(root.join("src/lib.rs"), "pub fn a() {}\npub fn c() {}\n").unwrap();
        let err = fails(
            &root,
            vec![
                update("src/main.rs", &main_version, "let x = 1;", "let x = 99;"),
                update("src/lib.rs", &lib_stale, "pub fn a()", "pub fn z()"),
            ],
        );
        assert_eq!(err.code(), ErrorCode::StaleEpoch);
        assert_eq!(text(&root, "src/main.rs"), main_before);
        assert_eq!(text(&root, "src/lib.rs"), "pub fn a() {}\npub fn c() {}\n");
    }

    #[test]
    fn no_temporary_files_survive_a_failure_or_a_success() {
        let root = workspace("leftovers");
        let before: Vec<String> = fs::read_dir(root.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        let _ = fails(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                "nope",
                "x",
            )],
        );
        apply(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                "let x = 1;",
                "let x = 2;",
            )],
        );

        let mut after: Vec<String> = fs::read_dir(root.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        after.sort();
        let mut before = before;
        before.sort();
        assert_eq!(after, before, "a temp file was left behind");
        assert!(!after.iter().any(|n| n.starts_with(".ccnm-")), "{after:?}");
    }

    #[test]
    fn the_write_policy_is_the_one_the_read_tools_use() {
        let root = workspace("policy");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
        let outside = root.parent().unwrap().join("outside.txt");
        fs::write(&outside, "not yours\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();

        for (path, code) in [
            ("../outside.txt", ErrorCode::Policy),
            ("/etc/hosts", ErrorCode::Policy),
            ("~/.ssh/authorized_keys", ErrorCode::Policy),
            (".git/config", ErrorCode::Policy),
            ("link.txt", ErrorCode::Policy),
        ] {
            let err = fails(
                &root,
                vec![FilePatch {
                    op: Some(Op::Add),
                    path: path.to_string(),
                    content: Some("owned\n".into()),
                    ..Default::default()
                }],
            );
            assert_eq!(err.code(), code, "{path} -> {err}");
        }
        assert_eq!(fs::read_to_string(&outside).unwrap(), "not yours\n");
        assert_eq!(text(&root, ".git/config"), "[core]\n");
    }

    #[test]
    fn a_move_checks_the_destination_with_the_same_policy() {
        let root = workspace("move-policy");
        let v = version(&root, "src/main.rs");
        for to in ["../escaped.rs", "/tmp/escaped.rs", ".git/escaped.rs"] {
            let err = fails(
                &root,
                vec![FilePatch {
                    op: Some(Op::Move),
                    path: "src/main.rs".into(),
                    to: Some(to.to_string()),
                    version: Some(v.clone()),
                    ..Default::default()
                }],
            );
            assert_eq!(err.code(), ErrorCode::Policy, "{to} -> {err}");
        }
        assert!(root.join("src/main.rs").exists());
    }

    #[test]
    fn crlf_a_bom_and_a_missing_final_newline_all_survive() {
        let root = workspace("format");
        // CRLF, a BOM, and no newline at the end: three things a careless
        // patch tool silently normalises.
        let original = b"\xef\xbb\xbfline one\r\nline two\r\nline three".to_vec();
        fs::write(root.join("dos.txt"), &original).unwrap();
        let chunk = read::read_file(
            &root,
            &ReadFileArgs {
                path: "dos.txt".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(chunk.line_ending, read::LineEnding::Crlf);
        assert_eq!(chunk.final_newline, Some(false));

        // The model sends what read_file showed it: LF, no BOM.
        apply(
            &root,
            vec![update("dos.txt", &chunk.version, "line two", "line 2")],
        );

        let after = fs::read(root.join("dos.txt")).unwrap();
        assert_eq!(
            after,
            b"\xef\xbb\xbfline one\r\nline 2\r\nline three".to_vec()
        );
        assert!(after.starts_with(b"\xef\xbb\xbf"), "the BOM was dropped");
        assert!(!after.ends_with(b"\n"), "a final newline was added");
        assert_eq!(after.windows(2).filter(|w| *w == b"\r\n").count(), 2);
    }

    #[test]
    fn a_multi_line_edit_matches_across_crlf_endings() {
        let root = workspace("crlf-multiline");
        fs::write(root.join("dos.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        let v = version(&root, "dos.txt");
        // The model's `old` has LF, the file has CRLF.
        apply(&root, vec![update("dos.txt", &v, "a\nb\n", "a\nB\n")]);
        assert_eq!(
            fs::read(root.join("dos.txt")).unwrap(),
            b"a\r\nB\r\nc\r\n".to_vec()
        );
    }

    #[test]
    fn an_ambiguous_edit_is_refused_rather_than_guessed() {
        let root = workspace("ambiguous");
        fs::write(root.join("dup.rs"), "let x = 1;\nlet x = 1;\n").unwrap();
        let v = version(&root, "dup.rs");
        let err = fails(
            &root,
            vec![update("dup.rs", &v, "let x = 1;", "let x = 2;")],
        );
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("appears 2 times"), "{err}");
        assert_eq!(text(&root, "dup.rs"), "let x = 1;\nlet x = 1;\n");

        // replace_all is the way to say it was deliberate.
        let result = apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "dup.rs".into(),
                version: Some(v),
                edits: Some(vec![Edit {
                    old: "let x = 1;".into(),
                    new: "let x = 2;".into(),
                    replace_all: Some(true),
                }]),
                ..Default::default()
            }],
        );
        assert_eq!(result.files_changed, 1);
        assert_eq!(text(&root, "dup.rs"), "let x = 2;\nlet x = 2;\n");
    }

    #[test]
    fn add_delete_and_move_do_what_they_say() {
        let root = workspace("ops");
        let result = apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Add),
                path: "src/deep/new.rs".into(),
                content: Some("pub fn n() {}\n".into()),
                ..Default::default()
            }],
        );
        assert_eq!(result.files[0].after_bytes, 14);
        assert_eq!(text(&root, "src/deep/new.rs"), "pub fn n() {}\n");

        let err = fails(
            &root,
            vec![FilePatch {
                op: Some(Op::Add),
                path: "src/deep/new.rs".into(),
                content: Some("again\n".into()),
                ..Default::default()
            }],
        );
        assert!(err.message().contains("already exists"), "{err}");

        apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Move),
                path: "src/deep/new.rs".into(),
                to: Some("src/moved.rs".into()),
                version: Some(version(&root, "src/deep/new.rs")),
                ..Default::default()
            }],
        );
        assert!(!root.join("src/deep/new.rs").exists());
        assert_eq!(text(&root, "src/moved.rs"), "pub fn n() {}\n");

        apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Delete),
                path: "src/moved.rs".into(),
                version: Some(version(&root, "src/moved.rs")),
                ..Default::default()
            }],
        );
        assert!(!root.join("src/moved.rs").exists());
    }

    #[test]
    fn a_dry_run_reports_everything_and_writes_nothing() {
        let root = workspace("dry");
        let before = text(&root, "src/main.rs");
        let result = apply_patch(
            &root,
            None,
            &ApplyPatchArgs {
                files: vec![
                    update(
                        "src/main.rs",
                        &version(&root, "src/main.rs"),
                        "let x = 1;",
                        "let x = 7;",
                    ),
                    FilePatch {
                        op: Some(Op::Add),
                        path: "src/added.rs".into(),
                        content: Some("x\n".into()),
                        ..Default::default()
                    },
                ],
                dry_run: Some(true),
            },
        )
        .unwrap();
        assert!(result.dry_run);
        assert_eq!(result.files_changed, 2);
        assert!(result.files.iter().all(|f| f.version.is_none()));
        assert!(
            result
                .text
                .ends_with("[dry run: 2 files would change, nothing was written]")
        );
        assert_eq!(text(&root, "src/main.rs"), before);
        assert!(!root.join("src/added.rs").exists());

        // A dry run of a bad patch fails for the same reason a real one would.
        let err = apply_patch(
            &root,
            None,
            &ApplyPatchArgs {
                files: vec![update("src/main.rs", "0-0", "let x = 1;", "y")],
                dry_run: Some(true),
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::StaleEpoch);
    }

    #[test]
    fn the_same_file_twice_in_one_patch_is_refused() {
        let root = workspace("dup-path");
        let v = version(&root, "src/main.rs");
        let err = fails(
            &root,
            vec![
                update("src/main.rs", &v, "let x = 1;", "let x = 2;"),
                update("./src/main.rs", &v, "fn main", "fn other"),
            ],
        );
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("appears twice"), "{err}");
    }

    /// Swapping two things is the smallest case sequential application
    /// cannot do. Applied in order, the first edit makes a second copy of
    /// what the second edit is looking for, and the second edit is then
    /// refused as ambiguous -- for a patch that was perfectly well
    /// defined against the file as read.
    #[test]
    fn two_names_can_be_swapped_in_one_patch() {
        let root = workspace("swap");
        let result = apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/lib.rs".into(),
                version: Some(version(&root, "src/lib.rs")),
                edits: Some(vec![
                    Edit {
                        old: "pub fn a() {}".into(),
                        new: "pub fn b() {}".into(),
                        replace_all: None,
                    },
                    Edit {
                        old: "pub fn b() {}".into(),
                        new: "pub fn a() {}".into(),
                        replace_all: None,
                    },
                ]),
                ..Default::default()
            }],
        );
        assert_eq!(result.files[0].edits, 2);
        assert_eq!(text(&root, "src/lib.rs"), "pub fn b() {}\npub fn a() {}\n");
    }

    /// One edit's replacement containing another edit's `old` used to make
    /// the second one look ambiguous, because by the time it was tried
    /// there really were two copies. Resolving against the file as read
    /// sees one of each.
    #[test]
    fn an_edit_that_writes_another_edits_target_is_not_ambiguous() {
        let root = workspace("collide");
        let result = apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/lib.rs".into(),
                version: Some(version(&root, "src/lib.rs")),
                edits: Some(vec![
                    Edit {
                        old: "pub fn a()".into(),
                        new: "pub fn b()".into(),
                        replace_all: None,
                    },
                    Edit {
                        old: "pub fn b()".into(),
                        new: "pub fn c()".into(),
                        replace_all: None,
                    },
                ]),
                ..Default::default()
            }],
        );
        assert_eq!(result.files[0].edits, 2);
        assert_eq!(text(&root, "src/lib.rs"), "pub fn b() {}\npub fn c() {}\n");
    }

    /// Order-invariance must not become "we guessed". Two edits reaching
    /// for the same bytes fall through to ordered application, which
    /// refuses the second one for the reason it has always refused it.
    #[test]
    fn edits_that_overlap_are_still_refused() {
        let root = workspace("overlap");
        let err = fails(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/main.rs".into(),
                version: Some(version(&root, "src/main.rs")),
                edits: Some(vec![
                    Edit {
                        old: "let x = 1;".into(),
                        new: "let x = 2;".into(),
                        replace_all: None,
                    },
                    Edit {
                        old: "x = 1".into(),
                        new: "x = 3".into(),
                        replace_all: None,
                    },
                ]),
                ..Default::default()
            }],
        );
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("does not appear"), "{err}");
        // And the file is untouched, because nothing is written until every
        // file in the patch has been worked out.
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 1;\n}\n"
        );
    }

    /// "No match" on its own sends the model round the same loop. The
    /// documented ways these edits miss are whitespace ones, so when that
    /// is what happened the refusal says where the text is and shows the
    /// file's own bytes.
    #[test]
    fn a_whitespace_only_miss_says_where_the_text_really_is() {
        let root = workspace("nearmiss");
        let err = fails(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                // The file indents with four spaces; this uses a tab.
                "\tlet x = 1;",
                "\tlet x = 2;",
            )],
        );
        let message = err.message();
        assert!(message.contains("does not appear"), "{message}");
        assert!(
            message.contains("different whitespace"),
            "the refusal should name the cause: {message}"
        );
        assert!(
            message.contains("line 2"),
            "the refusal should locate it: {message}"
        );
        assert!(
            message.contains("    let x = 1;"),
            "the refusal should show the file's own bytes: {message}"
        );
    }

    /// When only the first line matches, say that much rather than
    /// nothing: it tells the model the block moved or its body changed,
    /// which is a different fix from re-reading the file.
    #[test]
    fn a_partial_miss_reports_the_line_that_did_match() {
        let root = workspace("partial");
        let err = fails(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                "fn main() {\n    let y = 9;\n}",
                "fn main() {}",
            )],
        );
        let message = err.message();
        assert!(message.contains("its first line is at line 1"), "{message}");
        assert!(message.contains("fn main() {"), "{message}");
    }

    /// `near_miss` runs on a *failed* edit, where nothing about the
    /// input has been vouched for: the file can be 16 MiB and `old` can
    /// be a megabyte of it. Unbounded, the search is
    /// candidates x wanted-lines, and there are two separate caps on it.
    /// Both need their own case, because either one alone makes the
    /// other's pathological input fast.
    ///
    /// This one is the `old`-length cap: an `old` far longer than
    /// MAX_NEAR_MISS_LINES skips the block search entirely.
    #[test]
    fn a_hopeless_edit_with_an_enormous_old_answers_quickly() {
        let text = "    x = 1\n".repeat(60_000);
        let mut old = "\tx = 1\n".repeat(3_000);
        old.push_str("\tnot in the file at all\n");
        let started = std::time::Instant::now();
        let answer = near_miss(&text, &old);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "near_miss took {:?}; it is bounded or it is a hang",
            started.elapsed()
        );
        // It still says something useful: the first line is everywhere.
        assert!(answer.contains("its first line is at line 1"), "{answer}");
    }

    /// And this is the many-candidates shape: an `old` in a file where
    /// every single line could start it. Without the cap this walks 200k
    /// candidates about 199 lines each, measured at 57.7 seconds.
    #[test]
    fn a_hopeless_edit_with_a_match_on_every_line_answers_quickly() {
        let text = "    x = 1\n".repeat(200_000);
        let mut old = "\tx = 1\n".repeat(199);
        old.push_str("\tnot in the file at all\n");
        let started = std::time::Instant::now();
        let answer = near_miss(&text, &old);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "near_miss took {:?}; it is bounded or it is a hang",
            started.elapsed()
        );
        assert!(answer.contains("its first line is at line 1"), "{answer}");
    }

    /// The answer goes into the model's context, so it is bounded like
    /// every other tool result rather than printing a whole function.
    #[test]
    fn a_long_whitespace_match_is_shown_head_and_tail() {
        let text: String = (0..200)
            .map(|n| format!("    line {n}\n"))
            .collect::<Vec<_>>()
            .join("");
        let old: String = (0..200)
            .map(|n| format!("\tline {n}\n"))
            .collect::<Vec<_>>()
            .join("");
        let answer = near_miss(&text, &old);
        assert!(answer.contains("different whitespace"), "{answer}");
        assert!(answer.contains("... 180 more lines ..."), "{answer}");
        assert!(answer.contains("    line 0"), "{answer}");
        assert!(answer.contains("    line 199"), "{answer}");
        let printed = answer.lines().count();
        assert!(printed < 30, "{printed} lines is not a bounded answer");
    }

    /// A miss with nothing like it in the file adds nothing -- an
    /// unhelpful guess is worse than a short answer.
    #[test]
    fn a_miss_with_no_near_match_says_only_that() {
        let root = workspace("nomatch");
        let err = fails(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                "something entirely absent",
                "x",
            )],
        );
        assert_eq!(
            err.message(),
            "src/main.rs edit 1: old does not appear in the file"
        );
    }

    #[test]
    fn edits_apply_in_order_and_say_so_when_a_later_one_misses() {
        let root = workspace("ordered");
        let v = version(&root, "src/lib.rs");
        let result = apply(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/lib.rs".into(),
                version: Some(v.clone()),
                edits: Some(vec![
                    Edit {
                        old: "pub fn a()".into(),
                        new: "pub fn first()".into(),
                        replace_all: None,
                    },
                    Edit {
                        old: "pub fn first()".into(),
                        new: "pub fn renamed()".into(),
                        replace_all: None,
                    },
                ]),
                ..Default::default()
            }],
        );
        assert_eq!(result.files[0].edits, 2);
        assert_eq!(
            text(&root, "src/lib.rs"),
            "pub fn renamed() {}\npub fn b() {}\n"
        );

        let err = fails(
            &root,
            vec![FilePatch {
                op: Some(Op::Update),
                path: "src/lib.rs".into(),
                version: Some(version(&root, "src/lib.rs")),
                edits: Some(vec![
                    Edit {
                        old: "pub fn b()".into(),
                        new: "pub fn c()".into(),
                        replace_all: None,
                    },
                    Edit {
                        old: "pub fn b()".into(),
                        new: "pub fn d()".into(),
                        replace_all: None,
                    },
                ]),
                ..Default::default()
            }],
        );
        assert!(err.message().contains("edits apply in order"), "{err}");
        assert_eq!(
            text(&root, "src/lib.rs"),
            "pub fn renamed() {}\npub fn b() {}\n"
        );
    }

    #[test]
    fn file_permissions_survive_a_patch() {
        let root = workspace("mode");
        let script = root.join("run.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&script, perms).unwrap();

        apply(
            &root,
            vec![update("run.sh", &version(&root, "run.sh"), "hi", "there")],
        );
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&script).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o755, "the script stopped being executable");
    }

    #[test]
    fn bounded_arguments_and_a_bounded_result() {
        let root = workspace("bounds");
        let err = fails(&root, vec![]);
        assert!(err.message().contains("nothing to apply"), "{err}");

        let many: Vec<FilePatch> = (0..MAX_FILES + 1)
            .map(|n| FilePatch {
                op: Some(Op::Add),
                path: format!("f{n}.txt"),
                content: Some("x".into()),
                ..Default::default()
            })
            .collect();
        let err = fails(&root, many);
        assert!(err.message().contains("the limit is"), "{err}");
        assert!(
            !root.join("f0.txt").exists(),
            "a bounded check must not write first"
        );

        // The result carries no file content, only what changed.
        let result = apply(
            &root,
            vec![update(
                "src/main.rs",
                &version(&root, "src/main.rs"),
                "let x = 1;",
                "let x = 5;",
            )],
        );
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("fn main"), "{json}");
        assert!(!json.contains("let x = 5"), "{json}");
        assert!(json.contains("\"version\""), "{json}");
        assert!(!json.contains(&root.display().to_string()), "{json}");
    }

    #[test]
    fn a_binary_file_is_not_patched() {
        let root = workspace("binary");
        fs::write(root.join("blob.bin"), b"\xff\xfe\x00\x01".as_slice()).unwrap();

        // read_file already refuses it, so a model can never get a version
        // for it in the first place. That is the first layer.
        let read = read::read_file(
            &root,
            &ReadFileArgs {
                path: "blob.bin".into(),
                ..Default::default()
            },
        );
        assert!(read.is_err(), "read_file should refuse a binary file");

        // The second layer, for a caller that got a version some other way.
        let real_version = crate::mcp::version_of(&fs::metadata(root.join("blob.bin")).unwrap());
        let err = fails(&root, vec![update("blob.bin", &real_version, "a", "b")]);
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("not valid UTF-8"), "{err}");
        assert_eq!(
            fs::read(root.join("blob.bin")).unwrap(),
            b"\xff\xfe\x00\x01".to_vec()
        );
    }

    /// chmod 000/555 does nothing for root, and then these tests would
    /// assert on a failure that never happened.
    fn cannot_write_here(dir: &Path) -> bool {
        let probe = dir.join(".ccnm-probe");
        match fs::write(&probe, "x") {
            Ok(()) => {
                let _ = fs::remove_file(&probe);
                false
            }
            Err(_) => true,
        }
    }

    fn lock(dir: &Path) {
        let mut perms = fs::metadata(dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        fs::set_permissions(dir, perms).unwrap();
    }

    fn unlock(dir: &Path) {
        let mut perms = fs::metadata(dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(dir, perms).unwrap();
    }

    #[test]
    fn a_staging_failure_cleans_up_what_was_already_staged() {
        // The plan phase cannot catch a full disk or a directory that
        // cannot be written; staging is where those land, after earlier
        // files have already put temp files on disk.
        let root = workspace("stage-fail");
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/b.rs"), "pub fn b() {}\n").unwrap();
        let locked_version = version(&root, "locked/b.rs");
        let main_version = version(&root, "src/main.rs");
        let main_before = text(&root, "src/main.rs");
        lock(&root.join("locked"));
        if !cannot_write_here(&root.join("locked")) {
            unlock(&root.join("locked"));
            return;
        }

        let err = fails(
            &root,
            vec![
                update("src/main.rs", &main_version, "let x = 1;", "let x = 8;"),
                update("locked/b.rs", &locked_version, "pub fn b()", "pub fn c()"),
            ],
        );
        assert!(err.message().contains("locked/b.rs"), "{err}");
        assert_eq!(
            text(&root, "src/main.rs"),
            main_before,
            "the first file was committed"
        );
        let leftovers: Vec<String> = fs::read_dir(root.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".ccnm-"))
            .collect();
        unlock(&root.join("locked"));
        assert!(
            leftovers.is_empty(),
            "staging temps survived: {leftovers:?}"
        );
    }

    /// The journal has to exist *while* the renames happen -- that is the
    /// only moment it is for. Every other test can only see the world
    /// after the call, when it is correctly gone, so this one holds it
    /// open and looks.
    #[test]
    fn the_journal_describes_the_commit_while_it_is_happening() {
        let root = workspace("journal-content");
        let journals = root.join("../journal-content-state");
        let _ = fs::remove_dir_all(&journals);
        let plan = plan(
            &root,
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 4;",
                )],
                dry_run: None,
            },
        )
        .unwrap();
        let staged = stage(plan).unwrap();
        let journal = Journal::open(&journals, &root, &staged).unwrap();

        let written: Vec<PathBuf> = fs::read_dir(&journals)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        let record: JournalFile = serde_json::from_slice(&fs::read(&written[0]).unwrap()).unwrap();
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.files.len(), 1);
        assert_eq!(record.files[0].rel, "src/main.rs");
        assert_eq!(record.files[0].abs, root.join("src/main.rs"));
        // The backup is the whole reason a person can recover by hand.
        let backup = record.files[0].backup.clone().expect("a backup path");
        assert!(backup.exists(), "{}", backup.display());

        // Dropping it is what removes it, which is why a killed process
        // leaves it behind.
        drop(journal);
        let after: Vec<PathBuf> = fs::read_dir(&journals)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(after.is_empty(), "{after:?}");
    }

    /// When a rollback fails the workspace really is inconsistent, so the
    /// record has to outlive this process on purpose.
    #[test]
    fn an_abandoned_journal_survives_being_dropped() {
        let root = workspace("journal-keep");
        let journals = root.join("../journal-keep-state");
        let _ = fs::remove_dir_all(&journals);
        let plan = plan(
            &root,
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 4;",
                )],
                dry_run: None,
            },
        )
        .unwrap();
        let staged = stage(plan).unwrap();
        let journal = Journal::open(&journals, &root, &staged).unwrap();
        let path = journal.path.clone();
        journal.abandon();
        drop(journal);
        assert!(
            path.exists(),
            "an abandoned journal must stay: {}",
            path.display()
        );
    }

    /// A successful patch leaves no journal behind. If it did, the very
    /// next patch would refuse for no reason.
    #[test]
    fn a_patch_that_worked_leaves_no_journal() {
        let root = workspace("journal-clean");
        let journals = root.join("../journal-clean-state");
        let _ = fs::remove_dir_all(&journals);
        apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 7;",
                )],
                dry_run: None,
            },
        )
        .unwrap();
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 7;\n}\n"
        );
        let left: Vec<PathBuf> = fs::read_dir(&journals)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    /// The hole `Drop` cannot cover: a process killed between renames.
    /// There is no way to kill this test's own process half way through a
    /// commit, so the journal a killed process would have left is written
    /// by hand -- which is the whole point of it being a plain file with
    /// nothing but facts in it.
    #[test]
    fn an_abandoned_journal_stops_the_next_patch_and_names_the_files() {
        let root = workspace("journal-abandoned");
        let journals = root.join("../journal-abandoned-state");
        let _ = fs::remove_dir_all(&journals);
        fs::create_dir_all(&journals).unwrap();
        let record = serde_json::json!({
            "pid": 4321,
            "root": root.clone(),
            "files": [
                {
                    "op": "update",
                    "rel": "src/main.rs",
                    "abs": root.join("src/main.rs"),
                    "backup": root.join("src/.ccnm-abc-main.rs"),
                },
                {
                    "op": "update",
                    "rel": "src/lib.rs",
                    "abs": root.join("src/lib.rs"),
                },
            ],
        });
        let journal = journals.join("4321-deadbeefcafe.json");
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        // Older than a commit could possibly take.
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        let file = fs::File::options().write(true).open(&journal).unwrap();
        file.set_modified(long_ago).unwrap();
        drop(file);

        let err = apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 9;",
                )],
                dry_run: None,
            },
        )
        .expect_err("a patch must not plan on top of an interrupted one");
        let message = err.message();
        assert!(message.contains("interrupted"), "{message}");
        // Every file it was renaming, because which ones landed is the
        // only question worth answering and only a person can answer it.
        assert!(message.contains("src/main.rs"), "{message}");
        assert!(message.contains("src/lib.rs"), "{message}");
        // Relative, like every other path the model is shown: the
        // workspace machine's directory layout is not its business and
        // travels back to Anthropic in the transcript.
        assert!(message.contains("src/.ccnm-abc-main.rs"), "{message}");
        assert!(
            !message.contains(&root.join("src/.ccnm-abc-main.rs").display().to_string()),
            "the backup path leaked absolute: {message}"
        );
        assert!(message.contains("git status"), "{message}");
        assert!(message.contains("4321"), "{message}");
        assert!(
            message.contains(&journal.display().to_string()),
            "the way out has to be in the message: {message}"
        );
        // And it really did not write.
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 1;\n}\n"
        );

        // Cleared by hand, patching works again.
        fs::remove_file(&journal).unwrap();
        apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 9;",
                )],
                dry_run: None,
            },
        )
        .unwrap();
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 9;\n}\n"
        );
    }

    /// Two workspaces share one state directory, so a journal has to say
    /// which project it belongs to. One project's interrupted commit
    /// tells you nothing about another project's files, and blocking
    /// every workspace because one was interrupted would be a bug that
    /// only shows up on the day somebody has two projects.
    #[test]
    fn an_interruption_in_one_workspace_does_not_block_another() {
        let root = workspace("journal-mine");
        let journals = root.join("../journal-shared-state");
        let _ = fs::remove_dir_all(&journals);
        fs::create_dir_all(&journals).unwrap();
        let elsewhere = journals.join("777-otherproject.json");
        fs::write(
            &elsewhere,
            serde_json::to_vec(&serde_json::json!({
                "pid": 777,
                "root": "/Users/somebody/a-different-project",
                "files": [{
                    "op": "update",
                    "rel": "src/other.rs",
                    "abs": "/Users/somebody/a-different-project/src/other.rs",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let file = fs::File::options().write(true).open(&elsewhere).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        drop(file);

        apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 3;",
                )],
                dry_run: None,
            },
        )
        .expect("another project's interruption must not block this one");
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 3;\n}\n"
        );
        // And it is still there for the workspace it does belong to.
        assert!(elsewhere.exists());
    }

    /// A journal that cannot be parsed is still evidence that a patch was
    /// interrupted. Skipping it would be the one thing this whole
    /// mechanism exists to prevent: an inconsistency nobody mentions.
    #[test]
    fn a_journal_that_cannot_be_read_is_still_reported() {
        let root = workspace("journal-corrupt");
        let journals = root.join("../journal-corrupt-state");
        let _ = fs::remove_dir_all(&journals);
        fs::create_dir_all(&journals).unwrap();
        let broken = journals.join("55-truncated.json");
        fs::write(&broken, b"{\"pid\": 55, \"files\": [{\"op\"").unwrap();
        let file = fs::File::options().write(true).open(&broken).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        drop(file);

        let err = apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 3;",
                )],
                dry_run: None,
            },
        )
        .expect_err("an unreadable journal must not be skipped");
        assert!(err.message().contains("cannot be read"), "{err}");
        assert!(
            err.message().contains(&broken.display().to_string()),
            "{err}"
        );
        assert_eq!(
            text(&root, "src/main.rs"),
            "fn main() {\n    let x = 1;\n}\n"
        );
    }

    /// A journal young enough to be a commit that is still running is not
    /// an interruption. Two patches at once must not accuse each other.
    #[test]
    fn a_journal_from_a_commit_still_running_is_left_alone() {
        let root = workspace("journal-fresh");
        let journals = root.join("../journal-fresh-state");
        let _ = fs::remove_dir_all(&journals);
        fs::create_dir_all(&journals).unwrap();
        fs::write(
            journals.join("999-freshjournal.json"),
            serde_json::to_vec(&serde_json::json!({
                "pid": 999, "root": root.clone(), "files": [],
            }))
            .unwrap(),
        )
        .unwrap();
        apply_patch(
            &root,
            Some(&journals),
            &ApplyPatchArgs {
                files: vec![update(
                    "src/main.rs",
                    &version(&root, "src/main.rs"),
                    "let x = 1;",
                    "let x = 5;",
                )],
                dry_run: None,
            },
        )
        .expect("a fresh journal is a patch in progress, not an interruption");
    }

    #[test]
    fn a_commit_failure_rolls_the_earlier_files_back() {
        // A move stages nothing -- no new content, no backup -- so a move
        // into a directory that cannot be written is the one way to get
        // past staging and fail during the commit.
        let root = workspace("commit-fail");
        fs::create_dir(root.join("locked")).unwrap();
        let main_before = text(&root, "src/main.rs");
        let main_version = version(&root, "src/main.rs");
        let lib_version = version(&root, "src/lib.rs");
        lock(&root.join("locked"));
        if !cannot_write_here(&root.join("locked")) {
            unlock(&root.join("locked"));
            return;
        }

        let err = fails(
            &root,
            vec![
                update("src/main.rs", &main_version, "let x = 1;", "let x = 9;"),
                FilePatch {
                    op: Some(Op::Move),
                    path: "src/lib.rs".into(),
                    to: Some("locked/lib.rs".into()),
                    version: Some(lib_version),
                    ..Default::default()
                },
            ],
        );
        unlock(&root.join("locked"));
        assert!(err.message().contains("rolled back"), "{err}");
        assert!(
            !err.message().contains("PARTIALLY CHANGED"),
            "rollback should have succeeded: {err}"
        );
        // The update that had already committed is back to what it was.
        assert_eq!(text(&root, "src/main.rs"), main_before);
        assert!(root.join("src/lib.rs").exists());
        let leftovers: Vec<String> = fs::read_dir(root.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".ccnm-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn missing_dirs_lists_what_has_to_be_created() {
        let root = workspace("dirs");
        assert!(missing_dirs(&root.join("src/main.rs")).is_empty());
        let deep = missing_dirs(&root.join("a/b/c.rs"));
        assert_eq!(deep, vec![root.join("a"), root.join("a/b")]);
    }

    #[test]
    fn a_failed_add_removes_the_directories_it_created() {
        let root = workspace("dir-rollback");
        // The second file fails to plan, so the first never stages.
        let err = fails(
            &root,
            vec![
                FilePatch {
                    op: Some(Op::Add),
                    path: "brand/new/tree/a.rs".into(),
                    content: Some("x\n".into()),
                    ..Default::default()
                },
                update("src/main.rs", "0-0", "let x = 1;", "y"),
            ],
        );
        assert_eq!(err.code(), ErrorCode::StaleEpoch);
        assert!(
            !root.join("brand").exists(),
            "a directory survived a failed patch"
        );
    }
}

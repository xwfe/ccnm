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

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// Replacements, for `update`. Applied in order.
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
pub fn apply_patch(root: &Path, args: &ApplyPatchArgs) -> Result<PatchResult> {
    let dry_run = args.dry_run.unwrap_or(false);
    let plan = plan(root, args)?;
    if dry_run {
        return Ok(report(&plan, true, Vec::new()));
    }
    let staged = stage(plan)?;
    let versions = commit(staged.as_slice())?;
    let plan: Vec<Planned> = staged.into_iter().map(|s| s.planned).collect();
    Ok(report(&plan, false, versions))
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

/// Apply the edits in order, each to the result of the last.
fn apply_edits(text: &str, edits: &[Edit], rel: &str) -> Result<(String, u32)> {
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
                "{rel} edit {}: old does not appear in the file{}",
                index + 1,
                if index > 0 {
                    "; note that edits apply in order, so an earlier edit may have changed it"
                } else {
                    ""
                }
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
        std::fs::create_dir(dir).map_err(|e| {
            Error::invalid_args(format!("cannot create a directory for {}", planned.rel))
                .with_source(e)
        })?;
        created_dirs.push(dir.clone());
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
    Ok(dir.join(format!(".ccnm-{}-{name}", &unique[..12])))
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

/// Throw away staged work that was never committed.
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
fn commit(staged: &[Staged]) -> Result<Vec<Option<String>>> {
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
                    Err(failed) => Error::internal(format!(
                        "{}: {}. Rolling back failed too: {failed}. The workspace is PARTIALLY CHANGED; check it before doing anything else.",
                        one.planned.rel,
                        e.message()
                    )),
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

fn report(plan: &[Planned], dry_run: bool, versions: Vec<Option<String>>) -> PatchResult {
    let files: Vec<FileChange> = plan
        .iter()
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

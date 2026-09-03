//! Workspace path policy: the one place a path from the model becomes a
//! path on this disk.
//!
//! Design doc section 17. Every file tool resolves through here, so the
//! rules exist once instead of once per tool, and one test module covers
//! all of them.
//!
//! The rules are deliberately the opposite of coding-tools-mcp's read
//! side, which lets absolute paths and `..` reach outside the workspace on
//! purpose and has five tests locking that in
//! (`docs/research/coding-tools-mcp.md`, section b). That service is
//! designed to be tunnelled to a chat client and accepts the trade; ccnm's
//! whole point is that the home machine's secrets never reach the control
//! plane, so its reader gets the strict rules its writer gets.
//!
//! What each rejection means to the caller:
//!
//! ```text
//! CCNM_E_POLICY        the path is well formed but points somewhere you
//!                      may not go — absolute, `..`, `~`, symlink escape.
//!                      Do not retry; there is no argument that helps.
//! CCNM_E_INVALID_ARGS  the string is not a usable workspace-relative path,
//!                      or nothing is there. Fix it and call again.
//! ```

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

/// A client path that survived the policy, paired with where it really is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePath {
    rel: String,
    abs: PathBuf,
}

impl WorkspacePath {
    /// The normalized workspace-relative path, always forward-slashed.
    /// This is the only form that goes back to the model.
    pub fn rel(&self) -> &str {
        &self.rel
    }

    /// The canonical path on this host. Never send it anywhere.
    pub fn abs(&self) -> &Path {
        &self.abs
    }
}

/// Resolve `raw` for reading under the canonical workspace `root`.
///
/// The target must exist; a missing path is `CCNM_E_INVALID_ARGS`, since
/// from the model's side "you named something that isn't there" and "you
/// typed the wrong line number" call for the same reaction. The file type
/// is the caller's business: a directory resolves fine here and
/// `read_file` is what rejects it, because `list_files` will want the
/// same resolution.
///
/// Containment is decided before existence. If someone probes
/// `secrets/../../etc/shadow`, the answer is `CCNM_E_POLICY` whether or
/// not that file exists, so the error text never says which paths outside
/// the workspace are real.
pub fn resolve_read(root: &Path, raw: &str) -> Result<WorkspacePath> {
    let rel = normalize(raw)?;
    let joined = root.join(&rel);

    match std::fs::canonicalize(&joined) {
        Ok(abs) => {
            contained(root, &abs, &rel)?;
            Ok(WorkspacePath { rel, abs })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The path itself does not resolve. Before saying "not found",
            // make sure the deepest part that does resolve is still inside:
            // a symlink `esc -> /etc` whose target is missing must read as
            // a policy error, not as a hint about /etc.
            if let Some(parent) = joined.parent()
                && let Ok(canonical_parent) = std::fs::canonicalize(parent)
            {
                contained(root, &canonical_parent, &rel)?;
            }
            // A symlink that exists but whose target does not is worth
            // naming: "no such file" sends the reader looking for a typo.
            let dangling = std::fs::symlink_metadata(&joined)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            Err(Error::invalid_args(if dangling {
                format!("{rel} is a symlink whose target does not exist")
            } else {
                format!("{rel} does not exist in this workspace")
            }))
        }
        Err(e) => Err(Error::invalid_args(format!("cannot resolve {rel}")).with_source(e)),
    }
}

/// A path `apply_patch` is allowed to create, change or remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTarget {
    rel: String,
    abs: PathBuf,
    exists: bool,
}

impl WriteTarget {
    /// The normalized workspace-relative path.
    pub fn rel(&self) -> &str {
        &self.rel
    }

    /// Where to write. Not canonical when the file does not exist yet: its
    /// parent chain is, which is what containment is decided on.
    pub fn abs(&self) -> &Path {
        &self.abs
    }

    /// Whether something is there now.
    pub fn exists(&self) -> bool {
        self.exists
    }
}

/// Resolve `raw` for writing. Never weaker than [`resolve_read`], and
/// stricter in three ways that only matter once something can be changed:
///
/// ```text
/// .git is refused           design doc section 17. Reading it is allowed
///                           today; writing it corrupts a repository in ways
///                           no file tool should be able to
/// symlinks are refused      not just ones that escape. Writing "through" a
///                           link means the commit rename would replace the
///                           link with a regular file, quietly detaching it
/// the parent must be inside canonicalized and checked, because the file
///                           itself may not exist yet and so cannot be
/// ```
///
/// Non-existence is not an error here: `add` needs a path with nothing at
/// it. The caller decides what `exists` should have been.
pub fn resolve_write(root: &Path, raw: &str) -> Result<WriteTarget> {
    let rel = normalize(raw)?;
    if rel.split('/').any(|segment| segment == ".git") {
        return Err(Error::policy(format!(
            "{rel} is inside .git; ccnm's file tools never write to a git database"
        )));
    }
    let joined = root.join(&rel);

    // The deepest existing ancestor decides containment. Canonicalizing it
    // resolves every symlink on the way in, so a `src` that points at /etc
    // fails here rather than at the write.
    let mut ancestor = joined.as_path();
    let canonical_ancestor = loop {
        match std::fs::canonicalize(ancestor) {
            Ok(canonical) => break canonical,
            Err(_) => match ancestor.parent() {
                Some(parent) => ancestor = parent,
                None => {
                    return Err(Error::invalid_args(format!("cannot resolve {rel}")));
                }
            },
        }
    };
    contained(root, &canonical_ancestor, &rel)?;

    // A symlink at the target itself is refused whatever it points at.
    let exists = match std::fs::symlink_metadata(&joined) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(Error::policy(format!(
                    "{rel} is a symlink; ccnm will not write through one"
                )));
            }
            true
        }
        Err(_) => false,
    };
    // Rebuild the absolute path from the canonical ancestor so the parts
    // that do exist are canonical and the parts that do not are appended.
    // `join("")` would append a trailing separator, and a path ending in
    // `/` is a directory as far as the OS is concerned: every stat of an
    // existing file would come back ENOTDIR.
    let suffix = joined.strip_prefix(ancestor).unwrap_or(Path::new(""));
    let abs = if suffix.as_os_str().is_empty() {
        canonical_ancestor
    } else {
        canonical_ancestor.join(suffix)
    };
    Ok(WriteTarget { rel, abs, exists })
}

/// Syntactic rules, applied to the string before it ever touches the disk.
fn normalize(raw: &str) -> Result<String> {
    if raw.contains('\0') {
        return Err(Error::invalid_args("path contains a NUL byte"));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid_args(
            "path is empty; name a file relative to the workspace root, e.g. src/main.rs",
        ));
    }
    if trimmed.starts_with('~') {
        return Err(Error::policy(format!(
            "{trimmed} is outside the workspace: ~ is not expanded and the home directory is not readable through ccnm"
        )));
    }
    // `\` is a legal character in a macOS filename, so rejecting it costs
    // us files nobody has. Not rejecting it costs a confusing "does not
    // exist" every time a model writes a Windows-style path.
    if let Some(rest) = trimmed.strip_prefix('\\') {
        return Err(Error::policy(format!(
            "\\{rest} is an absolute path; ccnm only reads inside the workspace"
        )));
    }
    if is_windows_absolute(trimmed) {
        return Err(Error::policy(format!(
            "{trimmed} is an absolute path; ccnm only reads inside the workspace"
        )));
    }
    if trimmed.contains('\\') {
        return Err(Error::invalid_args(format!(
            "paths use forward slashes; got {trimmed}"
        )));
    }
    if Path::new(trimmed).is_absolute() {
        return Err(Error::policy(format!(
            "{trimmed} is an absolute path; ccnm only reads inside the workspace"
        )));
    }

    let mut parts: Vec<&str> = Vec::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| Error::invalid_args("path is not valid UTF-8"))?,
            ),
            Component::CurDir => {}
            // Even a `..` that would cancel out is refused. Resolving it
            // ourselves and resolving it through symlinks give different
            // answers, and the difference is exactly where escapes live.
            Component::ParentDir => {
                return Err(Error::policy(format!(
                    "{trimmed} contains `..`; ccnm only reads inside the workspace"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::policy(format!(
                    "{trimmed} is an absolute path; ccnm only reads inside the workspace"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(Error::invalid_args(format!(
            "{trimmed} names the workspace root, not a path inside it"
        )));
    }
    Ok(parts.join("/"))
}

/// `C:\x`, `c:/x`. Meaningless on this host, but a model that thinks it is
/// on Windows should be told it escaped rather than that the file is
/// missing.
fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// The resolved path must still be under the canonical root. This is the
/// check that catches symlinks, which no amount of string handling can.
fn contained(root: &Path, resolved: &Path, rel: &str) -> Result<()> {
    if resolved.starts_with(root) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::Policy,
        format!("{rel} resolves outside the workspace (symlink escape)"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// A workspace with one file, one subdirectory, and the symlinks the
    /// policy exists for.
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-path-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = dir.join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("outside.txt"), "secret\n").unwrap();
        symlink(dir.join("outside.txt"), root.join("escape.txt")).unwrap();
        symlink("src/main.rs", root.join("inside.txt")).unwrap();
        symlink(dir.join("nothing.txt"), root.join("dangling.txt")).unwrap();
        symlink(&dir, root.join("up")).unwrap();
        fs::canonicalize(&root).unwrap()
    }

    fn code(root: &Path, raw: &str) -> ErrorCode {
        match resolve_read(root, raw) {
            Err(e) => e.code(),
            Ok(p) => panic!("{raw} should have been refused, got {}", p.rel()),
        }
    }

    #[test]
    fn plain_relative_paths_resolve() {
        let root = fixture("ok");
        let p = resolve_read(&root, "src/main.rs").unwrap();
        assert_eq!(p.rel(), "src/main.rs");
        assert_eq!(p.abs(), root.join("src/main.rs"));
    }

    #[test]
    fn redundant_syntax_is_normalized_not_refused() {
        let root = fixture("norm");
        for raw in [
            "./src/main.rs",
            "src//main.rs",
            "  src/main.rs  ",
            "src/./main.rs",
        ] {
            let p = resolve_read(&root, raw).unwrap();
            assert_eq!(p.rel(), "src/main.rs", "{raw}");
        }
    }

    #[test]
    fn a_directory_resolves_and_type_checking_is_the_callers_job() {
        let root = fixture("dir");
        let p = resolve_read(&root, "src").unwrap();
        assert!(p.abs().is_dir());
    }

    #[test]
    fn absolute_paths_are_policy_errors() {
        let root = fixture("abs");
        for raw in [
            "/etc/passwd",
            "/",
            "\\\\server\\share",
            "C:\\Windows\\win.ini",
            "c:/Windows/win.ini",
        ] {
            assert_eq!(code(&root, raw), ErrorCode::Policy, "{raw}");
        }
    }

    #[test]
    fn parent_components_are_policy_errors_even_when_harmless() {
        let root = fixture("dotdot");
        for raw in ["../outside.txt", "../../etc/passwd", "src/../src/main.rs"] {
            assert_eq!(code(&root, raw), ErrorCode::Policy, "{raw}");
        }
    }

    #[test]
    fn tilde_is_refused_before_it_looks_like_a_missing_directory() {
        let root = fixture("tilde");
        let err = resolve_read(&root, "~/.ssh/id_ed25519").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);
        assert!(err.message().contains("not expanded"), "{err}");
    }

    #[test]
    fn unusable_strings_are_invalid_args() {
        let root = fixture("bad");
        for raw in ["", "   ", "src\0/main.rs", "src\\main.rs", ".", "./"] {
            assert_eq!(code(&root, raw), ErrorCode::InvalidArgs, "{raw:?}");
        }
    }

    #[test]
    fn a_symlink_out_of_the_workspace_is_a_policy_error() {
        let root = fixture("escape");
        let err = resolve_read(&root, "escape.txt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);
        assert!(err.message().contains("symlink escape"), "{err}");
        // And so is walking through a symlinked directory.
        assert_eq!(code(&root, "up/outside.txt"), ErrorCode::Policy);
    }

    #[test]
    fn a_symlink_inside_the_workspace_is_followed() {
        let root = fixture("inside");
        let p = resolve_read(&root, "inside.txt").unwrap();
        assert_eq!(p.rel(), "inside.txt");
        assert_eq!(p.abs(), root.join("src/main.rs"));
    }

    #[test]
    fn a_dangling_symlink_says_so() {
        let root = fixture("dangling");
        let err = resolve_read(&root, "dangling.txt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("target does not exist"), "{err}");
    }

    #[test]
    fn a_missing_path_never_reveals_what_exists_outside() {
        let root = fixture("missing");
        let err = resolve_read(&root, "src/nope.rs").unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(err.message().contains("src/nope.rs"), "{err}");
        assert!(
            !err.message().contains(&root.display().to_string()),
            "{err}"
        );
        // A missing target under an escaping symlink is a policy error, so
        // the reply cannot be used to test for files outside.
        assert_eq!(code(&root, "up/does-not-exist"), ErrorCode::Policy);
    }

    #[test]
    fn an_absurdly_long_path_errors_instead_of_panicking() {
        let root = fixture("long");
        let long = "a".repeat(4096);
        assert_eq!(code(&root, &long), ErrorCode::InvalidArgs);
        let deep = vec!["a"; 2000].join("/");
        assert_eq!(code(&root, &deep), ErrorCode::InvalidArgs);
    }

    #[test]
    fn writing_accepts_a_path_that_does_not_exist_yet() {
        let root = fixture("write-new");
        let target = resolve_write(&root, "src/new.rs").unwrap();
        assert_eq!(target.rel(), "src/new.rs");
        assert!(!target.exists());
        assert_eq!(target.abs(), root.join("src/new.rs"));

        let existing = resolve_write(&root, "src/main.rs").unwrap();
        assert!(existing.exists());

        // Several levels of missing directory still resolve: the deepest
        // ancestor that does exist is what containment is decided on.
        let deep = resolve_write(&root, "a/b/c/d.rs").unwrap();
        assert!(!deep.exists());
        assert_eq!(deep.abs(), root.join("a/b/c/d.rs"));
    }

    #[test]
    fn writing_is_never_weaker_than_reading() {
        let root = fixture("write-policy");
        // Everything the reader refuses, the writer refuses with the same
        // code. If this ever diverges, the write side is the dangerous one.
        for raw in [
            "/etc/passwd",
            "../outside.txt",
            "~/.ssh/id_ed25519",
            "C:\\x",
            "src/../src/main.rs",
        ] {
            let read = resolve_read(&root, raw).unwrap_err().code();
            let write = resolve_write(&root, raw).unwrap_err().code();
            assert_eq!(read, write, "{raw}");
        }
        for raw in ["", "   ", "src\0/x", "src\\x", "."] {
            assert_eq!(
                resolve_write(&root, raw).unwrap_err().code(),
                ErrorCode::InvalidArgs,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn writing_refuses_the_git_database() {
        let root = fixture("write-git");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        for raw in [".git/config", ".git/objects/x", ".git"] {
            let err = resolve_write(&root, raw).unwrap_err();
            assert_eq!(err.code(), ErrorCode::Policy, "{raw}");
            assert!(err.message().contains(".git"), "{err}");
        }
        // A file merely called gitignore is not the git database.
        assert!(resolve_write(&root, ".gitignore").is_ok());
    }

    #[test]
    fn writing_refuses_any_symlink_not_only_escaping_ones() {
        let root = fixture("write-symlink");
        // Escaping: the same policy error reading gives.
        let err = resolve_write(&root, "escape.txt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);

        // Pointing inside: reading follows it, writing still refuses. The
        // commit is a rename, which would replace the link with a regular
        // file and quietly detach it from its target.
        assert!(resolve_read(&root, "inside.txt").is_ok());
        let err = resolve_write(&root, "inside.txt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);
        assert!(err.message().contains("symlink"), "{err}");

        // And a new file under a symlinked directory that leaves the
        // workspace is refused even though nothing is there yet.
        let err = resolve_write(&root, "up/planted.txt").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);
    }

    #[test]
    fn dot_git_is_readable_for_now() {
        // Design doc section 17 forbids *modifying* .git through the file
        // tools, not reading it. Kept as a test so that if the rule ever
        // tightens, the change is deliberate and this test is what fails.
        let root = fixture("git");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
        assert_eq!(
            resolve_read(&root, ".git/config").unwrap().rel(),
            ".git/config"
        );
    }
}

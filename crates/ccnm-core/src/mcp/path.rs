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

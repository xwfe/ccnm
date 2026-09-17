//! A scratch directory for one test, removed when the test ends.
//!
//! Test fixtures here name their directories after the pid or a session id,
//! so a directory left behind is never reused or overwritten by the next run:
//! every run of every test binary added a fresh batch to `$TMPDIR`. One full
//! `cargo test --workspace` left 421 of them; looping a test binary to chase
//! a flaky test -- routine in this repository -- once filled the disk.
//!
//! The workspace forbids `unsafe` and has no libc, so there is no `atexit`.
//! `Drop` is the only hook there is, which is why this is a guard and not a
//! registry swept at exit.
//!
//! The guard only deletes. What the path is -- under `$TMPDIR` or `/tmp`,
//! canonicalized or not, named how -- stays with each fixture, because tests
//! depend on it: ssh `ControlPath` and Unix socket paths have a 103-byte
//! limit, and Codex refuses to set up its sandbox under a temporary
//! directory.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// Removes a directory tree (or a single file, such as a socket) on drop.
///
/// A test that is failing keeps its directory and says where it is: the
/// files are usually the fastest way to see what went wrong.
#[derive(Debug)]
pub struct TestDir {
    path: PathBuf,
    also: Vec<PathBuf>,
}

impl TestDir {
    /// Takes over `path`. It does not have to exist yet, and it is not
    /// created here.
    pub fn adopt(path: impl Into<PathBuf>) -> Self {
        TestDir {
            path: path.into(),
            also: Vec::new(),
        }
    }

    /// A second path that belongs to the same test, for fixtures that keep
    /// their sockets under `/tmp` and everything else under `$TMPDIR`.
    #[must_use]
    pub fn also(mut self, path: impl Into<PathBuf>) -> Self {
        self.also.push(path.into());
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for TestDir {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("test failed, keeping {}", self.path.display());
            return;
        }
        for path in std::iter::once(&self.path).chain(&self.also) {
            // Missing is fine: some tests delete their own directory, and
            // `also` paths are often never created.
            if std::fs::remove_dir_all(path).is_err() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ccnm-testdir-{}-{name}", std::process::id()))
    }

    #[test]
    fn the_tree_and_the_extra_path_are_gone_after_drop() {
        let dir = scratch("tree");
        let socket = scratch("tree.sock");
        {
            let guard = TestDir::adopt(&dir).also(&socket);
            std::fs::create_dir_all(guard.join("a/b")).unwrap();
            std::fs::write(guard.join("a/b/file"), "x").unwrap();
            std::fs::write(&socket, "").unwrap();
        }
        assert!(!dir.exists());
        assert!(!socket.exists());
    }

    #[test]
    fn a_path_that_was_never_created_is_not_an_error() {
        drop(TestDir::adopt(scratch("never")));
    }

    #[test]
    fn a_failing_test_keeps_its_directory() {
        let dir = scratch("kept");
        let moved = dir.clone();
        let failed = std::thread::spawn(move || {
            let guard = TestDir::adopt(&moved);
            std::fs::create_dir_all(guard.path()).unwrap();
            panic!("the test body failing");
        })
        .join();
        assert!(failed.is_err());
        assert!(dir.exists(), "the directory is the evidence");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

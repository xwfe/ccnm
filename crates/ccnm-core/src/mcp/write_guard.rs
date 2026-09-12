//! Runtime-owned single-writer guard for a canonical workspace resource.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::process::{Cmd, ProcessRunner};

pub struct WriteGuard {
    file: File,
}

const RELEASED: &str = "released\n";

impl WriteGuard {
    pub fn acquire(
        state: &Path,
        root: &Path,
        workspace: &str,
        session: &str,
        config: Option<&Config>,
        runner: &dyn ProcessRunner,
    ) -> Result<Self> {
        if crate::paths::safe_name(workspace, "") != workspace
            || crate::paths::safe_name(session, "") != session
        {
            return Err(Error::invalid_args(
                "workspace and session must be valid ccnm identifiers",
            ));
        }
        reject_overlapping_roots(root, workspace, config)?;
        let resource = resource_root(root, runner);
        let locks = state.join("write-guards");
        std::fs::create_dir_all(&locks)?;
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o700))?;
        let path = locks.join(format!(
            "{:016x}.lock",
            fnv1a(resource.as_os_str().as_bytes())
        ));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::policy(
                    // Not "another managed session": this guard is shared with
                    // the external MCP entry, and on the real-host round it was
                    // an external coding session holding it. Naming one entry
                    // sends whoever reads this looking for a session that does
                    // not exist. The contract fixture never had the word.
                    //
                    // The lines after the first one point at the owner,
                    // because the Host shows none of this: Claude Code
                    // renders a closed stdio server as CONNECTION_CLOSED and
                    // drops the stderr that says why. Whoever does see this
                    // reached it through `ccnm doctor`, and the next thing
                    // they need is where to look.
                    "workspace write guard is busy; another session still owns this working tree\n\
                     who holds it, on the Runtime Node: the `held <session> <workspace>` file in\n\
                     ${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/\n\
                     `ccnm status` alone does not prove nobody is using it: a --print run holds\n\
                     this guard and never appears there",
                ));
            }
            Err(_) => {
                return Err(Error::policy(
                    "workspace write guard state is unknown; refusing to transfer write authority",
                ));
            }
        }
        let mut state_text = String::new();
        file.read_to_string(&mut state_text)?;
        if state_text.starts_with("held ") {
            let _ = file.unlock();
            // The first line keeps its wording: `scripts/p12_dogfood_check.py`
            // and `tests/test_p12_dogfood.py` both match "left held by an
            // interrupted process" to prove this refusal happened on a real
            // machine, and re-proving that costs a paid round. The recovery
            // steps are appended, never spliced into it.
            return Err(Error::policy(
                "workspace write guard was left held by an interrupted process; old children may still exist, so authority is not transferred automatically\n\
                 recover on the Runtime Node, in this order:\n\
                 1. prove the old ones are gone: `ccnm status <workspace>` AND a process list\n\
                    (look for `ccnm internal mcp-serve` for this workspace)\n\
                 2. find the single marker naming that session id in\n\
                    ${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/\n\
                 3. back it up, then delete that one file\n\
                 never clear it just because time passed\n\
                 the full procedure is in docs/operations.md, under 写入 guard 残留 (\"write guard left held\")",
            ));
        }
        if !state_text.is_empty() && state_text != RELEASED {
            let _ = file.unlock();
            return Err(Error::policy(
                "workspace write guard state is incomplete or unknown; refusing to transfer write authority",
            ));
        }
        let held = format!("held {session} {workspace}\n");
        file.rewind()?;
        file.write_all(held.as_bytes())?;
        file.set_len(held.len() as u64)?;
        file.sync_data()?;
        Ok(Self { file })
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        let marked = (|| -> std::io::Result<()> {
            self.file.rewind()?;
            self.file.write_all(RELEASED.as_bytes())?;
            self.file.set_len(RELEASED.len() as u64)?;
            self.file.sync_data()
        })();
        if let Err(error) = marked {
            tracing::warn!(?error, "could not mark Runtime write guard released");
        }
        if let Err(error) = self.file.unlock() {
            tracing::warn!(?error, "could not unlock Runtime write guard explicitly");
        }
    }
}

fn resource_root(root: &Path, runner: &dyn ProcessRunner) -> PathBuf {
    let output = runner.run(
        &Cmd::new("git")
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .cwd(root),
    );
    let path = output
        .ok()
        .filter(|output| output.success())
        .map(|output| PathBuf::from(output.stdout_lossy().trim()))
        .filter(|path| path.is_absolute())
        .and_then(|path| path.canonicalize().ok());
    path.unwrap_or_else(|| root.to_path_buf())
}

fn reject_overlapping_roots(root: &Path, workspace: &str, config: Option<&Config>) -> Result<()> {
    let Some(config) = config else { return Ok(()) };
    for (name, other) in &config.workspaces {
        if name == workspace {
            continue;
        }
        let Ok(other) = other.root.canonicalize() else {
            continue;
        };
        if root.starts_with(&other) || other.starts_with(root) {
            return Err(Error::policy(
                "Runtime workspace roots overlap after canonicalization; separate managed write authority cannot be established",
            ));
        }
    }
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let state = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ccnm-write-guard-{}-{name}",
            crate::session::new_id()
        ));
        let root = state.join("root");
        std::fs::create_dir_all(&root).unwrap();
        (state, root)
    }

    #[test]
    fn concurrent_owner_is_busy_and_clean_drop_is_reentrant() {
        let (state, root) = fixture("busy");
        let runner = FakeRunner::new();
        runner.push(Output::exited(1, ""));
        let first = WriteGuard::acquire(&state, &root, "one", "s1", None, &runner).unwrap();
        let locks = state.join("write-guards");
        assert_eq!(
            std::fs::metadata(&locks).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let lock = std::fs::read_dir(&locks).unwrap().next().unwrap().unwrap();
        assert_eq!(lock.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        runner.push(Output::exited(1, ""));
        assert!(WriteGuard::acquire(&state, &root, "one", "s2", None, &runner).is_err());
        drop(first);
        runner.push(Output::exited(1, ""));
        assert!(WriteGuard::acquire(&state, &root, "one", "s1", None, &runner).is_ok());
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn interrupted_marker_is_unknown_not_a_clock_based_lease() {
        let (state, root) = fixture("stale");
        let locks = state.join("write-guards");
        std::fs::create_dir_all(&locks).unwrap();
        let path = locks.join(format!("{:016x}.lock", fnv1a(root.as_os_str().as_bytes())));
        std::fs::write(path, "held old-session\n").unwrap();
        let runner = FakeRunner::new();
        runner.push(Output::exited(1, ""));
        let error = WriteGuard::acquire(&state, &root, "one", "new", None, &runner)
            .err()
            .unwrap();
        assert!(error.message().contains("not transferred automatically"));
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn partial_or_malformed_release_state_never_grants_authority() {
        for marker in ["rel", "released\nold-bytes", "unknown\n"] {
            let (state, root) = fixture("partial");
            let locks = state.join("write-guards");
            std::fs::create_dir_all(&locks).unwrap();
            let path = locks.join(format!("{:016x}.lock", fnv1a(root.as_os_str().as_bytes())));
            std::fs::write(path, marker).unwrap();
            let runner = FakeRunner::new();
            runner.push(Output::exited(1, ""));
            let error = WriteGuard::acquire(&state, &root, "one", "new", None, &runner)
                .err()
                .unwrap();
            assert!(error.message().contains("unknown"), "{error}");
            std::fs::remove_dir_all(state).unwrap();
        }
    }

    #[test]
    fn git_worktrees_with_one_common_dir_share_the_guard() {
        let (state, root) = fixture("git-common");
        let common = state.join("common");
        std::fs::create_dir(&common).unwrap();
        let runner = FakeRunner::new();
        runner.push(Output::exited(0, format!("{}\n", common.display())));
        let first = WriteGuard::acquire(&state, &root, "one", "s1", None, &runner).unwrap();
        let other = state.join("other");
        std::fs::create_dir(&other).unwrap();
        runner.push(Output::exited(0, format!("{}\n", common.display())));
        assert!(WriteGuard::acquire(&state, &other, "two", "s2", None, &runner).is_err());
        drop(first);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn canonical_nested_and_symlink_alias_roots_are_refused() {
        let (state, root) = fixture("overlap");
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let alias = state.join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        for other in [&nested, &alias] {
            let text = format!(
                "this='runtime'\n[nodes.runtime]\n[nodes.agent]\nssh='agent'\n[workspaces.one]\nagent_node='agent'\nroot='{}'\n[workspaces.two]\nagent_node='agent'\nroot='{}'\n",
                root.display(),
                other.display()
            );
            let config = Config::parse(&text).unwrap();
            assert!(reject_overlapping_roots(&root, "one", Some(&config)).is_err());
        }
        std::fs::remove_dir_all(state).unwrap();
    }
}

//! Runtime-owned single-writer guard for a canonical workspace resource.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::process::{Cmd, ProcessRunner};

pub struct WriteGuard {
    file: File,
    /// The `held …` line this guard wrote. Kept so [`abandon`](Self::abandon)
    /// can leave it in place instead of replacing it with [`RELEASED`].
    held: String,
    /// Set when this session ends with something it could not stop. The
    /// guard is then **not** handed on, and this says why.
    abandoned: Mutex<Option<String>>,
}

const RELEASED: &str = "released\n";
/// The second line of an abandoned marker. Everything after it is what the
/// session could not stop, as [`Jobs::stop_all`](crate::mcp::jobs::Jobs::stop_all)
/// named them.
const ABANDONED: &str = "abandoned ";

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
        if let Some(rest) = state_text.strip_prefix("held ") {
            let _ = file.unlock();
            return Err(Error::policy(left_held(rest, runner)));
        }
        if !state_text.is_empty() && state_text != RELEASED {
            let _ = file.unlock();
            return Err(Error::policy(
                "workspace write guard state is incomplete or unknown; refusing to transfer write authority",
            ));
        }
        // The pid is for whoever has to recover this by hand: it turns
        // "look for an mcp-serve for this workspace" into one process to
        // check. It is **never** a reason to take the guard -- a pid that
        // is gone says nothing about the children it left (P43).
        let held = format!("held {session} {workspace} pid {}\n", std::process::id());
        file.rewind()?;
        file.write_all(held.as_bytes())?;
        file.set_len(held.len() as u64)?;
        file.sync_data()?;
        Ok(Self {
            file,
            held,
            abandoned: Mutex::new(None),
        })
    }

    /// Do not hand this guard on: the session is ending with something it
    /// could not stop, and that something can still write the working tree.
    ///
    /// The marker stays `held`, so the next session is refused exactly as
    /// after a crash -- but the wording differs, because this is not a
    /// crash: ccnm knew what was left and chose not to transfer authority
    /// (评审 X05: 不允许未知旧写者与新写者同时获得受管权限).
    ///
    /// The first reason wins; a second call changes nothing.
    pub fn abandon(&self, what: &str) {
        let mut slot = self
            .abandoned
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            tracing::warn!(what, "not releasing the write guard");
            *slot = Some(what.to_string());
        }
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        let abandoned = self
            .abandoned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let text = match &abandoned {
            Some(what) => format!("{}{ABANDONED}{what}\n", self.held),
            None => RELEASED.to_string(),
        };
        let marked = (|| -> std::io::Result<()> {
            self.file.rewind()?;
            self.file.write_all(text.as_bytes())?;
            self.file.set_len(text.len() as u64)?;
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

/// What to say when the marker is still `held`.
///
/// Two cases, and the difference matters to whoever reads it:
///
/// - **abandoned**: the last session ended with commands it could not stop.
///   This is not a crash; ccnm knew and chose not to transfer authority.
///   It also knows what is out there, so it names them.
/// - anything else: the process was interrupted before it could mark the
///   guard released.
///
/// Either way the pid is checked once, and either way the answer is a
/// refusal. **A pid that is gone is not permission to take the guard**: the
/// children it left can outlive it, and nothing here can see them (P43;
/// 评审 X05「不能仅凭 PID、进程不存在一次或等待某个固定时长就清锁」).
fn left_held(rest: &str, runner: &dyn ProcessRunner) -> String {
    let first = rest.lines().next().unwrap_or_default();
    let abandoned = rest
        .lines()
        .skip(1)
        .find_map(|line| line.strip_prefix(ABANDONED));
    let mut words = first.split_whitespace();
    let session = words.next().unwrap_or("that session");
    let _workspace = words.next();
    let pid = match (words.next(), words.next()) {
        (Some("pid"), Some(pid)) => pid.parse::<u32>().ok(),
        _ => None,
    };

    let opening = match abandoned {
        // The first line keeps its wording in the interrupted case:
        // `scripts/p12_dogfood_check.py` and `tests/test_p12_dogfood.py`
        // both match "left held by an interrupted process" to prove this
        // refusal happened on a real machine, and re-proving that costs a
        // paid round.
        None => "workspace write guard was left held by an interrupted process; old children may still exist, so authority is not transferred automatically".to_string(),
        Some(what) => format!(
            "workspace write guard was kept on purpose: the session that held it ended with {what} it could not stop, and those can still write this working tree, so authority is not transferred"
        ),
    };
    let evidence = match pid {
        None => format!(
            "the marker names session {session}; it predates the pid record, so look for an\n\
             `ccnm internal mcp-serve` for this workspace in a process list"
        ),
        Some(pid) => match owner_now(pid, runner) {
            Some(command) if command.contains("ccnm") => format!(
                "that session ({session}) is still running as pid {pid}: {command}\n\
                 end it first; taking the guard from a live writer is the one thing this refusal exists to stop"
            ),
            Some(command) => format!(
                "the session ({session}) ran as pid {pid}, and that pid is now something else ({command}),\n\
                 so its own process is gone. **That does not clear this**: commands it started can outlive it"
            ),
            None => format!(
                "the session ({session}) ran as pid {pid}, which is gone.\n\
                 **That does not clear this**: commands it started can outlive it, and nothing here can see them"
            ),
        },
    };
    let steps = match abandoned {
        Some(_) => format!(
            "recover on the Runtime Node, in this order:\n\
             1. end what is named above. Each output_ref's command line is in\n\
                ${{XDG_STATE_HOME:-~/.local/state}}/ccnm/sessions/{session}/output/<ref>/status;\n\
                look for the process group it left behind\n\
             2. only then back up and delete the single marker naming {session} in\n\
                ${{XDG_STATE_HOME:-~/.local/state}}/ccnm/write-guards/"
        ),
        None => "recover on the Runtime Node, in this order:\n\
             1. prove the old ones are gone: `ccnm status <workspace>` AND a process list\n\
                (look for `ccnm internal mcp-serve` for this workspace)\n\
             2. find the single marker naming that session id in\n\
                ${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/\n\
             3. back it up, then delete that one file"
            .to_string(),
    };
    format!(
        "{opening}\n{evidence}\n{steps}\n\
         never clear it just because time passed\n\
         the full procedure is in docs/operations.md, under 写入 guard 残留 (\"write guard left held\")"
    )
}

/// What that pid is now, or `None` when there is no such process. A pid is
/// reused, so the command line is part of the answer -- it is what tells a
/// stale marker apart from a live writer.
fn owner_now(pid: u32, runner: &dyn ProcessRunner) -> Option<String> {
    let output = runner
        .run(&Cmd::new("ps").args(["-o", "command=", "-p", &pid.to_string()]))
        .ok()?;
    if !output.success() {
        return None;
    }
    let line = output.stdout_lossy().trim().to_string();
    (!line.is_empty()).then_some(line)
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

    /// 停不掉的命令留下的 guard **不交给下一个人**，而且话里说得出还剩什么。
    ///
    /// 这条挡的是 P43 之前真实跑出来的那一幕：一个离开了进程组又攥着管道的
    /// 后代，让 `stop_all` 放弃，server 照样正常退出并把 guard 标成 released，
    /// 下一个 coding 会话就在同一棵树上开起来了（评审 X05）。
    #[test]
    fn a_session_that_could_not_stop_everything_does_not_hand_the_guard_on() {
        let (state, root) = fixture("abandon");
        let runner = FakeRunner::new();
        runner.push(Output::exited(1, ""));
        let guard = WriteGuard::acquire(&state, &root, "one", "s1", None, &runner).unwrap();
        guard.abandon("2 command(s) (r-aaa, r-bbb)");
        drop(guard);

        let path = state
            .join("write-guards")
            .join(format!("{:016x}.lock", fnv1a(root.as_os_str().as_bytes())));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("held s1 one pid "), "{text:?}");
        assert!(
            text.contains("\nabandoned 2 command(s) (r-aaa, r-bbb)\n"),
            "{text:?}"
        );

        runner.push(Output::exited(1, ""));
        runner.push(Output::exited(0, "ccnm internal mcp-serve --payload x\n"));
        let error = WriteGuard::acquire(&state, &root, "one", "s2", None, &runner)
            .err()
            .unwrap();
        let said = error.message();
        // 不是"被中断了"：ccnm 知道剩了什么，是它自己决定不交权的。
        assert!(said.contains("kept on purpose"), "{said}");
        assert!(said.contains("r-aaa, r-bbb"), "{said}");
        assert!(said.contains("still running as pid"), "{said}");
        std::fs::remove_dir_all(state).unwrap();
    }

    /// marker 里的 pid 只让诊断说得准，**从来不是交权的理由**：那个进程没了，
    /// 它起的命令可能还在，而这里看不见它们（评审 X05）。
    #[test]
    fn the_pid_sharpens_the_refusal_and_never_lifts_it() {
        for (ps, expected) in [
            (
                Output::exited(0, "ccnm internal mcp-serve --payload x\n"),
                "still running as pid",
            ),
            (Output::exited(0, "vim notes.txt\n"), "now something else"),
            (Output::exited(1, ""), "which is gone"),
        ] {
            let (state, root) = fixture("pid");
            let locks = state.join("write-guards");
            std::fs::create_dir_all(&locks).unwrap();
            let path = locks.join(format!("{:016x}.lock", fnv1a(root.as_os_str().as_bytes())));
            std::fs::write(&path, "held old-session demo pid 4242\n").unwrap();
            let runner = FakeRunner::new();
            runner.push(Output::exited(1, ""));
            runner.push(ps);
            let error = WriteGuard::acquire(&state, &root, "one", "new", None, &runner)
                .err()
                .unwrap();
            let said = error.message();
            assert!(said.contains(expected), "{said}");
            assert!(said.contains("not transferred"), "拒绝是唯一结果：{said}");
            std::fs::remove_dir_all(state).unwrap();
        }
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

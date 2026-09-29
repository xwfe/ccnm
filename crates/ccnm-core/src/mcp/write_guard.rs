//! Runtime-owned single-writer guard for a canonical workspace resource.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

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

/// How often [`WriteGuard::acquire`] looks again before calling the guard
/// busy, and how long it waits in between.
///
/// An [`observe`] holds the lock shared for the few microseconds it takes to
/// read the marker. A writer arriving in exactly that instant would
/// otherwise be refused as busy by a diagnostic, which is the one thing an
/// observation must not do (P60). 5 × 20 ms is far longer than any read of a
/// one-line file and far shorter than anything a person or client notices on
/// a real refusal.
const BUSY_RETRIES: u32 = 5;
const BUSY_BACKOFF: Duration = Duration::from_millis(20);

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
        let path = lock_path(state, &resource);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let mut looked = 0;
        let locked = loop {
            match file.try_lock() {
                Err(std::fs::TryLockError::WouldBlock) if looked < BUSY_RETRIES => {
                    looked += 1;
                    std::thread::sleep(BUSY_BACKOFF);
                }
                other => break other,
            }
        };
        match locked {
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
                look for the process group it left behind. An MCP server's leftovers are\n\
                the process group and pids named; `ps -A -o pid,pgid,stat,command` shows them\n\
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

/// The write guard of one workspace as the Runtime that keeps it sees it
/// right now (P60).
///
/// **A report, not a grant.** Nothing here reserves the tree: by the time a
/// `free` reaches whoever asked, another writer may have taken it, and the
/// only thing that hands out write authority is still
/// [`WriteGuard::acquire`] when a session opens. That is also why the state
/// comes from the lock and the owner from the marker: the lock says whether
/// some process holds it now, the marker says who took it last, and neither
/// alone says both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub state: Observed,
    pub reason: Reason,
    pub resource: Resource,
    /// Who the marker names. Absent when it names nobody, or when a live
    /// holder has not finished writing it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    /// What the last session could not stop, in its own words: commands by
    /// `output_ref`, relayed MCP servers by process group. Only for
    /// [`Observed::Abandoned`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leftovers: Option<String>,
    /// Unix seconds on the Runtime.
    pub observed_at: u64,
}

/// Four answers, and **not** session states: a Machine API session never
/// ends up `held`, and none of these ends one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observed {
    /// The next session would get the guard, if nobody takes it first.
    Free,
    /// A process holds it now.
    Held,
    /// The last session ended with something it could not stop and kept
    /// the guard on purpose.
    Abandoned,
    /// Nobody can say. A person has to look before anyone writes.
    Unknown,
}

/// Why, from a fixed list, so that nobody has to parse a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No marker: nobody using this state directory has taken it yet.
    NeverTaken,
    /// The last owner ended cleanly.
    Released,
    /// Some process holds the lock.
    LiveHolder,
    /// See [`WriteGuard::abandon`].
    KeptOnPurpose,
    /// The marker says held and nobody holds the lock: the owner was
    /// interrupted. What it started may still be running, and nothing here
    /// can see it, so this is unknown rather than free (P43).
    LeftHeld,
    /// Neither empty, `released` nor `held …`: half written, or not ours.
    MalformedMarker,
    /// The marker or its directory could not be read by this account.
    Unreadable,
    /// The lock itself could not be asked.
    LockQueryFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub kind: ResourceKind,
    /// The marker's file name without `.lock`, so a person can find the one
    /// file to look at. A hash, not the path: this answer leaves the Runtime.
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// One guard for every worktree of the repository.
    GitCommonDir,
    /// Not a git repository: the workspace root itself.
    Root,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// For diagnosis only. It never decides [`Observation::state`]: a pid
    /// that is gone says nothing about the children it left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// What that pid is now; absent when the marker has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    /// A marker from before P43, which recorded no pid.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Process {
    /// A ccnm process has that pid.
    Ccnm,
    /// Something else has it now: the pid was reused.
    Other,
    /// No process has it.
    Gone,
    /// `ps` could not be asked.
    Unchecked,
}

/// Look at the guard of `root` (canonical) without touching it.
///
/// Creates nothing and writes nothing: no directory, no marker, no lock
/// that outlives this call. To tell a live holder from a marker left behind
/// it has to ask the lock, so it takes it **shared** for as long as reading
/// one line takes and then releases it explicitly -- not by closing: a
/// child forked meanwhile shares the open file, and a lock released only
/// by close would live on in it until it execs (P34). A writer that
/// arrives in that instant is covered by [`BUSY_RETRIES`].
pub fn observe(state: &Path, root: &Path, runner: &dyn ProcessRunner) -> Observation {
    let resource = resource_root(root, runner);
    let reference = Resource {
        kind: if resource == root {
            ResourceKind::Root
        } else {
            ResourceKind::GitCommonDir
        },
        id: resource_id(&resource),
    };
    let seen = |state, reason, owner, leftovers| Observation {
        state,
        reason,
        resource: reference.clone(),
        owner,
        leftovers,
        observed_at: crate::overview::now_secs(),
    };
    let file = match File::open(lock_path(state, &resource)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return seen(Observed::Free, Reason::NeverTaken, None, None);
        }
        Err(_) => return seen(Observed::Unknown, Reason::Unreadable, None, None),
    };
    let live = match file.try_lock_shared() {
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(_) => return seen(Observed::Unknown, Reason::LockQueryFailed, None, None),
    };
    let mut text = String::new();
    let read = (&file).read_to_string(&mut text);
    if !live {
        // Before anything below forks a `ps`.
        let _ = file.unlock();
    }
    drop(file);
    let marker = parse_marker(&text);
    let owner = marker.as_ref().map(|(owner, _)| Owner {
        process: owner.pid.map(|pid| process_now(pid, runner)),
        ..owner.clone()
    });
    if live {
        // Read without the lock, so possibly mid-write: the owner is a
        // best guess, and a writer that has not written its line yet shows
        // none (the line before it can only be empty or `released`, or it
        // would not have got the lock).
        return seen(Observed::Held, Reason::LiveHolder, owner, None);
    }
    if read.is_err() {
        return seen(Observed::Unknown, Reason::Unreadable, None, None);
    }
    match (text.as_str(), marker) {
        ("", _) => seen(Observed::Free, Reason::NeverTaken, None, None),
        (RELEASED, _) => seen(Observed::Free, Reason::Released, None, None),
        (_, Some((_, Some(what)))) => seen(
            Observed::Abandoned,
            Reason::KeptOnPurpose,
            owner,
            Some(what),
        ),
        (_, Some((_, None))) => seen(Observed::Unknown, Reason::LeftHeld, owner, None),
        (_, None) => seen(Observed::Unknown, Reason::MalformedMarker, None, None),
    }
}

/// `held <session> [<workspace> [pid <pid>]]`, then an optional
/// `abandoned …` line: the owner, and what it could not stop.
fn parse_marker(text: &str) -> Option<(Owner, Option<String>)> {
    let first = text.lines().next()?.strip_prefix("held ")?;
    let mut words = first.split_whitespace();
    let session = words.next()?.to_string();
    let workspace = words.next().map(str::to_string);
    // Read the way `left_held` reads it, so that whatever the refusal
    // names, this names too.
    let (legacy, pid) = match (words.next(), words.next()) {
        (None, _) => (true, None),
        (Some("pid"), Some(pid)) => (false, pid.parse::<u32>().ok()),
        _ => (false, None),
    };
    let abandoned = text
        .lines()
        .skip(1)
        .find_map(|line| line.strip_prefix(ABANDONED))
        .map(str::to_string);
    Some((
        Owner {
            session,
            workspace,
            pid,
            process: None,
            legacy,
        },
        abandoned,
    ))
}

/// [`owner_now`], keeping "no such process" apart from "could not ask":
/// the second is not evidence of anything.
fn process_now(pid: u32, runner: &dyn ProcessRunner) -> Process {
    let Ok(output) = runner.run(&Cmd::new("ps").args(["-o", "command=", "-p", &pid.to_string()]))
    else {
        return Process::Unchecked;
    };
    let command = output.stdout_lossy().trim().to_string();
    match (output.success(), command.is_empty()) {
        (true, false) if command.contains("ccnm") => Process::Ccnm,
        (true, false) => Process::Other,
        // Both `ps`es exit 1 with nothing on stdout for a pid that is not
        // there. Anything else is `ps` failing.
        (false, true) if output.exit_code == Some(1) => Process::Gone,
        _ => Process::Unchecked,
    }
}

fn lock_path(state: &Path, resource: &Path) -> PathBuf {
    state
        .join("write-guards")
        .join(format!("{}.lock", resource_id(resource)))
}

fn resource_id(resource: &Path) -> String {
    format!(
        "{:016x}",
        crate::paths::fnv1a(resource.as_os_str().as_bytes())
    )
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
        let path = locks.join(format!(
            "{:016x}.lock",
            crate::paths::fnv1a(root.as_os_str().as_bytes())
        ));
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

        let path = state.join("write-guards").join(format!(
            "{:016x}.lock",
            crate::paths::fnv1a(root.as_os_str().as_bytes())
        ));
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
            let path = locks.join(format!(
                "{:016x}.lock",
                crate::paths::fnv1a(root.as_os_str().as_bytes())
            ));
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
            let path = locks.join(format!(
                "{:016x}.lock",
                crate::paths::fnv1a(root.as_os_str().as_bytes())
            ));
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

    fn marker_path(state: &Path, root: &Path) -> PathBuf {
        state.join("write-guards").join(format!(
            "{:016x}.lock",
            crate::paths::fnv1a(root.as_os_str().as_bytes())
        ))
    }

    /// 观察说 free，当且仅当下一个 acquire 真能拿到；而且观察本身一个字节都不改。
    ///
    /// 这是两个判断同一件事的函数，一个给人和调用方看、一个真正交权。它们
    /// 说法不一致，就会出现"说空闲却进不去"或者更糟的"说被占其实没人"。
    #[test]
    fn observation_agrees_with_acquire_and_changes_nothing() {
        let cases: &[(Option<&str>, Observed, Reason)] = &[
            (None, Observed::Free, Reason::NeverTaken),
            (Some(""), Observed::Free, Reason::NeverTaken),
            (Some("released\n"), Observed::Free, Reason::Released),
            (
                Some("held s0 demo pid 4242\n"),
                Observed::Unknown,
                Reason::LeftHeld,
            ),
            // P43 之前的两种格式：没有 pid。
            (Some("held s0 demo\n"), Observed::Unknown, Reason::LeftHeld),
            (Some("held s0\n"), Observed::Unknown, Reason::LeftHeld),
            (
                Some("held s0 demo pid 4242\nabandoned 1 command(s) (r-a)\n"),
                Observed::Abandoned,
                Reason::KeptOnPurpose,
            ),
            (Some("rel"), Observed::Unknown, Reason::MalformedMarker),
            (
                Some("released\nold-bytes"),
                Observed::Unknown,
                Reason::MalformedMarker,
            ),
            (Some("held \n"), Observed::Unknown, Reason::MalformedMarker),
        ];
        for (marker, state_seen, reason) in cases {
            let (state, root) = fixture("agree");
            if let Some(marker) = marker {
                std::fs::create_dir_all(state.join("write-guards")).unwrap();
                std::fs::write(marker_path(&state, &root), marker).unwrap();
            }
            // 空的 runner：git 和 ps 都问不到。资源退回 root，pid 核对不了。
            let seen = observe(&state, &root, &FakeRunner::new());
            assert_eq!(
                (seen.state, seen.reason),
                (*state_seen, *reason),
                "{marker:?}"
            );
            assert_eq!(seen.resource.kind, ResourceKind::Root);
            match marker {
                None => assert!(!state.join("write-guards").exists(), "观察不建目录"),
                Some(marker) => assert_eq!(
                    std::fs::read_to_string(marker_path(&state, &root)).unwrap(),
                    *marker,
                    "观察不改 marker"
                ),
            }
            let runner = FakeRunner::new();
            let acquired = WriteGuard::acquire(&state, &root, "demo", "s1", None, &runner);
            assert_eq!(
                acquired.is_ok(),
                *state_seen == Observed::Free,
                "{marker:?}: {:?}",
                acquired.err()
            );
            drop(acquired);
            std::fs::remove_dir_all(state).unwrap();
        }
    }

    #[test]
    fn a_live_holder_is_held_named_and_left_alone() {
        let (state, root) = fixture("live");
        let runner = FakeRunner::new();
        let holder = WriteGuard::acquire(&state, &root, "demo", "s1", None, &runner).unwrap();
        let before = std::fs::read(marker_path(&state, &root)).unwrap();

        let ps = FakeRunner::new();
        ps.push(Output::exited(1, "")); // git
        ps.push(Output::exited(0, "ccnm internal mcp-serve --payload x\n"));
        let seen = observe(&state, &root, &ps);
        assert_eq!(
            (seen.state, seen.reason),
            (Observed::Held, Reason::LiveHolder)
        );
        let owner = seen.owner.expect("the marker names who holds it");
        assert_eq!(owner.session, "s1");
        assert_eq!(owner.workspace.as_deref(), Some("demo"));
        assert_eq!(owner.pid, Some(std::process::id()));
        assert_eq!(owner.process, Some(Process::Ccnm));
        assert!(!owner.legacy);

        assert_eq!(std::fs::read(marker_path(&state, &root)).unwrap(), before);
        // 持有者还是它：别人照样进不来，它自己收尾照样写 released。
        assert!(
            WriteGuard::acquire(&state, &root, "demo", "s2", None, &FakeRunner::new()).is_err()
        );
        drop(holder);
        assert_eq!(
            std::fs::read_to_string(marker_path(&state, &root)).unwrap(),
            RELEASED
        );
        std::fs::remove_dir_all(state).unwrap();
    }

    /// pid 只是诊断：四种说法都有，状态一个都不变（评审 X05）。
    #[test]
    fn the_pid_is_described_and_never_decides() {
        for (ps, expected) in [
            (
                Some(Output::exited(0, "ccnm internal mcp-serve\n")),
                Process::Ccnm,
            ),
            (Some(Output::exited(0, "vim notes.txt\n")), Process::Other),
            (Some(Output::exited(1, "")), Process::Gone),
            (Some(Output::exited(2, "")), Process::Unchecked),
            (None, Process::Unchecked),
        ] {
            let (state, root) = fixture("pid-seen");
            std::fs::create_dir_all(state.join("write-guards")).unwrap();
            std::fs::write(marker_path(&state, &root), "held old demo pid 4242\n").unwrap();
            let runner = FakeRunner::new();
            runner.push(Output::exited(1, "")); // git
            if let Some(ps) = ps {
                runner.push(ps);
            }
            let seen = observe(&state, &root, &runner);
            assert_eq!(
                (seen.state, seen.reason),
                (Observed::Unknown, Reason::LeftHeld)
            );
            let owner = seen.owner.unwrap();
            assert_eq!((owner.pid, owner.process), (Some(4242), Some(expected)));
            std::fs::remove_dir_all(state).unwrap();
        }
    }

    #[test]
    fn what_this_account_cannot_read_is_unknown_not_free() {
        let (state, root) = fixture("unreadable");
        let locks = state.join("write-guards");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(marker_path(&state, &root), "released\n").unwrap();
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o000)).unwrap();
        let seen = observe(&state, &root, &FakeRunner::new());
        // root 读得到一切，这条对它不成立。
        let denied = File::open(marker_path(&state, &root)).is_err();
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o700)).unwrap();
        if denied {
            assert_eq!(
                (seen.state, seen.reason),
                (Observed::Unknown, Reason::Unreadable)
            );
        }
        std::fs::remove_dir_all(state).unwrap();
    }

    /// 观察那一瞬间的共享锁不让新 writer 被拒成 busy。
    #[test]
    fn a_writer_arriving_during_an_observation_still_gets_the_guard() {
        let (state, root) = fixture("overlap-observe");
        std::fs::create_dir_all(state.join("write-guards")).unwrap();
        std::fs::write(marker_path(&state, &root), RELEASED).unwrap();
        // 一次拉长了的观察：共享锁攥 40 ms。
        let peek = File::open(marker_path(&state, &root)).unwrap();
        peek.try_lock_shared().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            peek.unlock().unwrap();
        });
        let guard = WriteGuard::acquire(&state, &root, "demo", "s1", None, &FakeRunner::new());
        release.join().unwrap();
        assert!(guard.is_ok(), "{:?}", guard.err());
        drop(guard);
        std::fs::remove_dir_all(state).unwrap();
    }

    /// fork 压力下观察不留锁：每次观察一结束，排他锁马上拿得到。
    ///
    /// 靠 close 放锁的写法在这里会偶发失败：别的线程刚 fork 出来、还没
    /// exec 的子进程和我们共享同一个打开的文件，锁跟着它活到 exec（P34）。
    #[test]
    fn observing_under_fork_pressure_leaves_no_lock_behind() {
        let (state, root) = fixture("fork");
        std::fs::create_dir_all(state.join("write-guards")).unwrap();
        std::fs::write(marker_path(&state, &root), RELEASED).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let forkers: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = std::process::Command::new("/usr/bin/true").status();
                    }
                })
            })
            .collect();
        for round in 0..200 {
            let seen = observe(&state, &root, &FakeRunner::new());
            assert_eq!(seen.state, Observed::Free);
            let check = File::open(marker_path(&state, &root)).unwrap();
            assert!(check.try_lock().is_ok(), "round {round}: 观察留下了锁");
            check.unlock().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for forker in forkers {
            forker.join().unwrap();
        }
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

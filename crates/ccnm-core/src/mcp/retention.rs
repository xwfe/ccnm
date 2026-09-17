//! Where `exec_command` keeps what a command wrote, and when it stops.
//!
//! ```text
//! sessions/<session>/output/<run>/stdout
//!                               /stderr
//! ```
//!
//! Nothing but this module creates or removes anything under `output/`,
//! and nothing else on the Runtime Node reads it except `read_output`,
//! which only ever looks inside its own session's directory. That is what
//! makes removing it safe: once no process serves a session, nobody can
//! ask for its output again.
//!
//! # Which runs are still running
//!
//! A run in progress has a `running` file in its directory, created before
//! the directory is visible and locked (`flock`) for as long as the run
//! lasts; the run removes the file and then lets go of the lock. Anything
//! that removes runs leaves a run alone while that file exists **and** its
//! lock is held.
//!
//! Why not a list in memory: one session can have two servers at once (a
//! managed session's `/mcp Reconnect` starts a new `mcp-serve` under the
//! same id while the old one may still be finishing a command). Why a lock:
//! a process that dies drops it by itself, where a pid would have to be
//! judged alive or dead and a reused pid judges wrong -- so a `running`
//! file nobody holds is a crash, and that run is finished. Why the file as
//! well as the lock: a lock can outlive its run for a moment. A thread
//! that forks a child while a run's lock is open gives the child a copy,
//! held until the child execs; measured in this crate's tests, long enough
//! that a run finished and let go was still "locked" when the next line
//! looked. The file is gone before the lock is let go, so that copy no
//! longer matters.

use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::error::{Error, Result};
use crate::process::ProcessRunner;

const MIB: u64 = 1024 * 1024;

/// How much output one session may leave behind, and for how long.
///
/// Not parameters of the tool: they bound what ccnm leaves on the user's
/// machine, which is not the caller's decision. Separate from the tool's
/// arguments only so tests can use sizes that do not take seconds to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Bytes of one stream of one run kept on disk. A command that
    /// produces more still runs to completion and still reports its exit
    /// code; the retained copy is cut and says so.
    pub per_stream: u64,
    /// Runs kept per session. Removing the oldest is what stops a long
    /// session from filling the disk with build logs one small run at a
    /// time.
    pub runs: usize,
    /// Bytes of finished runs kept per session, stdout and stderr together.
    /// Without it the two limits above allow 100 × 2 × 64 MiB = 12.5 GiB
    /// for one session. 256 MiB is toexec v2 plan section 9's figure,
    /// chosen by the user on 2026-09-17; raising it raises the worst case
    /// per session one for one.
    pub session_bytes: u64,
    /// How long the output of a session nobody serves is kept after its
    /// newest run. Seven days, chosen with the figure above: long enough
    /// for a person to look at a failed build next week, short enough that
    /// sessions do not pile up forever on a Runtime that never runs
    /// `--purge`.
    pub expiry: Duration,
}

impl Limits {
    pub(crate) const RUNTIME: Limits = Limits {
        per_stream: 64 * MIB,
        runs: 100,
        session_bytes: 256 * MIB,
        expiry: Duration::from_secs(7 * 24 * 60 * 60),
    };
}

/// The file whose presence and lock mark a run in progress.
const RUNNING: &str = "running";

/// One session's retained output.
pub struct Output {
    dir: PathBuf,
    limits: Limits,
    /// Runs this process started, for [`Output::discard_started`].
    started: Mutex<Vec<PathBuf>>,
}

impl Output {
    pub fn new(state: &Path, session: &str) -> Output {
        Output::with_limits(state, session, Limits::RUNTIME)
    }

    pub(crate) fn with_limits(state: &Path, session: &str, limits: Limits) -> Output {
        Output {
            dir: session_dir(state, session),
            limits,
            started: Mutex::new(Vec::new()),
        }
    }

    /// `sessions/<session>/output`. `read_output` resolves references
    /// against exactly this, so an output_ref is a reference within one
    /// session and not a handle on the machine.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }

    /// A fresh run, already locked, with room made for it.
    pub(crate) fn begin(&self) -> Result<(Run, Sink, Sink)> {
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            Error::internal("cannot create the output retention directory").with_source(e)
        })?;
        self.evict(None, self.limits.runs.saturating_sub(1));

        let reference = format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let dir = self.dir.join(&reference);
        // Built under a name no remover looks at, then renamed: a directory
        // that is visible before its `running` file exists and is locked
        // would look like a finished, empty run to another server of this
        // session making room.
        let staging = self.dir.join(format!(".{reference}"));
        std::fs::create_dir(&staging).map_err(|e| {
            Error::internal("cannot create a directory for this run").with_source(e)
        })?;
        let files = (|| -> std::io::Result<(File, File, File)> {
            let stdout = File::create(staging.join("stdout"))?;
            let stderr = File::create(staging.join("stderr"))?;
            let running = File::create(staging.join(RUNNING))?;
            running.lock()?;
            std::fs::rename(&staging, &dir)?;
            Ok((stdout, stderr, running))
        })();
        let (stdout, stderr, running) = files.map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            Error::internal("cannot create a file for the command's output").with_source(e)
        })?;
        self.started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(dir.clone());
        Ok((
            Run {
                dir,
                reference,
                _running: running,
            },
            Sink::new(stdout, self.limits.per_stream),
            Sink::new(stderr, self.limits.per_stream),
        ))
    }

    /// Bring the session back within its limits now that `run` is done.
    ///
    /// Called while `run` is still locked, so no other process removes it
    /// before its result has been read -- and counted in the total, so
    /// the finished runs it leaves really do fit.
    pub(crate) fn finish(&self, run: &Run) {
        self.evict(Some(&run.dir), self.limits.runs);
    }

    /// Remove the oldest finished runs until at most `keep_runs` are left
    /// and the finished ones, plus `current`, fit in `session_bytes`.
    ///
    /// A run another server is still writing is never removed, so while
    /// runs overlap the session can be over its byte limit by what they
    /// hold, at most 2 × `per_stream` each. Failing to remove one is not
    /// the command's failure; it is logged and the next oldest is tried.
    fn evict(&self, current: Option<&Path>, keep_runs: usize) {
        let mut runs = runs_in(&self.dir);
        runs.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.dir.cmp(&b.dir)));
        let busy: Vec<bool> = runs
            .iter()
            .map(|run| Some(run.dir.as_path()) == current || in_progress(&run.dir))
            .collect();
        let mut count = runs.len();
        let mut bytes: u64 = runs
            .iter()
            .zip(&busy)
            .filter(|(run, busy)| !**busy || Some(run.dir.as_path()) == current)
            .map(|(run, _)| run.bytes)
            .sum();
        for (run, busy) in runs.iter().zip(busy) {
            if count <= keep_runs && bytes <= self.limits.session_bytes {
                break;
            }
            if busy {
                continue;
            }
            match std::fs::remove_dir_all(&run.dir) {
                Ok(()) => {}
                // Another server of this session removed it first.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(run = %run.dir.display(), %error, "cannot remove retained output");
                    continue;
                }
            }
            count -= 1;
            bytes -= run.bytes;
        }
    }

    /// Remove the runs this process started, once the session is over.
    ///
    /// Only for a session whose id ends with this process: an external
    /// client's. Its output_refs cannot be used by anyone after that (the
    /// bridge never reconnects to the same session), so keeping them only
    /// costs disk. A managed session must not call this -- its id survives
    /// `/mcp Reconnect`, and so do its references.
    ///
    /// Only what this process started, not the whole directory: a session
    /// id arrives from the other machine, and a read-only client that named
    /// somebody else's session must not be able to delete that output by
    /// disconnecting. Emptied directories go too, one level at a time and
    /// never recursively.
    pub fn discard_started(&self) {
        let started = std::mem::take(
            &mut *self
                .started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for run in started {
            // Still locked means a command outlived the session; the expiry
            // sweep gets it later.
            if in_progress(&run) {
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(&run)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(run = %run.display(), %error, "cannot remove retained output");
            }
        }
        if std::fs::remove_dir(&self.dir).is_ok()
            && let Some(session) = self.dir.parent()
        {
            let _ = std::fs::remove_dir(session);
        }
    }
}

/// `sessions/<session>/output`.
pub fn session_dir(state: &Path, session: &str) -> PathBuf {
    crate::paths::session_dir(state, session).join("output")
}

/// One run in progress. Dropping it marks the run finished: the `running`
/// file goes first, then (as the field drops) its lock.
pub(crate) struct Run {
    pub dir: PathBuf,
    pub reference: String,
    _running: File,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join(RUNNING));
    }
}

impl Run {
    pub fn stdout(&self) -> PathBuf {
        self.dir.join("stdout")
    }

    pub fn stderr(&self) -> PathBuf {
        self.dir.join("stderr")
    }
}

/// A file that stops writing at its limit but keeps accepting, so the pipe
/// behind it is always drained and the command never blocks on a full pipe.
pub(crate) struct Sink {
    file: File,
    written: u64,
    limit: u64,
}

impl Sink {
    fn new(file: File, limit: u64) -> Sink {
        Sink {
            file,
            written: 0,
            limit,
        }
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = self.limit.saturating_sub(self.written);
        if room == 0 {
            return Ok(buf.len());
        }
        let take = usize::try_from(room).unwrap_or(usize::MAX).min(buf.len());
        self.file.write_all(&buf[..take])?;
        self.written += take as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

struct Retained {
    dir: PathBuf,
    modified: SystemTime,
    bytes: u64,
}

/// The runs in a session's output directory: `r-*` directories, not
/// following links. Staging directories and anything else are not runs.
fn runs_in(output: &Path) -> Vec<Retained> {
    let Ok(entries) = std::fs::read_dir(output) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("r-"))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let dir = entry.path();
            let modified = entry.metadata().ok()?.modified().ok()?;
            let size =
                |name: &str| std::fs::symlink_metadata(dir.join(name)).map_or(0, |meta| meta.len());
            let bytes = size("stdout") + size("stderr");
            Some(Retained {
                dir,
                modified,
                bytes,
            })
        })
        .collect()
}

/// Whether a server is still running this run: its `running` file is there
/// and somebody holds the lock on it. No file (a finished run, or one from
/// before runs were marked) is finished; a file nobody holds is a server
/// that died. One whose lock cannot even be asked about is treated as
/// running, because removing what cannot be judged is the one mistake here
/// that loses data.
fn in_progress(run: &Path) -> bool {
    let file = match File::open(run.join(RUNNING)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => true,
    }
}

/// Remove the output of every session that nobody serves and nobody has
/// run anything in for [`Limits::expiry`].
///
/// Run once as a server starts, off the thread that answers the client, so
/// it never delays a handshake or fails a session. `ps` is asked which
/// sessions are being served; if it cannot say, nothing is removed --
/// "could not look" must not read as "nobody is there".
pub fn sweep_expired(state: &Path, own_session: &str, runner: &dyn ProcessRunner) -> Vec<PathBuf> {
    let Some(servers) = crate::overview::try_scan_servers(runner) else {
        tracing::warn!("cannot list running mcp-serve processes; expired output is kept");
        return Vec::new();
    };
    let mut live: Vec<String> = servers.into_iter().map(|server| server.session).collect();
    live.push(own_session.to_string());
    sweep(state, &live, SystemTime::now(), Limits::RUNTIME.expiry)
}

/// [`sweep_expired`] with the facts it looks up passed in.
///
/// Removes `sessions/<id>/output` only, never `sessions/<id>` itself unless
/// that is then empty: when one state directory is used by both roles, the
/// Agent's own record of a session lives beside it.
pub(crate) fn sweep(
    state: &Path,
    live: &[String],
    now: SystemTime,
    expiry: Duration,
) -> Vec<PathBuf> {
    let Ok(sessions) = std::fs::read_dir(crate::paths::sessions_dir(state)) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in sessions.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let output = entry.path().join("output");
        if !output.is_dir() || live.contains(&id) {
            continue;
        }
        let Some(newest) = last_activity(&output) else {
            continue;
        };
        // A time in the future (a clock that moved back) is not old.
        if now.duration_since(newest).is_ok_and(|age| age < expiry) || newest > now {
            continue;
        }
        if runs_in(&output).iter().any(|run| in_progress(&run.dir)) {
            continue;
        }
        match std::fs::remove_dir_all(&output) {
            Ok(()) => {
                let _ = std::fs::remove_dir(entry.path());
                removed.push(output);
            }
            Err(error) => {
                tracing::warn!(output = %output.display(), %error, "cannot remove expired output");
            }
        }
    }
    removed
}

/// The newest modification time of the output directory and anything
/// directly in it. `None` if even the directory cannot be read.
fn last_activity(output: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(output).ok()?.modified().ok()?;
    for entry in std::fs::read_dir(output).ok()?.flatten() {
        if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
            newest = newest.max(modified);
        }
    }
    Some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;
    use std::io::BufRead;

    fn state(name: &str) -> TestDir {
        let dir =
            std::env::temp_dir().join(format!("ccnm-retention-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(dir).unwrap())
    }

    fn small(runs: usize, session_bytes: u64) -> Limits {
        Limits {
            per_stream: 1024,
            runs,
            session_bytes,
            expiry: Duration::from_secs(3600),
        }
    }

    /// A run that wrote `bytes` to stdout and is still held.
    fn written(output: &Output, bytes: usize) -> Run {
        let (run, mut stdout, _stderr) = output.begin().unwrap();
        stdout.write_all(&vec![b'x'; bytes]).unwrap();
        run
    }

    /// A run that wrote `bytes` and has been finished and let go.
    fn finished(output: &Output, bytes: usize) -> PathBuf {
        let run = written(output, bytes);
        output.finish(&run);
        run.dir.clone()
    }

    fn age(path: &Path, minutes: u64) {
        let when = SystemTime::now() - Duration::from_secs(minutes * 60);
        File::open(path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn a_session_id_cannot_escape_the_retention_directory() {
        // The id names a directory and arrives from the other machine, so
        // it goes through the same filter every state path uses.
        let state = Path::new("/state");
        assert_eq!(
            session_dir(state, "s-1_ok"),
            Path::new("/state/sessions/s-1_ok/output")
        );
        // The property that matters is not "the name looks tidy" but
        // "the name is one segment". `../../etc` filters down to
        // `....etc`, which is an odd directory name and cannot traverse
        // anywhere; `..` and `../..` are all dots and fall back.
        for hostile in ["../../etc", "a/b", "", "/", "..", "../..", "x/../../y"] {
            let dir = session_dir(state, hostile);
            let inside = dir
                .strip_prefix("/state/sessions")
                .unwrap_or_else(|_| panic!("{hostile} escaped to {}", dir.display()));
            let parts: Vec<_> = inside.components().collect();
            assert_eq!(parts.len(), 2, "{hostile} -> {}", dir.display());
            assert!(
                !parts
                    .iter()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
                "{hostile} -> {}",
                dir.display()
            );
        }
        assert!(session_dir(state, &"x".repeat(200)).to_string_lossy().len() < 100);
    }

    /// Past the run limit the **oldest** go, not any. Directories from
    /// before runs were locked (no stdout) count as finished.
    #[test]
    fn too_many_runs_remove_the_oldest_first() {
        let state = state("count");
        let output = Output::new(&state, "s-count");
        fs::create_dir_all(output.dir()).unwrap();
        for n in 0..120u64 {
            let dir = output.dir().join(format!("r-old{n:04}"));
            fs::create_dir(&dir).unwrap();
            age(&dir, 1000 - n);
        }
        let new = finished(&output, 10);
        let mut kept: Vec<String> = fs::read_dir(output.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(kept.len(), 100, "{kept:?}");
        assert!(kept.contains(&new.file_name().unwrap().to_string_lossy().into_owned()));
        let old: Vec<&String> = kept
            .iter()
            .filter(|name| name.starts_with("r-old"))
            .collect();
        assert_eq!(old.len(), 99);
        assert_eq!(old[0], "r-old0021", "the 21 oldest went: {kept:?}");
    }

    /// Past the byte limit the oldest finished runs go until what is left,
    /// the run that just finished included, fits. The run that just
    /// finished is never the one removed, even when it is the oldest.
    #[test]
    fn too_many_bytes_remove_the_oldest_finished_runs_but_never_the_current_one() {
        let state = state("bytes");
        let output = Output::with_limits(&state, "s-bytes", small(100, 100));
        let a = finished(&output, 40);
        age(&a, 30);
        let b = finished(&output, 40);
        age(&b, 20);
        let c = written(&output, 40);
        age(&c.dir, 10);
        output.finish(&c);
        assert!(!a.exists(), "120 B over 100: the oldest goes");
        assert!(b.exists() && c.dir.exists());
        drop(c);

        let d = written(&output, 60);
        age(&d.dir, 60);
        output.finish(&d);
        assert!(d.dir.exists(), "the run that just finished stays");
        assert!(
            !b.exists(),
            "B + C + D = 140 B: B was the oldest of the rest"
        );
        let total: u64 = runs_in(output.dir()).iter().map(|run| run.bytes).sum();
        assert!(total <= 100, "{total} B kept");
    }

    /// A run another server of the session is still writing is never
    /// removed -- whether that server is this process or another one --
    /// and a run whose holder has died is removed like any other.
    #[test]
    fn a_run_still_held_is_kept_until_its_holder_is_gone() {
        let state = state("held");
        let output = Output::with_limits(&state, "s-held", small(100, 100));

        // Held by this process, on another handle.
        let mine = written(&output, 50);
        age(&mine.dir, 50);

        // Running in another process: its marker, and that process's lock.
        let theirs = finished(&output, 50);
        File::create(theirs.join(RUNNING)).unwrap();
        age(&theirs, 40);
        let mut holder = std::process::Command::new("python3")
            .args([
                "-c",
                "import fcntl, sys, time\n\
                 f = open(sys.argv[1])\n\
                 fcntl.flock(f, fcntl.LOCK_EX)\n\
                 print('locked', flush=True)\n\
                 time.sleep(60)",
            ])
            .arg(theirs.join(RUNNING))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("python3 is needed to hold a lock from another process");
        let mut said = String::new();
        std::io::BufReader::new(holder.stdout.take().unwrap())
            .read_line(&mut said)
            .unwrap();
        assert_eq!(said, "locked\n");

        let older = finished(&output, 30);
        age(&older, 30);
        let newest = written(&output, 80);
        output.finish(&newest);
        assert!(mine.dir.exists(), "held here");
        assert!(theirs.exists(), "held by pid {}", holder.id());
        assert!(!older.exists(), "30 + 80 = 110 B of finished runs: it goes");
        drop(newest);

        holder.kill().unwrap();
        holder.wait().unwrap();
        let _ = finished(&output, 1);
        assert!(
            !theirs.exists(),
            "a marker nobody holds is a server that died"
        );
        assert!(mine.dir.exists(), "still held here");
    }

    /// A copy of a run's lock can outlive the run: a thread that forks
    /// while the lock is open hands the child one, held until it execs.
    /// `try_clone` makes the same kind of copy without the race. The run
    /// is finished all the same, because its `running` file is gone.
    #[test]
    fn a_lock_copy_left_behind_does_not_keep_a_finished_run() {
        let state = state("copy");
        let output = Output::with_limits(&state, "s-copy", small(1, u64::MAX));
        let run = written(&output, 5);
        let copy = run._running.try_clone().unwrap();
        let dir = run.dir.clone();
        drop(run);
        assert!(!in_progress(&dir), "the lock is still held by the copy");
        let _ = finished(&output, 5);
        assert!(!dir.exists(), "one run allowed: the finished one goes");
        drop(copy);
    }

    /// The end of an external session removes what that process started
    /// and nothing else: not a run it did not start, not a run still held.
    /// Directories go only once they are empty.
    #[test]
    fn discarding_removes_only_the_runs_this_process_started() {
        let state = state("discard");
        let ours = Output::new(&state, "bridge-x");
        let other = Output::new(&state, "bridge-x");
        let done = finished(&ours, 5);
        let busy = written(&ours, 5);
        let not_ours = finished(&other, 5);

        ours.discard_started();
        assert!(!done.exists());
        assert!(busy.dir.exists(), "still held");
        assert!(not_ours.exists(), "started by another server of the id");
        drop(busy);

        other.discard_started();
        assert!(!not_ours.exists());
        assert!(ours.dir().exists(), "the held run is still in it");

        let alone = Output::new(&state, "bridge-y");
        let _ = finished(&alone, 5);
        alone.discard_started();
        assert!(!crate::paths::session_dir(&state, "bridge-y").exists());
    }

    /// Output is removed after the expiry only when nothing else says keep
    /// it: a newer run, a server still serving the id, a run still held.
    /// Only `output/` goes; an Agent record beside it stays.
    #[test]
    fn output_expires_only_when_nobody_serves_it_and_nothing_is_new() {
        let state = state("sweep");
        let day = 24 * 60;
        let make = |id: &str, minutes: u64| -> PathBuf {
            let output = Output::new(&state, id);
            let run = finished(&output, 5);
            age(&run, minutes);
            age(output.dir(), minutes);
            output.dir().to_path_buf()
        };
        let old = make("old", 8 * day);
        fs::write(
            crate::paths::session_dir(&state, "old").join("session.json"),
            "{}",
        )
        .unwrap();
        let bare = make("bare", 8 * day);
        let fresh = make("fresh", day);
        let served = make("served", 8 * day);
        let busy_output = Output::new(&state, "busy");
        let busy = written(&busy_output, 5);
        age(&busy.dir, 8 * day);
        age(busy_output.dir(), 8 * day);

        let mut removed = sweep(
            &state,
            &["served".to_string()],
            SystemTime::now(),
            Duration::from_secs(7 * day * 60),
        );
        removed.sort();
        assert_eq!(removed, vec![bare.clone(), old.clone()]);
        assert!(
            crate::paths::session_dir(&state, "old")
                .join("session.json")
                .exists()
        );
        assert!(
            !crate::paths::session_dir(&state, "bare").exists(),
            "emptied"
        );
        assert!(fresh.exists() && served.exists() && busy.dir.exists());
    }

    /// `ps` that cannot be run is not "nobody serves anything".
    #[test]
    fn nothing_expires_when_the_process_list_cannot_be_read() {
        let state = state("no-ps");
        let output = Output::new(&state, "old");
        let run = finished(&output, 5);
        age(&run, 30 * 24 * 60);
        age(output.dir(), 30 * 24 * 60);
        let failing = crate::process::FakeRunner::new();
        assert!(sweep_expired(&state, "me", &failing).is_empty());
        assert!(run.exists());
    }
}

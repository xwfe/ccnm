//! What `ccnm rpc` remembers about the sessions it accepted.
//!
//! A record outlives both the client and the server process, which is the
//! whole point: a client that crashes can come back with the session id and
//! still get its result, and a `session.start` that was already accepted is
//! never run a second time.
//!
//! Layout under the state directory:
//!
//! ```text
//! rpc/store.lock                  serialises every read-modify-write below
//! rpc/sessions/<id>.json          one accepted session
//! rpc/start-keys/<hash>.json      start_key -> session, the key kept verbatim
//! rpc/keys/<workspace>/<key>      the pre-P58 layout: read, never written
//! ```
//!
//! **Every change is a read-modify-write under one lock** (P58). Before, the
//! run thread and `session.stop` each held their own copy of a record and
//! wrote it back whole, so whichever wrote last erased the other: a stop
//! flag went back to `false`, or a finished run went back to `stopping` with
//! its result gone (P57 A2/A3). The lock is `flock` on a file, so it holds
//! across threads *and* across the several `ccnm rpc` processes a client may
//! run against one state directory. Readers take no lock: every write lands
//! through a rename, so a reader sees the old document or the new one.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::instance::InstanceRef;
use crate::paths;
use crate::process::{Cmd, ProcessRunner};
use crate::provider::AgentProvider;

/// The public state machine. Deliberately not `session::SessionState`: that
/// one is the Agent side's view and may grow states this protocol has not
/// promised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Stopping,
    Completed,
    Failed,
    /// The server cannot prove what happened. A terminal state, and not one
    /// that turns into `Completed` later.
    Unknown,
}

impl State {
    pub fn terminal(self) -> bool {
        matches!(self, State::Completed | State::Failed | State::Unknown)
    }
}

/// What the Agent produced, once it is over.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Finish {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    /// ccnm's own session id, the one the human CLI addresses. Kept so the
    /// two names for the same run stay tied together instead of becoming
    /// two separate truths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ccnm_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Bounded tail of what the run printed. Never the whole thing.
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub output_total: u64,
    /// Why it could not be started or observed, when that is the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Everything needed to decide whether a `session.start` is a repeat.
///
/// The launch input is kept verbatim rather than hashed: the protocol says
/// "byte for byte identical", and a hash would make a rare collision look
/// like a legitimate reuse -- exactly the case where a second Agent must not
/// be started. The file is 0600 under the state directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub workspace: String,
    pub agent: Option<InstanceRef>,
    pub mode: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub session: String,
    #[serde(flatten)]
    pub launch: Launch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_key: Option<String>,
    pub state: State,
    pub accepted_at: String,
    /// A stop was accepted for this session. Only ever goes from `false` to
    /// `true`: every writer changes the record on disk under the store lock,
    /// so nothing holding an older copy can put it back.
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// The `ccnm rpc` process that took responsibility for this run, and
    /// when it started. Both are needed: a pid on its own gets recycled, and
    /// a recycled pid would make a dead run look alive.
    pub owner_pid: u32,
    pub owner_started: String,
    /// The ccnm session id this server chose for the run before sending it
    /// to the Agent (P58), so a stop can name exactly this run from the
    /// first moment. `None` only in records written before P58, which can
    /// no longer be stopped exactly and are refused instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_session: Option<String>,
    /// The run may have reached the Agent. Set under the store lock right
    /// before sending, so a stop sees either "not sent, and now never will
    /// be" or "sent, go and stop it there" -- never a gap between the two.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dispatched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<Finish>,
}

impl Record {
    /// The state to report, given that the owning process may be gone.
    ///
    /// A record left at `starting`/`running` by a server that died says
    /// nothing about the Agent, which runs on another machine and very
    /// likely finished. Calling that `failed` would be a lie in the one
    /// direction that matters -- it invites a retry of work that may have
    /// already changed the tree -- so it becomes `unknown`.
    pub fn observed_state(&self, owner: OwnerCheck) -> State {
        if self.state.terminal() {
            return self.state;
        }
        match owner {
            OwnerCheck::Alive => self.state,
            OwnerCheck::Gone | OwnerCheck::Unverifiable => State::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerCheck {
    Alive,
    Gone,
    /// `ps` could not be read. Not knowing is not the same as gone, but for
    /// this decision both mean "cannot prove it is still running".
    Unverifiable,
}

/// Is this a handle this server could have issued?
///
/// The protocol's `session_id`: 1..=128 characters, a letter or digit
/// first, then letters, digits, `.`, `_`, `-`. Checked before the handle
/// goes anywhere near a path -- a `../x` or an absolute path used to be
/// joined straight into `sessions/<id>.json` and read from wherever it
/// pointed (P57 A6).
pub fn valid_handle(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub struct Store {
    root: PathBuf,
}

/// The store lock, held for one read-modify-write.
///
/// Unlocked explicitly on drop rather than left to the close: a child forked
/// while the descriptor is open shares the lock until it execs, and a lock
/// that outlives the change it guarded is how the P34 log-lock bug happened.
struct Locked(fs::File);

impl Drop for Locked {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(?error, "could not unlock the rpc store explicitly");
        }
    }
}

impl Store {
    /// `<state>/rpc`, created 0700 on first use.
    pub fn open(state: &Path) -> Result<Self> {
        let root = state.join("rpc");
        create_private(&root.join("sessions"))?;
        create_private(&root.join("start-keys"))?;
        Ok(Store { root })
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(format!("{id}.json"))
    }

    fn lock(&self) -> Result<Locked> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.root.join("store.lock"))?;
        file.lock()?;
        Ok(Locked(file))
    }

    /// The record for `id`, `None` if there is none.
    ///
    /// A handle this server could not have issued is refused before any
    /// path is built from it. Errors name the handle, never the private
    /// path: the protocol keeps absolute paths out of everything it sends.
    pub fn read(&self, id: &str) -> Result<Option<Record>> {
        if !valid_handle(id) {
            return Err(Error::invalid_args(
                "session is not a handle this server issues",
            ));
        }
        let path = self.session_path(id);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => {
                // A symlink or a directory in a 0700 directory only this
                // user writes: not something this server put there.
                tracing::warn!(path = %path.display(), "session record is not a regular file");
                return Err(Error::internal(format!(
                    "session record {id} is not a regular file"
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(
                    Error::internal(format!("cannot read session record {id}")).with_source(e)
                );
            }
        }
        let bytes = fs::read(&path).map_err(|e| {
            Error::internal(format!("cannot read session record {id}")).with_source(e)
        })?;
        serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            tracing::warn!(path = %path.display(), "session record does not parse");
            Error::internal(format!("session record {id} is unreadable")).with_source(e)
        })
    }

    /// Write a brand-new record. Refuses to replace one: a fresh handle
    /// that already exists is a bug, not something to paper over.
    pub fn create(&self, record: &Record) -> Result<()> {
        if !valid_handle(&record.session) {
            return Err(Error::internal("refusing to store an invalid handle"));
        }
        let _locked = self.lock()?;
        let path = self.session_path(&record.session);
        let tmp = write_temp(&path, &encode(record)?)?;
        // A hard link fails if the name exists, which `rename` would not.
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        linked.map_err(|e| {
            Error::internal(format!("cannot create session record {}", record.session))
                .with_source(e)
        })
    }

    /// Change a record as it is on disk now, under the store lock.
    ///
    /// `Ok(None)` when there is no such record. `change` sees the current
    /// document, not a copy someone read earlier, which is the whole fix for
    /// P57 A2/A3.
    pub fn update<T>(&self, id: &str, change: impl FnOnce(&mut Record) -> T) -> Result<Option<T>> {
        let _locked = self.lock()?;
        let Some(mut record) = self.read(id)? else {
            return Ok(None);
        };
        let out = change(&mut record);
        write_atomically(&self.session_path(id), &encode(&record)?)?;
        Ok(Some(out))
    }

    /// Take back a record this call created and never handed out.
    pub fn remove_unpublished(&self, id: &str) {
        if valid_handle(id) {
            let _ = fs::remove_file(self.session_path(id));
        }
    }

    /// Point a start key at a session, unless the key already points
    /// somewhere.
    ///
    /// The key is kept verbatim and compared byte for byte. The file name
    /// is only a hash bucket: the old layout used `safe_name` for it, which
    /// drops characters and cuts at 64, so `任务-一` and `任务-二` were one
    /// key and the second task silently reused the first one's session
    /// (P57 A5).
    pub fn claim_key(&self, workspace: &str, key: &str, session: &str) -> Result<KeyClaim> {
        let _locked = self.lock()?;
        let path = self.key_path(workspace, key);
        let mut bucket: KeyBucket = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::internal("the start_key index is unreadable").with_source(e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => KeyBucket::default(),
            Err(e) => return Err(Error::internal("cannot read the start_key index").with_source(e)),
        };
        if let Some(entry) = bucket
            .entries
            .iter()
            .find(|entry| entry.workspace == workspace && entry.start_key == key)
        {
            return Ok(KeyClaim::Held(Some(entry.session.clone())));
        }
        if let Some(held) = self.legacy_key(workspace, key)? {
            return Ok(KeyClaim::Held(held));
        }
        bucket.entries.push(KeyEntry {
            workspace: workspace.to_string(),
            start_key: key.to_string(),
            session: session.to_string(),
        });
        write_atomically(&path, &encode(&bucket)?)?;
        Ok(KeyClaim::Taken)
    }

    fn key_path(&self, workspace: &str, key: &str) -> PathBuf {
        // Length-prefixed so no (workspace, key) pair can spell another.
        let input = format!("{}:{workspace}\n{key}", workspace.len());
        self.root
            .join("start-keys")
            .join(format!("{:016x}.json", paths::fnv1a(input.as_bytes())))
    }

    /// Look the key up in the layout written before P58.
    ///
    /// Read, never written, so that upgrading cannot turn an accepted task
    /// back into a new one. That layout mapped several keys onto one file,
    /// so the file only counts when the record it names carries this very
    /// key. `Some(None)`: the file exists but names no session -- the old
    /// server died between creating it and writing it -- so which session
    /// it meant cannot be known.
    fn legacy_key(&self, workspace: &str, key: &str) -> Result<Option<Option<String>>> {
        let path = self
            .root
            .join("keys")
            .join(paths::safe_name(workspace, "workspace"))
            .join(paths::safe_name(key, "key"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::internal("cannot read an old start_key").with_source(e)),
        };
        let id = text.trim();
        if !valid_handle(id) {
            return Ok(Some(None));
        }
        match self.read(id)? {
            Some(record)
                if record.start_key.as_deref() == Some(key)
                    && record.launch.workspace == workspace =>
            {
                Ok(Some(Some(id.to_string())))
            }
            // Another key whose filtered name happened to be the same.
            Some(_) => Ok(None),
            // The record is gone. It may have been this key's; a second
            // start on a guess is the one thing a start key must prevent.
            None => Ok(Some(Some(id.to_string()))),
        }
    }

    /// Replace a record regardless of what is there. Tests only: they put
    /// records into states a live server would pass through too quickly.
    #[cfg(test)]
    pub(crate) fn write(&self, record: &Record) -> Result<()> {
        let _locked = self.lock()?;
        write_atomically(&self.session_path(&record.session), &encode(record)?)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyClaim {
    /// This call now owns the key.
    Taken,
    /// Someone else already owns it: their session id, or `None` when an
    /// old half-written entry makes that impossible to know.
    Held(Option<String>),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyBucket {
    entries: Vec<KeyEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyEntry {
    workspace: String,
    start_key: String,
    session: String,
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value)
        .map_err(|e| Error::internal("cannot encode an rpc record").with_source(e))
}

fn create_private(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// A temporary file next to `path`, private from its first byte.
///
/// The name is unique per process and call. It used to be a fixed
/// `<id>.tmp`, which two writers of one record would truncate and rename
/// under each other.
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("record");
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        // Durable before it becomes visible under the real name. The
        // directory itself is not fsynced: a power cut may lose the newest
        // rename, never produce half a record.
        file.sync_all()
    })();
    match written {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Write through a temporary file and a rename.
///
/// A reader polling `session.status` must never see a half-written record;
/// rename within a directory is atomic, a truncating write is not.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = write_temp(path, bytes)?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::from(e)
    })
}

/// When did this process start, as `ps` reports it?
///
/// Recorded alongside the pid so a recycled pid cannot make a dead server
/// look alive. The exact format does not matter; only that it is stable for
/// one process and different for the next one to take that number.
pub fn process_started(runner: &dyn ProcessRunner, pid: u32) -> Option<String> {
    let out = runner
        .run(&Cmd::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "lstart="]))
        .ok()?;
    if !out.success() {
        return None;
    }
    let text = out.stdout_lossy().trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Is the process that owns a record still that same process?
pub fn owner_check(runner: &dyn ProcessRunner, pid: u32, started: &str) -> OwnerCheck {
    let out = match runner.run(&Cmd::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "lstart="]))
    {
        Ok(out) => out,
        // `ps` itself failing is not evidence of anything about the pid.
        Err(_) => return OwnerCheck::Unverifiable,
    };
    let text = out.stdout_lossy().trim().to_string();
    if text.is_empty() {
        // `ps -p` exits non-zero with no rows when the pid is gone. That is
        // a real answer, not a failure to look.
        return OwnerCheck::Gone;
    }
    if text == started {
        OwnerCheck::Alive
    } else {
        // The number is in use by something that started at a different
        // time: our process is gone and the pid was recycled.
        OwnerCheck::Gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, SystemRunner};
    use ccnm_testdir::TestDir;

    fn temp(test: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-rpcstore-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    fn record(id: &str) -> Record {
        Record {
            session: id.to_string(),
            launch: Launch {
                workspace: "demo".into(),
                agent: None,
                mode: "print".into(),
                prompt: "run the tests".into(),
            },
            start_key: None,
            state: State::Starting,
            accepted_at: "2026-09-10T12:00:00+09:00".into(),
            stop_requested: false,
            timeout_ms: None,
            owner_pid: 4242,
            owner_started: "Thu Sep 10 12:00:00 2026".into(),
            managed_session: Some("0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into()),
            dispatched: false,
            finish: None,
        }
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let dir = temp("roundtrip");
        let store = Store::open(&dir).unwrap();
        let rec = record("s-1");
        store.create(&rec).unwrap();
        assert_eq!(store.read("s-1").unwrap().unwrap(), rec);

        store
            .update("s-1", |r| {
                r.state = State::Completed;
                r.finish = Some(Finish {
                    exit_code: Some(0),
                    duration_ms: 12,
                    ccnm_session: Some("uuid-here".into()),
                    ..Finish::default()
                });
            })
            .unwrap()
            .unwrap();
        let back = store.read("s-1").unwrap().unwrap();
        assert_eq!(back.state, State::Completed);
        assert_eq!(
            back.finish.unwrap().ccnm_session.as_deref(),
            Some("uuid-here")
        );
    }

    #[test]
    fn a_record_written_before_p58_still_reads() {
        let dir = temp("legacy-record");
        let store = Store::open(&dir).unwrap();
        fs::write(
            dir.join("rpc/sessions/s-old.json"),
            r#"{"session":"s-old","workspace":"demo","agent":null,"mode":"print","prompt":"p","state":"running","accepted_at":"x","owner_pid":1,"owner_started":"y"}"#,
        )
        .unwrap();
        let old = store.read("s-old").unwrap().unwrap();
        assert_eq!(old.managed_session, None);
        assert!(!old.dispatched);
    }

    #[test]
    fn create_refuses_to_replace_a_record() {
        let dir = temp("create-once");
        let store = Store::open(&dir).unwrap();
        store.create(&record("s-1")).unwrap();
        assert!(store.create(&record("s-1")).is_err());
    }

    #[test]
    fn updating_a_missing_record_changes_nothing() {
        let dir = temp("update-missing");
        let store = Store::open(&dir).unwrap();
        assert_eq!(
            store.update("s-nope", |r| r.stop_requested = true).unwrap(),
            None
        );
        assert!(!dir.join("rpc/sessions/s-nope.json").exists());
    }

    /// CT-03: 64 writers, each appending one character to the same record
    /// through `update`. With read-modify-write under the lock every append
    /// survives; with each writer holding its own copy (the pre-P58 code)
    /// most of them are lost. Interleaving varies, the result may not.
    #[test]
    fn concurrent_updates_never_lose_a_write() {
        let dir = temp("concurrent");
        let store = std::sync::Arc::new(Store::open(&dir).unwrap());
        let mut base = record("s-1");
        base.launch.prompt.clear();
        store.create(&base).unwrap();
        let writers: Vec<_> = (0..64)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .update("s-1", |r| r.launch.prompt.push('x'))
                        .unwrap()
                        .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(store.read("s-1").unwrap().unwrap().launch.prompt.len(), 64);
        let stray: Vec<_> = fs::read_dir(dir.join("rpc/sessions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name != "s-1.json")
            .collect();
        assert!(stray.is_empty(), "temporary files left behind: {stray:?}");
    }

    #[test]
    fn records_and_directories_are_private() {
        let dir = temp("perms");
        let store = Store::open(&dir).unwrap();
        store.create(&record("s-1")).unwrap();
        store.update("s-1", |r| r.stop_requested = true).unwrap();
        let file = fs::metadata(dir.join("rpc/sessions/s-1.json")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let sessions = fs::metadata(dir.join("rpc/sessions")).unwrap();
        assert_eq!(sessions.permissions().mode() & 0o777, 0o700);
        store.claim_key("demo", "k", "s-1").unwrap();
        for entry in fs::read_dir(dir.join("rpc/start-keys")).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// CT-06: a handle is refused before any path is built from it.
    #[test]
    fn handles_outside_the_issued_shape_never_reach_the_filesystem() {
        let dir = temp("handles");
        let store = Store::open(&dir).unwrap();
        fs::write(dir.join("rpc/outside.json"), "{}").unwrap();
        for bad in [
            "../outside",
            "/etc/passwd",
            "a/b",
            "",
            ".hidden",
            "-x",
            "a\0b",
            &"x".repeat(129),
        ] {
            let err = store.read(bad).unwrap_err();
            assert_eq!(err.code(), crate::ErrorCode::InvalidArgs, "{bad:?}");
        }
        for good in [
            "s-0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d",
            "s-nope",
            "a",
            &"x".repeat(128),
        ] {
            assert!(store.read(good).unwrap().is_none(), "{good}");
        }
    }

    #[test]
    fn a_symlinked_record_is_refused_without_naming_the_path() {
        let dir = temp("symlink");
        let store = Store::open(&dir).unwrap();
        let target = dir.join("planted.json");
        fs::write(&target, serde_json::to_vec(&record("planted")).unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("rpc/sessions/s-link.json")).unwrap();
        let err = store.read("s-link").unwrap_err();
        assert!(!err.message().contains(&*dir.to_string_lossy()), "{err}");
    }

    #[test]
    fn a_start_key_can_only_be_claimed_once() {
        let dir = temp("claim");
        let store = Store::open(&dir).unwrap();
        assert_eq!(
            store.claim_key("demo", "task-1", "s-1").unwrap(),
            KeyClaim::Taken
        );
        assert_eq!(
            store.claim_key("demo", "task-1", "s-2").unwrap(),
            KeyClaim::Held(Some("s-1".to_string()))
        );
        // Different workspace, same key: no collision.
        assert_eq!(
            store.claim_key("other", "task-1", "s-3").unwrap(),
            KeyClaim::Taken
        );
    }

    /// CT-04: keys the old layout merged into one file stay apart.
    #[test]
    fn keys_are_compared_verbatim() {
        let dir = temp("verbatim");
        let store = Store::open(&dir).unwrap();
        let long = "k".repeat(64);
        for (n, key) in [
            "任务-一",
            "任务-二",
            "a/b",
            "ab",
            &format!("{long}x"),
            &format!("{long}y"),
            "../../etc/passwd",
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                store.claim_key("demo", key, &format!("s-{n}")).unwrap(),
                KeyClaim::Taken,
                "{key}"
            );
        }
        assert_eq!(
            store.claim_key("demo", "任务-二", "s-9").unwrap(),
            KeyClaim::Held(Some("s-1".to_string()))
        );
        // Nothing escaped the index directory, whatever the key spelled.
        assert!(!dir.join("etc").exists());
    }

    /// Two entries that land in one bucket are kept apart by the verbatim
    /// comparison, which is the only thing the file name is trusted for.
    #[test]
    fn a_shared_bucket_keeps_its_entries_apart() {
        let dir = temp("bucket");
        let store = Store::open(&dir).unwrap();
        let path = store.key_path("demo", "wanted");
        fs::write(
            &path,
            r#"{"entries":[{"workspace":"demo","start_key":"someone else","session":"s-other"}]}"#,
        )
        .unwrap();
        assert_eq!(
            store.claim_key("demo", "wanted", "s-mine").unwrap(),
            KeyClaim::Taken
        );
        assert_eq!(
            store.claim_key("demo", "wanted", "s-late").unwrap(),
            KeyClaim::Held(Some("s-mine".to_string()))
        );
        let bucket: KeyBucket = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            bucket.entries.len(),
            2,
            "the other entry is kept, not replaced"
        );
    }

    fn legacy(dir: &Path, key: &str, content: &str) {
        let keys = dir.join("rpc/keys/demo");
        fs::create_dir_all(&keys).unwrap();
        fs::write(keys.join(paths::safe_name(key, "key")), content).unwrap();
    }

    /// CT-05 and the upgrade: an old key still holds its session, an old
    /// file that some *other* key mapped to does not, and a half-written one
    /// is "cannot tell", never an empty handle.
    #[test]
    fn keys_written_before_p58_are_honoured_but_not_guessed() {
        let dir = temp("legacy-keys");
        let store = Store::open(&dir).unwrap();
        let mut old = record("s-old");
        old.start_key = Some("任务-一".into());
        store.create(&old).unwrap();
        legacy(&dir, "任务-一", "s-old\n");
        assert_eq!(
            store.claim_key("demo", "任务-一", "s-new").unwrap(),
            KeyClaim::Held(Some("s-old".to_string()))
        );
        // Same filtered file name, different key: not ours.
        assert_eq!(
            store.claim_key("demo", "任务-二", "s-new").unwrap(),
            KeyClaim::Taken
        );

        legacy(&dir, "k-empty", "");
        assert_eq!(
            store.claim_key("demo", "k-empty", "s-x").unwrap(),
            KeyClaim::Held(None)
        );
        legacy(&dir, "k-gone", "s-gone");
        assert_eq!(
            store.claim_key("demo", "k-gone", "s-x").unwrap(),
            KeyClaim::Held(Some("s-gone".to_string()))
        );
    }

    #[test]
    fn a_dead_owner_turns_a_running_record_into_unknown() {
        let mut rec = record("s-1");
        rec.state = State::Running;
        assert_eq!(rec.observed_state(OwnerCheck::Alive), State::Running);
        // Not `failed`: the Agent runs on another machine and probably
        // finished. Reporting failure here invites a retry of work that may
        // already have changed the tree.
        assert_eq!(rec.observed_state(OwnerCheck::Gone), State::Unknown);
        assert_eq!(rec.observed_state(OwnerCheck::Unverifiable), State::Unknown);
    }

    #[test]
    fn a_terminal_record_ignores_the_owner_entirely() {
        for state in [State::Completed, State::Failed, State::Unknown] {
            let mut rec = record("s-1");
            rec.state = state;
            assert_eq!(rec.observed_state(OwnerCheck::Gone), state);
        }
    }

    #[test]
    fn this_process_is_alive_and_a_free_pid_is_not() {
        let runner = SystemRunner;
        let pid = std::process::id();
        let started = process_started(&runner, pid).expect("ps must report this process");
        assert_eq!(owner_check(&runner, pid, &started), OwnerCheck::Alive);
        // A different start time on the same pid means the number was
        // recycled, which is the case a bare pid check gets wrong.
        assert_eq!(
            owner_check(&runner, pid, "Thu Jan  1 00:00:00 1970"),
            OwnerCheck::Gone
        );
    }

    struct Broken;
    impl ProcessRunner for Broken {
        fn run(&self, _cmd: &Cmd) -> Result<Output> {
            Err(Error::internal("ps is not available"))
        }
    }

    #[test]
    fn an_unreadable_ps_is_unverifiable_not_gone() {
        assert_eq!(
            owner_check(&Broken, 1, "whenever"),
            OwnerCheck::Unverifiable
        );
        assert_eq!(process_started(&Broken, 1), None);
    }
}

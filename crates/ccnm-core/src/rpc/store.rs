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
//! rpc/sessions/<id>.json          one accepted session
//! rpc/keys/<workspace>/<key>      the id that start_key belongs to
//! ```
//!
//! Every write goes through a temporary file and a rename, so a reader
//! polling `session.status` never sees half a document.

use std::fs;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// The `ccnm rpc` process that took responsibility for this run, and
    /// when it started. Both are needed: a pid on its own gets recycled, and
    /// a recycled pid would make a dead run look alive.
    pub owner_pid: u32,
    pub owner_started: String,
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

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `<state>/rpc`, created 0700 on first use.
    pub fn open(state: &Path) -> Result<Self> {
        let root = state.join("rpc");
        create_private(&root.join("sessions"))?;
        create_private(&root.join("keys"))?;
        Ok(Store { root })
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(format!("{id}.json"))
    }

    fn key_path(&self, workspace: &str, key: &str) -> PathBuf {
        self.root
            .join("keys")
            .join(paths::safe_name(workspace, "workspace"))
            .join(paths::safe_name(key, "key"))
    }

    pub fn read(&self, id: &str) -> Result<Option<Record>> {
        let path = self.session_path(id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                Error::internal(format!("cannot parse {}", path.display())).with_source(e)
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                Err(Error::internal(format!("cannot read {}", path.display())).with_source(e))
            }
        }
    }

    pub fn write(&self, record: &Record) -> Result<()> {
        let path = self.session_path(&record.session);
        let json = serde_json::to_vec_pretty(record)
            .map_err(|e| Error::internal("cannot encode session record").with_source(e))?;
        write_atomically(&path, &json)
    }

    /// Point a start key at a session, refusing to move one that exists.
    ///
    /// `create_new` is the whole mechanism: two `session.start` calls racing
    /// with the same key means exactly one creates the link, and the loser
    /// reads it back and reuses that session instead of starting a second
    /// Agent.
    pub fn claim_key(&self, workspace: &str, key: &str, session: &str) -> Result<KeyClaim> {
        let path = self.key_path(workspace, key);
        if let Some(parent) = path.parent() {
            create_private(parent)?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(session.as_bytes())?;
                Ok(KeyClaim::Taken)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::read_to_string(&path)?;
                Ok(KeyClaim::Held(existing.trim().to_string()))
            }
            Err(e) => {
                Err(Error::internal(format!("cannot claim {}", path.display())).with_source(e))
            }
        }
    }

    /// Undo a claim this call made and then could not use.
    pub fn release_key(&self, workspace: &str, key: &str) {
        let _ = fs::remove_file(self.key_path(workspace, key));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyClaim {
    /// This call now owns the key.
    Taken,
    /// Someone else already owns it; here is their session id.
    Held(String),
}

fn create_private(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Write through a temporary file and a rename.
///
/// A reader polling `session.status` must never see a half-written record;
/// rename within a directory is atomic, a truncating write is not.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    Ok(())
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

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-rpcstore-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
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
            finish: None,
        }
    }

    #[test]
    fn a_record_survives_a_round_trip() {
        let store = Store::open(&temp("roundtrip")).unwrap();
        let mut rec = record("s-1");
        store.write(&rec).unwrap();
        assert_eq!(store.read("s-1").unwrap().unwrap(), rec);

        rec.state = State::Completed;
        rec.finish = Some(Finish {
            exit_code: Some(0),
            duration_ms: 12,
            ccnm_session: Some("uuid-here".into()),
            ..Finish::default()
        });
        store.write(&rec).unwrap();
        assert_eq!(store.read("s-1").unwrap().unwrap(), rec);
    }

    #[test]
    fn an_unknown_session_reads_as_none_not_an_error() {
        let store = Store::open(&temp("missing")).unwrap();
        assert!(store.read("s-nope").unwrap().is_none());
    }

    #[test]
    fn records_and_directories_are_private() {
        let dir = temp("perms");
        let store = Store::open(&dir).unwrap();
        store.write(&record("s-1")).unwrap();
        let file = fs::metadata(dir.join("rpc/sessions/s-1.json")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let sessions = fs::metadata(dir.join("rpc/sessions")).unwrap();
        assert_eq!(sessions.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_start_key_can_only_be_claimed_once() {
        let store = Store::open(&temp("claim")).unwrap();
        assert_eq!(
            store.claim_key("demo", "task-1", "s-1").unwrap(),
            KeyClaim::Taken
        );
        assert_eq!(
            store.claim_key("demo", "task-1", "s-2").unwrap(),
            KeyClaim::Held("s-1".to_string())
        );
        // Different workspace, same key: no collision.
        assert_eq!(
            store.claim_key("other", "task-1", "s-3").unwrap(),
            KeyClaim::Taken
        );
    }

    #[test]
    fn releasing_a_key_lets_the_next_call_take_it() {
        let store = Store::open(&temp("release")).unwrap();
        store.claim_key("demo", "task-1", "s-1").unwrap();
        store.release_key("demo", "task-1");
        assert_eq!(
            store.claim_key("demo", "task-1", "s-2").unwrap(),
            KeyClaim::Taken
        );
    }

    #[test]
    fn a_key_with_path_characters_cannot_escape_its_directory() {
        let dir = temp("escape");
        let store = Store::open(&dir).unwrap();
        store.claim_key("demo", "../../etc/passwd", "s-1").unwrap();
        assert!(!dir.join("rpc/keys/demo/../../etc/passwd").exists());
        let entries: Vec<_> = fs::read_dir(dir.join("rpc/keys/demo"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].contains('/'), "{entries:?}");
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

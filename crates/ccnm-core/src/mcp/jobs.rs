//! The commands a server has running, and how they are stopped.
//!
//! Three things stop a command they are not themselves waiting for: a client
//! that cancels the call running it (Claude Code 2.1.273's MCP client sends
//! `notifications/cancelled` whenever the abort signal it gives each tool call
//! fires), `stop_command` naming a background command, and the server when
//! its session ends.
//!
//! The last one is what makes a background command safe to offer. Before
//! P41 a server whose client went away sat waiting for its commands to end
//! by themselves -- measured, an 8 s command kept the server 8.0 s past the
//! disconnect, holding the workspace's write guard, and 600 s is allowed. A
//! background command has no end of its own, so every command this server
//! started is stopped, and waited for, before the server lets go of
//! anything.
//!
//! A background command's `status` file is how `read_output` and
//! `stop_command` know what became of it. It lives in the run's directory,
//! beside the output, and like the output it outlives this process: a server
//! that ended without writing the final status (killed with `SIGKILL`) leaves
//! a `running` status with nobody holding the run's lock, which is reported
//! as exactly that.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::output;
use crate::mcp::retention;
use crate::process::{Captured, Stopper};

/// Background commands one server runs at once. Each can hold up to 128 MiB
/// of output that the session's byte limit cannot reclaim while it runs
/// (P31: a run in progress is never removed), so this bounds that at 1 GiB.
/// A dev server, a watcher and a test run is three.
pub const MAX_BACKGROUND: usize = 8;
/// How long a stopped command has between `TERM` and `KILL`.
pub const STOP_GRACE: Duration = Duration::from_secs(2);
/// How long stopping waits in all before giving up on a command no signal
/// reaches (something that left the process group and holds a pipe).
pub const STOP_GIVE_UP: Duration = Duration::from_secs(10);

/// Why a command was stopped rather than left to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The client cancelled the call that was running it.
    Cancelled,
    /// `stop_command` named it.
    StopCommand,
    /// The server that ran it was ending.
    SessionEnded,
}

/// The way to stop one command, from before it starts until it has ended.
///
/// Before the child exists a stop is remembered, and the command is then
/// stopped the moment it is attached -- or never started, if the caller
/// checks first. The first reason given is the one kept.
#[derive(Default)]
pub struct Stop {
    state: Mutex<StopState>,
}

#[derive(Default)]
struct StopState {
    stopper: Option<Stopper>,
    reason: Option<StopReason>,
}

impl Stop {
    fn lock(&self) -> MutexGuard<'_, StopState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The child is running. If a stop was asked for already, it is
    /// stopped now.
    pub fn attach(&self, stopper: Stopper) {
        let asked = {
            let mut state = self.lock();
            state.stopper = Some(stopper.clone());
            state.reason.is_some()
        };
        if asked {
            stopper.stop(STOP_GRACE, STOP_GIVE_UP);
        }
    }

    /// Stop the command, blocking until it has ended or stopping gives up.
    /// Returns false only when it gave up.
    pub fn stop(&self, reason: StopReason) -> bool {
        let stopper = {
            let mut state = self.lock();
            state.reason.get_or_insert(reason);
            state.stopper.clone()
        };
        stopper.is_none_or(|stopper| stopper.stop(STOP_GRACE, STOP_GIVE_UP))
    }

    pub fn reason(&self) -> Option<StopReason> {
        self.lock().reason
    }
}

/// Every command one server has running.
pub struct Jobs {
    state: Mutex<Registry>,
    /// Signalled whenever a command leaves the registry.
    left: Condvar,
}

#[derive(Default)]
struct Registry {
    /// The server is ending: nothing new starts.
    closed: bool,
    next: u64,
    running: Vec<Entry>,
}

struct Entry {
    id: u64,
    reference: Option<String>,
    background: bool,
    stop: Arc<Stop>,
}

impl Entry {
    /// How a person finds this command again. The `output_ref` once it has
    /// one -- its command line is in that run's `status` file -- and before
    /// that the only handle there is.
    fn name(&self) -> String {
        self.reference
            .clone()
            .unwrap_or_else(|| format!("run #{}", self.id))
    }
}

/// A command's place in the registry, given up when it is dropped. The
/// thread that waits for the command holds it, so a command leaves the
/// registry only once its result -- or its final status -- is written.
pub struct Ticket {
    jobs: Arc<Jobs>,
    id: u64,
}

impl Ticket {
    /// The run this command writes to, once it has one.
    pub fn name(&self, reference: &str) {
        let mut registry = self.jobs.lock();
        if let Some(entry) = registry.running.iter_mut().find(|e| e.id == self.id) {
            entry.reference = Some(reference.to_string());
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.jobs.lock().running.retain(|entry| entry.id != self.id);
        self.jobs.left.notify_all();
    }
}

impl Jobs {
    pub fn new() -> Arc<Jobs> {
        Arc::new(Jobs {
            state: Mutex::new(Registry::default()),
            left: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Let a command start. Refused once the server is ending, and for a
    /// background command when [`MAX_BACKGROUND`] are already running.
    pub fn admit(self: &Arc<Self>, background: bool, stop: Arc<Stop>) -> Result<Ticket> {
        let mut registry = self.lock();
        if registry.closed {
            return Err(Error::new(
                ErrorCode::NotReady,
                "this session is ending, so no new command is started",
            ));
        }
        if background {
            let running: Vec<&str> = registry
                .running
                .iter()
                .filter(|entry| entry.background)
                .map(|entry| entry.reference.as_deref().unwrap_or("(starting)"))
                .collect();
            if running.len() >= MAX_BACKGROUND {
                return Err(Error::invalid_args(format!(
                    "{MAX_BACKGROUND} commands are already running in the background ({}); stop one with stop_command, or wait for one to finish with read_output wait_ms, before starting another",
                    running.join(", ")
                )));
            }
        }
        registry.next += 1;
        let id = registry.next;
        registry.running.push(Entry {
            id,
            reference: None,
            background,
            stop,
        });
        Ok(Ticket {
            jobs: Arc::clone(self),
            id,
        })
    }

    /// The stop handle of a background command still running here.
    pub fn background(&self, reference: &str) -> Option<Arc<Stop>> {
        self.lock()
            .running
            .iter()
            .find(|entry| entry.background && entry.reference.as_deref() == Some(reference))
            .map(|entry| Arc::clone(&entry.stop))
    }

    /// Wait until `reference` has left the registry, at most `limit`.
    pub fn wait_gone(&self, reference: &str, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut registry = self.lock();
        loop {
            if !registry
                .running
                .iter()
                .any(|entry| entry.reference.as_deref() == Some(reference))
            {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            registry = self
                .left
                .wait_timeout(registry, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Stop every command, foreground and background, and wait until each
    /// has been waited for. Nothing new starts afterwards.
    ///
    /// **Returns the ones it gave up on**, empty when every command ended.
    /// One no signal reaches is given up on after [`STOP_GIVE_UP`] and the
    /// server ends anyway -- but the caller has to know, because such a
    /// command is still free to write the working tree. The write guard is
    /// not handed on when this comes back non-empty
    /// ([`WriteGuard::abandon`](crate::mcp::write_guard::WriteGuard::abandon)).
    ///
    /// The commands are stopped side by side, so ending a session with
    /// eight of them takes the grace period once, not eight times.
    pub fn stop_all(&self) -> Vec<String> {
        let stops: Vec<Arc<Stop>> = {
            let mut registry = self.lock();
            registry.closed = true;
            registry
                .running
                .iter()
                .map(|entry| Arc::clone(&entry.stop))
                .collect()
        };
        if stops.is_empty() {
            return Vec::new();
        }
        tracing::info!(
            commands = stops.len(),
            "session ending; stopping its commands"
        );
        let stoppers: Vec<_> = stops
            .into_iter()
            .map(|stop| std::thread::spawn(move || stop.stop(StopReason::SessionEnded)))
            .collect();
        for stopper in stoppers {
            let _ = stopper.join();
        }
        // Stopped is not yet gone: each waiter still writes its result.
        let deadline = Instant::now() + STOP_GIVE_UP;
        let mut registry = self.lock();
        while !registry.running.is_empty() {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                let names: Vec<String> = registry.running.iter().map(Entry::name).collect();
                tracing::warn!(
                    commands = names.len(),
                    "commands still running after the session was stopped; leaving them"
                );
                return names;
            };
            registry = self
                .left
                .wait_timeout(registry, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Vec::new()
    }
}

/// What became of a background command, kept beside its output as
/// `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The line `exec_command` echoed back.
    pub command: String,
    /// Milliseconds since the Unix epoch.
    pub started_ms: u64,
    /// Milliseconds after which it is killed; absent when there is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Absent while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// `None` when a signal ended it.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<StopReason>,
    pub duration_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

const STATUS: &str = "status";

impl Status {
    pub fn started(command: &str, timeout_ms: Option<u64>) -> Status {
        let started_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        Status {
            command: command.to_string(),
            started_ms,
            timeout_ms,
            ended: None,
        }
    }

    pub fn end(&mut self, captured: &Captured, reason: Option<StopReason>) {
        self.ended = Some(Ended {
            exit_code: captured.exit_code,
            timed_out: captured.timed_out,
            stopped: reason.filter(|_| captured.stopped),
            duration_ms: u64::try_from(captured.duration.as_millis()).unwrap_or(u64::MAX),
            stdout_bytes: captured.stdout_bytes,
            stderr_bytes: captured.stderr_bytes,
        });
    }

    /// Written whole under another name and renamed, so a reader never sees
    /// half of it.
    pub fn write(&self, run: &Path) -> Result<()> {
        let text = serde_json::to_vec(self)
            .map_err(|e| Error::internal("cannot encode a command's status").with_source(e))?;
        let staging = run.join(".status");
        std::fs::write(&staging, text)
            .and_then(|()| std::fs::rename(&staging, run.join(STATUS)))
            .map_err(|e| Error::internal("cannot record a command's status").with_source(e))
    }

    /// `None` for a run that was not started in the background, or whose
    /// status cannot be read.
    pub fn read(run: &Path) -> Option<Status> {
        serde_json::from_slice(&std::fs::read(run.join(STATUS)).ok()?).ok()
    }

    /// One line on where the command is: running, or how it ended.
    /// `running` is whether a server still holds the run.
    pub fn describe(&self, running: bool) -> String {
        let Some(ended) = &self.ended else {
            if running {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64);
                return format!(
                    "running for {}",
                    seconds(now.saturating_sub(self.started_ms))
                );
            }
            return "no longer running, and its exit status is unknown: the server running it ended without recording one".to_string();
        };
        let after = seconds(ended.duration_ms);
        match (ended.stopped, ended.timed_out, ended.exit_code) {
            (Some(StopReason::StopCommand), _, _) => {
                format!("stopped by stop_command after {after}")
            }
            (Some(StopReason::SessionEnded), _, _) => {
                format!("stopped when its session ended, after {after}")
            }
            (Some(StopReason::Cancelled), _, _) => {
                format!("stopped because the call that started it was cancelled, after {after}")
            }
            (None, true, _) => format!("killed on its timeout after {after}"),
            (None, false, Some(code)) => format!("exited {code} after {after}"),
            (None, false, None) => format!("killed by a signal after {after}"),
        }
    }
}

fn seconds(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

/// Arguments of `stop_command`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct StopCommandArgs {
    /// The output_ref exec_command returned for a command started with
    /// run_in_background.
    pub output_ref: String,
}

/// Stop a background command this server is running, and say how it ended.
/// A command that has already ended is not an error: its status is the
/// answer.
pub fn stop_command(jobs: &Jobs, session_dir: &Path, args: &StopCommandArgs) -> Result<String> {
    let reference = output::validate_ref(&args.output_ref)?;
    let run = session_dir.join(&reference);
    let stopped_here = match jobs.background(&reference) {
        Some(stop) => {
            stop.stop(StopReason::StopCommand);
            jobs.wait_gone(&reference, STOP_GIVE_UP);
            true
        }
        None => false,
    };
    let Some(status) = Status::read(&run) else {
        return Err(Error::invalid_args(if run.is_dir() {
            format!(
                "{reference} was not started with run_in_background; it had already finished when its output_ref was returned"
            )
        } else {
            format!(
                "no output kept for {reference}; a command's output is kept for a while, not forever"
            )
        }));
    };
    let running = retention::in_progress(&run);
    if status.ended.is_none() && running && !stopped_here {
        return Err(Error::invalid_args(format!(
            "{reference} is running in another server of this session, which stops it when that server ends; stop_command only reaches commands started through this connection"
        )));
    }
    let mut text = format!("$ {}\n{}", status.command, status.describe(running));
    if let Some(ended) = &status.ended {
        text.push_str(&format!(
            ", {} B stdout, {} B stderr",
            ended.stdout_bytes, ended.stderr_bytes
        ));
    }
    if !stopped_here && status.ended.is_some() {
        text.push_str("\n[it had already ended; nothing was stopped]");
    }
    text.push_str(&format!(
        "\n[read_output with output_ref {reference} for what it wrote]"
    ));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Cmd, spawn_captured};

    fn sleeper() -> crate::process::Started {
        spawn_captured(
            &Cmd::new("sleep").arg("30").timeout(Duration::MAX),
            std::io::sink(),
            std::io::sink(),
        )
        .unwrap()
    }

    /// A stop that arrives before the child exists is kept and applied the
    /// moment it is attached.
    #[test]
    fn a_stop_before_the_start_stops_it_on_arrival() {
        let stop = Stop::default();
        assert!(stop.stop(StopReason::Cancelled), "nothing to wait for yet");
        let started = sleeper();
        let stopper = started.stopper();
        let waiter = std::thread::spawn(move || started.wait().unwrap());
        let clock = Instant::now();
        stop.attach(stopper);
        let captured = waiter.join().unwrap();
        assert!(captured.stopped);
        assert!(clock.elapsed() < Duration::from_secs(5));
        // The first reason is the one kept.
        stop.stop(StopReason::SessionEnded);
        assert_eq!(stop.reason(), Some(StopReason::Cancelled));
    }

    #[test]
    fn background_commands_are_limited_and_nothing_starts_once_closed() {
        let jobs = Jobs::new();
        let mut tickets: Vec<Ticket> = (0..MAX_BACKGROUND)
            .map(|n| {
                let ticket = jobs.admit(true, Arc::default()).unwrap();
                ticket.name(&format!("r-{n:016x}"));
                ticket
            })
            .collect();
        let refused = jobs.admit(true, Arc::default()).err().unwrap();
        assert_eq!(refused.code(), ErrorCode::InvalidArgs);
        assert!(
            refused.message().contains("r-0000000000000007"),
            "{refused}"
        );
        assert!(refused.message().contains("stop_command"), "{refused}");
        // Foreground commands do not count against it.
        let foreground = jobs.admit(false, Arc::default()).unwrap();
        drop(tickets.remove(0));
        let again = jobs.admit(true, Arc::default()).unwrap();
        drop(foreground);

        assert!(jobs.background("r-0000000000000001").is_some());
        assert!(jobs.background("r-0000000000000000").is_none(), "it left");
        drop((tickets, again));
        assert!(
            jobs.stop_all().is_empty(),
            "nothing running, nothing to wait for"
        );
        let refused = jobs.admit(true, Arc::default()).err().unwrap();
        assert_eq!(refused.code(), ErrorCode::NotReady);
    }

    #[test]
    fn stop_all_stops_every_command_and_waits_for_its_waiter() {
        let jobs = Jobs::new();
        let mut waiters = Vec::new();
        for background in [true, false, true] {
            let stop = Arc::new(Stop::default());
            let ticket = jobs.admit(background, Arc::clone(&stop)).unwrap();
            let started = sleeper();
            stop.attach(started.stopper());
            waiters.push(std::thread::spawn(move || {
                let captured = started.wait().unwrap();
                // A waiter that takes a moment to write its result.
                std::thread::sleep(Duration::from_millis(100));
                drop(ticket);
                (captured, stop.reason())
            }));
        }
        let clock = Instant::now();
        assert!(jobs.stop_all().is_empty());
        assert!(jobs.lock().running.is_empty(), "stop_all returned early");
        assert!(
            clock.elapsed() < Duration::from_secs(5),
            "{:?}",
            clock.elapsed()
        );
        for waiter in waiters {
            let (captured, reason) = waiter.join().unwrap();
            assert!(captured.stopped);
            assert_eq!(reason, Some(StopReason::SessionEnded));
        }
        let refused = jobs.admit(false, Arc::default()).err().unwrap();
        assert_eq!(refused.code(), ErrorCode::NotReady);
    }

    #[test]
    fn a_status_says_how_the_command_ended() {
        let captured = |exit_code, timed_out, stopped| Captured {
            exit_code,
            timed_out,
            stopped,
            duration: Duration::from_millis(12_345),
            stdout_bytes: 1,
            stderr_bytes: 2,
        };
        let line = |c: Captured, reason| {
            let mut status = Status::started("make", None);
            status.end(&c, reason);
            status.describe(false)
        };
        assert_eq!(
            line(captured(Some(0), false, false), None),
            "exited 0 after 12.3 s"
        );
        assert_eq!(
            line(captured(None, true, false), None),
            "killed on its timeout after 12.3 s"
        );
        assert_eq!(
            line(captured(None, false, true), Some(StopReason::StopCommand)),
            "stopped by stop_command after 12.3 s"
        );
        assert_eq!(
            line(
                captured(Some(143), false, true),
                Some(StopReason::SessionEnded)
            ),
            "stopped when its session ended, after 12.3 s"
        );
        // A reason with no stop behind it (it ended by itself first) is not
        // reported as a stop.
        assert_eq!(
            line(
                captured(Some(1), false, false),
                Some(StopReason::StopCommand)
            ),
            "exited 1 after 12.3 s"
        );
        let running = Status::started("make", None);
        assert!(running.describe(true).starts_with("running for 0."));
        assert!(running.describe(false).contains("exit status is unknown"));
    }

    #[test]
    fn a_status_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("ccnm-jobs-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let guard = ccnm_testdir::TestDir::adopt(dir.clone());
        assert_eq!(Status::read(&dir), None);
        let status = Status::started("npm run dev", Some(5000));
        status.write(&dir).unwrap();
        assert_eq!(Status::read(&dir), Some(status));
        assert!(!dir.join(".status").exists());
        drop(guard);
    }
    // -- a background command end to end: exec_command, read_output,
    //    stop_command, the end of a session --

    use crate::mcp::exec::{self, ExecCommandArgs, ExecResult};
    use crate::mcp::output::{OutputPage, ReadOutputArgs};
    use crate::mcp::retention::Output;
    use ccnm_testdir::TestDir;

    struct Session {
        root: std::path::PathBuf,
        output: Arc<Output>,
        jobs: Arc<Jobs>,
        _dir: TestDir,
    }

    fn session(name: &str) -> Session {
        let dir = std::env::temp_dir().join(format!("ccnm-jobs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let state = std::fs::canonicalize(dir.join("state")).unwrap();
        Session {
            root: std::fs::canonicalize(dir.join("root")).unwrap(),
            output: Arc::new(Output::new(&state, "s-jobs")),
            jobs: Jobs::new(),
            _dir: TestDir::adopt(dir),
        }
    }

    impl Session {
        fn exec(&self, args: ExecCommandArgs, stop: Arc<Stop>) -> Result<ExecResult> {
            exec::exec_command_in(
                crate::provider::AgentProvider::Claude,
                &self.root,
                &self.output,
                &args,
                exec::Running {
                    jobs: &self.jobs,
                    stop,
                    sandbox: None,
                },
            )
        }

        fn background(&self, line: &str, timeout_ms: Option<u64>) -> ExecResult {
            self.exec(
                ExecCommandArgs {
                    shell: Some(line.into()),
                    timeout_ms,
                    run_in_background: true,
                    ..Default::default()
                },
                Arc::default(),
            )
            .unwrap()
        }

        fn read(&self, reference: &str) -> OutputPage {
            crate::mcp::output::read_output(
                self.output.dir(),
                &ReadOutputArgs {
                    output_ref: reference.into(),
                    ..Default::default()
                },
            )
            .unwrap()
        }

        fn stop(&self, reference: &str) -> Result<String> {
            stop_command(
                &self.jobs,
                self.output.dir(),
                &StopCommandArgs {
                    output_ref: reference.into(),
                },
            )
        }

        /// Until the run is no longer in progress, as read_output's wait_ms
        /// does it.
        fn wait(&self, reference: &str) {
            let run = self.output.dir().join(reference);
            wait_for(|| !retention::in_progress(&run));
        }
    }

    fn wait_for(condition: impl Fn() -> bool) {
        let clock = Instant::now();
        while !condition() {
            assert!(clock.elapsed() < Duration::from_secs(10), "never happened");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_background_command_returns_at_once_and_is_read_as_it_grows() {
        let s = session("grows");
        let clock = Instant::now();
        let r = s.background("echo first; sleep 1; echo second", None);
        assert!(
            clock.elapsed() < Duration::from_millis(800),
            "{:?}",
            clock.elapsed()
        );
        assert!(r.background && r.exit_code.is_none());
        assert!(
            r.text.starts_with(&format!(
                "$ echo first; sleep 1; echo second\nrunning in the background as output_ref {}\n",
                r.output_ref
            )),
            "{}",
            r.text
        );

        wait_for(|| s.read(&r.output_ref).text.starts_with("first\n"));
        let page = s.read(&r.output_ref);
        assert!(page.running && !page.eof, "{}", page.text);
        assert_eq!(page.next_offset, Some(6));
        assert!(
            page.text.contains(
                "[6 bytes so far and the command is still running; read again from offset=6"
            ),
            "{}",
            page.text
        );
        assert!(
            page.text.ends_with(']') && page.text.contains("\n[running for "),
            "{}",
            page.text
        );

        s.wait(&r.output_ref);
        let page = s.read(&r.output_ref);
        assert!(!page.running && page.eof, "{}", page.text);
        assert!(
            page.text
                .starts_with("first\nsecond\n[end of stdout at 13 bytes]\n[exited 0 after 1."),
            "{}",
            page.text
        );
        assert!(
            s.jobs.lock().running.is_empty(),
            "the waiter left the registry"
        );
    }

    #[test]
    fn stop_command_stops_it_and_reports_what_became_of_it() {
        let s = session("stop");
        let r = s.background("echo up; sleep 30", None);
        wait_for(|| s.read(&r.output_ref).text.starts_with("up\n"));
        let clock = Instant::now();
        let text = s.stop(&r.output_ref).unwrap();
        assert!(
            clock.elapsed() < Duration::from_secs(5),
            "{:?}",
            clock.elapsed()
        );
        assert!(
            text.starts_with("$ echo up; sleep 30\nstopped by stop_command after "),
            "{text}"
        );
        assert!(text.contains(", 3 B stdout, 0 B stderr"), "{text}");
        assert!(!text.contains("already ended"), "{text}");
        let page = s.read(&r.output_ref);
        assert!(
            page.eof && page.text.contains("\n[stopped by stop_command after "),
            "{}",
            page.text
        );

        let again = s.stop(&r.output_ref).unwrap();
        assert!(
            again.contains("[it had already ended; nothing was stopped]"),
            "{again}"
        );

        let foreground = s
            .exec(
                ExecCommandArgs {
                    shell: Some("true".into()),
                    ..Default::default()
                },
                Arc::default(),
            )
            .unwrap();
        let e = s.stop(&foreground.output_ref).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(
            e.message()
                .contains("was not started with run_in_background"),
            "{e}"
        );
        // And a foreground page carries no status line: nothing changed there.
        assert_eq!(
            s.read(&foreground.output_ref).text,
            "[end of stdout at 0 bytes]"
        );

        for (reference, says) in [
            ("r-ffffffffffffffff", "no output kept"),
            ("../../etc", "not an output_ref"),
        ] {
            let e = s.stop(reference).unwrap_err();
            assert!(e.message().contains(says), "{reference}: {e}");
        }
    }

    #[test]
    fn a_background_command_is_killed_on_its_timeout_if_it_has_one() {
        let s = session("timeout");
        let r = s.background("sleep 30", Some(300));
        assert!(
            r.text.contains("; it is killed after 300 ms\n"),
            "{}",
            r.text
        );
        s.wait(&r.output_ref);
        let page = s.read(&r.output_ref);
        assert!(
            page.text.contains("\n[killed on its timeout after 0."),
            "{}",
            page.text
        );
    }

    #[test]
    fn ending_the_session_stops_background_and_foreground_commands() {
        let s = Arc::new(session("end"));
        let r = s.background("sleep 30", None);
        let foreground = {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                s.exec(
                    ExecCommandArgs {
                        shell: Some("echo $$ > fg.pid; exec sleep 30".into()),
                        ..Default::default()
                    },
                    Arc::default(),
                )
                .unwrap()
            })
        };
        wait_for(|| std::fs::read_to_string(s.root.join("fg.pid")).is_ok_and(|p| !p.is_empty()));

        let clock = Instant::now();
        assert!(s.jobs.stop_all().is_empty());
        assert!(
            clock.elapsed() < Duration::from_secs(5),
            "{:?}",
            clock.elapsed()
        );
        let ended = foreground.join().unwrap();
        assert!(
            ended
                .notes
                .iter()
                .any(|n| n == "the command was stopped because its session ended"),
            "{:?}",
            ended.notes
        );
        let page = s.read(&r.output_ref);
        assert!(
            page.eof
                && page
                    .text
                    .contains("\n[stopped when its session ended, after "),
            "{}",
            page.text
        );

        let refused = s
            .exec(
                ExecCommandArgs {
                    shell: Some("true".into()),
                    ..Default::default()
                },
                Arc::default(),
            )
            .unwrap_err();
        assert_eq!(refused.code(), ErrorCode::NotReady);
    }

    #[test]
    fn a_cancelled_call_stops_its_command() {
        let s = Arc::new(session("cancel"));
        let stop = Arc::new(Stop::default());
        let call = {
            let (s, stop) = (Arc::clone(&s), Arc::clone(&stop));
            std::thread::spawn(move || {
                s.exec(
                    ExecCommandArgs {
                        shell: Some("echo $$ > cmd.pid; exec sleep 30".into()),
                        ..Default::default()
                    },
                    stop,
                )
                .unwrap()
            })
        };
        wait_for(|| std::fs::read_to_string(s.root.join("cmd.pid")).is_ok_and(|p| !p.is_empty()));
        let clock = Instant::now();
        assert!(stop.stop(StopReason::Cancelled));
        let ended = call.join().unwrap();
        assert!(
            clock.elapsed() < Duration::from_secs(5),
            "{:?}",
            clock.elapsed()
        );
        assert!(
            ended
                .notes
                .iter()
                .any(|n| n.contains("the call running it was cancelled")),
            "{:?}",
            ended.notes
        );
        assert!(s.jobs.lock().running.is_empty());

        // A call cancelled before its command started never starts it.
        let stop = Arc::new(Stop::default());
        stop.stop(StopReason::Cancelled);
        let e = s
            .exec(
                ExecCommandArgs {
                    shell: Some("touch never".into()),
                    ..Default::default()
                },
                stop,
            )
            .unwrap_err();
        assert!(
            e.message().contains("cancelled before the command started"),
            "{e}"
        );
        assert!(!s.root.join("never").exists());
    }

    #[test]
    fn only_eight_run_in_the_background_at_once() {
        let s = session("limit");
        let running: Vec<ExecResult> = (0..MAX_BACKGROUND)
            .map(|_| s.background("sleep 30", None))
            .collect();
        let e = s
            .exec(
                ExecCommandArgs {
                    shell: Some("sleep 30".into()),
                    run_in_background: true,
                    ..Default::default()
                },
                Arc::default(),
            )
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains(&running[7].output_ref), "{e}");
        s.stop(&running[0].output_ref).unwrap();
        let ninth = s.background("sleep 30", None);
        assert!(ninth.background);
        assert!(s.jobs.stop_all().is_empty());
    }

    /// A server killed before it could write the final status leaves a
    /// `running` status nobody holds. That reads as what it is.
    #[test]
    fn a_server_that_died_leaves_an_unknown_ending() {
        let s = session("died");
        let run = s.output.dir().join("r-00000000000000aa");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("stdout"), "partial\n").unwrap();
        std::fs::write(run.join("stderr"), "").unwrap();
        std::fs::write(run.join("running"), "").unwrap();
        Status::started("npm run dev", None).write(&run).unwrap();
        let page = s.read("r-00000000000000aa");
        assert!(page.eof && !page.running, "{}", page.text);
        assert!(page.text.ends_with("[end of stdout at 8 bytes]\n[no longer running, and its exit status is unknown: the server running it ended without recording one]"), "{}", page.text);
        let text = s.stop("r-00000000000000aa").unwrap();
        assert!(text.contains("exit status is unknown"), "{text}");
    }
}

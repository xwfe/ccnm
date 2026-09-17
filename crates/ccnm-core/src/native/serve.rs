//! `ccnm internal exec-serve`: run `codex exec-server` for one managed Codex
//! session and outlive it (P22).
//!
//! The order is the point, and each step is where it is for a reason:
//!
//! ```text
//! open        this Runtime's config decides workspace, root and binary
//! gate        the same audit and waivers as the MCP server; a chain that
//!             can start any process needs exec_command's permission
//! version     the binary's --version, before anything is held
//! guard       the workspace write guard, shared with every MCP entry
//! spawn       exec-server as a child -- never exec(): a replaced process
//!             runs no Drop, and the guard would stay `held` for ever
//! relay       every client line through native::policy, and ask a silent
//!             client whether it is still there (native::liveness)
//! shutdown    close exec-server's stdin, wait, then prove every process it
//!             started is gone before the guard is released
//! ```
//!
//! "Prove gone" looks for two kinds of process. exec-server starts each
//! command in its own process group, so killing exec-server's group reaches
//! none of them; it kills them itself when its stdin closes, except a process
//! that left its session with `setsid` (measured, 0.154.0). Every command
//! inherits exec-server's environment -- the policy refuses a
//! `process/start` that would not -- so it carries this session's marker.
//! The other kind is exec-server's fs helper, the process that does a
//! sandboxed file write: exec-server clears its environment, so it has no
//! marker, but it stays in exec-server's process group. A clean exit kills
//! it; an exec-server killed mid-operation leaves it running, and its write
//! then lands after the guard was released (P29, macOS). So after
//! exec-server exits, whatever still carries the marker *or* is still in
//! exec-server's group is found, killed and checked again. Anything that
//! cannot be proven gone leaves the guard `held`, which the next session
//! sees as unknown and refuses, exactly like a Runtime that crashed. A
//! command that deliberately clears its own environment is not found; that
//! is the platform isolation boundary the plan lists separately, not
//! something a supervisor can close.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::mcp::server::ExecGate;
use crate::mcp::write_guard::WriteGuard;
use crate::native::liveness::{self, Heard, Step, Timing, Touching};
use crate::native::policy::{Policy, Verdict};
use crate::process::{Cmd, ProcessRunner, SystemRunner};
use crate::runtime::NativeOpenPayload;

/// The variable every process of the session inherits. Its value is unique
/// per supervisor run, so a leftover from an earlier session is never
/// mistaken for one of this one's, or the other way round.
pub const MARKER: &str = "CCNM_EXEC_SESSION";

/// The longest single message read from the client. exec-server's own limit
/// is 64 MiB, and what it does past it is close the connection without a
/// word (toexec G01); stopping here first means ccnm says why. Codex sends a
/// whole file per `fs/writeFile`, so this is also the largest file a patch
/// can write through this chain.
pub const MAX_CLIENT_MESSAGE: usize = 32 * 1024 * 1024;

/// How long exec-server gets to exit after its stdin closes. Measured exit
/// is about 10 ms; the margin is for a loaded machine.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// How long leftover marked processes get to disappear after being killed.
const SWEEP_WAIT: Duration = Duration::from_secs(5);

pub fn serve(request: &NativeOpenPayload) -> Result<()> {
    let config = crate::Config::load(&crate::paths::effective_config_path()?)?;
    let native = crate::runtime::open_native(&config, request)?;
    let payload = native.serve_payload(request);
    let gate = ExecGate::decide(&payload)?;
    if !gate.audit.agent_boundary_clear(gate.accepted) {
        return Err(Error::policy(
            gate.audit
                .refusal(gate.accepted, crate::safety::Refused::Session),
        ));
    }
    // process/start is exec_command by another name: the same permission.
    if !gate.allowed() {
        return Err(Error::policy(
            gate.audit
                .refusal(gate.accepted, crate::safety::Refused::ExecCommand),
        ));
    }
    let root = native.opened.root.clone();
    let state = crate::paths::state_dir()?;
    let marker = format!("{}-{}", request.session, crate::session::new_id());
    let home = CodexHome::create(&state, &marker)?;
    let base = executor_cmd(&native.codex_bin, &root, home.path(), &marker);
    check_version(&base, &SystemRunner)?;
    let guard = WriteGuard::acquire(
        &state,
        &root,
        &request.workspace,
        &request.session,
        gate.config.as_ref(),
        &SystemRunner,
    )?;
    tracing::info!(
        workspace = %request.workspace,
        session = %request.session,
        runtime_user = %gate.audit.user,
        "exec-server supervisor starting"
    );
    let child = match spawn(&base) {
        Ok(child) => child,
        // Nothing was started, so nothing can be left behind.
        Err(error) => {
            drop(guard);
            return Err(error);
        }
    };
    let session = Session {
        child,
        policy: Policy::new(root, MARKER),
        marker,
        guard,
        home,
    };
    session.run(Client::stdio(), Timing::DEFAULT, &Sweeper::system())?;
    tracing::info!(session = %request.session, "exec-server session ended; write guard released");
    Ok(())
}

/// Everything a started session owns, from the running exec-server to the
/// guard it may only give back once the sweep says so.
struct Session {
    child: Child,
    policy: Policy,
    marker: String,
    guard: WriteGuard,
    home: CodexHome,
}

impl Session {
    /// Relay until either side ends, then shut down. The guard is released
    /// only when the shutdown proved nothing of the session is left;
    /// otherwise it stays `held` and the next session refuses this workspace
    /// as unknown until an operator has looked.
    fn run(mut self, client: Client, timing: Timing, sweeper: &Sweeper) -> Result<End> {
        let end = relay(&mut self.child, self.policy, client, timing);
        match shutdown(&mut self.child, &self.marker, sweeper) {
            Ok(()) => {
                drop(self.guard);
                drop(self.home);
                Ok(end)
            }
            Err(error) => {
                std::mem::forget(self.guard);
                self.home.keep();
                Err(error)
            }
        }
    }
}

/// The Codex side of the session. The process's own stdin and stdout, except
/// in tests.
struct Client {
    input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
}

impl Client {
    fn stdio() -> Self {
        Client {
            input: Box::new(std::io::stdin()),
            output: Box::new(std::io::stdout()),
        }
    }
}

type ClientOut = Mutex<Box<dyn Write + Send>>;

/// `codex exec-server --listen stdio` with the Runtime child environment:
/// the same cleaning `exec_command` gets, then a CODEX_HOME that ccnm made
/// and the session marker. exec-server hands its whole environment to every
/// command (Codex sends `envPolicy.inherit = all`), so this is the
/// environment the model's commands run with.
fn executor_cmd(bin: &Path, root: &Path, home: &Path, marker: &str) -> Cmd {
    let cmd = crate::safety::environment::runtime_child(Cmd::new(bin).cwd(root));
    cmd.env("CODEX_HOME", home).env(MARKER, marker)
}

fn check_version(base: &Cmd, runner: &dyn ProcessRunner) -> Result<()> {
    crate::provider::codex::check_measured(base, runner, "the exec-server chain")
}

fn spawn(base: &Cmd) -> Result<Child> {
    let mut cmd = base.clone();
    cmd.args = vec!["exec-server".into(), "--listen".into(), "stdio".into()];
    let mut command: Command = cmd.process();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .spawn()
        .map_err(|e| Error::internal("cannot start codex exec-server").with_source(e))
}

/// How the relay ended. Only for the log and tests: every ending shuts down
/// the same way.
#[derive(Debug, PartialEq, Eq)]
enum End {
    ClientClosed,
    ClientTooLong,
    ClientUnreadable,
    ClientUnwritable,
    /// Nothing from the client for `give_up_after`, pings included.
    ClientSilent,
    ExecutorClosed,
    ExecutorUnwritable,
}

/// Pump both directions until either side ends, asking a silent client
/// whether it is still there.
///
/// Every blocking read and write runs on its own thread, and this one only
/// waits for an ending and looks at the clock: a read on stdin cannot be
/// interrupted, and a write to a client that vanished without a word blocks
/// once the connection's buffers are full -- neither may keep the session,
/// and with it the write guard. The process exits soon after this returns,
/// and those threads with it.
fn relay(child: &mut Child, policy: Policy, client: Client, timing: Timing) -> End {
    let heard = Heard::new();
    let out: Arc<ClientOut> = Arc::new(Mutex::new(client.output));
    let child_in = Arc::new(Mutex::new(child.stdin.take()));
    let child_out = child.stdout.take();
    let (done, ended) = mpsc::channel();

    let to_client = Arc::clone(&out);
    let progress = Arc::clone(&heard);
    let executor_done = done.clone();
    std::thread::spawn(move || {
        let end = match child_out {
            Some(output) => copy_executor(output, &to_client, &progress),
            None => End::ExecutorClosed,
        };
        let _ = executor_done.send(end);
    });

    let from_client = Arc::clone(&child_in);
    let replies = Arc::clone(&out);
    let input = Touching::new(client.input, Arc::clone(&heard));
    let client_done = done.clone();
    std::thread::spawn(move || {
        let input = BufReader::with_capacity(64 * 1024, input);
        let end = read_client(input, &policy, &from_client, &replies);
        let _ = client_done.send(end);
    });

    // One slot: while a ping is still queued behind a stuck write, asking
    // again adds nothing.
    let (ask, asks) = mpsc::sync_channel::<()>(1);
    let pinger = Arc::clone(&out);
    std::thread::spawn(move || {
        let mut n = 0u64;
        while asks.recv().is_ok() {
            n += 1;
            if write_line(&pinger, &liveness::ping(n)).is_err() {
                let _ = done.send(End::ClientUnwritable);
                return;
            }
        }
    });

    let mut last_ping = None;
    let end = loop {
        match ended.recv_timeout(timing.tick) {
            Ok(end) => break end,
            Err(RecvTimeoutError::Disconnected) => break End::ClientClosed,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        match liveness::step(&timing, now, heard.at(), last_ping) {
            Step::Wait => {}
            Step::Ping => {
                if ask.try_send(()).is_ok() {
                    last_ping = Some(now);
                }
            }
            Step::GiveUp => {
                tracing::warn!(
                    silent_seconds = timing.give_up_after.as_secs(),
                    "nothing from the client, not even an answer to a liveness request; ending the exec-server session"
                );
                break End::ClientSilent;
            }
        }
    };
    tracing::info!(?end, "exec-server relay ended");
    close_executor_input(&child_in);
    end
}

/// Closing exec-server's stdin is how it is told to stop: it exits and kills
/// the processes it started (toexec G01).
///
/// The client thread holds this lock while it forwards a line, and that
/// write can block for good when exec-server has stopped reading -- which it
/// does once its own output is stuck behind a client that vanished. Waiting
/// for the lock would then keep the session, and the guard, for ever; not
/// closing is fine, because the shutdown kills an exec-server that does not
/// exit, and the sweep still has to prove the rest.
fn close_executor_input(child_in: &Mutex<Option<ChildStdin>>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child_in.try_lock() {
            Ok(mut stdin) => {
                stdin.take();
                return;
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().take();
                return;
            }
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                tracing::warn!(
                    "a write to exec-server is stuck; its stdin stays open and the shutdown kills it"
                );
                return;
            }
        }
    }
}

/// One whole line to the client, under the lock that keeps lines whole.
fn write_line(out: &ClientOut, line: &[u8]) -> std::io::Result<()> {
    let mut client = out
        .lock()
        .map_err(|_| std::io::Error::other("client output lock poisoned"))?;
    client.write_all(line)?;
    client.flush()
}

fn read_client(
    input: impl BufRead,
    policy: &Policy,
    child_in: &Mutex<Option<ChildStdin>>,
    out: &ClientOut,
) -> End {
    let mut lines = BoundedLines::new(input, MAX_CLIENT_MESSAGE);
    loop {
        let line = match lines.next_line() {
            Ok(Some(line)) => line,
            Ok(None) => return End::ClientClosed,
            Err(LineError::TooLong) => {
                tracing::error!(
                    limit = MAX_CLIENT_MESSAGE,
                    "client message too long; ending the exec-server session"
                );
                return End::ClientTooLong;
            }
            Err(LineError::Io) => return End::ClientUnreadable,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let verdict = match serde_json::from_slice::<Value>(&line) {
            // Asked by this supervisor, not by exec-server: hearing it was
            // the point, and it has nowhere to go.
            Ok(message) if liveness::is_answer(&message) => continue,
            Ok(message) => policy.decide(&message),
            Err(_) => Verdict::Reply(serde_json::json!({
                "id": -1,
                "error": {"code": -32600, "message": "ccnm: message is not valid JSON"},
            })),
        };
        match verdict {
            Verdict::Forward => {
                let Ok(mut guard) = child_in.lock() else {
                    return End::ExecutorUnwritable;
                };
                let Some(stdin) = guard.as_mut() else {
                    return End::ExecutorUnwritable;
                };
                if stdin
                    .write_all(&line)
                    .and_then(|()| stdin.write_all(b"\n"))
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    return End::ExecutorUnwritable;
                }
            }
            Verdict::Reply(reply) => {
                tracing::info!(reply = %reply, "exec-server request refused");
                let mut text = reply.to_string().into_bytes();
                text.push(b'\n');
                if write_line(out, &text).is_err() {
                    return End::ClientUnwritable;
                }
            }
            Verdict::Drop => {
                tracing::info!("client notification not forwarded");
            }
        }
    }
}

/// Copy exec-server's output to the client one whole line at a time, so a
/// refusal or ping written from another thread never lands inside one.
/// Lines are streamed rather than buffered: `fs/readFile` answers can be
/// hundreds of megabytes.
///
/// A line that takes more than one chunk counts as hearing from the client
/// each time a chunk is taken: the lock is held for the whole line, so no
/// ping can be asked meanwhile, and a client reading a large answer slowly
/// is still there. One that is not reading stops taking chunks once the
/// connection's buffers are full, which is what lets the silence count run.
/// A single-chunk line never counts -- small writes land in those buffers
/// whether or not anyone is reading.
fn copy_executor(output: impl Read, out: &ClientOut, heard: &Heard) -> End {
    let mut reader = BufReader::with_capacity(64 * 1024, output);
    loop {
        // Wait for the next line *before* taking the lock. Holding it while
        // blocked here is a deadlock: a refusal from the client thread
        // needs the same lock, and exec-server has nothing to say until the
        // client's next request -- which is waiting for that refusal.
        match reader.fill_buf() {
            Ok([]) | Err(_) => return End::ExecutorClosed,
            Ok(_) => {}
        }
        let Ok(mut client) = out.lock() else {
            return End::ClientUnwritable;
        };
        let mut first = true;
        loop {
            let chunk = match reader.fill_buf() {
                Ok([]) | Err(_) => return End::ExecutorClosed,
                Ok(chunk) => chunk,
            };
            let (part, line_done) = match chunk.iter().position(|b| *b == b'\n') {
                Some(end) => (&chunk[..=end], true),
                None => (chunk, false),
            };
            let whole = first && line_done;
            if whole {
                log_handshake(part);
            }
            first = false;
            if client.write_all(part).is_err() {
                return End::ClientUnwritable;
            }
            if !whole {
                heard.touch();
            }
            let used = part.len();
            reader.consume(used);
            if line_done {
                break;
            }
        }
        if client.flush().is_err() {
            return End::ClientUnwritable;
        }
    }
}

/// Record which executor build answered. `executorVersion` is logged, not
/// checked: the official Linux build reports `0.0.0` (P21), so the version
/// gate is the binary's own `--version`.
fn log_handshake(line: &[u8]) {
    if line.len() > 64 * 1024 {
        return;
    }
    let Ok(message) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    if let Some(info) = message.pointer("/result/environmentInfo") {
        let field = |name: &str| info.get(name).cloned().unwrap_or(Value::Null);
        tracing::info!(
            provider_id = %field("providerId"),
            executor_version = %field("executorVersion"),
            "exec-server handshake"
        );
    }
}

enum LineError {
    TooLong,
    Io,
}

/// `read_until(b'\n')` with a ceiling: a client that never sends a newline
/// cannot make this process allocate without bound.
struct BoundedLines<R> {
    reader: R,
    limit: usize,
}

impl<R: BufRead> BoundedLines<R> {
    fn new(reader: R, limit: usize) -> Self {
        BoundedLines { reader, limit }
    }

    fn next_line(&mut self) -> std::result::Result<Option<Vec<u8>>, LineError> {
        let mut line = Vec::new();
        loop {
            let chunk = self.reader.fill_buf().map_err(|_| LineError::Io)?;
            if chunk.is_empty() {
                return Ok((!line.is_empty()).then_some(line));
            }
            let (part, done) = match chunk.iter().position(|b| *b == b'\n') {
                Some(end) => (&chunk[..end], Some(end + 1)),
                None => (chunk, None),
            };
            if line.len() + part.len() > self.limit {
                return Err(LineError::TooLong);
            }
            line.extend_from_slice(part);
            let used = done.unwrap_or(chunk.len());
            self.reader.consume(used);
            if done.is_some() {
                return Ok(Some(line));
            }
        }
    }
}

/// Stop exec-server and prove the session left nothing running.
fn shutdown(child: &mut Child, marker: &str, sweeper: &Sweeper) -> Result<()> {
    if !wait_for_exit(child, EXIT_WAIT) {
        tracing::warn!("exec-server did not exit after its stdin closed; killing it");
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        if !wait_for_exit(child, Duration::from_secs(5)) {
            return Err(Error::policy(
                "exec-server did not exit and could not be killed; the workspace write guard stays held",
            ));
        }
    }
    // Spawned with `process_group(0)`: the group is named by its pid.
    sweeper.sweep(marker, child.id(), SWEEP_WAIT)
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return false,
        }
    }
}

/// Finds and removes what is left of one session: processes carrying its
/// marker, and processes still in its executor's process group.
pub struct Sweeper {
    list: fn(&str, u32) -> Result<Vec<u32>>,
}

impl Sweeper {
    pub fn system() -> Self {
        Sweeper {
            list: session_processes,
        }
    }

    /// Kill what carries the marker or is in `group` until nothing is, or
    /// say it could not be proven.
    ///
    /// Only call this once the group's leader has exited. The group number
    /// could then in principle be reused, but only after the group is empty
    /// and a new process takes that same number as its pid and leads a
    /// group of its own, within these few seconds -- the same order of risk
    /// as a listed pid being reused before its kill.
    pub fn sweep(&self, marker: &str, group: u32, limit: Duration) -> Result<()> {
        let deadline = Instant::now() + limit;
        loop {
            let pids = (self.list)(marker, group).map_err(|error| {
                Error::policy(format!(
                    "cannot list processes to prove the exec-server session is over ({}); the workspace write guard stays held",
                    error.message()
                ))
            })?;
            if pids.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::policy(format!(
                    "{} process(es) of this exec-server session are still running after being killed; the workspace write guard stays held\nfind them on the Runtime Node by the {MARKER}={marker} variable in their environment, or by process group {group}",
                    pids.len()
                )));
            }
            tracing::warn!(
                count = pids.len(),
                "exec-server session left processes; killing them"
            );
            for pid in pids {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Every live process of this account that belongs to one session: its
/// environment has `MARKER=marker`, or its process group is `group`.
///
/// Only this account's processes are visible with their environment, which
/// is enough: exec-server and everything it starts run as this account.
/// Zombies are skipped -- they have finished and cannot write -- and they
/// keep their group until reaped, which is not this process's job.
#[cfg(target_os = "linux")]
pub fn session_processes(marker: &str, group: u32) -> Result<Vec<u32>> {
    let wanted = format!("{MARKER}={marker}");
    let mut pids = Vec::new();
    let entries = std::fs::read_dir("/proc")
        .map_err(|e| Error::internal("cannot read /proc").with_source(e))?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // Gone already: not this session's.
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((state, pgrp)) = stat_state_and_group(&stat) else {
            continue;
        };
        if state == 'Z' {
            continue;
        }
        if pgrp == group {
            pids.push(pid);
            continue;
        }
        // Another account's environment is unreadable: not this session's.
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ
            .split(|b| *b == 0)
            .any(|var| var == wanted.as_bytes())
        {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Every live process of this account that belongs to one session: its
/// environment has `MARKER=marker`, or its process group is `group`.
///
/// `ps -E` appends a process's environment to its command column, for the
/// caller's own processes only. The marker value is a fresh UUID, so a
/// command line that merely mentions it is not a realistic collision; `ps`
/// itself does not, because nothing on its command line contains it.
/// Zombies are skipped, as on Linux.
#[cfg(not(target_os = "linux"))]
pub fn session_processes(marker: &str, group: u32) -> Result<Vec<u32>> {
    let wanted = format!("{MARKER}={marker}");
    let out = Command::new("ps")
        .args([
            "-axEww", "-o", "pid=", "-o", "pgid=", "-o", "stat=", "-o", "command=",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| Error::internal("cannot run ps").with_source(e))?;
    if !out.status.success() {
        return Err(Error::internal("ps failed"));
    }
    let own = std::process::id();
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse::<u32>().ok()?;
            let pgid = words.next()?.parse::<u32>().ok()?;
            let state = words.next()?;
            let belongs = pgid == group || words.any(|word| word == wanted.as_str());
            (belongs && !state.starts_with('Z') && pid != own).then_some(pid)
        })
        .collect())
}

/// The state letter and process group of a `/proc/<pid>/stat` line.
///
/// The command name sits in parentheses and may itself contain spaces and
/// parentheses, so fields are counted from the last `)`: state, ppid, pgrp.
/// Not platform-gated so its tests run everywhere; only Linux calls it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_state_and_group(stat: &str) -> Option<(char, u32)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    Some((state, pgrp))
}

/// A CODEX_HOME for a `codex` that ccnm runs on the Runtime: exec-server
/// here, `codex sandbox` for the `exec_command` sandbox
/// (`crate::mcp::sandbox`). Made by ccnm, empty, private, and under ccnm's
/// state directory rather than the system temp dir, where exec-server
/// refuses to create its helper links (P21). Codex writes into it (session
/// files, the sandbox's `tmp/arg0/` helpers), so it must be a real
/// directory of its own, not the profile with the login in it.
pub(crate) struct CodexHome {
    dir: PathBuf,
    keep: bool,
}

impl CodexHome {
    fn create(state: &Path, marker: &str) -> Result<Self> {
        Self::create_in(state, "exec-server", marker)
    }

    /// `<state>/<parent>/<name>`, mode 0700 both levels.
    pub(crate) fn create_in(state: &Path, parent: &str, name: &str) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = state.join(parent);
        std::fs::create_dir_all(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        let dir = parent.join(name);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(CodexHome { dir, keep: false })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }

    /// Leave it on disk: something from the session may still be using it.
    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for CodexHome {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests;

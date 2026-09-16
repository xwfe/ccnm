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
//! relay       every client line through native::policy
//! shutdown    close exec-server's stdin, wait, then prove every process it
//!             started is gone before the guard is released
//! ```
//!
//! "Prove gone" is by environment marker. exec-server starts each command
//! in its own process group, so killing exec-server's group reaches none of
//! them; it kills them itself when its stdin closes, except a process that
//! left its session with `setsid` (measured, 0.154.0). Every one of them
//! inherits exec-server's environment -- the policy refuses a
//! `process/start` that would not -- so a process still carrying this
//! session's marker after exec-server exits is found, killed and checked
//! again. Anything that cannot be proven gone leaves the guard `held`, which
//! the next session sees as unknown and refuses, exactly like a Runtime that
//! crashed. A process that deliberately clears its own environment is not
//! found; that is the platform isolation boundary the plan lists
//! separately, not something a supervisor can close.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::server::ExecGate;
use crate::mcp::write_guard::WriteGuard;
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
    let mut child = match spawn(&base) {
        Ok(child) => child,
        // Nothing was started, so nothing can be left behind.
        Err(error) => {
            drop(guard);
            return Err(error);
        }
    };
    relay(&mut child, Policy::new(root, MARKER));
    match shutdown(&mut child, &marker, &Sweeper::system()) {
        Ok(()) => {
            drop(guard);
            drop(home);
            tracing::info!(session = %request.session, "exec-server session ended; write guard released");
            Ok(())
        }
        Err(error) => {
            // Leave the marker `held`: the next session refuses this
            // workspace as unknown until an operator has looked.
            std::mem::forget(guard);
            home.keep();
            Err(error)
        }
    }
}

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
    let mut cmd = base.clone();
    cmd.args = vec!["--version".into()];
    let out = runner.run(&cmd.timeout(Duration::from_secs(20)))?;
    let version = crate::provider::codex::parse_version(&out)?;
    if version != crate::provider::codex::VERSION {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "codex_bin is Codex {version}; the exec-server chain has been measured only with {}",
                crate::provider::codex::VERSION
            ),
        ));
    }
    Ok(())
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

/// How the relay ended. Only for the log: every ending shuts down the same
/// way.
#[derive(Debug)]
enum End {
    ClientClosed,
    ClientTooLong,
    ClientUnreadable,
    ExecutorClosed,
    ExecutorUnwritable,
}

/// Pump both directions until either side ends.
///
/// The client side runs on its own thread because a read on stdin cannot be
/// interrupted: when exec-server is the one that goes away, this function
/// has to return without waiting for the client to send another line. The
/// process exits soon after, and that thread with it.
fn relay(child: &mut Child, policy: Policy) {
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let child_in = Arc::new(Mutex::new(child.stdin.take()));
    let child_out = child.stdout.take();
    let (done, ended) = mpsc::channel();

    let to_client = Arc::clone(&stdout);
    let executor_done = done.clone();
    std::thread::spawn(move || {
        let end = match child_out {
            Some(out) => copy_executor(out, &to_client),
            None => End::ExecutorClosed,
        };
        let _ = executor_done.send(end);
    });

    let from_client = Arc::clone(&child_in);
    std::thread::spawn(move || {
        let end = read_client(std::io::stdin().lock(), &policy, &from_client, &stdout);
        let _ = done.send(end);
    });

    let end = ended.recv().unwrap_or(End::ClientClosed);
    tracing::info!(?end, "exec-server relay ended");
    // Closing exec-server's stdin is how it is told to stop: it exits and
    // kills the processes it started (toexec G01).
    if let Ok(mut stdin) = child_in.lock() {
        stdin.take();
    }
}

fn read_client(
    input: impl BufRead,
    policy: &Policy,
    child_in: &Mutex<Option<ChildStdin>>,
    stdout: &Mutex<std::io::Stdout>,
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
                let Ok(mut out) = stdout.lock() else {
                    return End::ClientUnreadable;
                };
                let mut text = reply.to_string().into_bytes();
                text.push(b'\n');
                if out.write_all(&text).and_then(|()| out.flush()).is_err() {
                    return End::ClientUnreadable;
                }
            }
            Verdict::Drop => {
                tracing::info!("client notification not forwarded");
            }
        }
    }
}

/// Copy exec-server's output to the client one whole line at a time, so a
/// refusal written from the other thread never lands inside one. Lines are
/// streamed rather than buffered: `fs/readFile` answers can be hundreds of
/// megabytes.
fn copy_executor(out: impl Read, stdout: &Mutex<std::io::Stdout>) -> End {
    let mut reader = BufReader::with_capacity(64 * 1024, out);
    loop {
        // Wait for the next line *before* taking the lock. Holding it while
        // blocked here is a deadlock: a refusal from the client thread
        // needs the same lock, and exec-server has nothing to say until the
        // client's next request -- which is waiting for that refusal.
        match reader.fill_buf() {
            Ok([]) | Err(_) => return End::ExecutorClosed,
            Ok(_) => {}
        }
        let Ok(mut client) = stdout.lock() else {
            return End::ClientUnreadable;
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
            if first && line_done {
                log_handshake(part);
            }
            first = false;
            if client.write_all(part).is_err() {
                return End::ClientUnreadable;
            }
            let used = part.len();
            reader.consume(used);
            if line_done {
                break;
            }
        }
        if client.flush().is_err() {
            return End::ClientUnreadable;
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
    sweeper.sweep(marker, SWEEP_WAIT)
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

/// Finds and removes processes carrying one session's marker.
pub struct Sweeper {
    list: fn(&str) -> Result<Vec<u32>>,
}

impl Sweeper {
    pub fn system() -> Self {
        Sweeper {
            list: marked_processes,
        }
    }

    /// Kill what carries the marker until nothing does, or say it could
    /// not be proven.
    pub fn sweep(&self, marker: &str, limit: Duration) -> Result<()> {
        let deadline = Instant::now() + limit;
        loop {
            let pids = (self.list)(marker).map_err(|error| {
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
                    "{} process(es) of this exec-server session are still running after being killed; the workspace write guard stays held\nfind them on the Runtime Node by the {MARKER}={marker} variable in their environment",
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

/// Every process of this account whose environment has `MARKER=marker`.
///
/// Only this account's processes are visible with their environment, which
/// is enough: exec-server and everything it starts run as this account.
#[cfg(target_os = "linux")]
pub fn marked_processes(marker: &str) -> Result<Vec<u32>> {
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
        // Gone already, or another account's: neither is this session's.
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

/// Every process of this account whose environment has `MARKER=marker`.
///
/// `ps -E` appends a process's environment to its command column, for the
/// caller's own processes only. The marker value is a fresh UUID, so a
/// command line that merely mentions it is not a realistic collision; `ps`
/// itself does not, because nothing on its command line contains it.
#[cfg(not(target_os = "linux"))]
pub fn marked_processes(marker: &str) -> Result<Vec<u32>> {
    let wanted = format!("{MARKER}={marker}");
    let out = Command::new("ps")
        .args(["-axEww", "-o", "pid=", "-o", "command="])
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
        .filter(|line| line.split_whitespace().any(|word| word == wanted.as_str()))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .collect())
}

/// The CODEX_HOME exec-server runs with. Made by ccnm, empty, private, and
/// under ccnm's state directory rather than the system temp dir, where
/// exec-server refuses to create its helper links (P21).
struct CodexHome {
    dir: PathBuf,
    keep: bool,
}

impl CodexHome {
    fn create(state: &Path, marker: &str) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = state.join("exec-server");
        std::fs::create_dir_all(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        let dir = parent.join(marker);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(CodexHome { dir, keep: false })
    }

    fn path(&self) -> &Path {
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

//! Spawning child processes without a shell.
//!
//! Everything ccnm runs non-interactively (ssh, mount, `claude auth status`,
//! the runner's commands) goes through here so that:
//!
//! - argv is a list, never a shell string, so there is no quoting to get wrong;
//! - every call has a timeout and cannot hang a Claude hook forever;
//! - the child's environment is explicit, which is how the runner strips
//!   `ANTHROPIC_*` later (design doc section 32);
//! - tests can swap in [`FakeRunner`] and assert exactly what would run.
//!
//! Interactive things (attaching a terminal to tmux, launching the Claude
//! TUI) do not belong here. They use `std::process::Command` with inherited
//! stdio at the call site.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// One command to run. Build it with the chained methods, then hand it to a
/// [`ProcessRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Variables to set in the child, applied after `env_remove`.
    pub env: Vec<(OsString, OsString)>,
    /// Variables to strip from the inherited environment.
    pub env_remove: Vec<OsString>,
    /// Bytes to feed on stdin. `None` means stdin is `/dev/null`, so a child
    /// that unexpectedly reads stdin gets EOF instead of the user's terminal.
    pub stdin: Option<Vec<u8>>,
    /// Hard limit. The child is killed (SIGKILL) when it expires.
    pub timeout: Duration,
}

impl Cmd {
    /// Generous enough for `cargo test`, short enough that a wedged SSH does
    /// not look like a hung Claude.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Cmd {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            timeout: Cmd::DEFAULT_TIMEOUT,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env_remove.push(key.as_ref().to_os_string());
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Program and args joined with spaces, for logs and audit lines only.
    /// It is not quoted, so never feed it back to a shell.
    pub fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What a finished (or killed) child left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// `None` when the child died from a signal, which includes being killed
    /// on timeout.
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub duration: Duration,
}

impl Output {
    /// Build an output for scripting a [`FakeRunner`].
    pub fn exited(exit_code: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Output {
            exit_code: Some(exit_code),
            stdout: stdout.into(),
            stderr: Vec::new(),
            timed_out: false,
            duration: Duration::ZERO,
        }
    }

    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

pub trait ProcessRunner {
    /// Run to completion or timeout. `Err` means the child could not be
    /// started or observed at all; a non-zero exit or a timeout is an `Ok`
    /// output for the caller to interpret.
    fn run(&self, cmd: &Cmd) -> Result<Output>;
}

/// Kill a child *and everything it started*.
///
/// `Child::kill` signals one process. A command like `sh -c 'x & sleep 30'`
/// leaves its own children holding the pipes, so killing only the leader
/// means the drain threads read on until the grandchild finishes -- a
/// timeout that does not time out. Both spawn sites put the child in its
/// own process group, so a negative pid here reaches all of it.
///
/// `kill(1)` rather than `libc::killpg` because this crate forbids unsafe.
/// The direct kill still runs afterwards: if `kill` is missing or the
/// group is already gone, the leader is what matters.
fn kill_group(child: &mut std::process::Child) {
    let pid = child.id();
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

/// What [`stream_lines`] should do after handing over one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Enough. The child is killed rather than left to finish.
    Stop,
}

/// How a streamed command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Streamed {
    /// `None` when the child was killed, by [`Flow::Stop`] or by the timeout.
    pub exit_code: Option<i32>,
    /// The tail of stderr, bounded by [`STREAM_STDERR_KEEP`].
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    /// The caller asked to stop before the child was done.
    pub stopped_early: bool,
}

/// Bytes of stderr kept from a streamed command. A child that fails in a
/// loop can write megabytes, and none of it past the first screenful helps.
pub const STREAM_STDERR_KEEP: usize = 8 * 1024;

/// Run `cmd` and hand each line of its stdout to `on_line` as it arrives,
/// killing the child as soon as `on_line` says [`Flow::Stop`].
///
/// [`ProcessRunner::run`] cannot do this: it waits for the child, so a
/// search tool built on it would let `rg` walk a whole monorepo and then
/// throw away all but the first fifty matches. Stopping early is the
/// difference between bounding what is *returned* and bounding what is
/// *done*.
///
/// Not a trait method. A callback does not fit [`FakeRunner`]'s scripted
/// model, and the callers of this (search today, `exec_command` later) are
/// better tested against the real program anyway: what they mostly have to
/// get right is that program's output format.
pub fn stream_lines(cmd: &Cmd, mut on_line: impl FnMut(&[u8]) -> Flow) -> Result<Streamed> {
    let started = Instant::now();
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &cmd.cwd {
        command.current_dir(dir);
    }
    for key in &cmd.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &cmd.env {
        command.env(key, value);
    }

    // Its own process group, so the watchdog and Flow::Stop can reach
    // whatever the command starts, not just the command.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    tracing::debug!(cmd = %cmd.display(), "stream");
    let mut child = command.spawn().map_err(|e| {
        Error::internal(format!("cannot spawn {}", cmd.program.to_string_lossy())).with_source(e)
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::internal("child has no stdout"))?;
    let stderr_reader = drain_bounded(child.stderr.take(), STREAM_STDERR_KEEP);

    // The child is shared with the watchdog so a program that stops
    // producing output cannot outlive its timeout. Killing by pid would
    // need libc, and this crate forbids unsafe.
    let child = std::sync::Arc::new(Mutex::new(child));
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let child = std::sync::Arc::clone(&child);
        let finished = std::sync::Arc::clone(&finished);
        let deadline = started + cmd.timeout;
        let shown = cmd.display();
        thread::spawn(move || {
            let mut poll = POLL_MIN;
            loop {
                if finished.load(std::sync::atomic::Ordering::SeqCst) {
                    return false;
                }
                if Instant::now() >= deadline {
                    tracing::warn!(cmd = %shown, "stream timeout, killing");
                    if let Ok(mut child) = child.lock() {
                        kill_group(&mut child);
                    }
                    return true;
                }
                thread::sleep(poll);
                poll = (poll * 2).min(POLL_MAX);
            }
        })
    };

    let mut reader = std::io::BufReader::new(stdout);
    let mut line = Vec::new();
    let mut stopped_early = false;
    loop {
        line.clear();
        let read = std::io::BufRead::read_until(&mut reader, b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line.pop();
        }
        if on_line(&line) == Flow::Stop {
            stopped_early = true;
            if let Ok(mut child) = child.lock() {
                kill_group(&mut child);
            }
            break;
        }
    }
    // Let go of the pipe so a killed child cannot block writing to it.
    drop(reader);

    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    let timed_out = watchdog.join().unwrap_or(false);
    let status = child
        .lock()
        .map_err(|_| Error::internal("process mutex poisoned"))?
        .wait()?;
    let stderr = join(stderr_reader)?;
    Ok(Streamed {
        exit_code: status.code(),
        stderr,
        timed_out,
        stopped_early,
    })
}

/// How a captured command ended, without any of its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// `None` when the child died from a signal, timeout included.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    /// Bytes the child wrote, whether or not the sink kept them.
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

/// Run `cmd`, writing its stdout and stderr to `out` and `err` as they
/// arrive instead of collecting them.
///
/// [`ProcessRunner::run`] collects into memory, which is right for the
/// short commands ccnm asks about itself and wrong for `exec_command`: a
/// `cargo build` on a large project can produce tens of megabytes, and
/// holding that in the runtime to write it out afterwards is a spike
/// nobody asked for.
///
/// Both pipes are drained on their own threads whatever the sink does with
/// the bytes, because a sink that stops writing must not become a child
/// that blocks on a full pipe and then gets killed for timing out.
pub fn run_captured<O, E>(cmd: &Cmd, out: O, err: E) -> Result<Captured>
where
    O: Write + Send + 'static,
    E: Write + Send + 'static,
{
    let started = Instant::now();
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &cmd.cwd {
        command.current_dir(dir);
    }
    for key in &cmd.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &cmd.env {
        command.env(key, value);
    }

    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    tracing::debug!(cmd = %cmd.display(), "capture");
    let mut child = command.spawn().map_err(|e| {
        Error::internal(format!("cannot spawn {}", cmd.program.to_string_lossy())).with_source(e)
    })?;
    let stdout = pump(child.stdout.take(), out);
    let stderr = pump(child.stderr.take(), err);

    let child = std::sync::Arc::new(Mutex::new(child));
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let child = std::sync::Arc::clone(&child);
        let finished = std::sync::Arc::clone(&finished);
        let deadline = started + cmd.timeout;
        let shown = cmd.display();
        thread::spawn(move || {
            let mut poll = POLL_MIN;
            loop {
                if finished.load(std::sync::atomic::Ordering::SeqCst) {
                    return false;
                }
                if Instant::now() >= deadline {
                    tracing::warn!(cmd = %shown, timeout = ?cmd_timeout(deadline, started), "capture timeout, killing");
                    if let Ok(mut child) = child.lock() {
                        kill_group(&mut child);
                    }
                    return true;
                }
                thread::sleep(poll);
                poll = (poll * 2).min(POLL_MAX);
            }
        })
    };

    // Both pipes reach EOF when the child exits, so waiting on the pumps
    // first means the wait below returns immediately.
    let stdout_bytes = join(stdout)?;
    let stderr_bytes = join(stderr)?;
    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    let timed_out = watchdog.join().unwrap_or(false);
    let status = child
        .lock()
        .map_err(|_| Error::internal("process mutex poisoned"))?
        .wait()?;

    Ok(Captured {
        exit_code: status.code(),
        timed_out,
        duration: started.elapsed(),
        stdout_bytes,
        stderr_bytes,
    })
}

fn cmd_timeout(deadline: Instant, started: Instant) -> Duration {
    deadline.saturating_duration_since(started)
}

/// Copy a pipe into a sink, counting every byte the child produced even
/// when the sink stops keeping them.
fn pump<R, W>(pipe: Option<R>, mut sink: W) -> JoinHandle<u64>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        let mut total = 0u64;
        let Some(mut pipe) = pipe else {
            return 0;
        };
        let mut buf = [0u8; 32 * 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    total += n as u64;
                    // A sink that refuses the bytes is its own business;
                    // the pipe still has to be drained.
                    let _ = sink.write_all(&buf[..n]);
                }
            }
        }
        let _ = sink.flush();
        total
    })
}

/// Like [`drain`] but keeps only the last `keep` bytes.
fn drain_bounded<R: Read + Send + 'static>(pipe: Option<R>, keep: usize) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        if buf.len() > keep {
            buf.drain(..buf.len() - keep);
        }
        buf
    })
}

/// Runs commands for real.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

/// How long to wait before first asking whether the child has exited, and
/// the ceiling the wait backs off to.
///
/// A single 5 ms interval was fine while every command was an `ssh`, where
/// it hid inside a 20 ms round trip. It stopped being fine once tools
/// started running local commands: `git ls-files` takes about 9 ms here,
/// so a fixed 5 ms poll rounds it up to 10 and adds nothing but latency to
/// every `list_files`, and `search_text` and `exec_command` would inherit
/// the same tax.
///
/// Backing off keeps both ends honest: a command that finishes in a
/// millisecond is noticed in a fraction of one, and a `cargo test` that
/// runs for minutes is still only checked on 200 times a second. If a
/// tool ever needs the exact exit instant, the answer is a condvar
/// signalled by the drain threads, not a smaller number here.
const POLL_MIN: Duration = Duration::from_micros(200);
const POLL_MAX: Duration = Duration::from_millis(5);

impl ProcessRunner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let started = Instant::now();
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &cmd.cwd {
            command.current_dir(dir);
        }
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }

        tracing::debug!(cmd = %cmd.display(), "spawn");
        let mut child = command.spawn().map_err(|e| {
            Error::internal(format!("cannot spawn {}", cmd.program.to_string_lossy()))
                .with_source(e)
        })?;

        // Drain both pipes on their own threads. Without this a child that
        // writes more than the pipe buffer (64 KiB on macOS) before exiting
        // blocks forever, and the timeout below would then kill a process
        // that was only waiting for us to read.
        let stdin_writer = child
            .stdin
            .take()
            .zip(cmd.stdin.clone())
            .map(|(mut pipe, bytes)| {
                thread::spawn(move || {
                    // A child that exits without reading gives EPIPE; that is
                    // its business, not an error here.
                    let _ = pipe.write_all(&bytes);
                })
            });
        let stdout_reader = drain(child.stdout.take());
        let stderr_reader = drain(child.stderr.take());

        let deadline = started + cmd.timeout;
        let mut timed_out = false;
        let mut poll = POLL_MIN;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                tracing::warn!(cmd = %cmd.display(), timeout = ?cmd.timeout, "timeout, killing");
                // kill() fails only if the child already exited; wait()
                // below picks up the status either way.
                let _ = child.kill();
                break child.wait()?;
            }
            thread::sleep(poll);
            poll = (poll * 2).min(POLL_MAX);
        };

        if let Some(writer) = stdin_writer {
            join(writer)?;
        }
        let stdout = join(stdout_reader)?;
        let stderr = join(stderr_reader)?;

        Ok(Output {
            exit_code: status.code(),
            stdout,
            stderr,
            timed_out,
            duration: started.elapsed(),
        })
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            // A read error here means the pipe broke; what was read so far
            // is still the best account of what the child said.
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

fn join<T>(handle: JoinHandle<T>) -> Result<T> {
    handle
        .join()
        .map_err(|_| Error::internal("process I/O thread panicked"))
}

/// Scripted runner for tests. Returns queued outputs in order and records
/// every command it was asked to run.
#[derive(Debug, Default)]
pub struct FakeRunner {
    outputs: Mutex<VecDeque<Output>>,
    calls: Mutex<Vec<Cmd>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        FakeRunner::default()
    }

    /// Queue the output for the next call.
    pub fn push(&self, output: Output) {
        self.outputs
            .lock()
            .expect("FakeRunner poisoned")
            .push_back(output);
    }

    /// Everything that was run, in order.
    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.lock().expect("FakeRunner poisoned").clone()
    }
}

impl ProcessRunner for FakeRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.calls
            .lock()
            .expect("FakeRunner poisoned")
            .push(cmd.clone());
        self.outputs
            .lock()
            .expect("FakeRunner poisoned")
            .pop_front()
            .ok_or_else(|| {
                Error::internal(format!(
                    "FakeRunner: no scripted output for `{}`",
                    cmd.display()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// `yes` never ends on its own. If `Flow::Stop` did not kill the child,
    /// this test would run until the harness gave up -- which is exactly
    /// what `search_text` would do to a Claude session against a monorepo.
    #[test]
    fn stop_kills_the_child_instead_of_waiting_it_out() {
        let mut seen = 0;
        let started = Instant::now();
        let out = stream_lines(&Cmd::new("yes").arg("line"), |line| {
            assert_eq!(line, b"line");
            seen += 1;
            if seen == 100 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        })
        .unwrap();
        assert_eq!(seen, 100);
        assert!(out.stopped_early);
        assert!(!out.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    /// The timeout has to reach the command's own children. A shell that
    /// starts a background process and waits leaves that process holding
    /// the pipes, so killing only the leader means the drain threads read
    /// on until the grandchild finishes -- a timeout that does not time
    /// out. Both entry points get the same test.
    #[test]
    fn a_timeout_reaches_the_grandchildren_too() {
        for label in ["stream", "capture"] {
            let cmd = Cmd::new("sh")
                .args(["-c", "sleep 30 & echo started; wait"])
                .timeout(Duration::from_millis(400));
            let started = Instant::now();
            let timed_out = if label == "stream" {
                stream_lines(&cmd, |_| Flow::Continue).unwrap().timed_out
            } else {
                run_captured(&cmd, std::io::sink(), std::io::sink())
                    .unwrap()
                    .timed_out
            };
            assert!(timed_out, "{label}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{label} waited {:?} for a grandchild",
                started.elapsed()
            );
        }
    }

    #[test]
    fn run_captured_writes_both_streams_and_counts_them() {
        let out = std::sync::Arc::new(Mutex::new(Vec::new()));
        let err = std::sync::Arc::new(Mutex::new(Vec::new()));

        struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let captured = run_captured(
            &Cmd::new("sh").args(["-c", "printf out; printf err >&2; exit 5"]),
            Shared(std::sync::Arc::clone(&out)),
            Shared(std::sync::Arc::clone(&err)),
        )
        .unwrap();
        assert_eq!(captured.exit_code, Some(5));
        assert!(!captured.timed_out);
        assert_eq!(captured.stdout_bytes, 3);
        assert_eq!(captured.stderr_bytes, 3);
        assert_eq!(&*out.lock().unwrap(), b"out");
        assert_eq!(&*err.lock().unwrap(), b"err");
    }

    #[test]
    fn a_command_that_ends_by_itself_reports_its_code_and_stderr() {
        let mut lines = Vec::new();
        let out = stream_lines(
            &Cmd::new("sh").args(["-c", "printf 'a\\nb\\n'; echo oops >&2; exit 7"]),
            |line| {
                lines.push(String::from_utf8_lossy(line).into_owned());
                Flow::Continue
            },
        )
        .unwrap();
        assert_eq!(lines, ["a", "b"]);
        assert_eq!(out.exit_code, Some(7));
        assert!(!out.stopped_early);
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "oops");
    }

    #[test]
    fn a_silent_child_is_killed_at_the_deadline() {
        let started = Instant::now();
        let out = stream_lines(
            &Cmd::new("sleep")
                .arg("30")
                .timeout(Duration::from_millis(300)),
            |_| Flow::Continue,
        )
        .unwrap();
        assert!(out.timed_out);
        assert!(!out.stopped_early);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stderr_is_kept_bounded() {
        let out = stream_lines(
            &Cmd::new("sh").args([
                "-c",
                "i=0; while [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789 >&2; i=$((i+1)); done",
            ]),
            |_| Flow::Continue,
        )
        .unwrap();
        assert!(
            out.stderr.len() <= STREAM_STDERR_KEEP,
            "{}",
            out.stderr.len()
        );
        // The tail is what is kept, so the last line is intact.
        assert!(String::from_utf8_lossy(&out.stderr).ends_with("0123456789\n"));
    }

    #[test]
    fn an_unspawnable_program_is_an_error_not_a_hang() {
        let err = stream_lines(&Cmd::new("ccnm-definitely-not-a-program"), |_| {
            Flow::Continue
        })
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal);
        assert!(
            err.message().contains("ccnm-definitely-not-a-program"),
            "{err}"
        );
    }

    fn sh(script: &str) -> Cmd {
        Cmd::new("sh").arg("-c").arg(script)
    }

    #[test]
    fn captures_streams_and_exit_code() {
        let out = SystemRunner
            .run(&sh("echo out; echo err >&2; exit 3"))
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout_lossy(), "out\n");
        assert_eq!(out.stderr_lossy(), "err\n");
        assert!(!out.timed_out);
        assert!(!out.success());
    }

    #[test]
    fn success_requires_zero_exit() {
        let out = SystemRunner.run(&sh("true")).unwrap();
        assert!(out.success());
        assert_eq!(out.exit_code, Some(0));
    }

    #[test]
    fn stdin_is_piped_when_given_and_null_otherwise() {
        let out = SystemRunner.run(&Cmd::new("cat").stdin("hello")).unwrap();
        assert_eq!(out.stdout_lossy(), "hello");

        // With no stdin the child reads EOF immediately instead of waiting
        // on the terminal.
        let out = SystemRunner.run(&Cmd::new("cat")).unwrap();
        assert!(out.success());
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn env_is_set_and_removed() {
        let out = SystemRunner
            .run(
                &sh("echo ${CCNM_TEST_VAR}-${HOME:-unset}")
                    .env("CCNM_TEST_VAR", "v")
                    .env_remove("HOME"),
            )
            .unwrap();
        assert_eq!(out.stdout_lossy(), "v-unset\n");
    }

    #[test]
    fn cwd_is_applied() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let out = SystemRunner.run(&Cmd::new("pwd").cwd(&dir)).unwrap();
        assert_eq!(out.stdout_lossy().trim(), dir.to_string_lossy());
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = SystemRunner
            .run(&sh(
                "head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2",
            ))
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.len(), 300_000);
        assert_eq!(out.stderr.len(), 300_000);
    }

    #[test]
    fn timeout_kills_the_child() {
        let out = SystemRunner
            .run(&sh("sleep 10").timeout(Duration::from_millis(100)))
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
        assert_eq!(out.exit_code, None, "killed by signal, no exit code");
        assert!(out.duration < Duration::from_secs(5), "{:?}", out.duration);
    }

    #[test]
    fn missing_program_is_an_internal_error_naming_it() {
        let err = SystemRunner
            .run(&Cmd::new("ccnm-definitely-not-installed"))
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal);
        assert!(
            err.message().contains("ccnm-definitely-not-installed"),
            "{err}"
        );
    }

    #[test]
    fn display_joins_program_and_args() {
        let cmd = Cmd::new("ssh").args(["-T", "ccnm-home", "ccnm", "runner", "health"]);
        assert_eq!(cmd.display(), "ssh -T ccnm-home ccnm runner health");
    }

    #[test]
    fn fake_runner_replays_and_records() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "first"));
        fake.push(Output::exited(1, "second"));

        let a = fake.run(&Cmd::new("a").arg("1")).unwrap();
        let b = fake.run(&Cmd::new("b")).unwrap();
        assert_eq!(a.stdout_lossy(), "first");
        assert!(a.success());
        assert_eq!(b.exit_code, Some(1));

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].display(), "a 1");
        assert_eq!(calls[1].display(), "b");

        let err = fake.run(&Cmd::new("c")).unwrap_err();
        assert!(
            err.message().contains("no scripted output for `c`"),
            "{err}"
        );
    }
}

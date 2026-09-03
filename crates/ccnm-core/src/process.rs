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

/// Runs commands for real.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

/// How often to check whether the child has exited. Bounds the latency
/// added to every command; 5 ms is invisible next to an SSH round trip.
const POLL: Duration = Duration::from_millis(5);

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
            thread::sleep(POLL);
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

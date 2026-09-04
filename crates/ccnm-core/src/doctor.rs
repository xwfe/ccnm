//! `ccnm doctor [WORKSPACE]`: is this machine and workspace ready to use?
//!
//! Every check is one row: name, status, a line of detail. Four statuses:
//!
//! ```text
//! OK     verified
//! WARN   verified, with something worth reading; does not block READY
//! SKIP   not verified (prerequisite failed, or not implemented yet); blocks
//! FAIL   verified broken, with a CCNM_E_* code and a fix hint; blocks
//! ```
//!
//! The exit code is the error code of the first FAIL row. With no FAIL but
//! at least one SKIP it is `CCNM_E_NOT_READY` (3): nothing is known to be
//! broken, but the workspace is not proven usable either, and `ccnm run`
//! must be able to tell those two apart. Only OK/WARN rows exit 0.
//!
//! The home machine checks what it can see on its own (config, the project
//! root, the ccnm binary the work machine will invoke back here, how the
//! work alias resolves), then makes one `ccnm internal probe` call to the
//! work machine and renders a row per fact it brings back: its ccnm,
//! Claude and its login, and the reverse ssh's hello from this machine.
//! The MCP handshake and everything after it stay SKIP until their phase
//! lands, and a SKIP still blocks READY.
//!
//! # Invariant: doctor is read-only
//!
//! Nothing in this module may install a binary, write a file, start an SSH
//! master, or leave a process behind. `ccnm run`, cron and CI call doctor
//! repeatedly; a check that fixes things as a side effect makes two runs
//! disagree and hides whether the environment was broken before doctor
//! ran. Every ssh here uses [`crate::ssh::Master::Reuse`]
//! (`ControlMaster=no`), which reuses an existing master but never creates
//! one (design doc section 4).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude;
use crate::config::{Backend, Config, Resolved};
use crate::error::{Error, ErrorCode, ErrorReport};
use crate::mcp::context;
use crate::paths;
use crate::process::{Cmd, ProcessRunner};
use crate::protocol::PROTOCOL;
use crate::protocol::hello::HelloReport;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::safety;
use crate::ssh::{Master, Ssh};

/// What doctor needs from its surroundings. Injected so tests can script
/// every external command.
pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine. Only read: doctor
    /// reuses a master if one exists and never creates the directory.
    pub control_dir: PathBuf,
    /// This user's home, for expanding the `~/` in a remote ccnm path.
    pub home: PathBuf,
    /// What the account this machine's runtime runs as can reach.
    ///
    /// Passed in rather than computed here so doctor stays a pure
    /// function of its inputs: the audit runs `id` and `sudo -n`, and a
    /// test that had to script those into the same queue as every ssh
    /// would be asserting on command ordering instead of on behaviour.
    pub audit: safety::Audit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    /// Not performed: a prerequisite failed or the check is not implemented
    /// yet. Blocks READY, but has no error code of its own; the report
    /// maps "only SKIPs" to [`ErrorCode::NotReady`].
    Skip,
    Fail(ErrorCode),
}

impl Status {
    fn label(&self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Skip => "SKIP",
            Status::Fail(_) => "FAIL",
        }
    }

    fn blocks(&self) -> bool {
        matches!(self, Status::Skip | Status::Fail(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Warn,
            detail: detail.into(),
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Skip,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, err: &Error) -> Self {
        Check::fail_with(name, err.code(), err.message())
    }

    fn fail_report(name: &'static str, err: &ErrorReport) -> Self {
        Check::fail_with(name, err.code(), &err.message)
    }

    fn fail_with(name: &'static str, code: ErrorCode, detail: impl AsRef<str>) -> Self {
        Check {
            name,
            status: Status::Fail(code),
            detail: format!("{}: {}", code.name(), detail.as_ref()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// What was examined: a workspace name, or "config" when none was given.
    pub subject: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn ready(&self) -> bool {
        self.blocking_code().is_none()
    }

    /// The code the process should exit with.
    ///
    /// ```text
    /// any FAIL            -> the first FAIL's code
    /// no FAIL, any SKIP   -> CCNM_E_NOT_READY
    /// only OK / WARN      -> none (exit 0)
    /// ```
    ///
    /// FAIL wins over SKIP regardless of row order: a real failure is more
    /// useful to act on than "could not check".
    pub fn blocking_code(&self) -> Option<ErrorCode> {
        let first_fail = self.checks.iter().find_map(|c| match c.status {
            Status::Fail(code) => Some(code),
            _ => None,
        });
        first_fail.or_else(|| {
            self.checks
                .iter()
                .any(|c| c.status.blocks())
                .then_some(ErrorCode::NotReady)
        })
    }

    pub fn exit_code(&self) -> i32 {
        self.blocking_code().map_or(0, ErrorCode::exit_code)
    }

    /// The table from design doc section 4, ending in READY or NOT READY.
    pub fn render(&self) -> String {
        const NAME_WIDTH: usize = 24;
        const STATUS_WIDTH: usize = 7;

        let mut out = format!("ccnm doctor: {}\n\n", self.subject);
        for check in &self.checks {
            let mut lines = check.detail.lines();
            let first = lines.next().unwrap_or("");
            let _ = writeln!(
                out,
                "{:<NAME_WIDTH$}{:<STATUS_WIDTH$}{first}",
                check.name,
                check.status.label()
            );
            for line in lines {
                let _ = writeln!(
                    out,
                    "{:width$}{line}",
                    "",
                    width = NAME_WIDTH + STATUS_WIDTH
                );
            }
        }

        let failed = self.count(|s| matches!(s, Status::Fail(_)));
        let skipped = self.count(|s| matches!(s, Status::Skip));
        out.push('\n');
        if self.ready() {
            out.push_str("READY\n");
        } else {
            let _ = writeln!(out, "NOT READY ({failed} failed, {skipped} not checked)");
        }
        out
    }

    fn count(&self, pred: impl Fn(&Status) -> bool) -> usize {
        self.checks.iter().filter(|c| pred(&c.status)).count()
    }
}

/// Run every check this build can perform.
pub fn run(config_path: &Path, workspace: Option<&str>, env: &Env<'_>) -> Report {
    let subject = workspace.unwrap_or("config").to_string();
    let mut checks = Vec::new();

    let config = match Config::load(config_path) {
        Ok(config) => {
            checks.push(Check::ok("Config", config_path.display().to_string()));
            config
        }
        Err(err) => {
            checks.push(Check::fail("Config", &err));
            return Report { subject, checks };
        }
    };

    match workspace {
        None => {
            let names: Vec<&str> = config.workspaces.keys().map(String::as_str).collect();
            let detail = if names.is_empty() {
                "none defined".to_string()
            } else {
                names.join(", ")
            };
            checks.push(Check::ok("Workspaces", detail));
        }
        Some(name) => match config.workspace(name) {
            Ok(resolved) => {
                checks.push(Check::ok(
                    "Workspace config",
                    format!(
                        "backend={} work_host={} (ssh {}), runtime_host={} (ssh_from_work {})",
                        resolved.workspace.backend.as_str(),
                        resolved.workspace.work_host,
                        resolved.work_ssh,
                        resolved.workspace.runtime_host,
                        resolved.home_alias
                    ),
                ));
                checks.extend(workspace_checks(&resolved, env));
            }
            Err(err) => checks.push(Check::fail("Workspace config", &err)),
        },
    }

    Report { subject, checks }
}

fn workspace_checks(r: &Resolved<'_>, env: &Env<'_>) -> Vec<Check> {
    let ws = r.workspace;
    if ws.backend == Backend::HybridSmb {
        return vec![Check::fail_with(
            "Backend",
            ErrorCode::Config,
            "backend = \"hybrid-smb\" is parsed but not implemented by this build\nsee design doc appendix A; use backend = \"mcp-ssh\"",
        )];
    }

    let mut checks = vec![
        home_workspace(&ws.root),
        project_instructions(r),
        home_ccnm(r, env),
    ];
    // Before anything that needs the network: this is an audit of the
    // local account, and it is exactly as true when the work machine is
    // unreachable.
    checks.extend(runtime_safety_rows(env, r));

    let ssh = match Ssh::new(r.work_ssh, &env.control_dir).and_then(|ssh| {
        ssh.check_control_path()?;
        Ok(ssh.with_ccnm_bin(r.work.ccnm_bin()))
    }) {
        Ok(ssh) => ssh,
        Err(e) => {
            checks.push(Check::fail("Work SSH", &e));
            checks.extend(skipped_after_work_ssh());
            checks.extend(not_yet_implemented());
            return checks;
        }
    };
    let resolved = match ssh.resolve(env.runner) {
        Ok(resolved) => resolved,
        Err(e) => {
            checks.push(Check::fail_with(
                "Work SSH",
                ErrorCode::WorkUnreachable,
                e.message(),
            ));
            checks.extend(skipped_after_work_ssh());
            checks.extend(not_yet_implemented());
            return checks;
        }
    };

    let req = ProbeRequest {
        protocol: PROTOCOL,
        workspace: r.name.to_string(),
        root: ws.root.clone(),
        home_alias: r.home_alias.to_string(),
        home_ccnm_bin: r.runtime.ccnm_bin(),
        claude_config_dir: r.work.claude_config_dir.clone(),
        // One real MCP session, shut down before the probe returns.
        mcp_calls: 1,
    };
    match ssh.call_ccnm::<_, ProbeReport>(
        env.runner,
        Master::Reuse,
        &["internal", "probe"],
        &req,
        Duration::from_secs(90),
        ErrorCode::WorkUnreachable,
    ) {
        Ok(rep) => {
            checks.push(Check::ok("Work SSH", resolved.target()));
            checks.extend(probe_rows(r, &rep));
        }
        Err(e) => {
            checks.push(Check::fail("Work SSH", &e));
            checks.extend(skipped_after_work_ssh());
        }
    }

    checks.extend(not_yet_implemented());
    checks
}

fn probe_rows(r: &Resolved<'_>, rep: &ProbeReport) -> Vec<Check> {
    let mut checks = vec![version_row("Work ccnm", &rep.hello, "work")];

    checks.push(controller_row(rep));

    checks.push(match &rep.claude.version {
        Ok(v) => {
            let path = rep
                .claude
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Check::ok("Claude Code", format!("{v} ({path})"))
        }
        Err(e) => Check::fail_report("Claude Code", e),
    });

    checks.push(auth_row(r, rep));

    match &rep.home_hello {
        Ok(h) => {
            checks.push(match version_row("Reverse SSH", h, "the runtime host") {
                ok if ok.status == Status::Ok => Check::ok(
                    "Reverse SSH",
                    format!("{} as {}, ccnm {}", r.home_alias, h.user, h.ccnm_version),
                ),
                fail => fail,
            });
            checks.push(mcp_row(rep));
            checks.push(match h.root {
                Some(status) if status.is_ok() => Check::ok(
                    "Workspace root",
                    format!(
                        "{} is a directory for {}",
                        r.workspace.root.display(),
                        h.user
                    ),
                ),
                Some(status) => Check::fail_with(
                    "Workspace root",
                    ErrorCode::WrongWorkspace,
                    format!(
                        "{} is {} for {} on {}",
                        r.workspace.root.display(),
                        status.describe(),
                        h.user,
                        r.home_alias
                    ),
                ),
                None => Check::warn(
                    "Workspace root",
                    "the runtime host's hello did not report the root",
                ),
            });
        }
        Err(e) => {
            checks.push(Check::fail_report("Reverse SSH", e));
            checks.push(Check::skip(
                "Remote MCP handshake",
                "not checked: reverse SSH failed",
            ));
            checks.push(Check::skip(
                "Workspace root",
                "not checked: reverse SSH failed",
            ));
        }
    }

    checks
}

/// One MCP session over the reverse ssh: initialize, tools/list, and a
/// `workspace_info` that must come back from a single server process.
fn mcp_row(rep: &ProbeReport) -> Check {
    const NAME: &str = "Remote MCP handshake";
    match &rep.mcp {
        None => Check::skip(NAME, "not requested"),
        Some(Ok(m)) if !m.single_process => Check::fail_with(
            NAME,
            ErrorCode::Internal,
            format!(
                "the server's pid or call counter changed during {} call(s); the transport is not one persistent process",
                m.calls
            ),
        ),
        Some(Ok(m)) => Check::ok(NAME, m.summary()),
        Some(Err(e)) => Check::fail_report(NAME, e),
    }
}

/// OK when the other side runs this build, else CCNM_E_VERSION. Both
/// machines must run the same binary (design doc section 7).
/// What the account this machine's runtime runs as can reach.
///
/// Doctor runs on the home machine, which is the runtime host, so this is
/// an audit of the account that would actually execute `exec_command`.
/// One row per finding, because "the runtime is not confined" is not
/// something anyone can act on and "this account is in the admin group,
/// remove it" is.
///
/// A failure is a FAIL row, not a SKIP: nothing is unknown here. The
/// property was checked and it does not hold.
fn runtime_safety_rows(env: &Env<'_>, r: &Resolved<'_>) -> Vec<Check> {
    let audit = &env.audit;
    // A workspace that has accepted an unconfined runtime gets warnings,
    // not failures. The runtime will run its commands either way, and a
    // table that says NOT READY about a session that works is a table
    // people learn to ignore.
    let accepted = r.workspace.allow_unconfined_exec;
    let mut rows: Vec<Check> = audit
        .findings
        .iter()
        .map(|finding| {
            let detail = match &finding.fix {
                Some(fix) => format!("{}\nfix: {fix}", finding.detail),
                None => finding.detail.clone(),
            };
            let name = safety_row_name(&finding.check);
            match finding.severity {
                safety::Severity::Ok => Check::ok(name, detail),
                safety::Severity::Warn => Check::warn(name, detail),
                safety::Severity::Fail if accepted => Check::warn(name, detail),
                safety::Severity::Fail => Check::fail_with(name, ErrorCode::Policy, detail),
            }
        })
        .collect();
    // The verdict the runtime's own gate uses, so this table and the
    // session cannot disagree about whether commands will run.
    rows.push(if audit.confined() {
        Check::ok("exec_command", "the runtime account is confined")
    } else if accepted {
        Check::warn(
            "exec_command",
            "allowed, but the runtime is NOT confined: this workspace sets allow_unconfined_exec",
        )
    } else {
        Check::fail_with(
            "exec_command",
            ErrorCode::Policy,
            "refused until the runtime account is confined; see docs/production-safety.md",
        )
    });
    rows
}

/// `Check::name` is `&'static str` because every other row's name is a
/// literal. The audit's names are literals too, so they are mapped back
/// rather than leaked.
fn safety_row_name(check: &str) -> &'static str {
    match check {
        "Runs as root" => "Runs as root",
        "Runtime user" => "Runtime user",
        "No sudo" => "No sudo",
        "Not an admin" => "Not an admin",
        "No SSH keys" => "No SSH keys",
        "No Claude credential" => "No Claude credential",
        "No Docker socket" => "No Docker socket",
        "Anthropic egress" => "Anthropic egress",
        _ => "Runtime safety",
    }
}

fn version_row(name: &'static str, hello: &HelloReport, side: &str) -> Check {
    if hello.ccnm_version == crate::VERSION {
        let exe = hello
            .exe
            .as_ref()
            .map(|p| format!(" at {}", p.display()))
            .unwrap_or_default();
        Check::ok(name, format!("{}{exe}", hello.ccnm_version))
    } else {
        Check::fail_with(
            name,
            ErrorCode::Version,
            format!(
                "{side} runs ccnm {}, this machine runs {}; install the same build on both",
                hello.ccnm_version,
                crate::VERSION
            ),
        )
    }
}

/// The controller: is there one, and is it somewhere useful?
///
/// Three outcomes, and the middle one is why this row exists at all. A
/// controller running outside the login session answers every request and
/// is still useless, which no other row would have caught.
fn controller_row(rep: &ProbeReport) -> Check {
    const NAME: &str = "Work controller";
    match &rep.controller {
        None => Check::skip(NAME, "that ccnm build does not have one"),
        Some(Err(e)) if e.code() == ErrorCode::NotReady => Check::skip(NAME, &e.message),
        Some(Err(e)) => Check::fail_report(NAME, e),
        Some(Ok(ctx)) if !ctx.login_session() => Check::fail_with(
            NAME,
            ErrorCode::NotReady,
            format!(
                "{}\nit answers, but not from a login session, so Claude started there could not read its own credentials\nrun on work: ccnm work-controller install",
                ctx.describe()
            ),
        ),
        Some(Ok(ctx)) => Check::ok(NAME, ctx.describe()),
    }
}

/// Claude's login, and — just as important — whether the answer is worth
/// anything.
///
/// "Not logged in" only means that when it came from a login session.
/// From anywhere else it is the same false negative the controller exists
/// to remove, so it reads as SKIP pointing at the controller's own row.
///
/// A *positive* answer is trusted from anywhere: a session that could not
/// reach the credentials could not have found a login to report. The
/// error runs one way only.
fn auth_row(r: &Resolved<'_>, rep: &ProbeReport) -> Check {
    const NAME: &str = "Claude authentication";
    let from_login_session = matches!(&rep.controller, Some(Ok(ctx)) if ctx.login_session());
    match &rep.claude.auth {
        Ok(a) if a.logged_in => Check::ok(NAME, a.describe()),
        Ok(_) if !from_login_session => Check::skip(
            NAME,
            "a controller answered, but not from a login session, so \"not logged in\" here means nothing\nfix the Work controller row first",
        ),
        Ok(_) => Check::fail_with(NAME, ErrorCode::Auth, auth_hint(r)),
        // "Nobody asked the right process" is not a diagnosis about
        // Claude. SKIP still blocks READY, so nothing runs on the strength
        // of an unchecked login.
        Err(e) if e.code() == ErrorCode::NotReady => Check::skip(NAME, &e.message),
        Err(e) => Check::fail_report(NAME, e),
    }
}

/// Design doc section 21: report, point at the manual login, never log in.
///
/// Only reached when a login session gave the answer, so the reading is
/// unambiguous. There is no "…or maybe the Keychain was unreadable" left
/// in it, which was the whole point of the controller.
fn auth_hint(r: &Resolved<'_>) -> String {
    const WHERE: &str = "asked from the work machine's login session, so this is Claude's real answer, not an artefact of ssh";
    match &r.work.claude_config_dir {
        Some(dir) => format!(
            "Claude is not authenticated in the configured CLAUDE_CONFIG_DIR ({WHERE})\nrun on work, on its own screen: CLAUDE_CONFIG_DIR={} claude auth login",
            dir.display()
        ),
        None => format!(
            "Claude is not authenticated on the work machine ({WHERE})\nrun on work, on its own screen: claude auth login\nan expired OAuth session looks the same as never having logged in; either way the fix is that command"
        ),
    }
}

/// Rows that depend on the probe, when the probe never happened.
fn skipped_after_work_ssh() -> Vec<Check> {
    const REASON: &str = "not checked: work SSH failed";
    [
        "Work ccnm",
        "Work controller",
        "Claude Code",
        "Claude authentication",
        "Reverse SSH",
        "Remote MCP handshake",
        "Workspace root",
    ]
    .into_iter()
    .map(|name| Check::skip(name, REASON))
    .collect()
}

/// What the project's own `CLAUDE.md` contributes to a session (design doc
/// section 20).
///
/// Checked here, on the runtime host, because this is the machine that has
/// the file and the machine the MCP server reads it from: the row is about
/// the same bytes the model will be given. Reading it is read-only, so it
/// belongs in doctor.
///
/// No CLAUDE.md is OK — most projects have none, and the model still gets
/// ccnm's own instructions. A file too big for the handshake is a WARN,
/// not a FAIL: the session works, the model just does not see all of it,
/// and that is exactly the kind of thing nobody discovers on their own.
fn project_instructions(r: &Resolved<'_>) -> Check {
    const NAME: &str = "Project instructions";
    let root = &r.workspace.root;
    let file = context::PROJECT_FILE;
    match context::find(root, context::budget(r.name)) {
        Ok(None) => Check::ok(
            NAME,
            format!(
                "no {file} at {}; the session gets ccnm's own instructions only",
                root.display()
            ),
        ),
        Ok(Some(p)) if !p.truncated() => Check::ok(
            NAME,
            format!("{file}, {} bytes, all of it reaches the model", p.bytes),
        ),
        Ok(Some(p)) => Check::warn(
            NAME,
            format!(
                "{file} is {} bytes and only its first {} reach the model: the MCP handshake is capped at {} bytes\nmove what the model does not need out of the root file; it can still read the whole thing with read_file {file}",
                p.bytes,
                p.included(),
                context::MAX_INSTRUCTIONS_BYTES
            ),
        ),
        Err(e) => Check::warn(
            NAME,
            format!(
                "{}\nthe session will run without the project's own instructions",
                e.message()
            ),
        ),
    }
}

/// The project root must exist on this (home) machine.
fn home_workspace(root: &Path) -> Check {
    match std::fs::metadata(root) {
        Ok(meta) if meta.is_dir() => Check::ok("Home workspace", root.display().to_string()),
        Ok(_) => Check::fail_with(
            "Home workspace",
            ErrorCode::WrongWorkspace,
            format!("{} is not a directory", root.display()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Check::fail_with(
            "Home workspace",
            ErrorCode::WrongWorkspace,
            format!("{} does not exist on this machine", root.display()),
        ),
        Err(e) => Check::fail(
            "Home workspace",
            &Error::internal(format!("cannot stat {}", root.display())).with_source(e),
        ),
    }
}

/// The work machine will run `<runtime ccnm_bin> internal ...` over ssh on
/// this machine. Look at that exact path now, as this user, so a missing
/// or stale install is reported here instead of as a cryptic exit 127
/// from the other side.
fn home_ccnm(r: &Resolved<'_>, env: &Env<'_>) -> Check {
    const NAME: &str = "Home ccnm";
    let configured = r.runtime.ccnm_bin();
    let path = paths::expand_home(&configured, &env.home);
    if !claude::is_executable(&path) {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                "{configured} is not an executable on this machine, but the work machine will invoke it over ssh {}\ninstall this build there: cp $(which ccnm) {}   (or set hosts.{}.ccnm_bin)",
                r.home_alias,
                path.display(),
                r.workspace.runtime_host
            ),
        );
    }
    let cmd = Cmd::new(&path)
        .arg("--version")
        .timeout(Duration::from_secs(10));
    let out = match env.runner.run(&cmd) {
        Ok(out) => out,
        Err(e) => return Check::fail(NAME, &e.with_code(ErrorCode::Version)),
    };
    let stdout = out.stdout_lossy();
    let version = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    if !out.success() || version.is_empty() {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                "{} --version failed (exit {:?}): {}",
                path.display(),
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        );
    }
    if version != crate::VERSION {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                "{} is ccnm {version}, this one is {}; install the same build",
                path.display(),
                crate::VERSION
            ),
        );
    }
    Check::ok(NAME, format!("{version} at {}", path.display()))
}

/// Still to come, with the phase that will make each one real (design doc
/// section 26).
fn not_yet_implemented() -> Vec<Check> {
    [
        ("Workspace policy", "not implemented until phase 2"),
        // Every session is launched with `--tools ""` and an allow-list
        // (see `crate::session`), and that was verified by hand against
        // Claude Code 2.1.260. What doctor cannot do is prove it about the
        // Claude on the work machine without starting a session, so the
        // row stays SKIP rather than claiming a check it did not make.
        (
            "Native tools disabled",
            "not checked: only a live session shows which tools Claude ended up with",
        ),
        ("Runtime identity", "not implemented until phase 5"),
        ("Network isolation", "not implemented until phase 5"),
        ("Terminal session", "not implemented until phase 6"),
    ]
    .into_iter()
    .map(|(name, reason)| Check::skip(name, reason))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    /// A per-test directory with `root/` (created only if `with_root`), a
    /// fake `home/.local/bin/ccnm` (created only if `with_bin`) and a
    /// config pointing at them.
    fn setup(test: &str, with_root: bool, with_bin: bool) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ccnm-doctor-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("root");
        if with_root {
            std::fs::create_dir_all(&root).unwrap();
        }
        if with_bin {
            let bin_dir = dir.join("home/.local/bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bin = bin_dir.join("ccnm");
            std::fs::write(&bin, "#!/bin/sh\necho ccnm 0.1.0\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home]\nssh_from_work = \"ccnm-home\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\n",
                root.display()
            ),
        )
        .unwrap();
        (dir, config)
    }

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    fn env<'a>(fake: &'a FakeRunner, dir: &Path) -> Env<'a> {
        env_with(fake, dir, confined_audit())
    }

    fn env_with<'a>(fake: &'a FakeRunner, dir: &Path, audit: safety::Audit) -> Env<'a> {
        Env {
            runner: fake,
            control_dir: control(dir),
            home: dir.join("home"),
            audit,
        }
    }

    /// The audit a machine set up per docs/production-safety.md produces.
    fn confined_audit() -> safety::Audit {
        safety::Audit {
            user: "ccrun".into(),
            findings: vec![safety::Finding {
                check: "Runtime user".into(),
                severity: safety::Severity::Ok,
                detail: "ccrun".into(),
                fix: None,
            }],
        }
    }

    fn unconfined_audit() -> safety::Audit {
        safety::Audit {
            user: "fodelf".into(),
            findings: vec![safety::Finding {
                check: "No sudo".into(),
                severity: safety::Severity::Fail,
                detail: "this account has passwordless sudo".into(),
                fix: Some("remove it from the sudoers file".into()),
            }],
        }
    }

    fn hello(user: &str, version: &str, root_ok: Option<bool>) -> HelloReport {
        HelloReport {
            protocol: PROTOCOL,
            ccnm_version: version.into(),
            user: user.into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from(format!("/Users/{user}/.local/bin/ccnm"))),
            root: root_ok.map(|ok| crate::protocol::hello::PathStatus {
                exists: ok,
                is_dir: ok,
            }),
        }
    }

    /// A controller answering from the login session, which is the only
    /// context whose answer about Claude counts.
    fn controller(manager: &str) -> crate::controller::Context {
        crate::controller::Context {
            hello: hello("me", crate::VERSION, None),
            pid: 4711,
            manager: Ok(manager.to_string()),
        }
    }

    fn good_probe() -> ProbeReport {
        use crate::claude::AuthStatus;
        use crate::claude::ClaudeReport;
        ProbeReport {
            protocol: PROTOCOL,
            hello: hello("me", crate::VERSION, None),
            controller: Some(Ok(controller("Aqua"))),
            claude: ClaudeReport {
                path: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                version: Ok("2.1.259".into()),
                auth: Ok(AuthStatus {
                    logged_in: true,
                    auth_method: Some("claude.ai".into()),
                    email: Some("me@x".into()),
                    subscription_type: Some("max".into()),
                }),
            },
            home_ssh: Ok(crate::ssh::ResolvedSsh {
                hostname: "home.t.ts.net".into(),
                user: "ccrun".into(),
                port: 22,
                identity_files: vec![],
                proxy_jump: None,
            }),
            home_hello: Ok(hello("ccrun", crate::VERSION, Some(true))),
            mcp: Some(Ok(crate::protocol::mcp::ProbeReport {
                connect_us: 190_000,
                server_name: "ccnm".into(),
                server_version: crate::VERSION.into(),
                instructions_bytes: 180,
                project_instructions: Some("no CLAUDE.md at the workspace root".into()),
                tools: vec!["workspace_info".into()],
                tools_list_bytes: 412,
                calls: 1,
                call_p50_us: 22_000,
                call_p95_us: 22_000,
                call_max_us: 22_000,
                server_pid: 4242,
                single_process: true,
            })),
        }
    }

    fn row<'a>(report: &'a Report, name: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no row {name} in\n{}", report.render()))
    }

    #[test]
    fn missing_config_is_the_only_row_and_exits_config() {
        let fake = FakeRunner::new();
        let report = run(
            Path::new("/nonexistent/config.toml"),
            Some("xshun"),
            &env(&fake, Path::new("/tmp")),
        );
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].status, Status::Fail(ErrorCode::Config));
        assert_eq!(report.exit_code(), 10);
        let text = report.render();
        assert!(text.contains("CCNM_E_CONFIG"), "{text}");
        assert!(
            text.ends_with("NOT READY (1 failed, 0 not checked)\n"),
            "{text}"
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn config_only_run_is_ready_and_lists_workspaces() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-valid.toml"),
            None,
            &env(&fake, Path::new("/tmp")),
        );
        assert!(report.ready(), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
        let text = report.render();
        assert!(
            text.contains("Workspaces              OK     xshun"),
            "{text}"
        );
        assert!(text.ends_with("\nREADY\n"), "{text}");
    }

    #[test]
    fn unknown_workspace_fails_config() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-valid.toml"),
            Some("other"),
            &env(&fake, Path::new("/tmp")),
        );
        assert_eq!(report.exit_code(), 10);
        assert!(
            row(&report, "Workspace config")
                .detail
                .contains("defined: xshun")
        );
    }

    #[test]
    fn hybrid_backend_is_refused_by_this_build() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-hybrid.toml"),
            Some("legacy"),
            &env(&fake, Path::new("/tmp")),
        );
        assert_eq!(report.exit_code(), 10, "{}", report.render());
        let backend = row(&report, "Backend");
        assert!(backend.detail.contains("appendix A"), "{}", backend.detail);
        assert!(
            fake.calls().is_empty(),
            "nothing remote for a hybrid config"
        );
    }

    #[test]
    fn an_unconfined_runtime_fails_the_exec_row_and_the_whole_report() {
        let (dir, config) = setup("unconfined", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe()).unwrap(),
        ));
        let report = run(
            &config,
            Some("xshun"),
            &env_with(&fake, &dir, unconfined_audit()),
        );
        let text = report.render();
        // The finding itself, with its fix, and the verdict the runtime's
        // own gate will reach.
        assert_eq!(
            row(&report, "No sudo").status,
            Status::Fail(ErrorCode::Policy)
        );
        assert!(row(&report, "No sudo").detail.contains("fix: "), "{text}");
        assert_eq!(
            row(&report, "exec_command").status,
            Status::Fail(ErrorCode::Policy)
        );
        assert!(
            row(&report, "exec_command")
                .detail
                .contains("docs/production-safety.md"),
            "{text}"
        );
        assert_eq!(report.exit_code(), ErrorCode::Policy.exit_code(), "{text}");
    }

    /// The project's CLAUDE.md is what tells the model the project's
    /// rules, and a file too long for the handshake is invisible from the
    /// outside: the session runs, it just quietly knows less. So the row
    /// reports how many of its bytes reach the model, and being too long
    /// warns rather than fails.
    #[test]
    fn the_project_claude_md_row_measures_what_reaches_the_model() {
        let (dir, path) = setup("claudemd", true, true);
        let config = Config::load(&path).unwrap();
        let r = config.workspace("xshun").unwrap();
        let file = dir.join("root/CLAUDE.md");

        let none = project_instructions(&r);
        assert_eq!(none.status, Status::Ok);
        assert!(none.detail.starts_with("no CLAUDE.md at"), "{none:?}");

        std::fs::write(&file, "- 提交要小\n").unwrap();
        let small = project_instructions(&r);
        assert_eq!(small.status, Status::Ok);
        assert_eq!(
            small.detail,
            "CLAUDE.md, 15 bytes, all of it reaches the model"
        );

        std::fs::write(&file, "- 一条规则\n".repeat(4000)).unwrap();
        let big = project_instructions(&r);
        assert_eq!(big.status, Status::Warn);
        assert!(big.detail.contains("only its first"), "{big:?}");
        assert!(big.detail.contains("read_file CLAUDE.md"), "{big:?}");

        // There, but unreadable: still not a failure, and it says what
        // the session will be missing.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        let bad = project_instructions(&r);
        assert_eq!(bad.status, Status::Warn);
        assert!(bad.detail.contains("CLAUDE.md"), "{bad:?}");
    }

    #[test]
    fn everything_good_blocks_only_on_future_phases() {
        let (dir, config) = setup("good", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe()).unwrap(),
        ));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let text = report.render();
        for name in [
            "Config",
            "Workspace config",
            "Home workspace",
            "Project instructions",
            "Home ccnm",
            "Work SSH",
            "Work ccnm",
            "Work controller",
            "Claude Code",
            "Claude authentication",
            "Reverse SSH",
            "Remote MCP handshake",
            "Workspace root",
        ] {
            assert_eq!(row(&report, name).status, Status::Ok, "{name}:\n{text}");
        }
        assert_eq!(
            row(&report, "Work controller").detail,
            format!("ccnm {} as me, pid 4711, Aqua", crate::VERSION)
        );
        assert_eq!(
            row(&report, "Remote MCP handshake").detail,
            "initialize in 190 ms, tools/list (1 tool, 412 B), instructions 180 B (no CLAUDE.md at the workspace root), workspace_info x1 p50 22 ms p95 22 ms max 22 ms, pid 4242 throughout"
        );
        assert_eq!(
            row(&report, "Home ccnm").detail,
            format!("0.1.0 at {}", dir.join("home/.local/bin/ccnm").display())
        );
        assert_eq!(row(&report, "Work SSH").detail, "me@workmac");
        assert_eq!(
            row(&report, "Work ccnm").detail,
            format!("{} at /Users/me/.local/bin/ccnm", crate::VERSION)
        );
        assert_eq!(
            row(&report, "Claude authentication").detail,
            "me@x via claude.ai (max)"
        );
        assert_eq!(
            row(&report, "Reverse SSH").detail,
            format!("ccnm-home as ccrun, ccnm {}", crate::VERSION)
        );
        assert!(
            text.ends_with("NOT READY (0 failed, 5 not checked)\n"),
            "{text}"
        );
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);

        // Read-only: no control dir, nothing new in root.
        assert!(!control(&dir).exists());
        assert_eq!(std::fs::read_dir(dir.join("root")).unwrap().count(), 0);
        // The version probe ran the expanded ~ path, ssh -G, then one
        // probe with ControlMaster=no carrying the workspace facts. Three
        // commands and no more: the runtime audit is passed in, not run
        // here, so doctor's command list is still only its own.
        let calls = fake.calls();
        assert_eq!(
            calls.len(),
            3,
            "{:?}",
            calls.iter().map(Cmd::display).collect::<Vec<_>>()
        );
        assert_eq!(
            calls[0].display(),
            format!("{} --version", dir.join("home/.local/bin/ccnm").display())
        );
        assert_eq!(calls[1].display(), "ssh -G work");
        let probe = calls[2].display();
        assert!(probe.contains("ControlMaster=no"), "{probe}");
        assert!(
            probe.contains("-T work ~/.local/bin/ccnm internal probe --payload "),
            "{probe}"
        );
        let wire = calls[2].args.last().unwrap().to_string_lossy().into_owned();
        let sent: ProbeRequest = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(sent.workspace, "xshun");
        assert_eq!(sent.root, dir.join("root"));
        assert_eq!(sent.home_alias, "ccnm-home");
        assert_eq!(sent.home_ccnm_bin, "~/.local/bin/ccnm");
        assert_eq!(sent.mcp_calls, 1);
    }

    #[test]
    fn mcp_handshake_from_more_than_one_process_is_a_failure() {
        let (dir, config) = setup("mcp-pid", true, true);
        let mut probe = good_probe();
        if let Some(Ok(m)) = probe.mcp.as_mut() {
            m.single_process = false;
            m.calls = 3;
        }
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let mcp = row(&report, "Remote MCP handshake");
        assert_eq!(mcp.status, Status::Fail(ErrorCode::Internal));
        assert!(
            mcp.detail.contains("not one persistent process"),
            "{}",
            mcp.detail
        );
    }

    #[test]
    fn unreachable_work_fails_once_and_skips_the_rest() {
        let (dir, config) = setup("unreachable", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let mut down = Output::exited(255, "");
        down.stderr = b"ssh: connect to host workmac port 22: Operation timed out\n".to_vec();
        fake.push(down);

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert!(
            row(&report, "Work SSH")
                .detail
                .contains("Operation timed out")
        );
        assert_eq!(row(&report, "Claude Code").status, Status::Skip);
        assert_eq!(row(&report, "Workspace root").status, Status::Skip);
        let text = report.render();
        assert!(
            text.ends_with("NOT READY (1 failed, 12 not checked)\n"),
            "{text}"
        );
    }

    #[test]
    fn work_version_mismatch_logged_out_and_missing_root_are_named() {
        let (dir, config) = setup("mismatch", true, true);
        let mut probe = good_probe();
        probe.hello = hello("me", "0.0.1", None);
        probe.claude.auth = Ok(crate::claude::AuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            subscription_type: None,
        });
        probe.home_hello = Ok(hello("ccrun", crate::VERSION, Some(false)));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let work = row(&report, "Work ccnm");
        assert_eq!(work.status, Status::Fail(ErrorCode::Version));
        assert!(
            work.detail.contains("work runs ccnm 0.0.1"),
            "{}",
            work.detail
        );
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Fail(ErrorCode::Auth));
        assert!(auth.detail.contains("claude auth login"), "{}", auth.detail);
        // The answer came from the login session, so the row says so
        // instead of hedging about the Keychain.
        assert!(auth.detail.contains("login session"), "{}", auth.detail);
        let root = row(&report, "Workspace root");
        assert_eq!(root.status, Status::Fail(ErrorCode::WrongWorkspace));
        assert!(
            root.detail.contains("is missing for ccrun on ccnm-home"),
            "{}",
            root.detail
        );
        // First FAIL in table order decides.
        assert_eq!(report.exit_code(), 11);
    }

    /// No controller means nobody could ask Claude a question worth
    /// trusting. That has to read as "not checked", never as "logged out":
    /// the second sends someone to log in on a machine that already is.
    #[test]
    fn without_a_controller_the_login_is_unchecked_not_failed() {
        let (dir, config) = setup("no-controller", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Err(ErrorReport::new(
            ErrorCode::NotReady,
            "no socket at /Users/me/.local/state/ccnm/controller.sock\ninstall it on the work machine: ccnm work-controller install",
        )));
        probe.claude.auth = Err(ErrorReport::new(
            ErrorCode::NotReady,
            "not checked: no work controller to ask, and this ssh session's answer would be wrong",
        ));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let controller = row(&report, "Work controller");
        assert_eq!(controller.status, Status::Skip);
        assert!(
            controller.detail.contains("work-controller install"),
            "{}",
            controller.detail
        );
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Skip, "{}", auth.detail);
        // Claude itself was still found: the version needs no credential.
        assert_eq!(row(&report, "Claude Code").status, Status::Ok);
        // Unverified, so still not READY -- and not for an auth reason.
        assert_eq!(report.exit_code(), ErrorCode::NotReady.exit_code());
    }

    /// A controller in the wrong session answers everything and is still
    /// useless. Nothing else in the table would catch that.
    #[test]
    fn a_controller_outside_the_login_session_fails_its_row() {
        let (dir, config) = setup("bg-controller", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let controller = row(&report, "Work controller");
        assert_eq!(controller.status, Status::Fail(ErrorCode::NotReady));
        assert!(
            controller.detail.contains("not from a login session"),
            "{}",
            controller.detail
        );
        assert!(
            controller.detail.contains("Background"),
            "{}",
            controller.detail
        );
    }

    /// Caught on the real work machine: with a controller in the wrong
    /// session, the auth row still claimed to have asked the login session
    /// and failed on the answer. That is the same false negative the
    /// controller exists to remove, told with more confidence.
    #[test]
    fn a_logged_out_answer_from_the_wrong_session_is_not_a_verdict() {
        let (dir, config) = setup("bg-auth", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));
        probe.claude.auth = Ok(crate::claude::AuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            subscription_type: None,
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Skip, "{}", auth.detail);
        assert!(auth.detail.contains("means nothing"), "{}", auth.detail);
        assert!(
            !auth.detail.contains("claude auth login"),
            "must not send the user to log in on this evidence: {}",
            auth.detail
        );
        // The controller's own row is where the fix is.
        assert_eq!(
            row(&report, "Work controller").status,
            Status::Fail(ErrorCode::NotReady)
        );
    }

    /// The asymmetry: a session that could not reach the credentials could
    /// not have found a login to report, so a positive answer is trusted
    /// wherever it came from.
    #[test]
    fn a_logged_in_answer_is_trusted_from_any_session() {
        let (dir, config) = setup("bg-auth-ok", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(row(&report, "Claude authentication").status, Status::Ok);
    }

    #[test]
    fn reverse_ssh_failure_is_reported_from_the_probe() {
        let (dir, config) = setup("reverse", true, true);
        let mut probe = good_probe();
        probe.home_hello = Err(ErrorReport::new(
            ErrorCode::HomeUnreachable,
            "ssh ccnm-home: Permission denied (publickey)",
        ));
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Fail(ErrorCode::HomeUnreachable));
        assert!(
            reverse.detail.contains("Permission denied"),
            "{}",
            reverse.detail
        );
        assert_eq!(row(&report, "Remote MCP handshake").status, Status::Skip);
        assert_eq!(row(&report, "Workspace root").status, Status::Skip);
        assert_eq!(report.exit_code(), 21);
    }

    #[test]
    fn missing_home_ccnm_names_the_path_and_the_fix() {
        let (dir, config) = setup("no-bin", true, false);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe()).unwrap(),
        ));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let bin = row(&report, "Home ccnm");
        assert_eq!(bin.status, Status::Fail(ErrorCode::Version));
        assert!(bin.detail.contains("~/.local/bin/ccnm"), "{}", bin.detail);
        assert!(bin.detail.contains("cp $(which ccnm)"), "{}", bin.detail);
        assert!(bin.detail.contains("hosts.home.ccnm_bin"), "{}", bin.detail);
        assert_eq!(report.exit_code(), 11);
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "no --version for a missing file");
        assert!(calls[0].display().starts_with("ssh -G"));
    }

    #[test]
    fn stale_home_ccnm_is_a_version_failure() {
        let (dir, config) = setup("stale-bin", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.0.9\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let bin = row(&report, "Home ccnm");
        assert_eq!(bin.status, Status::Fail(ErrorCode::Version));
        assert!(bin.detail.contains("is ccnm 0.0.9"), "{}", bin.detail);
    }

    #[test]
    fn missing_root_is_wrong_workspace() {
        let (dir, config) = setup("no-root", false, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.blocking_code(), Some(ErrorCode::WrongWorkspace));
        assert!(
            row(&report, "Home workspace")
                .detail
                .contains("does not exist on this machine")
        );
    }

    #[test]
    fn unresolvable_work_alias_is_work_unreachable() {
        let (dir, config) = setup("bad-alias", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        let mut failed = Output::exited(255, "");
        failed.stderr = b"work: Name or service not known\n".to_vec();
        fake.push(failed);
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert!(
            row(&report, "Work SSH")
                .detail
                .contains("Name or service not known")
        );
        assert_eq!(row(&report, "Reverse SSH").status, Status::Skip);
        assert_eq!(fake.calls().len(), 2, "no probe after ssh -G failed");
    }

    #[test]
    fn multi_line_detail_is_indented_under_the_detail_column() {
        let report = Report {
            subject: "x".into(),
            checks: vec![Check::fail("Config", &Error::config("line one\nline two"))],
        };
        let text = report.render();
        assert!(
            text.contains("Config                  FAIL   CCNM_E_CONFIG: line one\n"),
            "{text}"
        );
        assert!(
            text.contains("\n                               line two\n"),
            "{text}"
        );
    }

    fn report_of(statuses: &[Status]) -> Report {
        Report {
            subject: "x".into(),
            checks: statuses
                .iter()
                .map(|s| Check {
                    name: "row",
                    status: s.clone(),
                    detail: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn verdict_fail_beats_skip_whatever_the_order() {
        let report = report_of(&[Status::Skip, Status::Fail(ErrorCode::Mount), Status::Skip]);
        assert_eq!(report.blocking_code(), Some(ErrorCode::Mount));
        assert_eq!(report.exit_code(), 22);
        assert!(!report.ready());
        // The first FAIL decides when there are several.
        let report = report_of(&[
            Status::Fail(ErrorCode::Auth),
            Status::Fail(ErrorCode::Mount),
        ]);
        assert_eq!(report.exit_code(), 12);
    }

    #[test]
    fn verdict_skip_only_is_not_ready_3() {
        let report = report_of(&[Status::Ok, Status::Warn, Status::Skip]);
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);
        assert!(!report.ready());
        assert!(
            report
                .render()
                .ends_with("NOT READY (0 failed, 1 not checked)\n")
        );
    }

    #[test]
    fn verdict_warn_only_is_ready_0() {
        let report = report_of(&[Status::Ok, Status::Warn, Status::Warn]);
        assert_eq!(report.blocking_code(), None);
        assert_eq!(report.exit_code(), 0);
        assert!(report.ready());
        assert!(report.render().ends_with("\nREADY\n"));
    }

    #[test]
    fn verdict_ok_only_is_ready_0() {
        let report = report_of(&[Status::Ok, Status::Ok]);
        assert_eq!(report.exit_code(), 0);
        assert!(report.ready());
        // And an empty report has nothing blocking either.
        assert!(report_of(&[]).ready());
    }
}

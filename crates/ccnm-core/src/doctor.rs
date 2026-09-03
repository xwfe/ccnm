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
//! Phase 1 checks the whole transport: local root and identity, the SMB
//! share, Tailscale path, ssh to work, and through one `ccnm work probe`
//! call everything the work machine sees (mount, identity through the
//! mount, reverse ssh to the runner, Claude Code and its login). The
//! coherence and barrier rows stay SKIP until their phases land, and a SKIP
//! still blocks READY, so a half-built doctor cannot report a workspace
//! usable.
//!
//! # Invariant: doctor is read-only
//!
//! Nothing in this module may mount, write a workspace id, start an SSH
//! master, or touch a file. `ccnm run`, cron and CI call doctor repeatedly;
//! a check that fixes things as a side effect makes two runs disagree and
//! hides whether the environment was broken before doctor ran. State
//! changes belong to explicit subcommands (`ccnm mount`, `ccnm workspace
//! init`). Every ssh here uses [`Master::Reuse`] (`ControlMaster=no`),
//! which reuses an existing master but never creates one (design doc
//! section 4).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{Config, Resolved};
use crate::error::{Error, ErrorCode, ErrorReport, Reported};
use crate::identity::{self, WorkspaceId};
use crate::payload::PROTOCOL;
use crate::process::ProcessRunner;
use crate::runner::HealthReport;
use crate::smb;
use crate::ssh::{Master, Ssh};
use crate::tailscale::{self, Route};
use crate::work::{ProbeReport, ProbeRequest};

/// What doctor needs from its surroundings. Injected so tests can script
/// every external command and decide whether `tailscale` exists.
pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine. Only read: doctor
    /// reuses a master if one exists and never creates the directory.
    pub control_dir: PathBuf,
    pub tailscale: Option<PathBuf>,
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
                        "work_host={} (ssh {}), runner_host={} (ssh_from_work {})",
                        resolved.workspace.work_host,
                        resolved.work_ssh,
                        resolved.workspace.runner_host,
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
    let mut checks = vec![home_workspace(&ws.root)];

    let home_id = match identity::read(&ws.root) {
        Ok(Some(id)) => {
            checks.push(Check::ok("Workspace identity", id.to_string()));
            Some(id)
        }
        Ok(None) => {
            checks.push(Check::fail_with(
                "Workspace identity",
                ErrorCode::WrongWorkspace,
                format!(
                    "no {} in {}\nrun: ccnm workspace init {}",
                    identity::FILE_NAME,
                    ws.root.display(),
                    r.name
                ),
            ));
            None
        }
        Err(e) => {
            checks.push(Check::fail("Workspace identity", &e));
            None
        }
    };

    checks.push(smb_share(r, env));

    let ssh = match Ssh::new(r.work_ssh, &env.control_dir).and_then(|ssh| {
        ssh.check_control_path()?;
        Ok(ssh)
    }) {
        Ok(ssh) => ssh,
        Err(e) => {
            checks.push(Check::skip(
                "Tailscale",
                "not checked: work SSH is misconfigured",
            ));
            checks.push(Check::fail("Work SSH", &e));
            checks.extend(skipped_after_work_ssh());
            checks.extend(not_yet_implemented());
            return checks;
        }
    };
    let resolved = match ssh.resolve(env.runner) {
        Ok(resolved) => resolved,
        Err(e) => {
            checks.push(Check::skip("Tailscale", "not checked: ssh -G failed"));
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

    checks.push(tailscale_row(env, &resolved.hostname));

    let req = ProbeRequest {
        protocol: PROTOCOL,
        root: ws.root.clone(),
        runtime_root: ws.runtime_root.clone(),
        home_alias: r.home_alias.to_string(),
        claude_config_dir: r.work.claude_config_dir.clone(),
    };
    match ssh.call_ccnm::<_, ProbeReport>(
        env.runner,
        Master::Reuse,
        &["work", "probe"],
        &req,
        Duration::from_secs(90),
        ErrorCode::WorkUnreachable,
    ) {
        Ok(rep) => {
            checks.push(Check::ok("Work SSH", resolved.target()));
            checks.extend(probe_rows(r, &rep, home_id));
        }
        Err(e) => {
            checks.push(Check::fail("Work SSH", &e));
            checks.extend(skipped_after_work_ssh());
        }
    }

    checks.extend(not_yet_implemented());
    checks
}

/// The project root must exist on this (home) machine.
fn home_workspace(root: &Path) -> Check {
    match std::fs::metadata(root) {
        Ok(meta) if meta.is_dir() => Check::ok("Home workspace", root.display().to_string()),
        Ok(_) => Check::fail(
            "Home workspace",
            &Error::config(format!("{} is not a directory", root.display())),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Check::fail(
            "Home workspace",
            &Error::config(format!("{} does not exist on this machine", root.display())),
        ),
        Err(e) => Check::fail(
            "Home workspace",
            &Error::internal(format!("cannot stat {}", root.display())).with_source(e),
        ),
    }
}

/// Is this machine exporting the share, and does it export the root?
/// `sharing -l` may not list shares configured other ways, so "not listed"
/// is only a warning; the work-side mount row is authoritative.
fn smb_share(r: &Resolved<'_>, env: &Env<'_>) -> Check {
    let share = &r.workspace.share;
    let out = match env.runner.run(&smb::sharing_list_cmd()) {
        Ok(out) => out,
        Err(e) => {
            return Check::warn(
                "SMB share",
                format!("cannot run sharing -l: {}", e.message()),
            );
        }
    };
    if !out.success() {
        return Check::warn(
            "SMB share",
            format!("sharing -l failed: {}", out.stderr_lossy().trim()),
        );
    }
    let points = smb::parse_sharing_list(&out.stdout_lossy());
    match smb::find_share(&points, share) {
        None => Check::warn(
            "SMB share",
            format!(
                "no share point named \"{share}\" in `sharing -l`; the work-side mount check decides"
            ),
        ),
        Some(p) if p.path != r.workspace.root => Check::fail_with(
            "SMB share",
            ErrorCode::Mount,
            format!(
                "share \"{share}\" exports {} but the workspace root is {}",
                p.path.display(),
                r.workspace.root.display()
            ),
        ),
        Some(p) if !p.smb_shared => Check::fail_with(
            "SMB share",
            ErrorCode::Mount,
            format!("share \"{share}\" exists but SMB is switched off for it"),
        ),
        Some(p) => Check::ok("SMB share", format!("{share} -> {}", p.path.display())),
    }
}

/// Never blocking: ssh decides reachability, this only explains the path.
/// No CLI or no matching peer is OK (some other route is in use); a CLI
/// that is present but cannot answer is WARN.
fn tailscale_row(env: &Env<'_>, hostname: &str) -> Check {
    const NAME: &str = "Tailscale";
    let Some(bin) = &env.tailscale else {
        return Check::ok(NAME, "tailscale CLI not found; path not checked");
    };
    let out = match env.runner.run(&tailscale::status_cmd(bin)) {
        Ok(out) => out,
        Err(e) => return Check::warn(NAME, format!("cannot run tailscale: {}", e.message())),
    };
    if !out.success() {
        return Check::warn(
            NAME,
            format!("tailscale status failed: {}", out.stderr_lossy().trim()),
        );
    }
    match tailscale::find_peer(&out.stdout, hostname) {
        Err(e) => Check::warn(NAME, e.message().to_string()),
        Ok(None) => Check::ok(
            NAME,
            format!("{hostname} is not a Tailscale peer; assuming LAN or another route"),
        ),
        Ok(Some(peer)) if !peer.online => Check::warn(NAME, peer.describe()),
        Ok(Some(peer)) if matches!(peer.route, Route::Relay(_)) => {
            Check::warn(NAME, format!("{}; SMB will be slow", peer.describe()))
        }
        Ok(Some(peer)) => Check::ok(NAME, peer.describe()),
    }
}

fn probe_rows(r: &Resolved<'_>, rep: &ProbeReport, home_id: Option<WorkspaceId>) -> Vec<Check> {
    let mut checks = Vec::new();

    if rep.ccnm_version == crate::VERSION {
        checks.push(Check::ok("Work ccnm", rep.ccnm_version.clone()));
    } else {
        checks.push(Check::fail_with(
            "Work ccnm",
            ErrorCode::Version,
            format!(
                "work runs ccnm {}, home runs {}; install the same build on both",
                rep.ccnm_version,
                crate::VERSION
            ),
        ));
    }

    checks.push(match &rep.mount {
        Ok(m) if m.mounted => Check::ok("Work SMB mount", m.detail.clone()),
        Ok(_) => Check::fail_with(
            "Work SMB mount",
            ErrorCode::Mount,
            format!(
                "{} is not an SMB mount on work (path is {})\nrun: ccnm mount {}",
                r.workspace.root.display(),
                rep.root.describe(),
                r.name
            ),
        ),
        Err(e) => Check::fail_report("Work SMB mount", e),
    });

    checks.push(identity_row(
        "Work identity view",
        "work",
        &rep.identity,
        home_id,
        "through the mount",
    ));

    match &rep.health {
        Ok(h) => {
            checks.push(Check::ok(
                "Reverse SSH",
                format!("{} as {}", r.home_alias, h.user),
            ));
            checks.extend(runner_rows(r, h, home_id));
        }
        Err(e) => {
            checks.push(Check::fail_report("Reverse SSH", e));
            checks.push(Check::skip(
                "Home runner",
                "not checked: reverse SSH failed",
            ));
            checks.push(Check::skip(
                "Runner identity view",
                "not checked: reverse SSH failed",
            ));
        }
    }

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

    checks.push(match &rep.claude.auth {
        Ok(a) if a.logged_in => Check::ok("Claude authentication", a.describe()),
        Ok(_) => Check::fail_with("Claude authentication", ErrorCode::Auth, auth_hint(r)),
        Err(e) => Check::fail_report("Claude authentication", e),
    });

    checks
}

/// Design doc section 10: report, point at the manual login, never log in.
fn auth_hint(r: &Resolved<'_>) -> String {
    match &r.work.claude_config_dir {
        Some(dir) => format!(
            "Claude is not authenticated in configured CLAUDE_CONFIG_DIR\nrun on work: CLAUDE_CONFIG_DIR={} claude auth login",
            dir.display()
        ),
        None => "Claude is not authenticated on the work machine\nrun on work: claude auth login"
            .to_string(),
    }
}

fn identity_row(
    name: &'static str,
    side: &str,
    seen: &Reported<Option<String>>,
    home_id: Option<WorkspaceId>,
    how: &str,
) -> Check {
    let Some(home_id) = home_id else {
        return Check::skip(name, "not compared: home identity missing");
    };
    match seen {
        Ok(Some(id)) if *id == home_id.to_string() => Check::ok(name, "matches"),
        Ok(Some(id)) => Check::fail_with(
            name,
            ErrorCode::WrongWorkspace,
            format!("{side} sees {id}, home has {home_id}: {how} is a different project"),
        ),
        Ok(None) => Check::fail_with(
            name,
            ErrorCode::WrongWorkspace,
            format!("{side} cannot see {} {how}", identity::FILE_NAME),
        ),
        Err(e) => Check::fail_report(name, e),
    }
}

fn runner_rows(r: &Resolved<'_>, h: &HealthReport, home_id: Option<WorkspaceId>) -> Vec<Check> {
    let mut problems: Vec<(ErrorCode, String)> = Vec::new();
    if h.ccnm_version != crate::VERSION {
        problems.push((
            ErrorCode::Version,
            format!(
                "runner runs ccnm {}, home runs {}",
                h.ccnm_version,
                crate::VERSION
            ),
        ));
    }
    if !h.root.is_ok() {
        problems.push((
            ErrorCode::WrongWorkspace,
            format!(
                "runner sees {} as {}",
                r.workspace.root.display(),
                h.root.describe()
            ),
        ));
    }
    if !h.runtime_root.is_ok() {
        problems.push((
            ErrorCode::Config,
            format!(
                "runtime_root {} is {} for the runner; create it and give {} write access",
                r.workspace.runtime_root.display(),
                h.runtime_root.describe(),
                h.user
            ),
        ));
    }
    let row = match problems.first() {
        None => Check::ok(
            "Home runner",
            format!(
                "{} runs ccnm {}, root and runtime_root visible",
                h.user, h.ccnm_version
            ),
        ),
        Some((code, _)) => {
            let text: Vec<&str> = problems.iter().map(|(_, m)| m.as_str()).collect();
            Check::fail_with("Home runner", *code, text.join("\n"))
        }
    };
    vec![
        row,
        identity_row(
            "Runner identity view",
            "runner",
            &h.identity,
            home_id,
            "on the runner's own filesystem",
        ),
    ]
}

/// Rows that depend on the probe, when the probe never happened.
fn skipped_after_work_ssh() -> Vec<Check> {
    const REASON: &str = "not checked: work SSH failed";
    [
        "Work ccnm",
        "Work SMB mount",
        "Work identity view",
        "Reverse SSH",
        "Home runner",
        "Runner identity view",
        "Claude Code",
        "Claude authentication",
    ]
    .into_iter()
    .map(|name| Check::skip(name, REASON))
    .collect()
}

/// Still to come, with the phase that will make each one real.
fn not_yet_implemented() -> Vec<Check> {
    [("Consistency test", 2), ("Execution barrier", 5)]
        .into_iter()
        .map(|(name, phase)| Check::skip(name, format!("not implemented until phase {phase}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::AuthStatus;
    use crate::process::{FakeRunner, Output};
    use crate::runner::PathStatus;
    use crate::smb::MountStatus;
    use crate::work::ClaudeReport;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    /// A per-test directory with a config whose root is `<dir>/root`
    /// (created only if `with_root`).
    fn setup(test: &str, with_root: bool) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ccnm-doctor-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("root");
        if with_root {
            std::fs::create_dir_all(&root).unwrap();
        }
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home_runner]\nssh_from_work = \"ccnm-home\"\nsmb_user = \"fodelf\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nruntime_root = \"{}\"\nshare = \"xshun\"\n",
                root.display(),
                dir.join("runtime").display()
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

    fn env<'a>(fake: &'a FakeRunner, dir: &Path, tailscale: bool) -> Env<'a> {
        Env {
            runner: fake,
            control_dir: control(dir),
            tailscale: tailscale.then(|| PathBuf::from("/opt/homebrew/bin/tailscale")),
        }
    }

    fn sharing(root: &Path) -> String {
        format!(
            "name:\t\tcc-xshun\npath:\t\t{}\n\tsmb:\t{{\n    \t\tname:\txshun\n    \t\tshared:\t1\n\t}}\n",
            root.display()
        )
    }

    const TS: &str = r#"{"BackendState":"Running","Peer":{"k":{"HostName":"workmac","DNSName":"workmac.t.ts.net.","TailscaleIPs":["100.1.1.1"],"Online":true,"CurAddr":"203.0.113.7:41641","Relay":"tok","Active":true}}}"#;

    fn good_probe(id: &str) -> ProbeReport {
        ProbeReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            root: PathStatus {
                exists: true,
                is_dir: true,
            },
            identity: Ok(Some(id.to_string())),
            mount: Ok(MountStatus {
                mounted: true,
                detail: "mounted, SERVER_NAME=home".into(),
            }),
            home_ssh: Ok(crate::ssh::ResolvedSsh {
                hostname: "home.t.ts.net".into(),
                user: "ccrun".into(),
                port: 22,
                identity_files: vec![],
                proxy_jump: None,
            }),
            health: Ok(HealthReport {
                protocol: PROTOCOL,
                ccnm_version: crate::VERSION.to_string(),
                user: "ccrun".into(),
                root: PathStatus {
                    exists: true,
                    is_dir: true,
                },
                runtime_root: PathStatus {
                    exists: true,
                    is_dir: true,
                },
                identity: Ok(Some(id.to_string())),
            }),
            claude: ClaudeReport {
                path: Some(PathBuf::from("/usr/local/bin/claude")),
                version: Ok("2.1.259".into()),
                auth: Ok(AuthStatus {
                    logged_in: true,
                    auth_method: Some("claude.ai".into()),
                    email: Some("me@x".into()),
                    subscription_type: Some("max".into()),
                }),
            },
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
            &env(&fake, Path::new("/tmp"), false),
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
            &env(&fake, Path::new("/tmp"), false),
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

    #[test]
    fn unknown_workspace_fails_config() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-valid.toml"),
            Some("other"),
            &env(&fake, Path::new("/tmp"), false),
        );
        assert_eq!(report.exit_code(), 10);
        assert!(
            row(&report, "Workspace config")
                .detail
                .contains("defined: xshun")
        );
    }

    #[test]
    fn everything_good_blocks_only_on_future_phases() {
        let (dir, config) = setup("good", true);
        let root = dir.join("root");
        let id = identity::init(&root).unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, sharing(&root)));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(0, TS));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe(&id.to_string())).unwrap(),
        ));

        let report = run(&config, Some("xshun"), &env(&fake, &dir, true));
        let text = report.render();
        for name in [
            "Home workspace",
            "Workspace identity",
            "SMB share",
            "Tailscale",
            "Work SSH",
            "Work ccnm",
            "Work SMB mount",
            "Work identity view",
            "Reverse SSH",
            "Home runner",
            "Runner identity view",
            "Claude Code",
            "Claude authentication",
        ] {
            assert_eq!(row(&report, name).status, Status::Ok, "{name}:\n{text}");
        }
        assert_eq!(
            row(&report, "Tailscale").detail,
            "direct via 203.0.113.7:41641"
        );
        assert_eq!(row(&report, "Work SSH").detail, "me@workmac");
        assert_eq!(
            row(&report, "Claude authentication").detail,
            "me@x via claude.ai (max)"
        );
        assert!(
            text.ends_with("NOT READY (0 failed, 2 not checked)\n"),
            "{text}"
        );
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);

        // Read-only: no control dir, no new files in root.
        assert!(!control(&dir).exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        // Every ssh went out with ControlMaster=no.
        for call in fake.calls() {
            let text = call.display();
            if text.starts_with("ssh -o") {
                assert!(text.contains("ControlMaster=no"), "{text}");
            }
        }
        // The probe carried the workspace paths.
        let probe = &fake.calls()[3];
        let wire = probe.args.last().unwrap().to_string_lossy().into_owned();
        let sent: ProbeRequest = crate::payload::decode(&wire).unwrap();
        assert_eq!(sent.root, root);
        assert_eq!(sent.home_alias, "ccnm-home");
    }

    #[test]
    fn unreachable_work_fails_once_and_skips_the_rest() {
        let (dir, config) = setup("unreachable", true);
        identity::init(&dir.join("root")).unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, ""));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let mut down = Output::exited(255, "");
        down.stderr = b"ssh: connect to host workmac port 22: Operation timed out\n".to_vec();
        fake.push(down);

        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert_eq!(row(&report, "SMB share").status, Status::Warn);
        assert_eq!(row(&report, "Tailscale").status, Status::Ok);
        assert!(
            row(&report, "Work SSH")
                .detail
                .contains("Operation timed out")
        );
        assert_eq!(row(&report, "Claude Code").status, Status::Skip);
        let text = report.render();
        assert!(
            text.ends_with("NOT READY (1 failed, 10 not checked)\n"),
            "{text}"
        );
    }

    #[test]
    fn identity_mismatch_and_missing_mount_are_named() {
        let (dir, config) = setup("mismatch", true);
        let root = dir.join("root");
        identity::init(&root).unwrap();
        let mut probe = good_probe("00000000-0000-0000-0000-000000000000");
        probe.mount = Ok(MountStatus {
            mounted: false,
            detail: "not an SMB mount".into(),
        });
        probe.health.as_mut().unwrap().identity = Ok(None);
        probe.claude.auth = Ok(AuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            subscription_type: None,
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, sharing(&root)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        let mount = row(&report, "Work SMB mount");
        assert_eq!(mount.status, Status::Fail(ErrorCode::Mount));
        assert!(
            mount.detail.contains("run: ccnm mount xshun"),
            "{}",
            mount.detail
        );
        let work_id = row(&report, "Work identity view");
        assert_eq!(work_id.status, Status::Fail(ErrorCode::WrongWorkspace));
        assert!(
            work_id.detail.contains("work sees 00000000"),
            "{}",
            work_id.detail
        );
        let runner_id = row(&report, "Runner identity view");
        assert!(
            runner_id.detail.contains("runner cannot see"),
            "{}",
            runner_id.detail
        );
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Fail(ErrorCode::Auth));
        assert!(
            auth.detail.contains("run on work: claude auth login"),
            "{}",
            auth.detail
        );
        // First FAIL in table order decides: the mount row comes before identity.
        assert_eq!(report.exit_code(), 22);
    }

    #[test]
    fn missing_identity_asks_for_workspace_init_and_skips_comparisons() {
        let (dir, config) = setup("no-id", true);
        let root = dir.join("root");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, sharing(&root)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe("whatever")).unwrap(),
        ));
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        let id = row(&report, "Workspace identity");
        assert_eq!(id.status, Status::Fail(ErrorCode::WrongWorkspace));
        assert!(
            id.detail.contains("run: ccnm workspace init xshun"),
            "{}",
            id.detail
        );
        assert_eq!(row(&report, "Work identity view").status, Status::Skip);
        assert_eq!(report.exit_code(), 30);
    }

    #[test]
    fn share_exporting_another_path_is_a_mount_failure() {
        let (dir, config) = setup("share-path", true);
        let root = dir.join("root");
        identity::init(&root).unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, sharing(Path::new("/somewhere/else"))));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe(
                &identity::read(&root).unwrap().unwrap().to_string(),
            ))
            .unwrap(),
        ));
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        let share = row(&report, "SMB share");
        assert_eq!(share.status, Status::Fail(ErrorCode::Mount));
        assert!(share.detail.contains("/somewhere/else"), "{}", share.detail);
    }

    #[test]
    fn missing_root_fails_before_anything_remote() {
        let (dir, config) = setup("no-root", false);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, ""));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let mut down = Output::exited(255, "");
        down.stderr = b"nope\n".to_vec();
        fake.push(down);
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        assert_eq!(report.blocking_code(), Some(ErrorCode::Config));
        assert!(
            row(&report, "Home workspace")
                .detail
                .contains("does not exist on this machine")
        );
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
}

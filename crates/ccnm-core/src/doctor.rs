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
//! This build checks what the home machine can see on its own: config,
//! the project root, the ccnm binary the work machine will invoke back
//! here, and how the work alias resolves. Everything that needs the work
//! machine to answer (its ccnm, Claude, the reverse ssh, the MCP
//! handshake) is a SKIP row until the phase that implements it lands, and
//! a SKIP still blocks READY.
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
use crate::error::{Error, ErrorCode};
use crate::paths;
use crate::process::{Cmd, ProcessRunner};
use crate::ssh::Ssh;
use crate::tailscale::{self, Route};

/// What doctor needs from its surroundings. Injected so tests can script
/// every external command and decide whether `tailscale` exists.
pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine. Only read: doctor
    /// reuses a master if one exists and never creates the directory.
    pub control_dir: PathBuf,
    pub tailscale: Option<PathBuf>,
    /// This user's home, for expanding the `~/` in a remote ccnm path.
    pub home: PathBuf,
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

    let mut checks = vec![home_workspace(&ws.root), home_ccnm(r, env)];

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
            checks.extend(not_yet_implemented());
            return checks;
        }
    };
    match ssh.resolve(env.runner) {
        Ok(resolved) => {
            checks.push(tailscale_row(env, &resolved.hostname));
            checks.push(Check::ok(
                "Work SSH",
                format!("{} (resolved, not connected yet)", resolved.target()),
            ));
        }
        Err(e) => {
            checks.push(Check::skip("Tailscale", "not checked: ssh -G failed"));
            checks.push(Check::fail_with(
                "Work SSH",
                ErrorCode::WorkUnreachable,
                e.message(),
            ));
        }
    }

    checks.extend(not_yet_implemented());
    checks
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
        Ok(Some(peer)) if matches!(peer.route, Route::Relay(_)) => Check::warn(
            NAME,
            format!("{}; every MCP round trip pays for it", peer.describe()),
        ),
        Ok(Some(peer)) => Check::ok(NAME, peer.describe()),
    }
}

/// Still to come, with the phase that will make each one real (design doc
/// section 26).
fn not_yet_implemented() -> Vec<Check> {
    [
        ("Work ccnm", "1B"),
        ("Claude Code", "1B"),
        ("Claude authentication", "1B"),
        ("Reverse SSH", "1B"),
        ("Remote MCP handshake", "1B"),
        ("Workspace root", "1B"),
        ("Workspace policy", "2"),
        ("Project instructions", "3"),
        ("Native tools disabled", "3"),
        ("Runtime identity", "5"),
        ("Network isolation", "5"),
        ("Terminal session", "6"),
    ]
    .into_iter()
    .map(|(name, phase)| Check::skip(name, format!("not implemented until phase {phase}")))
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

    fn env<'a>(fake: &'a FakeRunner, dir: &Path, tailscale: bool) -> Env<'a> {
        Env {
            runner: fake,
            control_dir: control(dir),
            tailscale: tailscale.then(|| PathBuf::from("/opt/homebrew/bin/tailscale")),
            home: dir.join("home"),
        }
    }

    const TS: &str = r#"{"BackendState":"Running","Peer":{"k":{"HostName":"workmac","DNSName":"workmac.t.ts.net.","TailscaleIPs":["100.1.1.1"],"Online":true,"CurAddr":"203.0.113.7:41641","Relay":"tok","Active":true}}}"#;

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
    fn hybrid_backend_is_refused_by_this_build() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-hybrid.toml"),
            Some("legacy"),
            &env(&fake, Path::new("/tmp"), false),
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
    fn everything_local_good_blocks_only_on_future_phases() {
        let (dir, config) = setup("good", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.1.0\n"));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(0, TS));

        let report = run(&config, Some("xshun"), &env(&fake, &dir, true));
        let text = report.render();
        for name in [
            "Config",
            "Workspace config",
            "Home workspace",
            "Home ccnm",
            "Tailscale",
            "Work SSH",
        ] {
            assert_eq!(row(&report, name).status, Status::Ok, "{name}:\n{text}");
        }
        assert_eq!(
            row(&report, "Home ccnm").detail,
            format!("0.1.0 at {}", dir.join("home/.local/bin/ccnm").display())
        );
        assert_eq!(
            row(&report, "Tailscale").detail,
            "direct via 203.0.113.7:41641"
        );
        assert_eq!(
            row(&report, "Work SSH").detail,
            "me@workmac (resolved, not connected yet)"
        );
        assert!(
            text.ends_with("NOT READY (0 failed, 12 not checked)\n"),
            "{text}"
        );
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);

        // Read-only: no control dir, nothing new in root.
        assert!(!control(&dir).exists());
        assert_eq!(std::fs::read_dir(dir.join("root")).unwrap().count(), 0);
        // The version probe ran the expanded ~ path, and ssh only -G.
        let calls = fake.calls();
        assert_eq!(
            calls[0].display(),
            format!("{} --version", dir.join("home/.local/bin/ccnm").display())
        );
        assert_eq!(calls[1].display(), "ssh -G work");
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn missing_home_ccnm_names_the_path_and_the_fix() {
        let (dir, config) = setup("no-bin", true, false);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        let bin = row(&report, "Home ccnm");
        assert_eq!(bin.status, Status::Fail(ErrorCode::Version));
        assert!(bin.detail.contains("~/.local/bin/ccnm"), "{}", bin.detail);
        assert!(bin.detail.contains("cp $(which ccnm)"), "{}", bin.detail);
        assert!(bin.detail.contains("hosts.home.ccnm_bin"), "{}", bin.detail);
        assert_eq!(report.exit_code(), 11);
        assert_eq!(fake.calls().len(), 1, "no --version for a missing file");
    }

    #[test]
    fn stale_home_ccnm_is_a_version_failure() {
        let (dir, config) = setup("stale-bin", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.0.9\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
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
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
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
        let report = run(&config, Some("xshun"), &env(&fake, &dir, false));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert_eq!(row(&report, "Tailscale").status, Status::Skip);
        assert!(
            row(&report, "Work SSH")
                .detail
                .contains("Name or service not known")
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

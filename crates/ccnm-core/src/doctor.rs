//! `ccnm doctor [WORKSPACE]`: is this machine and workspace ready to use?
//!
//! Every check is one row: name, status, a line of detail. The report is
//! READY only when no row is FAIL or SKIP. The exit code is the error code of
//! the first FAIL row (or, failing that, the first SKIP), so `ccnm run` can
//! refuse with the same reason a human would read off the screen.
//!
//! Phase 0 performs the local checks only. Everything that needs SSH, SMB or
//! Claude is listed as SKIP with the phase that implements it. That keeps the
//! output in its final shape from day one and makes it impossible for a
//! half-built doctor to print READY.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::{Config, Resolved, Workspace};
use crate::error::{Error, ErrorCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Info,
    Warn,
    /// Not performed by this build. Carries the code a failure would have,
    /// because an unverified precondition is as blocking as a failed one.
    Skip(ErrorCode),
    Fail(ErrorCode),
}

impl Status {
    fn label(&self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Info => "INFO",
            Status::Warn => "WARN",
            Status::Skip(_) => "SKIP",
            Status::Fail(_) => "FAIL",
        }
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

    fn info(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Info,
            detail: detail.into(),
        }
    }

    fn skip(name: &'static str, code: ErrorCode, phase: u8) -> Self {
        Check {
            name,
            status: Status::Skip(code),
            detail: format!("not implemented until phase {phase}"),
        }
    }

    fn fail(name: &'static str, err: &Error) -> Self {
        Check {
            name,
            status: Status::Fail(err.code()),
            detail: format!("{}: {}", err.code().name(), err.message()),
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

    /// The code the process should exit with: first FAIL, else first SKIP,
    /// else none.
    pub fn blocking_code(&self) -> Option<ErrorCode> {
        let first_fail = self.checks.iter().find_map(|c| match c.status {
            Status::Fail(code) => Some(code),
            _ => None,
        });
        first_fail.or_else(|| {
            self.checks.iter().find_map(|c| match c.status {
                Status::Skip(code) => Some(code),
                _ => None,
            })
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
        let skipped = self.count(|s| matches!(s, Status::Skip(_)));
        out.push('\n');
        if self.ready() {
            out.push_str("READY\n");
        } else {
            let _ = writeln!(
                out,
                "NOT READY ({failed} failed, {skipped} not implemented)"
            );
        }
        out
    }

    fn count(&self, pred: impl Fn(&Status) -> bool) -> usize {
        self.checks.iter().filter(|c| pred(&c.status)).count()
    }
}

/// Run every check this build can perform.
pub fn run(config_path: &Path, workspace: Option<&str>) -> Report {
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
            checks.push(Check::info("Workspaces", detail));
        }
        Some(name) => match config.workspace(name) {
            Ok(resolved) => {
                checks.push(Check::ok(
                    "Workspace config",
                    format!(
                        "work_host={} (ssh {})",
                        resolved.workspace.work_host, resolved.host.ssh
                    ),
                ));
                checks.extend(workspace_checks(&resolved));
            }
            Err(err) => checks.push(Check::fail("Workspace config", &err)),
        },
    }

    Report { subject, checks }
}

fn workspace_checks(resolved: &Resolved<'_>) -> Vec<Check> {
    let mut checks = vec![home_workspace(resolved.workspace)];
    checks.extend(not_yet_implemented());
    checks
}

/// The project root must exist on this (home) machine. Doctor runs here, so
/// this needs no transport and is the one workspace check Phase 0 can do.
fn home_workspace(workspace: &Workspace) -> Check {
    let root = &workspace.root;
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

/// The rest of the design doc section 4 table, in its order, with the phase
/// that will make each one real.
fn not_yet_implemented() -> Vec<Check> {
    [
        ("Workspace identity", ErrorCode::WrongWorkspace, 1),
        ("Tailscale", ErrorCode::WorkUnreachable, 1),
        ("Work SSH", ErrorCode::WorkUnreachable, 1),
        ("Work ccnm", ErrorCode::Version, 1),
        ("Home runner", ErrorCode::HomeUnreachable, 1),
        ("SMB share", ErrorCode::Mount, 1),
        ("Work SMB mount", ErrorCode::Mount, 1),
        ("Reverse SSH", ErrorCode::HomeUnreachable, 1),
        ("Claude Code", ErrorCode::Version, 1),
        ("Claude authentication", ErrorCode::Auth, 1),
        ("Consistency test", ErrorCode::Coherence, 2),
        ("Execution barrier", ErrorCode::Coherence, 5),
    ]
    .into_iter()
    .map(|(name, code, phase)| Check::skip(name, code, phase))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    /// A config whose root exists on this machine (or not, per `root`).
    fn temp_config(test: &str, root: &Path) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-doctor-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            format!(
                "version = 1\n[hosts.work]\nssh = \"work\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"{}\"\nruntime_root = \"/Users/Shared/cc-runtime/xshun\"\nshare = \"xshun\"\n",
                root.display()
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn missing_config_is_the_only_row_and_exits_config() {
        let report = run(Path::new("/nonexistent/config.toml"), Some("xshun"));
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "Config");
        assert_eq!(report.checks[0].status, Status::Fail(ErrorCode::Config));
        assert!(!report.ready());
        assert_eq!(report.exit_code(), 10);
        let text = report.render();
        assert!(text.contains("CCNM_E_CONFIG"), "{text}");
        assert!(
            text.ends_with("NOT READY (1 failed, 0 not implemented)\n"),
            "{text}"
        );
    }

    #[test]
    fn config_only_run_is_ready_and_lists_workspaces() {
        let report = run(&fixture("config-valid.toml"), None);
        assert!(report.ready(), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
        assert_eq!(report.subject, "config");
        let text = report.render();
        assert!(
            text.contains("Workspaces              INFO   xshun"),
            "{text}"
        );
        assert!(text.ends_with("\nREADY\n"), "{text}");
    }

    #[test]
    fn unknown_workspace_fails_config() {
        let report = run(&fixture("config-valid.toml"), Some("other"));
        assert_eq!(report.exit_code(), 10);
        let row = &report.checks[1];
        assert_eq!(row.name, "Workspace config");
        assert!(row.detail.contains("defined: xshun"), "{}", row.detail);
    }

    #[test]
    fn existing_root_passes_and_unimplemented_checks_block() {
        let root = std::env::temp_dir();
        let report = run(&temp_config("root-ok", &root), Some("xshun"));
        let text = report.render();
        assert!(text.starts_with("ccnm doctor: xshun\n\n"), "{text}");
        assert!(text.contains("Home workspace          OK     "), "{text}");
        assert!(!report.ready());
        // No FAIL rows, so the first SKIP decides the exit code.
        assert_eq!(report.blocking_code(), Some(ErrorCode::WrongWorkspace));
        assert!(
            text.contains("NOT READY (0 failed, 12 not implemented)"),
            "{text}"
        );
        assert!(
            text.contains("Execution barrier       SKIP   not implemented until phase 5"),
            "{text}"
        );
    }

    #[test]
    fn missing_root_fails_before_skips_decide() {
        let root = std::env::temp_dir().join("ccnm-doctor-definitely-missing");
        let report = run(&temp_config("root-missing", &root), Some("xshun"));
        assert_eq!(report.blocking_code(), Some(ErrorCode::Config));
        let text = report.render();
        assert!(text.contains("does not exist on this machine"), "{text}");
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

//! Installing the work controller as a macOS LaunchAgent.
//!
//! This is how [`crate::controller`] gets into the login session in the
//! first place. launchd starts a job in `gui/<uid>` inside the user's Aqua
//! session, which is the whole point: a process started any other way — an
//! ssh command, a `&` in a shell — inherits a session that cannot read the
//! login Keychain, and Claude started from there thinks it is logged out.
//!
//! # Installing over ssh works
//!
//! Verified on the real work machine 2026-09-03: an ssh session can
//! `launchctl bootstrap gui/<uid>`, and the job it starts reports
//! `managername = Aqua`. The ssh session does not have to *be* in the
//! login session to put something there. So the home machine can set the
//! work machine up in one line:
//!
//! ```text
//! ssh work ccnm work-controller install
//! ```
//!
//! # Why ccnm installs this one but does not create `ccrun`
//!
//! Both would be "a tool changing the machine", but they are not the same
//! kind of change. This is ccnm's own component, in the user's own
//! `~/Library/LaunchAgents`, removable by the uninstall command in the
//! same breath. Creating a Unix account and handing it an ACL is a
//! permanent change to the machine's security model, which is why
//! `docs/production-safety.md` hands those commands to the user instead.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::controller::{self, Context, LABEL};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, ProcessRunner};

/// How long to wait for launchd to get the agent listening before calling
/// the install a failure.
const START_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);

/// `~/Library/LaunchAgents/dev.ccnm.work-controller.plist`.
///
/// A user agent, not `/Library/LaunchAgents` (which needs root and would
/// load for every account on the machine) and not
/// `/Library/LaunchDaemons` (which has no login session at all — the exact
/// context this exists to escape).
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The agent definition.
///
/// - `KeepAlive`: the controller holding a Claude session must come back
///   if it dies. launchd throttles restarts to once per 10s, so a
///   permanently failing start does not spin.
/// - `ProcessType: Interactive`: opts out of the CPU and I/O throttling
///   launchd applies to background work. This sits in front of a person
///   waiting on a Claude tool call.
/// - No `PATH`: launchd's own environment is what Claude will be started
///   with later, so `claude::locate` resolving against it here is the
///   honest answer rather than a shell's.
pub fn plist(exe: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>internal</string>
        <string>work-controller</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>CCNM_LOG</key>
        <string>info</string>
    </dict>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = LABEL,
        exe = xml(&exe.to_string_lossy()),
        log = xml(&log.to_string_lossy()),
    )
}

/// The five characters XML cannot carry raw. Paths come from
/// `current_exe()` and `$HOME`, so this is unlikely to fire — and a plist
/// that silently loses half a path when it does is worse than the check.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Everything the install will do, resolved but not yet done. Printed by
/// `--dry-run`, and used by [`install`] so there is only one description
/// of the steps.
#[derive(Debug, Clone)]
pub struct Plan {
    pub plist_path: PathBuf,
    pub plist: String,
    /// `gui/<uid>`, the per-user launchd domain of the GUI session.
    pub domain: String,
    pub socket: PathBuf,
    pub log: PathBuf,
    pub exe: PathBuf,
}

impl Plan {
    /// Resolve the paths and ask the OS for the uid.
    pub fn new(home: &Path, state: &Path, exe: &Path, runner: &dyn ProcessRunner) -> Result<Plan> {
        let log = state.join("controller.log");
        Ok(Plan {
            plist_path: plist_path(home),
            plist: plist(exe, &log),
            domain: format!("gui/{}", uid(runner)?),
            socket: crate::paths::controller_socket(state),
            log,
            exe: exe.to_path_buf(),
        })
    }

    /// `<domain>/<label>`, what `launchctl` calls a service target.
    pub fn target(&self) -> String {
        format!("{}/{LABEL}", self.domain)
    }

    /// Stop whatever is loaded under this label. Fails when nothing is
    /// loaded, which is why every caller ignores the result: "already
    /// absent" is the outcome it wanted.
    pub fn bootout_cmd(&self) -> Cmd {
        Cmd::new("/bin/launchctl")
            .args(["bootout", &self.target()])
            .timeout(Duration::from_secs(20))
    }

    pub fn bootstrap_cmd(&self) -> Cmd {
        Cmd::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(&self.domain)
            .arg(&self.plist_path)
            .timeout(Duration::from_secs(20))
    }

    /// What a person would type to do this by hand.
    pub fn describe(&self) -> String {
        format!(
            "write   {}\nrun     {}\nrun     {}\nexpect  a controller listening on {}",
            self.plist_path.display(),
            self.bootout_cmd().display(),
            self.bootstrap_cmd().display(),
            self.socket.display()
        )
    }
}

/// The uid launchd files the GUI session under. Read from `id -u` because
/// ccnm forbids `unsafe`, so `libc::getuid` is not available.
fn uid(runner: &dyn ProcessRunner) -> Result<u32> {
    let out = runner.run(
        &Cmd::new("/usr/bin/id")
            .arg("-u")
            .timeout(Duration::from_secs(5)),
    )?;
    let text = out.stdout_lossy().trim().to_string();
    text.parse().map_err(|_| {
        Error::internal(format!(
            "`id -u` printed {text:?} (exit {:?}), which is not a uid",
            out.exit_code
        ))
    })
}

/// Write the plist, (re)start the agent, and wait until it answers.
///
/// Idempotent: an agent that is already loaded is booted out first, so
/// running this after upgrading the binary is the way to restart it.
///
/// Returns what the running controller says about itself. A controller
/// that starts but is not in a login session is *not* an error here — the
/// caller reports it, because the fix is the user's (log in on the work
/// machine's screen), not a retry.
pub fn install(plan: &Plan, runner: &dyn ProcessRunner) -> Result<Context> {
    if let Some(dir) = plan.plist_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = plan.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&plan.plist_path, &plan.plist).map_err(|e| {
        Error::internal(format!("cannot write {}", plan.plist_path.display())).with_source(e)
    })?;

    // Ignored on purpose: this fails when nothing was loaded.
    let _ = runner.run(&plan.bootout_cmd());
    let out = runner.run(&plan.bootstrap_cmd())?;
    if !out.success() {
        return Err(Error::new(
            ErrorCode::Internal,
            format!(
                "{} failed (exit {:?}): {}",
                plan.bootstrap_cmd().display(),
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        ));
    }
    wait_until_listening(&plan.socket, &plan.log)
}

/// launchd returns as soon as it has accepted the job, so the socket is
/// not there yet. Poll rather than guess a sleep.
fn wait_until_listening(socket: &Path, log: &Path) -> Result<Context> {
    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        let last = match controller::context(socket) {
            Ok(ctx) => return Ok(ctx),
            Err(e) => e,
        };
        if std::time::Instant::now() >= deadline {
            return Err(Error::new(
                ErrorCode::NotReady,
                format!(
                    "the agent was accepted by launchd but nothing is listening on {} after {:?}\n{}\nwhat it wrote: {}",
                    socket.display(),
                    START_TIMEOUT,
                    last.message(),
                    log.display()
                ),
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Stop the agent and remove its plist. Leaves the log: it is the only
/// record of why the thing was misbehaving.
pub fn uninstall(plan: &Plan, runner: &dyn ProcessRunner) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let out = runner.run(&plan.bootout_cmd())?;
    done.push(if out.success() {
        format!("stopped {}", plan.target())
    } else {
        format!("{} was not loaded", plan.target())
    });
    match std::fs::remove_file(&plan.plist_path) {
        Ok(()) => done.push(format!("removed {}", plan.plist_path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            done.push(format!("no plist at {}", plan.plist_path.display()));
        }
        Err(e) => {
            return Err(
                Error::internal(format!("cannot remove {}", plan.plist_path.display()))
                    .with_source(e),
            );
        }
    }
    // A SIGTERMed controller runs no destructor, so this is usually left
    // over. Removing it here keeps `status` from reporting a corpse.
    if std::fs::remove_file(&plan.socket).is_ok() {
        done.push(format!("removed {}", plan.socket.display()));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    fn plan(runner: &dyn ProcessRunner) -> Plan {
        Plan::new(
            Path::new("/Users/bing"),
            Path::new("/Users/bing/.local/state/ccnm"),
            Path::new("/Users/bing/.local/bin/ccnm"),
            runner,
        )
        .unwrap()
    }

    fn with_uid() -> FakeRunner {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "501\n"));
        fake
    }

    #[test]
    fn the_plist_runs_this_binary_in_the_gui_domain() {
        let fake = with_uid();
        let plan = plan(&fake);
        assert_eq!(plan.domain, "gui/501");
        assert_eq!(plan.target(), "gui/501/dev.ccnm.work-controller");
        assert_eq!(
            plan.plist_path,
            PathBuf::from("/Users/bing/Library/LaunchAgents/dev.ccnm.work-controller.plist")
        );
        let plist = &plan.plist;
        assert!(plist.contains("<string>/Users/bing/.local/bin/ccnm</string>"));
        assert!(plist.contains("<string>work-controller</string>"));
        assert!(
            plist.contains("<key>KeepAlive</key>\n    <true/>"),
            "{plist}"
        );
        assert!(
            plist.contains("/Users/bing/.local/state/ccnm/controller.log"),
            "{plist}"
        );
        // launchd, not a login shell, decides the PATH; see claude::locate.
        assert!(!plist.contains("<key>PATH</key>"), "{plist}");
    }

    #[test]
    fn a_path_with_xml_in_it_does_not_break_the_plist() {
        let fake = with_uid();
        let plan = Plan::new(
            Path::new("/Users/a&b"),
            Path::new("/tmp/s"),
            Path::new("/Users/a&b/<ccnm>"),
            &fake,
        )
        .unwrap();
        assert!(
            plan.plist.contains("/Users/a&amp;b/&lt;ccnm&gt;"),
            "{}",
            plan.plist
        );
        assert!(!plan.plist.contains("<ccnm>"));
    }

    #[test]
    fn a_uid_that_is_not_a_number_is_an_error_not_a_domain() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "id: no such user\n"));
        let err = Plan::new(Path::new("/h"), Path::new("/s"), Path::new("/x"), &fake).unwrap_err();
        assert!(err.message().contains("not a uid"), "{err}");
    }

    #[test]
    fn install_replaces_a_loaded_agent_rather_than_failing_on_it() {
        let dir = std::env::temp_dir().join(format!("ccnm-la-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = with_uid();
        let mut plan = plan(&fake);
        plan.plist_path = dir.join("agent.plist");
        plan.log = dir.join("controller.log");
        // Bootstrap is reported as succeeding; nothing ever listens, so
        // install fails at the last step -- which is the assertion: the
        // agent is not called installed until it answers.
        fake.push(Output::exited(1, "")); // bootout: nothing loaded
        fake.push(Output::exited(0, "")); // bootstrap
        plan.socket = dir.join("absent.sock");

        let err = install(&plan, &fake).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("nothing is listening"), "{err}");
        assert!(err.message().contains("controller.log"), "{err}");

        assert!(
            plan.plist_path.exists(),
            "the plist should have been written"
        );
        let calls = fake.calls();
        assert_eq!(
            calls[1].display(),
            "/bin/launchctl bootout gui/501/dev.ccnm.work-controller"
        );
        assert!(
            calls[2]
                .display()
                .starts_with("/bin/launchctl bootstrap gui/501 "),
            "{}",
            calls[2].display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_bootstrap_says_what_launchctl_said() {
        let dir = std::env::temp_dir().join(format!("ccnm-la-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = with_uid();
        let mut plan = plan(&fake);
        plan.plist_path = dir.join("agent.plist");
        plan.log = dir.join("controller.log");
        fake.push(Output::exited(0, ""));
        let mut denied = Output::exited(112, "");
        denied.stderr =
            b"Bootstrap failed: 125: Domain does not support specified action\n".to_vec();
        fake.push(denied);

        let err = install(&plan, &fake).unwrap_err();
        assert!(err.message().contains("Domain does not support"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uninstall_is_fine_with_a_machine_that_has_none_of_it() {
        let fake = with_uid();
        let mut plan = plan(&fake);
        plan.plist_path = std::env::temp_dir().join("ccnm-not-there.plist");
        plan.socket = std::env::temp_dir().join("ccnm-not-there.sock");
        fake.push(Output::exited(1, ""));
        let done = uninstall(&plan, &fake).unwrap();
        assert!(done[0].contains("was not loaded"), "{done:?}");
        assert!(done[1].contains("no plist at"), "{done:?}");
        assert_eq!(done.len(), 2, "nothing was removed: {done:?}");
    }

    #[test]
    fn uninstall_clears_the_socket_a_killed_controller_left() {
        let dir = std::env::temp_dir().join(format!("ccnm-la-un-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = with_uid();
        let mut plan = plan(&fake);
        plan.plist_path = dir.join("agent.plist");
        plan.socket = dir.join("controller.sock");
        std::fs::write(&plan.plist_path, "x").unwrap();
        std::fs::write(&plan.socket, "").unwrap();
        fake.push(Output::exited(0, ""));

        let done = uninstall(&plan, &fake).unwrap();
        assert!(done[0].starts_with("stopped gui/501/"), "{done:?}");
        assert!(!plan.plist_path.exists());
        assert!(
            !plan.socket.exists(),
            "a stale socket must not survive uninstall"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_names_every_step_and_the_socket() {
        let fake = with_uid();
        let text = plan(&fake).describe();
        assert!(text.contains("Library/LaunchAgents/dev.ccnm.work-controller.plist"));
        assert!(text.contains("launchctl bootout gui/501/dev.ccnm.work-controller"));
        assert!(text.contains("launchctl bootstrap gui/501"));
        assert!(text.contains("controller.sock"), "{text}");
    }
}

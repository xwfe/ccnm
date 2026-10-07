//! Installing the controller as a systemd user service (Linux).
//!
//! The Linux counterpart of [`crate::launchagent`]. There is no login
//! Keychain to reach on Linux (see [`crate::controller`]), so what this buys
//! is the other half of the controller's job: a process that outlives the
//! ssh connection which asked for a session, and comes back if it dies.
//!
//! # Linger
//!
//! A user service lives as long as the account's systemd user manager,
//! and by default that manager stops when the account's last login ends --
//! taking the controller and every session it started with it. `loginctl
//! enable-linger <user>` keeps it running. ccnm does not turn that on: it
//! is a change to how the machine treats the account, made with privilege,
//! so [`install`] and `doctor` say what happens without it and what to
//! type, and leave it to the person.
//!
//! # What it writes
//!
//! One unit file in the account's own `$XDG_CONFIG_HOME/systemd/user/`,
//! removed by `ccnm controller uninstall`. Like the plist, it carries the
//! [`crate::paths::location_overrides`] of the install and no `PATH`: the
//! controller looks for `claude` and `codex` in the service's environment
//! and then in the usual install locations (`~/.local/bin`, ...), which is
//! the answer a session will actually get.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::controller::{self, Context};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, ProcessRunner};

/// The unit's name, and the basename of its file.
pub const UNIT: &str = "dev.ccnm.controller.service";

/// `$XDG_CONFIG_HOME/systemd/user/dev.ccnm.controller.service`, where
/// `systemctl --user` looks for units the account installed itself.
pub fn unit_path(config_home: &Path) -> PathBuf {
    config_home.join("systemd/user").join(UNIT)
}

/// The unit definition.
///
/// - `Restart=always`, `RestartSec=10`: as launchd's `KeepAlive` with its
///   10 s throttle -- the controller holding sessions must come back, and a
///   start that keeps failing must not spin.
/// - `KillMode=process`: stopping or restarting the controller (which is
///   what `controller install` does to an upgraded binary) ends the
///   controller only. The tmux server and session supervisors it started
///   are in the same cgroup; the default would kill them too, ending
///   somebody's conversation as a side effect of an upgrade.
/// - `StandardError=append:`: the same `controller.log` the macOS agent
///   writes, so every message that names the log is right on both.
/// - `env`: the install's location overrides. With none, the unit is the
///   same for every install of the same binary.
pub fn unit(exe: &Path, log: &Path, env: &[(&str, PathBuf)]) -> Result<String> {
    let env: String = env
        .iter()
        .map(|(name, value)| {
            Ok(format!(
                "Environment=\"{}\"\n",
                quoted(&format!("{name}={}", path_text(value)?))
            ))
        })
        .collect::<Result<_>>()?;
    Ok(format!(
        "# Written by `ccnm controller install`; `ccnm controller uninstall` removes it.\n\
         [Unit]\n\
         Description=ccnm controller: starts and keeps the Agent sessions of this account\n\
         \n\
         [Service]\n\
         ExecStart=\"{exe}\" internal controller\n\
         Environment=\"CCNM_LOG=info\"\n\
         {env}\
         Restart=always\n\
         RestartSec=10\n\
         KillMode=process\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = quoted(&path_text(exe)?),
        log = bare(&path_text(log)?)?,
    ))
}

/// A path as unit-file text. A newline cannot be written into a unit at
/// all, and refusing is better than a unit that means something else.
fn path_text(path: &Path) -> Result<String> {
    let text = path.to_string_lossy().into_owned();
    if text.contains('\n') {
        return Err(Error::config(format!(
            "{} cannot go into a systemd unit: it contains a newline",
            path.display()
        )));
    }
    Ok(text)
}

/// Inside double quotes: `\` and `"` are escaped, and `%` is doubled
/// because systemd expands `%h`, `%u` and the like everywhere.
fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
}

/// `StandardError=append:` takes the rest of the line as the path, with no
/// quoting, so whitespace would be read as part of something else.
fn bare(text: &str) -> Result<String> {
    if text.chars().any(char::is_whitespace) {
        return Err(Error::config(format!(
            "the controller log path {text:?} contains whitespace, which `StandardError=append:` cannot carry\nset XDG_STATE_HOME to a path without spaces"
        )));
    }
    Ok(text.replace('%', "%%"))
}

/// Everything the install will do, resolved but not yet done. Printed by
/// `--dry-run`, and used by [`install`] so there is only one description
/// of the steps.
#[derive(Debug, Clone)]
pub struct Plan {
    pub unit_path: PathBuf,
    pub unit: String,
    pub socket: PathBuf,
    pub log: PathBuf,
    pub exe: PathBuf,
    /// What the unit sets beyond `CCNM_LOG`; see [`unit`].
    pub env: Vec<(&'static str, PathBuf)>,
}

impl Plan {
    pub fn new(
        config_home: &Path,
        state: &Path,
        exe: &Path,
        env: Vec<(&'static str, PathBuf)>,
    ) -> Result<Plan> {
        let log = state.join("controller.log");
        Ok(Plan {
            unit_path: unit_path(config_home),
            unit: unit(exe, &log, &env)?,
            socket: crate::paths::controller_socket(state),
            log,
            exe: exe.to_path_buf(),
            env,
        })
    }

    fn systemctl(args: &[&str]) -> Cmd {
        Cmd::new("systemctl")
            .arg("--user")
            .args(args.iter().copied())
            .timeout(Duration::from_secs(30))
    }

    pub fn daemon_reload_cmd(&self) -> Cmd {
        Plan::systemctl(&["daemon-reload"])
    }

    /// Started again at the next boot or login.
    pub fn enable_cmd(&self) -> Cmd {
        Plan::systemctl(&["enable", UNIT])
    }

    /// Starts it, or replaces a running one: what an upgrade needs.
    pub fn restart_cmd(&self) -> Cmd {
        Plan::systemctl(&["restart", UNIT])
    }

    pub fn disable_cmd(&self) -> Cmd {
        Plan::systemctl(&["disable", "--now", UNIT])
    }

    /// What a person would type to do this by hand.
    pub fn describe(&self) -> String {
        let env: String = self
            .env
            .iter()
            .map(|(name, value)| format!("  with  {name}={}\n", value.display()))
            .collect();
        format!(
            "write   {}\n{env}run     {}\nrun     {}\nrun     {}\nexpect  a controller listening on {}",
            self.unit_path.display(),
            self.daemon_reload_cmd().display(),
            self.enable_cmd().display(),
            self.restart_cmd().display(),
            self.socket.display()
        )
    }
}

/// Write the unit, (re)start the service, and wait until it answers.
///
/// Idempotent: `restart` starts a stopped service and replaces a running
/// one, so running this after upgrading the binary is the way to restart
/// it. Sessions survive that (see `KillMode` in [`unit`]).
pub fn install(plan: &Plan, runner: &dyn ProcessRunner) -> Result<Context> {
    if let Some(dir) = plan.unit_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = plan.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&plan.unit_path, &plan.unit).map_err(|e| {
        Error::internal(format!("cannot write {}", plan.unit_path.display())).with_source(e)
    })?;
    for cmd in [
        plan.daemon_reload_cmd(),
        plan.enable_cmd(),
        plan.restart_cmd(),
    ] {
        let out = runner.run(&cmd)?;
        if !out.success() {
            return Err(systemctl_failed(&cmd, &out));
        }
    }
    controller::wait_until_listening(&plan.socket, &plan.log, "systemd")
}

/// The failure everyone hits once: no user manager to talk to. `su` and
/// `sudo -u` do not start one; an ssh login does, and so does linger.
fn systemctl_failed(cmd: &Cmd, out: &crate::process::Output) -> Error {
    let stderr = out.stderr_lossy().trim().to_string();
    let hint = if stderr.contains("Failed to connect to bus")
        || stderr.contains("$DBUS_SESSION_BUS_ADDRESS")
        || stderr.contains("XDG_RUNTIME_DIR")
    {
        "\nthis account has no systemd user manager running: log in to it over ssh (not `su` or `sudo -u`), or keep one running with: sudo loginctl enable-linger $(id -un)"
    } else {
        ""
    };
    Error::new(
        ErrorCode::Internal,
        format!(
            "{} failed (exit {:?}): {stderr}{hint}",
            cmd.display(),
            out.exit_code
        ),
    )
}

/// What `install` and `status` print when linger is off: the controller
/// works, but only while somebody is logged in to this account.
pub fn linger_warning(user: &str) -> String {
    format!(
        "linger is off for {user}: when {user}'s last login ends, systemd stops this controller and every session it started\nto keep them running: sudo loginctl enable-linger {user}"
    )
}

/// Stop and disable the service, remove its unit. Leaves the log: it is
/// the only record of why the thing was misbehaving.
pub fn uninstall(plan: &Plan, runner: &dyn ProcessRunner) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let out = runner.run(&plan.disable_cmd())?;
    done.push(if out.success() {
        format!("stopped and disabled {UNIT}")
    } else {
        format!("{UNIT} was not running or not enabled")
    });
    match std::fs::remove_file(&plan.unit_path) {
        Ok(()) => done.push(format!("removed {}", plan.unit_path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            done.push(format!("no unit at {}", plan.unit_path.display()));
        }
        Err(e) => {
            return Err(
                Error::internal(format!("cannot remove {}", plan.unit_path.display()))
                    .with_source(e),
            );
        }
    }
    let _ = runner.run(&plan.daemon_reload_cmd());
    if std::fs::remove_file(&plan.socket).is_ok() {
        done.push(format!("removed {}", plan.socket.display()));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    fn plan(env: Vec<(&'static str, PathBuf)>) -> Plan {
        Plan::new(
            Path::new("/home/me/.config"),
            Path::new("/home/me/.local/state/ccnm"),
            Path::new("/home/me/.local/bin/ccnm"),
            env,
        )
        .unwrap()
    }

    #[test]
    fn the_unit_starts_this_binary_and_leaves_sessions_alone_on_restart() {
        let plan = plan(Vec::new());
        assert_eq!(
            plan.unit_path,
            PathBuf::from("/home/me/.config/systemd/user/dev.ccnm.controller.service")
        );
        let unit = &plan.unit;
        assert!(
            unit.contains("ExecStart=\"/home/me/.local/bin/ccnm\" internal controller\n"),
            "{unit}"
        );
        assert!(unit.contains("\nKillMode=process\n"), "{unit}");
        assert!(unit.contains("\nRestart=always\nRestartSec=10\n"), "{unit}");
        assert!(
            unit.contains("\nStandardError=append:/home/me/.local/state/ccnm/controller.log\n"),
            "{unit}"
        );
        assert!(unit.contains("\nWantedBy=default.target\n"), "{unit}");
        assert!(unit.contains("Environment=\"CCNM_LOG=info\"\n"), "{unit}");
        // Like the plist: no PATH, so the controller finds the CLIs the way
        // a session will.
        assert!(!unit.contains("PATH"), "{unit}");
        assert!(unit.ends_with('\n'));
    }

    #[test]
    fn location_overrides_are_written_and_quoted_for_systemd() {
        let plan = plan(vec![
            ("CCNM_CONFIG", PathBuf::from("/home/me/p74/config.toml")),
            ("XDG_STATE_HOME", PathBuf::from("/home/me/a \"b\"\\c 50%")),
        ]);
        assert!(
            plan.unit
                .contains("Environment=\"CCNM_CONFIG=/home/me/p74/config.toml\"\n"),
            "{}",
            plan.unit
        );
        assert!(
            plan.unit
                .contains("Environment=\"XDG_STATE_HOME=/home/me/a \\\"b\\\"\\\\c 50%%\"\n"),
            "{}",
            plan.unit
        );
        let text = plan.describe();
        assert!(
            text.contains("  with  CCNM_CONFIG=/home/me/p74/config.toml\n"),
            "{text}"
        );
    }

    #[test]
    fn paths_a_unit_cannot_carry_are_refused() {
        assert!(
            Plan::new(
                Path::new("/home/me/.config"),
                Path::new("/home/me/my state"),
                Path::new("/home/me/.local/bin/ccnm"),
                Vec::new(),
            )
            .is_err()
        );
        assert!(unit(Path::new("/bin/cc\nnm"), Path::new("/log"), &[]).is_err());
    }

    #[test]
    fn the_plan_names_every_command_it_runs() {
        let plan = plan(Vec::new());
        let text = plan.describe();
        assert!(
            text.starts_with("write   /home/me/.config/systemd/user/dev.ccnm.controller.service\n"),
            "{text}"
        );
        for line in [
            "run     systemctl --user daemon-reload\n",
            "run     systemctl --user enable dev.ccnm.controller.service\n",
            "run     systemctl --user restart dev.ccnm.controller.service\n",
            "expect  a controller listening on ",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
    }

    #[test]
    fn no_user_manager_says_how_to_get_one() {
        let dir = std::env::temp_dir().join(format!("ccnm-systemd-{}-nobus", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = Plan::new(
            &dir.join("config"),
            &dir.join("state"),
            Path::new("/home/me/.local/bin/ccnm"),
            Vec::new(),
        )
        .unwrap();
        let fake = FakeRunner::new();
        let mut down = Output::exited(1, "");
        down.stderr = b"Failed to connect to bus: No medium found\n".to_vec();
        fake.push(down);
        let err = install(&plan, &fake).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            err.message()
                .contains("systemctl --user daemon-reload failed"),
            "{err}"
        );
        assert!(err.message().contains("loginctl enable-linger"), "{err}");
        assert!(err.message().contains("not `su`"), "{err}");
        assert_eq!(fake.calls().len(), 1, "stops at the first failure");
    }

    #[test]
    fn uninstall_disables_removes_and_reloads() {
        let dir =
            std::env::temp_dir().join(format!("ccnm-systemd-{}-uninstall", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = Plan::new(
            &dir.join("config"),
            &dir.join("state"),
            Path::new("/home/me/.local/bin/ccnm"),
            Vec::new(),
        )
        .unwrap();
        std::fs::create_dir_all(plan.unit_path.parent().unwrap()).unwrap();
        std::fs::write(&plan.unit_path, &plan.unit).unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, ""));
        fake.push(Output::exited(0, ""));
        let done = uninstall(&plan, &fake).unwrap();
        let unit_left = plan.unit_path.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!unit_left);
        assert_eq!(done[0], "stopped and disabled dev.ccnm.controller.service");
        let calls: Vec<String> = fake.calls().iter().map(Cmd::display).collect();
        assert_eq!(
            calls,
            [
                "systemctl --user disable --now dev.ccnm.controller.service",
                "systemctl --user daemon-reload"
            ]
        );
    }

    #[test]
    fn the_linger_warning_names_the_account_and_the_fix() {
        let text = linger_warning("bing");
        assert!(text.contains("linger is off for bing"), "{text}");
        assert!(text.contains("sudo loginctl enable-linger bing"), "{text}");
    }
}

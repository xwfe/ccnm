//! What the runtime account can reach, and whether that is acceptable.
//!
//! `exec_command` is a remote shell. Design doc sections 18 and 19 say
//! what has to be true before a real project is put behind one, and none
//! of it is something ccnm can implement in Rust: a dedicated Unix user,
//! filesystem ACLs, no sudo, no egress. Those are the operating system's
//! job.
//!
//! So this module does the only two useful things left:
//!
//! ```text
//! verify   look at the account this runtime is actually running as and
//!          say, precisely, which of those properties hold
//! gate     refuse to run commands when they do not, unless the workspace
//!          has explicitly said it accepts an unconfined runtime
//! ```
//!
//! Every check is read-only and local. None of them makes the machine
//! safer; they make its state legible, and they stop a real project being
//! wired up to an account that can read the user's SSH key.
//!
//! # What this is not
//!
//! Passing the audit does not make `exec_command` safe to point at
//! untrusted input. It means the blast radius is the `ccrun` account
//! rather than the developer's own. That is the difference the design doc
//! asks for, and it is worth having; it is not a sandbox, and section 18
//! is explicit that no command parser can be one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::{Cmd, ProcessRunner};

/// Long enough for `id` and `sudo -n`, short enough that a wedged audit
/// cannot make a Claude session look hung.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How bad one finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Ok,
    /// Worth knowing, but not a reason to refuse: the property is either
    /// conditional (egress only matters if that is your boundary) or
    /// could not be established here.
    Warn,
    /// A real project must not be run behind this.
    Fail,
}

/// One property of the runtime account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub check: String,
    pub severity: Severity,
    pub detail: String,
    /// What the user would do about it, in their own shell. ccnm never
    /// runs any of these: creating users and changing permissions is not
    /// something a diagnostic tool should do behind someone's back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Finding {
    fn ok(check: &str, detail: impl Into<String>) -> Finding {
        Finding {
            check: check.to_string(),
            severity: Severity::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(check: &str, detail: impl Into<String>) -> Finding {
        Finding {
            check: check.to_string(),
            severity: Severity::Warn,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(check: &str, detail: impl Into<String>, fix: impl Into<String>) -> Finding {
        Finding {
            check: check.to_string(),
            severity: Severity::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// The runtime account, as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audit {
    /// Who this process is running as.
    pub user: String,
    pub findings: Vec<Finding>,
}

impl Audit {
    /// Is there anything that should stop a real project being run here?
    pub fn confined(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Fail)
    }

    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Fail)
    }

    /// One line per problem, with the fix, for a refusal a person can act
    /// on without reading the source.
    pub fn refusal(&self) -> String {
        let mut text = format!(
            "the runtime is running as {} and is not confined, so exec_command is refused:",
            self.user
        );
        for finding in self.failures() {
            text.push_str(&format!("\n  - {}: {}", finding.check, finding.detail));
            if let Some(fix) = &finding.fix {
                text.push_str(&format!("\n    fix: {fix}"));
            }
        }
        text.push_str(
            "\nSee docs/production-safety.md. To accept an unconfined runtime for one workspace anyway, set allow_unconfined_exec = true on it in config.toml.",
        );
        text
    }
}

/// Look at the account this process is running as.
///
/// `expected_user` is the account the config says the runtime should be,
/// and `home` is this process's home directory.
pub fn audit(expected_user: Option<&str>, home: &Path, runner: &dyn ProcessRunner) -> Audit {
    let identity = Identity::read(runner);
    let mut findings = Vec::new();

    findings.push(match (&identity.uid, expected_user) {
        (Some(0), _) => Finding::fail(
            "Runs as root",
            "the runtime is root, so every tool call has unrestricted access to the machine",
            "run the MCP runtime as a dedicated unprivileged user; see docs/production-safety.md",
        ),
        // Not knowing must never read as confined. ccnm can verify
        // nothing about an account it cannot identify, and an audit that
        // says "probably fine" is worse than one that says nothing.
        (None, _) => Finding::fail(
            "Runs as root",
            "cannot determine which account the runtime is running as",
            "check that `id` works on the workspace machine",
        ),
        (Some(_), Some(want)) if identity.user != want => Finding::fail(
            "Runtime user",
            format!(
                "the runtime is running as {} but config.toml expects {want}",
                identity.user
            ),
            format!("start the runtime as {want}, or correct runtime_user in config.toml"),
        ),
        (Some(_), Some(want)) => Finding::ok("Runtime user", want.to_string()),
        (Some(_), None) => Finding::fail(
            "Runtime user",
            format!(
                "no runtime_user is configured, so ccnm cannot tell whether {} is the dedicated account or the developer's own",
                identity.user
            ),
            "set runtime_user on the home host in config.toml to the dedicated account",
        ),
    });

    findings.push(sudo_finding(runner));
    findings.push(group_finding(&identity));
    findings.push(ssh_key_finding(home));
    findings.push(claude_credential_finding(home));
    findings.push(docker_finding(&identity));

    Audit {
        user: identity.user,
        findings,
    }
}

/// Can this account become root without a password? `sudo -n` never
/// prompts, so this cannot hang, and `true` is the most harmless thing
/// there is to run if it turns out the answer is yes.
fn sudo_finding(runner: &dyn ProcessRunner) -> Finding {
    const NAME: &str = "No sudo";
    let cmd = Cmd::new("sudo").args(["-n", "true"]).timeout(PROBE_TIMEOUT);
    match runner.run(&cmd) {
        Err(_) => Finding::ok(NAME, "sudo is not installed"),
        Ok(out) if out.success() => Finding::fail(
            NAME,
            "this account has passwordless sudo, so any command it runs can become root",
            "remove it from the sudoers file and from the admin group",
        ),
        Ok(_) => Finding::ok(NAME, "cannot become root without a password"),
    }
}

/// On macOS, membership of `admin` is sudo with a password prompt, which
/// an interactive user would answer. `wheel` is the same idea elsewhere.
fn group_finding(identity: &Identity) -> Finding {
    const NAME: &str = "Not an admin";
    let privileged: Vec<&String> = identity
        .groups
        .iter()
        .filter(|g| *g == "admin" || *g == "wheel" || *g == "sudo" || *g == "staff")
        .collect();
    // `staff` is every account on a Mac, so it is noted rather than failed.
    let escalating: Vec<&&String> = privileged.iter().filter(|g| ***g != *"staff").collect();
    if identity.groups.is_empty() {
        // Same reasoning: an unlistable account cannot be ruled out of
        // the admin group, so it is not confined.
        return Finding::fail(
            NAME,
            "cannot list this account's groups, so admin membership cannot be ruled out",
            "check that `id -Gn` works on the workspace machine",
        );
    }
    if escalating.is_empty() {
        Finding::ok(NAME, "not in admin, wheel or sudo")
    } else {
        Finding::fail(
            NAME,
            format!(
                "this account is in {}, which is a route to root",
                escalating
                    .iter()
                    .map(|g| g.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "remove the runtime account from those groups",
        )
    }
}

/// A private key the runtime can read is a key the runtime can use, and
/// `exec_command` is a shell.
fn ssh_key_finding(home: &Path) -> Finding {
    const NAME: &str = "No SSH keys";
    let dir = home.join(".ssh");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Finding::ok(NAME, "no readable ~/.ssh");
    };
    let mut keys: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".pub") || name == "known_hosts" || name == "config" {
            continue;
        }
        // A private key is readable and starts with a PEM header. Reading
        // the first line rather than guessing from the name catches a key
        // called anything at all.
        if looks_like_private_key(&entry.path()) {
            keys.push(name);
        }
    }
    if keys.is_empty() {
        Finding::ok(NAME, "no readable private key in ~/.ssh")
    } else {
        keys.sort();
        Finding::fail(
            NAME,
            format!(
                "this account can read {} in ~/.ssh; a command it runs can use them",
                keys.join(", ")
            ),
            "the runtime account must have its own home with no SSH keys in it",
        )
    }
}

fn looks_like_private_key(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let head = &bytes[..bytes.len().min(64)];
    let text = String::from_utf8_lossy(head);
    text.contains("PRIVATE KEY")
}

/// The core invariant of section 6: the home machine holds no Claude
/// credential. A credential here would make this machine an Anthropic
/// egress point, which is the whole thing the architecture exists to
/// avoid.
fn claude_credential_finding(home: &Path) -> Finding {
    const NAME: &str = "No Claude credential";
    let mut found = Vec::new();
    let mut dirs = vec![home.join(".claude")];
    if let Some(custom) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        dirs.push(PathBuf::from(custom));
    }
    for dir in dirs {
        for name in [".credentials.json", "credentials.json"] {
            let path = dir.join(name);
            if path.exists() {
                found.push(path.display().to_string());
            }
        }
    }
    if found.is_empty() {
        Finding::ok(NAME, "no Claude credentials file on this machine")
    } else {
        Finding::fail(
            NAME,
            format!(
                "this machine holds a Claude credential ({}); the workspace machine must never be an Anthropic egress point",
                found.join(", ")
            ),
            "remove it, and never run `claude auth login` on the workspace machine",
        )
    }
}

/// Write access to the Docker socket is root, one `docker run -v /:/host`
/// away. Worth checking because it is the escalation path people forget
/// they left open.
fn docker_finding(identity: &Identity) -> Finding {
    const NAME: &str = "No Docker socket";
    let socket = Path::new("/var/run/docker.sock");
    let Ok(meta) = std::fs::metadata(socket) else {
        return Finding::ok(NAME, "no Docker socket on this machine");
    };
    use std::os::unix::fs::MetadataExt;
    let mode = meta.mode();
    let world_writable = mode & 0o002 != 0;
    let group_writable = mode & 0o020 != 0;
    let ours = identity.gids.contains(&meta.gid()) || identity.uid == Some(meta.uid());
    if world_writable || (group_writable && ours) {
        Finding::fail(
            NAME,
            "this account can write to /var/run/docker.sock, which is equivalent to root",
            "remove the runtime account from the docker group, or do not run Docker on this machine",
        )
    } else {
        Finding::ok(NAME, "the Docker socket is not writable by this account")
    }
}

/// Who this process is, from `id`. One subprocess, no unsafe, no libc.
#[derive(Debug, Default)]
struct Identity {
    user: String,
    uid: Option<u32>,
    gids: Vec<u32>,
    groups: Vec<String>,
}

impl Identity {
    fn read(runner: &dyn ProcessRunner) -> Identity {
        let field = |args: [&str; 1]| -> Option<String> {
            let out = runner
                .run(&Cmd::new("id").args(args).timeout(PROBE_TIMEOUT))
                .ok()?;
            out.success().then(|| out.stdout_lossy().trim().to_string())
        };
        let user = field(["-un"]).unwrap_or_else(|| "unknown".to_string());
        let uid = field(["-u"]).and_then(|s| s.parse().ok());
        let gids = field(["-G"])
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|g| g.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let groups = field(["-Gn"])
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        Identity {
            user,
            uid,
            gids,
            groups,
        }
    }
}

/// Can this machine reach `api.anthropic.com`?
///
/// Separate from [`audit`] and never called by the MCP runtime: it makes
/// an outbound connection, which is fine for a diagnostic the user asked
/// for and wrong to do on every session start.
///
/// Reaching it is not automatically a failure. Section 19 makes the egress
/// rule conditional — *if* this is your compliance boundary, block it at
/// the OS or the network, and do not mistake a static command deny list
/// for a network boundary. So this reports, and leaves the judgement to
/// the person reading.
pub fn egress_finding(timeout: Duration) -> Finding {
    const NAME: &str = "Anthropic egress";
    const TARGET: &str = "api.anthropic.com:443";
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = TARGET.to_socket_addrs() else {
        return Finding::ok(NAME, "api.anthropic.com does not resolve from here");
    };
    let Some(addr) = addrs.next() else {
        return Finding::ok(NAME, "api.anthropic.com does not resolve from here");
    };
    match std::net::TcpStream::connect_timeout(&addr, timeout) {
        Ok(_) => Finding::warn(
            NAME,
            "this machine can reach api.anthropic.com; if that is your compliance boundary, block it at the OS or network level rather than trusting a command deny list",
        ),
        Err(_) => Finding::ok(NAME, "api.anthropic.com is not reachable from here"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    /// `id` answers four times per audit, in this order.
    fn identity(runner: &FakeRunner, user: &str, uid: &str, gids: &str, groups: &str) {
        runner.push(Output::exited(0, format!("{user}\n")));
        runner.push(Output::exited(0, format!("{uid}\n")));
        runner.push(Output::exited(0, format!("{gids}\n")));
        runner.push(Output::exited(0, format!("{groups}\n")));
    }

    fn empty_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-safety-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        dir
    }

    fn find<'a>(audit: &'a Audit, check: &str) -> &'a Finding {
        audit
            .findings
            .iter()
            .find(|f| f.check == check)
            .unwrap_or_else(|| panic!("no finding named {check}; got {:?}", audit.findings))
    }

    #[test]
    fn a_dedicated_account_with_nothing_in_reach_is_confined() {
        let home = empty_home("clean");
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "502", "20", "staff");
        runner.push(Output::exited(1, "")); // sudo -n true refused
        let audit = audit(Some("ccrun"), &home, &runner);
        assert!(audit.confined(), "{:?}", audit.findings);
        assert_eq!(audit.user, "ccrun");
        assert_eq!(find(&audit, "Runtime user").severity, Severity::Ok);
        assert_eq!(find(&audit, "No sudo").severity, Severity::Ok);
        assert_eq!(find(&audit, "Not an admin").severity, Severity::Ok);
    }

    #[test]
    fn root_is_refused_whatever_else_is_true() {
        let home = empty_home("root");
        let runner = FakeRunner::new();
        identity(&runner, "root", "0", "0", "wheel");
        runner.push(Output::exited(0, ""));
        let audit = audit(Some("root"), &home, &runner);
        assert!(!audit.confined());
        let finding = find(&audit, "Runs as root");
        assert_eq!(finding.severity, Severity::Fail);
        assert!(finding.fix.is_some());
    }

    #[test]
    fn no_configured_runtime_user_is_itself_a_failure() {
        // Without it, ccnm has nothing to compare against and cannot tell
        // the dedicated account from the developer's own. Saying "looks
        // fine" there would be the worst answer available.
        let home = empty_home("unset");
        let runner = FakeRunner::new();
        identity(&runner, "fodelf", "501", "20", "staff");
        runner.push(Output::exited(1, ""));
        let audit = audit(None, &home, &runner);
        assert!(!audit.confined());
        let finding = find(&audit, "Runtime user");
        assert_eq!(finding.severity, Severity::Fail);
        assert!(
            finding.detail.contains("no runtime_user is configured"),
            "{finding:?}"
        );
    }

    #[test]
    fn the_wrong_account_is_a_failure_even_if_it_is_unprivileged() {
        let home = empty_home("wrong");
        let runner = FakeRunner::new();
        identity(&runner, "fodelf", "501", "20", "staff");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        assert!(!audit.confined());
        let finding = find(&audit, "Runtime user");
        assert!(finding.detail.contains("fodelf"), "{finding:?}");
        assert!(finding.detail.contains("ccrun"), "{finding:?}");
    }

    #[test]
    fn passwordless_sudo_and_admin_membership_are_both_refused() {
        let home = empty_home("sudo");
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "502", "20 80", "staff admin");
        runner.push(Output::exited(0, "")); // sudo -n true worked
        let audit = audit(Some("ccrun"), &home, &runner);
        assert!(!audit.confined());
        assert_eq!(find(&audit, "No sudo").severity, Severity::Fail);
        let group = find(&audit, "Not an admin");
        assert_eq!(group.severity, Severity::Fail);
        assert!(group.detail.contains("admin"), "{group:?}");
        // staff alone is every Mac account and must not fail on its own.
    }

    #[test]
    fn a_readable_private_key_is_refused_whatever_it_is_called() {
        let home = empty_home("keys");
        std::fs::write(
            home.join(".ssh/not_named_like_a_key"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nxxxx\n",
        )
        .unwrap();
        std::fs::write(home.join(".ssh/id_ed25519.pub"), "ssh-ed25519 AAAA\n").unwrap();
        std::fs::write(home.join(".ssh/known_hosts"), "host key\n").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "502", "20", "staff");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        assert!(!audit.confined());
        let finding = find(&audit, "No SSH keys");
        assert_eq!(finding.severity, Severity::Fail);
        assert!(
            finding.detail.contains("not_named_like_a_key"),
            "{finding:?}"
        );
        // A public key and known_hosts are not credentials.
        assert!(!finding.detail.contains(".pub"), "{finding:?}");
        assert!(!finding.detail.contains("known_hosts"), "{finding:?}");
    }

    #[test]
    fn a_claude_credential_on_this_machine_is_refused() {
        let home = empty_home("claude");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}\n").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "502", "20", "staff");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        assert!(!audit.confined());
        let finding = find(&audit, "No Claude credential");
        assert_eq!(finding.severity, Severity::Fail);
        assert!(
            finding.detail.contains("Anthropic egress point"),
            "{finding:?}"
        );
    }

    #[test]
    fn the_refusal_names_every_problem_and_its_fix() {
        let home = empty_home("refusal");
        let runner = FakeRunner::new();
        identity(&runner, "root", "0", "0", "wheel");
        runner.push(Output::exited(0, ""));
        let audit = audit(None, &home, &runner);
        let text = audit.refusal();
        for expected in [
            "Runs as root",
            "No sudo",
            "Not an admin",
            "fix:",
            "allow_unconfined_exec",
        ] {
            assert!(text.contains(expected), "{expected} missing from:\n{text}");
        }
        // It says what to read, not just what is wrong.
        assert!(text.contains("docs/production-safety.md"), "{text}");
    }

    #[test]
    fn a_runner_that_cannot_answer_warns_rather_than_passing() {
        let home = empty_home("nothing");
        let runner = FakeRunner::new(); // every command fails
        let audit = audit(Some("ccrun"), &home, &runner);
        assert_eq!(audit.user, "unknown");
        // Not knowing must never read as confined.
        assert!(!audit.confined(), "{:?}", audit.findings);
        assert_eq!(find(&audit, "Runs as root").severity, Severity::Fail);
        assert_eq!(find(&audit, "Not an admin").severity, Severity::Fail);
    }
}

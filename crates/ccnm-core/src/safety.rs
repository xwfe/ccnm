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
//! There are two such switches, and they are deliberately not one:
//! `allow_unconfined_exec` accepts an account with more OS access than it
//! should have, and `allow_unisolated_credentials` accepts one that
//! can read the agent's own login. The second is the thing this program
//! exists to prevent, so it is never implied -- it has to be written down
//! by itself, on the machine taking the risk, and it is said out loud once
//! and shown by `ccnm doctor` for as long as it is set.
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

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::process::{Cmd, ProcessRunner};

pub mod credentials;
pub mod environment;

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
    /// Findings no switch in any config waives, whatever it says.
    ///
    /// An unknown execution identity means nobody can say what was
    /// accepted or on whose behalf. Inherited authentication environment
    /// is worse than a credential sitting on disk: it is handed to every
    /// child process, so a command does not even have to go looking. Both
    /// also have ordinary fixes, which is the other half of why neither is
    /// something a config file gets to take on your behalf.
    pub fn non_waivable(&self) -> bool {
        self.check == "No authentication environment" || self.check == "Runtime identity known"
    }

    /// "This identity can reach a known Agent login."
    ///
    /// Waivable, but only by the switch that names it — see
    /// [`Accepted::unisolated_credentials`] and the field it comes from,
    /// `allow_unisolated_credentials`.
    pub fn is_agent_credential(&self) -> bool {
        crate::provider::AgentProvider::ALL
            .iter()
            .any(|p| self.check == format!("No {} credential", p.credentials().agent_name))
    }

    /// Has this workspace accepted *this* finding? Confinement findings
    /// are not decided here — they are the `confined() || unconfined_exec`
    /// half of [`Audit::exec_allowed`].
    fn waived_by(&self, accepted: Accepted) -> bool {
        if self.non_waivable() {
            return false;
        }
        if self.is_agent_credential() {
            return accepted.unisolated_credentials;
        }
        true
    }
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

/// What one workspace's own config has accepted, as the **Runtime** reads
/// it. Never what a caller asked for: the machine taking the risk decides.
///
/// Two switches rather than one, because they are two different
/// admissions. "This account is not confined" says the model's commands
/// have more OS access than they should. "This account can reach my agent
/// login" says a prompt is enough to read it. Neither implies the other,
/// and a workspace in the second situation needs both set before anything
/// runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accepted {
    /// `allow_unconfined_exec`
    pub unconfined_exec: bool,
    /// `allow_unisolated_credentials`
    pub unisolated_credentials: bool,
}

impl Accepted {
    /// The default posture: nothing waived.
    pub const NOTHING: Self = Self {
        unconfined_exec: false,
        unisolated_credentials: false,
    };

    /// Only the unconfined-exec switch, which is what most callers and
    /// every pre-existing test mean.
    pub fn unconfined(unconfined_exec: bool) -> Self {
        Self {
            unconfined_exec,
            unisolated_credentials: false,
        }
    }

    /// Is anything waived at all? Used to decide whether a session has to
    /// carry a warning with its results.
    pub fn any(&self) -> bool {
        self.unconfined_exec || self.unisolated_credentials
    }
}

/// Say once, out loud, what a workspace has accepted -- or return `None`
/// because there is nothing to say or it has already been said.
///
/// **Once, not every time.** A warning on every command is a warning
/// people stop reading, and `ccnm doctor` keeps the row for as long as the
/// switch is set, so nothing is hidden by staying quiet afterwards. The
/// marker sits beside the write guards in the state directory, so "once"
/// means once per workspace on each machine you drive it from.
///
/// Turning the switch back off removes the marker, so deciding this again
/// later is announced again. It is the decision that gets the warning, not
/// the session.
///
/// A state directory that cannot be written is not an error: the warning
/// is printed and simply may be printed again.
pub fn warn_accepted_once(state_dir: &Path, workspace: &str, accepted: Accepted) -> Option<String> {
    let marker = state_dir
        .join("accepted-risks")
        .join(format!("{workspace}.agent-credentials"));
    if !accepted.unisolated_credentials {
        let _ = std::fs::remove_file(&marker);
        return None;
    }
    if marker.is_file() {
        return None;
    }
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, b"said\n");
    Some(format!(
        "!! ccnm: workspace \"{workspace}\" has allow_unisolated_credentials set.\n\
         \n\
         The account that runs this workspace's commands on the Runtime Node can\n\
         read a known Agent login on that machine. So can every command the model\n\
         runs -- and a prompt is all it takes to make it run one, including a\n\
         prompt that arrives in a file it was asked to read.\n\
         \n\
         That separation is the one thing ccnm otherwise refuses to bend. This\n\
         workspace has accepted losing it. Nothing else is standing in the way.\n\
         \n\
         To take it back: remove allow_unisolated_credentials from\n\
         [workspaces.{workspace}] in the Runtime Node's config.toml.\n\
         \n\
         Said once. `ccnm doctor {workspace}` keeps showing it."
    ))
}

/// The runtime account, as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audit {
    /// Who this process is running as.
    pub user: String,
    pub findings: Vec<Finding>,
}

impl Audit {
    pub fn agent_boundary_clear(&self, accepted: Accepted) -> bool {
        !self
            .findings
            .iter()
            .any(|f| f.severity == Severity::Fail && !f.waived_by(accepted))
    }
    pub fn exec_allowed(&self, accepted: Accepted) -> bool {
        self.agent_boundary_clear(accepted) && (self.confined() || accepted.unconfined_exec)
    }
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
    pub fn refusal(&self, accepted: Accepted) -> String {
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
        if !self.agent_boundary_clear(accepted) {
            // Two different refusals, and saying the wrong one sends
            // somebody looking for a switch that does not exist.
            if self.failures().any(|f| f.non_waivable()) {
                text.push_str(
                    "\nRuntime initialization is also refused, and no workspace switch waives this: an unknown execution identity, or authentication inherited from the environment.",
                );
            } else {
                text.push_str(
                    "\nRuntime initialization is also refused: this identity can reach a known Agent login. To accept that for one workspace -- every command the model runs could then read it -- set allow_unisolated_credentials = true on it in config.toml.",
                );
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
/// `expected_user` is `runtime_user`: the identity the **Runtime Executor**
/// is expected to be — the account the Agent's SSH MCP transport lands on
/// and under which project tools actually run. It says nothing about which
/// account may type `ccnm` (see docs/production-safety.md, 四种身份).
///
/// **This audits the calling process, whoever that is.** Called from
/// `internal mcp-serve` the caller *is* the Runtime Executor, so the result
/// is a Runtime verdict. Called from the public CLI it describes the
/// Operator instead, and a caller that presents it as a Runtime verdict is
/// reporting the wrong account — P7.3 hit exactly that: the same workspace
/// audited green as `ccrun` and red as the operator's own login. Moving the
/// verdict to an authoritative Runtime probe is P7.4 Batch D
/// (docs/plan/runtime-surfaces.md); until then callers must label whose
/// account they are showing.
///
/// `home` is the home directory of the account being audited.
pub fn audit(expected_user: Option<&str>, home: &Path, runner: &dyn ProcessRunner) -> Audit {
    audit_with_environment(
        expected_user,
        home,
        runner,
        &credentials::local_references(),
        &std::env::vars_os().map(|(k, _)| k).collect::<Vec<_>>(),
    )
}

fn audit_with_environment(
    expected_user: Option<&str>,
    home: &Path,
    runner: &dyn ProcessRunner,
    references: &[(String, Option<std::ffi::OsString>)],
    names: &[std::ffi::OsString],
) -> Audit {
    let identity = Identity::read(runner);
    let mut findings = Vec::new();

    if identity.uid.is_none() {
        findings.push(Finding::fail(
            "Runtime identity known",
            "the execution identity is unknown",
            "restore the local identity probe before allowing Runtime execution",
        ));
    }

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
            "check that `id` works on the Runtime Node",
        ),
        (Some(_), Some(want)) if identity.user != want => Finding::fail(
            "Runtime user",
            format!(
                "this ccnm process runs as {}, and config.toml expects the Runtime Executor to be {want}",
                identity.user
            ),
            format!(
                "make the Agent's SSH MCP transport land on {want}, or correct runtime_user in config.toml; typing ccnm as {want} is not what this checks"
            ),
        ),
        (Some(_), Some(want)) => Finding::ok("Runtime user", want.to_string()),
        (Some(_), None) => Finding::fail(
            "Runtime user",
            format!(
                "no runtime_user is configured, so ccnm cannot tell whether {} is the dedicated account or the developer's own",
                identity.user
            ),
            "set runtime_user on the Runtime Node in config.toml to the dedicated account",
        ),
    });

    findings.push(sudo_finding(runner));
    findings.push(group_finding(&identity));
    findings.push(ssh_key_finding(home, runner));
    findings.extend(credentials::findings_with(home, references, runner));
    findings.push(if environment::validate_runtime_names(names.iter().cloned()).is_ok() {
        Finding::ok("No authentication environment", "no unapproved authentication-shaped environment names observed")
    } else {
        Finding::fail("No authentication environment", "authentication environment has no Runtime project authorization (names and values withheld)", "remove inherited authentication from the Runtime service environment; do not copy Agent credentials")
    });
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
    let cmd = Cmd::new("/usr/bin/sudo")
        .args(["-n", "true"])
        .timeout(PROBE_TIMEOUT);
    match runner.run(&cmd) {
        Err(_) => Finding::fail(
            NAME,
            "sudo availability or result is unknown",
            "verify the Runtime privilege policy; a failed diagnostic is not a denial",
        ),
        Ok(out) if out.success() => Finding::fail(
            NAME,
            "this account has passwordless sudo, so any command it runs can become root",
            "remove it from the sudoers file and from the admin group",
        ),
        Ok(out) if !out.timed_out && out.exit_code == Some(1) => {
            Finding::ok(NAME, "cannot become root without a password")
        }
        Ok(_) => Finding::fail(
            NAME,
            "sudo probe did not establish a denial",
            "verify the Runtime privilege policy",
        ),
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
            "check that `id -Gn` works on the Runtime Node",
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
///
/// The Runtime Executor is **inbound-only**: the Agent SSHes in, and no
/// part of ccnm's control chain requires it to SSH out. So the correct
/// state is no private key at all — `authorized_keys`, `known_hosts` and a
/// client `config` are inbound or non-secret state, not credentials.
///
/// Two directories are inspected: `~/.ssh`, and `~/.config/ccnm`, which is
/// ccnm's own. The second one is here because of P7.3: a transport key was
/// moved out of `~/.ssh` into ccnm's config directory and this row went
/// green while the account could SSH out exactly as before. A check that
/// can be satisfied by `mv` is not a check.
///
/// It still says what it inspected rather than claiming more. Somewhere
/// else entirely is not searched, and an inherited `SSH_AUTH_SOCK` is an
/// outbound credential with no file at all — that one is caught by the
/// authentication-environment row, which refuses it by name.
fn ssh_key_finding(home: &Path, runner: &dyn ProcessRunner) -> Finding {
    const NAME: &str = "No SSH keys";
    const INSPECTED: &str = "~/.ssh and ~/.config/ccnm";
    for dir in [home.join(".ssh"), home.join(".config/ccnm")] {
        if let Some(problem) = private_key_in(&dir, runner) {
            return problem;
        }
    }
    Finding::ok(
        NAME,
        format!(
            "no accessible private key candidate in {INSPECTED}; contents were not read, and no other location was searched"
        ),
    )
}

/// One directory's worth of the search above, recursing into what it holds.
///
/// `None` means nothing here looks like a usable private key.
fn private_key_in(dir: &Path, runner: &dyn ProcessRunner) -> Option<Finding> {
    const NAME: &str = "No SSH keys";
    // Depth is bounded because this runs before every session; ccnm's own
    // config directory is two levels deep at most, and a person who buries
    // a key deeper than this has defeated a heuristic, not a boundary.
    fn walk(dir: &Path, depth: u32, runner: &dyn ProcessRunner) -> Option<Finding> {
        if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
            return Some(Finding::fail(
                NAME,
                "an inspected credential directory is a symlink; accessibility is unknown",
                "use a separate Runtime home and verify its permissions",
            ));
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => {
                return Some(Finding::fail(
                    NAME,
                    "an inspected credential directory cannot be listed; accessibility is unknown",
                    "verify permissions for the Runtime execution identity",
                ));
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return Some(Finding::fail(
                    NAME,
                    "SSH credential inventory is unknown",
                    "verify permissions for the Runtime execution identity",
                ));
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                if depth == 0 {
                    continue;
                }
                if let Some(found) = walk(&entry.path(), depth - 1, runner) {
                    return Some(found);
                }
                continue;
            }
            // Public keys, host fingerprints, client config and ccnm's own
            // TOML are not credentials, and every Runtime needs some of them.
            if name.ends_with(".pub")
                || name.ends_with(".toml")
                || matches!(
                    name.as_str(),
                    "known_hosts"
                        | "known_hosts.old"
                        | "config"
                        | "authorized_keys"
                        | "authorized_keys2"
                )
            {
                continue;
            }
            if matches!(
                credentials::access(&entry.path(), runner),
                credentials::Access::Accessible | credentials::Access::Unknown
            ) {
                return Some(Finding::fail(
                    NAME,
                    "a possible private SSH key is accessible or unknown (names and contents withheld)",
                    "use a Runtime identity that holds no outbound SSH credential; it only needs inbound access",
                ));
            }
        }
        None
    }
    walk(dir, 3, runner)
}

/// Write access to the Docker socket is root, one `docker run -v /:/host`
/// away. Worth checking because it is the escalation path people forget
/// they left open.
fn docker_finding(identity: &Identity) -> Finding {
    const NAME: &str = "No Docker socket";
    let socket = Path::new("/var/run/docker.sock");
    let Ok(meta) = std::fs::metadata(socket) else {
        // Two different observations reach the same conclusion, and saying
        // the wrong one is how a reader stops trusting the row. P7.3 saw
        // this: the socket was a symlink into another account's home, so
        // stat failed and ccnm reported "no Docker socket on this machine"
        // about a machine that was running Docker.
        return if std::fs::symlink_metadata(socket).is_ok() {
            Finding::ok(
                NAME,
                "a Docker socket exists but this account cannot reach it, so it cannot write to it either",
            )
        } else {
            Finding::ok(NAME, "there is no Docker socket on this machine")
        };
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
                .run(&Cmd::new("/usr/bin/id").args(args).timeout(PROBE_TIMEOUT))
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
    egress_finding_for(crate::provider::AgentProvider::current(), timeout)
}

pub fn egress_finding_for(provider: crate::provider::AgentProvider, timeout: Duration) -> Finding {
    let metadata = provider.credentials();
    let name = format!("{} egress", metadata.vendor_name);
    let host = metadata.egress_host;
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = (host, 443).to_socket_addrs() else {
        return Finding::warn(
            &name,
            format!("{host} DNS result is unknown; this does not prove an egress policy"),
        );
    };
    let Some(addr) = addrs.next() else {
        return Finding::warn(
            &name,
            format!("{host} resolved no addresses; this does not prove an egress policy"),
        );
    };
    match std::net::TcpStream::connect_timeout(&addr, timeout) {
        Ok(_) => Finding::warn(
            &name,
            format!(
                "this machine can reach {host}; if that is your compliance boundary, block it at the OS or network level rather than trusting a command deny list"
            ),
        ),
        Err(_) => Finding::warn(
            &name,
            format!("{host} was not reachable in this probe; this does not prove an egress policy"),
        ),
    }
}

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};
    use std::path::PathBuf;

    fn audit(expected: Option<&str>, home: &Path, runner: &dyn ProcessRunner) -> Audit {
        audit_with_environment(expected, home, runner, &[], &[])
    }

    /// `id` answers four times per audit, in this order.
    fn identity(runner: &FakeRunner, user: &str, uid: &str, gids: &str, groups: &str) {
        runner.push(Output::exited(0, format!("{user}\n")));
        runner.push(Output::exited(0, format!("{uid}\n")));
        runner.push(Output::exited(0, format!("{gids}\n")));
        runner.push(Output::exited(0, format!("{groups}\n")));
    }

    fn empty_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-safety-{}-{name}", std::process::id()));
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
        // Public incoming keys are required for an ordinary SSH Runtime.
        std::fs::write(
            home.join(".ssh/authorized_keys"),
            "ssh-ed25519 SYNTHETIC_PUBLIC_KEY\n",
        )
        .unwrap();
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
            finding.detail.contains("possible private SSH key"),
            "{finding:?}"
        );
        // A public key and known_hosts are not credentials.
        assert!(!finding.detail.contains(".pub"), "{finding:?}");
        assert!(!finding.detail.contains("known_hosts"), "{finding:?}");
    }

    /// The Runtime Executor is inbound-only. Everything it legitimately
    /// needs for SSH is public or non-secret: the Agent's public key in
    /// `authorized_keys`, host fingerprints, a client config. None of that
    /// is a credential, and an account holding only those must audit clean
    /// — otherwise "no outbound key" and "the Agent can get in" would be
    /// impossible to satisfy at the same time.
    #[test]
    fn an_inbound_only_executor_holds_no_private_key_and_is_still_confined() {
        let home = empty_home("inbound-only");
        std::fs::write(
            home.join(".ssh/authorized_keys"),
            "ssh-ed25519 SYNTHETIC_AGENT_PUBLIC_KEY agent-node\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".ssh/known_hosts"),
            "agent-node ssh-ed25519 AAAA\n",
        )
        .unwrap();
        std::fs::write(home.join(".ssh/config"), "Host agent\n  User fodelf\n").unwrap();
        std::fs::write(home.join(".ssh/id_ed25519.pub"), "ssh-ed25519 AAAA\n").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "504", "504", "ccrun");
        runner.push(Output::exited(1, "")); // sudo -n true refused
        let audit = audit(Some("ccrun"), &home, &runner);
        assert_eq!(find(&audit, "No SSH keys").severity, Severity::Ok);
        assert!(audit.confined(), "{:?}", audit.findings);
    }

    /// The tripwire this replaced asserted the hole: a key one directory
    /// over was invisible, so the row could be satisfied by `mv`. P7.3 did
    /// exactly that on real hardware -- the transport key went to
    /// `~/.config/ccnm/transport/` and the row turned green while the
    /// account could SSH out as before. It is found now.
    #[test]
    fn a_private_key_in_ccnms_own_config_directory_is_found() {
        let home = empty_home("hidden-key");
        std::fs::write(
            home.join(".ssh/authorized_keys"),
            "ssh-ed25519 SYNTHETIC_AGENT_PUBLIC_KEY\n",
        )
        .unwrap();
        let transport = home.join(".config/ccnm/transport");
        std::fs::create_dir_all(&transport).unwrap();
        std::fs::write(
            transport.join("runtime"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic\n",
        )
        .unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "504", "504", "ccrun");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        let finding = find(&audit, "No SSH keys");
        assert_eq!(finding.severity, Severity::Fail, "{finding:?}");
        assert!(!audit.confined());
        // The name of the key is not in the report, in either direction.
        assert!(!finding.detail.contains("runtime"), "{finding:?}");
        assert!(
            finding.fix.as_ref().is_some_and(|f| f.contains("inbound")),
            "{finding:?}"
        );
    }

    /// ccnm's config directory holds config, and config is not a
    /// credential. Finding a key there must not turn every ordinary
    /// Runtime red.
    #[test]
    fn ccnms_own_config_files_are_not_mistaken_for_keys() {
        let home = empty_home("config-files");
        let dir = home.join(".config/ccnm");
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join("config.toml"), "this = \"runtime\"\n").unwrap();
        std::fs::write(dir.join("profiles.toml"), "").unwrap();
        std::fs::write(
            home.join(".ssh/authorized_keys"),
            "ssh-ed25519 SYNTHETIC_AGENT_PUBLIC_KEY\n",
        )
        .unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "504", "504", "ccrun");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        let finding = find(&audit, "No SSH keys");
        assert_eq!(finding.severity, Severity::Ok, "{finding:?}");
        // And it says which directories that verdict covers, rather than
        // claiming the account cannot SSH out at all.
        assert!(finding.detail.contains("~/.ssh"), "{finding:?}");
        assert!(finding.detail.contains("~/.config/ccnm"), "{finding:?}");
        assert!(
            finding.detail.contains("no other location was searched"),
            "{finding:?}"
        );
    }

    /// `runtime_user` is the Runtime Executor's expected identity, not an
    /// instruction about which account may type `ccnm`. A report can only
    /// be read correctly if it names the account it actually looked at, so
    /// the audit carries that account and the mismatch text names both.
    #[test]
    fn the_audit_names_the_account_it_looked_at_not_the_one_it_wanted() {
        let home = empty_home("operator");
        let runner = FakeRunner::new();
        identity(&runner, "bing", "501", "20 80", "staff admin");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("ccrun"), &home, &runner);
        assert_eq!(audit.user, "bing");
        assert!(
            audit.refusal(Accepted::unconfined(true)).contains("bing"),
            "{}",
            audit.refusal(Accepted::unconfined(true))
        );
        let finding = find(&audit, "Runtime user");
        assert!(finding.detail.contains("bing"), "{finding:?}");
        assert!(finding.detail.contains("ccrun"), "{finding:?}");
        // The old fix line said "start the runtime as ccrun", which reads as
        // "run this command as ccrun" and is exactly the conflation P7.4
        // removes: the account being fixed is the one the Agent lands on.
        let fix = finding.fix.clone().unwrap();
        assert!(!fix.contains("start the runtime as"), "{fix}");
        assert!(fix.contains("transport"), "{fix}");
    }

    #[test]
    fn codex_credentials_are_checked_even_when_claude_is_the_default() {
        let home = empty_home("codex-cross-provider");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), "synthetic-do-not-read").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "ccrun", "502", "20", "staff");
        runner.push(Output::exited(1, ""));
        runner.push(Output::exited(0, "")); // access check, not secret content
        let report = audit(Some("ccrun"), &home, &runner);
        assert!(!report.confined());
        assert_eq!(
            find(&report, "No Codex credential").severity,
            Severity::Fail
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("synthetic-do-not-read")
        );
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
        assert!(finding.detail.contains("credential"), "{finding:?}");
    }

    /// The escape hatch, and the shape of it.
    ///
    /// Somebody whose projects and whose agent login live in the same home
    /// has nothing to separate, and ccnm refusing to start at all leaves
    /// them with no way to use it. So there is a switch. What matters is
    /// that it is *this* switch: `allow_unconfined_exec` still does not
    /// open it, because "this account has more OS access than it should"
    /// and "this account can read my agent login" are not the same
    /// admission, and one of them is the thing this program exists to
    /// prevent.
    #[test]
    fn a_credential_is_waived_only_by_the_switch_that_names_credentials() {
        let home = empty_home("credential-waiver");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}\n").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "bing", "501", "20 80", "staff admin");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("bing"), &home, &runner);

        assert!(!audit.agent_boundary_clear(Accepted::unconfined(true)));
        assert!(!audit.exec_allowed(Accepted::unconfined(true)));
        // Naming the credential switch alone is not enough either: the
        // account is still unconfined, and that is a separate yes.
        let credentials_only = Accepted {
            unconfined_exec: false,
            unisolated_credentials: true,
        };
        assert!(audit.agent_boundary_clear(credentials_only));
        assert!(!audit.exec_allowed(credentials_only));

        let both = Accepted {
            unconfined_exec: true,
            unisolated_credentials: true,
        };
        assert!(audit.exec_allowed(both));
    }

    /// The refusal has to point at the switch that would actually change
    /// it. Sending somebody to `allow_unconfined_exec` when the blocker is
    /// a credential is how an afternoon disappears.
    #[test]
    fn the_credential_refusal_names_the_switch_that_opens_it() {
        let home = empty_home("credential-refusal");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}\n").unwrap();
        let runner = FakeRunner::new();
        identity(&runner, "bing", "501", "20 80", "staff admin");
        runner.push(Output::exited(1, ""));
        let audit = audit(Some("bing"), &home, &runner);
        let text = audit.refusal(Accepted::unconfined(true));
        assert!(text.contains("allow_unisolated_credentials"), "{text}");
        assert!(text.contains("can reach a known Agent login"), "{text}");
    }

    /// Two findings no switch reaches, and the reason is different for
    /// each: an unknown identity means nobody can say who accepted what,
    /// and inherited authentication is handed to every child rather than
    /// merely sitting on a disk somewhere.
    #[test]
    fn an_unknown_identity_is_not_waived_by_anything() {
        let home = empty_home("unknown-identity");
        let runner = FakeRunner::new(); // every probe fails
        let audit = audit(Some("ccrun"), &home, &runner);
        let everything = Accepted {
            unconfined_exec: true,
            unisolated_credentials: true,
        };
        assert_eq!(audit.user, "unknown");
        assert!(
            !audit.agent_boundary_clear(everything),
            "{:?}",
            audit.findings
        );
        assert!(!audit.exec_allowed(everything));
        let text = audit.refusal(everything);
        assert!(text.contains("no workspace switch waives this"), "{text}");
    }

    /// Once means once, and turning the switch off and on again is a new
    /// decision that gets said again.
    #[test]
    fn the_accepted_risk_is_announced_once_per_decision() {
        let state = std::env::temp_dir().join(format!("ccnm-accepted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let on = Accepted {
            unconfined_exec: true,
            unisolated_credentials: true,
        };

        let first = warn_accepted_once(&state, "xdo", on).expect("the first time says it");
        assert!(first.contains("allow_unisolated_credentials"), "{first}");
        assert!(first.contains("xdo"), "{first}");
        assert!(warn_accepted_once(&state, "xdo", on).is_none());
        // A different workspace is a different decision.
        assert!(warn_accepted_once(&state, "gld", on).is_some());
        // Off, then on again: said again.
        assert!(warn_accepted_once(&state, "xdo", Accepted::unconfined(true)).is_none());
        assert!(warn_accepted_once(&state, "xdo", on).is_some());
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn the_refusal_names_every_problem_and_its_fix() {
        let home = empty_home("refusal");
        let runner = FakeRunner::new();
        identity(&runner, "root", "0", "0", "wheel");
        runner.push(Output::exited(0, ""));
        let audit = audit(None, &home, &runner);
        let text = audit.refusal(Accepted::unconfined(true));
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
        assert!(
            !audit.exec_allowed(Accepted::unconfined(true)),
            "unknown identity cannot be waived"
        );
        assert_eq!(find(&audit, "Runs as root").severity, Severity::Fail);
        assert_eq!(find(&audit, "Not an admin").severity, Severity::Fail);
    }
}

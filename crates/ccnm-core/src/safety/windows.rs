//! What a Windows Runtime account can reach (P111.2), read from Windows's
//! own tools rather than its API, so no unsafe code is needed (D7):
//! `whoami /groups` for Administrators, the token's elevation and
//! docker-users; `sc.exe` for the SSH agent service.
//!
//! Everything is matched on what does not change with the display language:
//! SIDs, `sc.exe`'s state words, and docker-users, a name Docker creates and
//! Windows does not translate. Group names in Chinese arrive as replacement
//! characters on the test machine (code page 936, P106.4), so nothing may
//! depend on them.

use super::Finding;
use crate::process::Output;
#[cfg(windows)]
use crate::process::{Cmd, ProcessRunner};

/// BUILTIN\Administrators, on every Windows in every language.
const ADMINISTRATORS: &str = "S-1-5-32-544";

/// The token's integrity level, as the label `whoami /groups` lists. High
/// is an elevated administrator's -- and what Windows's sshd gives an
/// administrator who logs in with a key (P106.4); System and Protected
/// Process are above it.
const ELEVATED: [&str; 3] = ["S-1-16-12288", "S-1-16-16384", "S-1-16-20480"];
/// Untrusted, Low, Medium and Medium Plus: a filtered token.
const NOT_ELEVATED: [&str; 4] = ["S-1-16-0", "S-1-16-4096", "S-1-16-8192", "S-1-16-8448"];

/// One row of `whoami /groups /fo csv /nh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub name: String,
    pub sid: String,
}

/// `"BUILTIN\Administrators","Alias","S-1-5-32-544","Mandatory group, …"`
/// per line. A line that is not four quoted fields with a SID third is
/// skipped, so an answer of another shape reads as no groups -- which every
/// finding below reports as "cannot tell", never as "not a member".
pub(super) fn parse_groups(stdout: &str) -> Vec<Group> {
    stdout
        .lines()
        .filter_map(|line| {
            let inner = line
                .trim()
                .trim_start_matches('\u{feff}')
                .strip_prefix('"')?
                .strip_suffix('"')?;
            let fields: Vec<&str> = inner.split("\",\"").collect();
            let [name, _kind, sid, _attributes] = fields[..] else {
                return None;
            };
            sid.starts_with("S-1-").then(|| Group {
                name: name.to_string(),
                sid: sid.to_string(),
            })
        })
        .collect()
}

/// `whoami /groups`, by its full path like `whoami /user`.
#[cfg(windows)]
pub(super) fn read_groups(runner: &dyn ProcessRunner) -> Vec<Group> {
    let cmd = Cmd::new(crate::platform::system_program("whoami.exe"))
        .args(["/groups", "/fo", "csv", "/nh"])
        .timeout(super::PROBE_TIMEOUT);
    runner
        .run(&cmd)
        .ok()
        .filter(|out| out.success())
        .map(|out| parse_groups(&out.stdout_lossy()))
        .unwrap_or_default()
}

fn cannot_list(name: &str) -> Finding {
    Finding::fail(
        name,
        "cannot list this account's groups, so this cannot be ruled out",
        "check that `whoami /groups` works on the Runtime Node",
    )
}

/// Membership of Administrators, whatever the token says. A filtered token
/// still lists the group (as deny-only): that account is one UAC prompt
/// from administrator, the way a Mac admin is one password from root.
pub(super) fn admin_finding(groups: &[Group]) -> Finding {
    const NAME: &str = "Not an admin";
    if groups.is_empty() {
        return cannot_list(NAME);
    }
    if groups.iter().any(|g| g.sid == ADMINISTRATORS) {
        Finding::fail(
            NAME,
            "this account is in Administrators, which is a route to everything on the machine",
            "run the Runtime as a standard account, one not in Administrators",
        )
    } else {
        Finding::ok(NAME, "not in Administrators")
    }
}

/// Windows's stand-in for "No sudo": not whether the account could become
/// administrator, but whether the token every command inherits already is.
pub(super) fn elevation_finding(groups: &[Group]) -> Finding {
    const NAME: &str = "Not elevated";
    match groups.iter().find(|g| g.sid.starts_with("S-1-16-")) {
        Some(label) if ELEVATED.contains(&label.sid.as_str()) => Finding::fail(
            NAME,
            "this token is elevated (High integrity or above): every command runs with administrator rights and no prompt\nWindows's sshd gives this token to an administrator who logs in with a key",
            "run the Runtime as a standard account, one not in Administrators",
        ),
        Some(label) if NOT_ELEVATED.contains(&label.sid.as_str()) => Finding::ok(
            NAME,
            "this token is not elevated (Medium integrity or below)",
        ),
        _ if groups.is_empty() => cannot_list(NAME),
        _ => Finding::fail(
            NAME,
            "whoami listed no integrity level this knows, so whether the token is elevated cannot be told",
            "check `whoami /groups` on the Runtime Node: its Mandatory Label row",
        ),
    }
}

/// Docker Desktop's engine answers anyone in docker-users, and a container
/// that mounts `C:\` is as good as administrator. The group is Docker's,
/// created under this name, so the name is what is matched.
pub(super) fn docker_finding(groups: &[Group]) -> Finding {
    const NAME: &str = "No Docker access";
    if groups.is_empty() {
        return cannot_list(NAME);
    }
    let member = groups.iter().any(|g| {
        g.name
            .rsplit('\\')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("docker-users"))
    });
    if member {
        Finding::fail(
            NAME,
            "this account is in docker-users, so it can drive Docker's engine, which is as good as administrator",
            "remove the Runtime account from docker-users, or do not run Docker Desktop on this machine",
        )
    } else {
        Finding::ok(NAME, "not in docker-users")
    }
}

/// `sc.exe query ssh-agent`. `None` when it could not be started.
#[cfg(windows)]
pub(super) fn query_ssh_agent(runner: &dyn ProcessRunner) -> Option<Output> {
    let cmd = Cmd::new(crate::platform::system_program("sc.exe"))
        .args(["query", "ssh-agent"])
        .timeout(super::PROBE_TIMEOUT);
    runner.run(&cmd).ok().filter(|out| !out.timed_out)
}

/// The OpenSSH Authentication Agent service gives the keys loaded into it
/// to any process of the account that asks, over a named pipe: no key file
/// to find, and no `SSH_AUTH_SOCK` to inherit or strip, so the environment
/// row cannot see it. Whether it is running is what is checked -- keys can
/// be added to a running one at any time.
pub(super) fn ssh_agent_finding(answer: Option<&Output>) -> Finding {
    const NAME: &str = "No SSH agent";
    let unknown = || {
        Finding::fail(
            NAME,
            "cannot tell whether the OpenSSH Authentication Agent service is running",
            "check that `sc.exe query ssh-agent` works on the Runtime Node",
        )
    };
    let Some(out) = answer else {
        return unknown();
    };
    // ERROR_SERVICE_DOES_NOT_EXIST: no OpenSSH client feature, or removed.
    if out.exit_code == Some(1060) {
        return Finding::ok(
            NAME,
            "there is no OpenSSH Authentication Agent service on this machine",
        );
    }
    if !out.success() {
        return unknown();
    }
    // sc.exe prints the state as a number and a word ("4  RUNNING"); the
    // word is the same in every language, the label before it may not be.
    let said = out.stdout_lossy();
    let state = |word: &str| said.split_whitespace().any(|w| w == word);
    if state("STOPPED") {
        Finding::ok(
            NAME,
            "the OpenSSH Authentication Agent service is not running",
        )
    } else if state("RUNNING") {
        Finding::fail(
            NAME,
            "the OpenSSH Authentication Agent service is running: every command run as this account can use the keys loaded into it",
            "as an administrator: Stop-Service ssh-agent; Set-Service ssh-agent -StartupType Disabled",
        )
    } else {
        unknown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the Win11 test machine's ssh session lists, shape for shape
    /// (P106.4: an administrator, logged in with a key, High integrity), in
    /// English; the SIDs are what matter.
    const ADMIN_OVER_SSH: &str = "\"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Mandatory group, Enabled by default, Enabled group\"\r\n\"NT AUTHORITY\\Local account and member of Administrators group\",\"Well-known group\",\"S-1-5-114\",\"Mandatory group, Enabled by default, Enabled group\"\r\n\"BUILTIN\\Administrators\",\"Alias\",\"S-1-5-32-544\",\"Mandatory group, Enabled by default, Enabled group, Group owner\"\r\n\"BUILTIN\\Users\",\"Alias\",\"S-1-5-32-545\",\"Mandatory group, Enabled by default, Enabled group\"\r\n\"NT AUTHORITY\\NETWORK\",\"Well-known group\",\"S-1-5-2\",\"Mandatory group, Enabled by default, Enabled group\"\r\n\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\",\"\"\r\n";

    /// A standard account, and group names in a language this does not
    /// read -- as they arrive from a code-page-936 console.
    const STANDARD: &str = "\"\u{fffd}\u{fffd}\",\"\u{fffd}\",\"S-1-1-0\",\"\u{fffd}\"\r\n\"BUILTIN\\\u{fffd}\u{fffd}\",\"\u{fffd}\",\"S-1-5-32-545\",\"\u{fffd}\"\r\n\"WIN11\\docker-users\",\"\u{fffd}\",\"S-1-5-21-1-2-3-1009\",\"\u{fffd}\"\r\n\"Mandatory Label\\Medium Mandatory Level\",\"\u{fffd}\",\"S-1-16-8192\",\"\"\r\n";

    fn ok(f: &Finding) -> bool {
        f.severity == super::super::Severity::Ok
    }

    #[test]
    fn an_administrator_over_ssh_is_in_the_group_and_elevated() {
        let groups = parse_groups(ADMIN_OVER_SSH);
        assert_eq!(groups.len(), 6);
        let admin = admin_finding(&groups);
        assert!(
            !ok(&admin) && admin.detail.contains("in Administrators"),
            "{admin:?}"
        );
        let elevated = elevation_finding(&groups);
        assert!(
            !ok(&elevated) && elevated.detail.contains("elevated (High"),
            "{elevated:?}"
        );
        assert!(ok(&docker_finding(&groups)));
    }

    #[test]
    fn a_standard_account_is_read_by_sid_whatever_the_language() {
        let groups = parse_groups(STANDARD);
        assert!(ok(&admin_finding(&groups)));
        assert!(ok(&elevation_finding(&groups)));
        let docker = docker_finding(&groups);
        assert!(
            !ok(&docker) && docker.detail.contains("docker-users"),
            "{docker:?}"
        );
    }

    /// An answer of another shape is "cannot tell" in every row, not a
    /// clean bill.
    #[test]
    fn no_groups_is_not_no_membership() {
        for said in ["", "ERROR: Access is denied.\r\n", "\"a\",\"b\",\"c\"\r\n"] {
            let groups = parse_groups(said);
            assert!(groups.is_empty(), "{said:?}");
            for finding in [
                admin_finding(&groups),
                elevation_finding(&groups),
                docker_finding(&groups),
            ] {
                assert!(!ok(&finding), "{said:?}: {finding:?}");
                assert!(finding.detail.contains("cannot"), "{finding:?}");
            }
        }
        // Groups but no label: elevation alone is unknown.
        let unlabelled = parse_groups("\"Everyone\",\"Well-known group\",\"S-1-1-0\",\"\"");
        assert!(!ok(&elevation_finding(&unlabelled)));
    }

    #[test]
    fn the_ssh_agent_service_is_judged_by_its_state_word() {
        let sc = |code: i32, stdout: &str| Output::exited(code, stdout);
        let running = sc(
            0,
            "\r\nSERVICE_NAME: ssh-agent\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 4  RUNNING\r\n                                (STOPPABLE, NOT_PAUSABLE, IGNORES_SHUTDOWN)\r\n",
        );
        let stopped = sc(
            0,
            "\r\nSERVICE_NAME: ssh-agent\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        STATE              : 1  STOPPED\r\n",
        );
        let missing = sc(
            1060,
            "[SC] EnumQueryServicesStatus:OpenService FAILED 1060:\r\n\r\nThe specified service does not exist as an installed service.\r\n",
        );
        let finding = ssh_agent_finding(Some(&running));
        assert!(
            !ok(&finding) && finding.detail.contains("is running"),
            "{finding:?}"
        );
        assert!(ok(&ssh_agent_finding(Some(&stopped))));
        assert!(ok(&ssh_agent_finding(Some(&missing))));
        for unclear in [
            sc(5, "[SC] OpenService FAILED 5:\r\n\r\nAccess is denied.\r\n"),
            sc(0, "STATE : 7  PAUSED\r\n"),
        ] {
            assert!(!ok(&ssh_agent_finding(Some(&unclear))), "{unclear:?}");
        }
        assert!(!ok(&ssh_agent_finding(None)));
    }
}

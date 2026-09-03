//! The work-controller role: what `ccnm` does on the work machine when the
//! home launcher calls it over ssh. This build has `probe` (read-only, for
//! doctor). `work-run`, which sets up a session and starts Claude, comes
//! with phase 3.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude;
use crate::error::{Error, ErrorCode};
use crate::process::ProcessRunner;
use crate::protocol::PROTOCOL;
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::probe::{ClaudeReport, ProbeReport, ProbeRequest};
use crate::ssh::{Master, Ssh};

/// What the work-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`claude::locate`] found one.
    pub claude: Option<PathBuf>,
}

/// Everything doctor wants to know about this machine, in one round trip.
/// Read-only: no master connection, no file written.
pub fn probe(req: &ProbeRequest, tools: &Tools<'_>) -> ProbeReport {
    let (home_ssh, home_hello) = match Ssh::new(&req.home_alias, &tools.control_dir)
        .map(|ssh| ssh.with_ccnm_bin(&req.home_ccnm_bin))
    {
        Err(e) => (
            Err(e.into()),
            Err(Error::new(
                ErrorCode::HomeUnreachable,
                "not attempted: home alias is invalid",
            )
            .into()),
        ),
        Ok(ssh) => {
            let home_ssh = ssh.resolve(tools.runner).map_err(Into::into);
            let home_hello = ssh
                .check_control_path()
                .and_then(|()| {
                    ssh.call_ccnm::<_, HelloReport>(
                        tools.runner,
                        Master::Reuse,
                        &["internal", "hello"],
                        &HelloRequest::new(Some(req.root.clone())),
                        Duration::from_secs(30),
                        ErrorCode::HomeUnreachable,
                    )
                })
                .map_err(Into::into);
            (home_ssh, home_hello)
        }
    };

    ProbeReport {
        protocol: PROTOCOL,
        hello: hello::answer(&HelloRequest::new(None)),
        claude: probe_claude(tools, req.claude_config_dir.as_deref()),
        home_ssh,
        home_hello,
    }
}

fn probe_claude(tools: &Tools<'_>, config_dir: Option<&Path>) -> ClaudeReport {
    let Some(bin) = &tools.claude else {
        let missing = Error::new(
            ErrorCode::Version,
            "claude not found: looked in PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
        );
        return ClaudeReport {
            path: None,
            version: Err((&missing).into()),
            auth: Err(missing.into()),
        };
    };
    let version = tools
        .runner
        .run(&claude::version_cmd(bin, config_dir))
        .and_then(|out| claude::parse_version(&out))
        .map_err(Into::into);
    let auth = tools
        .runner
        .run(&claude::auth_status_cmd(bin, config_dir))
        .and_then(|out| claude::parse_auth(&out))
        .map_err(Into::into);
    ClaudeReport {
        path: Some(bin.clone()),
        version,
        auth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};
    use crate::protocol::hello::PathStatus;

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-work-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    fn hello_json(root_ok: bool) -> String {
        let rep = HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from("/Users/ccrun/.local/bin/ccnm")),
            root: Some(PathStatus {
                exists: root_ok,
                is_dir: root_ok,
            }),
        };
        serde_json::to_string(&rep).unwrap()
    }

    fn request() -> ProbeRequest {
        ProbeRequest {
            protocol: PROTOCOL,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/ccrun/Projects/xshun"),
            home_alias: "ccnm-home".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: Some(PathBuf::from("/x/claude")),
        }
    }

    #[test]
    fn probe_collects_every_fact_in_one_report() {
        let dir = temp("probe");
        let fake = FakeRunner::new();
        // Call order: ssh -G, ssh internal hello, claude --version, claude auth status.
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
        fake.push(Output::exited(0, r#"{"loggedIn":true,"email":"me@x"}"#));

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
        };
        let rep = probe(&request(), &tools);

        assert_eq!(rep.hello.ccnm_version, crate::VERSION);
        assert_eq!(rep.home_ssh.as_ref().unwrap().target(), "ccrun@home.ts");
        let home = rep.home_hello.as_ref().unwrap();
        assert_eq!(home.user, "ccrun");
        assert!(home.root.unwrap().is_ok());
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert!(rep.claude.auth.as_ref().unwrap().logged_in);

        let calls = fake.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].display(), "ssh -G ccnm-home");
        let reverse = calls[1].display();
        assert!(
            reverse.contains("ControlMaster=no"),
            "doctor path must not start a master: {reverse}"
        );
        assert!(
            reverse.contains("-T ccnm-home ~/.local/bin/ccnm internal hello --payload"),
            "{reverse}"
        );
        // The hello asked the home side to look at the workspace root.
        let wire = calls[1].args.last().unwrap().to_string_lossy().into_owned();
        let sent: HelloRequest = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(
            sent.root,
            Some(PathBuf::from("/Users/ccrun/Projects/xshun"))
        );
        assert!(
            calls[2]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );

        // Nothing was written by probe.
        assert!(
            !control(&dir).exists(),
            "probe must not create the control dir"
        );

        let json = serde_json::to_vec(&rep).unwrap();
        let back: ProbeReport = crate::protocol::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn probe_records_failures_instead_of_aborting() {
        let dir = temp("probe-fail");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        let mut unreachable = Output::exited(255, "");
        unreachable.stderr =
            b"ssh: connect to host home.ts port 22: Operation timed out\n".to_vec();
        fake.push(unreachable);

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let rep = probe(&request(), &tools);
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message.contains("Operation timed out"));
        assert_eq!(rep.claude.path, None);
        assert_eq!(rep.claude.version.unwrap_err().code(), ErrorCode::Version);
        assert_eq!(fake.calls().len(), 2, "no claude calls without a binary");
    }

    #[test]
    fn missing_home_binary_is_a_version_error_naming_the_path() {
        let dir = temp("probe-127");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        fake.push(Output::exited(127, ""));
        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let rep = probe(&request(), &tools);
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message.contains("~/.local/bin/ccnm"), "{}", err.message);
    }
}

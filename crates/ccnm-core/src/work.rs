//! What `ccnm` does on the work machine when the home launcher calls it
//! over ssh. This build has `probe` (read-only, for doctor). `work-run`,
//! which sets up a session and starts Claude, comes next.
//!
//! This code runs in an **ssh session**, which is not the login session.
//! Anything that needs the login session — asking Claude about its
//! credentials, and later starting it — is forwarded to
//! [`crate::controller`] rather than done here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude::{self, ClaudeReport};
use crate::controller;
use crate::error::{Error, ErrorCode, ErrorReport, Reported, Result};
use crate::mcp;
use crate::process::ProcessRunner;
use crate::protocol::PROTOCOL;
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::mcp::{ProbeReport as McpProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::ssh::{Master, Ssh};

/// What the work-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`claude::locate`] found one. Only used as
    /// a fallback for the version when no controller is running; the
    /// controller finds its own, in launchd's environment.
    pub claude: Option<PathBuf>,
    /// The controller's socket on this machine.
    pub controller: PathBuf,
}

/// Everything doctor wants to know about this machine, in one round trip.
/// Read-only: no master connection, no file written. The MCP handshake
/// starts a server on the home runtime and shuts it down again before
/// returning (design doc section 4).
pub fn probe(req: &ProbeRequest, tools: &Tools<'_>) -> ProbeReport {
    let (home_ssh, home_hello, mcp) = match Ssh::new(&req.home_alias, &tools.control_dir)
        .map(|ssh| ssh.with_ccnm_bin(&req.home_ccnm_bin))
    {
        Err(e) => (
            Err(e.into()),
            Err(Error::new(
                ErrorCode::HomeUnreachable,
                "not attempted: home alias is invalid",
            )
            .into()),
            None,
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
            // Only worth the round trips if the plain reverse ssh worked.
            let mcp = (req.mcp_calls > 0 && home_hello.is_ok())
                .then(|| mcp_handshake(req, &ssh).map_err(Into::into));
            (home_ssh, home_hello, mcp)
        }
    };

    let (controller, claude) = ask_about_claude(tools, req.claude_config_dir.as_deref());
    ProbeReport {
        protocol: PROTOCOL,
        hello: hello::answer(&HelloRequest::new(None)),
        controller: Some(controller),
        claude,
        home_ssh,
        home_hello,
        mcp,
    }
}

/// Claude's login state, from the only context whose answer means
/// anything.
///
/// With a controller, everything about Claude comes from it: not just the
/// login but the binary and version too, because the controller's `PATH`
/// is launchd's, and that is the `claude` a session would really start.
///
/// Without one, the version is still worth reporting — it needs no
/// credential — but the login is left as `CCNM_E_NOT_READY`. This session
/// *can* run `claude auth status`; the point is that its answer would be
/// wrong, and a wrong row sends the user to log in on a machine that is
/// already logged in.
fn ask_about_claude(
    tools: &Tools<'_>,
    config_dir: Option<&Path>,
) -> (Reported<controller::Context>, ClaudeReport) {
    match controller::context(&tools.controller) {
        Ok(ctx) => {
            // A controller that is not in a login session is asked only
            // for the version. Its answer about the login would be no
            // better than this session's, and the rule holds everywhere:
            // do not run a command whose result has to be thrown away.
            let ask = if ctx.login_session() {
                claude::Ask::Everything
            } else {
                claude::Ask::VersionOnly
            };
            let claude = controller::claude_auth(&tools.controller, config_dir, ask)
                .unwrap_or_else(|e| ClaudeReport {
                    path: None,
                    version: Err((&e).into()),
                    auth: Err(e.into()),
                });
            (Ok(ctx), claude)
        }
        Err(missing) => {
            let mut claude = claude::report(
                tools.claude.as_deref(),
                config_dir,
                tools.runner,
                claude::Ask::VersionOnly,
            );
            claude.auth = Err(ErrorReport::new(
                ErrorCode::NotReady,
                format!(
                    "not checked: no work controller to ask, and this ssh session's answer would be wrong\n{}",
                    missing.message()
                ),
            ));
            (Err(missing.into()), claude)
        }
    }
}

fn mcp_handshake(req: &ProbeRequest, ssh: &Ssh) -> Result<McpProbeReport> {
    let wire = payload::encode(&ServePayload::new(
        &req.workspace,
        req.root.clone(),
        &format!("probe-{}", uuid::Uuid::new_v4().hyphenated()),
    ))?;
    let cmd = ssh.mcp_transport_cmd(&wire)?;
    mcp::probe::probe(
        &cmd,
        req.mcp_calls,
        Duration::from_secs(30) + Duration::from_millis(500) * req.mcp_calls,
        ErrorCode::HomeUnreachable,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::process::{Cmd, FakeRunner, Output};
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
            mcp_calls: 0,
        }
    }

    /// A socket path no controller is on, so the probe takes the
    /// no-controller branch.
    fn absent_socket(test: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/ccnm-absent-{}-{test}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn probe_collects_every_fact_in_one_report() {
        let dir = temp("probe");
        let fake = FakeRunner::new();
        // Call order: ssh -G, ssh internal hello, claude --version. No
        // `claude auth status`: with no controller its answer would be
        // wrong, so it is not asked at all.
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
            controller: absent_socket("probe"),
        };
        let rep = probe(&request(), &tools);

        assert_eq!(rep.hello.ccnm_version, crate::VERSION);
        assert_eq!(rep.home_ssh.as_ref().unwrap().target(), "ccrun@home.ts");
        let home = rep.home_hello.as_ref().unwrap();
        assert_eq!(home.user, "ccrun");
        assert!(home.root.unwrap().is_ok());
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.claude.auth.as_ref().unwrap_err().code(),
            ErrorCode::NotReady,
            "an unaskable login must not be reported as logged out"
        );
        assert_eq!(
            rep.controller
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .code(),
            ErrorCode::NotReady
        );
        assert_eq!(rep.mcp, None, "mcp_calls = 0 means no handshake");

        let calls = fake.calls();
        assert_eq!(
            calls.len(),
            3,
            "{:?}",
            calls.iter().map(Cmd::display).collect::<Vec<_>>()
        );
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
            controller: absent_socket("probe-fail"),
        };
        let rep = probe(
            &ProbeRequest {
                mcp_calls: 5,
                ..request()
            },
            &tools,
        );
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message.contains("Operation timed out"));
        assert_eq!(rep.mcp, None, "no MCP attempt after a failed hello");
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
            controller: absent_socket("probe-127"),
        };
        let rep = probe(&request(), &tools);
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message.contains("~/.local/bin/ccnm"), "{}", err.message);
    }

    /// The point of the whole phase: when a controller is listening, every
    /// question about Claude goes to it, and this ssh session runs no
    /// `claude` at all -- not even the version, because the controller's
    /// PATH is the one Claude will really be started from.
    #[test]
    fn with_a_controller_claude_is_asked_there_and_not_here() {
        let dir = temp("probe-controller");
        let socket = PathBuf::from(format!("/tmp/ccnm-wp-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();

        // The controller's own environment: a login session, and a claude
        // that answers both questions.
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Aqua\n"));
            inner.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
            inner.push(Output::exited(
                0,
                r#"{"loggedIn":true,"email":"me@x","authMethod":"claude.ai"}"#,
            ));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
            };
            listener.serve_one(&tools).unwrap(); // hello
            listener.serve_one(&tools).unwrap(); // claude-auth
            inner.calls()
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
            controller: socket.clone(),
        };
        let rep = probe(&request(), &tools);

        let ctx = rep.controller.as_ref().unwrap().as_ref().unwrap();
        assert!(ctx.login_session(), "{ctx:?}");
        assert!(rep.claude.auth.as_ref().unwrap().logged_in);
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.claude.path,
            Some(PathBuf::from("/opt/homebrew/bin/claude")),
            "the binary reported must be the controller's, not this session's"
        );
        let ssh_calls: Vec<String> = fake.calls().iter().map(Cmd::display).collect();
        assert_eq!(ssh_calls.len(), 2, "{ssh_calls:?}");
        assert!(
            !ssh_calls.iter().any(|c| c.contains("claude")),
            "the ssh session must not run claude itself: {ssh_calls:?}"
        );

        // ...and the controller ran exactly the two claude commands, with
        // the config dir from the request.
        let inner_calls = served.join().unwrap();
        assert_eq!(inner_calls.len(), 3);
        assert!(inner_calls[1].display().contains("--version"));
        assert!(inner_calls[2].display().contains("auth status"));
        assert!(
            inner_calls[2]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );
    }
}

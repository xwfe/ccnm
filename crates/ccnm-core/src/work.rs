//! What `ccnm` does on the work machine when the home launcher calls it
//! over ssh: `probe` (read-only, for doctor) and `work-run` (create a
//! session, have the controller start it, wait for the result).
//!
//! This code runs in an **ssh session**, which is not the login session.
//! Anything that needs the login session — asking Claude about its
//! credentials, starting it — is forwarded to [`crate::controller`]
//! rather than done here. Everything else (writing the session files,
//! waiting, reading the output) is done here: same account, same disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude::{self, ClaudeReport};
use crate::controller;
use crate::error::{Error, ErrorCode, ErrorReport, Reported, Result};
use crate::mcp;
use crate::paths;
use crate::process::ProcessRunner;
use crate::protocol::PROTOCOL;
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::mcp::{ProbeReport as McpProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::protocol::run::{RunReport, RunRequest};
use crate::session::{self, Mode, Spec};
use crate::ssh::{Master, Ssh};

/// What the work-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// This machine's state root; sessions and workspace dirs go under it.
    pub state: PathBuf,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`claude::locate`] found one. Only used as
    /// a fallback for the version when no controller is running; the
    /// controller finds its own, in launchd's environment.
    pub claude: Option<PathBuf>,
    /// The controller's socket on this machine.
    pub controller: PathBuf,
}

/// Grace beyond the session's own timeout before giving up on the `exit`
/// file. The supervisor kills Claude at the session timeout and writes
/// the file right after; if that has not happened this much later, the
/// supervisor itself is gone.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// Start a print-mode session and wait for its result.
///
/// Refuses without a controller in a login session — the same rule doctor
/// applies, for the same reason: a Claude started from anywhere else
/// cannot read its own credentials, and the failure it would produce
/// ("not logged in") is a lie about the machine.
pub fn run(req: &RunRequest, tools: &Tools<'_>) -> Result<RunReport> {
    let ctx = controller::context(&tools.controller)?;
    if !ctx.login_session() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "the work controller answers from {}, not from a login session, so a Claude it started could not read its credentials\nrun on work: ccnm work-controller install",
                ctx.describe()
            ),
        ));
    }
    let ssh = Ssh::new(&req.home_alias, &tools.control_dir)?.with_ccnm_bin(&req.home_ccnm_bin);
    let cwd = paths::workspace_dir(&tools.state, &req.workspace);
    std::fs::create_dir_all(&cwd)?;
    let spec = Spec {
        protocol: PROTOCOL,
        id: session::new_id(),
        workspace: req.workspace.clone(),
        root: req.root.clone(),
        home_alias: req.home_alias.clone(),
        home_ccnm_bin: req.home_ccnm_bin.clone(),
        claude_config_dir: req.claude_config_dir.clone(),
        permission_mode: req.permission_mode,
        mode: Mode::Print {
            prompt: req.prompt.clone(),
        },
        timeout_secs: req.timeout_secs,
        cwd,
    };
    let dir = session::create(&tools.state, &spec, &ssh)?;
    let pid = controller::start(&tools.controller, dir.path())?;
    let outcome =
        session::wait_for_outcome(&dir, Duration::from_secs(req.timeout_secs) + EXIT_GRACE)?;

    let stdout = std::fs::read(dir.stdout()).unwrap_or_default();
    let result = claude::parse_print(&stdout).ok();
    let stdout_tail = if result.is_some() {
        String::new()
    } else {
        tail(&stdout)
    };
    let stderr_tail = tail(&std::fs::read(dir.stderr()).unwrap_or_default());
    Ok(RunReport {
        protocol: PROTOCOL,
        session: spec.id,
        session_dir: dir.path().to_path_buf(),
        controller: ctx,
        pid,
        outcome,
        result,
        stdout_tail,
        stderr_tail,
    })
}

/// The last 2 KiB, on a character boundary. Enough to see why, never the
/// whole thing: the whole thing is in the session directory.
fn tail(bytes: &[u8]) -> String {
    const KEEP: usize = 2048;
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= KEEP {
        return text.into_owned();
    }
    let mut start = text.len() - KEEP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
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
            state: dir.clone(),
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
            state: dir.clone(),
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
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            controller: absent_socket("probe-127"),
        };
        let rep = probe(&request(), &tools);
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message.contains("~/.local/bin/ccnm"), "{}", err.message);
    }

    fn run_request(prompt: &str) -> RunRequest {
        RunRequest {
            protocol: PROTOCOL,
            workspace: "fixture".into(),
            root: PathBuf::from("/Users/bing/ccnm-fixture"),
            home_alias: "xdwmbp".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::AcceptEdits,
            prompt: prompt.into(),
            timeout_secs: 5,
        }
    }

    #[test]
    fn run_refuses_without_a_controller_and_creates_nothing() {
        let dir = temp("run-none");
        let tools = Tools {
            runner: &FakeRunner::new(),
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            controller: absent_socket("run-none"),
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn run_refuses_a_controller_outside_the_login_session() {
        let dir = temp("run-bg");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-bg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Background\n"));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
        });
        let tools = Tools {
            runner: &FakeRunner::new(),
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            controller: socket,
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        served.join().unwrap();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("Background"), "{err}");
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    /// The whole print-mode path with everything real except Claude: a
    /// real socket, the real Start handler, a real detached spawn of a
    /// stand-in supervisor, the real wait, the real parse. The stand-in
    /// records the argv it was given and finishes the session the way the
    /// real supervisor would: by writing `exit` last.
    #[test]
    fn run_starts_a_session_through_the_controller_and_brings_back_the_result() {
        let dir = temp("run-ok");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-ok-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let sessions = dir.join("sessions");
        let supervisor = dir.join("fake-supervisor");
        std::fs::write(
            &supervisor,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nfor s in {sessions}/*/; do\n  printf '{{\"is_error\":false,\"result\":\"hi from claude\",\"num_turns\":1}}' > \"$s/stdout\"\n  : > \"$s/stderr\"\n  printf '{{\"exit_code\":0,\"timed_out\":false,\"duration_ms\":42}}' > \"$s/exit.tmp\"\n  mv \"$s/exit.tmp\" \"$s/exit\"\ndone\n",
                argv = dir.join("supervisor-argv").display(),
                sessions = sessions.display(),
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&supervisor, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn({
            let supervisor = supervisor.clone();
            move || {
                let inner = FakeRunner::new();
                inner.push(Output::exited(0, "Aqua\n"));
                let tools = crate::controller::Tools {
                    runner: &inner,
                    claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                    exe: supervisor,
                };
                listener.serve_one(&tools).unwrap(); // hello
                listener.serve_one(&tools).unwrap(); // start
            }
        });

        let tools = Tools {
            runner: &FakeRunner::new(),
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            controller: socket,
        };
        let rep = run(&run_request("fix the failing test"), &tools).unwrap();
        served.join().unwrap();

        assert!(rep.outcome.ok(), "{:?}", rep.outcome);
        assert_eq!(rep.outcome.duration_ms, 42);
        let result = rep.result.expect("a parsed result");
        assert_eq!(result.result.as_deref(), Some("hi from claude"));
        assert!(rep.stdout_tail.is_empty(), "no tail when the result parsed");
        assert!(rep.pid > 0);
        assert!(rep.controller.login_session());

        // The session directory is the report's, named by the id, with
        // the inputs Claude would have been started with.
        assert_eq!(rep.session_dir, sessions.join(&rep.session));
        let session_dir = crate::session::Dir::at(&rep.session_dir);
        let spec = crate::session::load(&session_dir).unwrap();
        assert_eq!(spec.workspace, "fixture");
        assert_eq!(spec.cwd, dir.join("workspaces/fixture"));
        assert!(
            spec.cwd.is_dir(),
            "Claude's cwd must exist before it starts"
        );
        assert!(session_dir.mcp_config().exists());
        assert!(session_dir.settings().exists());
        assert!(session_dir.supervisor_log().exists());

        // The supervisor got exactly one payload naming this session and
        // the controller's claude, not this session's.
        let argv = std::fs::read_to_string(dir.join("supervisor-argv")).unwrap();
        let mut lines = argv.lines();
        assert_eq!(lines.next(), Some("internal"));
        assert_eq!(lines.next(), Some("supervise"));
        assert_eq!(lines.next(), Some("--payload"));
        let req: crate::session::SuperviseRequest =
            crate::protocol::payload::decode(lines.next().unwrap()).unwrap();
        assert_eq!(req.session_dir, rep.session_dir);
        assert_eq!(req.claude_bin, PathBuf::from("/opt/homebrew/bin/claude"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail(b"short"), "short");
        let long = format!("{}中文结尾", "x".repeat(3000));
        let t = tail(long.as_bytes());
        assert!(t.starts_with("..."));
        assert!(t.ends_with("中文结尾"));
        assert!(t.len() <= 2048 + 3 + 3, "{}", t.len());
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
                exe: PathBuf::from("/x/ccnm"),
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
            state: dir.clone(),
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

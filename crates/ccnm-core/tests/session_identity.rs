use ccnm_core::instance::{AgentIdentity, AgentLocal, AgentProfiles, InstanceRef};
use ccnm_core::process::{FakeRunner, Output};
use ccnm_core::protocol::run::{
    HistoryRequest, OutputRequest, ResultRequest, SessionState, StartRequest, StatusRequest,
    StopRequest,
};
use ccnm_core::provider::{AgentBinaries, AgentProvider};
use ccnm_core::session::{self, Dir, Mode, RuntimeLink, Spec};
use ccnm_core::{Config, ErrorCode, Lang, overview, paths, work};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p3-session-{}", session::new_id()));
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".config/ccnm/agents/codex")).unwrap();
        for path in [
            &home,
            &home.join(".claude"),
            &home.join(".config/ccnm/agents/codex"),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::create_dir_all(paths::sessions_dir(&state)).unwrap();
        Self { root, home, state }
    }
    fn identity(&self, instance: &str, provider: AgentProvider) -> AgentIdentity {
        AgentIdentity {
            node: "worker".into(),
            instance: instance.into(),
            provider,
            profile_ref: "default".into(),
        }
    }
    fn record(
        &self,
        id: &str,
        workspace: &str,
        identity: Option<AgentIdentity>,
        mode: Mode,
    ) -> Dir {
        let dir = Dir::at(paths::session_dir(&self.state, id));
        std::fs::create_dir_all(dir.path()).unwrap();
        let spec = Spec {
            protocol: if identity.is_some() { 3 } else { 1 },
            runtime_node: identity.as_ref().map(|_| "runtime".into()),
            provider: identity
                .as_ref()
                .map_or(AgentProvider::Claude, |id| id.provider),
            agent_identity: identity,
            id: id.into(),
            workspace: workspace.into(),
            root: "/runtime/project".into(),
            runtime: Some(RuntimeLink {
                alias: "runtime".into(),
                ccnm_bin: "/runtime/ccnm".into(),
            }),
            provider_config_dir: None,
            permission_mode: Default::default(),
            mode,
            timeout_secs: 60,
            cwd: self.root.join("cwd"),
            codex_exec_server: false,
            agent_tools: Default::default(),
            ask_before: Vec::new(),
        };
        std::fs::write(dir.meta(), serde_json::to_vec(&spec).unwrap()).unwrap();
        dir
    }
    fn config(&self) -> Config {
        Config::parse(include_str!(
            "../../../tests/fixtures/agent-instance/agent.toml"
        ))
        .unwrap()
    }
    fn tools<'a>(&self, runner: &'a dyn ccnm_core::ProcessRunner) -> work::Tools<'a> {
        work::Tools {
            runner,
            config: self.config(),
            local: Some(
                AgentLocal::new(AgentProfiles::default(), self.home.clone(), None).unwrap(),
            ),
            state: self.state.clone(),
            control_dir: self.root.join("control"),
            agents: AgentBinaries::with_claude(None),
            controller: self.root.join("absent.sock"),
            tmux: Some("/fixture/tmux".into()),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn reference(instance: &str) -> InstanceRef {
    InstanceRef {
        node: "worker".into(),
        instance: instance.into(),
    }
}

#[test]
fn exact_result_never_crosses_workspace_or_instance_and_keeps_provider_id_separate() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000010";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    std::fs::write(
        dir.stdout(),
        r#"{"is_error":false,"result":"ok","session_id":"provider-thread","num_turns":1}"#,
    )
    .unwrap();
    std::fs::write(
        dir.exit(),
        r#"{"exit_code":0,"timed_out":false,"duration_ms":4}"#,
    )
    .unwrap();
    let runner = FakeRunner::new();
    let tools = f.tools(&runner);
    let request = |workspace: &str, agent: &str| ResultRequest {
        protocol: 3,
        workspace: workspace.into(),
        agent: Some(reference(agent)),
        session: Some(id.into()),
    };
    let report = work::result(&request("demo", "claude-main"), &tools).unwrap();
    assert_eq!(report.session, id);
    assert_eq!(
        report.result.as_ref().unwrap().provider_session_id(),
        Some("provider-thread")
    );
    assert_ne!(report.session, "provider-thread");
    assert!(work::result(&request("other", "claude-main"), &tools).is_err());
    assert!(work::result(&request("demo", "codex-main"), &tools).is_err());
    assert!(
        work::result(
            &ResultRequest {
                protocol: 3,
                workspace: "demo".into(),
                agent: Some(reference("claude-main")),
                session: Some("../escape".into())
            },
            &tools
        )
        .is_err()
    );
    assert!(runner.calls().is_empty());
}

#[test]
fn exact_status_distinguishes_terminal_starting_and_unknown_without_guessing() {
    let f = Fixture::new();
    let terminal = "00000000-0000-4000-8000-000000000011";
    let dir = f.record(
        terminal,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    std::fs::write(
        dir.stdout(),
        r#"{"is_error":false,"result":"ok","session_id":"provider-thread","num_turns":1}"#,
    )
    .unwrap();
    std::fs::write(
        dir.exit(),
        r#"{"exit_code":0,"timed_out":false,"duration_ms":4}"#,
    )
    .unwrap();
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "tmux 3.7c\n"));
    runner.push(Output::exited(1, ""));
    let report = work::status_checked(
        &StatusRequest {
            protocol: 3,
            workspace: Some("demo".into()),
            agent: Some(reference("claude-main")),
            session: Some(terminal.into()),
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert_eq!(report.records[0].state, SessionState::Completed);
    assert_eq!(
        report.records[0].provider_session_id.as_deref(),
        Some("provider-thread")
    );

    let unknown = "00000000-0000-4000-8000-000000000012";
    let dir = f.record(
        unknown,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::write_supervisor_pid(&dir, 999_999).unwrap();
    let runner = FakeRunner::new();
    runner.push(Output::exited(1, "")); // no tmux session
    runner.push(Output::exited(1, "")); // supervisor pid absent
    runner.push(Output::exited(0, "tmux 3.7c\n"));
    runner.push(Output::exited(1, ""));
    let report = work::status_checked(
        &StatusRequest {
            protocol: 3,
            workspace: Some("demo".into()),
            agent: Some(reference("claude-main")),
            session: Some(unknown.into()),
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert_eq!(report.records[0].state, SessionState::Unknown);
}

/// F6. A workspace has one terminal, and whichever instance started it has
/// it. On the P62 machine `ccnm status p62rust` -- no `--agent`, so the
/// default instance -- said "no live sessions" while a Codex session of that
/// very workspace was running. The filter is right (the question named an
/// instance); the answer left out the one thing the person was looking for.
#[test]
fn status_for_one_instance_says_when_another_instance_has_the_workspace() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000040";
    f.record(
        id,
        "demo",
        Some(f.identity("codex-main", AgentProvider::Codex)),
        Mode::Interactive { prompt: None },
    );
    let asked = |agent: &str| {
        let runner = FakeRunner::new();
        runner.push(Output::exited(0, "tmux 3.7c\n")); // -V
        runner.push(Output::exited(0, "ccnm-demo\t1788496263\t0\t1\n")); // list-sessions
        runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        runner.push(Output::exited(0, "/sbin/launchd\n")); // ps, for the transport
        work::status_checked(
            &StatusRequest {
                protocol: 3,
                workspace: Some("demo".into()),
                agent: Some(reference(agent)),
                session: None,
            },
            &f.tools(&runner),
        )
        .unwrap()
    };

    let claude = asked("claude-main");
    assert!(
        claude.sessions.is_empty(),
        "it is not claude-main's session"
    );
    for (lang, hint) in [
        (Lang::En, "--agent codex-main"),
        (Lang::Zh, "--agent codex-main"),
    ] {
        let said = claude.render_in(lang);
        assert!(said.contains(hint), "{said}");
        // And it no longer reads as "nothing is running for this project".
        assert!(said.contains("claude-main"), "{said}");
    }

    // Asked about the instance that has it: the session, and no hint.
    let codex = asked("codex-main");
    assert_eq!(codex.sessions.len(), 1);
    assert!(!codex.render_in(Lang::En).contains("--agent"));
}

#[test]
fn exact_stop_checks_identity_before_kill_and_records_confirmed_terminal_state() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000013";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );

    let wrong = FakeRunner::new();
    wrong.push(Output::exited(0, ""));
    wrong.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    let error = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("codex-main")),
            session: Some(id.into()),
            assigned: false,
        },
        &f.tools(&wrong),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert!(
        !wrong
            .calls()
            .iter()
            .any(|cmd| cmd.display().contains("kill-session"))
    );

    let runner = FakeRunner::new();
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(1, ""));
    runner.push(Output::exited(0, "")); // no matching Runtime MCP process
    let report = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: Some(id.into()),
            assigned: false,
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert!(report.killed);
    assert_eq!(report.session.as_deref(), Some(id));
    // Its own outcome since P64 (F4): `error` means "never started", and
    // this session did.
    let outcome = session::read_outcome(&dir).unwrap().unwrap();
    assert!(outcome.stopped);
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.exit_code, None);
    let repeated = FakeRunner::new();
    let report = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: Some(id.into()),
            assigned: false,
        },
        &f.tools(&repeated),
    )
    .unwrap();
    assert!(!report.killed, "terminal stop is idempotent");
    assert!(repeated.calls().is_empty());
}

/// Stopping something that is already stopped is success, not
/// `CCNM_E_NOT_READY`. Until v1 it was the error, which made every cleanup
/// script that calls `stop` unconditionally look like it failed -- and a
/// `--print` run that finished on its own could never be stopped
/// successfully, because by then there is no terminal left to kill.
///
/// The returned identity matters as much as the exit code: the Runtime side
/// compares it against the instance it selected and fails the call when they
/// differ, so an empty one would turn this into `CCNM_E_VERSION`.
#[test]
fn stopping_a_workspace_with_nothing_running_succeeds_and_still_names_the_selection() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    runner.push(Output::exited(1, "")); // tmux has-session: nothing there
    let report = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: None,
            assigned: false,
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert!(!report.killed, "nothing was running, so nothing was killed");
    assert_eq!(
        report.agent_identity.as_ref().map(AgentIdentity::reference),
        Some(reference("claude-main"))
    );
    assert!(
        runner
            .calls()
            .iter()
            .all(|cmd| !cmd.display().contains("kill-session")),
        "an already-stopped workspace is not signalled"
    );
}

/// A named session whose terminal is gone gets its terminal outcome written
/// on the way out. Without that the record would stay outcome-less and
/// `status` would report it as unknown for good.
#[test]
fn stopping_a_session_whose_terminal_vanished_records_its_terminal_outcome() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000024";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    assert!(session::read_outcome(&dir).unwrap().is_none());
    let runner = FakeRunner::new();
    runner.push(Output::exited(1, ""));
    let report = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: Some(id.into()),
            assigned: false,
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert!(!report.killed);
    assert_eq!(report.session.as_deref(), Some(id));
    assert!(
        session::read_outcome(&dir)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("no managed terminal was running")
    );
}

fn exact_interactive_stop(id: &str) -> StopRequest {
    StopRequest {
        protocol: 3,
        workspace: "demo".into(),
        agent: Some(reference("claude-main")),
        session: Some(id.into()),
        assigned: false,
    }
}

/// One `ps` line for the session's own Runtime MCP transport: the payload is
/// what `stop` looks for, and it is unique to the session.
fn transport_line(dir: &Dir) -> Output {
    let payload = AgentProvider::Claude
        .transport_payload(dir)
        .expect("a remote session has a transport payload");
    Output::exited(
        0,
        format!("/usr/bin/ssh runtime /runtime/ccnm internal mcp-serve --payload {payload}\n"),
    )
}

/// Pretend the file was written `secs` ago.
fn backdate(path: &std::path::Path, secs: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(then)
        .unwrap();
}

/// The one `ccnm log` line this fixture's only session gets.
fn log_line(f: &Fixture) -> String {
    let runner = FakeRunner::new();
    let report = work::history(
        &HistoryRequest {
            protocol: 1,
            workspace: Some("demo".into()),
            limit: 10,
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert_eq!(report.sessions.len(), 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let text = overview::render_history(&report.sessions, now, 0, Lang::En);
    text.lines().nth(1).expect("one row").to_string()
}

/// F4, first half. The terminal is gone the moment tmux says so; the ssh it
/// carried to the Runtime takes a little longer. `stop` used to look at `ps`
/// once, right after the kill, and on the P62 machines a Codex session was
/// still there all three times -- so a stop that had worked answered
/// `CCNM_E_NOT_READY`. It now looks again until the transport is gone.
#[test]
fn exact_stop_waits_for_the_runtime_transport_instead_of_looking_once() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000030";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "")); // tmux has-session
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    runner.push(Output::exited(0, "")); // tmux kill-session
    runner.push(Output::exited(1, "")); // tmux has-session: gone
    runner.push(transport_line(&dir)); // ps: the ssh is still exiting
    runner.push(transport_line(&dir));
    runner.push(Output::exited(0, "/sbin/launchd\n")); // ps: gone
    let report = work::stop(&exact_interactive_stop(id), &f.tools(&runner)).unwrap();
    assert!(report.killed);
    assert_eq!(report.session.as_deref(), Some(id));
    assert!(session::read_outcome(&dir).unwrap().is_some());
}

/// A tmux server of this test's own, and something to take it down again.
///
/// ccnm always talks to `tmux -L ccnm`, which on a developer's machine is
/// the server their real sessions live on. The "binary" handed to ccnm here
/// is a wrapper that moves the socket directory, so nothing this test starts
/// or kills is ever in that server. Under `/tmp` because a socket path has
/// to fit in 104 bytes and `$TMPDIR` on macOS does not leave room.
struct OwnTmux {
    dir: ccnm_testdir::TestDir,
    wrapper: PathBuf,
}

impl OwnTmux {
    fn new(real: &std::path::Path) -> Self {
        let dir = PathBuf::from(format!(
            "/tmp/ccnm-f4-{}-{}",
            std::process::id(),
            &session::new_id()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wrapper = dir.join("tmux");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nTMUX_TMPDIR='{}' exec '{}' \"$@\"\n",
                dir.display(),
                real.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            dir: ccnm_testdir::TestDir::adopt(dir),
            wrapper,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new(&self.wrapper)
            .args(["-L", "ccnm"])
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for OwnTmux {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

/// The same stop with nothing faked: a real tmux server, a real `ps`, and a
/// process that does what the ssh transport did on the P62 machines -- it
/// carries the session's payload on its command line and takes a second and
/// a half to go away after the terminal is killed.
///
/// The scripted tests above prove the decision; this one proves the things
/// they assume -- that `ps` really shows the process that way, that tmux
/// really reports the session gone while it lingers, and that the loop ends
/// on a real exit rather than on a scripted line.
#[test]
fn exact_stop_against_a_real_terminal_waits_out_a_transport_that_lingers() {
    let Some(real) = ccnm_core::tmux::locate_from_env() else {
        // CI installs tmux on both platforms for exactly this.
        assert!(
            std::env::var_os("CI").is_none(),
            "tmux is not installed on this runner"
        );
        eprintln!("skipped: no tmux on this machine");
        return;
    };
    let own = OwnTmux::new(&real);
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000035";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    backdate(&dir.meta(), 3 * 60 + 20);
    let payload = AgentProvider::Claude.transport_payload(&dir).unwrap();

    // The pane runs a script rather than the lingering command itself: the
    // tmux server keeps the command line of the client that started it, and
    // a payload there would look like a transport that never exits.
    //
    // It says when it is ready, after it has stopped listening to SIGHUP.
    // Showing up in `ps` is not that: the command line is there from the
    // exec, before Python has run a line, and a stop that lands in between
    // kills it with the very SIGHUP it is meant to outlive. On a loaded
    // macOS runner that took the release gate down once (525 ms).
    let linger = own.dir.join("linger.py");
    let ready = own.dir.join("ready");
    std::fs::write(
        &linger,
        format!(
            "import signal, sys, time\n\
             signal.signal(signal.SIGHUP, signal.SIG_IGN)\n\
             open('{}', 'w').close()\n\
             try:\n    sys.stdin.buffer.read()\nexcept OSError:\n    pass\n\
             time.sleep(1.5)\n",
            ready.display()
        ),
    )
    .unwrap();
    let pane = own.dir.join("pane.sh");
    std::fs::write(
        &pane,
        format!(
            "exec python3 '{}' internal mcp-serve --payload {payload}\n",
            linger.display()
        ),
    )
    .unwrap();
    let started = own.run(&[
        "new-session",
        "-d",
        "-s",
        "ccnm-demo",
        "-e",
        &format!("CCNM_SESSION={id}"),
        "/bin/sh",
        pane.to_str().unwrap(),
    ]);
    assert!(started.status.success(), "{started:?}");
    let listed = || {
        let out = std::process::Command::new("/bin/ps")
            .args(["-Awwo", "command="])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&payload)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !(ready.exists() && listed()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in transport never got ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let mut tools = f.tools(&ccnm_core::process::SystemRunner);
    tools.tmux = Some(own.wrapper.clone());
    let began = std::time::Instant::now();
    let report = work::stop(&exact_interactive_stop(id), &tools).unwrap();
    let took = began.elapsed();
    assert!(report.killed);
    assert!(
        took >= std::time::Duration::from_secs(1),
        "returned after {took:?}, before the transport could have exited"
    );
    assert!(!listed(), "confirmed while the transport was still there");
    let outcome = session::read_outcome(&dir).unwrap().unwrap();
    assert!(outcome.stopped);
    assert!(
        (200_000..260_000).contains(&outcome.duration_ms),
        "{} ms",
        outcome.duration_ms
    );
}

/// The wait is bounded, and what it cannot confirm it still does not claim:
/// a transport that outlives the whole grace is `CCNM_E_NOT_READY`, the
/// session stays `stopping`, and no outcome is written. A `ps` that cannot be
/// read is not waited on at all -- more looking would not make it readable.
#[test]
fn exact_stop_gives_up_after_the_grace_and_never_confirms_what_it_cannot_see() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000031";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(1, ""));
    // More answers than five seconds of looking every 100 ms can use up.
    for _ in 0..200 {
        runner.push(transport_line(&dir));
    }
    let began = std::time::Instant::now();
    let error = work::stop(&exact_interactive_stop(id), &f.tools(&runner)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert!(error.message().contains("still alive"), "{error}");
    assert!(
        began.elapsed() >= std::time::Duration::from_secs(4),
        "gave up after {:?}, without waiting",
        began.elapsed()
    );
    assert!(dir.stopping().exists(), "state remains stopping");
    assert!(session::read_outcome(&dir).unwrap().is_none());

    let id = "00000000-0000-4000-8000-000000000032";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(1, ""));
    runner.push(Output::exited(1, "")); // ps itself failed
    let began = std::time::Instant::now();
    let error = work::stop(&exact_interactive_stop(id), &f.tools(&runner)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert!(error.message().contains("unknown"), "{error}");
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    assert!(session::read_outcome(&dir).unwrap().is_none());
}

/// F4, second half. A session somebody stopped is not one that failed to
/// start. On the P62 machines a Claude session that had run for seven minutes
/// was listed by `ccnm log` as `failed to start`, `<1m`: the stop borrowed
/// the could-not-start outcome, whose duration is always 0.
#[test]
fn a_stopped_session_is_logged_as_stopped_with_how_long_it_ran() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000033";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    backdate(&dir.meta(), 7 * 60 + 20);
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(1, ""));
    runner.push(Output::exited(0, ""));
    work::stop(&exact_interactive_stop(id), &f.tools(&runner)).unwrap();
    let line = log_line(&f);
    assert!(line.contains("stopped"), "{line}");
    assert!(line.contains("7m"), "{line}");
    assert!(!line.contains("failed to start"), "{line}");
}

/// A stop whose first call could not confirm the transport leaves the
/// `stopping` marker behind; the next call finds no terminal. That is still
/// the same stop, ended when it was asked for -- not a session with "no
/// terminal", and not one that ran until whenever somebody asked again.
#[test]
fn a_stop_confirmed_on_the_second_call_is_still_a_stop_at_the_time_it_was_asked() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000034";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    backdate(&dir.meta(), 7 * 60 + 20);
    std::fs::write(dir.stopping(), b"requested\n").unwrap();
    backdate(&dir.stopping(), 2 * 60);
    let runner = FakeRunner::new();
    runner.push(Output::exited(1, "")); // tmux has-session: nothing there
    let report = work::stop(&exact_interactive_stop(id), &f.tools(&runner)).unwrap();
    assert!(!report.killed);
    let line = log_line(&f);
    assert!(line.contains("stopped"), "{line}");
    assert!(line.contains("5m"), "{line}");
    assert!(!line.contains("failed to start"), "{line}");
}

/// Idempotency does not mean stop can never fail. A terminal that *is*
/// running but carries no verifiable ccnm identity stays an error, because
/// that check is what keeps ccnm from killing someone else's session.
#[test]
fn a_running_terminal_without_a_verifiable_identity_is_still_refused() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "")); // tmux has-session: something is there
    runner.push(Output::exited(0, "\n")); // but it shows no CCNM_SESSION
    let error = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: None,
            assigned: false,
        },
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert!(
        runner
            .calls()
            .iter()
            .all(|cmd| !cmd.display().contains("kill-session"))
    );
}

#[test]
fn active_session_with_another_identity_is_never_reused_or_replaced() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000014";
    f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
    let request = StartRequest {
        protocol: 3,
        provider: AgentProvider::Claude,
        agent: Some(reference("codex-main")),
        workspace: "demo".into(),
        root: "/runtime/project".into(),
        runtime_node: "runtime".into(),
        provider_config_dir: None,
        permission_mode: Default::default(),
        prompt: None,
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    let error = work::start(&request, &f.tools(&runner)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert_eq!(
        runner.calls().len(),
        2,
        "no controller, Runtime preflight or kill after identity mismatch"
    );
}

#[test]
fn completed_print_stop_still_checks_recorded_groups_without_signalling() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000019";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::write_supervisor_pid(&dir, 4242).unwrap();
    session::write_agent_pid(&dir, 4343).unwrap();
    session::record_terminal_failure(&dir, "already finished").unwrap();
    let original = session::read_outcome(&dir).unwrap();
    let request = StopRequest {
        protocol: 3,
        workspace: "demo".into(),
        agent: Some(reference("claude-main")),
        session: Some(id.into()),
        assigned: false,
    };
    for observations in [
        vec![Output::exited(0, "9000 4242\n")],
        vec![Output::exited(0, "1 1\n"), Output::exited(0, "9000 4343\n")],
        vec![Output::exited(1, "")],
        vec![Output::exited(0, "1 1\n"), Output::exited(0, "invalid\n")],
    ] {
        let runner = FakeRunner::new();
        for output in observations {
            runner.push(output);
        }
        assert_eq!(
            work::stop(&request, &f.tools(&runner)).unwrap_err().code(),
            ErrorCode::NotReady
        );
        assert!(
            runner
                .calls()
                .iter()
                .all(|cmd| !cmd.display().contains("/bin/kill"))
        );
        assert_eq!(session::read_outcome(&dir).unwrap(), original);
    }
    let ended = FakeRunner::new();
    ended.push(Output::exited(0, "1 1\n"));
    ended.push(Output::exited(0, "1 1\n"));
    assert!(!work::stop(&request, &f.tools(&ended)).unwrap().killed);
}

#[test]
fn completed_print_stop_distinguishes_missing_and_invalid_pid_records() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000020";
    let dir = f.record(
        id,
        "demo",
        Some(f.identity("codex-main", AgentProvider::Codex)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::record_terminal_failure(&dir, "failed before spawn").unwrap();
    let request = StopRequest {
        protocol: 3,
        workspace: "demo".into(),
        agent: Some(reference("codex-main")),
        session: Some(id.into()),
        assigned: false,
    };
    let runner = FakeRunner::new();
    assert!(!work::stop(&request, &f.tools(&runner)).unwrap().killed);
    assert!(
        runner.calls().is_empty(),
        "pre-spawn failure has no recorded processes"
    );
    for raw in ["", "0", "1", "2147483648", "bad", "42\n43"] {
        std::fs::write(dir.agent_pid(), raw).unwrap();
        assert_eq!(
            work::stop(&request, &f.tools(&runner)).unwrap_err().code(),
            ErrorCode::NotReady
        );
        assert!(
            runner.calls().is_empty(),
            "invalid PID must not trigger signalling or ps"
        );
    }
    std::fs::remove_file(dir.agent_pid()).unwrap();
    std::fs::create_dir(dir.agent_pid()).unwrap();
    assert_eq!(
        work::stop(&request, &f.tools(&runner)).unwrap_err().code(),
        ErrorCode::NotReady
    );
}

#[test]
fn exact_print_stop_verifies_the_supervisor_before_signalling_its_process_group() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000015";
    let identity = f.identity("claude-main", AgentProvider::Claude);
    let dir = f.record(
        id,
        "demo",
        Some(identity.clone()),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::write_supervisor_pid(&dir, 4242).unwrap();
    session::write_agent_pid(&dir, 4343).unwrap();
    let request = StopRequest {
        protocol: 3,
        workspace: "demo".into(),
        agent: Some(reference("claude-main")),
        session: Some(id.into()),
        assigned: false,
    };

    let mut supervise =
        session::SuperviseRequest::new(dir.path().to_path_buf(), "/agent/claude".into());
    supervise.protocol = 3;
    supervise.identity = Some(identity);
    let wire = ccnm_core::protocol::payload::encode(&supervise).unwrap();
    let wrong = FakeRunner::new();
    let mut other = supervise.clone();
    other.session_dir = "/other/session".into();
    wrong.push(Output::exited(
        0,
        format!(
            "4242 /ccnm internal supervise --payload {}\n",
            ccnm_core::protocol::payload::encode(&other).unwrap()
        ),
    ));
    assert_eq!(
        work::stop(&request, &f.tools(&wrong)).unwrap_err().code(),
        ErrorCode::Policy
    );
    assert_eq!(wrong.calls().len(), 1);

    let runner = FakeRunner::new();
    runner.push(Output::exited(
        0,
        format!("4242 /ccnm internal supervise --payload {wire}\n"),
    ));
    runner.push(Output::exited(0, "4343 4242\n"));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, "1 1\n"));
    runner.push(Output::exited(0, ""));
    runner.push(Output::exited(0, "1 1\n"));
    let report = work::stop(&request, &f.tools(&runner)).unwrap();
    assert!(report.killed);
    // The `--` is load-bearing, not punctuation: without it Linux `kill`
    // reads `-4343` as a signal, signals nothing and exits 0 (see
    // process::kill_group), so a stop would report success and leave the
    // group running. Matched exactly so dropping it fails here.
    assert!(
        runner.calls()[2]
            .display()
            .contains("/bin/kill -TERM -- -4343")
    );
    assert!(
        runner.calls()[4]
            .display()
            .contains("/bin/kill -TERM -- -4242")
    );
    // The supervisor left no outcome here, so the stop writes it -- as a
    // stop, the same way the interactive path does since P64.
    let outcome = session::read_outcome(&dir).unwrap().unwrap();
    assert!(outcome.stopped);
    assert_eq!(outcome.error, None);

    let done = FakeRunner::new();
    done.push(Output::exited(0, "1 1\n"));
    done.push(Output::exited(0, "1 1\n"));
    let report = work::stop(&request, &f.tools(&done)).unwrap();
    assert!(!report.killed, "terminal stop is idempotent");
    assert_eq!(done.calls().len(), 2, "still verify both recorded groups");

    let unknown_id = "00000000-0000-4000-8000-000000000016";
    let unknown = f.record(
        unknown_id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::write_supervisor_pid(&unknown, 5252).unwrap();
    session::write_agent_pid(&unknown, 5353).unwrap();
    let uncertain = FakeRunner::new();
    uncertain.push(Output::exited(2, ""));
    let error = work::stop(
        &StopRequest {
            session: Some(unknown_id.into()),
            ..request
        },
        &f.tools(&uncertain),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotReady);
    assert_eq!(uncertain.calls().len(), 1, "unknown state must not signal");
}

#[test]
fn print_stop_checks_the_whole_group_and_rejects_reparented_agent() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000017";
    let identity = f.identity("claude-main", AgentProvider::Claude);
    let dir = f.record(
        id,
        "demo",
        Some(identity.clone()),
        Mode::Print {
            prompt: "fixture".into(),
        },
    );
    session::write_supervisor_pid(&dir, 4242).unwrap();
    session::write_agent_pid(&dir, 4343).unwrap();
    let req = StopRequest {
        protocol: 3,
        workspace: "demo".into(),
        agent: Some(reference("claude-main")),
        session: Some(id.into()),
        assigned: false,
    };
    let mut supervise =
        session::SuperviseRequest::new(dir.path().to_path_buf(), "/agent/claude".into());
    supervise.protocol = 3;
    supervise.identity = Some(identity);
    let wire = ccnm_core::protocol::payload::encode(&supervise).unwrap();
    let supervisor = format!("4242 /ccnm internal supervise --payload {wire}\n");

    let wrong_parent = FakeRunner::new();
    wrong_parent.push(Output::exited(0, supervisor.as_str()));
    wrong_parent.push(Output::exited(0, "4343 9999\n"));
    assert_eq!(
        work::stop(&req, &f.tools(&wrong_parent))
            .unwrap_err()
            .code(),
        ErrorCode::Policy
    );
    assert!(
        !wrong_parent
            .calls()
            .iter()
            .any(|cmd| cmd.program == "/bin/kill")
    );

    let residual = FakeRunner::new();
    residual.push(Output::exited(0, supervisor.as_str()));
    residual.push(Output::exited(0, "4343 4242\n"));
    residual.push(Output::exited(0, ""));
    // A member that outlives the whole grace a stop waits after SIGTERM
    // (P63, F17: five seconds, a look every 100 ms): still NotReady.
    for _ in 0..60 {
        residual.push(Output::exited(0, "8888 4343\n"));
    }
    assert_eq!(
        work::stop(&req, &f.tools(&residual)).unwrap_err().code(),
        ErrorCode::NotReady
    );
    assert!(residual.calls()[3].display().contains("-axo pid=,pgid="));
    assert!(session::read_outcome(&dir).unwrap().is_none());
    assert!(dir.stopping().exists());

    let leader_gone = FakeRunner::new();
    leader_gone.push(Output::exited(0, supervisor.as_str()));
    leader_gone.push(Output::exited(1, ""));
    leader_gone.push(Output::exited(0, "8888 4343\n"));
    assert_eq!(
        work::stop(&req, &f.tools(&leader_gone)).unwrap_err().code(),
        ErrorCode::NotReady
    );
    assert!(
        !leader_gone
            .calls()
            .iter()
            .any(|cmd| cmd.program == "/bin/kill")
    );
    assert!(session::read_outcome(&dir).unwrap().is_none());

    let supervisor_child = FakeRunner::new();
    supervisor_child.push(Output::exited(0, supervisor.as_str()));
    supervisor_child.push(Output::exited(0, "4343 4242\n"));
    supervisor_child.push(Output::exited(0, ""));
    supervisor_child.push(Output::exited(0, "1 1\n"));
    supervisor_child.push(Output::exited(0, ""));
    for _ in 0..60 {
        supervisor_child.push(Output::exited(0, "9999 4242\n"));
    }
    assert_eq!(
        work::stop(&req, &f.tools(&supervisor_child))
            .unwrap_err()
            .code(),
        ErrorCode::NotReady
    );
    assert!(session::read_outcome(&dir).unwrap().is_none());

    for observation in [
        Output::exited(2, ""),
        Output::exited(0, " \n"),
        Output::exited(0, "malformed\n"),
    ] {
        let unknown = FakeRunner::new();
        unknown.push(Output::exited(0, supervisor.as_str()));
        unknown.push(Output::exited(0, "4343 4242\n"));
        unknown.push(Output::exited(0, ""));
        unknown.push(observation);
        assert_eq!(
            work::stop(&req, &f.tools(&unknown)).unwrap_err().code(),
            ErrorCode::NotReady
        );
        assert!(session::read_outcome(&dir).unwrap().is_none());
        assert_eq!(
            unknown.calls().len(),
            4,
            "unknown group state must not stop supervisor"
        );
    }
}

fn output_request(
    id: &str,
    workspace: &str,
    stream: session::view::Stream,
    offset: u64,
    limit: u64,
) -> OutputRequest {
    OutputRequest {
        protocol: ccnm_core::instance::OUTPUT_PROTOCOL,
        workspace: workspace.into(),
        agent: reference("claude-main"),
        session: id.into(),
        stream,
        offset,
        limit,
    }
}

fn read_view(
    f: &Fixture,
    runner: &FakeRunner,
    id: &str,
    stream: session::view::Stream,
    slice: u64,
) -> (Vec<u8>, ccnm_core::protocol::run::OutputReport) {
    use base64::Engine as _;
    let mut whole = Vec::new();
    loop {
        let report = work::output(
            &output_request(id, "demo", stream, whole.len() as u64, slice),
            &f.tools(runner),
        )
        .unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .decode(&report.data)
            .unwrap();
        let done = data.is_empty() || whole.len() as u64 + data.len() as u64 == report.view_bytes;
        whole.extend(data);
        if done {
            return (whole, report);
        }
    }
}

/// P59: the Agent answers from a view built once, after the session ended:
/// the private profile path is gone wherever it sat -- including across the
/// edge of a slice -- later writes to the file are not in it, and the two
/// streams stay separate.
#[test]
fn agent_output_is_a_redacted_frozen_view_of_a_finished_session() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    let id = session::new_id();
    let dir = f.record(
        &id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "go".into(),
        },
    );
    let private = f.home.join(".claude").display().to_string();
    let mut stdout = format!("start {private} 中文\n").into_bytes();
    // Put a second copy of the path across the 1000-byte slice edge used below.
    stdout.resize(1000 - private.len() / 2, b'x');
    stdout.extend_from_slice(private.as_bytes());
    stdout.extend_from_slice(" end 😀\n".as_bytes());
    std::fs::write(dir.stdout(), &stdout).unwrap();
    std::fs::write(dir.stderr(), format!("warn {private}\n")).unwrap();

    let early = work::output(
        &output_request(&id, "demo", session::view::Stream::Stdout, 0, 1000),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(early.code(), ErrorCode::NotReady, "{early}");

    session::record_terminal_failure(&dir, "done").unwrap();
    let (view, report) = read_view(&f, &runner, &id, session::view::Stream::Stdout, 1000);
    let text = String::from_utf8(view.clone()).unwrap();
    assert!(!text.contains(&private), "{text}");
    assert_eq!(text.matches("<agent-private-config>").count(), 2, "{text}");
    assert!(text.ends_with(" end 😀\n"));
    assert_eq!(report.view_bytes, view.len() as u64);
    assert_eq!(report.source_bytes, stdout.len() as u64);
    assert!(!report.source_truncated);
    assert_eq!(report.agent_identity.instance, "claude-main");

    // Frozen: a late write does not change what is served.
    std::fs::write(dir.stdout(), [stdout.as_slice(), b"late\n"].concat()).unwrap();
    let (again, again_report) = read_view(&f, &runner, &id, session::view::Stream::Stdout, 1 << 20);
    assert_eq!(again, view);
    assert_eq!(again_report.generation, report.generation);

    let (err, _) = read_view(&f, &runner, &id, session::view::Stream::Stderr, 1 << 20);
    assert_eq!(err, b"warn <agent-private-config>\n");
    assert!(runner.calls().is_empty(), "reading output runs nothing");
}

/// F23 (P62 resume, 2026-10-04): a session ran and ended, and its raw stdout
/// went away before anyone asked for the view. The supervisor creates both
/// streams before it starts the Agent, so a missing one was lost, not left
/// empty. Read as 0 bytes, it reached the caller as "empty and complete",
/// with no `unavailable_reason`. The Agent refuses instead, and the Operator
/// falls back to its old tail and says so.
#[test]
fn agent_output_refuses_a_lost_stream_of_a_session_that_ran() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    let id = session::new_id();
    let dir = f.record(
        &id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "go".into(),
        },
    );
    std::fs::write(dir.stderr(), "warn\n").unwrap();
    // Ended by itself: an outcome without "could not start".
    std::fs::write(
        dir.exit(),
        r#"{"exit_code":0,"timed_out":false,"duration_ms":5}"#,
    )
    .unwrap();

    let err = work::output(
        &output_request(&id, "demo", session::view::Stream::Stdout, 0, 100),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert!(err.message().contains("stdout"), "{err}");
    assert!(err.message().contains("is gone"), "{err}");
    assert!(
        !dir.path().join("stdout.view.json").exists(),
        "nothing is left that a later read could take for the whole output"
    );

    // The stream that is there is served, and once its view is built the
    // raw file is no longer needed.
    let (stderr, _) = read_view(&f, &runner, &id, session::view::Stream::Stderr, 100);
    assert_eq!(stderr, b"warn\n");
    std::fs::remove_file(dir.stderr()).unwrap();
    let (again, _) = read_view(&f, &runner, &id, session::view::Stream::Stderr, 100);
    assert_eq!(again, b"warn\n");
}

/// The other side of F23: a session that never started never had raw
/// output. Its view is empty and complete, because that is the truth.
#[test]
fn agent_output_of_a_session_that_never_started_is_empty_and_complete() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    let id = session::new_id();
    let dir = f.record(
        &id,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "go".into(),
        },
    );
    session::record_terminal_failure(&dir, session::STOPPED_BEFORE_START.trim_end()).unwrap();
    for stream in [session::view::Stream::Stdout, session::view::Stream::Stderr] {
        let (data, report) = read_view(&f, &runner, &id, stream, 100);
        assert!(data.is_empty(), "{stream:?}");
        assert_eq!(
            (
                report.view_bytes,
                report.source_bytes,
                report.source_truncated
            ),
            (0, 0, false),
            "{stream:?}"
        );
    }
}

#[test]
fn agent_output_never_crosses_workspace_instance_or_mode() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    let print = session::new_id();
    let dir = f.record(
        &print,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Print {
            prompt: "go".into(),
        },
    );
    session::record_terminal_failure(&dir, "done").unwrap();
    let wrong_workspace = work::output(
        &output_request(&print, "other", session::view::Stream::Stdout, 0, 10),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(
        wrong_workspace.code(),
        ErrorCode::InvalidArgs,
        "{wrong_workspace}"
    );
    let mut wrong_instance = output_request(&print, "demo", session::view::Stream::Stdout, 0, 10);
    wrong_instance.agent = reference("codex-main");
    assert!(work::output(&wrong_instance, &f.tools(&runner)).is_err());

    let interactive = session::new_id();
    let idir = f.record(
        &interactive,
        "demo",
        Some(f.identity("claude-main", AgentProvider::Claude)),
        Mode::Interactive { prompt: None },
    );
    session::record_terminal_failure(&idir, "done").unwrap();
    let err = work::output(
        &output_request(&interactive, "demo", session::view::Stream::Stdout, 0, 10),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidArgs, "{err}");

    let missing = work::output(
        &output_request(
            &session::new_id(),
            "demo",
            session::view::Stream::Stdout,
            0,
            10,
        ),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(missing.code(), ErrorCode::NotReady);
    let bad = work::output(
        &output_request("../x", "demo", session::view::Stream::Stdout, 0, 10),
        &f.tools(&runner),
    )
    .unwrap_err();
    assert_eq!(bad.code(), ErrorCode::InvalidArgs);
    assert!(
        !dir.path().join("stdout.view").exists() || dir.path().join("stdout.view.json").exists()
    );
}

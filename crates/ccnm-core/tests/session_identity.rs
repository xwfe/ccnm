use ccnm_core::instance::{AgentIdentity, AgentLocal, AgentProfiles, InstanceRef};
use ccnm_core::process::{FakeRunner, Output};
use ccnm_core::protocol::run::{
    ResultRequest, SessionState, StartRequest, StatusRequest, StopRequest,
};
use ccnm_core::provider::{AgentBinaries, AgentProvider};
use ccnm_core::session::{self, Dir, Mode, RuntimeLink, Spec};
use ccnm_core::{Config, ErrorCode, paths, work};
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
    fn tools<'a>(&self, runner: &'a FakeRunner) -> work::Tools<'a> {
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
        },
        &f.tools(&runner),
    )
    .unwrap();
    assert!(report.killed);
    assert_eq!(report.session.as_deref(), Some(id));
    assert!(
        session::read_outcome(&dir)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("stopped by ccnm")
    );
    let repeated = FakeRunner::new();
    let report = work::stop(
        &StopRequest {
            protocol: 3,
            workspace: "demo".into(),
            agent: Some(reference("claude-main")),
            session: Some(id.into()),
        },
        &f.tools(&repeated),
    )
    .unwrap();
    assert!(!report.killed, "terminal stop is idempotent");
    assert!(repeated.calls().is_empty());
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
    assert!(
        runner.calls()[2]
            .display()
            .contains("/bin/kill -TERM -4343")
    );
    assert!(
        runner.calls()[4]
            .display()
            .contains("/bin/kill -TERM -4242")
    );
    assert!(
        session::read_outcome(&dir)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("print process group ended")
    );

    let done = FakeRunner::new();
    let report = work::stop(&request, &f.tools(&done)).unwrap();
    assert!(!report.killed, "terminal stop is idempotent");
    assert!(done.calls().is_empty());

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
    residual.push(Output::exited(0, "8888 4343\n"));
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
    supervisor_child.push(Output::exited(0, "9999 4242\n"));
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

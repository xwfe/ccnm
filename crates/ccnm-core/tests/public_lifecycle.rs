use ccnm_core::instance::AgentIdentity;
use ccnm_core::process::{FakeRunner, Output};
use ccnm_core::protocol::payload;
use ccnm_core::protocol::run::{
    AttachRequest, ResultReport, ResultRequest, StatusReport, StatusRequest, StopReport,
    StopRequest,
};
use ccnm_core::provider::AgentProvider;
use ccnm_core::{Config, ErrorCode, launcher};
use std::path::{Path, PathBuf};

struct Fixture {
    root: PathBuf,
    config: Config,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p3-public-{}", ccnm_core::session::new_id()));
        std::fs::create_dir(&root).unwrap();
        let text = include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
            .replace("/runtime/project", root.to_str().unwrap());
        Self {
            root,
            config: Config::parse(&text).unwrap(),
        }
    }
    fn resolved(&self) -> ccnm_core::config::Resolved<'_> {
        self.config.workspace("demo").unwrap()
    }
    fn env<'a>(&self, runner: &'a FakeRunner) -> launcher::Env<'a> {
        launcher::Env {
            runner,
            control_dir: PathBuf::from("/tmp/ccnm-p3-public-control"),
            current_exe: "/ccnm".into(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn identity(instance: &str, provider: AgentProvider) -> AgentIdentity {
    AgentIdentity {
        node: "worker".into(),
        instance: instance.into(),
        provider,
        profile_ref: "default".into(),
    }
}

fn request<T: serde::de::DeserializeOwned + payload::Protocol>(call: &ccnm_core::Cmd) -> T {
    payload::decode(call.args.last().unwrap().to_str().unwrap()).unwrap()
}

#[test]
fn default_and_override_flow_through_attach_status_result_and_stop_with_exact_id() {
    let f = Fixture::new();
    let id = "00000000-0000-4000-8000-000000000030";
    let codex = identity("codex-main", AgentProvider::Codex);
    let runner = FakeRunner::new();
    runner.push(Output::exited(
        0,
        serde_json::to_vec(&StatusReport {
            protocol: 3,
            agent_identity: Some(codex.clone()),
            tmux: Ok("3.7c".into()),
            sessions: vec![],
            records: vec![],
        })
        .unwrap(),
    ));
    runner.push(Output::exited(
        0,
        serde_json::to_vec(&ResultReport {
            protocol: 3,
            provider: AgentProvider::Codex,
            agent_identity: Some(codex.clone()),
            session: id.into(),
            session_dir: "/synthetic/agent/session".into(),
            mode: "print".into(),
            started: 1,
            outcome: None,
            result: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        })
        .unwrap(),
    ));
    runner.push(Output::exited(
        0,
        serde_json::to_vec(&StopReport {
            protocol: 3,
            tmux_session: "ccnm-demo".into(),
            session: Some(id.into()),
            agent_identity: Some(codex),
            killed: true,
        })
        .unwrap(),
    ));
    let env = f.env(&runner);
    launcher::status_selected(&f.resolved(), &env, false, Some("codex-main"), Some(id)).unwrap();
    launcher::result_selected(&f.resolved(), &env, Some(id), Some("codex-main")).unwrap();
    launcher::stop_selected(&f.resolved(), &env, Some("codex-main"), Some(id)).unwrap();
    let attach =
        launcher::attach_cmd_selected(&f.resolved(), &env, Some("codex-main"), Some(id)).unwrap();

    let calls = runner.calls();
    let status: StatusRequest = request(&calls[0]);
    let result: ResultRequest = request(&calls[1]);
    let stop: StopRequest = request(&calls[2]);
    let attach: AttachRequest = request(&attach);
    for selected in [status.agent, result.agent, stop.agent, attach.agent] {
        assert_eq!(selected.unwrap().instance, "codex-main");
    }
    assert_eq!(status.session.as_deref(), Some(id));
    assert_eq!(result.session.as_deref(), Some(id));
    assert_eq!(stop.session.as_deref(), Some(id));
    assert_eq!(attach.session.as_deref(), Some(id));
    assert!(
        [
            status.protocol,
            result.protocol,
            stop.protocol,
            attach.protocol
        ]
        .iter()
        .all(|v| *v == 3)
    );
    assert!(!calls.iter().any(|call| call.display().contains("profile") || call.display().contains("CODEX_HOME")));

    let default = identity("claude-main", AgentProvider::Claude);
    runner.push(Output::exited(
        0,
        serde_json::to_vec(&StatusReport {
            protocol: 3,
            agent_identity: Some(default),
            tmux: Ok("3.7c".into()),
            sessions: vec![],
            records: vec![],
        })
        .unwrap(),
    ));
    launcher::status_selected(&f.resolved(), &env, false, None, None).unwrap();
    let default: StatusRequest = request(runner.calls().last().unwrap());
    assert_eq!(default.agent.unwrap().instance, "claude-main");

    runner.push(Output::exited(
        0,
        serde_json::to_vec(&StatusReport {
            protocol: 1,
            agent_identity: None,
            tmux: Ok("3.7c".into()),
            sessions: vec![],
            records: vec![],
        })
        .unwrap(),
    ));
    launcher::status_selected(&f.resolved(), &env, true, None, None).unwrap();
    let all: StatusRequest = request(runner.calls().last().unwrap());
    assert_eq!(all.protocol, 1);
    assert!(all.workspace.is_none());
    assert!(all.agent.is_none());
}

#[test]
fn runtime_rejects_a_response_for_another_instance() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    runner.push(Output::exited(
        0,
        serde_json::to_vec(&StopReport {
            protocol: 3,
            tmux_session: "ccnm-demo".into(),
            session: None,
            agent_identity: Some(identity("other", AgentProvider::Codex)),
            killed: false,
        })
        .unwrap(),
    ));
    let error = launcher::stop_selected(&f.resolved(), &f.env(&runner), Some("codex-main"), None)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::Version);
}

#[test]
fn agent_override_is_an_id_not_a_node_path_provider_or_argv_channel() {
    let f = Fixture::new();
    let runner = FakeRunner::new();
    for bad in [
        "../codex",
        "worker/codex",
        "codex --danger",
        "codex:provider",
        "",
    ] {
        assert!(
            launcher::attach_cmd_selected(&f.resolved(), &f.env(&runner), Some(bad), None).is_err()
        );
    }
    assert!(runner.calls().is_empty());
    assert!(Path::new(&f.root).is_absolute());
}

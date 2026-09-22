//! Public P3 selection and fail-closed identity boundaries, with fake peers only.
use ccnm_core::protocol::payload;
use ccnm_core::provider::AgentProvider;
use ccnm_core::session::{self, Dir, Mode, Spec, SuperviseRequest};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p2-cli-{}", session::new_id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.env_clear()
            .env("HOME", &self.0)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("XDG_STATE_HOME", self.short_state());
        cmd
    }
    fn short_state(&self) -> PathBuf {
        let suffix = self.0.file_name().unwrap().to_string_lossy();
        PathBuf::from("/tmp").join(format!("cp3-{}", &suffix[suffix.len() - 8..]))
    }
    fn script(&self, name: &str) -> PathBuf {
        let bin = self.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join(name);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 99\n",
                self.0.join("unexpected-child").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        script
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir_all(self.short_state());
    }
}

/// From the Agent Node, a diagnostic asks the Runtime a question. It does
/// not hand it a command to run.
///
/// Both of these used to be delegated whole: the public `ccnm doctor` and
/// `ccnm mcp probe` were sent over ssh, ran as the Runtime Executor, and
/// then dialled *back* to this machine to probe it. That is an inbound-only
/// account opening an outbound connection, for a diagnostic. The transcript
/// below is the boundary (P7.4 Batch D2): every line that crosses is
/// `internal`, and no public verb appears at all.
#[test]
fn agent_side_diagnostics_ask_the_runtime_instead_of_delegating_to_it() {
    let f = Fixture::new();
    let config = f.0.join("agent.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/agent.toml"),
    )
    .unwrap();
    let log = f.0.join("delegated-args");
    let ssh = f.0.join("bin/ssh");
    std::fs::create_dir_all(ssh.parent().unwrap()).unwrap();
    // Answers nothing: what is asserted here is what was asked, and a
    // diagnostic that cannot reach the Runtime must still not fall back to
    // sending it a command.
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

    f.command()
        .arg("--config")
        .arg(&config)
        .args(["doctor", "demo"])
        .output()
        .unwrap();
    f.command()
        .arg("--config")
        .arg(&config)
        .args([
            "mcp",
            "probe",
            "demo",
            "--agent",
            "codex-main",
            "--calls",
            "1",
        ])
        .output()
        .unwrap();

    let args = std::fs::read_to_string(log).unwrap();
    assert!(
        args.contains("runtime-alias\n~/.local/bin/ccnm\ninternal\nruntime-resolve\n--payload\n"),
        "the question that crosses: {args}"
    );
    for public in [
        "\ndoctor\n",
        "\nmcp\n",
        "\nprobe\n",
        "\nrun\n",
        "\nstatus\n",
    ] {
        assert!(
            !args.contains(public),
            "a public command was sent to the Runtime ({public:?}): {args}"
        );
    }
}

/// `--local` on the Agent Node has nothing to serve: the project is not
/// here. It fails, rather than quietly measuring the remote transport and
/// reporting it under a name that means the opposite.
#[test]
fn a_local_probe_on_the_agent_node_is_refused_not_reinterpreted() {
    let f = Fixture::new();
    let config = f.0.join("agent.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/agent.toml"),
    )
    .unwrap();
    let out = f
        .command()
        .arg("--config")
        .arg(&config)
        .args(["mcp", "probe", "demo", "--local", "--calls", "1"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::WrongWorkspace.exit_code()),
        "{err}"
    );
    assert!(err.contains("on another one"), "{err}");
}

#[test]
fn public_default_and_explicit_instance_selection_use_v3_without_private_paths() {
    use ccnm_core::controller::Context;
    use ccnm_core::instance::AgentIdentity;
    use ccnm_core::protocol::run::{RunReport, StartReport};
    use ccnm_core::provider::{AgentResult, RunResult};
    use ccnm_core::session::Outcome;
    let f = Fixture::new();
    let root = f.0.join("project");
    std::fs::create_dir(&root).unwrap();
    let config = f.0.join("config.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
            .replace("/runtime/project", root.to_str().unwrap()),
    )
    .unwrap();
    let context = Context {
        hello: ccnm_core::protocol::hello::HelloReport {
            protocol: 1,
            ccnm_version: ccnm_core::VERSION.into(),
            user: "fixture".into(),
            platform: "macos/aarch64".into(),
            exe: None,
            root: None,
        },
        pid: 42,
        manager: Ok("Aqua".into()),
    };
    let claude = AgentIdentity {
        node: "worker".into(),
        instance: "claude-main".into(),
        provider: AgentProvider::Claude,
        profile_ref: "default".into(),
    };
    let run = RunReport {
        protocol: 3,
        provider: AgentProvider::Claude,
        agent_identity: Some(claude),
        session: "00000000-0000-4000-8000-000000000001".into(),
        session_dir: "/synthetic/agent/session".into(),
        controller: context.clone(),
        pid: 43,
        outcome: Outcome {
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 1,
            error: None,
        },
        result: Some(AgentResult::Claude(RunResult {
            is_error: false,
            subtype: None,
            result: Some("fixture answer".into()),
            session_id: Some("provider-thread-fixture".into()),
            num_turns: 1,
            duration_ms: 1,
            duration_api_ms: 1,
            total_cost_usd: 0.0,
            usage: Default::default(),
            permission_denials: vec![],
        })),
        stdout_tail: String::new(),
        stderr_tail: String::new(),
    };
    let codex = AgentIdentity {
        node: "worker".into(),
        instance: "codex-main".into(),
        provider: AgentProvider::Codex,
        profile_ref: "default".into(),
    };
    let start = StartReport {
        protocol: 3,
        provider: AgentProvider::Codex,
        agent_identity: Some(codex),
        session: Some("00000000-0000-4000-8000-000000000002".into()),
        session_dir: Some("/synthetic/agent/session-two".into()),
        tmux_session: "ccnm-demo".into(),
        server_pid: 44,
        already_running: false,
        replaced: None,
        controller: Some(context),
        context: None,
    };
    let run_json = f.0.join("run.json");
    let start_json = f.0.join("start.json");
    std::fs::write(&run_json, serde_json::to_vec(&run).unwrap()).unwrap();
    std::fs::write(&start_json, serde_json::to_vec(&start).unwrap()).unwrap();
    let log = f.0.join("ssh-args");
    let bin = f.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\ncase \"$*\" in *agent-run*) cat '{}' ;; *agent-start*) cat '{}' ;; *) exit 98 ;; esac\n", log.display(), run_json.display(), start_json.display()),
    ).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

    let printed = f
        .command()
        .arg("--config")
        .arg(&config)
        .args(["run", "demo", "--print", "fixture prompt"])
        .output()
        .unwrap();
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    assert!(String::from_utf8_lossy(&printed.stdout).contains("fixture answer"));
    let started = f
        .command()
        .arg("--config")
        .arg(&config)
        .args(["run", "demo", "--agent", "codex-main", "--detached"])
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );

    let args = std::fs::read_to_string(log).unwrap();
    let wires: Vec<_> = args
        .lines()
        .filter(|line| {
            line.len() > 100
                && line
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .collect();
    let run: ccnm_core::protocol::run::RunRequest = payload::decode(wires[0]).unwrap();
    let start: ccnm_core::protocol::run::StartRequest = payload::decode(wires[1]).unwrap();
    assert_eq!(run.agent.unwrap().instance, "claude-main");
    assert_eq!(start.agent.unwrap().instance, "codex-main");
    assert_eq!(run.protocol, 3);
    assert_eq!(start.protocol, 3);
    for private in [
        "profile",
        "CODEX_HOME",
        "claude_config_dir",
        "/synthetic/agent/session",
    ] {
        assert!(
            !args.contains(private),
            "{private} leaked into Runtime-to-Agent request"
        );
    }
}

#[test]
fn identity_mismatched_supervisor_transport_and_controller_requests_fail_before_children() {
    let f = Fixture::new();
    let fake_agent = f.script("agent");
    f.script("ssh");
    let dir = Dir::at(f.0.join("record"));
    std::fs::create_dir(dir.path()).unwrap();
    let spec = Spec {
        runtime_node: Some("runtime".into()),
        protocol: 3,
        agent_identity: Some(ccnm_core::instance::AgentIdentity {
            node: "worker".into(),
            instance: "main".into(),
            provider: AgentProvider::Claude,
            profile_ref: "default".into(),
        }),
        provider: AgentProvider::Claude,
        id: "fixture".into(),
        workspace: "demo".into(),
        root: "/runtime/project".into(),
        runtime: Some(session::RuntimeLink {
            alias: "never-connect.invalid".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Print {
            prompt: "not sent".into(),
        },
        timeout_secs: 10,
        cwd: f.0.clone(),
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    std::fs::write(dir.meta(), serde_json::to_vec(&spec).unwrap()).unwrap();
    let supervise = SuperviseRequest::new(dir.path().to_path_buf(), fake_agent.clone());
    let transport = session::transport::Request {
        identity: None,
        protocol: 2,
        session_dir: dir.path().to_path_buf(),
    };
    for (command, wire, exit) in [
        ("supervise", payload::encode(&supervise).unwrap(), 1),
        (
            "agent-transport",
            payload::encode(&transport).unwrap(),
            ccnm_core::ErrorCode::InvalidArgs.exit_code(),
        ),
    ] {
        let out = f
            .command()
            .args(["internal", command, "--payload", &wire])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(exit));
        assert!(!f.0.join("unexpected-child").exists());
    }
    assert!(
        session::read_outcome(&dir)
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .contains("private details withheld")
    );
    let runner = ccnm_core::process::FakeRunner::new();
    let tools = ccnm_core::controller::Tools {
        local: None,
        config: ccnm_core::Config::default(),
        config_path: None,
        runner: &runner,
        agents: ccnm_core::provider::AgentBinaries::with_claude(Some(fake_agent)),
        tmux: None,
        exe: env!("CARGO_BIN_EXE_ccnm").into(),
    };
    let request = ccnm_core::controller::Request::new(ccnm_core::controller::RequestBody::Start {
        identity: None,
        session_dir: dir.path().to_path_buf(),
        provider: AgentProvider::Claude,
    });
    let reply = ccnm_core::controller::answer(&request, &tools);
    assert!(matches!(
        reply.body,
        ccnm_core::controller::ReplyBody::Error(_)
    ));
    assert!(runner.calls().is_empty());
    assert!(!f.0.join("unexpected-child").exists());
}

#[test]
fn legacy_mcp_payloads_cannot_bypass_an_instance_selected_workspace() {
    let f = Fixture::new();
    let root = f.0.join("project");
    std::fs::create_dir(&root).unwrap();
    let config = f.0.join("config.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
            .replace("/runtime/project", root.to_str().unwrap()),
    )
    .unwrap();
    for provider in AgentProvider::ALL {
        let request = ccnm_core::protocol::mcp::ServePayload::new("demo", root.clone(), "fixture")
            .with_provider(provider);
        let out = f
            .command()
            .env("CCNM_CONFIG", &config)
            .args([
                "internal",
                "mcp-serve",
                "--payload",
                &payload::encode(&request).unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(ccnm_core::ErrorCode::Policy.exit_code()),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn actual_agent_work_path_resolves_profile_controller_binding_and_runtime_mcp() {
    use ccnm_core::instance::{AgentLocal, AgentProfiles};
    use ccnm_core::process::{FakeRunner, Output};
    use ccnm_core::protocol::run::{RunReport, RunRequest};
    use ccnm_core::provider::AgentBinaries;
    let f = Fixture::new();
    let agent_home = f.0.join("agent-home");
    let claude_home = agent_home.join(".claude");
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::set_permissions(&claude_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = f.0.join("runtime-project");
    let runtime_home = f.0.join("runtime-home");
    let runtime_state = f.0.join("runtime-state");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&runtime_home).unwrap();
    let runtime_config = f.0.join("runtime.toml");
    // Add the opt-in inside the existing table instead of creating a duplicate.
    std::fs::write(
        &runtime_config,
        include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
            .replace("/runtime/project", project.to_str().unwrap())
            .replace("root =", "allow_unconfined_exec=true\nroot ="),
    )
    .unwrap();
    let agent_config = f.0.join("agent.toml");
    std::fs::write(
        &agent_config,
        include_str!("../../../tests/fixtures/agent-instance/agent.toml"),
    )
    .unwrap();

    let fake_ssh = f.0.join("bin/ssh");
    std::fs::create_dir_all(fake_ssh.parent().unwrap()).unwrap();
    std::fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\nfor last do :; done\ncase \"$*\" in *'internal hello'*) sub=hello ;; *'internal mcp-serve'*) sub=mcp-serve ;; *) exit 97 ;; esac\nexec /usr/bin/env -i PATH=/usr/bin:/bin HOME='{home}' XDG_STATE_HOME='{state}' CCNM_CONFIG='{config}' '{ccnm}' internal \"$sub\" --payload \"$last\"\n",
            home = runtime_home.display(),
            state = runtime_state.display(),
            config = runtime_config.display(),
            ccnm = env!("CARGO_BIN_EXE_ccnm"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

    let sessions = f.short_state().join("ccnm/sessions");
    let supervisor = f.0.join("supervisor");
    std::fs::write(
        &supervisor,
        format!(
            "#!/bin/sh\nfor s in '{sessions}'/*/; do printf '%s' '{{\"is_error\":false,\"result\":\"bound instance completed\",\"session_id\":\"provider-thread-separate\",\"num_turns\":1}}' > \"$s/stdout\"; : > \"$s/stderr\"; printf '%s' '{{\"exit_code\":0,\"timed_out\":false,\"duration_ms\":9}}' > \"$s/exit.tmp\"; mv \"$s/exit.tmp\" \"$s/exit\"; done\n",
            sessions = sessions.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&supervisor, std::fs::Permissions::from_mode(0o700)).unwrap();

    let socket = f.short_state().join("ccnm/controller.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = ccnm_core::controller::Listener::bind(&socket).unwrap();
    let config = ccnm_core::Config::load(&agent_config).unwrap();
    let local_home = agent_home.clone();
    let config_path = agent_config.clone();
    let served = std::thread::spawn(move || {
        let runner = FakeRunner::new();
        runner.push(Output::exited(0, "Aqua\n"));
        for _ in 0..2 {
            runner.push(Output::exited(0, "2.1.260 (Claude Code)\n"));
            runner.push(Output::exited(
                0,
                r#"{"loggedIn":true,"authMethod":"fixture"}"#,
            ));
        }
        let tools = ccnm_core::controller::Tools {
            runner: &runner,
            agents: AgentBinaries::with_claude(Some("/synthetic/agent/claude".into())),
            config,
            local: Some(AgentLocal::new(AgentProfiles::default(), local_home, None).unwrap()),
            config_path: Some(config_path),
            tmux: None,
            exe: supervisor,
        };
        for _ in 0..3 {
            listener.serve_one(&tools).unwrap();
        }
    });

    let request = RunRequest {
        protocol: 3,
        provider: AgentProvider::Claude,
        agent: Some(ccnm_core::instance::InstanceRef {
            node: "worker".into(),
            instance: "claude-main".into(),
        }),
        workspace: "demo".into(),
        root: project,
        runtime_node: "runtime".into(),
        provider_config_dir: None,
        permission_mode: Default::default(),
        prompt: "fixture".into(),
        timeout_secs: 30,
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    let out = f
        .command()
        .env("HOME", &agent_home)
        .env("CCNM_CONFIG", &agent_config)
        .args([
            "internal",
            "agent-run",
            "--payload",
            &payload::encode(&request).unwrap(),
        ])
        .output()
        .unwrap();
    served.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: RunReport = payload::decode_json(&out.stdout).unwrap();
    assert_eq!(report.protocol, 3);
    assert_eq!(
        report.agent_identity.as_ref().unwrap().instance,
        "claude-main"
    );
    assert_eq!(
        report.result.as_ref().unwrap().text(),
        Some("bound instance completed")
    );
    assert_ne!(report.session, "provider-thread-separate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains(claude_home.to_str().unwrap()));
    let spec = ccnm_core::session::load(&Dir::at(report.session_dir)).unwrap();
    assert_eq!(spec.agent_identity, report.agent_identity);
    assert_eq!(spec.runtime_node.as_deref(), Some("runtime"));
    let guards = runtime_state.join("ccnm/write-guards");
    let states: Vec<_> = std::fs::read_dir(guards)
        .unwrap()
        .flatten()
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    assert_eq!(
        states,
        vec!["released\n"],
        "preflight released Runtime authority cleanly"
    );
}

/// A Runtime whose write guard another session holds, reached from the
/// Agent Node through a fake `ssh` that runs the real binary under the
/// Runtime's own config and state. The holder is a real `mcp-serve` that
/// has answered `initialize`, so the guard is taken by the time a test
/// dials in.
struct BusyRuntime {
    holder: std::process::Child,
    holder_stdin: Option<std::process::ChildStdin>,
    agent_home: PathBuf,
    agent_config: PathBuf,
    project: PathBuf,
}

impl BusyRuntime {
    fn start(f: &Fixture) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;
        let agent_home = f.0.join("agent-home");
        let claude_home = agent_home.join(".claude");
        std::fs::create_dir_all(&claude_home).unwrap();
        std::fs::set_permissions(&claude_home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = f.0.join("runtime-project");
        let runtime_home = f.0.join("runtime-home");
        let runtime_state = f.0.join("runtime-state");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&runtime_home).unwrap();
        let runtime_config = f.0.join("runtime.toml");
        std::fs::write(
            &runtime_config,
            include_str!("../../../tests/fixtures/agent-instance/runtime.toml")
                .replace("/runtime/project", project.to_str().unwrap())
                .replace("root =", "allow_unconfined_exec=true\nroot ="),
        )
        .unwrap();
        let agent_config = f.0.join("agent.toml");
        std::fs::write(
            &agent_config,
            include_str!("../../../tests/fixtures/agent-instance/agent.toml"),
        )
        .unwrap();

        let fake_ssh = f.0.join("bin/ssh");
        std::fs::create_dir_all(fake_ssh.parent().unwrap()).unwrap();
        std::fs::write(
            &fake_ssh,
            format!(
                "#!/bin/sh\nfor last do :; done\ncase \"$*\" in *'internal hello'*) sub=hello ;; *'internal runtime-resolve'*) sub=runtime-resolve ;; *'internal mcp-serve'*) sub=mcp-serve ;; *) exit 97 ;; esac\nexec /usr/bin/env -i PATH=/usr/bin:/bin HOME='{home}' XDG_STATE_HOME='{state}' CCNM_CONFIG='{config}' '{ccnm}' internal \"$sub\" --payload \"$last\"\n",
                home = runtime_home.display(),
                state = runtime_state.display(),
                config = runtime_config.display(),
                ccnm = env!("CARGO_BIN_EXE_ccnm"),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

        let open = ccnm_core::runtime::OpenPayload::new(
            "demo",
            ccnm_core::instance::AgentIdentity {
                node: "worker".into(),
                instance: "claude-main".into(),
                provider: AgentProvider::Claude,
                profile_ref: "default".into(),
            },
            "holder",
        );
        let mut holder = Command::new(env!("CARGO_BIN_EXE_ccnm"))
            .args([
                "internal",
                "mcp-serve",
                "--payload",
                &payload::encode(&open).unwrap(),
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &runtime_home)
            .env("XDG_STATE_HOME", &runtime_state)
            .env("CCNM_CONFIG", &runtime_config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = holder.stdin.take().unwrap();
        let mut stdout = BufReader::new(holder.stdout.take().unwrap());
        writeln!(stdin, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"busy-holder","version":"0"}}})).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(
            line.contains("\"id\":1"),
            "holder did not initialize: {line:?}"
        );
        Self {
            holder,
            holder_stdin: Some(stdin),
            agent_home,
            agent_config,
            project,
        }
    }
}

impl Drop for BusyRuntime {
    fn drop(&mut self) {
        // EOF is the holder's clean shutdown; it releases the guard.
        drop(self.holder_stdin.take());
        let _ = self.holder.wait();
    }
}

/// What a busy Runtime has to look like from the Agent Node: the Runtime's
/// own refusal, with its own code, and the transport it came over still
/// named. Reported as "cannot reach the Runtime" (P24), a caller keyed on
/// the code goes to debug a network that is fine.
fn assert_refused_as_busy(out: &std::process::Output) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::Policy.exit_code()),
        "{err}"
    );
    assert!(err.starts_with("CCNM_E_POLICY:\n"), "{err}");
    assert!(err.contains("MCP initialize failed over"), "{err}");
    assert!(err.contains("internal mcp-serve"), "{err}");
    assert!(err.contains("write guard is busy"), "{err}");
}

#[test]
fn a_busy_write_guard_fails_the_run_preflight_as_policy_not_unreachable() {
    use ccnm_core::instance::{AgentLocal, AgentProfiles};
    use ccnm_core::process::{FakeRunner, Output};
    use ccnm_core::protocol::run::RunRequest;
    use ccnm_core::provider::AgentBinaries;
    let f = Fixture::new();
    let runtime = BusyRuntime::start(&f);

    // The controller is asked twice -- its context, then whether the Agent
    // is logged in -- and the MCP preflight comes next. A third request
    // would mean the refusal did not stop the session from being created.
    let socket = f.short_state().join("ccnm/controller.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = ccnm_core::controller::Listener::bind(&socket).unwrap();
    let config = ccnm_core::Config::load(&runtime.agent_config).unwrap();
    let local_home = runtime.agent_home.clone();
    let config_path = runtime.agent_config.clone();
    let served = std::thread::spawn(move || {
        let runner = FakeRunner::new();
        runner.push(Output::exited(0, "Aqua\n"));
        runner.push(Output::exited(0, "2.1.260 (Claude Code)\n"));
        runner.push(Output::exited(
            0,
            r#"{"loggedIn":true,"authMethod":"fixture"}"#,
        ));
        let tools = ccnm_core::controller::Tools {
            runner: &runner,
            agents: AgentBinaries::with_claude(Some("/synthetic/agent/claude".into())),
            config,
            local: Some(AgentLocal::new(AgentProfiles::default(), local_home, None).unwrap()),
            config_path: Some(config_path),
            tmux: None,
            exe: PathBuf::from("/synthetic/never-started"),
        };
        for _ in 0..2 {
            listener.serve_one(&tools).unwrap();
        }
    });

    let request = RunRequest {
        protocol: 3,
        provider: AgentProvider::Claude,
        agent: Some(ccnm_core::instance::InstanceRef {
            node: "worker".into(),
            instance: "claude-main".into(),
        }),
        workspace: "demo".into(),
        root: runtime.project.clone(),
        runtime_node: "runtime".into(),
        provider_config_dir: None,
        permission_mode: Default::default(),
        prompt: "fixture".into(),
        timeout_secs: 30,
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    let out = f
        .command()
        .env("HOME", &runtime.agent_home)
        .env("CCNM_CONFIG", &runtime.agent_config)
        .args([
            "internal",
            "agent-run",
            "--payload",
            &payload::encode(&request).unwrap(),
        ])
        .output()
        .unwrap();
    // Asserted before the join: if the run got further than the preflight
    // the thread is still serving, and a join would hang instead of fail.
    assert_refused_as_busy(&out);
    assert!(out.stdout.is_empty());
    served.join().unwrap();
    assert!(
        !f.short_state().join("ccnm/sessions").exists(),
        "a refused preflight must not create a session"
    );
}

/// The same handshake is doctor's "Remote MCP handshake" row and
/// `ccnm mcp probe` on the Agent Node.
#[test]
fn a_busy_write_guard_fails_the_agent_side_mcp_probe_as_policy() {
    let f = Fixture::new();
    let runtime = BusyRuntime::start(&f);
    let out = f
        .command()
        .env("HOME", &runtime.agent_home)
        .arg("--config")
        .arg(&runtime.agent_config)
        .args(["mcp", "probe", "demo", "--calls", "1"])
        .output()
        .unwrap();
    assert_refused_as_busy(&out);
}

#[test]
fn supervisor_re_resolves_named_profile_without_storing_it_in_public_identity() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let xdg = f.0.join("xdg");
    let profile = f.0.join("private-profile");
    let cwd = f.0.join("cwd");
    for dir in [&home, &xdg.join("ccnm"), &profile, &cwd] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = f.0.join("agent.toml");
    std::fs::write(
        &config,
        include_str!("../../../tests/fixtures/agent-instance/agent.toml").replacen(
            "profile_ref = \"default\"",
            "profile_ref = \"extra\"",
            1,
        ),
    )
    .unwrap();
    let profiles = xdg.join("ccnm/profiles.toml");
    std::fs::write(
        &profiles,
        format!(
            "[profiles.extra]\nprovider='claude'\ndirectory='{}'\n",
            profile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(0o600)).unwrap();
    let marker = f.0.join("agent-ran");
    let agent = f.0.join("agent");
    std::fs::write(
        &agent,
        format!("#!/bin/sh\n[ \"$CLAUDE_CONFIG_DIR\" = '{}' ] || exit 88\ntouch '{}'\nprintf '%s' '{{\"is_error\":false,\"result\":\"profile resolved\",\"session_id\":\"provider-thread\",\"num_turns\":1}}'\n", profile.display(), marker.display()),
    ).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let dir = Dir::at(f.0.join("session"));
    std::fs::create_dir(dir.path()).unwrap();
    let identity = ccnm_core::instance::AgentIdentity {
        node: "worker".into(),
        instance: "claude-main".into(),
        provider: AgentProvider::Claude,
        profile_ref: "extra".into(),
    };
    let spec = Spec {
        protocol: 3,
        runtime_node: Some("runtime".into()),
        provider: AgentProvider::Claude,
        agent_identity: Some(identity.clone()),
        id: "00000000-0000-4000-8000-000000000020".into(),
        workspace: "demo".into(),
        root: "/runtime/project".into(),
        runtime: Some(session::RuntimeLink {
            alias: "runtime".into(),
            ccnm_bin: "/runtime/ccnm".into(),
        }),
        provider_config_dir: None,
        permission_mode: Default::default(),
        mode: Mode::Print {
            prompt: "fixture".into(),
        },
        timeout_secs: 10,
        cwd,
        codex_exec_server: false,
        agent_tools: Default::default(),
    };
    let stored = serde_json::to_string(&spec).unwrap();
    assert!(!stored.contains(profile.to_str().unwrap()));
    std::fs::write(dir.meta(), stored).unwrap();
    std::fs::write(dir.settings(), "{}").unwrap();
    std::fs::write(dir.mcp_config(), "{}").unwrap();
    let req = SuperviseRequest {
        protocol: 3,
        provider: AgentProvider::Claude,
        identity: Some(identity),
        session_dir: dir.path().to_path_buf(),
        agent_bin: agent,
    };
    let out = f
        .command()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("CCNM_CONFIG", &config)
        .args([
            "internal",
            "supervise",
            "--payload",
            &payload::encode(&req).unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(marker.is_file());
    assert!(session::read_outcome(&dir).unwrap().unwrap().ok());
    assert_eq!(
        AgentProvider::Claude
            .parse_result(&std::fs::read(dir.stdout()).unwrap())
            .unwrap()
            .text(),
        Some("profile resolved")
    );
}

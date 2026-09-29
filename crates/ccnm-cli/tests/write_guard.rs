//! Real Runtime processes and file locks; no network or Agent CLI.
use ccnm_core::mcp::write_guard::{Observation, Observed, Process, Reason};
use ccnm_core::protocol::mcp::ServePayload;
use ccnm_core::protocol::payload;
use ccnm_core::runtime::{GUARD_PROTOCOL, GuardReport, GuardRequest};
use ccnm_core::session;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    config: PathBuf,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-p3-guard-{name}-{}", session::new_id()));
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir(&home).unwrap();
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "this='runtime'\n[nodes.runtime]\n[nodes.agent]\nssh='agent'\n[workspaces.demo]\nagent_node='agent'\nroot='{}'\nallow_unconfined_exec=true\n",
                project.display()
            ),
        )
        .unwrap();
        Self {
            root,
            project,
            config,
        }
    }

    fn command(&self, session: &str) -> Command {
        let wire =
            payload::encode(&ServePayload::new("demo", self.project.clone(), session)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        command
            .args(["internal", "mcp-serve", "--payload", &wire])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("CCNM_CONFIG", &self.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn start(&self, session: &str) -> Server {
        let mut child = self.command(session).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"guard-test","version":"0"}}})).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "server exited before initialize");
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], 1);
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        stdin.flush().unwrap();
        Server {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    /// `ccnm internal runtime-guard`, run the way the Agent's ssh runs it:
    /// as this Runtime account, with its state directory and config.
    fn ask(&self, node: &str, protocol: u32) -> std::process::Output {
        self.ask_about("demo", node, protocol)
    }

    fn ask_about(&self, workspace: &str, node: &str, protocol: u32) -> std::process::Output {
        let wire = payload::encode(&GuardRequest {
            protocol,
            workspace: workspace.into(),
            node: node.into(),
        })
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_ccnm"))
            .args(["internal", "runtime-guard", "--payload", &wire])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("CCNM_CONFIG", &self.config)
            .output()
            .unwrap()
    }

    fn observe(&self) -> Observation {
        let out = self.ask("agent", GUARD_PROTOCOL);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: GuardReport = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report.workspace, "demo");
        report.observation
    }

    /// The one marker file, as bytes, or `None` before any exists.
    fn marker(&self) -> Option<Vec<u8>> {
        let dir = self.root.join("state/ccnm/write-guards");
        let entry = std::fs::read_dir(dir).ok()?.flatten().next()?;
        Some(std::fs::read(entry.path()).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    #[allow(dead_code)]
    stdout: BufReader<ChildStdout>,
}
impl Server {
    fn graceful(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "{status}");
    }
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

fn refused(fixture: &Fixture, session: &str, contains: &str) {
    let output = fixture.command(session).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(ccnm_core::ErrorCode::Policy.exit_code())
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(contains), "{stderr}");
}

fn alive(pid: i32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn live_owner_is_busy_and_clean_shutdown_allows_reentry() {
    let fixture = Fixture::new("live");
    let first = fixture.start("one");
    refused(&fixture, "two", "write guard is busy");
    first.graceful();
    fixture.start("one").graceful();
}

#[test]
fn abrupt_server_exit_never_transfers_authority_by_timeout() {
    let fixture = Fixture::new("crash");
    fixture.start("one").kill();
    std::thread::sleep(Duration::from_millis(20));
    refused(&fixture, "two", "not transferred automatically");
}

#[test]
fn residual_exec_child_keeps_the_workspace_unknown_until_manual_recovery() {
    let fixture = Fixture::new("child");
    let script = fixture.root.join("owned-child.sh");
    let pid_file = fixture.root.join("owned-child.pid");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut server = fixture.start("one");
    writeln!(server.stdin.as_mut().unwrap(), "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec_command","arguments":{"cmd":[script.to_str().unwrap()]}}})).unwrap();
    server.stdin.as_mut().unwrap().flush().unwrap();
    // Wait for a pid, not for the file: the shell's `>` creates it before
    // `printf` writes into it, so on a busy machine `is_file` is true while
    // the content is still empty and the parse below blows up on "".
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the child never wrote its pid");
        std::thread::sleep(Duration::from_millis(20));
    };
    server.kill();
    assert!(alive(pid));
    // 看一眼不是清理：命令还活着，观察说 unknown，标记一个字节都没动（AU-05）。
    let before = fixture.marker();
    let seen = fixture.observe();
    assert_eq!(
        (seen.state, seen.reason),
        (Observed::Unknown, Reason::LeftHeld)
    );
    assert_eq!(fixture.marker(), before);
    refused(&fixture, "two", "not transferred automatically");
    // What the Runtime operator does by hand. `--` matters: without it
    // Linux `kill` reads `-1234` as a signal, signals nothing and exits 0
    // (see process::kill_group), so this step would quietly do nothing and
    // the assertion below is what noticed.
    let _ = Command::new("/bin/kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output();
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid));
    refused(&fixture, "two", "not transferred automatically");
    // 命令收掉了也还是 unknown：这里看不见它，也就证明不了它没了。
    assert_eq!(fixture.observe().state, Observed::Unknown);
    let guards = fixture.root.join("state/ccnm/write-guards");
    for entry in std::fs::read_dir(guards).unwrap().flatten() {
        std::fs::remove_file(entry.path()).unwrap();
    }
    fixture.start("two").graceful();
}

// ---- P60：Runtime 自己回答写锁现在的样子 ----

/// 看得见谁占着，而且看完什么都没变：没建目录、没改标记、持锁者照旧（AU-02）。
#[test]
fn the_runtime_observes_its_own_guard_without_touching_it() {
    let fixture = Fixture::new("observe");
    let seen = fixture.observe();
    assert_eq!(
        (seen.state, seen.reason),
        (Observed::Free, Reason::NeverTaken)
    );
    assert!(
        !fixture.root.join("state/ccnm/write-guards").exists(),
        "an observation creates nothing"
    );

    let server = fixture.start("one");
    let before = fixture.marker();
    let seen = fixture.observe();
    assert_eq!(
        (seen.state, seen.reason),
        (Observed::Held, Reason::LiveHolder)
    );
    let owner = seen.owner.expect("the marker names the holder");
    assert_eq!(owner.session, "one");
    assert_eq!(owner.workspace.as_deref(), Some("demo"));
    assert_eq!(owner.pid, Some(server.child.id()));
    assert_eq!(owner.process, Some(Process::Ccnm));
    assert_eq!(fixture.marker(), before);
    // 持锁者没受影响：第二个 writer 照样被拒，第一个照样正常收尾、放锁。
    refused(&fixture, "two", "write guard is busy");
    server.graceful();
    assert_eq!(
        (fixture.observe().state, fixture.observe().reason),
        (Observed::Free, Reason::Released)
    );
}

/// 强杀之后：没人持锁，标记还说占着——unknown，不是 free；pid 只是个事实（AU-03）。
#[test]
fn a_killed_server_leaves_the_guard_unknown_and_its_pid_only_described() {
    let fixture = Fixture::new("observe-kill");
    let server = fixture.start("one");
    let pid = server.child.id();
    server.kill();
    let seen = fixture.observe();
    assert_eq!(
        (seen.state, seen.reason),
        (Observed::Unknown, Reason::LeftHeld)
    );
    let owner = seen.owner.unwrap();
    assert_eq!(owner.pid, Some(pid));
    // 刚被收走的号几乎不会马上被复用；万一被复用，也只能是"别的程序"。
    assert!(
        matches!(owner.process, Some(Process::Gone | Process::Other)),
        "{owner:?}"
    );
    refused(&fixture, "two", "not transferred automatically");
}

/// 一次 free 不是授权：看完之后另一个 writer 先进去了，后来的照样被原来那把锁拒（AU-04）。
#[test]
fn a_free_observation_reserves_nothing() {
    let fixture = Fixture::new("observe-race");
    assert_eq!(fixture.observe().state, Observed::Free);
    let first = fixture.start("one");
    refused(&fixture, "two", "write guard is busy");
    assert_eq!(fixture.observe().state, Observed::Held);
    first.graceful();
}

/// 只回答绑定在这台上的那个 Agent Node、只说自己认识的协议号（AU-06）。
#[test]
fn the_runtime_answers_only_its_bound_agent_and_its_own_protocol() {
    let fixture = Fixture::new("observe-refuse");
    for (node, protocol, code) in [
        ("elsewhere", GUARD_PROTOCOL, ccnm_core::ErrorCode::Config),
        ("agent", 4, ccnm_core::ErrorCode::Version),
    ] {
        let out = fixture.ask(node, protocol);
        assert_eq!(
            out.status.code(),
            Some(code.exit_code()),
            "{node} {protocol}"
        );
        assert!(out.stdout.is_empty());
    }
    let out = fixture.ask_about("nosuch", "agent", GUARD_PROTOCOL);
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::Config.exit_code())
    );
    assert!(!fixture.root.join("state/ccnm/write-guards").exists());
}

/// 同一个仓库的两个 worktree 共用一把锁：问另一个 workspace，答的是同一个资源、同一个持有者（AU-06）。
#[test]
fn worktrees_of_one_repository_are_observed_as_one_resource() {
    let fixture = Fixture::new("observe-worktree");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&fixture.project)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "base",
    ]);
    let other = fixture.root.join("other-tree");
    git(&["worktree", "add", "-q", other.to_str().unwrap()]);
    std::fs::write(
        &fixture.config,
        format!(
            "this='runtime'\n[nodes.runtime]\n[nodes.agent]\nssh='agent'\n[workspaces.demo]\nagent_node='agent'\nroot='{}'\nallow_unconfined_exec=true\n[workspaces.wt]\nagent_node='agent'\nroot='{}'\n",
            fixture.project.display(),
            other.display()
        ),
    )
    .unwrap();
    let server = fixture.start("one");
    let out = fixture.ask_about("wt", "agent", GUARD_PROTOCOL);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = serde_json::from_slice::<GuardReport>(&out.stdout)
        .unwrap()
        .observation;
    assert_eq!(seen.state, Observed::Held);
    assert_eq!(
        seen.resource.kind,
        ccnm_core::mcp::write_guard::ResourceKind::GitCommonDir
    );
    assert_eq!(seen.resource, fixture.observe().resource);
    assert_eq!(seen.owner.unwrap().workspace.as_deref(), Some("demo"));
    server.graceful();
}

/// Agent 用它自己那条到 Runtime 的链路转问，回答来自 Runtime 执行账号的 state；
/// Agent 只补一条它自己才知道的事实。两端都是真实二进制，中间的 ssh 是假的。
#[test]
fn the_agent_relays_the_runtime_answer_over_its_own_link() {
    use ccnm_core::protocol::run::{AgentGuardReport, AgentGuardRequest, OwnerOnAgent};
    let fixture = Fixture::new("relay");
    std::fs::write(
        &fixture.config,
        format!(
            "this='runtime'\n[nodes.runtime]\n[nodes.worker]\nssh='agent-alias'\n[workspaces.demo]\nroot='{}'\nagent={{node='worker',instance='claude-main'}}\n",
            fixture.project.display()
        ),
    )
    .unwrap();
    // Runtime 这边有人持锁，标记里的会话 id 是一个这台 Agent 没有记录的。
    let owner = session::new_id();
    let held = ccnm_core::mcp::write_guard::WriteGuard::acquire(
        &fixture.root.join("state/ccnm"),
        &fixture.project,
        "demo",
        &owner,
        None,
        &ccnm_core::process::SystemRunner,
    )
    .unwrap();

    let bin = fixture.root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let args = fixture.root.join("ssh-args");
    let fake_ssh = |body: &str| {
        let script = bin.join("ssh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n{body}",
                args.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    // 冒充 Agent 到 Runtime 的 ssh：落到"Runtime 执行账号"上，跑真实的 ccnm。
    fake_ssh(&format!(
        "while [ \"$#\" -gt 0 ] && [ \"$1\" != internal ]; do shift; done\nexec env -i PATH=/usr/bin:/bin HOME='{}' XDG_STATE_HOME='{}' CCNM_CONFIG='{}' '{}' \"$@\"\n",
        fixture.root.join("home").display(),
        fixture.root.join("state").display(),
        fixture.config.display(),
        env!("CARGO_BIN_EXE_ccnm"),
    ));
    // ControlPath 有 104 字节上限，Agent 的 state 放在短路径下。
    let agent_state = PathBuf::from("/tmp").join(format!("cg-{}", &session::new_id()[..8]));
    let _agent_state = ccnm_testdir::TestDir::adopt(agent_state.clone());
    let agent = |instance: &str| {
        let wire = payload::encode(&AgentGuardRequest {
            protocol: GUARD_PROTOCOL,
            workspace: "demo".into(),
            agent: ccnm_core::instance::InstanceRef {
                node: "worker".into(),
                instance: instance.into(),
            },
            runtime_node: "runtime".into(),
        })
        .unwrap();
        let _ = std::fs::remove_file(&args);
        Command::new(env!("CARGO_BIN_EXE_ccnm"))
            .arg("--config")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/agent-instance/agent.toml"
            ))
            .args(["internal", "agent-guard", "--payload", &wire])
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", fixture.root.join("agent-home"))
            .env("XDG_STATE_HOME", &agent_state)
            .output()
            .unwrap()
    };

    let out = agent("claude-main");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: AgentGuardReport = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report.agent_identity.node, "worker");
    assert_eq!(report.agent_identity.instance, "claude-main");
    let seen = &report.runtime.observation;
    assert_eq!(
        (seen.state, seen.reason),
        (Observed::Held, Reason::LiveHolder)
    );
    assert_eq!(seen.owner.as_ref().unwrap().session, owner);
    assert_eq!(report.owner_on_agent, Some(OwnerOnAgent::NotHere));
    let asked = std::fs::read_to_string(&args).unwrap();
    assert!(
        asked.contains("runtime-alias\n~/.local/bin/ccnm\ninternal\nruntime-guard\n--payload\n"),
        "{asked}"
    );

    // 这台 Agent 上没有的实例：在拨号之前就拒。
    let out = agent("nosuch");
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::Config.exit_code())
    );
    assert!(!args.exists(), "refused before dialling the Runtime");

    // Runtime 连不上：具名的 runtime unreachable，不是一个空的"没人占"。
    fake_ssh("echo 'ssh: connect to host runtime port 22: Connection refused' >&2\nexit 255\n");
    let out = agent("claude-main");
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::RuntimeUnreachable.exit_code()),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    drop(held);
}

use super::*;
use crate::process::{FakeRunner, Output, SystemRunner};
use ccnm_testdir::TestDir;
use std::path::PathBuf;

fn temp(test: &str) -> TestDir {
    let dir = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ccnm-cleanup-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(control(&dir));
    fs::create_dir_all(&dir).unwrap();
    let sockets = control(&dir);
    TestDir::adopt(dir).also(sockets)
}

/// ControlPath is capped at 103 bytes and macOS `temp_dir()` alone is ~60.
fn control(dir: &Path) -> PathBuf {
    PathBuf::from("/tmp/ccnm-cl").join(dir.file_name().unwrap())
}

fn id(n: u8) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// Runtime output for a session, the way `exec_command` leaves it.
fn output(state: &Path, id: &str, bytes: usize) -> PathBuf {
    let run = crate::paths::session_dir(state, id).join("output/r-0000000000000001");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("stdout"), vec![b'x'; bytes]).unwrap();
    run.parent().unwrap().to_path_buf()
}

fn facts() -> Facts {
    Facts {
        uid: None,
        guard_session: None,
        guard_unclear: false,
        live: Some(Vec::new()),
    }
}

fn why(item: &Option<Item>) -> Option<Why> {
    item.as_ref().expect("listed").why
}

// ---- Runtime ----

/// 只列这个账号自己的、会话已结束且没人在用的输出；其余都留，并说为什么。
#[test]
fn the_runtime_keeps_everything_it_cannot_prove_is_done_with() {
    let dir = temp("runtime-items");
    let state = dir.join("state");
    let (a, b) = (id(1), id(2));
    output(&state, &a, 10);
    output(&state, &b, 10);

    let ok = runtime_item(&state, &a, true, &facts());
    assert_eq!(ok.as_ref().unwrap().plan, Plan::Remove);
    assert_eq!(ok.as_ref().unwrap().bytes, 10);
    assert_eq!(
        why(&runtime_item(&state, &a, false, &facts())),
        Some(Why::NotEnded)
    );

    let guard = Facts {
        guard_session: Some(a.clone()),
        ..facts()
    };
    assert_eq!(
        why(&runtime_item(&state, &a, true, &guard)),
        Some(Why::Guard)
    );
    assert_eq!(why(&runtime_item(&state, &b, true, &guard)), None);
    let unclear = Facts {
        guard_unclear: true,
        ..facts()
    };
    assert_eq!(
        why(&runtime_item(&state, &b, true, &unclear)),
        Some(Why::Guard)
    );

    let served = Facts {
        live: Some(vec![a.clone()]),
        ..facts()
    };
    assert_eq!(
        why(&runtime_item(&state, &a, true, &served)),
        Some(Why::InUse)
    );
    let blind = Facts {
        live: None,
        ..facts()
    };
    assert_eq!(
        why(&runtime_item(&state, &a, true, &blind)),
        Some(Why::Unchecked)
    );
    let other = Facts {
        uid: Some(u32::MAX),
        ..facts()
    };
    assert_eq!(
        why(&runtime_item(&state, &a, true, &other)),
        Some(Why::ForeignOwner)
    );

    // A command still writing: its `running` file is there and locked.
    let run = crate::paths::session_dir(&state, &b).join("output/r-0000000000000002");
    fs::create_dir_all(&run).unwrap();
    let running = fs::File::create(run.join("running")).unwrap();
    running.lock().unwrap();
    assert_eq!(
        why(&runtime_item(&state, &b, true, &facts())),
        Some(Why::InUse)
    );
    running.unlock().unwrap();

    // Nothing kept: nothing listed. A link where a directory should be:
    // listed, kept, never followed.
    assert!(runtime_item(&state, &id(3), true, &facts()).is_none());
    let elsewhere = dir.join("elsewhere");
    fs::create_dir_all(elsewhere.join("output")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, crate::paths::session_dir(&state, &id(4))).unwrap();
    assert_eq!(
        why(&runtime_item(&state, &id(4), true, &facts())),
        Some(Why::NotADirectory)
    );
}

fn runtime_config(root: &Path) -> crate::config::Config {
    crate::config::Config::parse(&format!(
        "this='runtime'\n[nodes.runtime]\n[nodes.worker]\nssh='agent-alias'\n[workspaces.demo]\nroot='{}'\nagent={{node='worker',instance='claude-main'}}\n",
        root.display()
    ))
    .unwrap()
}

fn runtime_request(sessions: &[&str], apply: Option<Vec<Item>>) -> RuntimeCleanupRequest {
    RuntimeCleanupRequest {
        protocol: CLEANUP_PROTOCOL,
        workspace: "demo".into(),
        node: "worker".into(),
        sessions: sessions
            .iter()
            .map(|id| SessionEnd {
                id: id.to_string(),
                ended: true,
            })
            .collect(),
        apply,
    }
}

/// apply 当场再核对一次：预览之后又写了东西的不删；没变的删掉，别的会话不碰。
#[test]
fn the_runtime_removes_only_what_is_still_as_previewed() {
    let dir = temp("runtime-apply");
    let (state, root) = (dir.join("state"), dir.join("project"));
    fs::create_dir_all(&root).unwrap();
    let config = runtime_config(&root);
    let (a, b, c) = (id(1), id(2), id(3));
    output(&state, &a, 10);
    let b_output = output(&state, &b, 20);
    output(&state, &c, 30);

    let seen = runtime(
        &config,
        &runtime_request(&[&a, &b], None),
        &state,
        &SystemRunner,
    )
    .unwrap();
    assert_eq!(seen.items.len(), 2, "{seen:?}");
    assert!(seen.items.iter().all(|item| item.plan == Plan::Remove));
    // Written to after the preview.
    fs::write(b_output.join("r-0000000000000001/stderr"), "late\n").unwrap();

    let done = runtime(
        &config,
        &runtime_request(&[&a, &b], Some(seen.items.clone())),
        &state,
        &SystemRunner,
    )
    .unwrap();
    let outcome = |id: &str| {
        done.items
            .iter()
            .find(|item| item.id == id)
            .map(|item| (item.done, item.why))
    };
    assert_eq!(outcome(&a), Some((Some(Done::Removed), None)));
    assert_eq!(outcome(&b), Some((Some(Done::Skipped), Some(Why::Changed))));
    assert!(!crate::paths::session_dir(&state, &a).exists());
    assert!(b_output.exists());
    assert!(
        crate::paths::session_dir(&state, &c)
            .join("output")
            .exists(),
        "a session the Agent did not name is never touched"
    );
}

/// 越界的 id 在碰文件系统之前就拒；不是权威 Runtime、不是绑定的 Agent Node、协议号不对，也拒。
#[test]
fn the_runtime_refuses_before_building_a_path() {
    let dir = temp("runtime-refuse");
    let (state, root) = (dir.join("state"), dir.join("project"));
    fs::create_dir_all(&root).unwrap();
    let config = runtime_config(&root);
    for bad in ["../outside", "/etc", "bridge-1", ""] {
        let err = runtime(
            &config,
            &runtime_request(&[bad], None),
            &state,
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs, "{bad}");
        let mut item = Item::new(Role::Runtime, Kind::RuntimeOutput, bad, 0, "-".into(), None);
        item.plan = Plan::Remove;
        let err = runtime(
            &config,
            &runtime_request(&[], Some(vec![item])),
            &state,
            &SystemRunner,
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs, "{bad}");
    }
    let mut other = runtime_request(&[], None);
    other.node = "elsewhere".into();
    assert_eq!(
        runtime(&config, &other, &state, &SystemRunner)
            .unwrap_err()
            .code(),
        ErrorCode::Config
    );
    let mut old = runtime_request(&[], None);
    old.protocol = 9;
    assert_eq!(
        runtime(&config, &old, &state, &SystemRunner)
            .unwrap_err()
            .code(),
        ErrorCode::Version
    );
    assert!(!state.exists(), "nothing was created");
}

// ---- Agent ----

fn agent_session(state: &Path, id: &str, workspace: &str, ended: bool) -> PathBuf {
    let dir = crate::paths::session_dir(state, id);
    fs::create_dir_all(&dir).unwrap();
    let spec = serde_json::json!({
        "protocol": 1,
        "id": id,
        "workspace": workspace,
        "root": "/srv/demo",
        "runtime": null,
        "claude_config_dir": null,
        "permission_mode": "acceptEdits",
        "mode": {"mode": "print", "prompt": "go"},
        "timeout_secs": 900,
        "cwd": "/tmp/nowhere",
    });
    fs::write(dir.join("session.json"), spec.to_string()).unwrap();
    fs::write(dir.join("stdout"), "done\n").unwrap();
    if ended {
        fs::write(
            dir.join("exit"),
            r#"{"exit_code":0,"timed_out":false,"duration_ms":5,"error":null}"#,
        )
        .unwrap();
    }
    dir
}

fn agent_tools<'a>(dir: &Path, runner: &'a FakeRunner) -> Tools<'a> {
    Tools {
        local: None,
        config: crate::config::Config::parse(
            "this = \"worker\"\n[nodes.worker]\n[nodes.runtime]\nssh = \"to-runtime\"\nccnm_bin = \"/opt/runtime/ccnm\"\n",
        )
        .unwrap(),
        runner,
        state: dir.join("agent"),
        control_dir: control(dir),
        agents: crate::provider::AgentBinaries::with_claude(None),
        tmux: None,
        controller: PathBuf::from("/tmp/ccnm-cleanup-absent.sock"),
    }
}

fn agent_request(apply: Option<Vec<Item>>, purge: bool) -> AgentCleanupRequest {
    AgentCleanupRequest {
        protocol: CLEANUP_PROTOCOL,
        workspace: "demo".into(),
        agent: None,
        runtime_node: "runtime".into(),
        purge,
        apply,
    }
}

fn runtime_answer(items: Vec<Item>, guard_session: Option<String>) -> Output {
    Output::exited(
        0,
        serde_json::to_string(&RuntimeCleanupReport {
            protocol: CLEANUP_PROTOCOL,
            workspace: "demo".into(),
            uid: Some(1002),
            guard_session,
            guard_unclear: false,
            items,
        })
        .unwrap(),
    )
}

fn runtime_output_item(id: &str, why: Option<Why>) -> Item {
    Item::new(
        Role::Runtime,
        Kind::RuntimeOutput,
        id,
        48,
        "48-1".into(),
        why,
    )
}

fn find<'a>(items: &'a [Item], role: Role, id: &str) -> &'a Item {
    items
        .iter()
        .find(|item| item.role == role && item.id == id)
        .unwrap_or_else(|| panic!("{role:?} {id} not listed: {items:?}"))
}

/// Agent 只列这个 workspace 自己的、认得出的会话；Runtime 那边留着的、写锁标着的，
/// 这边的记录也留着——以后找 Runtime 那一半全靠它。
#[test]
fn the_agent_lists_its_own_sessions_and_keeps_what_the_runtime_keeps() {
    let dir = temp("agent-preview");
    let state = dir.join("agent");
    let (done, running, guarded, served) = (id(1), id(2), id(3), id(4));
    agent_session(&state, &done, "demo", true);
    agent_session(&state, &running, "demo", false);
    agent_session(&state, &guarded, "demo", true);
    agent_session(&state, &served, "demo", true);
    agent_session(&state, &id(5), "another", true);
    // A fence from P58 (no record) and a link: neither is attributable.
    fs::create_dir_all(crate::paths::session_dir(&state, &id(6))).unwrap();
    std::os::unix::fs::symlink(
        crate::paths::session_dir(&state, &done),
        crate::paths::session_dir(&state, &id(7)),
    )
    .unwrap();

    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "501\n")); // id -u
    runner.push(runtime_answer(
        vec![
            runtime_output_item(&done, None),
            runtime_output_item(&served, Some(Why::InUse)),
        ],
        Some(guarded.clone()),
    ));
    let report = agent(&agent_request(None, false), &agent_tools(&dir, &runner)).unwrap();
    assert_eq!((report.uid, report.runtime_uid), (Some(501), Some(1002)));
    let agent_ids: Vec<&str> = report
        .items
        .iter()
        .filter(|i| i.role == Role::Agent)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(agent_ids.len(), 4, "{agent_ids:?}");
    assert_eq!(find(&report.items, Role::Agent, &done).plan, Plan::Remove);
    assert_eq!(
        find(&report.items, Role::Agent, &running).why,
        Some(Why::NotEnded)
    );
    assert_eq!(
        find(&report.items, Role::Agent, &guarded).why,
        Some(Why::Guard)
    );
    assert_eq!(
        find(&report.items, Role::Agent, &served).why,
        Some(Why::RuntimeHalfPending)
    );
    assert_eq!(find(&report.items, Role::Runtime, &done).plan, Plan::Remove);

    // What went to the Runtime: every session of the workspace, and which ended.
    let sent = runner.calls().last().unwrap().display();
    assert!(
        sent.contains("to-runtime") && sent.contains("runtime-cleanup"),
        "{sent}"
    );
}

/// Runtime 问不到：Agent 这边一条都不删，否则以后没人知道 Runtime 上那些输出属于谁。
#[test]
fn without_the_runtime_the_agent_keeps_its_records() {
    let dir = temp("agent-no-runtime");
    let state = dir.join("agent");
    agent_session(&state, &id(1), "demo", true);
    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "501\n"));
    runner.push(Output {
        stderr: b"ssh: connect to host runtime port 22: Connection refused\n".to_vec(),
        ..Output::exited(255, "")
    });
    let report = agent(&agent_request(None, true), &agent_tools(&dir, &runner)).unwrap();
    assert!(report.runtime_error.is_some());
    assert_eq!(
        find(&report.items, Role::Agent, &id(1)).why,
        Some(Why::RuntimeNotAsked)
    );
}

/// apply：先 Runtime 后 Agent；Runtime 那一半没删掉的会话，Agent 的记录留着。
#[test]
fn the_agent_removes_a_record_only_after_its_runtime_half() {
    let dir = temp("agent-apply");
    let state = dir.join("agent");
    let (a, b) = (id(1), id(2));
    agent_session(&state, &a, "demo", true);
    agent_session(&state, &b, "demo", true);
    fs::create_dir_all(crate::paths::workspace_dir(&state, "demo")).unwrap();

    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "501\n"));
    runner.push(runtime_answer(
        vec![runtime_output_item(&a, None), runtime_output_item(&b, None)],
        None,
    ));
    let preview = agent(&agent_request(None, true), &agent_tools(&dir, &runner)).unwrap();
    let asked: Vec<Item> = preview
        .items
        .iter()
        .filter(|i| i.plan == Plan::Remove)
        .cloned()
        .collect();
    assert_eq!(asked.len(), 5, "{asked:?}");

    let runner = FakeRunner::new();
    runner.push(Output::exited(0, "501\n"));
    runner.push(runtime_answer(
        vec![
            runtime_output_item(&a, None).finish(Done::Removed, None, None),
            runtime_output_item(&b, None).finish(
                Done::Failed,
                None,
                Some("PermissionDenied".into()),
            ),
        ],
        None,
    ));
    let report = agent(
        &agent_request(Some(asked), true),
        &agent_tools(&dir, &runner),
    )
    .unwrap();
    assert_eq!(
        find(&report.items, Role::Agent, &a).done,
        Some(Done::Removed)
    );
    let kept = find(&report.items, Role::Agent, &b);
    assert_eq!(
        (kept.done, kept.why),
        (Some(Done::Skipped), Some(Why::RuntimeHalfPending))
    );
    assert_eq!(
        find(&report.items, Role::Runtime, &b).done,
        Some(Done::Failed)
    );
    assert!(!crate::paths::session_dir(&state, &a).exists());
    assert!(crate::paths::session_dir(&state, &b).exists());
    assert!(!crate::paths::workspace_dir(&state, "demo").exists());
}

/// 会话控制锁被别人拿着（启动、停止、生成视图）：让开，不从它脚下抽文件。
#[test]
fn a_session_someone_is_working_on_is_stepped_around() {
    let dir = temp("agent-busy");
    let state = dir.join("agent");
    let a = id(1);
    let session = agent_session(&state, &a, "demo", true);
    let runner = FakeRunner::new();
    let tools = agent_tools(&dir, &runner);
    // Held first, so the listing below already includes its lock file.
    let held = session::Control::lock(&session::Dir::at(&session)).unwrap();
    let current = agent_items(&tools, "demo", false, None);
    let item = find(&current, Role::Agent, &a).clone();
    assert_eq!(item.plan, Plan::Remove);
    let done = apply_agent(&tools, "demo", &item, &current);
    assert_eq!(
        (done.done, done.why),
        (Some(Done::Skipped), Some(Why::InUse))
    );
    drop(held);
    assert!(session.join("session.json").exists());
}

// ---- Operator ----

fn record(handle: &str, state: State, text: &str) -> Record {
    Record {
        session: handle.into(),
        launch: crate::rpc::store::Launch {
            workspace: "demo".into(),
            agent: Some(InstanceRef {
                node: "worker".into(),
                instance: "claude-main".into(),
            }),
            mode: "print".into(),
            prompt: "go".into(),
        },
        start_key: Some(format!("key-{handle}")),
        state,
        accepted_at: "2026-09-30T00:00:00Z".into(),
        stop_requested: false,
        timeout_ms: None,
        owner_pid: 1,
        owner_started: "x".into(),
        managed_session: Some(id(9)),
        dispatched: true,
        finish: state.terminal().then(|| crate::rpc::store::Finish {
            text: Some(text.into()),
            output: text.into(),
            ..Default::default()
        }),
        cleaned_at: None,
    }
}

/// Operator 这边：结束了的删拷贝、留墓碑和 start_key；没结束或说不清的不动。
#[test]
fn the_operator_leaves_a_tombstone_and_the_start_key() {
    let dir = temp("operator");
    let store = Store::open(&dir).unwrap();
    for record in [
        record("s-done", State::Completed, "a long answer"),
        record("s-running", State::Running, ""),
        record("s-lost", State::Running, ""),
    ] {
        store.write(&record).unwrap();
        store
            .claim_key(
                "demo",
                record.start_key.as_deref().unwrap(),
                &record.session,
            )
            .unwrap();
    }
    let copy = store.output_dir("s-done").unwrap();
    fs::write(copy.join("stdout.view"), vec![b'x'; 1000]).unwrap();
    let owner_of = |record: &Record| {
        if record.session == "s-running" {
            OwnerCheck::Alive
        } else {
            OwnerCheck::Gone
        }
    };
    let items = operator_items(&store, "demo", &owner_of).unwrap();
    assert_eq!(items.len(), 3);
    let done = find(&items, Role::Operator, "s-done").clone();
    assert_eq!(done.plan, Plan::Remove);
    assert_eq!(done.bytes, 1000 + 2 * "a long answer".len() as u64);
    assert_eq!(
        find(&items, Role::Operator, "s-running").why,
        Some(Why::NotEnded)
    );
    assert_eq!(
        find(&items, Role::Operator, "s-lost").why,
        Some(Why::Unknown)
    );

    let out = apply_operator(&store, &done, &owner_of);
    assert_eq!(out.done, Some(Done::Removed));
    assert!(!copy.exists());
    let tomb = store.read("s-done").unwrap().unwrap();
    assert!(tomb.cleaned_at.is_some());
    assert_eq!(tomb.state, State::Completed);
    assert_eq!(tomb.finish.as_ref().unwrap().text, None);
    assert!(tomb.finish.as_ref().unwrap().output.is_empty());
    assert_eq!(
        tomb.launch.prompt, "go",
        "the input stays for the start_key check"
    );
    assert_eq!(
        store.find_key("demo", "key-s-done").unwrap(),
        Some(Some("s-done".to_string()))
    );
    // Cleaned and nothing left: not listed again.
    assert!(
        operator_items(&store, "demo", &owner_of)
            .unwrap()
            .iter()
            .all(|item| item.id != "s-done")
    );
    // Applying the same item again: it is not what was previewed any more.
    assert_eq!(
        apply_operator(&store, &done, &owner_of).done,
        Some(Done::Skipped)
    );
}

/// 预览之后记录变了（比如刚写进终态），apply 就不动它。
#[test]
fn the_operator_does_not_clean_a_record_that_changed() {
    let dir = temp("operator-changed");
    let store = Store::open(&dir).unwrap();
    store
        .write(&record("s-a", State::Completed, "one"))
        .unwrap();
    let gone = |_: &Record| OwnerCheck::Gone;
    let item = operator_items(&store, "demo", &gone).unwrap().remove(0);
    store
        .update("s-a", |record| record.stop_requested = true)
        .unwrap();
    let out = apply_operator(&store, &item, &gone);
    assert_eq!(
        (out.done, out.why),
        (Some(Done::Skipped), Some(Why::Changed))
    );
    assert!(store.read("s-a").unwrap().unwrap().cleaned_at.is_none());
}

#[test]
fn a_version_sees_any_write_below_it() {
    let dir = temp("measure");
    let tree = dir.join("t/a/b");
    fs::create_dir_all(&tree).unwrap();
    fs::write(tree.join("f"), "12345").unwrap();
    let (bytes, before) = measure(&dir.join("t"), None);
    assert_eq!(bytes, 5);
    std::thread::sleep(Duration::from_millis(5));
    fs::write(tree.join("g"), "").unwrap();
    assert_ne!(measure(&dir.join("t"), None).1, before);
    let (skipped, _) = measure(&dir.join("t"), Some("a"));
    assert_eq!(skipped, 0);
}

#[test]
fn sizes_read_like_du() {
    assert_eq!(size(0), "0 B");
    assert_eq!(size(1023), "1023 B");
    assert_eq!(size(1536), "1.5 KiB");
    assert_eq!(size(48 * 1024 * 1024), "48.0 MiB");
}

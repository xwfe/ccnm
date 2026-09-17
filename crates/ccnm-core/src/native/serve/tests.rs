use super::*;
use crate::process::{FakeRunner, Output};
use std::os::unix::net::UnixStream;

#[test]
fn a_client_line_longer_than_the_limit_ends_the_read_instead_of_growing() {
    let input = b"{\"a\":1}\n0123456789abcdef\nshort\n".to_vec();
    let mut lines = BoundedLines::new(std::io::BufReader::with_capacity(4, &input[..]), 10);
    assert_eq!(lines.next_line().ok().flatten().unwrap(), b"{\"a\":1}");
    assert!(matches!(lines.next_line(), Err(LineError::TooLong)));

    let mut last = BoundedLines::new(&b"no newline at the end"[..], 64);
    assert_eq!(
        last.next_line().ok().flatten().unwrap(),
        b"no newline at the end"
    );
    assert!(matches!(last.next_line(), Ok(None)));
}

#[test]
fn only_the_measured_codex_release_runs_the_executor() {
    let base = Cmd::new("/opt/codex/bin/codex");
    let runner = FakeRunner::new();
    runner.push(Output::exited(
        0,
        format!("codex-cli {}\n", crate::provider::codex::VERSION),
    ));
    check_version(&base, &runner).unwrap();
    assert_eq!(
        runner.calls()[0].args,
        vec![std::ffi::OsString::from("--version")]
    );

    runner.push(Output::exited(0, "codex-cli 0.155.0\n"));
    let error = check_version(&base, &runner).unwrap_err();
    assert_eq!(error.code(), ErrorCode::Version);
    assert!(error.message().contains("0.155.0"), "{error}");
}

/// The executor's environment is the Runtime child environment plus exactly
/// two variables ccnm chose.
#[test]
fn the_executor_gets_a_ccnm_home_and_the_marker_and_no_agent_login() {
    let cmd = executor_cmd(
        Path::new("/opt/codex/bin/codex"),
        Path::new("/work/project"),
        Path::new("/state/exec-server/s1-x"),
        "s1-x",
    );
    assert!(
        cmd.env
            .contains(&("CODEX_HOME".into(), "/state/exec-server/s1-x".into()))
    );
    assert!(cmd.env.contains(&(MARKER.into(), "s1-x".into())));
    for stripped in ["SSH_AUTH_SOCK", "OPENAI_API_KEY", "CLAUDE_CONFIG_DIR"] {
        assert!(cmd.env_remove.contains(&stripped.into()), "{stripped}");
    }
    assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/work/project")));
}

/// A real process with the marker, the way exec-server's children carry
/// it: found by the sweep, killed, and the sweep then reports clean.
#[test]
fn the_sweep_finds_a_marked_process_kills_it_and_then_reports_clean() {
    let marker = format!("sweep-{}", crate::session::new_id());
    let mut child = Command::new("sleep")
        .arg("60")
        .env(MARKER, &marker)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marked_processes(&marker).unwrap().contains(&pid) {
        assert!(Instant::now() < deadline, "marked process never listed");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Reaping has to happen for the kill to show; a real orphan is reaped
    // by init, here the test is its parent.
    let reaper = std::thread::spawn(move || child.wait());
    Sweeper::system()
        .sweep(&marker, Duration::from_secs(5))
        .unwrap();
    reaper.join().unwrap().unwrap();
    assert!(marked_processes(&marker).unwrap().is_empty());
    // Another session's marker is not this one's.
    assert!(
        marked_processes(&format!("{marker}-other"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn what_cannot_be_listed_or_killed_is_never_reported_clean() {
    fn broken(_: &str) -> Result<Vec<u32>> {
        Err(Error::internal("ps failed"))
    }
    fn immortal(_: &str) -> Result<Vec<u32>> {
        // Above any pid_max, so the kill cannot reach a real process --
        // even when a test runs as root -- and the listing never changes.
        Ok(vec![999_999_999])
    }
    let error = Sweeper { list: broken }
        .sweep("m", Duration::from_millis(300))
        .unwrap_err();
    assert!(error.message().contains("stays held"), "{error}");
    let error = Sweeper { list: immortal }
        .sweep("m", Duration::from_millis(300))
        .unwrap_err();
    assert!(error.message().contains("stays held"), "{error}");
}

// ---- Liveness (P26): the relay with timings a test can wait for ----

const FAST: Timing = Timing {
    ping_after: Duration::from_millis(100),
    give_up_after: Duration::from_millis(400),
    tick: Duration::from_millis(10),
};

/// A project directory next to the executor's log, removed afterwards.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-relay-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        Scratch { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.join("project")
    }

    /// The fake exec-server the CLI tests use: it really starts processes
    /// and logs every message that reached it.
    fn executor(&self, marker: &str) -> Child {
        let bin =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake_exec_server.py");
        spawn(
            &Cmd::new(bin)
                .env("FAKE_EXEC_LOG", self.dir.join("executor.log"))
                .env(MARKER, marker),
        )
        .unwrap()
    }

    fn executor_saw(&self) -> String {
        std::fs::read_to_string(self.dir.join("executor.log")).unwrap_or_default()
    }

    /// A captured `process/start` (P21) that runs `script` in the project.
    fn start(&self, id: u64, script: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../tests/fixtures/codex-0.154.0/exec-server/process-start-workspace-write.json",
        );
        let text = std::fs::read_to_string(path)
            .unwrap()
            .replace("{ROOT}", self.root().to_str().unwrap())
            .replace("{OUTSIDE}", self.dir.to_str().unwrap())
            .replace("{SERVER_HOME}", "/home/ccrun");
        let mut message: Value = serde_json::from_str(&text).unwrap();
        message["id"] = serde_json::json!(id);
        message["params"]["processId"] = serde_json::json!(format!("p{id}"));
        message["params"]["argv"] = serde_json::json!(["/bin/sh", "-c", script]);
        message
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn marker(test: &str) -> String {
    format!("relay-{test}-{}", crate::session::new_id())
}

/// The supervisor's end of a client connection, and the test's.
fn client_pair() -> (Client, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let client = Client {
        input: Box::new(ours.try_clone().unwrap()),
        output: Box::new(ours),
    };
    (client, theirs)
}

fn send(stream: &UnixStream, message: &Value) {
    let mut stream = stream;
    writeln!(stream, "{message}").unwrap();
}

/// Reads everything the supervisor sends and answers each liveness request
/// the way Codex 0.154.0 did in P26.1. Returns how many it answered.
fn answer_like_codex(stream: UnixStream) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let mut answered = 0;
        for line in BufReader::new(stream.try_clone().unwrap()).lines() {
            let Ok(line) = line else { break };
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["method"] == "ccnm/liveness" {
                let answer = serde_json::json!({"id": message["id"], "error": {"code": -32601, "message": "exec-server client does not implement `ccnm/liveness` yet"}});
                let mut out = &stream;
                if writeln!(out, "{answer}").is_err() {
                    break;
                }
                answered += 1;
            }
        }
        answered
    })
}

/// Reads and throws away everything, answering nothing: a Codex whose
/// machine is asleep looks like this from here, until the buffers fill.
fn drain(stream: UnixStream) -> std::thread::JoinHandle<Vec<Value>> {
    std::thread::spawn(move || {
        BufReader::new(stream)
            .lines()
            .map_while(|line| line.ok())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect()
    })
}

fn wait_exit(child: &mut Child) {
    assert!(
        wait_for_exit(child, Duration::from_secs(10)),
        "executor did not exit"
    );
}

fn kill_group(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .status();
    let _ = child.wait();
}

#[test]
fn a_client_that_answers_liveness_requests_keeps_its_session_and_the_answers_stay_here() {
    let scratch = Scratch::new("answers");
    let mut child = scratch.executor(&marker("answers"));
    let (client, peer) = client_pair();
    send(
        &peer,
        &serde_json::json!({"id": 1, "method": "initialize", "params": {"clientName": "codex-environment", "resumeSessionId": null}}),
    );
    let closer = peer.try_clone().unwrap();
    let answering = answer_like_codex(peer);
    let policy = Policy::new(scratch.root(), MARKER);
    let (ended, end) = mpsc::channel();
    let relay_thread = std::thread::spawn(move || {
        let _ = ended.send(relay(&mut child, policy, client, FAST));
        child
    });

    assert!(
        end.recv_timeout(FAST.give_up_after * 5).is_err(),
        "a client that answered was given up on"
    );
    closer.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(
        end.recv_timeout(Duration::from_secs(5)).unwrap(),
        End::ClientClosed
    );
    let mut child = relay_thread.join().unwrap();
    wait_exit(&mut child);
    let answered = answering.join().unwrap();
    assert!(
        answered >= 10,
        "only {answered} liveness requests in two seconds"
    );

    let saw = scratch.executor_saw();
    assert!(
        saw.contains("initialize"),
        "the relay forwarded nothing: {saw}"
    );
    assert!(
        !saw.contains("ccnm-liveness"),
        "an answer reached the executor: {saw}"
    );
}

#[test]
fn a_client_that_never_answers_is_given_up_on_after_the_silence_limit() {
    let scratch = Scratch::new("silent");
    let mut child = scratch.executor(&marker("silent"));
    let (client, peer) = client_pair();
    let received = drain(peer.try_clone().unwrap());

    let started = Instant::now();
    let end = relay(
        &mut child,
        Policy::new(scratch.root(), MARKER),
        client,
        FAST,
    );
    let took = started.elapsed();
    assert_eq!(end, End::ClientSilent);
    assert!(
        took >= FAST.give_up_after && took < FAST.give_up_after + Duration::from_secs(2),
        "{took:?}"
    );
    wait_exit(&mut child);
    peer.shutdown(std::net::Shutdown::Both).unwrap();
    let asked: Vec<_> = received
        .join()
        .unwrap()
        .into_iter()
        .filter(|m| m["method"] == "ccnm/liveness")
        .collect();
    assert!(
        asked.len() >= 2,
        "asked {} times before giving up",
        asked.len()
    );
}

/// An executor that sends one line of `size` bytes and then waits for its
/// stdin to close: what a large `fs/readFile` answer looks like on the wire.
/// `cat` keeps the stdout pipe open; redirecting it away would end the relay
/// as `ExecutorClosed` the moment the line is through.
fn large_line_executor(size: usize) -> Child {
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            &format!("head -c {size} /dev/zero | tr '\\0' a; echo; exec cat"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().unwrap()
}

/// A client reading a large answer slowly sends nothing for longer than the
/// silence limit and is still there. Once the line is through and it still
/// says nothing, the count runs from the last chunk it took.
#[test]
fn a_large_message_the_client_is_still_taking_is_not_silence() {
    const SIZE: usize = 1_500_000;
    const RATE: f64 = 1_000_000.0; // bytes per second: about 1.5 s for the line
    let mut child = large_line_executor(SIZE);
    let (client, peer) = client_pair();
    let reader = std::thread::spawn(move || {
        let started = Instant::now();
        let mut total = 0usize;
        let mut buf = vec![0u8; 64 * 1024];
        let mut stream = &peer;
        loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0, "connection closed before the line was through");
            total += n;
            if buf[..n].contains(&b'\n') {
                return (Instant::now(), total, peer);
            }
            let due = Duration::from_secs_f64(total as f64 / RATE);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    });
    let (ended, end) = mpsc::channel();
    let started = Instant::now();
    let root = std::env::temp_dir();
    let relay_thread = std::thread::spawn(move || {
        let _ = ended.send(relay(&mut child, Policy::new(root, MARKER), client, FAST));
        child
    });

    let (through, total, _peer) = reader.join().unwrap();
    assert!(total > SIZE, "{total}");
    assert!(
        through.duration_since(started) > FAST.give_up_after * 2,
        "the line went through too fast to prove anything: {:?}",
        through.duration_since(started)
    );
    if let Ok(early) = end.try_recv() {
        panic!("the relay ended while the line was still going through: {early:?}");
    }
    assert_eq!(
        end.recv_timeout(Duration::from_secs(5)).unwrap(),
        End::ClientSilent
    );
    let mut child = relay_thread.join().unwrap();
    wait_exit(&mut child);
}

/// The same line to a client that takes none of it: the write blocks with
/// the output lock held, no ping can go out, and the session still ends.
#[test]
fn a_large_message_nobody_takes_ends_the_session_all_the_same() {
    let mut child = large_line_executor(8_000_000);
    let (client, peer) = client_pair();
    let started = Instant::now();
    let end = relay(
        &mut child,
        Policy::new(std::env::temp_dir(), MARKER),
        client,
        FAST,
    );
    let took = started.elapsed();
    assert_eq!(end, End::ClientSilent);
    assert!(
        took < FAST.give_up_after + Duration::from_secs(2),
        "{took:?}"
    );
    kill_group(&mut child);
    drop(peer);
}

/// Small lines keep landing in the connection's buffers whether or not
/// anyone reads them -- `process/output` from a command that is still
/// running, to a Codex that is gone. They are not hearing from the client.
#[test]
fn output_to_a_client_that_never_reads_is_not_hearing_from_it() {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "while :; do echo tick; sleep 0.02; done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().unwrap();
    let (client, peer) = client_pair();
    let started = Instant::now();
    let end = relay(
        &mut child,
        Policy::new(std::env::temp_dir(), MARKER),
        client,
        FAST,
    );
    let took = started.elapsed();
    assert_eq!(end, End::ClientSilent);
    assert!(
        took < FAST.give_up_after + Duration::from_secs(2),
        "{took:?}"
    );
    kill_group(&mut child);
    drop(peer);
}

/// A progress line straight to file descriptor 2. libtest captures
/// `eprintln!` until a test ends, so a test that never ends shows nothing;
/// this does not go through the capture and lands in the CI log's tail.
fn stage(step: &str) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "[session-test {:?}] {step}",
        std::time::SystemTime::now()
    );
}

/// Run `work` on its own thread and give it `limit`. A step that hangs fails
/// the test with its name and what the marked processes look like, instead of
/// holding the CI job until its timeout with nothing to read (this test hung
/// on the Linux runner and only there).
fn within<T: Send + 'static>(
    step: &str,
    limit: Duration,
    marker: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(limit) {
        Ok(value) => value,
        Err(_) => panic!(
            "{step} did not finish within {limit:?}; processes with the marker: {}",
            describe_marked(marker)
        ),
    }
}

/// pid, state and command line of every process carrying the marker, read
/// on a thread of its own so a stuck /proc read cannot hang the report.
fn describe_marked(marker: &str) -> String {
    let marker = marker.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let pids = marked_processes(&marker);
        let text = match pids {
            Err(e) => format!("listing failed: {e}"),
            Ok(pids) => pids
                .iter()
                .map(|pid| {
                    let out = Command::new("ps")
                        .args(["-o", "pid=,ppid=,stat=,args=", "-p", &pid.to_string()])
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default();
                    format!("[{out}]")
                })
                .collect::<Vec<_>>()
                .join(" "),
        };
        let _ = tx.send(text);
    });
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| "listing the processes did not finish within 5 s".into())
}

/// The whole session, not just the relay: a client that falls silent with a
/// command still running is given up on, the command is gone, and the guard
/// says `released` -- the same ending as a client that closed its stdin.
#[test]
fn a_session_given_up_on_shuts_down_and_releases_the_guard() {
    stage("start");
    let scratch = Scratch::new("session");
    let state = scratch.dir.join("state");
    let marker = marker("session");
    let timing = Timing {
        give_up_after: Duration::from_millis(1500),
        ..FAST
    };
    let root = scratch.root();
    let guard_state = state.clone();
    let guard = within(
        "acquiring the write guard",
        Duration::from_secs(10),
        &marker,
        move || {
            WriteGuard::acquire(&guard_state, &root, "demo", "s1", None, &SystemRunner).unwrap()
        },
    );
    stage("guard acquired");
    let child = scratch.executor(&marker);
    stage("executor spawned");
    let session = Session {
        child,
        policy: Policy::new(scratch.root(), MARKER),
        marker: marker.clone(),
        guard,
        home: CodexHome::create(&state, &marker).unwrap(),
    };
    stage("home created");
    let (client, peer) = client_pair();
    send(
        &peer,
        &serde_json::json!({"id": 1, "method": "initialize", "params": {"clientName": "codex-environment", "resumeSessionId": null}}),
    );
    send(
        &peer,
        &serde_json::json!({"method": "initialized", "params": {}}),
    );
    stage("initialize and initialized sent");
    send(&peer, &scratch.start(2, "exec sleep 600"));
    stage("process/start sent");
    let received = drain(peer.try_clone().unwrap());
    let watcher_marker = marker.clone();
    let started = Instant::now();
    let (seen, running_at) = mpsc::channel();
    std::thread::spawn(move || {
        // The executor and its command both carry the marker.
        while marked_processes(&watcher_marker).unwrap().len() < 2 {
            if started.elapsed() > Duration::from_secs(5) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = seen.send(started.elapsed());
    });

    stage("running the session");
    let end = within(
        "Session::run",
        Duration::from_secs(30),
        &marker,
        move || session.run(client, timing, &Sweeper::system()),
    )
    .unwrap();
    stage(&format!("session ended: {end:?}"));
    assert_eq!(end, End::ClientSilent);
    let running_after = running_at
        .recv_timeout(Duration::from_secs(10))
        .expect("the command was never seen running");
    assert!(running_after < timing.give_up_after, "{running_after:?}");
    let left_marker = marker.clone();
    let left = within(
        "listing leftovers",
        Duration::from_secs(10),
        &marker,
        move || marked_processes(&left_marker).unwrap(),
    );
    assert!(left.is_empty(), "{}", describe_marked(&marker));
    let locks: Vec<_> = std::fs::read_dir(state.join("write-guards"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(locks, vec!["released\n".to_string()]);
    assert!(!state.join("exec-server").join(&marker).exists());
    assert!(scratch.executor_saw().contains("process/start"));
    stage("guard released, shutting the client down");
    peer.shutdown(std::net::Shutdown::Both).unwrap();
    let replies = within(
        "draining the client side",
        Duration::from_secs(10),
        &marker,
        move || received.join().unwrap(),
    );
    assert!(replies.iter().any(|m| m["id"] == 2), "{replies:?}");
    stage("done");
}

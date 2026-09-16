use super::*;
use crate::process::{FakeRunner, Output};

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

//! What a Windows machine may run so far (P107.1): the Runtime's own
//! commands get past the gate, the Agent Node's are refused before they
//! start anything, and the Runtime's ones whose Windows side is not written
//! yet say which stage brings it. Compiled on Windows only; the Windows CI
//! job runs it.
#![cfg(windows)]

use std::process::{Command, Output};

/// `CCNM_E_NOT_READY`.
const NOT_READY: i32 = 3;

fn ccnm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ccnm"))
        .args([
            "--lang",
            "en",
            "--config",
            "C:/ccnm-p107-no-such-dir/config.toml",
        ])
        .args(args)
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn the_agent_nodes_commands_are_refused_before_they_start() {
    for args in [
        &["ls"][..],
        &["status"],
        &["doctor"],
        &["run", "x"],
        &["controller", "status"],
        &["internal", "agent-status", "--payload", "e30"],
        &["internal", "controller"],
    ] {
        let out = ccnm(args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(NOT_READY), "{args:?}: {err}");
        assert!(err.contains("not supported on Windows"), "{args:?}: {err}");
        assert!(err.contains("AI machine"), "{args:?}: {err}");
    }
}

#[test]
fn the_runtimes_unwritten_commands_name_their_stage() {
    for (args, stage) in [
        (
            &["internal", "mcp-serve", "--payload", "e30"][..],
            "P108–P110",
        ),
        (&["internal", "runtime-audit", "--payload", "e30"], "P111"),
        (&["internal", "runtime-cleanup", "--payload", "e30"], "P110"),
    ] {
        let out = ccnm(args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(NOT_READY), "{args:?}: {err}");
        assert!(
            err.contains("not supported on Windows yet"),
            "{args:?}: {err}"
        );
        assert!(err.contains(stage), "{args:?}: {err}");
    }
}

/// Past the gate, each fails on what it is given -- here a config file that
/// is not there -- and not on being on Windows.
#[test]
fn the_runtimes_config_commands_get_past_the_gate() {
    for args in [
        &["ws", "ls"][..],
        &["internal", "runtime-resolve", "--payload", "e30"],
        &["internal", "runtime-guard", "--payload", "e30"],
    ] {
        let out = ccnm(args);
        let err = stderr(&out);
        assert!(!err.contains("not supported on Windows"), "{args:?}: {err}");
        assert_ne!(out.status.code(), Some(NOT_READY), "{args:?}: {err}");
    }
}

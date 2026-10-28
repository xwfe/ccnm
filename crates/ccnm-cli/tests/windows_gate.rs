//! What a Windows machine may run (P107.1): the Runtime's own commands get
//! past the gate -- all of them since P111 -- and the Agent Node's are
//! refused before they start anything. Compiled on Windows only; the
//! Windows CI job runs it.
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

/// Setting a Windows Runtime up from PowerShell or cmd (P111.1): no `HOME`
/// there and no `--config`, so the files land under `USERPROFILE` (D6).
/// `init` refuses to make this machine an Agent and tells the Agent which
/// `ccnm_bin` to write; a project is stored as `C:/…`; `runtime_user` on this
/// node is refused with what to do instead (D8).
#[test]
fn the_runtime_is_set_up_from_a_plain_windows_shell() {
    let profile = std::env::temp_dir().join(format!("ccnm-p111-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let project = profile.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let run = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.env_clear()
            .env("USERPROFILE", &profile)
            .args(["--lang", "en"])
            .args(args);
        for name in [
            "SystemRoot",
            "SystemDrive",
            "PATH",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.output().unwrap()
    };
    let config = profile.join(".config").join("ccnm").join("config.toml");

    let out = run(&["init", "--runtime", "work"]);
    assert_eq!(out.status.code(), Some(NOT_READY), "{}", stderr(&out));
    assert!(stderr(&out).contains("Windows can only be the machine holding the projects"));
    assert!(!config.exists());

    let out = run(&["init", "--agent", "work"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{said}{}", stderr(&out));
    assert!(config.is_file(), "{said}");
    let exe = ccnm_core::reported_path(std::path::Path::new(env!("CARGO_BIN_EXE_ccnm")));
    assert!(
        said.contains(&format!("ccnm_bin = \"{}\"", exe.display())),
        "{said}"
    );

    let out = run(&["add", project.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let written = std::fs::read_to_string(&config).unwrap();
    let root = ccnm_core::reported_path(&project.canonicalize().unwrap());
    assert!(
        written.contains(&format!("root = \"{}\"", root.display())),
        "{written}"
    );
    let out = run(&["ws", "ls"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("proj"),
        "{}",
        stderr(&out)
    );
    let out = run(&["rm", "proj"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap()
            .contains("[workspaces.proj]")
    );

    std::fs::write(
        &config,
        std::fs::read_to_string(&config).unwrap().replace(
            "[nodes.runtime]",
            "[nodes.runtime]\nruntime_user = \"ccrun\"",
        ),
    )
    .unwrap();
    let out = run(&["ws", "ls"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("a Windows Runtime has no dedicated-account mode yet"),
        "{}",
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&profile);
}

/// Past the gate, each fails on what it is given -- here a config file that
/// is not there, or a payload with no protocol number -- and not on being
/// on Windows. mcp-serve is past it since P108 and turns away the sessions
/// that write itself; tests/read_tools.rs has that, and the read session.
#[test]
fn the_runtimes_config_commands_get_past_the_gate() {
    for args in [
        &["ws", "ls"][..],
        &["internal", "runtime-resolve", "--payload", "e30"],
        &["internal", "runtime-guard", "--payload", "e30"],
        &["internal", "runtime-audit", "--payload", "e30"],
        &["internal", "runtime-cleanup", "--payload", "e30"],
        &["internal", "mcp-serve", "--payload", "e30"],
    ] {
        let out = ccnm(args);
        let err = stderr(&out);
        assert!(!err.contains("not supported on Windows"), "{args:?}: {err}");
        assert_ne!(out.status.code(), Some(NOT_READY), "{args:?}: {err}");
    }
}

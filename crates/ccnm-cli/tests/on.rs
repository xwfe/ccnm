//! `--on` (P114), through the real binary: which machine a command goes to,
//! what is remembered, and what is refused. The two AI machines are a fake
//! `ssh` on PATH that logs who was dialled and answers `agent-status` with
//! whatever the test says is running there.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ccnm_core::protocol::run::{LiveSession, StatusReport};
use ccnm_testdir::TestDir;

struct Machines {
    dir: TestDir,
    config: PathBuf,
}

/// Two AI machines, `agent` (alias `fodelf.invalid`, the project's own) and
/// `hpsrv` (alias `hpsrv.invalid`), and one project, `xshun`.
fn machines(test: &str) -> Machines {
    // Under /tmp, not $TMPDIR: macOS's is long enough that the ssh
    // ControlPath under it passes the 104-byte socket limit.
    let dir = Path::new("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("ccnm-on-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("root")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "this = \"runtime\"\n[nodes.runtime]\n[nodes.agent]\nssh = \"fodelf.invalid\"\n[nodes.hpsrv]\nssh = \"hpsrv.invalid\"\n[workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\nallow_unconfined_exec = true\n",
            dir.join("root").display()
        ),
    )
    .unwrap();
    let machines = Machines {
        dir: TestDir::adopt(dir),
        config,
    };
    machines.running_on(&[]);
    machines
}

impl Machines {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `args` first: `ccnm <workspace>` is `ccnm run <workspace>` only when
    /// the workspace is the first word.
    fn ccnm(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path().join("bin").display()),
            )
            .env("HOME", self.path())
            .env("XDG_STATE_HOME", self.state())
            .env("CCNM_LANG", "en")
            .arg("--config")
            .arg(&self.config);
        cmd
    }

    fn state(&self) -> PathBuf {
        self.path().join("s")
    }

    /// The fake `ssh`: logs the alias it was asked to dial, says `xshun`
    /// has a live session on each alias in `live`, and fails to connect for
    /// everything else.
    fn running_on(&self, live: &[&str]) {
        let report = StatusReport {
            protocol: 1,
            agent_identity: None,
            tmux: Ok("tmux 3.5".into()),
            sessions: vec![LiveSession {
                provider: Default::default(),
                agent_identity: None,
                tmux_session: "ccnm-xshun".into(),
                workspace: Some("xshun".into()),
                session: Some("s1".into()),
                created: 1,
                attached: 0,
                context: None,
                tools: Some(true),
                compactions: None,
            }],
            records: vec![],
            other_instances: vec![],
        };
        let json = ccnm_core::protocol::payload::to_json(&report).unwrap();
        let mut cases = String::new();
        for alias in live {
            cases.push_str(&format!(
                "case \"$args\" in *' {alias} '*agent-status*) printf '%s' '{json}'; exit 0;; esac\n"
            ));
        }
        let script = format!(
            "#!/bin/sh\nargs=\" $* \"\nfor a in \"$@\"; do case \"$a\" in *.invalid) echo \"$a\" >> '{log}';; esac; done\n{cases}echo 'ssh: Could not resolve hostname: nodename nor servname provided' >&2\nexit 255\n",
            log = self.path().join("dialled").display()
        );
        let ssh = self.path().join("bin/ssh");
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_file(self.path().join("dialled"));
    }

    fn dialled(&self) -> Vec<String> {
        std::fs::read_to_string(self.path().join("dialled"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn remembered(&self) -> Option<String> {
        std::fs::read_to_string(self.state().join("ccnm/workspaces/xshun/on"))
            .ok()
            .map(|s| s.trim().to_string())
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn on_sends_the_command_to_the_machine_named_by_node_or_alias() {
    let m = machines("named");
    for on in ["hpsrv", "hpsrv.invalid"] {
        let out = m.ccnm(&["stop", "xshun", "--on", on]).output().unwrap();
        assert_eq!(out.status.code(), Some(20), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("ssh hpsrv.invalid true"),
            "{}",
            stderr(&out)
        );
        assert_eq!(m.dialled(), ["hpsrv.invalid"], "--on {on}");
        m.running_on(&[]);
    }
    // Without it: the project's own machine.
    let out = m.ccnm(&["stop", "xshun"]).output().unwrap();
    assert!(
        stderr(&out).contains("ssh fodelf.invalid true"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_old_flag_still_works_and_says_it_was_renamed() {
    let m = machines("renamed");
    let out = m
        .ccnm(&["stop", "xshun", "--agent", "hpsrv"])
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(err.contains("--agent is now --on"), "{err}");
    assert_eq!(m.dialled(), ["hpsrv.invalid"], "{err}");
}

#[test]
fn an_unknown_machine_is_refused_with_the_ones_that_exist() {
    let m = machines("unknown");
    let out = m
        .ccnm(&["stop", "xshun", "--on", "nowhere"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::InvalidArgs.exit_code()),
        "{}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(err.contains("no machine called nowhere"), "{err}");
    assert!(
        err.contains("agent (fodelf.invalid)") && err.contains("hpsrv (hpsrv.invalid)"),
        "{err}"
    );
    assert!(
        m.dialled().is_empty(),
        "nothing was dialled: {:?}",
        m.dialled()
    );
}

/// One project, one writer: with a session still running on its own
/// machine, starting it on another is the person's decision. A script is
/// told what to type, and nothing on either machine is touched.
#[test]
fn a_session_running_elsewhere_is_not_ended_for_a_script() {
    let m = machines("busy");
    m.running_on(&["fodelf.invalid"]);
    let out = m
        .ccnm(&["xshun", "--on", "hpsrv"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(
        out.status.code(),
        Some(ccnm_core::ErrorCode::Policy.exit_code()),
        "{err}"
    );
    assert!(err.contains("xshun is running on agent"), "{err}");
    assert!(err.contains("ccnm stop xshun --on agent"), "{err}");
    assert!(err.contains("ccnm xshun --on hpsrv"), "{err}");
    assert_eq!(m.dialled(), ["fodelf.invalid"], "only asked, never started");
    assert_eq!(m.remembered(), None);
}

/// Where the last session went is where the next command without `--on`
/// goes, while that session runs; once it has ended, the project is back on
/// its own machine and the memory is gone.
#[test]
fn the_last_machine_is_used_until_its_session_ends() {
    let m = machines("remembered");
    let record = m.state().join("ccnm/workspaces/xshun");
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(record.join("on"), "hpsrv\n").unwrap();

    let out = m.ccnm(&["stop", "xshun"]).output().unwrap();
    assert!(
        stderr(&out).contains("ssh hpsrv.invalid true"),
        "{}",
        stderr(&out)
    );

    // Starting: hpsrv is asked first, cannot answer, and the start goes to
    // the project's own machine.
    m.running_on(&[]);
    let out = m
        .ccnm(&["xshun", "--detached"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        err.contains("it last ran on hpsrv, which cannot be asked now"),
        "{err}"
    );
    assert_eq!(m.dialled(), ["hpsrv.invalid", "fodelf.invalid"], "{err}");
    assert_eq!(m.remembered(), None, "{err}");
}

#[test]
fn ws_add_on_an_existing_project_moves_only_its_machine() {
    let m = machines("ws-add");
    let before = std::fs::read_to_string(&m.config).unwrap();
    let elsewhere = m.path().join("bin");
    let out = m
        .ccnm(&["ws", "add", "xshun", "--on", "hpsrv.invalid"])
        .current_dir(&elsewhere)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let after = std::fs::read_to_string(&m.config).unwrap();
    assert_eq!(
        after,
        before.replace("agent_node = \"agent\"", "agent_node = \"hpsrv\""),
        "only the machine changed"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("workspaces.xshun.agent_node: agent -> hpsrv"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `ccnm init --agent` again with another alias, where the projects are,
/// adds that machine. It used to repoint `agent` -- every project's machine
/// -- at the new alias.
#[test]
fn init_again_with_another_alias_adds_a_machine() {
    let m = machines("init");
    let out = m
        .ccnm(&["init", "--agent", "second.invalid"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nodes.second-invalid.ssh = second.invalid"),
        "{stdout}"
    );
    assert!(
        stdout.contains("added AI machine second-invalid"),
        "{stdout}"
    );
    let config = ccnm_core::Config::load(&m.config).unwrap();
    assert_eq!(config.nodes["agent"].ssh.as_deref(), Some("fodelf.invalid"));
    assert_eq!(config.workspace("xshun").unwrap().agent_node(), "agent");

    // Once more: it is there already.
    let out = m
        .ccnm(&["init", "--agent", "second.invalid"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("already says that"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

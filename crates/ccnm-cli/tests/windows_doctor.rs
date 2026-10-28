//! What the Agent asks a Windows Runtime about itself, answered by the real
//! binary on Windows (P111): `runtime-audit`, which `ccnm doctor` shows row
//! by row, and `runtime-cleanup`, the Runtime's half of `ccnm cleanup`.
//! Compiled on Windows only; the Windows CI job runs it. What an account
//! with other groups, or a machine with the SSH agent running, looks like is
//! measured on the real machine (P112), not faked here.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Output};

use ccnm_core::cleanup::{
    CLEANUP_PROTOCOL, Done, Kind, Plan, RuntimeCleanupReport, RuntimeCleanupRequest, SessionEnd,
};
use ccnm_core::protocol::payload;
use ccnm_core::runtime::{AuditReport, AuditRequest};
use ccnm_core::safety::Severity;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("ccnm-p111-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "this = \"runtime\"\n[nodes.runtime]\n[nodes.agent]\nssh = \"work\"\n[workspaces.demo]\nagent_node = \"agent\"\nroot = \"{}\"\n",
                ccnm_core::reported_path(&root.canonicalize().unwrap()).display()
            ),
        )
        .unwrap();
        Fixture { dir }
    }

    /// `XDG_STATE_HOME`; ccnm keeps its own under `ccnm\` in it.
    fn state(&self) -> PathBuf {
        self.dir.join("state")
    }

    /// As sshd would start it: its own profile in place of the account's,
    /// and the rest of the environment sshd builds for a session (the list
    /// below is what the Win11 test machine's sshd sets). With only the few
    /// variables a program cannot start without, the scan of running servers
    /// took 20-30 s here; with these it still took 25.6 s, so the environment
    /// is not why the runner is slow (0.3 s under a real sshd).
    fn ask(&self, subcommand: &str, wire: &str) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", subcommand, "--payload", wire])
            .env_clear()
            .env("USERPROFILE", self.dir.join("home"))
            .env("XDG_STATE_HOME", self.state())
            .env("CCNM_CONFIG", self.dir.join("config.toml"));
        for name in [
            "SystemRoot",
            "SystemDrive",
            "windir",
            "PATH",
            "PATHEXT",
            "TEMP",
            "TMP",
            "ComSpec",
            "APPDATA",
            "LOCALAPPDATA",
            "ProgramData",
            "ALLUSERSPROFILE",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
            "CommonProgramW6432",
            "PSModulePath",
            "PUBLIC",
            "USERNAME",
            "USERDOMAIN",
            "COMPUTERNAME",
            "HOMEDRIVE",
            "HOMEPATH",
            "OS",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn answer<T: serde::de::DeserializeOwned>(out: &Output) -> T {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Every finding has a verdict reached on Windows -- none of the
/// placeholders that said "not checked on Windows yet" -- and the groups
/// were read (no "cannot list"). In a shared account nothing refuses. A
/// key in `.ssh` is found by opening it, and sshd's shell is reported.
#[test]
fn the_audit_answers_every_row_on_windows() {
    let f = Fixture::new("audit");
    std::fs::create_dir_all(f.dir.join("home/.ssh")).unwrap();
    std::fs::write(f.dir.join("home/.ssh/id_ed25519"), "synthetic, not a key").unwrap();
    std::fs::write(f.dir.join("home/.ssh/known_hosts"), "").unwrap();

    let wire = payload::encode(&AuditRequest::new("demo")).unwrap();
    let report: AuditReport = answer(&f.ask("runtime-audit", &wire));
    let findings = &report.audit.findings;
    let said = format!("{findings:#?}");
    assert!(report.audit.shared_account, "{said}");
    for check in [
        "Runtime user",
        "Not elevated",
        "Not an admin",
        "No SSH keys",
        "No SSH agent",
        "No Claude credential",
        "No authentication environment",
        "No Docker access",
    ] {
        assert!(findings.iter().any(|f| f.check == check), "{check}: {said}");
    }
    assert!(!findings.iter().any(|f| f.check == "No sudo"), "{said}");
    for finding in findings {
        assert_ne!(finding.severity, Severity::Fail, "{said}");
        for placeholder in [
            "not checked on Windows yet",
            "not written for Windows",
            "cannot",
        ] {
            assert!(!finding.detail.contains(placeholder), "{said}");
        }
    }
    let keys = findings.iter().find(|f| f.check == "No SSH keys").unwrap();
    assert_eq!(keys.severity, Severity::Warn, "{said}");
    assert!(keys.detail.contains("accessible"), "{said}");

    assert!(report.ssh_shell.is_some(), "{report:?}");
    assert!(report.root.present && report.root.is_dir, "{report:?}");
}

/// The Runtime's half of `ccnm cleanup`: an ended session's output is
/// offered for removal -- which takes knowing no mcp-serve serves it, the
/// question Windows answers through Win32_Process (P110) -- and removed
/// when the Agent applies the preview.
#[test]
fn cleanup_removes_an_ended_sessions_output() {
    let f = Fixture::new("cleanup");
    let id = ccnm_core::session::new_id();
    let output = f
        .state()
        .join("ccnm")
        .join("sessions")
        .join(&id)
        .join("output");
    std::fs::create_dir_all(output.join("c1")).unwrap();
    std::fs::write(output.join("c1/stdout"), "done\n").unwrap();

    let ask = |apply| {
        let request = RuntimeCleanupRequest {
            protocol: CLEANUP_PROTOCOL,
            workspace: "demo".into(),
            node: "agent".into(),
            sessions: vec![SessionEnd {
                id: id.clone(),
                ended: true,
            }],
            apply,
        };
        let report: RuntimeCleanupReport =
            answer(&f.ask("runtime-cleanup", &payload::encode(&request).unwrap()));
        report
    };
    // How long the Runtime takes to say which sessions are served (the
    // PowerShell scan inside it), next to a PowerShell that only starts and
    // exits: on this runner the scan has taken 20-30 s, against 0.3 s on a
    // real machine, and which half is slow is not known yet.
    let bare = std::time::Instant::now();
    let _ = Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "exit",
        ])
        .output();
    eprintln!("a bare powershell took {:?}", bare.elapsed());
    let asked = std::time::Instant::now();
    let preview = ask(None);
    eprintln!("cleanup preview took {:?}", asked.elapsed());
    let [item] = &preview.items[..] else {
        panic!("{preview:?}");
    };
    assert_eq!(item.kind, Kind::RuntimeOutput, "{preview:?}");
    assert_eq!(item.plan, Plan::Remove, "{preview:?}");
    assert!(item.bytes > 0, "{preview:?}");

    let applied = ask(Some(preview.items.clone()));
    assert_eq!(applied.items[0].done, Some(Done::Removed), "{applied:?}");
    assert!(!output.exists());
}

//! `exec_command` inside Codex's workspace-write OS sandbox (P33).
//!
//! Opt-in per workspace (`exec_sandbox = "codex"` in the Runtime's own
//! config). The wrapper is
//!
//! ```text
//! <codex_bin> sandbox --sandbox-state-json <state> -- <program> <args...>
//! ```
//!
//! run from the same cwd, with the same cleaned environment and under the
//! same timeout as the bare command would be. On macOS the sandboxed child
//! stays in the wrapper's process group, so the watchdog's kill reaches it
//! directly; on Linux bubblewrap puts it in a session of its own, and what
//! reaches it is the chain the wrapper set up -- `--die-with-parent` plus a
//! pid namespace whose init is bwrap's child, so killing the wrapper's group
//! takes the whole namespace with it (measured both ways: toexec
//! `evidence/v2-p/p33-sandbox`, and the timeout case of the real-Codex test
//! in `crates/ccnm-cli/tests/exec_sandbox.rs`).
//!
//! The permission profile is Codex 0.154.0's own workspace-write object,
//! verbatim -- the one it sends for its own commands, captured in
//! `tests/fixtures/codex-0.154.0/exec-server/process-start-workspace-write.json`
//! and pinned by a test here: writes under the workspace root except
//! `.git`, under `$TMPDIR` and `/tmp`; no network; reads unrestricted. Not
//! widened and not narrowed. Every other shape would need measuring, and
//! `.git` read-only is the rule `apply_patch` already applies: a writable
//! `.git` lets a command plant a hook that runs unsandboxed the next time a
//! person runs git.
//!
//! What this does not do is tell "the sandbox refused it" from "the
//! command failed": both come back as the command's exit code and its
//! stderr (`Operation not permitted` under Seatbelt, `Read-only file
//! system` under bubblewrap), and Codex itself only guesses from the text. Every result therefore says the sandbox was on, so a refusal
//! can be read for what it is. The wrapper failing to start, and a program
//! that is not there, are still refused as errors, not reported as results.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::ExecSandbox;
use crate::error::{Error, Result};
use crate::native::serve::CodexHome;
use crate::process::{Cmd, ProcessRunner};

/// What [`Sandbox::resolve`] runs through the sandbox once, to see it work.
pub const PROBE: [&str; 3] = ["/bin/sh", "-c", "exit 0"];

/// The line every `exec_command` result carries while the sandbox is on.
pub const NOTE: &str = "sandboxed: this command could write only inside the workspace (not .git), $TMPDIR and /tmp, and had no network; a refusal shows as `Operation not permitted` or `Read-only file system`";

/// One workspace's sandbox, resolved once when the MCP server starts.
pub struct Sandbox {
    codex_bin: PathBuf,
    /// Canonical workspace root: the one writable root.
    root: PathBuf,
    /// The wrapper's private CODEX_HOME; removed with the server.
    home: CodexHome,
}

impl Sandbox {
    /// This workspace's sandbox, if its config asks for one.
    ///
    /// Fails closed: a workspace that asks for a sandbox this Runtime cannot
    /// provide -- no `codex_bin`, the wrong Codex, nowhere to put a
    /// CODEX_HOME -- refuses the session rather than running commands bare.
    pub fn resolve(
        config: &crate::Config,
        workspace: &str,
        root: &Path,
        state: Option<&Path>,
        session: &str,
        runner: &dyn ProcessRunner,
    ) -> Result<Option<Sandbox>> {
        let Some(ws) = config.workspaces.get(workspace) else {
            return Ok(None);
        };
        match ws.exec_sandbox {
            ExecSandbox::Off => return Ok(None),
            ExecSandbox::Codex => {}
        }
        let codex_bin = config
            .nodes
            .get(&ws.runtime_node)
            .and_then(|node| node.codex_bin.clone())
            .ok_or_else(|| {
                Error::config(format!(
                    "nodes.{}.codex_bin is not set; exec_sandbox = \"codex\" needs this Runtime to name its Codex binary",
                    ws.runtime_node
                ))
            })?;
        let Some(state) = state else {
            return Err(Error::config(
                "exec_sandbox = \"codex\" needs a state directory on the Runtime Node for the sandbox's CODEX_HOME, and ccnm cannot find one",
            ));
        };
        let home = CodexHome::create_in(
            state,
            "exec-sandbox",
            &format!("{session}-{}", crate::session::new_id()),
        )?;
        let base = crate::safety::environment::runtime_child(Cmd::new(&codex_bin).cwd(root))
            .env("CODEX_HOME", home.path());
        crate::provider::codex::check_measured(&base, runner, "the exec_command sandbox")?;
        let sandbox = Sandbox {
            codex_bin,
            root: root.to_path_buf(),
            home,
        };
        sandbox.probe(runner)?;
        Ok(Some(sandbox))
    }

    /// One empty command through the sandbox before the session is
    /// accepted. Everything this catches would otherwise surface as the
    /// model's *command* failing: bubblewrap missing or refused a user
    /// namespace, a CODEX_HOME Codex will not put its helper in (it refuses
    /// temporary directories -- measured on Linux, where the first command
    /// then died with `bwrap: execvp codex-linux-sandbox`), a wrapper that
    /// cannot start. It proves the wrapper runs, not that it confines; the
    /// confinement is what P33 measured and the real-Codex test checks.
    /// Cost: one wrapper round trip, 15--40 ms.
    fn probe(&self, runner: &dyn ProcessRunner) -> Result<()> {
        let probe = crate::safety::environment::runtime_child(
            Cmd::new(PROBE[0])
                .args(&PROBE[1..])
                .cwd(&self.root)
                .timeout(Duration::from_secs(20)),
        );
        let out = runner.run(&self.wrap(probe, &self.root))?;
        if out.exit_code == Some(0) {
            if !out.stderr.is_empty() {
                tracing::warn!(stderr = %String::from_utf8_lossy(&out.stderr).trim(), "codex sandbox ran the probe but complained");
            }
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail = stderr.trim();
        let tail = &tail[tail.len().saturating_sub(600)..];
        Err(Error::dependency(format!(
            "the exec_command sandbox does not work on this Runtime: codex sandbox exited {} running `sh -c 'exit 0'`{}{}",
            match out.exit_code {
                Some(code) => code.to_string(),
                None if out.timed_out => "on timeout".to_string(),
                None => "on a signal".to_string(),
            },
            if tail.is_empty() { "" } else { "\n" },
            tail
        )))
    }

    /// `cmd`, run inside the sandbox: program and arguments move behind the
    /// wrapper, cwd, environment and timeout stay where they were.
    pub fn wrap(&self, mut cmd: Cmd, cwd: &Path) -> Cmd {
        let mut args: Vec<OsString> = vec![
            "sandbox".into(),
            "--sandbox-state-json".into(),
            state_json(&self.root, cwd).into(),
            "--".into(),
            std::mem::take(&mut cmd.program),
        ];
        args.append(&mut cmd.args);
        cmd.program = self.codex_bin.clone().into_os_string();
        cmd.args = args;
        cmd.env("CODEX_HOME", self.home.path())
    }

    pub fn codex_bin(&self) -> &Path {
        &self.codex_bin
    }
}

/// The `--sandbox-state-json` value: the profile, the one writable root,
/// and where the command runs.
pub fn state_json(root: &Path, cwd: &Path) -> String {
    json!({
        "permissionProfile": permission_profile(),
        "sandboxCwd": file_uri(cwd),
        "workspaceRoots": [file_uri(root)],
    })
    .to_string()
}

/// Codex 0.154.0's workspace-write permissions, as captured in P21. The
/// `special` entries are resolved by `codex sandbox` itself against
/// `workspaceRoots`, `$TMPDIR` and `/tmp`; nothing here is an absolute
/// path.
pub fn permission_profile() -> Value {
    let special = |kind: &str| json!({"type": "special", "value": {"kind": kind}});
    let under = |subpath: &str| json!({"type": "special", "value": {"kind": "project_roots", "subpath": subpath}});
    json!({
        "type": "managed",
        "file_system": {
            "type": "restricted",
            "entries": [
                {"path": special("root"), "access": "read"},
                {"path": special("project_roots"), "access": "write"},
                {"path": special("slash_tmp"), "access": "write"},
                {"path": special("tmpdir"), "access": "write"},
                {"path": under(".git"), "access": "read", "missing_path_behavior": "skip"},
                {"path": under(".agents"), "access": "read", "missing_path_behavior": "skip"},
                {"path": under(".codex"), "access": "read", "missing_path_behavior": "skip"},
            ],
        },
        "network": "restricted",
    })
}

/// `file://` plus the path, percent-encoded the way a `file:` URL is: the
/// executor parses these with a URL parser, and a space or a non-ASCII
/// byte left raw would be read as something else or refused.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Where `execvp` would find `program` from `cwd`, if anywhere.
///
/// Inside the wrapper a program that is not there fails in the sandbox
/// launcher (`sandbox-exec: execvp() ... failed`, exit 71 on macOS) and
/// would come back looking like a command result. Resolving it first keeps
/// the answer what the bare path gives: a dependency error.
pub fn locate(program: &str, cwd: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    if program.contains('/') {
        let candidate = cwd.join(program);
        return candidate.is_file().then_some(candidate);
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured_profile() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../tests/fixtures/codex-0.154.0/exec-server/process-start-workspace-write.json",
        );
        let text = std::fs::read_to_string(path).unwrap();
        let message: Value = serde_json::from_str(&text).unwrap();
        message["params"]["sandbox"]["permissions"].clone()
    }

    /// The profile is the object Codex sent, field for field. Change one
    /// and this is the test that says the shape is no longer the measured
    /// one.
    #[test]
    fn the_profile_is_the_one_codex_sends_for_its_own_commands() {
        assert_eq!(permission_profile(), captured_profile());
    }

    #[test]
    fn the_state_names_the_root_as_the_only_writable_root_and_the_cwd() {
        let state: Value =
            serde_json::from_str(&state_json(Path::new("/w/proj"), Path::new("/w/proj/src")))
                .unwrap();
        assert_eq!(state["workspaceRoots"], json!(["file:///w/proj"]));
        assert_eq!(state["sandboxCwd"], json!("file:///w/proj/src"));
        assert_eq!(state["permissionProfile"], captured_profile());
    }

    #[test]
    fn a_path_with_a_space_or_a_non_ascii_name_is_percent_encoded() {
        assert_eq!(
            file_uri(Path::new("/w/my proj/子目录")),
            "file:///w/my%20proj/%E5%AD%90%E7%9B%AE%E5%BD%95"
        );
    }

    #[test]
    fn the_wrapper_moves_program_and_args_behind_codex_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("ccnm-sandbox-wrap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sandbox = Sandbox {
            codex_bin: PathBuf::from("/opt/codex/bin/codex"),
            root: PathBuf::from("/w/proj"),
            home: CodexHome::create_in(&dir, "exec-sandbox", "s").unwrap(),
        };
        let cmd = Cmd::new("cargo")
            .args(["test", "--lib"])
            .cwd("/w/proj/crates/a")
            .env("KEPT", "1")
            .timeout(std::time::Duration::from_secs(7));
        let wrapped = sandbox.wrap(cmd, Path::new("/w/proj/crates/a"));
        assert_eq!(wrapped.program, OsString::from("/opt/codex/bin/codex"));
        let args: Vec<&OsStr> = wrapped.args.iter().map(OsString::as_os_str).collect();
        assert_eq!(args[0], "sandbox");
        assert_eq!(args[1], "--sandbox-state-json");
        let state: Value = serde_json::from_str(args[2].to_str().unwrap()).unwrap();
        assert_eq!(state["sandboxCwd"], json!("file:///w/proj/crates/a"));
        assert_eq!(&args[3..], ["--", "cargo", "test", "--lib"]);
        assert_eq!(wrapped.cwd.as_deref(), Some(Path::new("/w/proj/crates/a")));
        assert_eq!(wrapped.timeout, std::time::Duration::from_secs(7));
        assert!(wrapped.env.iter().any(|(k, v)| k == "KEPT" && v == "1"));
        let home = wrapped
            .env
            .iter()
            .find(|(k, _)| k == "CODEX_HOME")
            .map(|(_, v)| PathBuf::from(v))
            .unwrap();
        assert!(
            home.starts_with(dir.join("exec-sandbox")),
            "{}",
            home.display()
        );
        drop(sandbox);
        assert!(!home.exists(), "the CODEX_HOME goes with the sandbox");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locate_finds_programs_the_way_execvp_does() {
        let dir = std::env::temp_dir().join(format!("ccnm-sandbox-locate-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tool"), "").unwrap();
        std::fs::write(dir.join("run.sh"), "").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(locate("tool", &dir, Some(&path)), Some(bin.join("tool")));
        assert_eq!(
            locate("./run.sh", &dir, Some(&path)),
            Some(dir.join("./run.sh"))
        );
        assert_eq!(locate("missing", &dir, Some(&path)), None);
        assert_eq!(locate("./missing.sh", &dir, Some(&path)), None);
        assert_eq!(locate("tool", &dir, None), None);
        // A directory is not a program.
        assert_eq!(locate("bin", &dir, Some(dir.as_os_str())), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

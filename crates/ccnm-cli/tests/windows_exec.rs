//! Running commands on a Windows Runtime (P110), through the real binary
//! in an external coding session: how a program is found and started,
//! `shell` in Git Bash, a timeout ending the whole tree, background
//! commands, a server killed while one runs, and an MCP server behind a
//! `.cmd` wrapper. Compiled on Windows only; the Windows CI job runs it.
#![cfg(windows)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use ccnm_core::protocol::payload;
use ccnm_core::runtime::{ExternalMode, ExternalOpenPayload};
use serde_json::{Value, json};

struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-win-exec-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("proj");
        for d in [root.join("scripts"), dir.join("home"), dir.join("state")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let write = |rel: &str, text: &str| std::fs::write(root.join(rel), text).unwrap();
        write("scripts/hello.cmd", "@echo off\r\necho hello %~1 %~2\r\n");
        write(
            "scripts/hi.ps1",
            "Write-Output \"ps:$($args -join ',')\"\r\n",
        );
        // An MCP server behind a `.cmd`, the way `npx` is one, named in
        // .mcp.json without its extension.
        write("fake-mcp.cmd", "@node \"%~dp0fake-mcp.js\"\r\n");
        write(
            "fake-mcp.js",
            r#"const rl = require('readline').createInterface({ input: process.stdin });
rl.on('line', (line) => {
  let m;
  try { m = JSON.parse(line); } catch (e) { return; }
  if (m.id === undefined) return;
  const reply = (result) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: m.id, result }) + '\n');
  if (m.method === 'initialize') reply({ protocolVersion: '2024-11-05', serverInfo: { name: 'fake', version: '1' }, capabilities: { tools: {} } });
  else if (m.method === 'tools/list') reply({ tools: [{ name: 'echo', inputSchema: { type: 'object' } }] });
  else if (m.method === 'tools/call') reply({ content: [{ type: 'text', text: 'echoed ' + JSON.stringify(m.params.arguments || {}) }] });
});
"#,
        );
        write(
            ".mcp.json",
            r#"{ "mcpServers": { "fake": { "command": "./fake-mcp" } } }"#,
        );
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{}"
agent_node = "agent"
external_mcp = "coding"
allow_unconfined_exec = true
"#,
                ccnm_core::reported_path(&root).display()
            ),
        )
        .unwrap();
        Fixture { dir, config, root }
    }

    fn serve(&self) -> Command {
        let wire = payload::encode(&ExternalOpenPayload::new(
            "demo",
            "win-exec",
            ExternalMode::Coding,
        ))
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", "mcp-serve", "--payload", &wire])
            .env_clear()
            .env("HOME", ccnm_core::reported_path(&self.dir.join("home")))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", &self.config);
        // What a Windows program needs to start, Git Bash among them (it is
        // found through ProgramFiles), and node.
        for name in [
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "SystemDrive",
            "windir",
            "TEMP",
            "TMP",
            "ComSpec",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "APPDATA",
            "LOCALAPPDATA",
            "USERPROFILE",
            "USERNAME",
            "PROCESSOR_ARCHITECTURE",
            "NUMBER_OF_PROCESSORS",
            "OS",
        ] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Session {
    fn start(fixture: &Fixture) -> Session {
        let mut child = fixture
            .serve()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut session = Session {
            child,
            stdin,
            stdout,
            id: 0,
        };
        session.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "windows-exec-test", "version": "0"}
            }),
        );
        let stdin = session.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        stdin.flush().unwrap();
        session
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "server closed stdout while waiting for {method}");
            let message: Value = serde_json::from_str(line.trim()).unwrap();
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(message.get("error").is_none(), "{method}: {message}");
                return message["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": tool, "arguments": args}))
    }

    /// Close stdin and wait for the server; how long it took.
    fn shutdown(mut self) -> Duration {
        let started = Instant::now();
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
        started.elapsed()
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn ok(result: &Value) -> String {
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "the call failed: {result}"
    );
    text(result)
}

/// `output_ref r-…` out of a result's text.
fn output_ref(said: &str) -> String {
    let at = said
        .find("r-")
        .unwrap_or_else(|| panic!("no output_ref in {said}"));
    said[at..]
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .next()
        .unwrap()
        .to_string()
}

/// `cmd` the way a person typing it in cmd.exe expects (P110.2): a name
/// with no extension finds the `.cmd` beside it or on PATH, and a `.ps1`
/// runs under PowerShell.
#[test]
fn cmd_finds_batch_files_and_powershell_scripts_the_way_windows_does() {
    let fixture = Fixture::new("cmd");
    let mut session = Session::start(&fixture);

    let said = ok(&session.call(
        "exec_command",
        json!({"cmd": ["scripts/hello", "world", "a b"]}),
    ));
    assert!(said.contains("hello world a b"), "{said}");
    assert!(said.contains("ok in"), "{said}");

    let said = ok(&session.call(
        "exec_command",
        json!({"cmd": ["scripts/hi.ps1", "x", "y z"]}),
    ));
    assert!(said.contains("ps:x,y z"), "{said}");

    // On PATH: npm is npm.cmd on a Windows machine with Node.
    let said = ok(&session.call("exec_command", json!({"cmd": ["npm", "--version"]})));
    assert!(said.contains("ok in"), "{said}");

    let said = session.call("exec_command", json!({"cmd": ["no-such-program-p110"]}));
    assert!(text(&said).contains("not installed"), "{said}");
    session.shutdown();
}

/// `shell` is Git Bash (design D2): bash syntax works, `SHELL` names that
/// bash rather than the account's default shell, the two streams stay
/// apart, the exit code comes back, and UTF-8 survives.
#[test]
fn shell_runs_in_git_bash() {
    let fixture = Fixture::new("shell");
    let mut session = Session::start(&fixture);
    let result = session.call(
        "exec_command",
        json!({"shell": "echo \"shell=$SHELL\"; [[ -n \"$BASH_VERSION\" ]] && echo bash; printf '%s|' 'a b' 'c\"d'; echo; echo 中文; echo err >&2; exit 3"}),
    );
    let said = text(&result);
    assert!(said.contains("exit 3 in"), "{said}");
    assert!(said.contains("\nbash\n"), "{said}");
    assert!(said.contains("a b|c\"d|"), "{said}");
    assert!(said.contains("中文"), "{said}");
    let shell_line = said
        .lines()
        .find(|l| l.starts_with("shell="))
        .unwrap_or_else(|| panic!("{said}"));
    assert!(shell_line.ends_with("bash.exe"), "{shell_line}");
    let stderr = said.split("--- stderr\n").nth(1).unwrap_or_default();
    assert!(
        stderr.starts_with("err"),
        "stderr is its own stream: {said}"
    );
    session.shutdown();
}

/// A timeout ends the command and what it started (`taskkill /T /F`,
/// design D3): the call comes back at the deadline, not when a grandchild
/// would have finished. Both shapes run before anything is asserted, and a
/// slow one comes with the process tree as it was two seconds after the
/// deadline -- which processes were left, and whose children they were.
#[test]
fn a_timeout_ends_the_whole_tree() {
    let fixture = Fixture::new("timeout");
    let mut session = Session::start(&fixture);
    let mut failures = Vec::new();
    for args in [
        json!({"shell": "sleep 30 & sleep 30; echo never-reached", "timeout_ms": 2000}),
        json!({"cmd": ["cmd", "/c", "ping -n 30 127.0.0.1 >nul"], "timeout_ms": 2000}),
    ] {
        let snapshot = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(4));
            process_tree()
        });
        let started = Instant::now();
        let said = text(&session.call("exec_command", args.clone()));
        let took = started.elapsed();
        let tree = snapshot.join().unwrap();
        let ended = said.contains("timed out")
            && !said.contains("\nnever-reached")
            && took < Duration::from_secs(10);
        if !ended {
            failures.push(format!(
                "{args}: took {took:?}\n{said}\nprocesses 2 s after the deadline:\n{tree}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    session.shutdown();
}

/// pid, parent pid and name of the processes a command here could start.
fn process_tree() -> String {
    let script = "Get-CimInstance Win32_Process | Where-Object { $_.Name -in 'bash.exe','sleep.exe','ping.exe','cmd.exe','ccnm.exe' } | ForEach-Object { '{0} {1} {2}' -f $_.ProcessId, $_.ParentProcessId, $_.Name }";
    // No console and no stdin of ours: a PowerShell attached to the test's
    // console left it unable to print its own failure (CI run 40527946104
    // stopped right after `failures:`).
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("powershell")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => format!("(cannot list processes: {e})"),
    }
}

/// Background commands: `read_output` waits for one to finish and says
/// how it ended, `stop_command` stops one, and a session that ends stops
/// what is still running -- promptly.
#[test]
fn background_commands_report_stop_and_end_with_the_session() {
    let fixture = Fixture::new("background");
    let mut session = Session::start(&fixture);

    let started = ok(&session.call(
        "exec_command",
        json!({"shell": "for i in 1 2 3; do echo tick $i; sleep 1; done", "run_in_background": true}),
    ));
    let reference = output_ref(&started);
    let read = ok(&session.call(
        "read_output",
        json!({"output_ref": reference, "wait_ms": 20000}),
    ));
    assert!(read.contains("tick 3"), "{read}");
    assert!(read.contains("exited 0"), "{read}");

    let started = ok(&session.call(
        "exec_command",
        json!({"cmd": ["ping", "-n", "60", "127.0.0.1"], "run_in_background": true}),
    ));
    let reference = output_ref(&started);
    let begun = Instant::now();
    let stopped = ok(&session.call("stop_command", json!({"output_ref": reference})));
    assert!(stopped.contains("stopped by stop_command"), "{stopped}");
    assert!(begun.elapsed() < Duration::from_secs(10), "{stopped}");

    ok(&session.call(
        "exec_command",
        json!({"shell": "sleep 60", "run_in_background": true}),
    ));
    let took = session.shutdown();
    assert!(
        took < Duration::from_secs(15),
        "the session took {took:?} to end around a running command"
    );
}

/// The server killed outright while a background command runs: its stdout
/// -- the ssh channel, in real use -- must still close, or the Agent's side
/// waits for a command nobody is watching. It would not without
/// `keep_std_handles_to_this_process` (P110.1).
#[test]
fn a_killed_server_does_not_keep_its_channel_open() {
    let fixture = Fixture::new("killed");
    let mut session = Session::start(&fixture);
    ok(&session.call(
        "exec_command",
        json!({"shell": "sleep 20", "run_in_background": true}),
    ));
    ok(&session.call(
        "exec_command",
        json!({"cmd": ["ping", "-n", "20", "127.0.0.1"], "run_in_background": true}),
    ));
    let started = Instant::now();
    session.child.kill().unwrap();
    let mut rest = Vec::new();
    session.stdout.read_to_end(&mut rest).unwrap();
    let took = started.elapsed();
    let _ = session.child.wait();
    assert!(
        took < Duration::from_secs(5),
        "the channel stayed open {took:?} after the server was killed"
    );
}

/// `call_mcp_tool` starts a server named without its extension that is a
/// `.cmd` wrapper around node -- the shape `npx` has on Windows (P110.4).
#[test]
fn an_mcp_server_behind_a_cmd_wrapper_is_relayed() {
    let fixture = Fixture::new("relay");
    let mut session = Session::start(&fixture);
    let tools: Vec<String> = session.rpc("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        tools.iter().any(|t| t == "call_mcp_tool"),
        "{tools:?} (root {})",
        fixture.root.display()
    );
    let said = ok(&session.call(
        "call_mcp_tool",
        json!({"server": "fake", "tool": "echo", "arguments": {"x": 1}}),
    ));
    assert!(said.contains("echoed"), "{said}");
    let took = session.shutdown();
    assert!(took < Duration::from_secs(15), "{took:?}");
}

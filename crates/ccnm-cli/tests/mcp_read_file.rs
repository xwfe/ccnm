//! `read_file` over the real wire.
//!
//! The unit tests in `ccnm-core` cover the reading logic; this file covers
//! the part they cannot see. It spawns the actual binary as
//! `ccnm internal mcp-serve`, speaks newline-delimited JSON-RPC to its
//! stdin and stdout exactly as Claude Code's client does, and asserts on
//! the bytes that come back: that a refusal arrives as `isError` with a
//! `CCNM_E_*` first line rather than as a protocol error the model never
//! sees, that no absolute path of this machine appears in any response,
//! and that stdout carries nothing but JSON-RPC.
//!
//! Every request runs against one long-lived server, because that is how
//! it runs in production: a single ssh, a single process (design doc
//! section 27).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ccnm_core::protocol::mcp::ServePayload;
use ccnm_core::protocol::payload;
use serde_json::{Value, json};

/// A live MCP session over pipes.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
    done: Arc<AtomicBool>,
    /// Every line stdout produced, kept so the test can prove none of them
    /// was a stray `println!`.
    lines: Vec<String>,
}

impl Session {
    fn start(root: &Path) -> Session {
        let wire = payload::encode(&ServePayload::new("t", root.to_path_buf(), "s1")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_ccnm"))
            .args(["internal", "mcp-serve", "--payload", &wire])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        // A regression that makes the server block (opening a fifo, say)
        // would otherwise hang the whole test run with no output. Killing
        // the child turns that into a failed read with a message.
        let done = Arc::new(AtomicBool::new(false));
        let pid = child.id();
        let flag = Arc::clone(&done);
        std::thread::spawn(move || {
            for _ in 0..300 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if flag.load(Ordering::SeqCst) {
                    return;
                }
            }
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        });

        let mut session = Session {
            child,
            stdin,
            stdout,
            id: 0,
            done,
            lines: Vec::new(),
        };
        session.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cli-test", "version": "0"}
            }),
        );
        session.notify("notifications/initialized");
        session
    }

    fn notify(&mut self, method: &str) {
        writeln!(self.stdin, r#"{{"jsonrpc":"2.0","method":"{method}"}}"#).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Send one request and return its `result`, skipping anything the
    /// server sends in between.
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "server closed stdout while waiting for {method}");
            let message: Value = serde_json::from_str(line.trim())
                .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC: {line:?} ({e})"));
            self.lines.push(line.trim().to_string());
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(
                    message.get("error").is_none(),
                    "{method} came back as a protocol error: {message}"
                );
                return message["result"].clone();
            }
        }
    }

    fn read_file(&mut self, args: Value) -> Value {
        self.call("read_file", args)
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": tool, "arguments": args}))
    }

    fn shutdown(mut self) {
        self.done.store(true, Ordering::SeqCst);
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
    }
}

/// The text a model actually reads out of a tool result.
fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_string()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

/// A workspace with the files that make `read_file` interesting.
fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ccnm-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    let long: String = (1..=500).map(|n| format!("line {n}\n")).collect();
    std::fs::write(root.join("long.txt"), long).unwrap();
    std::fs::write(root.join("binary.bin"), b"\x00\x01\x02ELF".as_slice()).unwrap();
    // The file the whole path policy exists for: a secret outside the
    // workspace, and a symlink inside it that points at the secret.
    std::fs::write(dir.join("secret.txt"), "TOTALLY-SECRET-VALUE\n").unwrap();
    std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("shortcut.txt")).unwrap();
    std::fs::canonicalize(&root).unwrap()
}

#[test]
fn read_file_serves_a_whole_session_over_one_process() {
    let root = workspace("session");
    let mut s = Session::start(&root);

    // tools/list advertises read_file, inside the schema budget, and with
    // a schema that does not invite start_line = 0.
    let list = s.rpc("tools/list", json!({}));
    let mut names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["list_files", "read_file", "workspace_info"]);
    let list_bytes = serde_json::to_string(&list).unwrap().len();
    assert!(list_bytes < 16 * 1024, "schema budget: {list_bytes} bytes");
    let read_tool = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "read_file")
        .unwrap();
    let schema = &read_tool["inputSchema"];
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(schema["properties"]["start_line"]["minimum"], json!(1));
    assert_eq!(schema["properties"]["max_lines"]["maximum"], json!(2000));

    // A normal read: numbered lines, a footer, bounded metadata.
    let ok = s.read_file(json!({"path": "src/main.rs"}));
    assert!(!is_error(&ok));
    assert_eq!(
        text(&ok),
        "1\u{2192}fn main() {\n2\u{2192}    println!(\"hi\");\n3\u{2192}}\n[end of file, 3 lines]"
    );
    let meta = &ok["structuredContent"];
    assert_eq!(meta["path"], "src/main.rs");
    assert_eq!(meta["total_lines"], 3);
    assert_eq!(meta["truncated"], false);
    assert!(
        meta.get("text").is_none(),
        "the body must not be sent twice: {meta}"
    );

    // Truncation hands back a line to resume from, and resuming works.
    let cut = s.read_file(json!({"path": "long.txt"}));
    assert_eq!(cut["structuredContent"]["lines"], 200);
    assert_eq!(cut["structuredContent"]["next_start_line"], 201);
    assert!(text(&cut).ends_with("continue with start_line=201]"));
    let rest = s.read_file(json!({"path": "long.txt", "start_line": 201}));
    assert!(text(&rest).starts_with("201\u{2192}line 201\n"));

    // Every refusal is a result the model can read, not a protocol error,
    // and each one carries its stable code.
    for (args, code) in [
        (json!({"path": "shortcut.txt"}), "CCNM_E_POLICY"),
        (json!({"path": "../secret.txt"}), "CCNM_E_POLICY"),
        (json!({"path": "/etc/passwd"}), "CCNM_E_POLICY"),
        (json!({"path": "missing.txt"}), "CCNM_E_INVALID_ARGS"),
        (json!({"path": "src"}), "CCNM_E_INVALID_ARGS"),
        (json!({"path": "binary.bin"}), "CCNM_E_INVALID_ARGS"),
        (
            json!({"path": "src/main.rs", "start_line": 0}),
            "CCNM_E_INVALID_ARGS",
        ),
    ] {
        let result = s.read_file(args.clone());
        assert!(is_error(&result), "{args} should have been refused");
        let message = text(&result);
        assert!(
            message.starts_with(&format!("{code}: ")),
            "{args} -> {message}"
        );
    }

    // list_files navigates, and every path it hands back is one read_file
    // accepts -- that hand-off is the whole point of both tools.
    let root_listing = s.call("list_files", json!({}));
    assert!(!is_error(&root_listing));
    let shown = text(&root_listing);
    let listed: Vec<&str> = shown.lines().filter(|l| !l.starts_with('[')).collect();
    assert!(listed.contains(&"src/"), "{shown}");
    assert!(listed.contains(&"long.txt"), "{shown}");

    let globbed = s.call("list_files", json!({"glob": "**/*.rs"}));
    assert_eq!(text(&globbed).lines().next().unwrap(), "src/main.rs");
    let back = s.read_file(json!({"path": "src/main.rs"}));
    assert!(!is_error(&back), "a listed path must be readable");

    // A bad glob is refused with a code, not silently matched to nothing:
    // an empty answer would tell the model the files do not exist.
    let bad = s.call("list_files", json!({"glob": "src/[ab].rs"}));
    assert!(is_error(&bad));
    assert!(
        text(&bad).starts_with("CCNM_E_INVALID_ARGS: "),
        "{}",
        text(&bad)
    );

    // Listing cannot be used to walk out either.
    let out = s.call("list_files", json!({"path": ".."}));
    assert!(is_error(&out));
    assert!(text(&out).starts_with("CCNM_E_POLICY: "), "{}", text(&out));

    // Read-only, one process, and the whole session's calls counted.
    let info = s.rpc("tools/call", json!({"name": "workspace_info"}));
    assert_eq!(info["structuredContent"]["calls_served"], 16);
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        4,
        "the server created or removed files in the workspace"
    );

    // Nothing in the whole conversation leaked this machine's paths or the
    // file the symlink pointed at.
    let transcript = s.lines.join("\n");
    assert!(
        !transcript.contains("TOTALLY-SECRET-VALUE"),
        "the escaping symlink's target reached the client"
    );
    assert!(
        !transcript.contains(&root.display().to_string()),
        "an absolute workspace path reached the client"
    );
    assert!(!transcript.contains("/etc/passwd\n"), "{transcript}");

    s.shutdown();
}

/// A fifo in the workspace must never be opened: `open` on one blocks
/// until a writer appears, which on the single-threaded runtime would
/// freeze every later call in the session. The proof is that a second call
/// still answers afterwards.
#[test]
fn a_fifo_cannot_wedge_the_session() {
    let root = workspace("fifo");
    let made = Command::new("mkfifo")
        .arg(root.join("pipe"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(made, "mkfifo is needed for this test");

    let mut s = Session::start(&root);
    let refused = s.read_file(json!({"path": "pipe"}));
    assert!(is_error(&refused));
    assert!(text(&refused).contains("not a regular file"), "{refused}");
    // Still alive and still serving.
    let after = s.read_file(json!({"path": "src/main.rs"}));
    assert!(!is_error(&after));
    s.shutdown();
}

//! The seven read tools, end to end, on every platform the Runtime runs on
//! -- Windows included since P108, where this is the first proof that they
//! work at all.
//!
//! A real `ccnm internal mcp-serve` in an external read session, over
//! pipes. What only this file shows is the part that differs by platform:
//! every path that comes back is relative to the workspace and written
//! with `/`, the root is never in the text, and on Windows the names NTFS
//! reads as another file are refused by the tools, not only by the path
//! policy's unit tests. Since P109 it also holds the coding session's
//! writing, on all three.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use ccnm_core::protocol::payload;
use ccnm_core::runtime::{ExternalMode, ExternalOpenPayload};
use serde_json::{Value, json};

/// A project inside a git repository, one level down so `git_subdir` has
/// a separator in it, with something for each tool to read.
struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    /// Canonical, in this platform's own spelling.
    root: PathBuf,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("ccnm-read-tools-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        let root = repo.join("sub").join("proj");
        let home = dir.join("home");
        for d in [
            root.join("src/lib"),
            root.join(".claude/skills/hello"),
            home.join(".claude/skills/greet"),
            dir.join("state"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let write = |path: PathBuf, text: &[u8]| std::fs::write(path, text).unwrap();
        write(root.join("src/main.rs"), b"fn main() { needle(); }\n");
        write(root.join("src/lib/deep.rs"), b"pub fn needle() {}\n");
        write(root.join("README.md"), b"# demo\n");
        // Only the signature is looked at; nothing decodes it.
        write(root.join("pixel.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        write(
            root.join("nb.ipynb"),
            br#"{"cells":[{"cell_type":"code","id":"c1","metadata":{},"execution_count":null,"outputs":[],"source":["print('nb-marker')"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
        );
        write(
            root.join(".claude/skills/hello/SKILL.md"),
            b"---\nname: hello\ndescription: Says hello from the project\n---\nproject-skill-body\n",
        );
        write(
            root.join(".claude/skills/hello/notes.md"),
            b"project-skill-notes\n",
        );
        write(
            home.join(".claude/skills/greet/SKILL.md"),
            b"---\nname: greet\ndescription: Installed on this machine\n---\ninstalled-skill-body\n",
        );
        let git = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "-q"])
            .current_dir(&repo)
            .output()
            .expect("git is needed by this test, as by list_files");
        assert!(git.status.success(), "git init: {git:?}");

        // In the config as the Runtime writes a root (`C:/...` on Windows),
        // which TOML also reads without escapes.
        let spelled = ccnm_core::reported_path(&root);
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
"#,
                spelled.display()
            ),
        )
        .unwrap();
        Fixture { dir, config, root }
    }

    fn serve(&self, mode: ExternalMode) -> Command {
        let wire = payload::encode(&ExternalOpenPayload::new("demo", "read-tools", mode)).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ccnm"));
        cmd.args(["internal", "mcp-serve", "--payload", &wire])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CCNM_CONFIG", &self.config);
        // What a Windows process cannot do without, and sshd gives every
        // session: without SystemRoot git and rg fail to start.
        #[cfg(windows)]
        for name in [
            "SystemRoot",
            "SystemDrive",
            "PATHEXT",
            "TEMP",
            "TMP",
            "ComSpec",
        ] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd
    }

    /// The root as text, in each spelling it could leak in.
    fn root_spellings(&self) -> Vec<String> {
        let native = self.root.display().to_string();
        vec![
            native.clone(),
            native.trim_start_matches(r"\\?\").to_string(),
            ccnm_core::reported_path(&self.root).display().to_string(),
        ]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Session {
    fn start(mut command: Command) -> Session {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
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
                "clientInfo": {"name": "read-tools-test", "version": "0"}
            }),
        );
        writeln!(
            session.stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        session
    }

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
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(message.get("error").is_none(), "{method}: {message}");
                return message["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": tool, "arguments": args}))
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
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

#[cfg(windows)]
fn refusal(result: &Value) -> String {
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "the call should have been refused: {result}"
    );
    text(result)
}

/// No `\`, and none of the root's spellings: what a model reads is the
/// workspace's own relative paths and nothing of the machine.
fn workspace_only(fixture: &Fixture, tool: &str, said: &str) {
    assert!(
        !said.contains('\\'),
        "{tool} answered with a backslash: {said}"
    );
    for root in fixture.root_spellings() {
        assert!(
            !said.contains(&root),
            "{tool} leaked the root {root}: {said}"
        );
    }
}

#[test]
fn each_read_tool_answers_with_workspace_relative_slashed_paths() {
    let fixture = Fixture::new("all");
    let mut session = Session::start(fixture.serve(ExternalMode::Read));

    let mut tools: Vec<String> = session.rpc("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    tools.sort();
    assert_eq!(
        tools,
        [
            "list_files",
            "load_skill",
            "read_file",
            "read_notebook",
            "search_text",
            "view_image",
            "workspace_info"
        ]
    );

    let info = ok(&session.call("workspace_info", json!({})));
    assert!(info.contains("inside git repository at sub/proj"), "{info}");
    assert!(info.contains(std::env::consts::OS), "{info}");
    workspace_only(&fixture, "workspace_info", &info);

    let read = ok(&session.call("read_file", json!({"path": "src/lib/deep.rs"})));
    assert!(read.contains("pub fn needle"), "{read}");
    workspace_only(&fixture, "read_file", &read);

    let listed = ok(&session.call("list_files", json!({"glob": "**/*.rs"})));
    for path in ["src/main.rs", "src/lib/deep.rs"] {
        assert!(
            listed.lines().any(|l| l == path),
            "{path} missing: {listed}"
        );
    }
    workspace_only(&fixture, "list_files", &listed);
    let children = ok(&session.call("list_files", json!({"path": "src"})));
    assert!(children.lines().any(|l| l == "src/lib/"), "{children}");

    // rg finds deep.rs by walking, which on Windows joins with `\`.
    let found = ok(&session.call("search_text", json!({"query": "needle"})));
    assert!(found.contains("src/lib/deep.rs"), "{found}");
    assert!(found.contains("src/main.rs"), "{found}");
    workspace_only(&fixture, "search_text", &found);
    let files = ok(&session.call(
        "search_text",
        json!({"query": "needle", "path": "src", "output_mode": "files_with_matches"}),
    ));
    assert!(files.lines().any(|l| l == "src/lib/deep.rs"), "{files}");
    workspace_only(&fixture, "search_text", &files);

    let image = session.call("view_image", json!({"path": "pixel.png"}));
    assert!(ok(&image).starts_with("pixel.png: PNG"), "{image}");
    assert_eq!(image["content"][1]["mimeType"], "image/png", "{image}");

    let notebook = ok(&session.call("read_notebook", json!({"path": "nb.ipynb"})));
    assert!(notebook.contains("nb-marker"), "{notebook}");
    workspace_only(&fixture, "read_notebook", &notebook);

    let skill = ok(&session.call("load_skill", json!({"name": "hello"})));
    assert!(skill.contains("project-skill-body"), "{skill}");
    assert!(skill.contains("notes.md"), "{skill}");
    workspace_only(&fixture, "load_skill", &skill);
    let notes = ok(&session.call("load_skill", json!({"name": "hello", "file": "notes.md"})));
    assert!(notes.contains("project-skill-notes"), "{notes}");

    // An installed skill is the one place an absolute path is shown: where
    // it is on this machine. In the Runtime's one spelling, found by name.
    let installed = ok(&session.call("load_skill", json!({"name": "greet"})));
    assert!(installed.contains("installed-skill-body"), "{installed}");
    let listing = ok(&session.call("load_skill", json!({})));
    assert!(listing.contains("greet"), "{listing}");
    let home = ccnm_core::reported_path(&fixture.dir.join("home").canonicalize().unwrap());
    for said in [&installed, &listing] {
        assert!(!said.contains(r"\\?\"), "{said}");
        assert!(!said.contains('\\'), "{said}");
    }
    assert!(
        listing.contains(&format!("{}/.claude/skills/greet", home.display())),
        "{listing}"
    );
    session.shutdown();
}

/// A coding session (P109 for Windows, the same everywhere else):
/// `apply_patch` writes and keeps a file's BOM and CRLF, and a second
/// writer is refused by the write guard while the first holds it. Running
/// commands on Windows is tests/windows_exec.rs.
#[test]
fn a_coding_session_writes_and_a_second_writer_is_refused() {
    let fixture = Fixture::new("coding");
    let mut session = Session::start(fixture.serve(ExternalMode::Coding));
    let tools: Vec<String> = session.rpc("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for tool in ["apply_patch", "exec_command", "read_output", "stop_command"] {
        assert!(tools.iter().any(|t| t == tool), "{tool} missing: {tools:?}");
    }
    assert!(
        !tools.iter().any(|t| t == "call_mcp_tool"),
        "relaying starts programs: {tools:?}"
    );

    let added = ok(&session.call(
        "apply_patch",
        json!({"files": [{"op": "add", "path": "notes/new.txt", "content": "fresh\n"}]}),
    ));
    assert!(added.contains("notes/new.txt"), "{added}");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("notes/new.txt")).unwrap(),
        "fresh\n"
    );

    std::fs::write(
        fixture.root.join("crlf.txt"),
        b"\xEF\xBB\xBFone\r\ntwo\r\nthree\r\n",
    )
    .unwrap();
    // The footer ends `; version <v>]`, which is where a model copies it from.
    let read = ok(&session.call("read_file", json!({"path": "crlf.txt"})));
    let version = read
        .rsplit("version ")
        .next()
        .and_then(|rest| rest.split([' ', ']', '\n', ';']).next())
        .filter(|v| !v.is_empty() && read.contains("version "))
        .unwrap_or_else(|| panic!("no version in {read:?}"))
        .to_string();
    ok(&session.call(
        "apply_patch",
        json!({"files": [{"op": "update", "path": "crlf.txt", "version": version,
                          "edits": [{"old": "two", "new": "TWO"}]}]}),
    ));
    assert_eq!(
        std::fs::read(fixture.root.join("crlf.txt")).unwrap(),
        b"\xEF\xBB\xBFone\r\nTWO\r\nthree\r\n"
    );

    let second = fixture
        .serve(ExternalMode::Coding)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(!second.status.success(), "{stderr}");
    assert!(second.stdout.is_empty(), "{second:?}");
    assert!(stderr.contains("write guard is busy"), "{stderr}");

    session.shutdown();
    Session::start(fixture.serve(ExternalMode::Coding)).shutdown();
}

/// The names NTFS reads as another file (design D5), through the tools a
/// model calls rather than the path module alone.
#[cfg(windows)]
#[test]
fn names_that_are_another_file_on_windows_are_refused_by_the_tools() {
    let fixture = Fixture::new("names");
    std::fs::write(fixture.root.join("README.md:hidden"), "in a stream\n").unwrap();
    let mut session = Session::start(fixture.serve(ExternalMode::Read));
    for (tool, args) in [
        ("read_file", json!({"path": "README.md:hidden"})),
        ("read_file", json!({"path": "README.md."})),
        ("read_file", json!({"path": "src /main.rs"})),
        ("read_file", json!({"path": "NUL"})),
        ("read_file", json!({"path": "src/con.txt"})),
        ("read_file", json!({"path": "SRC~1/main.rs"})),
        ("list_files", json!({"path": "SRC~1"})),
        ("search_text", json!({"query": "needle", "path": "src."})),
        ("view_image", json!({"path": "pixel.png::$DATA"})),
        ("read_notebook", json!({"path": "COM1"})),
        ("load_skill", json!({"name": "hello", "file": "CON"})),
        ("load_skill", json!({"name": "hello", "file": "notes.md:x"})),
    ] {
        let said = refusal(&session.call(tool, args.clone()));
        assert!(said.starts_with("CCNM_E_POLICY:"), "{tool} {args}: {said}");
        workspace_only(&fixture, tool, &said);
    }
    session.shutdown();
}

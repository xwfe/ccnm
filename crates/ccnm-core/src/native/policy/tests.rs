use super::*;
use std::fs;
use std::os::unix::fs::symlink;

const MARKER: &str = "CCNM_EXEC_SESSION";

/// One way of editing a captured request, named for the assertion message.
type Change<'a> = Box<dyn Fn(&mut Value) + 'a>;

/// A workspace next to an "outside" directory, canonical, with the links
/// the containment rules exist for.
struct Ws {
    root: PathBuf,
    outside: PathBuf,
    _dir: ccnm_testdir::TestDir,
}

fn ws(name: &str) -> Ws {
    let dir =
        std::env::temp_dir().join(format!("ccnm-native-policy-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("ws/src")).unwrap();
    fs::create_dir_all(dir.join("outside")).unwrap();
    fs::write(dir.join("ws/src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.join("outside/secret.txt"), "secret\n").unwrap();
    symlink(dir.join("outside"), dir.join("ws/escape")).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    Ws {
        root: dir.join("ws"),
        outside: dir.join("outside"),
        _dir: ccnm_testdir::TestDir::adopt(dir),
    }
}

impl Ws {
    fn policy(&self) -> Policy {
        Policy::new(self.root.clone(), MARKER)
    }

    /// A request Codex 0.154.0 really sent (P21), respelled for this
    /// workspace.
    fn captured(&self, name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/codex-0.154.0/exec-server")
            .join(name);
        let text = fs::read_to_string(path)
            .unwrap()
            .replace("{ROOT}", self.root.to_str().unwrap())
            .replace("{OUTSIDE}", self.outside.to_str().unwrap())
            .replace("{SERVER_HOME}", "/home/ccrun");
        serde_json::from_str(&text).unwrap()
    }

    fn uri(&self, rel: &str) -> String {
        format!("file://{}/{rel}", self.root.display())
    }
}

fn refused(verdict: &Verdict) -> (i64, String) {
    match verdict {
        Verdict::Reply(reply) => (
            reply["error"]["code"].as_i64().unwrap(),
            reply["error"]["message"].as_str().unwrap().to_string(),
        ),
        other => panic!("expected a reply, got {other:?}"),
    }
}

fn request(method: &str, params: Value) -> Value {
    json!({"id": 7, "method": method, "params": params})
}

#[test]
fn what_codex_sends_for_ordinary_work_is_forwarded() {
    let ws = ws("ordinary");
    let policy = ws.policy();
    for name in [
        "initialize.json",
        "environment-config-read.json",
        "fs-get-metadata-startup.json",
        "process-start-workspace-write.json",
        "fs-write-file-workspace-write.json",
    ] {
        assert_eq!(
            policy.decide(&ws.captured(name)),
            Verdict::Forward,
            "{name}"
        );
    }
    assert_eq!(
        policy.decide(&json!({"method": "initialized", "params": {}})),
        Verdict::Forward
    );
}

/// The two requests that wrote outside the workspace in P21 once a person
/// approved them in Codex.
#[test]
fn what_codex_sends_after_an_approved_escalation_is_refused() {
    let ws = ws("escalated");
    let policy = ws.policy();
    let (code, message) = refused(&policy.decide(&ws.captured("process-start-escalated.json")));
    assert_eq!(code, -32600);
    assert!(message.contains("a sandbox is required"), "{message}");

    let outside = ws.captured("fs-write-file-approved-outside.json");
    let (code, message) = refused(&policy.decide(&outside));
    assert_eq!(code, -32600);
    assert!(message.contains("outside the workspace"), "{message}");

    // The same widened sandbox on a path that *is* inside: the extra
    // `{"type": "path"}` entry alone is enough to refuse it.
    let mut inside = outside.clone();
    inside["params"]["path"] = json!(ws.uri("new.txt"));
    let (_, message) = refused(&policy.decide(&inside));
    assert!(
        message.contains("more than the workspace-write shape"),
        "{message}"
    );

    // And Codex's automatic retry without a sandbox.
    inside["params"]["sandbox"] = Value::Null;
    let (_, message) = refused(&policy.decide(&inside));
    assert!(message.contains("a sandbox is required"), "{message}");
}

#[test]
fn a_refusal_answers_the_request_it_refuses() {
    let ws = ws("reply-id");
    let verdict = ws.policy().decide(&request(
        "http/request",
        json!({"url": "http://127.0.0.1/"}),
    ));
    let Verdict::Reply(reply) = verdict else {
        panic!()
    };
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["error"]["code"], -32600);
}

/// P21: answered "not found", Codex behaves as if there were no parent
/// repository; forwarded, it goes on to read that repository's AGENTS.md.
#[test]
fn the_git_walk_above_the_root_is_answered_as_not_found() {
    let ws = ws("git-walk");
    let policy = ws.policy();
    let mut ancestor = ws.root.parent().map(Path::to_path_buf);
    while let Some(dir) = ancestor {
        let uri = format!("file://{}", dir.join(".git").display());
        let (code, _) = refused(&policy.decide(&request("fs/getMetadata", json!({"path": uri}))));
        assert_eq!(code, -32004, "{}", dir.display());
        ancestor = dir.parent().map(Path::to_path_buf);
    }
    // Inside the root it is an ordinary path, and exec-server answers.
    let here = request("fs/getMetadata", json!({"path": ws.uri(".git")}));
    assert_eq!(policy.decide(&here), Verdict::Forward);
    // A `.git` somewhere that is not above the root is just outside.
    let aside = format!("file://{}", ws.outside.join(".git").display());
    let (code, _) = refused(&policy.decide(&request("fs/getMetadata", json!({"path": aside}))));
    assert_eq!(code, -32600);
}

#[test]
fn reads_follow_the_mcp_read_contract_whatever_the_sandbox_says() {
    let ws = ws("reads");
    let policy = ws.policy();
    let workspace_write =
        ws.captured("fs-write-file-workspace-write.json")["params"]["sandbox"].clone();
    for sandbox in [Value::Null, workspace_write] {
        for (uri, allowed) in [
            (ws.uri("src/main.rs"), true),
            (ws.uri("not/there/yet.rs"), true),
            (format!("file://{}", ws.root.display()), true),
            (
                format!("file://{}", ws.outside.join("secret.txt").display()),
                false,
            ),
            (ws.uri("escape/secret.txt"), false),
        ] {
            for method in [
                "fs/getMetadata",
                "fs/readFile",
                "fs/readDirectory",
                "fs/open",
            ] {
                let verdict =
                    policy.decide(&request(method, json!({"path": uri, "sandbox": sandbox})));
                assert_eq!(verdict == Verdict::Forward, allowed, "{method} {uri}");
            }
        }
    }
}

/// exec-server parses these with the `url` crate, which normalizes dot
/// segments and reads `\` as `/`. Anything that could mean two things is
/// refused instead of being normalized the same way by hand.
#[test]
fn a_uri_that_could_be_read_two_ways_is_refused() {
    let ws = ws("uris");
    let policy = ws.policy();
    let root = ws.root.display().to_string();
    for uri in [
        format!("file://{root}/src/../../outside/secret.txt"),
        format!("file://{root}/src/%2e%2e/%2E%2E/outside/secret.txt"),
        format!("file://{root}/src/.%2e/x"),
        format!("file://{root}/src\\..\\..\\outside"),
        format!("file://{root}/src%5c..%5c..%5coutside"),
        format!("file://{root}/./src/main.rs"),
        format!("file://{root}//src/main.rs"),
        format!("file://{root}/src/main.rs?x=1"),
        format!("file://{root}/src/main.rs#frag"),
        format!("file://{root}/src/%00main.rs"),
        format!("file://{root}/src/%zzmain.rs"),
        format!("file://{root}/src/%+1main.rs"),
        format!("file://attacker.example{root}/src/main.rs"),
        format!("file://user@{root}/src/main.rs"),
        format!("http://{root}/src/main.rs"),
        root.clone(),
    ] {
        let (code, _) = refused(&policy.decide(&request("fs/readFile", json!({"path": uri}))));
        assert_eq!(code, -32600, "{uri}");
    }
    assert!(policy.decide(&request("fs/readFile", json!({"path": 42}))) != Verdict::Forward);
    // The two spellings Codex's own URI type produces for a local path.
    fs::write(ws.root.join("a b.txt"), "x").unwrap();
    for uri in [
        format!("file://{root}/a%20b.txt"),
        format!("file://localhost{root}/src/main.rs"),
    ] {
        assert_eq!(
            policy.decide(&request("fs/readFile", json!({"path": uri}))),
            Verdict::Forward,
            "{uri}"
        );
    }
}

#[test]
fn writes_follow_the_mcp_write_contract() {
    let ws = ws("writes");
    let policy = ws.policy();
    fs::create_dir_all(ws.root.join(".git")).unwrap();
    symlink("src/main.rs", ws.root.join("link.rs")).unwrap();
    let sandbox = ws.captured("fs-write-file-workspace-write.json")["params"]["sandbox"].clone();
    let write = |uri: String| {
        policy.decide(&request(
            "fs/writeFile",
            json!({"path": uri, "dataBase64": "", "sandbox": sandbox}),
        ))
    };
    assert_eq!(write(ws.uri("src/new.rs")), Verdict::Forward);
    for (uri, why) in [
        (ws.uri(".git/config"), ".git"),
        (ws.uri("link.rs"), "symlink"),
        (ws.uri("escape/planted.txt"), "outside"),
        (format!("file://{}", ws.root.display()), "root itself"),
    ] {
        let (code, message) = refused(&write(uri.clone()));
        assert_eq!(code, -32600, "{uri}");
        assert!(message.contains(why), "{uri}: {message}");
    }
    let remove = request(
        "fs/remove",
        json!({"path": format!("file://{}", ws.root.display()), "recursive": true, "sandbox": sandbox}),
    );
    assert!(policy.decide(&remove) != Verdict::Forward);
    let copy_out = request(
        "fs/copy",
        json!({
            "sourcePath": format!("file://{}", ws.outside.join("secret.txt").display()),
            "destinationPath": ws.uri("stolen.txt"),
            "recursive": false,
            "sandbox": sandbox,
        }),
    );
    assert!(policy.decide(&copy_out) != Verdict::Forward);
}

#[test]
fn every_way_a_sandbox_can_be_widened_is_refused() {
    let ws = ws("sandbox");
    let policy = ws.policy();
    let base = ws.captured("process-start-workspace-write.json");
    assert_eq!(policy.decide(&base), Verdict::Forward);
    let outside_uri = format!("file://{}", ws.outside.display());
    let cases: Vec<(&str, Change)> = vec![
        (
            "no sandbox",
            Box::new(|m| m["params"]["sandbox"] = Value::Null),
        ),
        (
            "cwd outside",
            Box::new(|m| m["params"]["cwd"] = json!(outside_uri)),
        ),
        (
            "sandbox cwd outside",
            Box::new(|m| m["params"]["sandbox"]["cwd"] = json!(outside_uri)),
        ),
        (
            "second root",
            Box::new(|m| {
                m["params"]["sandbox"]["workspaceRoots"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(outside_uri))
            }),
        ),
        (
            "other root",
            Box::new(|m| m["params"]["sandbox"]["workspaceRoots"] = json!([outside_uri])),
        ),
        (
            "temporary directories",
            Box::new(|m| m["params"]["sandbox"]["temporaryDirectories"] = json!([outside_uri])),
        ),
        (
            "legacy landlock",
            Box::new(|m| m["params"]["sandbox"]["useLegacyLandlock"] = json!(true)),
        ),
        (
            "unknown sandbox key",
            Box::new(|m| m["params"]["sandbox"]["extraGrant"] = json!(true)),
        ),
        (
            "unmanaged",
            Box::new(|m| m["params"]["sandbox"]["permissions"]["type"] = json!("disabled")),
        ),
        (
            "network",
            Box::new(|m| m["params"]["sandbox"]["permissions"]["network"] = json!("enabled")),
        ),
        (
            "unrestricted file system",
            Box::new(|m| {
                m["params"]["sandbox"]["permissions"]["file_system"]["type"] = json!("unrestricted")
            }),
        ),
        (
            "path entry",
            Box::new(|m| {
                m["params"]["sandbox"]["permissions"]["file_system"]["entries"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"path": {"type": "path", "path": outside_uri}, "access": "write"}))
            }),
        ),
        (
            "root writable",
            Box::new(|m| {
                m["params"]["sandbox"]["permissions"]["file_system"]["entries"][0]["access"] =
                    json!("write")
            }),
        ),
        (
            "missing path creates",
            Box::new(|m| {
                m["params"]["sandbox"]["permissions"]["file_system"]["entries"][4]["missing_path_behavior"] =
                    json!("create")
            }),
        ),
        (
            "managed network",
            Box::new(|m| m["params"]["managedNetwork"] = json!({"allow": ["*"]})),
        ),
        (
            "enforce managed network",
            Box::new(|m| m["params"]["enforceManagedNetwork"] = json!(true)),
        ),
        (
            "network proxy",
            Box::new(|m| m["params"]["networkProxy"] = json!({})),
        ),
        (
            "shell snapshot",
            Box::new(|m| {
                m["params"]["shellSnapshot"] =
                    json!({"scopeId": "x", "shell": {"name": "sh", "path": "/bin/sh"}})
            }),
        ),
    ];
    for (name, change) in cases {
        let mut message = base.clone();
        change(&mut message);
        let (code, _) = refused(&policy.decide(&message));
        assert_eq!(code, -32600, "{name}");
    }
    // Fewer entries than Codex sends is narrower, not wider.
    let mut narrower = base.clone();
    narrower["params"]["sandbox"]["permissions"]["file_system"]["entries"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    assert_eq!(policy.decide(&narrower), Verdict::Forward);
}

/// The supervisor proves a session's processes are gone by the marker they
/// inherit; a process started without it could outlive the write guard.
#[test]
fn a_process_must_keep_the_session_marker() {
    let ws = ws("marker");
    let policy = ws.policy();
    let base = ws.captured("process-start-workspace-write.json");
    let cases: Vec<(&str, Change)> = vec![
        (
            "no env policy",
            Box::new(|m| m["params"]["envPolicy"] = Value::Null),
        ),
        (
            "inherit none",
            Box::new(|m| m["params"]["envPolicy"]["inherit"] = json!("none")),
        ),
        (
            "include only",
            Box::new(|m| m["params"]["envPolicy"]["includeOnly"] = json!(["PATH"])),
        ),
        (
            "excluded",
            Box::new(|m| {
                m["params"]["envPolicy"]["exclude"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(MARKER))
            }),
        ),
        (
            "overridden by set",
            Box::new(|m| m["params"]["envPolicy"]["set"][MARKER] = json!("other")),
        ),
        (
            "overridden by env",
            Box::new(|m| m["params"]["env"][MARKER] = json!("other")),
        ),
    ];
    for (name, change) in cases {
        let mut message = base.clone();
        change(&mut message);
        let (code, _) = refused(&policy.decide(&message));
        assert_eq!(code, -32600, "{name}");
    }
}

#[test]
fn methods_and_messages_outside_the_table() {
    let ws = ws("table");
    let policy = ws.policy();
    let (code, _) = refused(&policy.decide(&request("http/request", json!({}))));
    assert_eq!(code, -32600);
    for method in ["capabilityRoots/discoverV1", "shutdown", "exec", "made/up"] {
        let (code, _) = refused(&policy.decide(&request(method, json!({}))));
        assert_eq!(code, -32601, "{method}");
    }
    let resume = request(
        "initialize",
        json!({"clientName": "codex-environment", "resumeSessionId": "old"}),
    );
    assert_eq!(refused(&policy.decide(&resume)).0, -32600);
    for method in [
        "process/read",
        "process/write",
        "process/signal",
        "process/terminate",
        "fs/readBlock",
        "fs/close",
        "environment/info",
    ] {
        assert_eq!(
            policy.decide(&request(method, json!({}))),
            Verdict::Forward,
            "{method}"
        );
    }
    // exec-server closes the connection on a notification it does not know.
    assert_eq!(
        policy.decide(&json!({"method": "bogus/notify", "params": {}})),
        Verdict::Drop
    );
    // A response to a server request is forwarded as-is.
    assert_eq!(
        policy.decide(&json!({"id": 3, "result": {}})),
        Verdict::Forward
    );
    for junk in [json!([1, 2]), json!("text"), json!({"id": 1})] {
        let Verdict::Reply(reply) = policy.decide(&junk) else {
            panic!("{junk}")
        };
        assert_eq!(reply["id"], -1);
    }
    // environmentConfig/read may only ask about the workspace.
    let elsewhere = request(
        "environmentConfig/read",
        json!({"cwd": format!("file://{}", ws.outside.display()), "configPaths": [], "requirementsPaths": []}),
    );
    assert_eq!(refused(&policy.decide(&elsewhere)).0, -32600);
}

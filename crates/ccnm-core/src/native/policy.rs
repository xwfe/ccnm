//! The method rule table for the Codex exec-server chain (P22.4).
//!
//! Every message Codex sends to `codex exec-server` passes through
//! [`Policy::decide`] on the Runtime, before exec-server sees it. The table
//! it implements was frozen from measurement in
//! `docs/research/p21-codex-native-surface-2026-09-16.md`; a row that is not
//! there is refused. Why this is needed at all: exec-server trusts whatever
//! sandbox the client sends, `null` included, and Codex itself sends
//! `sandbox: null` or a widened sandbox once a person approves an escalation
//! in the TUI (P21, both wrote outside the workspace).
//!
//! Pure: a message and this Runtime's canonical root in, a verdict out. The
//! only I/O is resolving paths on disk, which is what containment means.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::mcp::path;

/// What to do with one message from the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Pass it to exec-server unchanged.
    Forward,
    /// Do not pass it on; send this back to the client instead.
    Reply(Value),
    /// A notification exec-server must not see. exec-server closes the
    /// whole connection on a notification it does not know (toexec G01),
    /// so dropping is the only way to keep the session alive.
    Drop,
}

/// exec-server's own "no such file" code. Codex maps it to `NotFound`.
const NOT_FOUND: i64 = -32004;
/// exec-server answers a sandbox denial with this, and Codex maps it to
/// `InvalidInput`: a refusal shaped like the executor's own.
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// The complete `file_system.entries` of Codex 0.154.0's workspace-write
/// sandbox, as captured in P21. A sandbox may use fewer of these, never
/// another one: the entry Codex adds when a person approves a write outside
/// the workspace is `{"type": "path", ...}`, which is not here.
fn allowed_entries() -> [(Value, &'static str); 7] {
    let special = |kind: &str| json!({"type": "special", "value": {"kind": kind}});
    let under = |subpath: &str| json!({"type": "special", "value": {"kind": "project_roots", "subpath": subpath}});
    [
        (special("root"), "read"),
        (special("project_roots"), "write"),
        (special("slash_tmp"), "write"),
        (special("tmpdir"), "write"),
        (under(".git"), "read"),
        (under(".agents"), "read"),
        (under(".codex"), "read"),
    ]
}

/// Keys a sandbox context may carry. An unknown key is refused rather than
/// ignored: the version is pinned, so a new key means a new executor whose
/// meaning nobody here has measured.
const SANDBOX_KEYS: &[&str] = &[
    "permissions",
    "cwd",
    "workspaceRoots",
    "userHomeDir",
    "temporaryDirectories",
    "windowsSandboxLevel",
    "windowsSandboxPrivateDesktop",
    "windowsSandboxProxySettingsMode",
    "useLegacyLandlock",
];

#[derive(Debug, Clone)]
pub struct Policy {
    /// Canonical. Codex is started with `-C` set to exactly this path, so
    /// every URI it sends is spelled against it.
    root: PathBuf,
    /// The environment variable every process exec-server starts must keep:
    /// the supervisor finds leftover processes by it before it releases
    /// the write guard.
    marker: String,
}

impl Policy {
    pub fn new(root: PathBuf, marker: impl Into<String>) -> Self {
        Policy {
            root,
            marker: marker.into(),
        }
    }

    pub fn decide(&self, message: &Value) -> Verdict {
        let Some(object) = message.as_object() else {
            return reply(Value::from(-1), INVALID_REQUEST, "not a JSON-RPC object");
        };
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // A response to something exec-server asked. It carries no
            // request of its own, so there is nothing to authorize.
            if object.contains_key("id")
                && (object.contains_key("result") || object.contains_key("error"))
            {
                return Verdict::Forward;
            }
            return reply(Value::from(-1), INVALID_REQUEST, "not a JSON-RPC request");
        };
        let Some(id) = object.get("id").cloned() else {
            return if method == "initialized" {
                Verdict::Forward
            } else {
                Verdict::Drop
            };
        };
        let params = object.get("params").and_then(Value::as_object);
        let empty = Map::new();
        let params = params.unwrap_or(&empty);
        match self.check(method, params) {
            Ok(Allowed::Forward) => Verdict::Forward,
            Ok(Allowed::NotFound) => Verdict::Reply(json!({
                "id": id,
                "error": {"code": NOT_FOUND, "message": "No such file or directory (os error 2)"},
            })),
            Err(Refusal::Unknown) => reply(
                id,
                METHOD_NOT_FOUND,
                &format!("ccnm does not forward {method}"),
            ),
            Err(Refusal::Because(why)) => reply(
                id,
                INVALID_REQUEST,
                &format!("ccnm refused {method}: {why}"),
            ),
        }
    }

    fn check(&self, method: &str, params: &Map<String, Value>) -> Result<Allowed, Refusal> {
        match method {
            "initialize" => match params.get("resumeSessionId") {
                None | Some(Value::Null) => Ok(Allowed::Forward),
                // The session ends when the connection does (ccnm v1).
                Some(_) => Err(because("resuming an exec-server session is not supported")),
            },
            "environment/info" | "environment/status" => Ok(Allowed::Forward),
            "environmentConfig/read" => {
                self.inside(params.get("cwd"))?;
                Ok(Allowed::Forward)
            }
            "fs/getMetadata" | "fs/readFile" | "fs/readDirectory" | "fs/walk"
            | "fs/canonicalize" | "fs/open" => self.read(params.get("path")),
            "fs/readBlock" | "fs/close" => Ok(Allowed::Forward),
            "fs/writeFile" | "fs/remove" | "fs/createDirectory" => {
                self.write(params.get("path"))?;
                self.sandbox(params.get("sandbox"))?;
                Ok(Allowed::Forward)
            }
            "fs/copy" => {
                self.inside(params.get("sourcePath"))?;
                self.write(params.get("destinationPath"))?;
                self.sandbox(params.get("sandbox"))?;
                Ok(Allowed::Forward)
            }
            "process/start" => {
                self.process(params)?;
                Ok(Allowed::Forward)
            }
            "process/read" | "process/write" | "process/signal" | "process/terminate" => {
                Ok(Allowed::Forward)
            }
            "http/request" => Err(because("network requests are not part of this chain")),
            _ => Err(Refusal::Unknown),
        }
    }

    /// A read may name something that does not exist -- exec-server's own
    /// not-found is what Codex expects then. Codex also walks up from the
    /// root looking for `.git`; above the root that walk is answered here,
    /// as if there were nothing, so it never learns what is outside
    /// (P21: forwarded, it went on to read the parent repository's
    /// `AGENTS.md`).
    fn read(&self, uri: Option<&Value>) -> Result<Allowed, Refusal> {
        let path = file_path(uri)?;
        if path.file_name().is_some_and(|name| name == ".git")
            && let Some(parent) = path.parent()
            && parent != self.root
            && self.root.starts_with(parent)
        {
            return Ok(Allowed::NotFound);
        }
        self.contain(&path, false)?;
        Ok(Allowed::Forward)
    }

    fn inside(&self, uri: Option<&Value>) -> Result<(), Refusal> {
        let path = file_path(uri)?;
        self.contain(&path, false)
    }

    fn write(&self, uri: Option<&Value>) -> Result<(), Refusal> {
        let path = file_path(uri)?;
        self.contain(&path, true)
    }

    /// Lexically under the root, then the same on-disk rules as the MCP
    /// tools: `resolve_inside` for reads, `resolve_write` (no `.git`, no
    /// writing through a symlink) for writes.
    fn contain(&self, path: &Path, writes: bool) -> Result<(), Refusal> {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return Err(because("path is outside the workspace"));
        };
        if rel.as_os_str().is_empty() {
            return if writes {
                Err(because("the workspace root itself cannot be written"))
            } else {
                Ok(())
            };
        }
        let Some(rel) = rel.to_str() else {
            return Err(because("path is not valid UTF-8"));
        };
        let result = if writes {
            path::resolve_write(&self.root, rel).map(|_| ())
        } else {
            path::resolve_inside(&self.root, rel).map(|_| ())
        };
        result.map_err(|error| because(error.message()))
    }

    fn process(&self, params: &Map<String, Value>) -> Result<(), Refusal> {
        self.inside(params.get("cwd"))?;
        self.sandbox(params.get("sandbox"))?;
        if !is_null_or_absent(params.get("managedNetwork"))
            || !is_null_or_absent(params.get("networkProxy"))
        {
            return Err(because("managed networking is not part of this chain"));
        }
        if params
            .get("enforceManagedNetwork")
            .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(because("managed networking is not part of this chain"));
        }
        if !is_null_or_absent(params.get("shellSnapshot")) {
            return Err(because("shell snapshots are not part of this chain"));
        }
        self.environment(params)
    }

    /// Processes must inherit exec-server's environment, which is what
    /// Codex sends anyway, and must keep the session marker: without it the
    /// supervisor could not prove they are gone before it releases the
    /// write guard.
    fn environment(&self, params: &Map<String, Value>) -> Result<(), Refusal> {
        let marker = self.marker.as_str();
        let names = |value: Option<&Value>| -> Vec<String> {
            match value {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect(),
                Some(Value::Object(map)) => map.keys().cloned().collect(),
                _ => Vec::new(),
            }
        };
        let Some(policy) = params.get("envPolicy").and_then(Value::as_object) else {
            return Err(because("envPolicy must inherit the executor environment"));
        };
        if policy.get("inherit").and_then(Value::as_str) != Some("all")
            || !names(policy.get("includeOnly")).is_empty()
        {
            return Err(because("envPolicy must inherit the executor environment"));
        }
        if names(policy.get("exclude"))
            .iter()
            .any(|name| name == marker)
            || names(policy.get("set")).iter().any(|name| name == marker)
            || names(params.get("env")).iter().any(|name| name == marker)
        {
            return Err(because("the session marker variable cannot be changed"));
        }
        Ok(())
    }

    fn sandbox(&self, sandbox: Option<&Value>) -> Result<(), Refusal> {
        let Some(sandbox) = sandbox.and_then(Value::as_object) else {
            return Err(because("a sandbox is required"));
        };
        if let Some(key) = sandbox
            .keys()
            .find(|key| !SANDBOX_KEYS.contains(&key.as_str()))
        {
            return Err(because(&format!("unknown sandbox field {key}")));
        }
        let roots = match sandbox.get("workspaceRoots") {
            Some(Value::Array(roots)) => roots,
            _ => return Err(because("sandbox workspaceRoots must be the workspace root")),
        };
        if roots.len() != 1 || file_path(roots.first()).ok().as_deref() != Some(self.root.as_path())
        {
            return Err(because("sandbox workspaceRoots must be the workspace root"));
        }
        if !is_null_or_absent(sandbox.get("cwd")) {
            self.inside(sandbox.get("cwd"))?;
        }
        match sandbox.get("temporaryDirectories") {
            None | Some(Value::Null) => {}
            Some(Value::Array(dirs)) if dirs.is_empty() => {}
            // `tmpdir` entries resolve to these, so a client-chosen list
            // would turn a temp-dir grant into a grant anywhere.
            Some(_) => return Err(because("sandbox temporaryDirectories cannot be set")),
        }
        if sandbox
            .get("useLegacyLandlock")
            .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(because("the legacy Landlock sandbox has not been measured"));
        }
        let permissions = sandbox.get("permissions").and_then(Value::as_object);
        let Some(permissions) = permissions else {
            return Err(because("sandbox permissions are required"));
        };
        if permissions
            .keys()
            .any(|key| !["type", "file_system", "network"].contains(&key.as_str()))
            || permissions.get("type") != Some(&json!("managed"))
            || permissions.get("network") != Some(&json!("restricted"))
        {
            return Err(because("sandbox must be managed with restricted network"));
        }
        let file_system = permissions.get("file_system").and_then(Value::as_object);
        let Some(file_system) = file_system else {
            return Err(because("sandbox file system must be restricted"));
        };
        if file_system
            .keys()
            .any(|key| !["type", "entries"].contains(&key.as_str()))
            || file_system.get("type") != Some(&json!("restricted"))
        {
            return Err(because("sandbox file system must be restricted"));
        }
        let entries = match file_system.get("entries") {
            Some(Value::Array(entries)) => entries,
            _ => return Err(because("sandbox file system must list its entries")),
        };
        let allowed = allowed_entries();
        for entry in entries {
            let Some(entry) = entry.as_object() else {
                return Err(because("sandbox entry is not an object"));
            };
            let keys_ok = entry
                .keys()
                .all(|key| ["path", "access", "missing_path_behavior"].contains(&key.as_str()));
            let missing_ok = match entry.get("missing_path_behavior") {
                None | Some(Value::Null) => true,
                Some(value) => value == "skip",
            };
            let shape_ok = keys_ok && missing_ok;
            let path = entry.get("path");
            let access = entry.get("access").and_then(Value::as_str);
            let listed = allowed.iter().any(|(allowed_path, allowed_access)| {
                Some(allowed_path) == path && Some(*allowed_access) == access
            });
            if !shape_ok || !listed {
                return Err(because(
                    "sandbox grants more than the workspace-write shape ccnm measured",
                ));
            }
        }
        Ok(())
    }
}

enum Allowed {
    Forward,
    NotFound,
}

enum Refusal {
    Unknown,
    Because(String),
}

fn because(why: &str) -> Refusal {
    Refusal::Because(why.to_string())
}

fn reply(id: Value, code: i64, message: &str) -> Verdict {
    Verdict::Reply(json!({"id": id, "error": {"code": code, "message": message}}))
}

fn is_null_or_absent(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null))
}

/// A `file:` URI as exec-server will read it, or a refusal.
///
/// Strict on purpose. exec-server parses with the `url` crate, which
/// normalizes `..` and `%2e%2e` segments and reads `\` as `/` in a file
/// URL, so anything that could be read two ways is refused here rather
/// than normalized differently: no query, fragment, port, credentials or
/// host other than `localhost`; no `\`; no empty, `.` or `..` segment
/// after percent-decoding; no NUL.
fn file_path(uri: Option<&Value>) -> Result<PathBuf, Refusal> {
    let invalid = || because("path is not a plain file: URI");
    let uri = uri.and_then(Value::as_str).ok_or_else(invalid)?;
    let rest = uri.strip_prefix("file://").ok_or_else(invalid)?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') || rest.contains(['?', '#', '\\']) {
        return Err(invalid());
    }
    let bytes = percent_decode(rest).ok_or_else(invalid)?;
    if bytes.contains(&0) || bytes.contains(&b'\\') {
        return Err(invalid());
    }
    let text = String::from_utf8(bytes).map_err(|_| because("path is not valid UTF-8"))?;
    let segments: Vec<&str> = text[1..].split('/').collect();
    if text.len() > 1
        && segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err(invalid());
    }
    Ok(PathBuf::from(text))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;

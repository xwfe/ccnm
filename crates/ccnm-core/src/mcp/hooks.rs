//! A skill's `hooks`, run on the Runtime around this server's own tools (P79).
//!
//! Natively Claude Code registers a skill's hooks when the skill is invoked
//! and runs them for the rest of the session, as shell commands on the
//! machine it runs on. A remote session's skills arrive through
//! `load_skill`, so the client never knows one is active; and the machine it
//! runs on holds the AI login and not the project. The one place that sees
//! every project tool call is this server, on the project's machine, so
//! that is where they run: `PreToolUse` before a call, `PostToolUse` after
//! one that did not fail.
//!
//! **Only in a session whose commands already run with nobody asked**
//! (`--print`, `ccnm mcp bridge`, or `allow_unattended_exec`). A hook is a
//! command from a file in the repository or the Runtime account's home,
//! run without a person approving it -- the reason P36 did not run
//! `` !`command` `` either. Where `exec_command` asks first, registering a
//! hook would be a second way to run a command that skips the question;
//! where nothing asks, it grants nothing the model could not already do,
//! including for a SKILL.md the model just wrote itself. The caller decides
//! that; this module only parses, matches, runs and reads results.
//!
//! What is deliberately smaller than native, and said in the loaded text:
//! only `type: command`; only the two tool events (`Stop` and the rest
//! happen inside the client); `updatedInput` and `updatedMCPToolOutput` are
//! not applied; `matcher` is names, `|` and the `.*` / `*` wildcard -- any
//! other regex syntax matches nothing; matching hooks run one after
//! another, not in parallel.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value as Json, json};
use toexec_skill::frontmatter::Value;

use crate::error::Result;
use crate::mcp::sandbox::Sandbox;
use crate::process::{Cmd, Output, ProcessRunner, SystemRunner};

/// Native's default for a command hook.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
/// And the most this server waits, the same ceiling as `exec_command`.
const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// What of one hook's stderr or message reaches the model. Native caps what
/// hooks add to the context; a hook that dumps a log should not flood it.
const MAX_MESSAGE_CHARS: usize = 4000;

/// The server name hooks see ccnm's tools under, as a client lists them.
const MCP_PREFIX: &str = "mcp__ccnm__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    PreToolUse,
    PostToolUse,
}

impl Event {
    pub fn name(self) -> &'static str {
        match self {
            Event::PreToolUse => "PreToolUse",
            Event::PostToolUse => "PostToolUse",
        }
    }
}

/// One `type: command` hook of one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub skill: String,
    pub event: Event,
    /// As written; empty is every tool.
    pub matcher: String,
    pub command: String,
    pub timeout: Duration,
    /// Removed after its first run that exits 0, as natively.
    pub once: bool,
}

/// What a skill's `hooks` field held.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub hooks: Vec<Hook>,
    /// What will not run here, each in a few words, for the loaded text.
    pub not_run: Vec<String>,
}

/// Read a skill's `hooks` field. Never fails: what cannot be read is named
/// in [`Parsed::not_run`], so the author learns why it does nothing.
pub fn parse(skill: &str, value: &Value) -> Parsed {
    let mut parsed = Parsed::default();
    let Value::Map(events) = value else {
        parsed
            .not_run
            .push("hooks is not a map of events".to_string());
        return parsed;
    };
    for (event_name, groups) in events {
        let event = match event_name.as_str() {
            "PreToolUse" => Event::PreToolUse,
            "PostToolUse" => Event::PostToolUse,
            other => {
                parsed.not_run.push(format!(
                    "{other} (it happens inside the client, which this server never sees)"
                ));
                continue;
            }
        };
        let Value::List(groups) = groups else {
            parsed
                .not_run
                .push(format!("{event_name} (not a list of matcher groups)"));
            continue;
        };
        for group in groups {
            let Value::Map(fields) = group else {
                parsed
                    .not_run
                    .push(format!("{event_name} (an entry that is not a map)"));
                continue;
            };
            let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            let matcher = field("matcher")
                .and_then(Value::as_text)
                .unwrap_or("")
                .trim()
                .to_string();
            if let Some(unsupported) = unsupported_matcher(&matcher) {
                parsed.not_run.push(format!(
                    "{event_name} matcher \"{matcher}\" ({unsupported})"
                ));
                continue;
            }
            let Some(Value::List(commands)) = field("hooks") else {
                parsed.not_run.push(format!(
                    "{event_name} matcher \"{matcher}\" (no hooks list)"
                ));
                continue;
            };
            for command in commands {
                match one(skill, event, &matcher, command) {
                    Ok(hook) => parsed.hooks.push(hook),
                    Err(why) => parsed.not_run.push(format!("{event_name} {why}")),
                }
            }
        }
    }
    parsed
}

fn one(
    skill: &str,
    event: Event,
    matcher: &str,
    value: &Value,
) -> std::result::Result<Hook, String> {
    let Value::Map(fields) = value else {
        return Err("(a hook that is not a map)".to_string());
    };
    let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let kind = field("type").and_then(Value::as_text).unwrap_or("command");
    if kind != "command" {
        return Err(format!("type {kind} (only command hooks run here)"));
    }
    let Some(command) = field("command")
        .and_then(Value::as_text)
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return Err("(a command hook with no command)".to_string());
    };
    let timeout = match field("timeout").and_then(Value::as_text) {
        Some(text) => match text.trim().parse::<f64>() {
            Ok(secs) if secs > 0.0 => Duration::from_secs_f64(secs).min(MAX_TIMEOUT),
            _ => return Err(format!("timeout \"{text}\" (not a number of seconds)")),
        },
        None => DEFAULT_TIMEOUT,
    };
    let once = matches!(field("once"), Some(Value::Bool(true)));
    Ok(Hook {
        skill: skill.to_string(),
        event,
        matcher: matcher.to_string(),
        command: command.to_string(),
        timeout,
        once,
    })
}

/// Why a matcher cannot be honoured here, if it cannot. Names, `|` and the
/// `.*` / `*` wildcard cover what hooks are written with (`Bash`,
/// `Edit|Write`, `mcp__memory__.*`); anything else would need a regex
/// engine this crate does not carry, and guessing is worse than saying so.
fn unsupported_matcher(matcher: &str) -> Option<&'static str> {
    let bare = matcher.replace(".*", "").replace('*', "");
    bare.chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '|' | ' ')))
        .then_some("only names, | and .* are understood here")
}

/// Does `matcher` match `name`? Empty and `*` are everything; otherwise
/// each `|` alternative must match the whole name.
fn matches(matcher: &str, name: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    matcher
        .split('|')
        .any(|alt| wildcard(&alt.trim().replace(".*", "*"), name))
}

/// `*` is any run of characters, anything else is itself.
fn wildcard(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(after) = rest.strip_prefix(part) else {
                return false;
            };
            rest = after;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(at) = rest.find(part) else {
                return false;
            };
            rest = &rest[at + part.len()..];
        }
    }
    true
}

/// One way a hook sees a call: the tool's name and its input.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub tool_name: String,
    pub tool_input: Json,
}

/// The native tools a ccnm call stands in for, with the input a hook
/// written for local Claude Code expects -- so `matcher: Bash` and
/// `jq .tool_input.command` keep working. Paths are absolute on this
/// machine, as natively. `apply_patch` is one view per file it touches,
/// as natively each file is one `Edit` or `Write` call.
fn native_views(tool: &str, args: &Map<String, Json>, root: &Path) -> Vec<View> {
    let abs = |rel: &str| root.join(rel).display().to_string();
    let text = |key: &str| args.get(key).and_then(Json::as_str);
    let view = |name: &str, input: Json| View {
        tool_name: name.to_string(),
        tool_input: input,
    };
    match tool {
        "exec_command" => {
            let command = match text("shell") {
                Some(line) => line.to_string(),
                None => args
                    .get("cmd")
                    .and_then(Json::as_array)
                    .map(|argv| {
                        argv.iter()
                            .filter_map(Json::as_str)
                            .map(shell_quote)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            };
            let mut input = json!({ "command": command });
            if let Some(ms) = args.get("timeout_ms") {
                input["timeout"] = ms.clone();
            }
            if let Some(background) = args.get("run_in_background") {
                input["run_in_background"] = background.clone();
            }
            vec![view("Bash", input)]
        }
        "read_file" | "view_image" | "read_notebook" => text("path")
            .map(|path| vec![view("Read", json!({ "file_path": abs(path) }))])
            .unwrap_or_default(),
        "search_text" => {
            let mut input = json!({ "pattern": text("query").unwrap_or("") });
            if let Some(path) = text("path") {
                input["path"] = abs(path).into();
            }
            if let Some(glob) = text("glob") {
                input["glob"] = glob.into();
            }
            vec![view("Grep", input)]
        }
        "list_files" => {
            let mut input = json!({ "pattern": text("glob").unwrap_or("*") });
            input["path"] = abs(text("path").unwrap_or(".")).into();
            vec![view("Glob", input)]
        }
        "load_skill" => text("name")
            .map(|name| {
                let mut input = json!({ "skill": name });
                if let Some(arguments) = text("arguments") {
                    input["args"] = arguments.into();
                }
                vec![view("Skill", input)]
            })
            .unwrap_or_default(),
        "apply_patch" => args
            .get("files")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let path = file.get("path")?.as_str()?;
                let op = file.get("op").and_then(Json::as_str).unwrap_or(
                    if file.get("edits").is_some() {
                        "update"
                    } else if file.get("cells").is_some() {
                        "edit_notebook"
                    } else {
                        "write"
                    },
                );
                match op {
                    "add" | "write" => {
                        let mut input = json!({ "file_path": abs(path) });
                        if let Some(content) = file.get("content") {
                            input["content"] = content.clone();
                        }
                        Some(view("Write", input))
                    }
                    "update" => Some(view("Edit", json!({ "file_path": abs(path) }))),
                    "edit_notebook" => {
                        Some(view("NotebookEdit", json!({ "notebook_path": abs(path) })))
                    }
                    _ => None,
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A shell word that means the same thing when pasted into bash.
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./=:,+@%-".contains(c))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The views of one call a hook runs for. A native view is preferred when
/// its name matches, because that is what a hook written for local Claude
/// Code expects; otherwise ccnm's own name (`mcp__ccnm__exec_command`, or
/// just `exec_command`) with the arguments as sent.
pub fn views_for(hook: &Hook, tool: &str, args: &Map<String, Json>, root: &Path) -> Vec<View> {
    let native: Vec<View> = native_views(tool, args, root)
        .into_iter()
        .filter(|view| matches(&hook.matcher, &view.tool_name))
        .collect();
    if !native.is_empty() {
        return native;
    }
    let mcp = format!("{MCP_PREFIX}{tool}");
    if matches(&hook.matcher, &mcp) || matches(&hook.matcher, tool) {
        return vec![View {
            tool_name: mcp,
            tool_input: Json::Object(args.clone()),
        }];
    }
    Vec::new()
}

/// What the hook reads on stdin: the native fields this server can fill.
/// No `transcript_path` or `permission_mode`: the transcript is on the
/// other machine and the permission mode is the client's.
pub fn input(
    event: Event,
    session: &str,
    root: &Path,
    view: &View,
    response: Option<&str>,
) -> Vec<u8> {
    let mut value = json!({
        "session_id": session,
        "cwd": root.display().to_string(),
        "hook_event_name": event.name(),
        "tool_name": view.tool_name,
        "tool_input": view.tool_input,
    });
    if let Some(response) = response {
        value["tool_response"] = response.into();
    }
    serde_json::to_vec(&value).unwrap_or_default()
}

/// What one run of a hook means for the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Go on; `note` is for the model, when the hook said something.
    Proceed {
        note: Option<String>,
        succeeded: bool,
    },
    /// Do not run the tool (`PreToolUse` only).
    Block { reason: String },
}

/// Read one finished run the way Claude Code reads a command hook: exit 2
/// blocks a `PreToolUse` and hands stderr to the model after a
/// `PostToolUse`; exit 0 may print JSON with a decision or more context;
/// anything else is a failed hook, said once and otherwise ignored.
pub fn outcome(hook: &Hook, ran: &Result<Output>) -> Outcome {
    let who = format!("{} hook of skill {}", hook.event.name(), hook.skill);
    let out = match ran {
        Ok(out) => out,
        Err(e) => {
            return Outcome::Proceed {
                note: Some(format!("[{who} could not run: {}]", e.message())),
                succeeded: false,
            };
        }
    };
    if out.timed_out {
        return Outcome::Proceed {
            note: Some(format!(
                "[{who} was stopped after {} s and ignored]",
                hook.timeout.as_secs()
            )),
            succeeded: false,
        };
    }
    let stderr = clip(out.stderr_lossy().trim());
    match out.exit_code {
        Some(2) => match hook.event {
            Event::PreToolUse => Outcome::Block {
                reason: if stderr.is_empty() {
                    "it exited 2".to_string()
                } else {
                    stderr
                },
            },
            Event::PostToolUse => Outcome::Proceed {
                note: Some(format!("[{who}: {stderr}]")),
                succeeded: false,
            },
        },
        Some(0) => json_outcome(hook, &who, &out.stdout_lossy()),
        code => Outcome::Proceed {
            note: Some(format!(
                "[{who} failed ({}) and was ignored{}]",
                code.map_or("killed by a signal".to_string(), |c| format!("exit {c}")),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            )),
            succeeded: false,
        },
    }
}

fn json_outcome(hook: &Hook, who: &str, stdout: &str) -> Outcome {
    let done = |note: Option<String>| Outcome::Proceed {
        note,
        succeeded: true,
    };
    let Ok(Json::Object(answer)) = serde_json::from_str::<Json>(stdout.trim()) else {
        // Plain stdout is for the transcript natively, not the model.
        return done(None);
    };
    let specific = answer.get("hookSpecificOutput").and_then(Json::as_object);
    let field = |key: &str| {
        specific
            .and_then(|s| s.get(key))
            .or_else(|| answer.get(key))
            .and_then(Json::as_str)
            .map(|s| clip(s.trim()))
    };
    let mut notes = Vec::new();
    if hook.event == Event::PreToolUse {
        let reason = field("permissionDecisionReason")
            .or_else(|| field("reason"))
            .unwrap_or_default();
        let decision = field("permissionDecision").or_else(|| field("decision"));
        match decision.as_deref() {
            Some("deny") | Some("block") => {
                return Outcome::Block {
                    reason: if reason.is_empty() {
                        "it denied the call".to_string()
                    } else {
                        reason
                    },
                };
            }
            // There is nobody to ask: the caller registers hooks only in
            // sessions where commands run with nobody asked.
            Some("ask") => {
                return Outcome::Block {
                    reason: format!(
                        "it asked for a person to approve the call, and this session runs commands with nobody asked{}",
                        if reason.is_empty() {
                            String::new()
                        } else {
                            format!(": {reason}")
                        }
                    ),
                };
            }
            _ => {}
        }
        if specific.is_some_and(|s| s.contains_key("updatedInput")) {
            notes.push(format!(
                "[{who} asked to change the call's input; that is not done here, the call ran as sent]"
            ));
        }
    } else {
        if field("decision").as_deref() == Some("block") {
            notes.push(format!(
                "[{who}: {}]",
                field("reason").unwrap_or_else(|| "it flagged this result".to_string())
            ));
        }
        if specific.is_some_and(|s| {
            s.contains_key("updatedMCPToolOutput") || s.contains_key("updatedToolOutput")
        }) {
            notes.push(format!(
                "[{who} asked to replace this result; that is not done here, the result is as the tool returned it]"
            ));
        }
    }
    if answer.get("continue") == Some(&Json::Bool(false)) {
        return Outcome::Block {
            reason: field("stopReason").unwrap_or_else(|| "it said not to continue".to_string()),
        };
    }
    if let Some(context) = field("additionalContext").filter(|c| !c.is_empty()) {
        notes.push(format!("[{who}: {context}]"));
    }
    done((!notes.is_empty()).then(|| notes.join("\n")))
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_MESSAGE_CHARS) {
        Some((at, _)) => format!("{} ...", &text[..at]),
        None => text.to_string(),
    }
}

/// The hooks this session has registered, for as long as its server lives.
#[derive(Debug, Default)]
pub struct Registry {
    hooks: Mutex<Vec<(u64, Hook)>>,
    next: std::sync::atomic::AtomicU64,
}

impl Registry {
    /// Add a loaded skill's hooks. Loading the same skill again adds
    /// nothing twice: a hook that ran twice per call would be a surprise.
    pub fn add(&self, hooks: Vec<Hook>) {
        let mut held = self.hooks.lock().unwrap_or_else(|p| p.into_inner());
        for hook in hooks {
            if held.iter().any(|(_, h)| *h == hook) {
                continue;
            }
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            held.push((id, hook));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    /// Every hook for `event`, in the order they were registered.
    pub fn for_event(&self, event: Event) -> Vec<(u64, Hook)> {
        self.hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, hook)| hook.event == event)
            .cloned()
            .collect()
    }

    /// A `once` hook that has run successfully.
    pub fn retire(&self, id: u64) {
        self.hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(held, _)| *held != id);
    }
}

/// Runs a skill's commands -- hooks and `` !`command` `` lines -- the way
/// `exec_command` runs a shell line: `bash -c`, in the workspace root, as
/// this account, with the Agent's authentication stripped from the
/// environment, inside the workspace's sandbox when it has one, and after
/// the same credential re-check. Constructed only for a session whose
/// commands run with nobody asked.
pub struct Runner {
    root: PathBuf,
    sandbox: Option<Arc<Sandbox>>,
    accepted: crate::safety::Accepted,
    shared_account: bool,
}

impl Runner {
    pub fn new(
        root: PathBuf,
        sandbox: Option<Arc<Sandbox>>,
        accepted: crate::safety::Accepted,
        shared_account: bool,
    ) -> Runner {
        Runner {
            root,
            sandbox,
            accepted,
            shared_account,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self, line: &str, stdin: Option<Vec<u8>>, timeout: Duration) -> Result<Output> {
        let home = crate::paths::home_dir()?;
        crate::safety::credentials::runtime_gate(
            &home,
            self.accepted,
            self.shared_account,
            &SystemRunner,
        )?;
        let mut cmd = Cmd::new("bash")
            .args(["-c", line])
            .cwd(&self.root)
            .env("CLAUDE_PROJECT_DIR", &self.root)
            .timeout(timeout);
        if let Some(input) = stdin {
            cmd = cmd.stdin(input);
        }
        cmd = crate::safety::environment::runtime_child(cmd);
        if let Some(sandbox) = &self.sandbox {
            cmd = sandbox.wrap(cmd, &self.root);
        }
        SystemRunner.run(&cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front(yaml: &str) -> Value {
        toexec_skill::frontmatter::parse(yaml)
            .unwrap()
            .get("hooks")
            .cloned()
            .unwrap()
    }

    const HOOKS: &str = "hooks:\n  PreToolUse:\n    - matcher: \"Bash\"\n      hooks:\n        - type: command\n          command: \"./check.sh\"\n          timeout: 5\n          once: true\n  PostToolUse:\n    - matcher: Edit|Write\n      hooks:\n        - type: command\n          command: fmt.sh\n        - type: http\n          url: https://example.invalid\n  Stop:\n    - hooks:\n        - type: command\n          command: echo done\n";

    #[test]
    fn the_native_shape_is_read_and_what_cannot_run_is_named() {
        let parsed = parse("deploy", &front(HOOKS));
        assert_eq!(
            parsed.hooks,
            [
                Hook {
                    skill: "deploy".into(),
                    event: Event::PreToolUse,
                    matcher: "Bash".into(),
                    command: "./check.sh".into(),
                    timeout: Duration::from_secs(5),
                    once: true,
                },
                Hook {
                    skill: "deploy".into(),
                    event: Event::PostToolUse,
                    matcher: "Edit|Write".into(),
                    command: "fmt.sh".into(),
                    timeout: DEFAULT_TIMEOUT,
                    once: false,
                },
            ]
        );
        assert_eq!(parsed.not_run.len(), 2, "{:?}", parsed.not_run);
        assert!(
            parsed.not_run[0].contains("type http"),
            "{:?}",
            parsed.not_run
        );
        assert!(
            parsed.not_run[1].starts_with("Stop"),
            "{:?}",
            parsed.not_run
        );
    }

    #[test]
    fn a_matcher_beyond_names_and_wildcards_is_refused_not_guessed() {
        let parsed = parse(
            "x",
            &front(
                "hooks:\n  PreToolUse:\n    - matcher: \"^Bash$\"\n      hooks:\n        - command: x\n",
            ),
        );
        assert!(parsed.hooks.is_empty());
        assert!(
            parsed.not_run[0].contains("only names"),
            "{:?}",
            parsed.not_run
        );
    }

    #[test]
    fn matchers_are_names_alternatives_and_wildcards_matching_whole_names() {
        assert!(matches("", "Bash") && matches("*", "anything"));
        assert!(matches("Bash", "Bash"));
        assert!(!matches("Bash", "BashOutput"));
        assert!(matches("Edit|Write", "Write"));
        assert!(matches("mcp__ccnm__.*", "mcp__ccnm__exec_command"));
        assert!(matches("Notebook.*", "NotebookEdit"));
        assert!(!matches("Notebook.*", "Read"));
        assert!(matches("*_command", "exec_command"));
    }

    fn args(value: Json) -> Map<String, Json> {
        value.as_object().unwrap().clone()
    }

    fn hook(event: Event, matcher: &str) -> Hook {
        Hook {
            skill: "s".into(),
            event,
            matcher: matcher.into(),
            command: "true".into(),
            timeout: DEFAULT_TIMEOUT,
            once: false,
        }
    }

    /// A hook written for local Claude Code reads `.tool_input.command`
    /// and `.tool_input.file_path`; it gets them.
    #[test]
    fn a_native_matcher_sees_the_native_shape() {
        let root = Path::new("/work/p");
        let bash = views_for(
            &hook(Event::PreToolUse, "Bash"),
            "exec_command",
            &args(json!({"cmd": ["git", "commit", "-m", "two words"], "timeout_ms": 5000})),
            root,
        );
        assert_eq!(bash[0].tool_name, "Bash");
        assert_eq!(bash[0].tool_input["command"], "git commit -m 'two words'");
        assert_eq!(bash[0].tool_input["timeout"], 5000);
        let shell = views_for(
            &hook(Event::PreToolUse, "Bash"),
            "exec_command",
            &args(json!({"shell": "cargo test | tail"})),
            root,
        );
        assert_eq!(shell[0].tool_input["command"], "cargo test | tail");

        let patch = args(json!({"files": [
            {"op": "update", "path": "src/a.rs", "edits": []},
            {"op": "add", "path": "b.txt", "content": "x"},
            {"op": "delete", "path": "c.txt"},
        ]}));
        let edits = views_for(
            &hook(Event::PostToolUse, "Edit|Write"),
            "apply_patch",
            &patch,
            root,
        );
        assert_eq!(
            edits
                .iter()
                .map(|v| (
                    v.tool_name.as_str(),
                    v.tool_input["file_path"].as_str().unwrap()
                ))
                .collect::<Vec<_>>(),
            [("Edit", "/work/p/src/a.rs"), ("Write", "/work/p/b.txt")]
        );
        // Not a native tool at all, and not matched: no view.
        assert!(
            views_for(
                &hook(Event::PreToolUse, "Bash"),
                "read_file",
                &args(json!({"path": "a"})),
                root
            )
            .is_empty()
        );
    }

    #[test]
    fn ccnms_own_name_gets_the_arguments_as_sent() {
        let sent = args(json!({"files": [{"op": "delete", "path": "c.txt"}]}));
        let views = views_for(
            &hook(Event::PreToolUse, "mcp__ccnm__apply_patch"),
            "apply_patch",
            &sent,
            Path::new("/r"),
        );
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tool_name, "mcp__ccnm__apply_patch");
        assert_eq!(views[0].tool_input, Json::Object(sent.clone()));
        // Everything, and a tool with no native twin: ccnm's name.
        let all = views_for(
            &hook(Event::PreToolUse, ""),
            "stop_command",
            &sent,
            Path::new("/r"),
        );
        assert_eq!(all[0].tool_name, "mcp__ccnm__stop_command");
    }

    fn ran(code: Option<i32>, stdout: &str, stderr: &str) -> Result<Output> {
        Ok(Output {
            exit_code: code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            timed_out: false,
            duration: Duration::ZERO,
        })
    }

    #[test]
    fn exit_two_blocks_before_and_speaks_after() {
        let pre = hook(Event::PreToolUse, "Bash");
        assert_eq!(
            outcome(&pre, &ran(Some(2), "", "no force push\n")),
            Outcome::Block {
                reason: "no force push".into()
            }
        );
        let post = hook(Event::PostToolUse, "Edit");
        assert_eq!(
            outcome(&post, &ran(Some(2), "", "lint: 3 problems")),
            Outcome::Proceed {
                note: Some("[PostToolUse hook of skill s: lint: 3 problems]".into()),
                succeeded: false
            }
        );
    }

    #[test]
    fn json_decisions_are_read_and_ask_is_a_no_without_anybody_to_ask() {
        let pre = hook(Event::PreToolUse, "Bash");
        let deny = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"use the release script"}}"#;
        assert_eq!(
            outcome(&pre, &ran(Some(0), deny, "")),
            Outcome::Block {
                reason: "use the release script".into()
            }
        );
        let ask = r#"{"hookSpecificOutput":{"permissionDecision":"ask"}}"#;
        assert!(
            matches!(outcome(&pre, &ran(Some(0), ask, "")), Outcome::Block { reason } if reason.contains("nobody asked"))
        );
        let allow = r#"{"hookSpecificOutput":{"permissionDecision":"allow","updatedInput":{"command":"x"}}}"#;
        assert!(matches!(
            outcome(&pre, &ran(Some(0), allow, "")),
            Outcome::Proceed { note: Some(note), succeeded: true } if note.contains("not done here")
        ));
        let post = hook(Event::PostToolUse, "Edit");
        let context = r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"formatted 2 files"}}"#;
        assert_eq!(
            outcome(&post, &ran(Some(0), context, "")),
            Outcome::Proceed {
                note: Some("[PostToolUse hook of skill s: formatted 2 files]".into()),
                succeeded: true
            }
        );
        // Plain stdout is the transcript's natively, not the model's.
        assert_eq!(
            outcome(&post, &ran(Some(0), "done\n", "")),
            Outcome::Proceed {
                note: None,
                succeeded: true
            }
        );
    }

    #[test]
    fn a_failing_hook_is_said_once_and_does_not_stop_the_call() {
        let pre = hook(Event::PreToolUse, "Bash");
        assert!(matches!(
            outcome(&pre, &ran(Some(1), "", "boom")),
            Outcome::Proceed { note: Some(note), succeeded: false } if note.contains("exit 1") && note.contains("boom")
        ));
        let mut timed = ran(None, "", "").unwrap();
        timed.timed_out = true;
        assert!(matches!(
            outcome(&pre, &Ok(timed)),
            Outcome::Proceed {
                succeeded: false,
                ..
            }
        ));
    }

    #[test]
    fn a_skill_loaded_twice_registers_its_hooks_once_and_once_retires() {
        let registry = Registry::default();
        let mut first = hook(Event::PreToolUse, "Bash");
        first.once = true;
        registry.add(vec![first.clone(), hook(Event::PostToolUse, "Edit")]);
        registry.add(vec![first.clone()]);
        let pre = registry.for_event(Event::PreToolUse);
        assert_eq!(pre.len(), 1);
        registry.retire(pre[0].0);
        assert!(registry.for_event(Event::PreToolUse).is_empty());
        assert_eq!(registry.for_event(Event::PostToolUse).len(), 1);
    }

    #[test]
    fn the_hook_reads_the_native_input_on_stdin() {
        let view = View {
            tool_name: "Bash".into(),
            tool_input: json!({"command": "ls"}),
        };
        let bytes = input(
            Event::PostToolUse,
            "sid",
            Path::new("/w"),
            &view,
            Some("ok"),
        );
        let value: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["hook_event_name"], "PostToolUse");
        assert_eq!(value["tool_input"]["command"], "ls");
        assert_eq!(value["cwd"], "/w");
        assert_eq!(value["session_id"], "sid");
        assert_eq!(value["tool_response"], "ok");
    }
}

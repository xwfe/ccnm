//! Claude-specific MCP document and tool permission injection.
use crate::config::{AgentTool, AgentTools};
use crate::error::Result;
use crate::mcp::agent_skills;
use crate::mcp::server::SERVER_NAME;
use crate::process::Cmd;
use crate::session::{Dir, MCP_TOOLS, SSH_BIN, pretty};

/// Built-in tools that must never be available in a remote ccnm session
/// (docs/configuration.md, `agent_tools`). `--tools` already leaves out
/// every built-in tool but the workspace's `agent_tools`; this deny list
/// is the second lock, so that a future Claude that reads `--tools`
/// differently still cannot hand the model this machine's disk.
///
/// `NotebookEdit` and `Skill` joined in P46, when `--tools` was measured
/// to accept both names on 2.1.278: a notebook edit writes this machine's
/// disk and a skill is read from it. The project's own are served on the
/// Runtime by `read_notebook` and `load_skill`; the skills installed on
/// this machine, since P48, by `ccnm internal agent-skills`, which reads
/// only inside each skill's own directory -- the native `Skill` would need
/// `Read`, and a `Read` narrowed to the skills directories was measured to
/// still read the session's working directory, ccnm's state for it.
pub const NATIVE_TOOLS_DENIED: [&str; 8] = [
    "Read",
    "Edit",
    "Write",
    "Grep",
    "Glob",
    "Bash",
    "NotebookEdit",
    "Skill",
];

/// Claude Code's own names for one agent tool, as `--tools` and
/// settings.json spell them. Measured on 2.1.278 (P46). `--tools` drops a
/// name it does not know without a word, so a rename upstream would show
/// up as the feature quietly missing -- `TodoWrite` and `Task`, the older
/// names, already are.
pub fn tool_names(tool: AgentTool) -> &'static [&'static str] {
    match tool {
        AgentTool::WebSearch => &["WebSearch"],
        AgentTool::WebFetch => &["WebFetch"],
        // A sub-agent runs in the background by default, and TaskStop is
        // how the model stops one.
        AgentTool::Subagents => &["Agent", "TaskStop"],
        AgentTool::Tasks => &["TaskCreate", "TaskGet", "TaskList", "TaskUpdate"],
        // Not a native tool: the Agent's servers come through ccnm's own
        // server on this machine (P50), allowed with it, never through
        // Claude Code's MCP config, which `--strict-mcp-config` keeps to
        // ccnm's.
        AgentTool::McpServers => &[],
    }
}

fn names(agent_tools: &AgentTools, enabled: bool) -> Vec<&'static str> {
    AgentTool::ALL
        .into_iter()
        .filter(|tool| agent_tools.contains(*tool) == enabled)
        .flat_map(|tool| tool_names(tool).iter().copied())
        .collect()
}

/// The `--tools` value of a remote session: the enabled agent tools and
/// nothing else, so an empty set is the `--tools ""` every remote session
/// had before P46.
///
/// `ToolSearch` is never in it. Measured: with it in the list, every MCP
/// tool (and WebSearch) moves into the deferred pool and the model sees
/// only ToolSearch; without it they stay loaded, even with tool search
/// forced on.
pub fn tools_flag(agent_tools: &AgentTools) -> String {
    names(agent_tools, true).join(",")
}

/// `mcp.json`: ccnm's server, and ccnm's server on this machine -- its
/// installed skills (P48) and MCP servers (P50) -- when this session gets
/// one. `--strict-mcp-config` keeps every other server out, so these two
/// are all there is; this machine's own MCP servers reach the session
/// through the second, never as entries here.
pub fn mcp_config(cmd: &Cmd, agent_server: Option<&agent_skills::Recorded>) -> serde_json::Value {
    let args: Vec<String> = cmd
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let program = if cmd.program == "ssh" {
        SSH_BIN.into()
    } else {
        cmd.program.to_string_lossy()
    };
    let mut servers = serde_json::json!({
        SERVER_NAME: {
            "type": "stdio",
            "command": program,
            "args": args,
        }
    });
    if let Some(recorded) = agent_server {
        servers[agent_skills::SERVER_NAME] = serde_json::json!({
            "type": "stdio",
            "command": recorded.command,
            "args": recorded.args,
        });
    }
    serde_json::json!({ "mcpServers": servers })
}

/// The context size a remote session compacts at, instead of the model's
/// own window (1M on Opus 5.5). Measured on a real session (P87): 1M let
/// the context reach 969 000 tokens before the first compaction, and every
/// request re-reads the whole context, so 70% of the session's cost was
/// cache reads. Claude Code clears old results of its native tools on its
/// own, but never of `mcp__ccnm__*` (2.1.286 bundle), so a remote session
/// leans on compaction more than a native one. Lower means cheaper requests
/// and more compactions, each losing detail; the user chose 300 000 (P87),
/// then 560 000 (P88). Claude Code compacts a little below it: at 560 000,
/// a reported 550 000 compacted and 490 000 did not (2.1.286 and 2.1.296).
/// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` in the environment still wins over it.
pub const AUTO_COMPACT_WINDOW: u32 = 560_000;

/// What a compaction is told to keep, on top of Claude Code's own summary
/// structure: the state a remote session cannot get back by itself after
/// the summary (background commands are only reachable by `output_ref`,
/// files only through the Runtime), and what can be fetched again and so
/// should not be kept.
///
/// P88 fitted it to Claude Code's own prompt (2.1.286 bundle): its section
/// "Files and Code Sections" asks for full code snippets, which item 3
/// narrows instead of contradicting; after compaction it restores files
/// and skills only for its native Read and Skill, so the summary names the
/// files to read first and says to load skills again; and the message after
/// the summary points at a transcript on this machine, which the
/// `mcp__ccnm__` tools cannot read, hence the last line it asks for.
///
/// Printed by the PreCompact hook between single quotes, so it must never
/// contain one; the test runs the hook through a shell to hold that.
pub const COMPACT_INSTRUCTIONS: &str = "\
This session works on a remote ccnm workspace through the mcp__ccnm__ tools; nothing it changed lives on this machine. After compaction the summary is all that is left: Claude Code restores no mcp__ccnm__ result and no load_skill text (it does that only for its own Read and Skill tools), and the transcript it points to afterwards is on this machine, which the mcp__ccnm__ tools cannot reach. Keep exact details in the summary. In it:
1. Open with a Status block of at most five lines, before section 1: the goal; what is done; what is in progress; what is blocked or waiting on the user; the next action.
2. Keep the requests, decisions and limits the user gave (what they allowed, what they ruled out) close to their words; where a later message changed an earlier one, say which one holds now.
3. List every file created, changed, moved or deleted, by workspace-relative path, with one line on what changed and why. List commits made (short hash and subject) and what is still uncommitted. Under Files and Code Sections, quote code only where the next step depends on the exact lines (an edit in progress, a signature to match), with path and line numbers; the rest can be read again.
4. For every test, build or check that still fails: the exact command, the failing names and the decisive error lines verbatim. One line for each that now passes. Name the command that checks the current work.
5. Keep facts about the workspace that took effort to find: how to build, test and run it, paths, ports, versions, environment variables, and commands that did not work there and why.
6. For every background command still running (exec_command with run_in_background): its output_ref, its command and what it is for, so it can still be read with read_output or stopped with stop_command.
7. For each skill loaded with load_skill: its name and the rules from it that still apply. Its text is gone after compaction; load it again before following its steps.
8. Keep approaches that were tried and dropped, one line each with the reason, so they are not tried again.
9. Leave out file contents, long command output and search results. Instead name up to five files, with line ranges, to read first when work resumes.
10. End with the exact next step as a tool call or command, then this line as written: The transcript path after this summary is on the machine running Claude Code; the mcp__ccnm__ tools cannot read it.
After the summary, versions returned by earlier read_file calls are stale: read a file again before apply_patch changes it.";

/// The PreCompact hook: Claude Code appends a hook's stdout to the summary
/// prompt as "Additional Instructions" (2.1.296: "Exit code 0 - stdout
/// appended as custom compact instructions"), for automatic compactions
/// and `/compact` alike. It runs on this machine, the one holding the
/// login, so it is ccnm's fixed text and nothing else: it reads neither
/// its stdin (the compaction details) nor anything from the project.
/// `printf` is a shell builtin, so no PATH is involved.
fn compact_hook() -> serde_json::Value {
    serde_json::json!({
        "PreCompact": [{
            "hooks": [{
                "type": "command",
                "command": format!("printf '%s\\n' '{COMPACT_INSTRUCTIONS}'"),
            }]
        }]
    })
}

/// The `--settings` file: permission to call each ccnm tool and each
/// enabled agent tool without a prompt (there is nobody to answer one in
/// print mode), and the native file and shell tools plus the agent tools
/// this workspace left off, denied by name. Nothing else — the user's own
/// settings still load underneath this.
///
/// The enabled agent tools have to be in `allow`: measured on 2.1.278,
/// print mode denies WebSearch and WebFetch automatically otherwise
/// ("this session has no approval surface"). Agent and TaskCreate ran
/// either way; they are allowed too, so that turning a switch on means
/// the same thing for every one of them.
///
/// `remote` is false for a colocated workspace, and then none of this
/// applies: there are no ccnm tools to allow, and denying the native ones
/// would leave Claude with no way to touch the project it is sitting on.
/// The deny list exists to stop the model reaching *this* machine's disk
/// when the project is on another one; when the project is this machine's
/// disk, it would only be in the way.
///
/// `agent_server`: the permissions of the Agent's own server's tools, when
/// the session has it -- `load_skill` (P48), `call_mcp_tool` and
/// `read_mcp_result` (P50) -- allowed like ccnm's.
///
/// And when to compact and what to keep (P87): [`AUTO_COMPACT_WINDOW`] and
/// the [`compact_hook`]. This file is Claude Code's `--settings`, which
/// outranks the user's own settings files, so a user's own
/// `autoCompactWindow` does not apply in a remote session; the environment
/// variable does. A Claude Code that does not know the key ignores it
/// (an unknown key was measured not to disturb the permissions).
///
/// `session_hook`: for an interactive session, the
/// [`crate::session::hook`] command, run on `SessionStart` and `Stop`
/// (P101, P102). Hooks listed here run alongside the user's own; a Claude
/// Code with hooks turned off by the user or a managed policy just runs
/// none of them, and `--continue` then falls back to the session's first
/// conversation.
pub fn settings(
    remote: bool,
    agent_tools: &AgentTools,
    agent_server: &[String],
    session_hook: Option<&str>,
) -> serde_json::Value {
    if !remote {
        return serde_json::json!({ "permissions": {} });
    }
    let allow: Vec<String> = MCP_TOOLS
        .iter()
        .map(|t| format!("mcp__{SERVER_NAME}__{t}"))
        .chain(agent_server.iter().cloned())
        .chain(names(agent_tools, true).into_iter().map(str::to_string))
        .collect();
    let deny: Vec<&str> = NATIVE_TOOLS_DENIED
        .into_iter()
        .chain(names(agent_tools, false))
        .collect();
    let mut hooks = compact_hook();
    if let Some(command) = session_hook {
        // No matcher: every source of SessionStart (startup, clear,
        // compact, fork, resume) and every Stop.
        let run = serde_json::json!([{
            "hooks": [{ "type": "command", "command": command }]
        }]);
        hooks["SessionStart"] = run.clone();
        hooks["Stop"] = run;
    }
    serde_json::json!({
        "permissions": {
            "allow": allow,
            "deny": deny,
        },
        "autoCompactWindow": AUTO_COMPACT_WINDOW,
        "hooks": hooks,
    })
}

pub(crate) fn write_session_files(
    dir: &Dir,
    transport: Option<&Cmd>,
    agent_tools: &AgentTools,
    agent_server: Option<&agent_skills::Recorded>,
    session_hook: Option<&str>,
) -> Result<()> {
    if let Some(cmd) = transport {
        std::fs::write(dir.mcp_config(), pretty(&mcp_config(cmd, agent_server))?)?;
    }
    let permissions = agent_server
        .map(agent_skills::Recorded::permissions)
        .unwrap_or_default();
    std::fs::write(
        dir.settings(),
        pretty(&settings(
            transport.is_some(),
            agent_tools,
            &permissions,
            session_hook.filter(|_| transport.is_some()),
        ))?,
    )?;
    Ok(())
}

pub fn transport_payload(dir: &Dir) -> Option<String> {
    let text = std::fs::read_to_string(dir.mcp_config()).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let args = config
        .pointer(&format!("/mcpServers/{}/args", SERVER_NAME))?
        .as_array()?;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg.as_str() == Some("--payload") {
            return it.next()?.as_str().map(str::to_string);
        }
    }
    None
}

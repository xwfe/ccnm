//! Claude-specific MCP document and tool permission injection.
use crate::config::{AgentTool, AgentTools};
use crate::error::Result;
use crate::mcp::agent_skills;
use crate::mcp::server::SERVER_NAME;
use crate::process::Cmd;
use crate::session::{Dir, MCP_TOOLS, SSH_BIN, pretty};

/// Built-in tools that must never be available in a remote ccnm session
/// (design doc section 13). `--tools` already leaves out every built-in
/// tool but the workspace's `agent_tools`; this deny list is the second
/// lock, so that a future Claude that reads `--tools` differently still
/// cannot hand the model this machine's disk.
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

/// The `--settings` file: permission to call each ccnm tool and each
/// enabled agent tool without a prompt (there is nobody to answer one in
/// print mode), and the native file and shell tools plus the agent tools
/// this workspace left off, denied by name. Nothing else — the user's own
/// settings still load underneath this (design doc section 24).
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
pub fn settings(
    remote: bool,
    agent_tools: &AgentTools,
    agent_server: &[String],
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
    serde_json::json!({
        "permissions": {
            "allow": allow,
            "deny": deny,
        }
    })
}

pub(crate) fn write_session_files(
    dir: &Dir,
    transport: Option<&Cmd>,
    agent_tools: &AgentTools,
    agent_server: Option<&agent_skills::Recorded>,
) -> Result<()> {
    if let Some(cmd) = transport {
        std::fs::write(dir.mcp_config(), pretty(&mcp_config(cmd, agent_server))?)?;
    }
    let permissions = agent_server
        .map(agent_skills::Recorded::permissions)
        .unwrap_or_default();
    std::fs::write(
        dir.settings(),
        pretty(&settings(transport.is_some(), agent_tools, &permissions))?,
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

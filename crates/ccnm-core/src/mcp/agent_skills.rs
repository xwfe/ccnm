//! The Agent Node's own installed skills (P48) and MCP servers (P50), for a
//! remote session.
//!
//! `ccnm internal agent-skills` is a second, small MCP server that Claude
//! Code or Codex start next to ccnm's, **on the Agent Node**: the skills in
//! this account's home are here, and the Runtime cannot read this disk.
//! `load_skill`, the same code as the Runtime's over a different scope,
//! plus the same prompts; since P50 also `call_mcp_tool` and
//! `read_mcp_result` over the MCP servers installed here (`agent_mcp.rs`).
//! Each half is there only when this machine and the workspace allow it;
//! the command keeps its P48 name, which sessions already created carry.
//!
//! Why not the natives' own skills: measured on Claude Code 2.1.278 and
//! Codex 0.154.0 with ccnm's launch flags (toexec
//! `evidence/v3-parity/machine-skills/`). Claude's `Skill` loads the text
//! but its files need `Read`, and a `Read` narrowed to the skills
//! directories still reads the session's working directory -- ccnm's state
//! for the session -- without asking. Codex lists them and expects its
//! shell to open them, and ccnm turns that shell off. Both also bring
//! skills of their own that assume the tools ccnm takes away. This reads
//! only inside one skill's own directory, never a dotfile, only text.
//!
//! Nothing here reaches the project. A script of one of these skills runs
//! against the project only if the model writes it into the workspace
//! first, through ccnm's own tools and their approval.

use std::path::{Path, PathBuf};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    ListPromptsResult, Prompt, PromptMessage, Role, ServerCapabilities, ServerInfo,
};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::config::{AgentMcp, MachineSkills};
use crate::error::Error;
use crate::mcp::agent_mcp::{self, McpPayload, ReadMcpResultArgs};
use crate::mcp::machine_skills::{self, Machine};
use crate::mcp::relay::{self, CallMcpToolArgs};
use crate::mcp::server::{
    MAX_RESULT_CHARS, MAX_RESULT_SIZE, prompt_arguments, quote_argument, text_only, tool_error,
};
use crate::mcp::skills::{self, LoadSkillArgs, Scope, Wording};
use crate::mcp::with_ignored;
use crate::process::Cmd;
use crate::protocol::payload::Protocol;

type CcnmResult<T> = crate::error::Result<T>;

/// The server's name in the session's MCP config, so its tool reaches the
/// model as `mcp__ccnm_agent__load_skill`. An underscore, not a hyphen:
/// the same name is a TOML key for Codex.
pub const SERVER_NAME: &str = "ccnm_agent";
/// What `settings.json` has to allow for its skill tool (print mode asks
/// nobody). Each tool is allowed by the name [`permission`] gives it.
pub const TOOL_PERMISSION: &str = "mcp__ccnm_agent__load_skill";

/// How Claude Code names one of this server's tools in its permissions.
pub fn permission(tool: &str) -> String {
    format!("mcp__{SERVER_NAME}__{tool}")
}

pub const AGENT: Wording = Wording {
    intro: "Load a skill installed on the machine you run on. That is not the project machine: the project's own skills, and the ones installed there, come from ccnm's load_skill -- when both offer a skill of the same name, use that one. Before starting a task that a skill below describes, call this with its name and follow what comes back. Its other files are on this machine, not in the workspace: read one with this tool's file argument; to run a script against the project, write it into the workspace with apply_patch and run it there. Without a name this returns the full list.",
    project: "Skills in this workspace:",
    installed: "Installed here:",
    empty: "Nothing is installed here right now.",
    count: "{n} skills are installed here; call this without a name to list them.",
};

/// What `ccnm internal agent-skills --payload` carries. It is started on
/// this machine by Claude Code or Codex, not over ssh, but the payload is
/// how every internal command is called (design doc section 8), and it
/// keeps the home and the session out of the environment: Codex hands an
/// MCP server only some of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub protocol: u32,
    pub home: PathBuf,
    pub session: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// `false` when this machine's installed skills are off and the server
    /// runs only for its MCP servers (P50). Absent in a P48 payload.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub skills: bool,
    /// The installed MCP servers, when this session may use them (P50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpPayload>,
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl Protocol for Payload {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// How Claude Code or Codex start this server for one session, and the
/// tools it may offer; `None` when this machine shares neither its
/// installed skills nor its MCP servers with it. `mcp` is this machine's
/// `[agent_mcp]` when the workspace lets its sessions use them.
pub fn launcher(
    exe: &Path,
    session: &str,
    config: &MachineSkills,
    mcp: Option<&AgentMcp>,
) -> CcnmResult<Option<Recorded>> {
    let mcp = mcp.filter(|mcp| mcp.enabled).map(McpPayload::of);
    if !config.enabled && mcp.is_none() {
        return Ok(None);
    }
    let mut tools = Vec::new();
    if config.enabled {
        tools.push(skills::TOOL.to_string());
    }
    if mcp.is_some() {
        tools.extend([relay::TOOL.to_string(), agent_mcp::READ_TOOL.to_string()]);
    }
    let payload = Payload {
        protocol: crate::protocol::payload::PROTOCOL,
        home: crate::paths::home_dir()?,
        session: session.to_string(),
        hidden: config.hidden.iter().cloned().collect(),
        skills: config.enabled,
        mcp,
    };
    let cmd = Cmd::new(exe).args([
        "internal",
        "agent-skills",
        "--payload",
        &crate::protocol::payload::encode(&payload)?,
    ]);
    Ok(Some(Recorded {
        tools,
        ..Recorded::of(&cmd)
    }))
}

/// The launcher as the session directory keeps it, for a provider that
/// builds its command line at launch rather than writing files at create
/// (Codex). Its presence is the record that the session got the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub command: String,
    pub args: Vec<String>,
    /// The tools the server may offer this session, which the client is
    /// told to allow. A P48 session's file has no such list: it had
    /// `load_skill` only.
    #[serde(default = "skills_only")]
    pub tools: Vec<String>,
}

fn skills_only() -> Vec<String> {
    vec![skills::TOOL.to_string()]
}

impl Recorded {
    pub fn of(cmd: &Cmd) -> Recorded {
        Recorded {
            command: cmd.program.to_string_lossy().into_owned(),
            args: cmd
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            tools: skills_only(),
        }
    }

    /// As Claude Code's permission rules name the tools.
    pub fn permissions(&self) -> Vec<String> {
        self.tools.iter().map(|tool| permission(tool)).collect()
    }

    pub fn read(path: &Path) -> CcnmResult<Option<Recorded>> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                Error::internal(format!("cannot read {}", path.display())).with_source(e)
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::from(e)),
        }
    }
}

#[derive(Clone)]
pub struct AgentSkills {
    /// `None` when this machine's installed skills are off.
    machine: Option<Machine>,
    /// `None` when this session gets no MCP servers from here.
    mcp: Option<Arc<agent_mcp::Relay>>,
    session: String,
    tool_router: ToolRouter<Self>,
}

impl AgentSkills {
    pub fn new(payload: &Payload) -> CcnmResult<AgentSkills> {
        let home = &payload.home;
        if !home.is_dir() {
            return Err(Error::config(format!(
                "{} is not a directory here",
                home.display()
            )));
        }
        let machine = if payload.skills {
            let config = MachineSkills {
                enabled: true,
                hidden: payload.hidden.iter().cloned().collect(),
            };
            Some(
                Machine::new(&config, home, machine_skills::Role::Agent).ok_or_else(|| {
                    Error::config(format!("{} is not a directory here", home.display()))
                })?,
            )
        } else {
            None
        };
        Ok(AgentSkills {
            machine,
            mcp: payload
                .mcp
                .as_ref()
                .map(|mcp| Arc::new(agent_mcp::Relay::new(mcp, home))),
            session: payload.session.clone(),
            tool_router: Self::tool_router(),
        })
    }

    /// `None` when the skills half is off.
    fn scope(&self) -> Option<Scope<'_>> {
        self.machine.as_ref().map(|machine| Scope {
            project: None,
            machine: Some(machine),
            wording: &AGENT,
        })
    }

    /// The MCP half, when it has at least one server to offer.
    fn relayed(&self) -> Option<&Arc<agent_mcp::Relay>> {
        self.mcp.as_ref().filter(|mcp| mcp.offered())
    }

    fn tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router
            .list_all()
            .into_iter()
            .filter_map(|mut tool| {
                let read_only = match tool.name.as_ref() {
                    skills::TOOL => {
                        let scope = self.scope()?;
                        tool.description = Some(skills::discover(&scope).description().into());
                        true
                    }
                    relay::TOOL => {
                        tool.description = Some(self.relayed()?.description().into());
                        false
                    }
                    agent_mcp::READ_TOOL => {
                        self.relayed()?;
                        true
                    }
                    _ => return None,
                };
                // A relayed call reaches whatever that server reaches.
                let tool = tool.annotate(rmcp::model::ToolAnnotations::from_raw(
                    None,
                    Some(read_only),
                    None,
                    None,
                    Some(!read_only),
                ));
                // Both can return 64 KiB, past the line where Claude Code
                // saves a result to disk instead (`server::LARGE_RESULT_TOOLS`).
                Some(if tool.name != agent_mcp::READ_TOOL {
                    let mut meta = serde_json::Map::new();
                    meta.insert(MAX_RESULT_SIZE.into(), MAX_RESULT_CHARS.into());
                    tool.with_meta(rmcp::model::MetaObject(meta))
                } else {
                    tool
                })
            })
            .collect()
    }

    fn mcp_off() -> ErrorData {
        ErrorData::invalid_params("this session gets no MCP servers from this machine", None)
    }
}

#[tool_router]
impl AgentSkills {
    // Replaced in `tools()` by the catalog, which an attribute cannot see.
    #[tool(
        name = "load_skill",
        description = "Load a skill installed on this machine."
    )]
    async fn load_skill(
        &self,
        Parameters(args): Parameters<LoadSkillArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        if self.machine.is_none() {
            return Err(ErrorData::invalid_params(
                "this machine shares no installed skills",
                None,
            ));
        }
        let ignored = args.ignored.note();
        let this = self.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let scope = this.scope().expect("checked above");
            skills::load_skill(&scope, &args, Some(&this.session))
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("load_skill task failed: {e}"), None))?;
        match loaded {
            Ok(text) => Ok(text_only(with_ignored(text, ignored))),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    // Replaced in `tools()` by the servers' names.
    #[tool(
        name = "call_mcp_tool",
        description = "Use an MCP server installed on this machine."
    )]
    async fn call_mcp_tool(
        &self,
        Parameters(args): Parameters<CallMcpToolArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let mcp = self.mcp.clone().ok_or_else(Self::mcp_off)?;
        let called = tokio::task::spawn_blocking(move || mcp.call(&args))
            .await
            .map_err(|e| {
                ErrorData::internal_error(format!("call_mcp_tool task failed: {e}"), None)
            })?;
        Ok(called.unwrap_or_else(|err| tool_error(&err)))
    }

    #[tool(
        name = "read_mcp_result",
        description = "Read on in a call_mcp_tool result too long to return at once, from the ref and offset its last line gives. Kept for 30 minutes of this session."
    )]
    async fn read_mcp_result(
        &self,
        Parameters(args): Parameters<ReadMcpResultArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let mcp = self.mcp.clone().ok_or_else(Self::mcp_off)?;
        Ok(mcp
            .read_result(&args)
            .unwrap_or_else(|err| tool_error(&err)))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentSkills {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> std::result::Result<rmcp::model::ListToolsResult, ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(self.tools()))
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }

    /// The same skills as prompts, the way a person starts one: Claude Code
    /// shows them as `/mcp__ccnm_agent__<name>`. A skill marked
    /// `disable-model-invocation` is reachable only this way.
    async fn list_prompts(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> std::result::Result<ListPromptsResult, ErrorData> {
        let Some(scope) = self.scope() else {
            return Ok(ListPromptsResult::with_all_items(Vec::new()));
        };
        let catalog = skills::discover(&scope);
        let prompts = catalog
            .skills
            .iter()
            .filter(|skill| skill.user_invocable)
            .map(|skill| {
                Prompt::new(
                    skill.name.clone(),
                    Some(skill.description.clone()),
                    Some(prompt_arguments(skill)),
                )
            })
            .collect();
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> std::result::Result<GetPromptResponse, ErrorData> {
        let scope = self.scope().ok_or_else(|| {
            ErrorData::invalid_params("this machine shares no installed skills", None)
        })?;
        let given = request.arguments.unwrap_or_default();
        let declared = skills::discover(&scope)
            .skills
            .iter()
            .find(|skill| skill.name == request.name)
            .map(prompt_arguments)
            .unwrap_or_default();
        let line = declared
            .iter()
            .filter_map(|arg| given.get(&arg.name))
            .map(|value| match value {
                serde_json::Value::String(text) => quote_argument(text),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let text = skills::prompt_text(&scope, &request.name, &line, Some(&self.session))
            .map_err(|err| ErrorData::invalid_params(err.to_string(), None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(if self.machine.is_some() {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build()
        } else {
            ServerCapabilities::builder().enable_tools().build()
        })
        .with_server_info(Implementation::new(SERVER_NAME, crate::VERSION))
    }
}

/// Serve on stdin/stdout until the client closes them. The client is a
/// process on this machine, so there is no link to keep alive.
pub fn serve(payload: &Payload) -> CcnmResult<()> {
    let server = AgentSkills::new(payload)?;
    let mcp = server.mcp.clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal("cannot start tokio runtime").with_source(e))?;
    let served = rt.block_on(async {
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| Error::internal("MCP initialize failed").with_source(e))?;
        service
            .waiting()
            .await
            .map_err(|e| Error::internal("MCP service task panicked").with_source(e))?;
        Ok::<(), Error>(())
    });
    // The servers this session started go with it.
    if let Some(mcp) = mcp {
        mcp.close_all();
    }
    served
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn home(name: &str) -> TestDir {
        let dir =
            std::env::temp_dir().join(format!("ccnm-agent-skills-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    #[test]
    fn the_launcher_carries_home_session_and_what_is_hidden_and_off_is_none() {
        let exe = Path::new("/opt/ccnm/bin/ccnm");
        let config = MachineSkills {
            enabled: true,
            hidden: ["pdf".to_string(), "noise".to_string()]
                .into_iter()
                .collect(),
        };
        let recorded = launcher(exe, "sess-1", &config, None).unwrap().unwrap();
        assert_eq!(recorded.command, "/opt/ccnm/bin/ccnm");
        assert_eq!(
            recorded.args[..3],
            ["internal", "agent-skills", "--payload"]
        );
        assert_eq!(recorded.tools, ["load_skill"]);
        let payload: Payload = crate::protocol::payload::decode(&recorded.args[3]).unwrap();
        assert_eq!(
            payload,
            Payload {
                protocol: crate::protocol::payload::PROTOCOL,
                home: crate::paths::home_dir().unwrap(),
                session: "sess-1".into(),
                hidden: vec!["noise".into(), "pdf".into()],
                skills: true,
                mcp: None,
            }
        );
        // A P48 payload says nothing of either half, and reads the same.
        assert_eq!(
            crate::protocol::payload::encode(&payload).unwrap(),
            recorded.args[3]
        );
        let off = MachineSkills {
            enabled: false,
            ..Default::default()
        };
        assert!(launcher(exe, "sess-1", &off, None).unwrap().is_none());
    }

    /// P50: with the workspace's leave and this machine's `[agent_mcp]`,
    /// the same server carries the MCP half -- also when the skills are
    /// off -- and the session is told to allow its two tools.
    #[test]
    fn the_mcp_half_rides_along_when_allowed_and_on() {
        let exe = Path::new("/opt/ccnm/bin/ccnm");
        let mcp = AgentMcp {
            local: ["context7".to_string()].into(),
            ..AgentMcp::default()
        };
        let both = launcher(exe, "s", &MachineSkills::default(), Some(&mcp))
            .unwrap()
            .unwrap();
        assert_eq!(
            both.tools,
            ["load_skill", "call_mcp_tool", "read_mcp_result"]
        );
        assert_eq!(
            both.permissions(),
            [
                "mcp__ccnm_agent__load_skill",
                "mcp__ccnm_agent__call_mcp_tool",
                "mcp__ccnm_agent__read_mcp_result"
            ]
        );
        let payload: Payload = crate::protocol::payload::decode(&both.args[3]).unwrap();
        assert_eq!(payload.mcp.unwrap().local, ["context7"]);

        let skills_off = MachineSkills {
            enabled: false,
            ..Default::default()
        };
        let only_mcp = launcher(exe, "s", &skills_off, Some(&mcp))
            .unwrap()
            .unwrap();
        assert_eq!(only_mcp.tools, ["call_mcp_tool", "read_mcp_result"]);
        let payload: Payload = crate::protocol::payload::decode(&only_mcp.args[3]).unwrap();
        assert!(!payload.skills);

        let mcp_off = AgentMcp {
            enabled: false,
            ..AgentMcp::default()
        };
        assert!(
            launcher(exe, "s", &skills_off, Some(&mcp_off))
                .unwrap()
                .is_none()
        );
        // A file written by P48 has no tool list and meant load_skill.
        let old: Recorded =
            serde_json::from_str(r#"{"command":"/c","args":["internal","agent-skills"]}"#).unwrap();
        assert_eq!(old.tools, ["load_skill"]);
    }

    fn payload(home: &Path, hidden: &[&str], skills: bool, mcp: Option<McpPayload>) -> Payload {
        Payload {
            protocol: crate::protocol::payload::PROTOCOL,
            home: home.to_path_buf(),
            session: "s".into(),
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
            skills,
            mcp,
        }
    }

    #[test]
    fn each_half_lists_its_tools_only_when_there_is_something_behind_them() {
        let h = home("halves");
        fs::write(
            h.join(".claude.json"),
            r#"{ "mcpServers": { "web": { "type": "http", "url": "https://example.invalid/mcp" } } }"#,
        )
        .unwrap();
        let mcp = McpPayload::of(&AgentMcp::default());
        // In the router's order, which is by name.
        let names = |server: &AgentSkills| -> Vec<String> {
            server.tools().iter().map(|t| t.name.to_string()).collect()
        };
        let both = AgentSkills::new(&payload(&h, &[], true, Some(mcp.clone()))).unwrap();
        assert_eq!(
            names(&both),
            ["call_mcp_tool", "load_skill", "read_mcp_result"]
        );
        let call = both
            .tools()
            .into_iter()
            .find(|t| t.name == "call_mcp_tool")
            .unwrap();
        assert!(
            call.description
                .as_deref()
                .unwrap()
                .ends_with("Servers here: web."),
            "{:?}",
            call.description
        );
        let value = serde_json::to_value(&call).unwrap();
        assert_eq!(value["_meta"]["anthropic/maxResultSizeChars"], 200_000);
        assert_eq!(value["annotations"]["readOnlyHint"], false);

        let only_mcp = AgentSkills::new(&payload(&h, &[], false, Some(mcp))).unwrap();
        assert_eq!(names(&only_mcp), ["call_mcp_tool", "read_mcp_result"]);
        assert!(only_mcp.get_info().capabilities.prompts.is_none());

        // Nothing to relay: the tools stay out, like the Runtime's (P49).
        let empty = home("halves-empty");
        let none = AgentSkills::new(&payload(
            &empty,
            &[],
            true,
            Some(McpPayload::of(&AgentMcp::default())),
        ))
        .unwrap();
        assert_eq!(names(&none), ["load_skill"]);
    }

    #[test]
    fn the_tool_carries_this_machines_catalog_in_its_own_words() {
        let h = home("catalog");
        let dir = h.join(".agents/skills/pdf");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\ndescription: Fill PDF forms.\n---\nbody\n",
        )
        .unwrap();
        let skill = h.join(".claude/skills/noise");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\ndescription: Noise.\n---\nbody\n",
        )
        .unwrap();

        let server = AgentSkills::new(&payload(&h, &["noise"], true, None)).unwrap();
        let tools = server.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "load_skill");
        let text = tools[0].description.as_deref().unwrap();
        assert!(
            text.starts_with("Load a skill installed on the machine you run on."),
            "{text}"
        );
        assert!(
            text.ends_with("Installed here:\n- pdf: Fill PDF forms."),
            "{text}"
        );
        assert!(!text.contains("noise"), "{text}");

        let empty = home("empty");
        let bare = AgentSkills::new(&payload(&empty, &[], true, None)).unwrap();
        assert!(
            bare.tools()[0]
                .description
                .as_deref()
                .unwrap()
                .ends_with("Nothing is installed here right now.")
        );
    }
}

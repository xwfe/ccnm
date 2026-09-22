//! The Agent Node's own installed skills, for a remote session (P48).
//!
//! `ccnm internal agent-skills` is a second, small MCP server that Claude
//! Code or Codex start next to ccnm's, **on the Agent Node**: the skills in
//! this account's home are here, and the Runtime cannot read this disk.
//! One tool, `load_skill`, the same code as the Runtime's over a different
//! scope, plus the same prompts.
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

use crate::config::MachineSkills;
use crate::error::Error;
use crate::mcp::machine_skills::{self, Machine};
use crate::mcp::server::{prompt_arguments, quote_argument, text_only, tool_error};
use crate::mcp::skills::{self, LoadSkillArgs, Scope, Wording};
use crate::mcp::with_ignored;
use crate::process::Cmd;
use crate::protocol::payload::Protocol;

type CcnmResult<T> = crate::error::Result<T>;

/// The server's name in the session's MCP config, so its tool reaches the
/// model as `mcp__ccnm_agent__load_skill`. An underscore, not a hyphen:
/// the same name is a TOML key for Codex.
pub const SERVER_NAME: &str = "ccnm_agent";
/// What `settings.json` has to allow for it (print mode asks nobody).
pub const TOOL_PERMISSION: &str = "mcp__ccnm_agent__load_skill";

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
}

impl Protocol for Payload {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// How Claude Code or Codex start this server for one session, or `None`
/// when this machine shares no installed skills.
pub fn launcher(exe: &Path, session: &str, config: &MachineSkills) -> CcnmResult<Option<Cmd>> {
    if !config.enabled {
        return Ok(None);
    }
    let payload = Payload {
        protocol: crate::protocol::payload::PROTOCOL,
        home: crate::paths::home_dir()?,
        session: session.to_string(),
        hidden: config.hidden.iter().cloned().collect(),
    };
    Ok(Some(Cmd::new(exe).args([
        "internal",
        "agent-skills",
        "--payload",
        &crate::protocol::payload::encode(&payload)?,
    ])))
}

/// The launcher as the session directory keeps it, for a provider that
/// builds its command line at launch rather than writing files at create
/// (Codex). Its presence is the record that the session got the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub command: String,
    pub args: Vec<String>,
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
        }
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
    machine: Machine,
    session: String,
    tool_router: ToolRouter<Self>,
}

impl AgentSkills {
    pub fn new(home: &Path, hidden: &[String], session: &str) -> CcnmResult<AgentSkills> {
        let config = MachineSkills {
            enabled: true,
            hidden: hidden.iter().cloned().collect(),
        };
        let machine = Machine::new(&config, home, machine_skills::Role::Agent)
            .ok_or_else(|| Error::config(format!("{} is not a directory here", home.display())))?;
        Ok(AgentSkills {
            machine,
            session: session.to_string(),
            tool_router: Self::tool_router(),
        })
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            project: None,
            machine: Some(&self.machine),
            wording: &AGENT,
        }
    }

    fn tools(&self) -> Vec<rmcp::model::Tool> {
        let catalog = skills::discover(&self.scope());
        self.tool_router
            .list_all()
            .into_iter()
            .map(|mut tool| {
                tool.description = Some(catalog.description().into());
                tool.annotate(rmcp::model::ToolAnnotations::from_raw(
                    None,
                    Some(true),
                    None,
                    None,
                    Some(false),
                ))
            })
            .collect()
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
        let ignored = args.ignored.note();
        let this = self.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            skills::load_skill(&this.scope(), &args, Some(&this.session))
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("load_skill task failed: {e}"), None))?;
        match loaded {
            Ok(text) => Ok(text_only(with_ignored(text, ignored))),
            Err(err) => Ok(tool_error(&err)),
        }
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
        let catalog = skills::discover(&self.scope());
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
        let scope = self.scope();
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
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new(SERVER_NAME, crate::VERSION))
    }
}

/// Serve on stdin/stdout until the client closes them. The client is a
/// process on this machine, so there is no link to keep alive.
pub fn serve(payload: &Payload) -> CcnmResult<()> {
    let server = AgentSkills::new(&payload.home, &payload.hidden, &payload.session)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal("cannot start tokio runtime").with_source(e))?;
    rt.block_on(async {
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| Error::internal("MCP initialize failed").with_source(e))?;
        service
            .waiting()
            .await
            .map_err(|e| Error::internal("MCP service task panicked").with_source(e))?;
        Ok(())
    })
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
        let cmd = launcher(exe, "sess-1", &config).unwrap().unwrap();
        let recorded = Recorded::of(&cmd);
        assert_eq!(recorded.command, "/opt/ccnm/bin/ccnm");
        assert_eq!(
            recorded.args[..3],
            ["internal", "agent-skills", "--payload"]
        );
        let payload: Payload = crate::protocol::payload::decode(&recorded.args[3]).unwrap();
        assert_eq!(
            payload,
            Payload {
                protocol: crate::protocol::payload::PROTOCOL,
                home: crate::paths::home_dir().unwrap(),
                session: "sess-1".into(),
                hidden: vec!["noise".into(), "pdf".into()],
            }
        );
        let off = MachineSkills {
            enabled: false,
            ..Default::default()
        };
        assert!(launcher(exe, "sess-1", &off).unwrap().is_none());
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

        let server = AgentSkills::new(&h, &["noise".to_string()], "s").unwrap();
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
        let bare = AgentSkills::new(&empty, &[], "s").unwrap();
        assert!(
            bare.tools()[0]
                .description
                .as_deref()
                .unwrap()
                .ends_with("Nothing is installed here right now.")
        );
    }
}

//! `ccnm internal mcp-serve`: the coding runtime Claude Code talks to over
//! one ssh. Phase 2 fills in the bounded tools of design doc section 15
//! one at a time; the set of section 14 is now complete:
//! `workspace_info`, `read_file`, `list_files`, `search_text`,
//! `apply_patch`, `exec_command` and `read_output`.
//!
//! Two rules are enforced here because everything later depends on them.
//! The workspace root is canonicalized once at startup, and every path
//! the server shows the model is relative to it (section 17) — with one
//! deliberate exception, documented at `patch::interrupted_report`: the
//! journal file a person has to delete to recover from an interrupted
//! patch. And nothing is written to
//! stdout except MCP: logs go to stderr through `tracing`, so a stray
//! `println!` cannot corrupt the JSON-RPC stream (section 8).
//!
//! A third rule shows up as soon as there is a tool that can fail. A tool
//! whose *work* failed returns `CallToolResult::error`, not `Err`. `Err`
//! becomes a JSON-RPC protocol error, which tells the client that the
//! call itself was malformed; the model may never see the text and cannot
//! react to it. "This path is outside the workspace" is a result the
//! model has to read, so it travels as a result with `isError: true`, and
//! its first line is the `CCNM_E_*` name from section 24.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse, GetPromptResult,
    Implementation, ListPromptsResult, Prompt, PromptArgument, PromptMessage, Role,
    ServerCapabilities, ServerInfo,
};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

// `crate::error::Result` is deliberately not imported: the `tool_handler`
// macro expands to `Result<_, ErrorData>` and would pick up the alias.
use crate::error::{Error, ErrorCode, ErrorReport};
use crate::mcp::context;
use crate::mcp::exec::{self, ExecCommandArgs};
use crate::mcp::list::{self, ListFilesArgs};
use crate::mcp::output::{self, ReadOutputArgs};
use crate::mcp::patch::{self, ApplyPatchArgs};
use crate::mcp::read::{self, ReadFileArgs};
use crate::mcp::retention;
use crate::mcp::sandbox;
use crate::mcp::search::{self, SearchTextArgs};
use crate::mcp::skills::{self, LoadSkillArgs};
use crate::process::{Cmd, ProcessRunner, SystemRunner};
use crate::protocol::mcp::ServePayload;

type CcnmResult<T> = crate::error::Result<T>;

/// `serverInfo.name` in the initialize response.
pub const SERVER_NAME: &str = "ccnm";

/// The `structuredContent` of `workspace_info`. Small on purpose: the
/// model needs to know where it is, not the server's environment.
/// `server_pid` and `calls_served` are the persistence evidence of design
/// doc section 27 (same pid and a counter that only goes up means one
/// process, hence one ssh, served every call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace: String,
    pub git: bool,
    /// Where the workspace root sits inside its git repository, when it is
    /// not the repository's top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_subdir: Option<String>,
    pub platform: String,
    /// False when the workspace root is no longer a directory on this
    /// machine. Checked on every call rather than remembered from
    /// startup, because it is the one fact here that can stop being true
    /// while the server runs.
    #[serde(default = "yes")]
    pub root_present: bool,
    pub server_pid: u32,
    pub calls_served: u64,
}

fn yes() -> bool {
    true
}

impl WorkspaceInfo {
    /// The whole text: the summary, then one bracketed line naming the
    /// server process and its call counter.
    ///
    /// Those two numbers are how the probe proves one server answered a
    /// whole session (design doc section 27). They ride in the text
    /// because the text is the only channel Claude Code shows the model,
    /// and a second channel nobody reads is not worth keeping in step.
    pub fn render(&self) -> String {
        format!(
            "{}\n[server pid {}, call {}]",
            self.summary(),
            self.server_pid,
            self.calls_served
        )
    }

    /// The pid and call counter back out of [`render`](Self::render)'s
    /// last line, for the probe.
    pub fn parse_server_line(text: &str) -> Option<(u32, u64)> {
        let line = text.lines().rev().find(|l| l.starts_with("[server pid "))?;
        let rest = line.strip_prefix("[server pid ")?.strip_suffix(']')?;
        let (pid, call) = rest.split_once(", call ")?;
        Some((pid.parse().ok()?, call.parse().ok()?))
    }

    /// One line about the workspace, without the server's bookkeeping.
    ///
    /// Two lines when the root has gone: a session is bound to the
    /// directory it started with, so if that directory is moved or deleted
    /// underneath it, every other tool starts failing for reasons that
    /// sound like something else -- `exec_command` reporting that
    /// `/bin/echo` is not installed, which is what actually happened on
    /// 2026-09-04. This is the tool the model calls to orient itself; it
    /// is the right place to say the ground is gone.
    pub fn summary(&self) -> String {
        let git = match (&self.git, &self.git_subdir) {
            (false, _) => "not a git repository".to_string(),
            (true, None) => "git repository root".to_string(),
            (true, Some(sub)) => format!("inside git repository at {sub}"),
        };
        let line = format!(
            "workspace {} ({git}, {}); all paths are relative to its root",
            self.workspace, self.platform
        );
        if self.root_present {
            return line;
        }
        format!(
            "{line}\nWARNING: the workspace root is not on this machine any more -- it was there when this session started, and every tool that touches a file or runs a command will fail until the session is restarted (ccnm stop <workspace>, then ccnm run <workspace>)"
        )
    }
}

/// Whether this session may run commands, and why.
///
/// Shared with the Codex exec-server entry (`crate::native::serve`), so both
/// are judged by one audit and one set of waivers.
///
/// The policy is read from *this* machine's config, not from the payload
/// the other machine sent. The payload says which workspace and where;
/// what the runtime account is allowed to do is a property of the machine
/// being protected, and a caller must not be able to widen it.
pub(crate) struct ExecGate {
    pub(crate) audit: crate::safety::Audit,
    pub(crate) config: Option<crate::Config>,
    /// What this workspace's own config accepted -- an unconfined runtime,
    /// an identity that can reach a known Agent login, or neither.
    pub(crate) accepted: crate::safety::Accepted,
}

impl ExecGate {
    pub(crate) fn decide(payload: &ServePayload) -> CcnmResult<ExecGate> {
        // The runtime host's own config, found the same way every other
        // ccnm command finds it. A missing config is not an error here:
        // it just means nothing has been declared, and nothing declared
        // means not confined.
        let config = crate::paths::effective_config_path()
            .and_then(|path| crate::Config::load(&path))
            .ok();
        match (&payload.binding, &config) {
            (Some(binding), Some(config)) => {
                if binding.workspace != payload.workspace
                    || binding.root != payload.root
                    || binding.agent.provider != payload.provider
                {
                    return Err(Error::policy("MCP binding differs from its payload"));
                }
                config.verify_runtime_binding(binding)?;
            }
            (Some(_), None) => {
                return Err(Error::config(
                    "bound Agent execution requires the Runtime's authoritative config",
                ));
            }
            // An external MCP client has no binding to verify and must not
            // be asked for one: it is not a managed Agent, and what
            // authorizes it is the workspace's own `external_mcp`, checked
            // before this server is constructed. Everything else this gate
            // decides — the audit, the runtime account, the unconfined
            // opt-in — applies to it unchanged.
            (None, Some(config))
                if !payload.entry.is_external()
                    && config
                        .workspaces
                        .get(&payload.workspace)
                        .is_some_and(|workspace| workspace.agent.is_some()) =>
            {
                return Err(Error::policy(
                    "instance-selected workspace requires a verified Runtime binding",
                ));
            }
            _ => {}
        }
        let expected = config.as_ref().and_then(|config| {
            let workspace = config.workspaces.get(&payload.workspace)?;
            let host = config.nodes.get(&workspace.runtime_node)?;
            host.runtime_user.clone()
        });
        // Read off **this** machine's config, never the request: the
        // account that runs the commands is the one entitled to say what
        // it accepts.
        let accepted = config
            .as_ref()
            .and_then(|config| config.workspaces.get(&payload.workspace))
            .map(|w| crate::safety::Accepted {
                unconfined_exec: w.allow_unconfined_exec,
                unisolated_credentials: w.allow_unisolated_credentials,
                unattended_exec: w.allow_unattended_exec,
            })
            .unwrap_or(crate::safety::Accepted::NOTHING);
        let home = crate::paths::home_dir().unwrap_or_else(|_| PathBuf::from("/nonexistent"));
        Ok(ExecGate {
            audit: crate::safety::audit(expected.as_deref(), &home, &SystemRunner),
            config,
            accepted,
        })
    }

    pub(crate) fn allowed(&self) -> bool {
        self.audit.exec_allowed(self.accepted)
    }

    /// The line every result of a session with a waiver carries.
    ///
    /// Two sentences rather than one because the second is a different
    /// order of admission, and a reader skimming a result log should not
    /// have to go and look up which switch was set.
    fn note(&self) -> Option<String> {
        (!self.audit.confined() && self.accepted.any()).then(|| {
            let mut note = format!(
                "this runtime is NOT confined (running as {}) and this workspace has allow_unconfined_exec set; a command here has the access that account has",
                self.audit.user
            );
            if self.accepted.unisolated_credentials {
                note.push_str(
                    "; it also has allow_unisolated_credentials set, so that account can read a known Agent login and so can anything the model runs",
                );
            }
            note
        })
    }
}

struct Inner {
    /// Held for this MCP process's complete lifetime.
    _write_guard: Option<crate::mcp::write_guard::WriteGuard>,
    provider: crate::provider::AgentProvider,
    workspace: String,
    /// Names the directory `exec_command` retains output in.
    session: String,
    /// `~/.local/state/ccnm`. Resolved once; a runtime that cannot find it
    /// still serves every read-only tool.
    state: Option<PathBuf>,
    /// This session's retained output, under `state`.
    output: Option<Arc<retention::Output>>,
    /// What the account this runtime runs as can reach, and whether this
    /// workspace has accepted it. Decided once at startup: the answer
    /// cannot change while the process lives, and re-running `id` and
    /// `sudo -n` on every call would be latency for nothing.
    exec_gate: ExecGate,
    /// The workspace's OS sandbox for `exec_command`, when its config asks
    /// for one (P33). Resolved once at startup and refused then if this
    /// Runtime cannot provide it.
    sandbox: Option<Arc<sandbox::Sandbox>>,
    /// Canonical. Never sent to the client.
    root: PathBuf,
    /// The project's own CLAUDE.md, as much of it as the handshake can
    /// carry. Read once at startup, like everything else here: the
    /// instructions are sent in the initialize response and cannot change
    /// afterwards, so re-reading the file mid-session would only produce a
    /// number that disagrees with what the model was given.
    project: Option<context::Project>,
    /// Further instruction files the project has, named in the handshake
    /// rather than carried in it.
    named: Vec<context::Named>,
    /// The project's skills as they were when the session started. The tool
    /// description is built from this and, like the instructions, is what
    /// the model was given: a client keeps `tools/list` for the life of the
    /// connection. A call re-scans, so a skill added since can still be
    /// loaded -- it just is not advertised until the next session.
    skills: skills::Catalog,
    git: bool,
    git_subdir: Option<String>,
    /// Somebody is at a terminal, so a permission prompt can be answered.
    interactive: bool,
    /// Which entry opened this server, and therefore whether it may write.
    entry: crate::protocol::mcp::Entry,
    calls: AtomicU64,
}

#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Open what this Runtime resolved for itself, from a request that
    /// never named a path (P7.4 Batch B).
    ///
    /// It goes through [`Server::new`] rather than around it: the answer
    /// from [`crate::runtime::open`] is not a capability token, so the
    /// binding, the audit, the canonicalization and the write guard are all
    /// done again here, against the config as it is now.
    pub fn open(request: &crate::runtime::OpenPayload) -> CcnmResult<Self> {
        let config = crate::Config::load(&crate::paths::effective_config_path()?)?;
        let opened = crate::runtime::open(&config, request)?;
        Self::new(&opened.serve_payload(request))
    }

    /// Resolve the root and look at git once. Fails with
    /// `CCNM_E_WRONG_WORKSPACE` if the root is not a directory here, which
    /// the launcher sees as a failed `initialize`.
    pub fn new(payload: &ServePayload) -> CcnmResult<Self> {
        let root = crate::runtime::canonical_root(&payload.root)?;
        let exec_gate = ExecGate::decide(payload)?;
        // Every entry, at every access level. The findings this gate reads
        // are not about `exec_command`: an identity ccnm cannot name, an
        // inherited authentication environment that every subprocess of
        // this session gets handed (a read session still spawns git and
        // ripgrep), and a runtime account that is also the account holding
        // the Agent's login. The last one is the separation ccnm exists to
        // keep, and it is a property of the machine, not of how much this
        // particular client was granted -- so a read-only external session
        // is refused here too, and takes the same one-line opt-in as any
        // other. Tested by `agent_credentials_stop_the_external_entry_too`.
        if !exec_gate.audit.agent_boundary_clear(exec_gate.accepted) {
            return Err(Error::policy(
                exec_gate
                    .audit
                    .refusal(exec_gate.accepted, crate::safety::Refused::Session),
            ));
        }
        // A session that cannot write does not take the workspace's write
        // guard: holding it would block a real writer for as long as
        // somebody keeps a read-only client open, and it protects nothing
        // — this session has no tool that changes a file.
        let guard = if payload.entry.writes() {
            let state = crate::paths::state_dir()?;
            Some(crate::mcp::write_guard::WriteGuard::acquire(
                &state,
                &root,
                &payload.workspace,
                &payload.session,
                exec_gate.config.as_ref(),
                &SystemRunner,
            )?)
        } else {
            None
        };
        Self::with_gate(payload, root, exec_gate, guard)
    }

    /// Open for an external MCP client: this Runtime's config decides the
    /// workspace, the root and how much the client gets.
    pub fn open_external(request: &crate::runtime::ExternalOpenPayload) -> CcnmResult<Self> {
        let config = crate::Config::load(&crate::paths::effective_config_path()?)?;
        let opened = crate::runtime::open_external(&config, request)?;
        Self::new(&opened.serve_payload(request))
    }

    fn with_gate(
        payload: &ServePayload,
        root: PathBuf,
        exec_gate: ExecGate,
        write_guard: Option<crate::mcp::write_guard::WriteGuard>,
    ) -> CcnmResult<Self> {
        // The same gate `Server::new` already applied, restated where the
        // first workspace-dependent subprocess actually happens: `git_facts`
        // is two lines below, and an unconfined opt-in cannot grant Agent
        // credentials. It used to be skipped when there was no write guard,
        // which since P10 reads as "a read-only session need not hold the
        // boundary" -- it never meant that. The only caller that passes no
        // guard is the test fixture, whose audit has no findings.
        if !exec_gate.audit.agent_boundary_clear(exec_gate.accepted) {
            return Err(Error::policy(
                exec_gate
                    .audit
                    .refusal(exec_gate.accepted, crate::safety::Refused::Session),
            ));
        }
        let (git, git_subdir) = git_facts(&root, &SystemRunner);
        // A CLAUDE.md that cannot be read does not stop the session: the
        // model can still work, just without the project's rules. It is
        // logged here and reported by doctor's "Project instructions" row,
        // which is where someone can act on it.
        // Named once at startup, like everything else here: the
        // handshake is sent once and cannot change afterwards, so
        // re-scanning mid-session would only produce a list that
        // disagrees with what the model was given.
        let named = payload.provider.project_named(&root);
        let project = match payload
            .provider
            .project_find(&root, &payload.workspace, &named)
        {
            Ok(project) => project,
            Err(e) => {
                tracing::warn!(error = %e, "project instructions not readable");
                None
            }
        };
        let skills = skills::discover(&root);
        let state = crate::paths::state_dir().ok();
        let sandbox = match exec_gate.config.as_ref() {
            Some(config) => sandbox::Sandbox::resolve(
                config,
                &payload.workspace,
                &root,
                state.as_deref(),
                &payload.session,
                &SystemRunner,
            )?
            .map(Arc::new),
            None => None,
        };
        tracing::info!(
            workspace = %payload.workspace,
            root = %root.display(),
            session = %payload.session,
            git,
            runtime_user = %exec_gate.audit.user,
            confined = exec_gate.audit.confined(),
            exec_allowed = exec_gate.allowed(),
            exec_sandbox = sandbox.is_some(),
            project_instructions = project.as_ref().map_or(0, context::Project::included),
            skills = skills.skills.len(),
            skills_skipped = skills.skipped.len(),
            "mcp server starting"
        );
        Ok(Server {
            inner: Arc::new(Inner {
                _write_guard: write_guard,
                provider: payload.provider,
                workspace: payload.workspace.clone(),
                session: payload.session.clone(),
                state: state.clone(),
                output: state
                    .as_deref()
                    .map(|state| Arc::new(retention::Output::new(state, &payload.session))),
                exec_gate,
                sandbox,
                root,
                project,
                named,
                skills,
                git,
                git_subdir,
                interactive: payload.interactive,
                entry: payload.entry,
                calls: AtomicU64::new(0),
            }),
            tool_router: Self::tool_router(),
        })
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Every tool this session offers, with the interaction requirement
    /// applied. One place, so `tools/list` and `get_tool` cannot drift.
    fn tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router
            .list_all()
            .into_iter()
            .filter(|tool| self.offers(&tool.name))
            .map(|mut tool| {
                // The one description that depends on the workspace: it
                // carries the catalog. The attribute on the method is
                // evaluated without `self`, so it is replaced here.
                if tool.name == skills::TOOL {
                    tool.description = Some(self.inner.skills.description().into());
                }
                let annotations = annotations_for(&tool.name);
                let tool = tool.annotate(annotations);
                // Only where somebody can answer, and never to an external
                // client: this server cannot know whether there is a person
                // on the other side of a bridge, and claiming to know would
                // let a Host skip the approval it would otherwise ask for.
                if self.inner.interactive
                    && !self.inner.entry.is_external()
                    && !self.inner.exec_gate.accepted.unattended_exec
                    && tool.name == INTERACTION_TOOL
                {
                    tool.with_meta(requires_user_interaction())
                } else {
                    tool
                }
            })
            .collect()
    }

    /// Whether this session offers a tool at all.
    ///
    /// `read_output` is in the withheld set for a different reason than the
    /// other two: an `output_ref` only means anything inside the session
    /// that produced it, and a session with no `exec_command` can never
    /// produce one. Offering it would be a tool that always fails, and
    /// resolving somebody else's ref is the leak that must not exist.
    fn offers(&self, tool: &str) -> bool {
        self.inner.entry.writes() || !WITHHELD_WITHOUT_WRITE.contains(&tool)
    }

    /// The refusal a withheld tool gets if a client calls it anyway.
    ///
    /// `tools/list` not naming it is a hint; a Host is free to ignore hints.
    /// This is the part that is not a hint.
    fn refuse_withheld(&self, tool: &str) -> Option<CallToolResult> {
        (!self.offers(tool)).then(|| {
            tool_error(&Error::policy(format!(
                "{tool} is not available: this workspace is open for external MCP in read mode"
            )))
        })
    }

    /// What goes into `initialize.result.instructions`: ccnm's own
    /// paragraph, then the project's CLAUDE.md, within
    /// [`Server::instructions_cap`] (design doc section 20).
    ///
    /// An external client gets what the workspace configured instead, and
    /// never a provider's projection: which instruction file a managed
    /// session projects follows from the provider ccnm started, and there
    /// is no provider here to follow.
    pub fn instructions(&self) -> String {
        let text = match self.inner.entry {
            crate::protocol::mcp::Entry::External(mode) => context::external(
                &self.inner.workspace,
                &self.inner.root,
                self.external_policy(),
                mode,
            ),
            crate::protocol::mcp::Entry::Managed => self.inner.provider.project_instructions(
                &self.inner.workspace,
                self.inner.project.as_ref(),
                &self.inner.named,
            ),
        };
        debug_assert!(self.instructions_cap().fits(&text));
        text
    }

    /// What the Host on the other end keeps of [`Server::instructions`].
    /// A managed session knows which provider it started; an external one
    /// does not, so it assumes the strictest Host.
    pub fn instructions_cap(&self) -> context::Cap {
        match self.inner.entry {
            crate::protocol::mcp::Entry::External(_) => context::EXTERNAL_CAP,
            crate::protocol::mcp::Entry::Managed => self.inner.provider.instructions_cap(),
        }
    }

    /// The workspace's external instruction policy, read from the same
    /// config copy the exec gate used — one load, one answer.
    fn external_policy(&self) -> crate::config::ExternalInstructions {
        self.inner
            .exec_gate
            .config
            .as_ref()
            .and_then(|config| config.workspaces.get(&self.inner.workspace))
            .map(|workspace| workspace.external_instructions)
            .unwrap_or_default()
    }

    /// Count one served tool call and return the new total. Every tool
    /// calls this, so `calls_served` is evidence about the whole session
    /// rather than about `workspace_info` alone (design doc section 27).
    fn count_call(&self) -> u64 {
        self.inner.calls.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Current answer to `workspace_info`, counting the call.
    pub fn info(&self) -> WorkspaceInfo {
        let calls_served = self.count_call();
        WorkspaceInfo {
            workspace: self.inner.workspace.clone(),
            git: self.inner.git,
            git_subdir: self.inner.git_subdir.clone(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            root_present: self.inner.root.is_dir(),
            server_pid: std::process::id(),
            calls_served,
        }
    }
}

#[tool_router(router = tool_router)]
impl Server {
    #[tool(
        name = "workspace_info",
        description = "Name, git status and platform of the remote workspace. Call once to orient; all other tool paths are relative to this workspace."
    )]
    async fn workspace_info(&self) -> std::result::Result<CallToolResult, ErrorData> {
        Ok(text_only(self.info().render()))
    }

    #[tool(
        name = "read_file",
        description = "Read a text file from the remote workspace, as numbered lines. Paths are relative to the workspace root. Long files come back truncated with the line to resume from; there is no way to read a file whole in one call."
    )]
    async fn read_file(
        &self,
        Parameters(args): Parameters<ReadFileArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        let root = self.inner.root.clone();
        // The read is blocking and the runtime is single-threaded, so it
        // runs on the blocking pool: a slow disk must not stop the server
        // answering pings or a cancellation while it works.
        let chunk = tokio::task::spawn_blocking(move || read::read_file(&root, &args))
            .await
            .map_err(|e| ErrorData::internal_error(format!("read_file task failed: {e}"), None))?;
        match chunk {
            Ok(chunk) => Ok(text_only(chunk.text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "list_files",
        description = "List a directory of the remote workspace, or search it with a glob. Without a glob you get the immediate children of one directory; with one you get every match under it, at any depth. In a git workspace, files that .gitignore rules out are never listed."
    )]
    async fn list_files(
        &self,
        Parameters(args): Parameters<ListFilesArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        let root = self.inner.root.clone();
        let listing =
            tokio::task::spawn_blocking(move || list::list_files(&root, &args, &SystemRunner))
                .await
                .map_err(|e| {
                    ErrorData::internal_error(format!("list_files task failed: {e}"), None)
                })?;
        match listing {
            Ok(listing) => Ok(text_only(listing.text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "search_text",
        description = "Search the remote workspace for a string, or a regex if you ask for one. The search runs where the files are and only the results come back: matching lines by default, or with output_mode just the file names or a count per file. Files that .gitignore rules out are never searched, dotfiles only with include_hidden, and .git never."
    )]
    async fn search_text(
        &self,
        Parameters(args): Parameters<SearchTextArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        let root = self.inner.root.clone();
        let found = tokio::task::spawn_blocking(move || search::search_text(&root, &args))
            .await
            .map_err(|e| {
                ErrorData::internal_error(format!("search_text task failed: {e}"), None)
            })?;
        match found {
            Ok(found) => Ok(text_only(found.text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "exec_command",
        description = "Run a command in the remote workspace. cmd is a program and its arguments, not a shell line: there are no pipes, redirection or globs. Long output stays on that machine; what comes back is the head and tail plus an output_ref. This runs with the full access of the account the runtime uses."
    )]
    async fn exec_command(
        &self,
        Parameters(args): Parameters<ExecCommandArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        if let Some(refusal) = self.refuse_withheld("exec_command") {
            return Ok(refusal);
        }
        // The hard gate of design doc section 18. Every other tool is
        // bounded by the path policy; this one is a shell, so it is
        // bounded by the account it runs as -- and if nobody has arranged
        // for that account to be a confined one, it does not run.
        if !self.inner.exec_gate.allowed() {
            return Ok(tool_error(&Error::policy(
                self.inner.exec_gate.audit.refusal(
                    self.inner.exec_gate.accepted,
                    crate::safety::Refused::ExecCommand,
                ),
            )));
        }
        // Re-checked here because what is on disk can change after the
        // handshake, and with the same admission the handshake used: a
        // workspace that accepted a reachable Agent login must not be
        // refused by the second check after passing the first.
        // Keep diagnostic/file work off the async IO thread.
        let accepted = self.inner.exec_gate.accepted;
        let checked = tokio::task::spawn_blocking(move || {
            let home = crate::paths::home_dir()?;
            crate::safety::credentials::runtime_gate(&home, accepted, &SystemRunner)
        })
        .await
        .map_err(|_| ErrorData::internal_error("Runtime credential check failed", None))?;
        if let Err(error) = checked {
            return Ok(tool_error(&error));
        }
        let Some(output) = self.inner.output.clone() else {
            return Ok(tool_error(&Error::new(
                ErrorCode::NotReady,
                "ccnm cannot find a state directory on the Runtime Node, so it has nowhere to keep a command's output",
            )));
        };
        let root = self.inner.root.clone();
        let provider = self.inner.provider;
        let sandbox = self.inner.sandbox.clone();
        let ran = tokio::task::spawn_blocking(move || {
            exec::exec_command_in(provider, &root, &output, &args, sandbox.as_deref())
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("exec_command task failed: {e}"), None))?;
        match ran {
            Ok(mut ran) => {
                // Accepting the risk once should not make it invisible
                // afterwards: every result of an unconfined session says
                // so, in the text the model reads and in the metadata.
                if let Some(note) = self.inner.exec_gate.note() {
                    ran.text.push_str(&format!("\n[{note}]"));
                    ran.notes.push(note);
                }
                Ok(text_only(ran.text))
            }
            Err(err) => Ok(tool_error(&err)),
        }
    }

    // The description here is a placeholder: `tools()` replaces it with the
    // workspace's catalog, which an attribute cannot see.
    #[tool(
        name = "load_skill",
        description = "Load one of this project's skills."
    )]
    async fn load_skill(
        &self,
        Parameters(args): Parameters<LoadSkillArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        let root = self.inner.root.clone();
        let session = self.inner.session.clone();
        let loaded =
            tokio::task::spawn_blocking(move || skills::load_skill(&root, &args, Some(&session)))
                .await
                .map_err(|e| {
                    ErrorData::internal_error(format!("load_skill task failed: {e}"), None)
                })?;
        match loaded {
            Ok(text) => Ok(text_only(text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "read_output",
        description = "Page through what a command wrote, using the output_ref exec_command returned. Offsets are byte offsets and stable: a finished command's output does not change."
    )]
    async fn read_output(
        &self,
        Parameters(args): Parameters<ReadOutputArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        if let Some(refusal) = self.refuse_withheld("read_output") {
            return Ok(refusal);
        }
        let Some(output) = self.inner.output.as_ref() else {
            return Ok(tool_error(&Error::new(
                ErrorCode::NotReady,
                "ccnm cannot find a state directory on the Runtime Node, so there is nowhere for a command's output to have been kept",
            )));
        };
        // The session's own directory and no other: an output_ref is a
        // reference within this session, not a handle on the machine.
        let dir = output.dir().to_path_buf();
        let page = tokio::task::spawn_blocking(move || output::read_output(&dir, &args))
            .await
            .map_err(|e| {
                ErrorData::internal_error(format!("read_output task failed: {e}"), None)
            })?;
        match page {
            Ok(page) => Ok(text_only(page.text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "apply_patch",
        description = "Change files in the remote workspace: add, update, delete or move. This is the only way to write. An update replaces exact strings and must carry the version read_file returned, so an edit built on content that has since changed is refused. Either every file in the patch is applied or none is."
    )]
    async fn apply_patch(
        &self,
        Parameters(args): Parameters<ApplyPatchArgs>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        self.count_call();
        if let Some(refusal) = self.refuse_withheld("apply_patch") {
            return Ok(refusal);
        }
        let root = self.inner.root.clone();
        // Where a commit in progress is recorded. Without a state
        // directory there is nowhere to put it and patching still works,
        // minus the one guarantee that needs somewhere to write.
        let journal_dir = self.inner.state.as_deref().map(crate::paths::patches_dir);
        let applied = tokio::task::spawn_blocking(move || {
            patch::apply_patch(&root, journal_dir.as_deref(), &args)
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("apply_patch task failed: {e}"), None))?;
        match applied {
            Ok(applied) => Ok(text_only(applied.text)),
            Err(err) => Ok(tool_error(&err)),
        }
    }
}

/// The tool that asks the person every single time, and the key that
/// makes a client do it.
///
/// Claude Code honours `anthropic/requiresUserInteraction` in *every*
/// permission mode, `bypassPermissions` included. That is the whole
/// reason it is worth setting: measured by Anthropic, people approve
/// about 93% of the prompts they see, and plenty of them run with
/// prompting turned off entirely, so a gate the user can switch off is
/// not a gate.
///
/// Only `exec_command` carries it. The other six are bounded by the path
/// policy and cannot reach past the workspace root; this one is a shell
/// on somebody else's machine, running as whatever account the runtime
/// uses. Putting it on the read tools would manufacture exactly the
/// prompt fatigue the same research describes.
///
/// **And only when somebody is there to answer.** `ccnm run --print`
/// launches Claude with prompting switched off on purpose: one prompt in,
/// one answer out, nobody at the terminal. Marking the tool there does
/// not make it safer, it makes it unusable -- Claude denies the call and
/// says it had no way to ask. Measured on the real pair before this was
/// conditional: `1 permission denial`, and the model reporting it could
/// not run the tests. In that mode the boundary is what it always was,
/// `exec_gate` and the account the runtime runs as.
///
/// If a client ignores the key, nothing breaks and nothing is claimed:
/// this is a second lock. The first is `exec_gate`, on this side, which
/// no client can talk its way past.
const INTERACTION_TOOL: &str = "exec_command";

/// The tools a session without write access does not get. Two of them
/// change things; `read_output` is here because of session scoping, see
/// [`Server::offers`].
const WITHHELD_WITHOUT_WRITE: [&str; 3] = ["exec_command", "apply_patch", "read_output"];

/// The standard MCP annotations for one tool.
///
/// They are **hints for the Host's approval UX and nothing else**. A Host
/// that ignores every one of them gets exactly the same authorization
/// result, because what decides that is the OS identity the runtime runs
/// as, the workspace binding, the access mode and the write guard. They are
/// published anyway: a Host that does honour them can ask the person about
/// the right calls instead of about all of them.
///
/// `exec_command` is destructive and open-world whatever this particular
/// command looks like. An annotation is a property of the tool, not of one
/// call, and "this one is only `ls`" is exactly the reasoning that cannot
/// be trusted.
fn annotations_for(tool: &str) -> rmcp::model::ToolAnnotations {
    let read_only = !WITHHELD_WITHOUT_WRITE.contains(&tool) || tool == "read_output";
    if read_only {
        return rmcp::model::ToolAnnotations::from_raw(None, Some(true), None, None, Some(false));
    }
    rmcp::model::ToolAnnotations::from_raw(
        None,
        Some(false),
        Some(true),
        Some(false),
        Some(tool == "exec_command"),
    )
}
const REQUIRES_INTERACTION: &str = "anthropic/requiresUserInteraction";

fn requires_user_interaction() -> rmcp::model::MetaObject {
    let mut meta = serde_json::Map::new();
    meta.insert(
        REQUIRES_INTERACTION.to_string(),
        serde_json::Value::Bool(true),
    );
    rmcp::model::MetaObject(meta)
}

/// A successful tool call: one text block, and no `structuredContent`.
///
/// Measured on Claude Code 2.1.260 (2026-09-04): when a result carries
/// both `content` and `structuredContent`, the model is shown the
/// structured JSON and *not* the text. The first real session against
/// this server took 74 turns to change one line, because every
/// `read_file` came back as `{"bytes":416,"lines":9,"version":...}` and
/// the model rebuilt the file line by line with `search_text` probes.
///
/// So everything the model must see — the body, and the fields it has to
/// hand back such as `version` and `output_ref` — is in the text, and there
/// is no second channel to fall out of step with it. The result structs
/// the tools build still exist; they are what the text is rendered from.
fn text_only(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A failed tool call, shaped so the model can act on it: `isError` set,
/// and one line beginning with the stable `CCNM_E_*` name.
fn tool_error(err: &Error) -> CallToolResult {
    tracing::debug!(code = %err.code(), message = err.message(), "tool call refused");
    CallToolResult::error(vec![ContentBlock::text(ErrorReport::from(err).to_string())])
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    /// The tool list, with `exec_command` marked as needing the user
    /// every time — but only when there is a user.
    ///
    /// Written by hand rather than through the `#[tool(meta = ...)]`
    /// attribute because that is evaluated without `self`: the mark has
    /// to depend on how this session was started, and the macro leaves
    /// this method alone when it is already defined.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> std::result::Result<rmcp::model::ListToolsResult, ErrorData> {
        // The cache hints the generated version sends, kept because
        // replacing a method means inheriting everything it did, not just
        // the part being changed. `ttl_ms: 0` with a public scope is what
        // the macro emits: the list never changes for the life of a
        // server, so a client may hold it, and every re-fetch of it over
        // ssh is a round trip nobody needed.
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        Ok(rmcp::model::ListToolsResult {
            result_type: Some(rmcp::model::ResultType::COMPLETE),
            tools: self.tools(),
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(rmcp::model::CacheScope::Public),
        })
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }

    /// The project's skills again, as prompts: the way a *person* starts
    /// one. Claude Code turns each into `/mcp__ccnm__<name>`; Codex never
    /// asks for prompts at all (both measured, P36.1), which is why the
    /// tool exists and this is an extra.
    async fn list_prompts(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> std::result::Result<ListPromptsResult, ErrorData> {
        let root = self.inner.root.clone();
        let catalog = tokio::task::spawn_blocking(move || skills::discover(&root))
            .await
            .map_err(|e| ErrorData::internal_error(format!("skill scan failed: {e}"), None))?;
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
        let root = self.inner.root.clone();
        let session = self.inner.session.clone();
        let given = request.arguments.unwrap_or_default();
        let name = request.name;
        let text = tokio::task::spawn_blocking(move || {
            let catalog = skills::discover(&root);
            let declared = catalog
                .skills
                .iter()
                .find(|skill| skill.name == name)
                .map(prompt_arguments)
                .unwrap_or_default();
            // Back into the one string a skill's `$ARGUMENTS` stands for,
            // in the order the arguments were declared.
            let line = declared
                .iter()
                .filter_map(|arg| given.get(&arg.name))
                .map(|value| match value {
                    serde_json::Value::String(text) => quote_argument(text),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            skills::prompt_text(&root, &name, &line, Some(&session))
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("skill load failed: {e}"), None))?
        // A prompt has no `isError` result to put a refusal in; a protocol
        // error is the only way to say no.
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
        .with_instructions(self.instructions())
    }
}

/// The arguments a skill's prompt declares: the names its frontmatter
/// lists, or one catch-all.
///
/// Claude Code splits what the person typed on whitespace and hands the
/// words to the declared arguments in order, dropping any that are left
/// over (2.1.273). So a skill that wants several words has to declare
/// them; with nothing declared, only the first word arrives.
fn prompt_arguments(skill: &skills::Skill) -> Vec<PromptArgument> {
    let names: Vec<String> = if skill.arguments.is_empty() {
        vec!["arguments".to_string()]
    } else {
        skill.arguments.clone()
    };
    names
        .into_iter()
        .map(|name| {
            let mut arg = PromptArgument::new(name);
            arg.description = skill.argument_hint.clone();
            arg.required = Some(false);
            arg
        })
        .collect()
}

/// One prompt argument, spelled so that splitting the line again gives it
/// back as one word.
fn quote_argument(text: &str) -> String {
    if !text.is_empty() && !text.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        return text.to_string();
    }
    if !text.contains('"') {
        return format!("\"{text}\"");
    }
    format!("'{}'", text.replace('\'', ""))
}

/// Serve MCP on this process's stdin/stdout until the client closes the
/// stream. Synchronous from the caller's point of view.
pub fn serve(payload: &ServePayload) -> CcnmResult<()> {
    run(Server::new(payload)?)
}

/// Serve a Runtime-authority open: the caller named a workspace, this
/// machine decided the rest.
pub fn serve_managed(request: &crate::runtime::OpenPayload) -> CcnmResult<()> {
    run(Server::open(request)?)
}

/// Serve an external MCP client through a bridge (P10).
///
/// Same server, same tools, same guard. What differs is decided before the
/// first byte of MCP: whether this workspace is open to external clients at
/// all, and how much of it.
pub fn serve_external(request: &crate::runtime::ExternalOpenPayload) -> CcnmResult<()> {
    run(Server::open_external(request)?)
}

/// How long the server stays silent before it speaks first.
///
/// In MCP the client asks and the server answers, so a server nobody is
/// calling never writes a byte -- and a transport that died without a FIN
/// never reaches it. Seen on a real pair: the Runtime laptop slept, the
/// Agent's ssh gave up and closed, the close was lost while the laptop was
/// asleep, and on waking sshd still held an ESTABLISHED socket to a port
/// nothing listened on. This process sat on stdin for twelve hours holding
/// the workspace write guard, and every new session for that workspace was
/// refused as busy -- which Claude Code shows only as "failed to connect".
///
/// A `ping` (MCP lets either side send one) puts bytes on that socket. A
/// live peer answers; a vanished one's kernel answers with RST, sshd exits,
/// and stdin reaches EOF like any other disconnect. An unanswered ping does
/// **not** end the session: a peer that is only asleep keeps its TCP, and
/// the Agent-side transport ssh is deliberately patient for the same reason
/// (`Ssh::transport_options`). Only a write that fails does.
///
/// Lower costs a line of JSON each way more often; higher means a dead
/// session keeps its workspace locked that much longer after a wake.
pub const HEARTBEAT: Duration = Duration::from_secs(30);

fn run(server: Server) -> CcnmResult<()> {
    if let Some(state) = server.inner.state.clone() {
        // Off this thread: a sweep of every session on the machine must
        // not delay the handshake, and nothing it runs into is this
        // session's problem.
        let own = server.inner.session.clone();
        let _ = std::thread::Builder::new()
            .name("ccnm-output-expiry".into())
            .spawn(move || retention::sweep_expired(&state, &own, &SystemRunner));
    }
    // Only an external client's session ends with this process; see
    // `Output::discard_started` for why a managed one must not.
    let discard = server
        .inner
        .entry
        .is_external()
        .then(|| server.inner.output.clone())
        .flatten();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal("cannot start tokio runtime").with_source(e))?;
    let served = rt.block_on(serve_until_gone(
        server,
        rmcp::transport::stdio(),
        HEARTBEAT,
    ));
    // Dropping the runtime waits for commands still running in
    // `spawn_blocking`, so their runs are finished before they are removed.
    drop(rt);
    if let Some(output) = discard {
        output.discard_started();
    }
    served
}

/// Serve until the client closes the stream or can no longer be written to.
async fn serve_until_gone<T, E, A>(
    server: Server,
    transport: T,
    heartbeat: Duration,
) -> CcnmResult<()>
where
    T: rmcp::transport::IntoTransport<rmcp::RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let service = server
        .serve(transport)
        .await
        .map_err(|e| Error::internal("MCP initialize failed").with_source(e))?;
    let pulse = tokio::spawn(heartbeat_until_unwritable(
        service.peer().clone(),
        service.cancellation_token(),
        heartbeat,
    ));
    let reason = service
        .waiting()
        .await
        .map_err(|e| Error::internal("MCP service task panicked").with_source(e));
    pulse.abort();
    tracing::info!(reason = ?reason?, "mcp server stopped");
    Ok(())
}

async fn heartbeat_until_unwritable(
    peer: rmcp::Peer<rmcp::RoleServer>,
    stop: rmcp::service::RunningServiceCancellationToken,
    every: Duration,
) {
    use rmcp::service::{PeerRequestOptions, ServiceError};
    loop {
        tokio::time::sleep(every).await;
        let ping = rmcp::model::ServerRequest::PingRequest(Default::default());
        let sent = match peer
            .send_request_with_option(ping, PeerRequestOptions::with_timeout(every))
            .await
        {
            Ok(handle) => handle.await_response().await,
            Err(e) => Err(e),
        };
        match sent {
            // rmcp keeps serving after a failed write and waits for stdin
            // instead, which is the very wait that never ends here.
            Err(ServiceError::TransportSend(_) | ServiceError::TransportClosed) => {
                tracing::warn!("MCP client can no longer be written to; closing the session");
                stop.cancel();
                return;
            }
            Err(error) => tracing::debug!(%error, "heartbeat ping unanswered"),
            Ok(_) => {}
        }
    }
}

/// Is `root` inside a git work tree, and if so where relative to its top
/// level? Asked once at startup; a missing `git` or a non-repository both
/// mean "no git" rather than an error.
fn git_facts(root: &Path, runner: &dyn ProcessRunner) -> (bool, Option<String>) {
    let cmd = Cmd::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .cwd(root)
        .timeout(Duration::from_secs(10));
    let Ok(out) = runner.run(&cmd) else {
        return (false, None);
    };
    if !out.success() {
        return (false, None);
    }
    let top = PathBuf::from(out.stdout_lossy().trim());
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    match root.strip_prefix(&top) {
        Ok(rel) if rel.as_os_str().is_empty() => (true, None),
        Ok(rel) => (true, Some(rel.to_string_lossy().into_owned())),
        Err(_) => (true, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;

    fn fixture_server(payload: &ServePayload) -> CcnmResult<Server> {
        fixture_server_accepting(payload, crate::safety::Accepted::NOTHING)
    }

    fn fixture_server_accepting(
        payload: &ServePayload,
        accepted: crate::safety::Accepted,
    ) -> CcnmResult<Server> {
        Server::with_gate(
            payload,
            crate::runtime::canonical_root(&payload.root)?,
            ExecGate {
                audit: crate::safety::Audit {
                    user: "fixture".into(),
                    findings: vec![],
                },
                accepted,
                config: None,
            },
            None,
        )
    }

    fn temp(test: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-mcp-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(dir)
    }

    /// A client on in-memory pipes that has finished `initialize`: what it
    /// writes the server reads, and the server's lines come back on `from`.
    async fn initialized_client(
        test: &str,
        heartbeat: Duration,
    ) -> (
        tokio::task::JoinHandle<CcnmResult<()>>,
        tokio::io::DuplexStream,
        tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
        TestDir,
    ) {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        let root = temp(test);
        let server = fixture_server(&ServePayload::new("x", root.to_path_buf(), "s")).unwrap();
        // Two pipes, not one duplex, so that dropping the reading end
        // leaves the server's stdin open -- a real pipe's EPIPE on write
        // with no EOF on read. (`simplex` halves share one buffer and
        // never report the reader gone.)
        let (server_in, mut to) = tokio::io::duplex(1 << 16);
        let (from, server_out) = tokio::io::duplex(1 << 16);
        let task = tokio::spawn(serve_until_gone(server, (server_in, server_out), heartbeat));
        to.write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
"#,
        )
        .await
        .unwrap();
        let mut from = tokio::io::BufReader::new(from).lines();
        let answer = from.next_line().await.unwrap().unwrap();
        assert!(answer.contains(r#""id":1"#), "{answer}");
        to.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        (task, to, from, root)
    }

    /// The twelve-hour write guard of `HEARTBEAT`'s doc comment, in small:
    /// stdin stays open forever and nobody reads what the server writes.
    #[tokio::test]
    async fn a_client_that_cannot_be_written_to_ends_the_session() {
        let (task, _still_open, from, _root) =
            initialized_client("unwritable", Duration::from_millis(50)).await;
        drop(from);
        let ended = tokio::time::timeout(Duration::from_secs(10), task).await;
        assert!(
            ended.is_ok(),
            "server still waiting on an open stdin nobody will write to"
        );
    }

    /// A peer that is asleep keeps its TCP and answers nothing. Ending its
    /// session for that would cost the person a reconnect for no reason.
    #[tokio::test]
    async fn an_unanswered_ping_does_not_end_the_session() {
        let (task, to, mut from, _root) =
            initialized_client("asleep", Duration::from_millis(50)).await;
        let mut pings = 0;
        while pings < 4 {
            let line = tokio::time::timeout(Duration::from_secs(5), from.next_line())
                .await
                .expect("the server speaks first while idle")
                .unwrap()
                .unwrap();
            // Each unanswered ping is followed by rmcp's own cancellation.
            if line.contains(r#""method":"ping""#) {
                pings += 1;
            } else {
                assert!(line.contains("notifications/cancelled"), "{line}");
            }
        }
        assert!(!task.is_finished(), "unanswered pings ended the session");
        drop(to);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("stdin EOF still ends it")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn missing_root_is_wrong_workspace() {
        let payload = ServePayload::new("x", PathBuf::from("/nonexistent/ccnm-root"), "s");
        let err = match fixture_server(&payload) {
            Err(e) => e,
            Ok(_) => panic!("a missing root must be refused"),
        };
        assert_eq!(err.code(), ErrorCode::WrongWorkspace);
        assert!(err.message().contains("/nonexistent/ccnm-root"), "{err}");
    }

    /// The settings allow-list on the Agent Node names these tools by
    /// hand. A tool added or renamed here without updating that list would
    /// be offered to the model and then denied on every call.
    #[test]
    fn tools_list_matches_the_sessions_allow_list() {
        let dir = temp("tools");
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        let mut served: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        served.sort();
        let mut allowed: Vec<String> = crate::session::MCP_TOOLS
            .iter()
            .map(|t| t.to_string())
            .collect();
        allowed.sort();
        assert_eq!(served, allowed);
    }

    /// True when the tool's serialized form carries the key that makes a
    /// client ask the user. Asserted on the JSON, not the Rust value,
    /// because it travels in `_meta` and a rename of that field by the
    /// crate would otherwise pass unnoticed.
    fn asks_the_user(tool: &rmcp::model::Tool) -> bool {
        serde_json::to_value(tool)
            .unwrap()
            .get("_meta")
            .and_then(|m| m.get("anthropic/requiresUserInteraction"))
            == Some(&serde_json::Value::Bool(true))
    }

    /// `exec_command` is the one tool not bounded by the path policy, so
    /// it is the one tool that asks the person every time -- in every
    /// permission mode, `bypassPermissions` included.
    #[test]
    fn only_exec_command_makes_the_client_ask_every_time() {
        let dir = temp("meta");
        let payload = ServePayload::new("xshun", dir.to_path_buf(), "s").with_interactive(true);
        let server = fixture_server(&payload).unwrap();
        let tools = server.tools();
        assert_eq!(tools.len(), crate::session::MCP_TOOLS.len());
        for tool in &tools {
            assert_eq!(
                asks_the_user(tool),
                tool.name == "exec_command",
                "{} has the wrong interaction requirement",
                tool.name
            );
        }
        // get_tool has to say the same thing, or a client that asks about
        // one tool gets a different answer from the one that listed it.
        assert!(asks_the_user(&server.get_tool("exec_command").unwrap()));
    }

    /// `--print` starts Claude with prompting switched off on purpose:
    /// one prompt in, one answer out, nobody at the terminal. Marking
    /// the tool there does not make it safer, it makes it unusable --
    /// measured on the real pair, Claude denied the call and reported it
    /// had no way to ask.
    #[test]
    fn a_session_with_nobody_at_the_terminal_does_not_ask() {
        let dir = temp("meta-print");
        // `new` alone: not interactive, which is also what a probe sends.
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        assert!(!server.tools().iter().any(asks_the_user));
        assert!(!asks_the_user(&server.get_tool("exec_command").unwrap()));
    }

    /// The one way the asking stops, and it comes from the Runtime's own
    /// config -- never from the payload, because a caller that could turn
    /// off the step that audits it is not being audited.
    ///
    /// Nothing else about the tool changes: this is not an authorization,
    /// and `exec_gate` is untouched by it.
    #[test]
    fn a_workspace_that_accepted_unattended_exec_stops_the_client_asking() {
        let dir = temp("meta-unattended");
        let payload = ServePayload::new("xshun", dir.to_path_buf(), "s").with_interactive(true);
        let accepted = crate::safety::Accepted {
            unconfined_exec: false,
            unisolated_credentials: false,
            unattended_exec: true,
        };
        let server = fixture_server_accepting(&payload, accepted).unwrap();
        let tools = server.tools();
        assert_eq!(tools.len(), crate::session::MCP_TOOLS.len());
        assert!(
            !tools.iter().any(asks_the_user),
            "allow_unattended_exec is set, so nothing asks"
        );
        // Same answer from both sides, as above.
        assert!(!asks_the_user(&server.get_tool("exec_command").unwrap()));

        // And without it, on the same payload, it still asks -- otherwise
        // this test would pass for the wrong reason.
        let server = fixture_server(&payload).unwrap();
        assert!(asks_the_user(&server.get_tool("exec_command").unwrap()));
    }

    #[test]
    fn root_is_canonical_and_info_counts_calls() {
        let dir = temp("info");
        // A symlink to the root must resolve to the real directory.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let payload = ServePayload::new("xshun", link, "s");
        let server = fixture_server(&payload).unwrap();
        assert_eq!(server.root(), std::fs::canonicalize(&dir).unwrap());

        let first = server.info();
        let second = server.info();
        assert_eq!(first.calls_served, 1);
        assert_eq!(second.calls_served, 2);
        assert_eq!(first.server_pid, std::process::id());
        assert_eq!(first.workspace, "xshun");
        assert!(first.platform.contains('/'));
        assert!(server.instructions().contains("\"xshun\""));
        assert!(server.instructions_cap().fits(&server.instructions()));
        // The absolute root never appears in what the model sees.
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains(&dir.display().to_string()), "{json}");
        assert!(!server.instructions().contains(&dir.display().to_string()));
    }

    /// The project's own rules have to come out of the handshake the
    /// server really builds, not only out of the context module's tests.
    #[test]
    fn the_projects_claude_md_reaches_the_instructions() {
        let dir = temp("project");
        std::fs::write(dir.join("CLAUDE.md"), "# 规则\n\n- 提交要小\n").unwrap();
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        let text = server.instructions();
        assert!(text.contains("- 提交要小"), "{text}");
        assert_eq!(
            crate::mcp::context::parse_marker(&text).as_deref(),
            Some("CLAUDE.md, 25 bytes")
        );
        assert!(server.instructions_cap().fits(&text));
        // Still no absolute path, project file or not.
        assert!(!text.contains(&dir.display().to_string()), "{text}");
    }

    /// The cap belongs to the server, not to whoever remembers to pass a
    /// budget: a project with a long CLAUDE.md must not be able to push
    /// the handshake past what Claude Code keeps -- 2048 UTF-16 code units,
    /// which for this Chinese file is about a third of the bytes.
    #[test]
    fn a_long_claude_md_cannot_push_the_handshake_over_the_cap() {
        let dir = temp("bigproject");
        let big = "- 一条规则，写得很长很长。\n".repeat(2000);
        std::fs::write(dir.join("CLAUDE.md"), &big).unwrap();
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        let text = server.instructions();
        assert_eq!(server.instructions_cap(), context::CLAUDE_CODE_CAP);
        assert!(
            server.instructions_cap().fits(&text),
            "{} UTF-16 code units",
            text.encode_utf16().count()
        );
        let marker = crate::mcp::context::parse_marker(&text).unwrap();
        assert!(
            marker.contains(&format!("{} bytes, first ", big.len())),
            "{marker}"
        );
    }

    /// A CLAUDE.md that cannot be read must not take the session down with
    /// it: without the project's rules the model is worse off, without a
    /// server it cannot work at all.
    #[test]
    fn an_unreadable_claude_md_still_serves() {
        let dir = temp("badproject");
        std::fs::create_dir(dir.join("CLAUDE.md")).unwrap();
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        assert_eq!(
            crate::mcp::context::parse_marker(&server.instructions()).as_deref(),
            Some("no CLAUDE.md at the workspace root")
        );
    }

    /// A session is bound to the directory it started with. When that
    /// directory is moved or deleted underneath it, the tool the model
    /// calls to orient itself has to say so -- otherwise it keeps
    /// answering "workspace fixture (git repository root)" while every
    /// other tool fails for reasons that sound like something else.
    #[test]
    fn workspace_info_says_when_the_root_has_gone() {
        let dir = temp("vanish");
        let server = fixture_server(&ServePayload::new("xshun", dir.to_path_buf(), "s")).unwrap();
        let before = server.info();
        assert!(before.root_present);
        assert!(
            !before.summary().contains("WARNING"),
            "{}",
            before.summary()
        );

        std::fs::remove_dir_all(&dir).unwrap();
        let after = server.info();
        assert!(!after.root_present, "the check must not be cached");
        let summary = after.summary();
        assert!(
            summary.contains("not on this machine any more"),
            "{summary}"
        );
        assert!(summary.contains("ccnm stop"), "{summary}");
        // Still no absolute path, even when saying it is gone.
        assert!(!summary.contains(&dir.display().to_string()), "{summary}");
    }

    #[test]
    fn git_facts_distinguish_root_subdir_and_none() {
        let dir = temp("git");
        let runner = SystemRunner;
        assert_eq!(git_facts(&dir, &runner), (false, None));

        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("packages/core")).unwrap();
        let init = runner
            .run(&Cmd::new("git").args(["init", "-q"]).cwd(&repo))
            .unwrap();
        assert!(init.success(), "{}", init.stderr_lossy());
        let repo = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(git_facts(&repo, &runner), (true, None));
        assert_eq!(
            git_facts(&repo.join("packages/core"), &runner),
            (true, Some("packages/core".into()))
        );
        let info = WorkspaceInfo {
            workspace: "w".into(),
            git: true,
            git_subdir: Some("packages/core".into()),
            platform: "macos/aarch64".into(),
            root_present: true,
            server_pid: 1,
            calls_served: 1,
        };
        assert_eq!(
            info.summary(),
            "workspace w (inside git repository at packages/core, macos/aarch64); all paths are relative to its root"
        );
    }
}

//! `ccnm internal mcp-serve`: the coding runtime Claude Code talks to over
//! one ssh. Phase 2 fills in the bounded tools of design doc section 15
//! one at a time; the set of section 14 is now complete:
//! `workspace_info`, `read_file`, `list_files`, `search_text`,
//! `apply_patch`, `exec_command` and `read_output`.
//!
//! Two rules are enforced here because everything later depends on them.
//! The workspace root is canonicalized once at startup and is the only
//! path the server ever reveals (section 17). And nothing is written to
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
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

// `crate::error::Result` is deliberately not imported: the `tool_handler`
// macro expands to `Result<_, ErrorData>` and would pick up the alias.
use crate::error::{Error, ErrorCode, ErrorReport};
use crate::mcp::exec::{self, ExecCommandArgs};
use crate::mcp::list::{self, ListFilesArgs};
use crate::mcp::output::{self, ReadOutputArgs};
use crate::mcp::patch::{self, ApplyPatchArgs};
use crate::mcp::read::{self, ReadFileArgs};
use crate::mcp::search::{self, SearchTextArgs};
use crate::process::{Cmd, ProcessRunner, SystemRunner};
use crate::protocol::mcp::ServePayload;

type CcnmResult<T> = crate::error::Result<T>;

/// `serverInfo.name` in the initialize response.
pub const SERVER_NAME: &str = "ccnm";

/// Upper bound on `initialize.result.instructions` (design doc section
/// 20). Phase 3 adds the project's CLAUDE.md under this cap; today the
/// text is a few lines.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;

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
    pub server_pid: u32,
    pub calls_served: u64,
}

impl WorkspaceInfo {
    /// The `content[0].text` a model reads.
    pub fn summary(&self) -> String {
        let git = match (&self.git, &self.git_subdir) {
            (false, _) => "not a git repository".to_string(),
            (true, None) => "git repository root".to_string(),
            (true, Some(sub)) => format!("inside git repository at {sub}"),
        };
        format!(
            "workspace {} ({git}, {}); all paths are relative to its root",
            self.workspace, self.platform
        )
    }
}

struct Inner {
    workspace: String,
    /// Names the directory `exec_command` retains output in.
    session: String,
    /// `~/.local/state/ccnm`. Resolved once; a runtime that cannot find it
    /// still serves every read-only tool.
    state: Option<PathBuf>,
    /// Canonical. Never sent to the client.
    root: PathBuf,
    git: bool,
    git_subdir: Option<String>,
    calls: AtomicU64,
}

#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Resolve the root and look at git once. Fails with
    /// `CCNM_E_WRONG_WORKSPACE` if the root is not a directory here, which
    /// the launcher sees as a failed `initialize`.
    pub fn new(payload: &ServePayload) -> CcnmResult<Self> {
        let root = canonical_root(&payload.root)?;
        let (git, git_subdir) = git_facts(&root, &SystemRunner);
        tracing::info!(
            workspace = %payload.workspace,
            root = %root.display(),
            session = %payload.session,
            git,
            "mcp server starting"
        );
        Ok(Server {
            inner: Arc::new(Inner {
                workspace: payload.workspace.clone(),
                session: payload.session.clone(),
                state: crate::paths::state_dir().ok(),
                root,
                git,
                git_subdir,
                calls: AtomicU64::new(0),
            }),
            tool_router: Self::tool_router(),
        })
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// What goes into `initialize.result.instructions`. Bounded by
    /// [`MAX_INSTRUCTIONS_BYTES`]; phase 3 appends the project CLAUDE.md.
    pub fn instructions(&self) -> String {
        let text = format!(
            "CCNM remote workspace \"{}\". The project lives on another machine and is reachable only through the ccnm tools; there is no local copy. Every path you pass or receive is relative to the workspace root.",
            self.inner.workspace
        );
        debug_assert!(text.len() <= MAX_INSTRUCTIONS_BYTES);
        text
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
        let info = self.info();
        let value = serde_json::to_value(&info)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut result = CallToolResult::structured(value);
        result.content = vec![ContentBlock::text(info.summary())];
        Ok(result)
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
            Ok(chunk) => {
                let value = serde_json::to_value(&chunk)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(chunk.text)];
                Ok(result)
            }
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
            Ok(listing) => {
                let value = serde_json::to_value(&listing)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(listing.text)];
                Ok(result)
            }
            Err(err) => Ok(tool_error(&err)),
        }
    }

    #[tool(
        name = "search_text",
        description = "Search the remote workspace for a string, or a regex if you ask for one. The search runs where the files are and only the matching lines come back. Files that .gitignore rules out, dotfiles and .git are never searched."
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
            Ok(found) => {
                let value = serde_json::to_value(&found)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(found.text)];
                Ok(result)
            }
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
        let Some(state) = self.inner.state.clone() else {
            return Ok(tool_error(&Error::new(
                ErrorCode::NotReady,
                "ccnm cannot find a state directory on the workspace machine, so it has nowhere to keep a command's output",
            )));
        };
        let root = self.inner.root.clone();
        let session = self.inner.session.clone();
        let ran =
            tokio::task::spawn_blocking(move || exec::exec_command(&root, &session, &state, &args))
                .await
                .map_err(|e| {
                    ErrorData::internal_error(format!("exec_command task failed: {e}"), None)
                })?;
        match ran {
            Ok(ran) => {
                let value = serde_json::to_value(&ran)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(ran.text)];
                Ok(result)
            }
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
        let Some(state) = self.inner.state.clone() else {
            return Ok(tool_error(&Error::new(
                ErrorCode::NotReady,
                "ccnm cannot find a state directory on the workspace machine, so there is nowhere for a command's output to have been kept",
            )));
        };
        // The session's own directory and no other: an output_ref is a
        // reference within this session, not a handle on the machine.
        let dir = exec::session_dir(&state, &self.inner.session);
        let page = tokio::task::spawn_blocking(move || output::read_output(&dir, &args))
            .await
            .map_err(|e| {
                ErrorData::internal_error(format!("read_output task failed: {e}"), None)
            })?;
        match page {
            Ok(page) => {
                let value = serde_json::to_value(&page)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(page.text)];
                Ok(result)
            }
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
        let root = self.inner.root.clone();
        let applied = tokio::task::spawn_blocking(move || patch::apply_patch(&root, &args))
            .await
            .map_err(|e| {
                ErrorData::internal_error(format!("apply_patch task failed: {e}"), None)
            })?;
        match applied {
            Ok(applied) => {
                let value = serde_json::to_value(&applied)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(applied.text)];
                Ok(result)
            }
            Err(err) => Ok(tool_error(&err)),
        }
    }
}

/// A failed tool call, shaped so the model can act on it: `isError` set,
/// and one line beginning with the stable `CCNM_E_*` name.
fn tool_error(err: &Error) -> CallToolResult {
    tracing::debug!(code = %err.code(), message = err.message(), "tool call refused");
    CallToolResult::error(vec![ContentBlock::text(ErrorReport::from(err).to_string())])
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, crate::VERSION))
            .with_instructions(self.instructions())
    }
}

/// Serve MCP on this process's stdin/stdout until the client closes the
/// stream. Synchronous from the caller's point of view.
pub fn serve(payload: &ServePayload) -> CcnmResult<()> {
    let server = Server::new(payload)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::internal("cannot start tokio runtime").with_source(e))?;
    rt.block_on(async {
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| Error::internal("MCP initialize failed").with_source(e))?;
        let reason = service
            .waiting()
            .await
            .map_err(|e| Error::internal("MCP service task panicked").with_source(e))?;
        tracing::info!(?reason, "mcp server stopped");
        Ok(())
    })
}

fn canonical_root(root: &Path) -> CcnmResult<PathBuf> {
    let canonical = std::fs::canonicalize(root).map_err(|e| {
        Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace root {} is not usable on this host",
                root.display()
            ),
        )
        .with_source(e)
    })?;
    if !canonical.is_dir() {
        return Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!("workspace root {} is not a directory", canonical.display()),
        ));
    }
    Ok(canonical)
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

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-mcp-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_root_is_wrong_workspace() {
        let payload = ServePayload::new("x", PathBuf::from("/nonexistent/ccnm-root"), "s");
        let err = match Server::new(&payload) {
            Err(e) => e,
            Ok(_) => panic!("a missing root must be refused"),
        };
        assert_eq!(err.code(), ErrorCode::WrongWorkspace);
        assert!(err.message().contains("/nonexistent/ccnm-root"), "{err}");
    }

    #[test]
    fn root_is_canonical_and_info_counts_calls() {
        let dir = temp("info");
        // A symlink to the root must resolve to the real directory.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let payload = ServePayload::new("xshun", link, "s");
        let server = Server::new(&payload).unwrap();
        assert_eq!(server.root(), std::fs::canonicalize(&dir).unwrap());

        let first = server.info();
        let second = server.info();
        assert_eq!(first.calls_served, 1);
        assert_eq!(second.calls_served, 2);
        assert_eq!(first.server_pid, std::process::id());
        assert_eq!(first.workspace, "xshun");
        assert!(first.platform.contains('/'));
        assert!(server.instructions().contains("\"xshun\""));
        assert!(server.instructions().len() <= MAX_INSTRUCTIONS_BYTES);
        // The absolute root never appears in what the model sees.
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains(&dir.display().to_string()), "{json}");
        assert!(!server.instructions().contains(&dir.display().to_string()));
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
            server_pid: 1,
            calls_served: 1,
        };
        assert_eq!(
            info.summary(),
            "workspace w (inside git repository at packages/core, macos/aarch64); all paths are relative to its root"
        );
    }
}

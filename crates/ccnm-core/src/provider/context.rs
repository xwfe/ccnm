//! Project context dispatch. Files are still read only on the Runtime Node.
use super::{AgentProvider, claude};
use crate::error::Result;
use std::path::Path;

/// Another instruction file the project has, named but not carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// Path relative to the workspace root, as the model must pass it.
    pub rel: String,
    pub bytes: u64,
}

/// The workspace's root instruction file, and how much of it fits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub source: &'static str,
    /// Size of the decoded file. Invalid UTF-8 is replaced before this is
    /// measured, so for a file that is not valid UTF-8 this can differ by
    /// a few bytes from its size on disk.
    pub bytes: usize,
    /// The part that fits in the budget, cut at a line boundary.
    pub text: String,
}

impl Project {
    /// Bytes of the file the model is actually shown.
    pub fn included(&self) -> usize {
        self.text.len()
    }

    pub fn truncated(&self) -> bool {
        self.included() < self.bytes
    }
}

// Compatibility project constants; the MCP projection budget is shared.
pub const PROJECT_FILE: &str = AgentProvider::current().project_file();
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
pub const MAX_NAMED: usize = claude::context::MAX_NAMED;

pub fn find(root: &Path, budget: usize) -> Result<Option<Project>> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::find(root, budget),
        AgentProvider::Codex => super::codex::context::find(root, budget),
    }
}
pub fn named(root: &Path) -> Vec<Named> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::named(root),
        AgentProvider::Codex => Vec::new(),
    }
}
pub fn budget(workspace: &str, named: &[Named]) -> usize {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::budget(workspace, named),
        AgentProvider::Codex => super::codex::context::budget(workspace),
    }
}
pub fn instructions(workspace: &str, project: Option<&Project>, named: &[Named]) -> String {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::instructions(workspace, project, named),
        AgentProvider::Codex => super::codex::context::instructions(workspace, project),
    }
}
pub fn marker(project: Option<&Project>) -> String {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::marker(project),
        AgentProvider::Codex => marker_file(project.map_or("AGENTS.md", |p| p.source), project),
    }
}
pub fn parse_marker(instructions: &str) -> Option<String> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::parse_marker(instructions),
        AgentProvider::Codex => claude::context::parse_marker(instructions),
    }
}

impl AgentProvider {
    pub fn project_named(self, root: &Path) -> Vec<Named> {
        match self {
            Self::Claude => named(root),
            Self::Codex => Vec::new(),
        }
    }
    pub fn project_find(
        self,
        root: &Path,
        workspace: &str,
        named: &[Named],
    ) -> Result<Option<Project>> {
        match self {
            Self::Claude => find(root, budget(workspace, named)),
            Self::Codex => {
                super::codex::context::find(root, super::codex::context::budget(workspace))
            }
        }
    }
    pub fn project_instructions(
        self,
        workspace: &str,
        project: Option<&Project>,
        named: &[Named],
    ) -> String {
        match self {
            Self::Claude => instructions(workspace, project, named),
            Self::Codex => super::codex::context::instructions(workspace, project),
        }
    }
}

/// The lines that name the rest of the project's instructions.
fn named_block(named: &[Named]) -> String {
    if named.is_empty() {
        return String::new();
    }
    let list: Vec<String> = named
        .iter()
        .map(|n| format!("  {} ({} bytes)", n.rel, n.bytes))
        .collect();
    format!(
        "\n\nThis project has further instructions in these files. They are not included here; read the ones that apply to what you are doing, with read_file:\n{}\n",
        list.join("\n")
    )
}

/// The paragraph every session opens with, whichever entry it came in
/// through.
///
/// The second sentence exists because of a real session: Claude's own
/// environment block said its cwd was not a git repository (true -- that is
/// the Agent Node's state directory), while workspace_info said the project
/// was one, and it refused to commit on the contradiction. Claude Code
/// cannot be stopped from describing the directory it runs in, so the
/// instructions say which one to believe.
pub(crate) fn base(workspace: &str) -> String {
    format!(
        "CCNM remote workspace \"{workspace}\". The project lives on another machine and is reachable only through the ccnm tools; there is no local copy. Whatever your own environment says about the current directory, its git status or its files describes the machine you run on, not the project: for the project, workspace_info is the truth. Every path you pass or receive is relative to the workspace root."
    )
}

/// The project's own instruction file, wrapped in the frame that says whose
/// rules these are.
fn project_block(file: &str, project: &Project) -> String {
    format!(
        "\n\n--- {file} from the workspace root. These are the project's own instructions, written for this project; they are not about the machine you run on. Follow them. ---\n{}\n--- end of {file} ---",
        project.text.trim_end()
    )
}

pub(crate) fn render(
    file: &str,
    workspace: &str,
    project: Option<&Project>,
    named: &[Named],
) -> String {
    let base = base(workspace);
    let more = named_block(named);
    let Some(project) = project else {
        return format!("{base}{more}\n{}", marker_file(file, None));
    };
    format!(
        "{base}{}{more}\n{}",
        project_block(file, project),
        marker_file(file, Some(project))
    )
}

/// The instruction files an external client's project may have, in the
/// order the Runtime looks for them.
///
/// Order, not provider: a bridge carries no provider, and what a client
/// calls itself in `clientInfo` is a string it chose. Guessing from it
/// would make the handshake depend on something anybody can write.
const EXTERNAL_PROJECT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// `initialize.result.instructions` for an external MCP client.
///
/// Three shapes, chosen by the workspace's `external_instructions`; the
/// mode sentence is always there, because a client that does not know it
/// cannot write will spend the session trying.
pub fn external(
    workspace: &str,
    root: &Path,
    policy: crate::config::ExternalInstructions,
    mode: crate::runtime::ExternalMode,
) -> String {
    use crate::config::ExternalInstructions as Policy;
    if policy == Policy::None {
        return String::new();
    }
    let mode_line = match mode {
        crate::runtime::ExternalMode::Read => {
            "\n\nThis session is read-only: it has no tool that changes a file or runs a command, and asking for one is refused by the machine the project is on, not by this text."
        }
        crate::runtime::ExternalMode::Coding => {
            "\n\nThis session may change the project. It holds that workspace's single write lock for as long as it lasts, so nothing else can be editing the same working tree at the same time."
        }
    };
    let head = format!("{}{mode_line}", base(workspace));
    if policy == Policy::Generic {
        return head;
    }
    // What is left of the cap once the frame is rendered, measured the same
    // way the provider budgets are: render the worst case and subtract.
    let worst = Project {
        source: EXTERNAL_PROJECT_FILES[0],
        bytes: usize::MAX,
        text: String::new(),
    };
    let frame = format!(
        "{head}{}\n{}",
        project_block(worst.source, &worst),
        marker_file(worst.source, Some(&worst))
    );
    let budget = MAX_INSTRUCTIONS_BYTES.saturating_sub(frame.len());
    for file in EXTERNAL_PROJECT_FILES {
        // A file that is there but unreadable is not a reason to fail the
        // handshake; the session works, without the project's rules. The
        // marker says which, and a person can act on it.
        match claude::context::find_file(root, file, budget) {
            Ok(Some(project)) => {
                return format!(
                    "{head}{}\n{}",
                    project_block(file, &project),
                    marker_file(file, Some(&project))
                );
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, file, "project instructions not readable"),
        }
    }
    format!("{head}\n{}", marker_file(EXTERNAL_PROJECT_FILES[0], None))
}

pub(crate) fn marker_file(file: &str, project: Option<&Project>) -> String {
    match project {
        None => format!("[project instructions: no {file} at the workspace root]"),
        Some(p) if !p.truncated() => {
            format!("[project instructions: {file}, {} bytes]", p.bytes)
        }
        Some(p) => format!(
            "[project instructions: {file}, {} bytes, first {} shown; read_file {file} for the rest]",
            p.bytes,
            p.included()
        ),
    }
}

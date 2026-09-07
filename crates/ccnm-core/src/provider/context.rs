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

pub(crate) fn render(
    file: &str,
    workspace: &str,
    project: Option<&Project>,
    named: &[Named],
) -> String {
    // The second sentence exists because of a real session: Claude's own
    // environment block said its cwd was not a git repository (true --
    // that is the Agent Node's state directory), while workspace_info
    // said the project was one, and it refused to commit on the
    // contradiction. Claude Code cannot be stopped from describing the
    // directory it runs in, so the instructions say which one to believe.
    let base = format!(
        "CCNM remote workspace \"{workspace}\". The project lives on another machine and is reachable only through the ccnm tools; there is no local copy. Whatever your own environment says about the current directory, its git status or its files describes the machine you run on, not the project: for the project, workspace_info is the truth. Every path you pass or receive is relative to the workspace root."
    );
    let more = named_block(named);
    let Some(project) = project else {
        return format!("{base}{more}\n{}", marker_file(file, None));
    };
    format!(
        "{base}\n\n--- {file} from the workspace root. These are the project's own instructions, written for this project; they are not about the machine you run on. Follow them. ---\n{}\n--- end of {file} ---{more}\n{}",
        project.text.trim_end(),
        marker_file(file, Some(project))
    )
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

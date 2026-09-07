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

// Compatibility constants for the current, sole provider and MCP budget.
pub const PROJECT_FILE: &str = AgentProvider::current().project_file();
pub const MAX_INSTRUCTIONS_BYTES: usize = claude::context::MAX_INSTRUCTIONS_BYTES;
pub const MAX_NAMED: usize = claude::context::MAX_NAMED;

pub fn find(root: &Path, budget: usize) -> Result<Option<Project>> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::find(root, budget),
    }
}
pub fn named(root: &Path) -> Vec<Named> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::named(root),
    }
}
pub fn budget(workspace: &str, named: &[Named]) -> usize {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::budget(workspace, named),
    }
}
pub fn instructions(workspace: &str, project: Option<&Project>, named: &[Named]) -> String {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::instructions(workspace, project, named),
    }
}
pub fn marker(project: Option<&Project>) -> String {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::marker(project),
    }
}
pub fn parse_marker(instructions: &str) -> Option<String> {
    match AgentProvider::current() {
        AgentProvider::Claude => claude::context::parse_marker(instructions),
    }
}

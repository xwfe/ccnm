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

/// How much of `initialize.result.instructions` a Host keeps, in the unit
/// it counts in.
///
/// The unit matters as much as the number: Claude Code compares a
/// JavaScript `string.length`, so a Chinese character costs 1 and the same
/// text measured in bytes costs 3. A budget in the wrong unit is either a
/// third of what fits or three times too much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// UTF-16 code units, what `string.length` counts.
    Utf16(usize),
    Bytes(usize),
}

impl Cap {
    pub fn limit(self) -> usize {
        match self {
            Cap::Utf16(n) | Cap::Bytes(n) => n,
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Cap::Utf16(_) => "UTF-16 code units",
            Cap::Bytes(_) => "bytes",
        }
    }

    pub fn measure(self, text: &str) -> usize {
        match self {
            Cap::Utf16(_) => text.encode_utf16().count(),
            Cap::Bytes(_) => text.len(),
        }
    }

    pub fn fits(self, text: &str) -> bool {
        self.measure(text) <= self.limit()
    }

    /// What is left of this cap once `used` is spent, in the same unit.
    pub fn minus(self, used: &str) -> Cap {
        let rest = self.limit().saturating_sub(self.measure(used));
        match self {
            Cap::Utf16(_) => Cap::Utf16(rest),
            Cap::Bytes(_) => Cap::Bytes(rest),
        }
    }

    /// The longest prefix of `text` within the cap that ends on a line
    /// boundary; with no newline to fall back to, a character boundary.
    /// Half a rule is worse than one rule fewer.
    pub fn keep(self, text: &str) -> &str {
        if self.fits(text) {
            return text;
        }
        let mut used = 0;
        let mut end = 0;
        for (at, c) in text.char_indices() {
            used += match self {
                Cap::Utf16(_) => c.len_utf16(),
                Cap::Bytes(_) => c.len_utf8(),
            };
            if used > self.limit() {
                break;
            }
            end = at + c.len_utf8();
        }
        let head = &text[..end];
        head.rfind('\n').map_or(head, |nl| &head[..=nl])
    }
}

/// Claude Code drops everything past 2048 UTF-16 code units of a server's
/// instructions and appends `… [truncated]` (2.1.269: `FT=2048`, compared
/// with `string.length`; its debug log says "Server instructions truncated
/// from 4600 to 2048 chars" for the old ccnm handshake). ccnm cuts first,
/// so what gets dropped is chosen here and the marker line can say so.
pub const CLAUDE_CODE_CAP: Cap = Cap::Utf16(2048);
/// Codex puts the whole text into its tool namespace description on the
/// code-mode path ccnm launches, with no cut found in 0.154, so the budget
/// ccnm always had stays.
pub const CODEX_CAP: Cap = Cap::Bytes(16 * 1024);
/// A bridge cannot know which Host is on the other end, so the strictest
/// one it knows about.
pub const EXTERNAL_CAP: Cap = CLAUDE_CODE_CAP;

// Compatibility project constants.
pub const PROJECT_FILE: &str = AgentProvider::current().project_file();
pub const MAX_NAMED: usize = claude::context::MAX_NAMED;

/// Room the list of further instruction files may take, in UTF-16 code
/// units, header included. About fifteen short paths. The list comes before
/// the project's own file, so without this bound a project with forty skills
/// would leave no room for the rules that apply to everything.
pub const MAX_NAMED_UNITS: usize = 768;

pub fn find(root: &Path, budget: Cap) -> Result<Option<Project>> {
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
pub fn budget(workspace: &str, named: &[Named]) -> Cap {
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
    /// What the Host this provider launches keeps of the handshake text.
    pub const fn instructions_cap(self) -> Cap {
        match self {
            Self::Claude => CLAUDE_CODE_CAP,
            Self::Codex => CODEX_CAP,
        }
    }
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

/// The lines that name the rest of the project's instructions, as many as
/// fit in [`MAX_NAMED_UNITS`]. When some do not, the header says how many
/// there are, so the model knows the list is not the whole story.
fn named_block(named: &[Named]) -> String {
    let render = |shown: usize| {
        let list: Vec<String> = named[..shown]
            .iter()
            .map(|n| format!("  {} ({} bytes)", n.rel, n.bytes))
            .collect();
        // The scan itself stops at MAX_NAMED, so a full scan only proves
        // "at least that many".
        let count = match named.len() {
            n if shown == n => String::new(),
            n if n >= MAX_NAMED => format!(", {shown} of {n} or more listed"),
            n => format!(", {shown} of {n} listed"),
        };
        format!(
            "\n\nThis project has further instructions in these files{count}. They are not included here; read the ones that apply to what you are doing, with read_file:\n{}",
            list.join("\n")
        )
    };
    if named.is_empty() {
        return String::new();
    }
    let fits = |text: &String| Cap::Utf16(MAX_NAMED_UNITS).fits(text);
    (0..=named.len())
        .rev()
        .map(render)
        .find(fits)
        .unwrap_or_else(|| render(0))
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

/// Order is what a Host that cuts from the end leaves standing: ccnm's own
/// paragraph, the line saying how much of the project's file is here and
/// how to read the rest, the other files to read, and only then the file
/// itself -- the one part that can be read again with `read_file`.
pub(crate) fn render(
    file: &str,
    workspace: &str,
    project: Option<&Project>,
    named: &[Named],
) -> String {
    let base = base(workspace);
    let marker = marker_file(file, project);
    let more = named_block(named);
    let body = project.map_or(String::new(), |p| project_block(file, p));
    format!("{base}\n{marker}{more}{body}")
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
    let budget = EXTERNAL_CAP.minus(&external_project(&head, worst.source, &worst));
    for file in EXTERNAL_PROJECT_FILES {
        // A file that is there but unreadable is not a reason to fail the
        // handshake; the session works, without the project's rules. The
        // marker says which, and a person can act on it.
        match claude::context::find_file(root, file, budget) {
            Ok(Some(project)) => return external_project(&head, file, &project),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, file, "project instructions not readable"),
        }
    }
    format!("{head}\n{}", marker_file(EXTERNAL_PROJECT_FILES[0], None))
}

/// Same order as [`render`]: the marker before the file it describes.
fn external_project(head: &str, file: &str, project: &Project) -> String {
    format!(
        "{head}\n{}{}",
        marker_file(file, Some(project)),
        project_block(file, project)
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

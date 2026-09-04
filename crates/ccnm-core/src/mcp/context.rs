//! The project's own `CLAUDE.md`, carried to the model in the MCP
//! handshake (design doc section 20).
//!
//! Why this file exists at all: Claude Code loads `CLAUDE.md` from its own
//! working directory. Under ccnm that directory is on the *work* machine
//! and holds nothing but session bookkeeping, while the project — and its
//! `CLAUDE.md` — is on the home machine, reachable only through the tools.
//! So nothing loads it, and the model works on a project whose rules it
//! has never read. Nobody notices, because the result is not an error: it
//! is a session that ignores conventions it was never told about.
//!
//! The fix is to read the file where it actually is (this process runs on
//! the machine that has it) and put it in `initialize.result.instructions`,
//! which Claude Code does read.
//!
//! Two limits, both deliberate:
//!
//! * Only the root `CLAUDE.md`. Not `.claude/rules/`, not skills, and
//!   certainly not the source: this is a metadata projection, not a
//!   project mirror. The moment source is copied, the consistency problem
//!   of the old SMB design is back.
//! * At most [`MAX_INSTRUCTIONS_BYTES`] for the whole handshake text. A
//!   long `CLAUDE.md` is cut at a line boundary and the model is told, in
//!   the marker line, how much it is missing and how to read the rest.
//!   Silently stuffing a 200 KiB file into every session's context is the
//!   one thing this must not do.

use std::path::Path;

use crate::error::{Error, Result};

/// The one project file this build projects.
pub const PROJECT_FILE: &str = "CLAUDE.md";

/// Upper bound on `initialize.result.instructions`, everything included.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;

/// The workspace root's `CLAUDE.md`, and how much of it fits.
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

/// Read `<root>/CLAUDE.md`, keeping at most `budget` bytes of it.
///
/// `Ok(None)` is the ordinary "this project has no CLAUDE.md" and is not a
/// problem. `Err` means there is something at that path that could not be
/// read — a directory, or a file this account has no permission for. That
/// is worth a doctor row, because it looks exactly like the file working
/// from the outside and the model would never see the difference.
pub fn find(root: &Path, budget: usize) -> Result<Option<Project>> {
    let path = root.join(PROJECT_FILE);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::internal(format!("cannot read {}", path.display())).with_source(e));
        }
    };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let bytes = text.len();
    Ok(Some(Project {
        bytes,
        text: keep(&text, budget),
    }))
}

/// The longest prefix of `text` that fits in `budget` and ends on a line
/// boundary. Half a rule is worse than one rule fewer.
fn keep(text: &str, budget: usize) -> String {
    let head = super::truncate_bytes(text, budget);
    if head.len() == text.len() {
        return head.to_string();
    }
    match head.rfind('\n') {
        Some(nl) => head[..=nl].to_string(),
        None => head.to_string(),
    }
}

/// How many bytes of `CLAUDE.md` fit, for this workspace name.
///
/// Measured, not guessed: the frame is rendered once around an empty body
/// with the longest number this machine can print, and what is left of the
/// cap is the budget. So the budget cannot drift away from the text that
/// actually gets sent when the wording here changes.
pub fn budget(workspace: &str) -> usize {
    let worst = Project {
        bytes: usize::MAX,
        text: String::new(),
    };
    MAX_INSTRUCTIONS_BYTES.saturating_sub(instructions(workspace, Some(&worst)).len())
}

/// The whole `initialize.result.instructions`: what ccnm has to say, then
/// the project's own file when it has one.
pub fn instructions(workspace: &str, project: Option<&Project>) -> String {
    // The second sentence exists because of a real session: Claude's own
    // environment block said its cwd was not a git repository (true --
    // that is the work machine's state directory), while workspace_info
    // said the project was one, and it refused to commit on the
    // contradiction. Claude Code cannot be stopped from describing the
    // directory it runs in, so the instructions say which one to believe.
    let base = format!(
        "CCNM remote workspace \"{workspace}\". The project lives on another machine and is reachable only through the ccnm tools; there is no local copy. Whatever your own environment says about the current directory, its git status or its files describes the machine you run on, not the project: for the project, workspace_info is the truth. Every path you pass or receive is relative to the workspace root."
    );
    let Some(project) = project else {
        return format!("{base}\n{}", marker(None));
    };
    format!(
        "{base}\n\n--- {PROJECT_FILE} from the workspace root. These are the project's own instructions, written for this project; they are not about the machine you run on. Follow them. ---\n{}\n--- end of {PROJECT_FILE} ---\n{}",
        project.text.trim_end(),
        marker(Some(project))
    )
}

/// The bracketed line that says what was projected. Same shape as the
/// `[server pid ..]` line of `workspace_info`, and for the same reason:
/// the text is the only channel the model is shown, so anything a probe
/// needs to check has to be in the text the model reads.
pub fn marker(project: Option<&Project>) -> String {
    match project {
        None => format!("[project instructions: no {PROJECT_FILE} at the workspace root]"),
        Some(p) if !p.truncated() => {
            format!("[project instructions: {PROJECT_FILE}, {} bytes]", p.bytes)
        }
        Some(p) => format!(
            "[project instructions: {PROJECT_FILE}, {} bytes, first {} shown; read_file {PROJECT_FILE} for the rest]",
            p.bytes,
            p.included()
        ),
    }
}

/// What [`marker`] put in the brackets, out of a handshake's instructions.
/// `None` from a server that sends no marker at all.
pub fn parse_marker(instructions: &str) -> Option<String> {
    let line = instructions
        .lines()
        .rev()
        .find(|l| l.starts_with("[project instructions: "))?;
    Some(
        line.strip_prefix("[project instructions: ")?
            .strip_suffix(']')?
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-ctx-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_claude_md_is_not_an_error_and_says_so() {
        let dir = temp("none");
        assert_eq!(find(&dir, 4096).unwrap(), None);
        let text = instructions("xshun", None);
        assert!(text.contains("CCNM remote workspace \"xshun\""));
        assert_eq!(
            parse_marker(&text).as_deref(),
            Some("no CLAUDE.md at the workspace root")
        );
    }

    #[test]
    fn a_short_file_is_projected_whole() {
        let dir = temp("short");
        std::fs::write(dir.join("CLAUDE.md"), "# rules\n\n- 用中文回复\n").unwrap();
        let found = find(&dir, 4096).unwrap().unwrap();
        assert!(!found.truncated());
        assert_eq!(found.bytes, found.included());
        let text = instructions("xshun", Some(&found));
        assert!(text.contains("- 用中文回复"));
        assert!(text.contains("--- end of CLAUDE.md ---"));
        assert_eq!(
            parse_marker(&text).as_deref(),
            Some(format!("CLAUDE.md, {} bytes", found.bytes).as_str())
        );
    }

    /// The cap is the whole point: a project file bigger than the
    /// handshake allows must not be able to push the instructions past it,
    /// and the model must be told what it is missing.
    #[test]
    fn a_huge_file_is_cut_at_a_line_and_the_marker_admits_it() {
        let dir = temp("huge");
        let line = "- 每一条规则都写在这里，长得很。\n";
        let big = line.repeat(4000);
        std::fs::write(dir.join("CLAUDE.md"), &big).unwrap();

        let found = find(&dir, budget("xshun")).unwrap().unwrap();
        assert!(found.truncated());
        assert_eq!(found.bytes, big.len());
        // Cut on a line boundary, and never inside a multi-byte character.
        assert!(found.text.ends_with('\n'));
        assert!(big.starts_with(&found.text));

        let text = instructions("xshun", Some(&found));
        assert!(text.len() <= MAX_INSTRUCTIONS_BYTES, "{}", text.len());
        // Close to the cap, or the budget is being wasted.
        assert!(text.len() > MAX_INSTRUCTIONS_BYTES - 200, "{}", text.len());
        let marker = parse_marker(&text).unwrap();
        assert!(
            marker.contains(&format!("{} bytes, first ", big.len())),
            "{marker}"
        );
        assert!(
            marker.contains("read_file CLAUDE.md for the rest"),
            "{marker}"
        );
    }

    #[test]
    fn a_claude_md_that_cannot_be_read_is_an_error_not_a_silent_none() {
        let dir = temp("dir");
        std::fs::create_dir(dir.join("CLAUDE.md")).unwrap();
        let err = find(&dir, 4096).unwrap_err();
        assert!(err.message().contains("CLAUDE.md"), "{err}");
    }

    #[test]
    fn keep_cuts_on_a_line_and_falls_back_to_a_character_boundary() {
        assert_eq!(keep("a\nb\nc\n", 99), "a\nb\nc\n");
        assert_eq!(keep("a\nb\nc\n", 4), "a\nb\n");
        // The budget lands mid-line: that line goes, whole.
        assert_eq!(keep("a\nbbbb\n", 4), "a\n");
        // No newline to fall back to: the character boundary is the limit.
        assert_eq!(keep("中中中", 4), "中");
    }

    #[test]
    fn parse_marker_ignores_text_without_one() {
        assert_eq!(parse_marker("nothing here\n[server pid 1, call 1]"), None);
    }
}

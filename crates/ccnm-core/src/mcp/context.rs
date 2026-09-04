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
//! Only the root `CLAUDE.md` is carried. A project usually has more --
//! nested `CLAUDE.md` files, `.claude/rules/`, skills -- and those are
//! *named* rather than copied: the handshake lists where they are and the
//! model reads the ones it needs with `read_file`.
//!
//! That asymmetry is the whole design. Inlining everything would cost the
//! same 16 KiB budget for every session whether or not any of it mattered,
//! and a project with more rules than budget would silently lose some. A
//! list costs a few hundred bytes for any number of files and cannot
//! overflow into the part that matters. The root `CLAUDE.md` is the
//! exception because it applies to everything the model does; the rest
//! applies to whatever it happens to touch.
//!
//! What is never carried, in either form: **anything executable**. Not
//! hooks, not MCP server definitions, not plugins. Those would run on the
//! *work* machine -- the one holding the Anthropic credential -- so
//! honouring them would let any repository this tool opens execute
//! commands where the credentials are. That is the inversion the whole
//! architecture exists to prevent. The user's own Claude settings still
//! load, from the work machine, which is where they belong.
//!
//! And at most [`MAX_INSTRUCTIONS_BYTES`] for the whole handshake text. A
//! long `CLAUDE.md` is cut at a line boundary and the model is told, in
//! the marker line, how much it is missing and how to read the rest.
//! Silently stuffing a 200 KiB file into every session's context is the
//! one thing this must not do.

use std::path::Path;

use crate::error::{Error, Result};

/// The one project file this build projects.
pub const PROJECT_FILE: &str = "CLAUDE.md";

/// Upper bound on `initialize.result.instructions`, everything included.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;

/// How many further instruction files the handshake will name. Past this
/// the list stops being an aid and becomes the noise it was meant to
/// avoid.
pub const MAX_NAMED: usize = 40;

/// How many candidates the scan will hold before it stops looking. The
/// list is sorted and *then* cut to [`MAX_NAMED`], so which files are
/// named does not depend on the order the filesystem happened to return
/// them in -- one project must always produce the same handshake. This is
/// only the bound on how much can be sorted.
const MAX_SCANNED: usize = 1_000;

/// How deep to look for nested `CLAUDE.md`. Three levels reaches
/// `crates/x/y/CLAUDE.md`, which is as far as anyone puts one.
const MAX_DEPTH: usize = 3;

/// Directories never worth walking for instruction files: they hold
/// dependencies and build output, they are enormous, and a `CLAUDE.md` in
/// one belongs to somebody else's project.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".tox",
];

/// Another instruction file the project has, named but not carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// Path relative to the workspace root, as the model must pass it.
    pub rel: String,
    pub bytes: u64,
}

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
pub fn budget(workspace: &str, named: &[Named]) -> usize {
    let worst = Project {
        bytes: usize::MAX,
        text: String::new(),
    };
    // The listing is measured too, so naming a lot of rule files takes
    // room from the projected CLAUDE.md rather than pushing the handshake
    // over the cap. That is the right way round: the list is bounded and
    // the file is not.
    MAX_INSTRUCTIONS_BYTES.saturating_sub(instructions(workspace, Some(&worst), named).len())
}

/// Every further instruction file the project has, sorted, capped at
/// [`MAX_NAMED`].
///
/// Named, not read: the point is to cost a few hundred bytes regardless
/// of how much project context exists, so the model can spend the tools
/// it already has on the parts that turn out to matter.
///
/// Three shapes, matching where Claude Code itself looks: a `CLAUDE.md`
/// in a subdirectory, a rule in `.claude/rules/`, and a skill's
/// `SKILL.md`. The walk is depth-bounded and skips dependency and build
/// directories, so it costs a handful of `read_dir` calls on any project
/// and cannot be made expensive by a large one.
pub fn named(root: &Path) -> Vec<Named> {
    let mut found = Vec::new();
    walk(root, root, 0, &mut found);
    for dir in [".claude/rules", ".claude/skills"] {
        collect_claude_dir(root, dir, &mut found);
    }
    // Sorted before it is cut, so the same project always names the same
    // files. `read_dir` returns entries in whatever order the filesystem
    // likes, and a handshake that varies between runs for no reason is a
    // session that behaves differently for no reason.
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    found.truncate(MAX_NAMED);
    found
}

/// Nested `CLAUDE.md`, excluding the root's own -- that one is carried in
/// full, so naming it as well would just be confusing.
fn walk(root: &Path, dir: &Path, depth: usize, found: &mut Vec<Named>) {
    if depth > MAX_DEPTH || found.len() >= MAX_SCANNED {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            // Symlinked directories are not followed: the same rule the
            // file tools use, and for the same reason.
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(root, &entry.path(), depth + 1, found);
        } else if name == PROJECT_FILE && depth > 0 {
            push(root, &entry.path(), found);
        }
    }
}

/// `.claude/rules/*.md`, and each skill's `SKILL.md`.
fn collect_claude_dir(root: &Path, rel: &str, found: &mut Vec<Named>) {
    let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
            push(root, &path, found);
        } else if kind.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                push(root, &skill, found);
            }
        }
    }
}

fn push(root: &Path, path: &Path, found: &mut Vec<Named>) {
    if found.len() >= MAX_SCANNED {
        return;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return;
    };
    let Some(rel) = rel.to_str() else { return };
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    found.push(Named {
        rel: rel.to_string(),
        bytes,
    });
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

/// The whole `initialize.result.instructions`: what ccnm has to say, then
/// the project's own file when it has one.
pub fn instructions(workspace: &str, project: Option<&Project>, named: &[Named]) -> String {
    // The second sentence exists because of a real session: Claude's own
    // environment block said its cwd was not a git repository (true --
    // that is the work machine's state directory), while workspace_info
    // said the project was one, and it refused to commit on the
    // contradiction. Claude Code cannot be stopped from describing the
    // directory it runs in, so the instructions say which one to believe.
    let base = format!(
        "CCNM remote workspace \"{workspace}\". The project lives on another machine and is reachable only through the ccnm tools; there is no local copy. Whatever your own environment says about the current directory, its git status or its files describes the machine you run on, not the project: for the project, workspace_info is the truth. Every path you pass or receive is relative to the workspace root."
    );
    let more = named_block(named);
    let Some(project) = project else {
        return format!("{base}{more}\n{}", marker(None));
    };
    format!(
        "{base}\n\n--- {PROJECT_FILE} from the workspace root. These are the project's own instructions, written for this project; they are not about the machine you run on. Follow them. ---\n{}\n--- end of {PROJECT_FILE} ---{more}\n{}",
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
    use std::fs;

    /// The three places Claude Code itself keeps project instructions get
    /// named. Named and not carried: a project with more rules than
    /// budget must not silently lose some, and a session that never
    /// touches the frontend should not pay for the frontend's rules.
    #[test]
    fn the_projects_other_instruction_files_are_named_not_carried() {
        let root = std::env::temp_dir().join(format!("ccnm-named-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude/rules")).unwrap();
        fs::create_dir_all(root.join(".claude/skills/deploy")).unwrap();
        fs::create_dir_all(root.join("crates/core")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("CLAUDE.md"), "root rules\n").unwrap();
        fs::write(root.join("crates/core/CLAUDE.md"), "core rules\n").unwrap();
        fs::write(root.join(".claude/rules/style.md"), "style\n").unwrap();
        fs::write(root.join(".claude/rules/notes.txt"), "not markdown\n").unwrap();
        fs::write(root.join(".claude/skills/deploy/SKILL.md"), "deploy\n").unwrap();
        // Somebody else's project, vendored in.
        fs::write(root.join("node_modules/pkg/CLAUDE.md"), "theirs\n").unwrap();

        let found = named(&root);
        let names: Vec<&str> = found.iter().map(|n| n.rel.as_str()).collect();
        assert_eq!(
            names,
            vec![
                ".claude/rules/style.md",
                ".claude/skills/deploy/SKILL.md",
                "crates/core/CLAUDE.md",
            ]
        );
        assert!(found[0].bytes > 0);

        let project = find(&root, budget("x", &found)).unwrap();
        let text = instructions("x", project.as_ref(), &found);
        // The root file is carried; the others are only pointed at.
        assert!(text.contains("root rules"), "{text}");
        assert!(text.contains("crates/core/CLAUDE.md"), "{text}");
        assert!(!text.contains("core rules"), "{text}");
        assert!(text.contains("read_file"), "{text}");
        assert!(text.len() <= MAX_INSTRUCTIONS_BYTES);
    }

    /// A project with hundreds of rule files must not turn the handshake
    /// into a directory listing, and must not push it over the cap.
    #[test]
    fn naming_is_bounded_and_takes_its_room_from_the_projected_file() {
        let root = std::env::temp_dir().join(format!("ccnm-named-many-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude/rules")).unwrap();
        for n in 0..200 {
            fs::write(root.join(format!(".claude/rules/r{n:03}.md")), "x\n").unwrap();
        }
        fs::write(root.join("CLAUDE.md"), "a\n".repeat(40_000)).unwrap();

        let found = named(&root);
        assert_eq!(found.len(), MAX_NAMED);
        // Which 40, not just how many: sorted before it is cut, so the
        // handshake does not change between runs because `read_dir`
        // returned a different order.
        let names: Vec<&str> = found.iter().map(|n| n.rel.as_str()).collect();
        assert_eq!(names[0], ".claude/rules/r000.md");
        assert_eq!(names[MAX_NAMED - 1], ".claude/rules/r039.md");
        let narrowed = budget("x", &found);
        assert!(
            narrowed < budget("x", &[]),
            "naming files has to cost the projected file, not the cap"
        );
        let project = find(&root, narrowed).unwrap();
        let text = instructions("x", project.as_ref(), &found);
        assert!(
            text.len() <= MAX_INSTRUCTIONS_BYTES,
            "{} bytes is over the cap",
            text.len()
        );
        // And the truncation is still announced.
        assert!(text.contains("for the rest"), "{text}");
    }

    /// Nothing executable is ever named, whatever the project puts in
    /// `.claude/`. Hooks and MCP definitions would run on the work
    /// machine, where the credentials are.
    #[test]
    fn executable_project_config_is_never_mentioned() {
        let root = std::env::temp_dir().join(format!("ccnm-named-exec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude/hooks")).unwrap();
        fs::write(
            root.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"command":"curl evil.example"}]}}"#,
        )
        .unwrap();
        fs::write(root.join(".claude/hooks/run.sh"), "#!/bin/sh\ncurl x\n").unwrap();
        fs::write(root.join(".mcp.json"), r#"{"mcpServers":{"x":{}}}"#).unwrap();

        let found = named(&root);
        assert!(found.is_empty(), "{found:?}");
        let text = instructions("x", None, &found);
        assert!(!text.contains("settings.json"), "{text}");
        assert!(!text.contains("hooks"), "{text}");
        assert!(!text.contains("mcp.json"), "{text}");
    }

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
        let text = instructions("xshun", None, &[]);
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
        let text = instructions("xshun", Some(&found), &[]);
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

        let found = find(&dir, budget("xshun", &[])).unwrap().unwrap();
        assert!(found.truncated());
        assert_eq!(found.bytes, big.len());
        // Cut on a line boundary, and never inside a multi-byte character.
        assert!(found.text.ends_with('\n'));
        assert!(big.starts_with(&found.text));

        let text = instructions("xshun", Some(&found), &[]);
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

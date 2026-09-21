//! The project's own skills: found where the project is, handed to the
//! model by one tool.
//!
//! Why this exists: Claude Code and Codex find skills by looking under the
//! directory they run in. Under ccnm that directory is on the Agent Node and
//! holds session bookkeeping; the project, and its `.claude/skills/`, is
//! here on the Runtime Node. So nothing finds them, and the failure is
//! silent: a session that does the deploy by guesswork because it was never
//! told the project wrote down how.
//!
//! What the handshake used to do about it was name each `SKILL.md` path and
//! leave the rest to `read_file`. A path says nothing about *when* to read
//! it. A skill's description is the part that does, and it is the part the
//! native tools keep in context at all times.
//!
//! So the catalog -- name and description -- rides in this tool's own
//! description, where every client keeps it in front of the model
//! (measured, P36.1: Claude Code keeps 2048 UTF-16 code units of each
//! tool's description, separately from the 2048 it keeps of the
//! instructions; Codex keeps all of it and never asks for prompts or
//! resources, so a tool is the only channel it has). The body comes back
//! when the model asks for a skill by name.
//!
//! Nothing here executes anything. A skill's scripts are files in the
//! workspace: the model runs them with `exec_command`, on this machine, as
//! the execution account, under the same write guard and sandbox as every
//! other command. Even the `` !`command` `` lines a skill may carry -- which
//! the native client runs while loading the skill -- are listed and left
//! alone: a call that reads must not be a call that runs what the
//! repository chose, around the approval `exec_command` is gated by.

use std::collections::BTreeSet;
use std::path::Path;

use rmcp::schemars;
use serde::Deserialize;
use toexec_skill::{Frontmatter, Reading, args, frontmatter, inject};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::path;
use crate::provider::context::Cap;

/// The tool's name. One tool, not a list/get pair: every tool costs a
/// description in every session, and "no name" is an obvious way to ask
/// what there is.
pub const TOOL: &str = "load_skill";

/// Where skills live, in the order that wins a name. The first two hold
/// `<name>/SKILL.md`; `.agents/skills` is the cross-agent spelling Codex
/// looks for. Commands are single files and lose to a skill of the same
/// name, as they do natively.
const SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];
const COMMAND_DIR: &str = ".claude/commands";
/// `commands/frontend/component.md` is as deep as anybody nests them.
const COMMAND_DEPTH: usize = 3;

/// How many the catalog holds. The native client stops at the same number
/// for skills that arrive over MCP.
pub const MAX_SKILLS: usize = 100;
/// A `SKILL.md` past this is skipped, not read: the guidance is "under 500
/// lines", and a file this big is a mistake or an attack on the scan.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// What one call returns of a body -- the most `read_file` returns too, and
/// well under what a Host accepts from one MCP result (Claude Code: 25,000
/// tokens). The rest is one `read_file` away and the text says from where.
const MAX_BODY_BYTES: usize = 64 * 1024;
/// What Claude Code keeps of one tool's description. Codex keeps more, but
/// the catalog must not depend on which client happened to connect: one
/// workspace is opened by all of them.
const DESCRIPTION_CAP: Cap = Cap::Utf16(2048);
/// Per skill, in the always-present catalog. Short, so that more skills
/// fit; the full text is in the list this tool returns without a name.
const CATALOG_DESCRIPTION_CHARS: usize = 200;
/// Per skill, in the returned list: what the native client shows of one.
const LIST_DESCRIPTION_CHARS: usize = 1536;

const INTRO: &str = "Load one of this project's skills: task instructions the project keeps in .claude/skills, .claude/commands or .agents/skills. Before starting a task that a skill below describes, call this with the skill's name and follow what comes back. A skill's scripts and reference files are ordinary workspace files under its directory: read them with read_file and run them with exec_command. Without a name this returns the full list.";

/// Frontmatter this server cannot honour, named in the loaded text so the
/// model does not assume they took effect.
const IGNORED_FIELDS: [&str; 8] = [
    "allowed-tools",
    "disallowed-tools",
    "hooks",
    "model",
    "effort",
    "context",
    "agent",
    "shell",
];

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct LoadSkillArgs {
    /// The skill to load, as named in this tool's description. Leave it out
    /// to get the full list with complete descriptions.
    #[serde(default)]
    pub name: Option<String>,
    /// What to pass to the skill, as one string -- the text a person would
    /// type after the skill's name.
    #[serde(default)]
    pub arguments: Option<String>,
    /// Anything this tool does not declare: reported back, not obeyed. See
    /// [`crate::mcp::Ignored`].
    #[serde(flatten)]
    pub ignored: crate::mcp::Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Skill,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// `description` and `when_to_use`, as one line. Never empty: a file
    /// with neither is described by its first line of text.
    pub description: String,
    pub kind: Kind,
    /// The `.md` file, workspace-relative.
    pub file: String,
    /// What `${CLAUDE_SKILL_DIR}` stands for: the directory holding the
    /// file, workspace-relative.
    pub dir: String,
    pub argument_hint: Option<String>,
    pub arguments: Vec<String>,
    /// `disable-model-invocation: true` takes this away: the skill is for a
    /// person to start, and this tool refuses it.
    pub model_invocable: bool,
    /// `user-invocable: false` takes this away: no prompt is offered.
    pub user_invocable: bool,
}

/// A file that looked like a skill and is not in the catalog, and why.
/// Shown in the list, because the alternative is a person wondering why
/// the skill they wrote does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Sorted by name, so one project always produces one description.
    pub skills: Vec<Skill>,
    pub skipped: Vec<Skipped>,
    /// There were more than [`MAX_SKILLS`].
    pub more: bool,
}

/// Scan the workspace. Cheap enough to do on every call -- a `read_dir` per
/// directory and one small file per skill -- which is what lets a skill the
/// model has just written be loaded in the same session.
pub fn discover(root: &Path) -> Catalog {
    let mut candidates: Vec<(Kind, String)> = Vec::new();
    for base in SKILL_DIRS {
        for name in children(&root.join(base)) {
            candidates.push((Kind::Skill, format!("{base}/{name}/SKILL.md")));
        }
    }
    commands(root, COMMAND_DIR, 0, &mut candidates);

    let mut catalog = Catalog::default();
    let mut taken = BTreeSet::new();
    for (kind, file) in candidates {
        let skill = match read(root, kind, &file) {
            Ok(Some(skill)) => skill,
            // A directory under skills/ with no SKILL.md in it is not a
            // broken skill, it is not a skill.
            Ok(None) => continue,
            Err(reason) => {
                catalog.skipped.push(Skipped { file, reason });
                continue;
            }
        };
        if !taken.insert(skill.name.clone()) {
            let winner = catalog
                .skills
                .iter()
                .find(|s| s.name == skill.name)
                .map_or("another file", |s| s.file.as_str());
            catalog.skipped.push(Skipped {
                file,
                reason: format!("the name \"{}\" is already taken by {winner}", skill.name),
            });
            continue;
        }
        if catalog.skills.len() == MAX_SKILLS {
            catalog.more = true;
            break;
        }
        catalog.skills.push(skill);
    }
    catalog.skills.sort_by(|a, b| a.name.cmp(&b.name));
    catalog
}

/// Entry names of a directory, sorted: `read_dir` order is the
/// filesystem's, and which skill wins a name must not depend on it.
fn children(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn commands(root: &Path, rel: &str, depth: usize, out: &mut Vec<(Kind, String)>) {
    if depth >= COMMAND_DEPTH {
        return;
    }
    for name in children(&root.join(rel)) {
        let child = format!("{rel}/{name}");
        if root.join(&child).is_dir() {
            commands(root, &child, depth + 1, out);
        } else if name.ends_with(".md") {
            out.push((Kind::Command, child));
        }
    }
}

/// One candidate file. `Ok(None)`: nothing there. `Err`: something there
/// that cannot be offered, with the reason a person will read.
fn read(root: &Path, kind: Kind, file: &str) -> std::result::Result<Option<Skill>, String> {
    // Through the same policy as `read_file`: a skills directory that is a
    // symlink out of the workspace is refused here exactly as it would be
    // there, rather than becoming the one way to read outside the root.
    let resolved = match path::resolve_read(root, file) {
        Ok(resolved) => resolved,
        Err(e) if e.code() == ErrorCode::InvalidArgs => return Ok(None),
        Err(e) => return Err(e.message().to_string()),
    };
    let meta = std::fs::metadata(resolved.abs()).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} bytes; a skill file over {MAX_FILE_BYTES} is not read",
            meta.len()
        ));
    }
    let raw = std::fs::read_to_string(resolved.abs()).map_err(|e| e.to_string())?;
    let (front, body) = frontmatter::split(&raw);
    let front = match front {
        Some(text) => frontmatter::parse(text).map_err(|e| e.to_string())?,
        None => Frontmatter::default(),
    };

    let (dir, stem) = match file.rsplit_once('/') {
        Some((dir, leaf)) => (dir, leaf.trim_end_matches(".md")),
        None => ("", file),
    };
    let fallback = match kind {
        Kind::Skill => dir.rsplit('/').next().unwrap_or(dir),
        Kind::Command => stem,
    };
    let name = front
        .text("name")
        .filter(|n| valid_name(n))
        .unwrap_or(fallback);
    if !valid_name(name) {
        return Err(format!("\"{name}\" cannot be used as a skill name"));
    }
    // `when_to_use` and `argument-hint` are for show, and the host shows
    // String(value): the docs' own `argument-hint: [issue-number]` is a YAML
    // list and still has text.
    let description = [
        front.text("description").map(str::to_string),
        front.string("when_to_use"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let description = one_line(if description.is_empty() {
        first_text_line(body)
    } else {
        &description
    });
    if description.is_empty() {
        return Err("no description, and no text to take one from".into());
    }
    Ok(Some(Skill {
        name: name.to_string(),
        description,
        kind,
        file: resolved.rel().to_string(),
        dir: dir.to_string(),
        argument_hint: front.string("argument-hint").map(|hint| one_line(&hint)),
        arguments: front.words("arguments"),
        model_invocable: front.flag("disable-model-invocation") != Some(true),
        // Not the mirror image of the line above, because the host's two
        // switches are not: `user-invocable` left out means yes, but once
        // written only a true counts -- an empty value or a word it does not
        // know takes the skill off the `/` menu.
        user_invocable: front.get("user-invocable").is_none()
            || front.flag("user-invocable") == Some(true),
    }))
}

/// Usable as a tool argument, a prompt name and a line in a list.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_text_line(body: &str) -> &str {
    body.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let head: String = text.chars().take(chars.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

fn entry(skill: &Skill, chars: usize) -> String {
    let hint = match (&skill.argument_hint, skill.arguments.is_empty()) {
        (Some(hint), _) => format!(" (arguments: {hint})"),
        (None, false) => format!(" (arguments: {})", skill.arguments.join(" ")),
        (None, true) => String::new(),
    };
    format!(
        "- {}{hint}: {}",
        skill.name,
        clip(&skill.description, chars)
    )
}

impl Catalog {
    fn offered(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter().filter(|s| s.model_invocable)
    }

    /// The tool's description: what the model has in front of it for the
    /// whole session. As many skills as fit in [`DESCRIPTION_CAP`], and when
    /// some do not, their names -- a name is enough to ask for one.
    pub fn description(&self) -> String {
        let offered: Vec<&Skill> = self.offered().collect();
        if offered.is_empty() {
            return format!("{INTRO}\n\nThis workspace has no skills right now.");
        }
        let render = |shown: usize| {
            let mut text = format!("{INTRO}\n\nSkills in this workspace:\n");
            let lines: Vec<String> = offered[..shown]
                .iter()
                .map(|s| entry(s, CATALOG_DESCRIPTION_CHARS))
                .collect();
            text.push_str(&lines.join("\n"));
            if shown < offered.len() {
                let rest: Vec<&str> = offered[shown..].iter().map(|s| s.name.as_str()).collect();
                text.push_str(&format!(
                    "\n{} more, described in the full list: {}",
                    rest.len(),
                    rest.join(", ")
                ));
            }
            text
        };
        (0..=offered.len())
            .rev()
            .map(render)
            .find(|text| DESCRIPTION_CAP.fits(text))
            // Even the names do not fit: say how many and stop.
            .unwrap_or_else(|| {
                format!(
                    "{INTRO}\n\nThis workspace has {} skills; call this without a name to list them.",
                    offered.len()
                )
            })
    }

    /// What a call without a name returns.
    pub fn list(&self) -> String {
        let mut out = String::new();
        let offered: Vec<&Skill> = self.offered().collect();
        if offered.is_empty() {
            out.push_str("This workspace has no skills you can load.\n");
        } else {
            out.push_str(&format!(
                "{} skill(s). Load one with {TOOL} and its name.\n\n",
                offered.len()
            ));
            for skill in &offered {
                out.push_str(&entry(skill, LIST_DESCRIPTION_CHARS));
                out.push_str(&format!("\n  [{}]\n", skill.file));
            }
        }
        let user_only: Vec<&Skill> = self.skills.iter().filter(|s| !s.model_invocable).collect();
        if !user_only.is_empty() {
            out.push_str("\nOnly a person can start these (disable-model-invocation):\n");
            for skill in user_only {
                out.push_str(&format!("- {} [{}]\n", skill.name, skill.file));
            }
        }
        if self.more {
            out.push_str(&format!(
                "\nThere are more than {MAX_SKILLS}; the rest are not offered.\n"
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str("\nNot offered:\n");
            for skipped in &self.skipped {
                out.push_str(&format!("- {}: {}\n", skipped.file, skipped.reason));
            }
        }
        out
    }
}

/// The tool: the list, or one skill's instructions.
pub fn load_skill(root: &Path, call: &LoadSkillArgs, session: Option<&str>) -> Result<String> {
    let catalog = discover(root);
    let Some(name) = call
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        return Ok(catalog.list());
    };
    let skill = find(&catalog, name)?;
    if !skill.model_invocable {
        return Err(Error::policy(format!(
            "skill \"{}\" is marked disable-model-invocation: only a person can start it",
            skill.name
        )));
    }
    render(
        root,
        skill,
        call.arguments.as_deref().unwrap_or(""),
        session,
    )
}

/// The same text, for a person who started the skill through a prompt.
/// `disable-model-invocation` does not apply: this *is* the person.
pub fn prompt_text(
    root: &Path,
    name: &str,
    arguments: &str,
    session: Option<&str>,
) -> Result<String> {
    let catalog = discover(root);
    let skill = find(&catalog, name)?;
    if !skill.user_invocable {
        return Err(Error::invalid_args(format!(
            "skill \"{}\" is marked user-invocable: false",
            skill.name
        )));
    }
    render(root, skill, arguments, session)
}

fn find<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a Skill> {
    // People and models both write `/deploy`.
    let wanted = name.trim_start_matches('/');
    catalog
        .skills
        .iter()
        .find(|s| s.name == wanted)
        .ok_or_else(|| {
            let known: Vec<&str> = catalog.skills.iter().map(|s| s.name.as_str()).collect();
            Error::invalid_args(if known.is_empty() {
                format!("no skill named \"{wanted}\": this workspace has no skills")
            } else {
                format!(
                    "no skill named \"{wanted}\"; there are: {}",
                    known.join(", ")
                )
            })
        })
}

fn render(root: &Path, skill: &Skill, arguments: &str, session: Option<&str>) -> Result<String> {
    let resolved = path::resolve_read(root, &skill.file)?;
    let raw = std::fs::read_to_string(resolved.abs())
        .map_err(|e| Error::invalid_args(format!("cannot read {}", skill.file)).with_source(e))?;
    let (front_text, body) = frontmatter::split(&raw);
    let front = front_text
        .and_then(|text| frontmatter::parse(text).ok())
        .unwrap_or_default();
    // Line numbers the model can hand to `read_file`: lines of the file,
    // not of the body.
    let body_starts = raw[..raw.len() - body.len()].lines().count();

    let mut head = format!(
        "[skill {} -- {}, {} bytes; ${{CLAUDE_SKILL_DIR}} is {}]\n",
        skill.name,
        skill.file,
        raw.len(),
        if skill.dir.is_empty() {
            "."
        } else {
            &skill.dir
        }
    );
    let injections = inject::find(body);
    if !injections.is_empty() {
        head.push_str(&format!(
            "[{} command(s) this skill wants run as it loads were NOT run. Their output is not below; where you need it, run the command with exec_command:\n",
            injections.len()
        ));
        for found in &injections {
            let first = found.command.lines().next().unwrap_or("");
            let more = if found.command.lines().count() > 1 {
                " ..."
            } else {
                ""
            };
            head.push_str(&format!(
                "  line {}: {first}{more}\n",
                found.line + body_starts
            ));
        }
        head.push_str("]\n");
    }
    let ignored: Vec<&str> = IGNORED_FIELDS
        .into_iter()
        .filter(|key| front.get(key).is_some())
        .collect();
    if !ignored.is_empty() {
        head.push_str(&format!(
            "[frontmatter with no effect here: {}]\n",
            ignored.join(", ")
        ));
    }
    // Both are for the author, through the model: the file reads differently
    // natively, and nothing else would ever say so.
    if front.reading() == Reading::Lenient {
        head.push_str("[this frontmatter is not valid YAML: native Claude Code ignores all of it (name, description, disable-model-invocation); it was read leniently here]\n");
    }
    for group in front.duplicates() {
        let lines: Vec<String> = group.iter().map(|(_, line)| line.to_string()).collect();
        head.push_str(&format!(
            "[frontmatter sets \"{}\" more than once (lines {}); the last one counts]\n",
            group[group.len() - 1].0,
            lines.join(", ")
        ));
    }

    let context = args::Context {
        skill_dir: Some(if skill.dir.is_empty() {
            "."
        } else {
            &skill.dir
        }),
        project_dir: Some("."),
        session_id: session,
    };
    let filled = args::substitute(
        body.trim_start_matches('\n'),
        arguments,
        &skill.arguments,
        context,
    );
    let kept = Cap::Bytes(MAX_BODY_BYTES).keep(&filled);
    let mut out = format!("{head}\n{kept}");
    if kept.len() < filled.len() {
        let shown = kept.lines().count();
        out.push_str(&format!(
            "\n[cut after {shown} lines of the skill's text; read_file {} from line {} for the rest]\n",
            skill.file,
            body_starts + shown + 1
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn workspace(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-skills-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn names(catalog: &Catalog) -> Vec<&str> {
        catalog.skills.iter().map(|s| s.name.as_str()).collect()
    }

    const DEPLOY: &str = "---\nname: deploy\ndescription: >\n  Deploy the service.\n  Use after tests pass.\nargument-hint: <env>\narguments: [env]\nallowed-tools: Bash(git *)\n---\n\n# Deploy\n\nTarget: $env. Status first: !`git status --short`\nRun ${CLAUDE_SKILL_DIR}/scripts/go.sh $ARGUMENTS\n";

    #[test]
    fn skills_and_commands_are_found_in_all_three_places() {
        let root = workspace("found");
        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        write(
            &root,
            ".agents/skills/review/SKILL.md",
            "---\ndescription: Review a change.\n---\nbody\n",
        );
        write(
            &root,
            ".claude/commands/fix.md",
            "Fix the issue $ARGUMENTS\n",
        );
        write(
            &root,
            ".claude/commands/frontend/component.md",
            "---\ndescription: New component\n---\nMake $0\n",
        );
        // Not skills: a directory without SKILL.md, a stray file, a dotfile.
        write(&root, ".claude/skills/empty/notes.txt", "x");
        write(&root, ".claude/skills/README.md", "x");
        write(&root, ".claude/commands/.hidden.md", "x");

        let catalog = discover(&root);
        assert_eq!(names(&catalog), ["component", "deploy", "fix", "review"]);
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);

        let deploy = &catalog.skills[1];
        assert_eq!(
            deploy.description,
            "Deploy the service. Use after tests pass."
        );
        assert_eq!(deploy.kind, Kind::Skill);
        assert_eq!(deploy.file, ".claude/skills/deploy/SKILL.md");
        assert_eq!(deploy.dir, ".claude/skills/deploy");
        assert_eq!(deploy.arguments, ["env"]);
        // No frontmatter at all: named for the file, described by its text.
        let fix = &catalog.skills[2];
        assert_eq!(
            (fix.kind, fix.description.as_str()),
            (Kind::Command, "Fix the issue $ARGUMENTS")
        );
        // No `name`: a skill is named for its directory.
        assert_eq!(catalog.skills[3].file, ".agents/skills/review/SKILL.md");
    }

    #[test]
    fn a_skill_beats_a_command_of_the_same_name_and_the_loser_is_explained() {
        let root = workspace("collide");
        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        write(&root, ".claude/commands/deploy.md", "the old command\n");
        let catalog = discover(&root);
        assert_eq!(names(&catalog), ["deploy"]);
        assert_eq!(catalog.skills[0].kind, Kind::Skill);
        assert_eq!(catalog.skipped.len(), 1);
        assert_eq!(catalog.skipped[0].file, ".claude/commands/deploy.md");
        assert!(
            catalog.skipped[0]
                .reason
                .contains(".claude/skills/deploy/SKILL.md")
        );
    }

    #[test]
    fn what_cannot_be_offered_says_why_instead_of_vanishing() {
        let root = workspace("skipped");
        // An unclosed quote: unreadable here and natively. (Until P45 this
        // used `description: &anchor x`, which the host reads and so, now,
        // does this -- see the next test.)
        write(
            &root,
            ".claude/skills/broken/SKILL.md",
            "---\nname: broken\ndescription: \"open\n---\nbody\n",
        );
        write(
            &root,
            ".claude/skills/blank/SKILL.md",
            "---\nname: blank\n---\n\n\n",
        );
        let catalog = discover(&root);
        assert!(catalog.skills.is_empty());
        let reasons: Vec<(&str, &str)> = catalog
            .skipped
            .iter()
            .map(|s| (s.file.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(reasons[0].0, ".claude/skills/blank/SKILL.md");
        assert!(reasons[0].1.contains("no description"), "{reasons:?}");
        assert!(reasons[1].1.contains("frontmatter line 2"), "{reasons:?}");
        assert!(catalog.list().contains("frontmatter line 2"));
    }

    /// What YAML itself cannot read but Claude Code reads anyway, by quoting
    /// the value and trying again, is offered here too. Before P45 all four
    /// were skipped with "unsupported YAML construct" -- and the hint in the
    /// official docs' own example came out empty.
    #[test]
    fn what_the_host_reads_by_quoting_is_offered() {
        let root = workspace("rescued");
        for (name, front) in [
            ("tick", "description: `git` helper\n"),
            ("at", "description: @claude does it\n"),
            ("bold", "description: *Bold* first\n"),
            (
                "hint",
                "description: fix\nargument-hint: [filename] [format]\n",
            ),
            ("list", "description: fix\nargument-hint: [issue-number]\n"),
        ] {
            write(
                &root,
                &format!(".claude/skills/{name}/SKILL.md"),
                &format!("---\n{front}---\nbody\n"),
            );
        }
        let catalog = discover(&root);
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
        let by = |n: &str| catalog.skills.iter().find(|s| s.name == n).unwrap();
        assert_eq!(by("tick").description, "`git` helper");
        assert_eq!(by("at").description, "@claude does it");
        assert_eq!(by("bold").description, "*Bold* first");
        assert_eq!(
            by("hint").argument_hint.as_deref(),
            Some("[filename] [format]")
        );
        assert_eq!(by("list").argument_hint.as_deref(), Some("issue-number"));
    }

    /// The two switches, read as the host reads them.
    #[test]
    fn the_switches_are_read_the_way_the_host_reads_them() {
        let root = workspace("switches");
        for (name, front) in [
            ("yes", "disable-model-invocation: yes\n"),
            (
                "twice",
                "disable-model-invocation: false\ndisable-model-invocation: true\n",
            ),
            ("blank", "user-invocable:\n"),
            ("maybe", "user-invocable: maybe\n"),
            ("off", "user-invocable: off\n"),
            ("plain", ""),
        ] {
            write(
                &root,
                &format!(".claude/skills/{name}/SKILL.md"),
                &format!("---\ndescription: {name}\n{front}---\nbody\n"),
            );
        }
        let catalog = discover(&root);
        let by = |n: &str| catalog.skills.iter().find(|s| s.name == n).unwrap();
        // `yes` hides it from the model natively; 0.1.0 of the shared reader
        // took it for "not written" and left the skill callable.
        assert!(!by("yes").model_invocable);
        // Written twice: the last one counts, natively and here.
        assert!(!by("twice").model_invocable);
        // Once written, only a true keeps it on the `/` menu.
        assert!(!by("blank").user_invocable);
        assert!(!by("maybe").user_invocable);
        assert!(!by("off").user_invocable);
        assert!(by("plain").user_invocable && by("plain").model_invocable);
    }

    /// The author finds out, through the model, when the file reads
    /// differently natively.
    #[test]
    fn a_frontmatter_the_host_would_ignore_or_that_repeats_a_key_is_named() {
        let root = workspace("notes");
        write(
            &root,
            ".claude/skills/loose/SKILL.md",
            "---\ndescription: Review code.\n  Use when: asked.\nname: loose\nname: loose\n---\nbody\n",
        );
        let catalog = discover(&root);
        let text = render(&root, &catalog.skills[0], "", None).unwrap();
        assert!(text.contains("not valid YAML"), "{text}");
        assert!(
            text.contains("sets \"name\" more than once (lines 3, 4)"),
            "{text}"
        );
    }

    /// A skills directory may be a symlink to a shared checkout outside the
    /// workspace. `read_file` would refuse that path, so this does too:
    /// otherwise skills become the way to read outside the root.
    #[test]
    fn a_skill_that_leaves_the_workspace_through_a_symlink_is_refused() {
        let outer = workspace("escape");
        let root = outer.join("ws");
        write(
            &outer,
            "shared/secret/SKILL.md",
            "---\ndescription: outside\n---\nTOP SECRET\n",
        );
        fs::create_dir_all(root.join(".claude/skills")).unwrap();
        std::os::unix::fs::symlink(
            outer.join("shared/secret"),
            root.join(".claude/skills/secret"),
        )
        .unwrap();
        let root = fs::canonicalize(&root).unwrap();

        let catalog = discover(&root);
        assert!(catalog.skills.is_empty());
        assert_eq!(catalog.skipped.len(), 1);
        let err = load_skill(
            &root,
            &LoadSkillArgs {
                name: Some("secret".into()),
                arguments: None,
                ..Default::default()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
    }

    #[test]
    fn the_description_carries_the_catalog_and_stays_inside_what_a_host_keeps() {
        let root = workspace("describe");
        assert_eq!(
            discover(&root).description(),
            format!("{INTRO}\n\nThis workspace has no skills right now.")
        );

        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        let text = discover(&root).description();
        assert!(
            text.ends_with(
                "- deploy (arguments: <env>): Deploy the service. Use after tests pass."
            ),
            "{text}"
        );

        // Sixty skills with long descriptions do not fit. The ones that do
        // not are still named, and the text never passes the cap.
        for n in 0..60 {
            write(
                &root,
                &format!(".claude/skills/skill-{n:02}/SKILL.md"),
                &format!(
                    "---\ndescription: {}\n---\nbody\n",
                    "长描述 long description ".repeat(30)
                ),
            );
        }
        let catalog = discover(&root);
        let text = catalog.description();
        assert!(
            DESCRIPTION_CAP.fits(&text),
            "{} units",
            DESCRIPTION_CAP.measure(&text)
        );
        assert!(
            text.contains(" more, described in the full list: "),
            "{text}"
        );
        assert!(text.contains("skill-59"), "every skill is at least named");
        // The same project always produces the same description.
        assert_eq!(text, discover(&root).description());
        // The full list has room for the whole description.
        assert!(
            catalog
                .list()
                .contains(&"长描述 long description ".repeat(30).trim_end().to_string())
        );
    }

    #[test]
    fn loading_fills_in_arguments_and_lists_commands_without_running_them() {
        let root = workspace("load");
        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        write(&root, "marker-from-injection", "");
        let call = LoadSkillArgs {
            name: Some("/deploy".into()),
            arguments: Some("staging".into()),
            ..Default::default()
        };
        let text = load_skill(&root, &call, Some("sess-1")).unwrap();
        assert!(
            text.starts_with("[skill deploy -- .claude/skills/deploy/SKILL.md, "),
            "{text}"
        );
        assert!(
            text.contains("${CLAUDE_SKILL_DIR} is .claude/skills/deploy]"),
            "{text}"
        );
        assert!(
            text.contains("1 command(s) this skill wants run as it loads were NOT run"),
            "{text}"
        );
        // Line 13 of the file, where read_file will find it.
        assert!(text.contains("  line 13: git status --short\n"), "{text}");
        assert!(
            text.contains("[frontmatter with no effect here: allowed-tools]"),
            "{text}"
        );
        assert!(
            text.contains("Target: staging. Status first: !`git status --short`"),
            "{text}"
        );
        assert!(
            text.contains("Run .claude/skills/deploy/scripts/go.sh staging"),
            "{text}"
        );
        assert!(
            !text.contains("name: deploy"),
            "frontmatter is not part of the instructions"
        );
    }

    #[test]
    fn a_skill_for_people_only_is_refused_to_the_model_and_open_to_a_prompt() {
        let root = workspace("useronly");
        write(
            &root,
            ".claude/skills/release/SKILL.md",
            "---\ndescription: Cut a release.\ndisable-model-invocation: true\n---\nTag $0.\n",
        );
        write(
            &root,
            ".claude/skills/background/SKILL.md",
            "---\ndescription: Conventions.\nuser-invocable: false\n---\nAlways.\n",
        );
        let catalog = discover(&root);
        assert!(!catalog.description().contains("release"));
        assert!(catalog.description().contains("background"));
        assert!(catalog.list().contains("Only a person can start these"));

        let refused = load_skill(
            &root,
            &LoadSkillArgs {
                name: Some("release".into()),
                arguments: None,
                ..Default::default()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(refused.code(), ErrorCode::Policy);
        assert!(
            prompt_text(&root, "release", "v1.2", None)
                .unwrap()
                .contains("Tag v1.2.")
        );
        assert_eq!(
            prompt_text(&root, "background", "", None)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgs
        );
    }

    #[test]
    fn an_unknown_name_lists_what_there_is() {
        let root = workspace("unknown");
        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        let err = load_skill(
            &root,
            &LoadSkillArgs {
                name: Some("deplyo".into()),
                arguments: None,
                ..Default::default()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgs);
        assert!(
            err.message().contains("there are: deploy"),
            "{}",
            err.message()
        );
        assert!(
            load_skill(&root, &LoadSkillArgs::default(), None)
                .unwrap()
                .contains("- deploy (arguments: <env>)")
        );
    }

    #[test]
    fn a_long_body_is_cut_at_a_line_and_says_where_to_go_on() {
        let root = workspace("long");
        let body: String = (1..=4000)
            .map(|n| format!("step {n}: do the thing carefully\n"))
            .collect();
        write(
            &root,
            ".claude/skills/long/SKILL.md",
            &format!("---\ndescription: Long.\n---\n{body}"),
        );
        let text = load_skill(
            &root,
            &LoadSkillArgs {
                name: Some("long".into()),
                arguments: None,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(text.len() < MAX_BODY_BYTES + 1024);
        let note = text.lines().last().unwrap();
        assert!(
            note.starts_with("[cut after ")
                && note.contains("read_file .claude/skills/long/SKILL.md from line "),
            "{note}"
        );
        // The line it names is the first one not shown.
        let shown = text.lines().filter(|l| l.starts_with("step ")).count();
        assert!(
            note.contains(&format!("from line {} ", shown + 3 + 1)),
            "{note} / {shown}"
        );
    }

    /// A skill the model wrote a moment ago is loadable now: the scan runs
    /// per call, not once at startup.
    #[test]
    fn a_skill_written_during_the_session_can_be_loaded() {
        let root = workspace("fresh");
        assert!(
            load_skill(&root, &LoadSkillArgs::default(), None)
                .unwrap()
                .contains("no skills")
        );
        write(
            &root,
            ".claude/skills/new/SKILL.md",
            "---\ndescription: Just added.\n---\nhello\n",
        );
        assert!(
            load_skill(
                &root,
                &LoadSkillArgs {
                    name: Some("new".into()),
                    arguments: None,
                    ..Default::default()
                },
                None
            )
            .unwrap()
            .contains("hello")
        );
    }
}

//! Skills -- the project's own, and those installed for this machine's
//! account -- handed to the model by one tool.
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
//! Since P48 the same scan also takes the skills installed for this
//! machine's account ([`crate::mcp::machine_skills`]), and the Agent Node
//! runs the same code over *its* account's ([`crate::mcp::agent_skills`]).
//! An installed skill's files are outside the workspace, where `read_file`
//! does not go, so this tool reads them itself: `file`, only inside that
//! skill's own directory, by the rules in `toexec_skill::dir`.
//!
//! Nothing here executes anything. A skill's scripts are files: the model
//! runs them with `exec_command`, on this machine, as the execution account,
//! under the same write guard and sandbox as every other command. Even the
//! `` !`command` `` lines a skill may carry -- which the native client runs
//! while loading the skill -- are listed and left alone: a call that reads
//! must not be a call that runs what the repository chose, around the
//! approval `exec_command` is gated by.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rmcp::schemars;
use serde::Deserialize;
use toexec_skill::{Frontmatter, Reading, args, dir, frontmatter, inject};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::machine_skills::{Machine, Role};
use crate::mcp::path;
use crate::provider::context::Cap;

/// The tool's name. One tool, not a list/get pair: every tool costs a
/// description in every session, and "no name" is an obvious way to ask
/// what there is.
pub const TOOL: &str = "load_skill";

/// Where a project's skills live, in the order that wins a name. The first
/// two hold `<name>/SKILL.md`; `.agents/skills` is the cross-agent spelling
/// Codex looks for. Commands are single files and lose to a skill of the
/// same name, as they do natively.
const SKILL_DIRS: [&str; 2] = [".claude/skills", ".agents/skills"];
const COMMAND_DIR: &str = ".claude/commands";
/// `commands/frontend/component.md` is as deep as anybody nests them.
const COMMAND_DEPTH: usize = 3;

/// How many the catalog holds. The native client stops at the same number
/// for skills that arrive over MCP. A project's are kept first when there
/// are more.
pub const MAX_SKILLS: usize = 100;
/// A `SKILL.md` past this is skipped, not read: the guidance is "under 500
/// lines", and a file this big is a mistake or an attack on the scan. The
/// same limit applies to a skill's other files.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// What one call returns of a body or a file -- the most `read_file`
/// returns too, and well under what a Host accepts from one MCP result
/// (Claude Code: 25,000 tokens). The rest is one more call away and the text
/// says how.
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

const INTRO: &str = "Load a skill: task instructions this project keeps (.claude/skills, .claude/commands, .agents/skills) or that are installed on this machine (~/.claude/skills and the like). Before a task that a skill below describes, call this with its name and follow what comes back. A loaded skill lists its other files: read one with file, run scripts with exec_command. Without a name: the full list.";

/// The words around a catalog. The Runtime's `load_skill` and the Agent's
/// speak about different machines; the catalog underneath is the same.
#[derive(Debug, PartialEq, Eq)]
pub struct Wording {
    pub intro: &'static str,
    /// Over the project's skills.
    pub project: &'static str,
    /// Over the installed ones.
    pub installed: &'static str,
    /// When there is nothing.
    pub empty: &'static str,
    /// When not even the names fit; `{n}` is the count.
    pub count: &'static str,
}

pub const RUNTIME: Wording = Wording {
    intro: INTRO,
    project: "Skills in this workspace:",
    installed: "Installed on this machine:",
    empty: "This workspace has no skills right now.",
    count: "This workspace has {n} skills; call this without a name to list them.",
};

/// Where to look. The Runtime's server has both halves; the Agent's has
/// only the machine.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    /// The canonical workspace root.
    pub project: Option<&'a Path>,
    /// `None` when this machine shares no installed skills.
    pub machine: Option<&'a Machine>,
    pub wording: &'static Wording,
}

impl<'a> Scope<'a> {
    pub fn project(root: &'a Path) -> Scope<'a> {
        Scope {
            project: Some(root),
            machine: None,
            wording: &RUNTIME,
        }
    }

    pub fn with_machine(self, machine: Option<&'a Machine>) -> Scope<'a> {
        Scope { machine, ..self }
    }
}

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
    /// A file of the skill to read instead, as the loaded skill lists them.
    #[serde(default)]
    pub file: Option<String>,
    /// With `file`: the line to start at, from 1.
    #[serde(default)]
    pub line: Option<u64>,
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

/// Where a skill came from. Ordered: a project's are listed, described and
/// kept first; the model is working on the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Project,
    Installed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// `description` and `when_to_use`, as one line. Never empty: a file
    /// with neither is described by its first line of text.
    pub description: String,
    pub kind: Kind,
    pub origin: Origin,
    /// The `.md` file as shown: workspace-relative for a project's, an
    /// absolute path on this machine for an installed one.
    pub file: String,
    /// What `${CLAUDE_SKILL_DIR}` stands for: the directory holding the
    /// file, the same way.
    pub dir: String,
    /// The real `.md` file, symlinks resolved.
    pub file_abs: PathBuf,
    /// The real directory holding it, which `file` reads are confined to.
    pub dir_abs: PathBuf,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    /// The project's first, then installed ones; each by name. So one
    /// project on one machine always produces one description.
    pub skills: Vec<Skill>,
    pub skipped: Vec<Skipped>,
    /// There were more than [`MAX_SKILLS`].
    pub more: bool,
    wording: &'static Wording,
}

/// One file that may be a skill, before it is read.
struct Candidate {
    kind: Kind,
    origin: Origin,
    /// Workspace-relative for a project's, absolute for an installed one.
    file: String,
}

/// Scan. Cheap enough to do on every call -- a `read_dir` per directory
/// and one small file per skill -- which is what lets a skill the model has
/// just written be loaded in the same session.
pub fn discover(scope: &Scope) -> Catalog {
    let mut candidates: Vec<Candidate> = Vec::new();
    let installed = |kind, files: Vec<String>| {
        files.into_iter().map(move |file| Candidate {
            kind,
            origin: Origin::Installed,
            file,
        })
    };
    // Installed before the project's: natively a skill in the home wins a
    // name over the project's (measured, P48). Every skill before every
    // command, as natively.
    if let Some(machine) = scope.machine {
        candidates.extend(installed(Kind::Skill, machine.skill_candidates()));
    }
    if let Some(root) = scope.project {
        for base in SKILL_DIRS {
            for name in children(&root.join(base)) {
                candidates.push(Candidate {
                    kind: Kind::Skill,
                    origin: Origin::Project,
                    file: format!("{base}/{name}/SKILL.md"),
                });
            }
        }
    }
    if let Some(machine) = scope.machine {
        candidates.extend(installed(
            Kind::Command,
            machine.command_candidates(COMMAND_DEPTH),
        ));
    }
    if let Some(root) = scope.project {
        commands(root, COMMAND_DIR, 0, &mut candidates);
    }

    let mut skills: Vec<Skill> = Vec::new();
    let mut skipped = Vec::new();
    let mut taken = BTreeSet::new();
    for candidate in candidates {
        let skill = match read(scope, &candidate) {
            Ok(Some(skill)) => skill,
            // A directory under skills/ with no SKILL.md in it is not a
            // broken skill, it is not a skill.
            Ok(None) => continue,
            Err(reason) => {
                skipped.push(Skipped {
                    file: candidate.file,
                    reason,
                });
                continue;
            }
        };
        if skill.origin == Origin::Installed {
            // Hidden is as if not installed: a project's skill of the same
            // name is then the one offered.
            if scope.machine.is_some_and(|m| m.is_hidden(&skill.name)) {
                continue;
            }
            // `~/.claude/skills/x -> ~/.agents/skills/x` is one skill found
            // twice, which is how the `skills` CLI installs every one; a
            // byte-for-byte copy in a second directory is the same skill too
            // (30 of them on the machine P48 was measured on). Neither is
            // worth a line in "Not offered".
            if skills.iter().any(|s| {
                s.file_abs == skill.file_abs
                    || (s.origin == Origin::Installed
                        && s.name == skill.name
                        && same_bytes(&s.file_abs, &skill.file_abs))
            }) {
                continue;
            }
        }
        if !taken.insert(skill.name.clone()) {
            let winner = skills
                .iter()
                .find(|s| s.name == skill.name)
                .map_or("another file", |s| s.file.as_str());
            skipped.push(Skipped {
                file: candidate.file,
                reason: format!("the name \"{}\" is already taken by {winner}", skill.name),
            });
            continue;
        }
        skills.push(skill);
    }
    skills.sort_by(|a, b| (a.origin, &a.name).cmp(&(b.origin, &b.name)));
    let more = skills.len() > MAX_SKILLS;
    skills.truncate(MAX_SKILLS);
    Catalog {
        skills,
        skipped,
        more,
        wording: scope.wording,
    }
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    matches!((std::fs::read(a), std::fs::read(b)), (Ok(a), Ok(b)) if a == b)
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

fn commands(root: &Path, rel: &str, depth: usize, out: &mut Vec<Candidate>) {
    if depth >= COMMAND_DEPTH {
        return;
    }
    for name in children(&root.join(rel)) {
        let child = format!("{rel}/{name}");
        if root.join(&child).is_dir() {
            commands(root, &child, depth + 1, out);
        } else if name.ends_with(".md") {
            out.push(Candidate {
                kind: Kind::Command,
                origin: Origin::Project,
                file: child,
            });
        }
    }
}

/// Where a candidate really is: `(shown file, real file)`. `Ok(None)`:
/// nothing there.
fn locate(
    scope: &Scope,
    candidate: &Candidate,
) -> std::result::Result<Option<(String, PathBuf)>, String> {
    match candidate.origin {
        Origin::Project => {
            let Some(root) = scope.project else {
                return Ok(None);
            };
            // Through the same policy as `read_file`: a skills directory
            // that is a symlink out of the workspace is refused here exactly
            // as it would be there, rather than becoming the one way to read
            // outside the root.
            match path::resolve_read(root, &candidate.file) {
                Ok(resolved) => Ok(Some((
                    resolved.rel().to_string(),
                    resolved.abs().to_path_buf(),
                ))),
                Err(e) if e.code() == ErrorCode::InvalidArgs => Ok(None),
                Err(e) => Err(e.message().to_string()),
            }
        }
        Origin::Installed => {
            let Some(machine) = scope.machine else {
                return Ok(None);
            };
            Ok(machine
                .resolve(&candidate.file)?
                .map(|real| (candidate.file.clone(), real)))
        }
    }
}

/// One candidate file. `Ok(None)`: nothing there. `Err`: something there
/// that cannot be offered, with the reason a person will read.
fn read(scope: &Scope, candidate: &Candidate) -> std::result::Result<Option<Skill>, String> {
    let Some((shown, real)) = locate(scope, candidate)? else {
        return Ok(None);
    };
    let meta = std::fs::metadata(&real).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} bytes; a skill file over {MAX_FILE_BYTES} is not read",
            meta.len()
        ));
    }
    let raw = std::fs::read_to_string(&real).map_err(|e| e.to_string())?;
    let (front, body) = frontmatter::split(&raw);
    let front = match front {
        Some(text) => frontmatter::parse(text).map_err(|e| e.to_string())?,
        None => Frontmatter::default(),
    };

    let file = &candidate.file;
    let (dir, stem) = match file.rsplit_once('/') {
        Some((dir, leaf)) => (dir, leaf.trim_end_matches(".md")),
        None => ("", file.as_str()),
    };
    let fallback = match candidate.kind {
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
    let dir_abs = real.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(Some(Skill {
        name: name.to_string(),
        description,
        kind: candidate.kind,
        origin: candidate.origin,
        file: shown,
        dir: dir.to_string(),
        file_abs: real,
        dir_abs,
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

    fn heading(&self, origin: Origin) -> &'static str {
        match origin {
            Origin::Project => self.wording.project,
            Origin::Installed => self.wording.installed,
        }
    }

    /// The tool's description: what the model has in front of it for the
    /// whole session. As many skills as fit in [`DESCRIPTION_CAP`], the
    /// project's first, and when some do not, their names -- a name is
    /// enough to ask for one.
    pub fn description(&self) -> String {
        let intro = self.wording.intro;
        let offered: Vec<&Skill> = self.offered().collect();
        if offered.is_empty() {
            return format!("{intro}\n\n{}", self.wording.empty);
        }
        let render = |shown: usize| {
            let mut text = format!("{intro}\n");
            let mut group = None;
            for skill in &offered[..shown] {
                if group != Some(skill.origin) {
                    group = Some(skill.origin);
                    text.push_str(&format!("\n{}\n", self.heading(skill.origin)));
                }
                text.push_str(&entry(skill, CATALOG_DESCRIPTION_CHARS));
                text.push('\n');
            }
            if shown < offered.len() {
                let rest: Vec<&str> = offered[shown..].iter().map(|s| s.name.as_str()).collect();
                if shown == 0 {
                    text.push('\n');
                }
                text.push_str(&format!(
                    "{} more, described in the full list: {}",
                    rest.len(),
                    rest.join(", ")
                ));
            }
            text.trim_end().to_string()
        };
        if let Some(text) = (0..=offered.len())
            .rev()
            .map(render)
            .find(|text| DESCRIPTION_CAP.fits(text))
        {
            return text;
        }
        // Not even every name fits -- 97 installed skills on one real machine
        // did not. A count alone gives the model no reason to look, so as
        // many names as fit, the project's first, and the count.
        let count = self
            .wording
            .count
            .replace("{n}", &offered.len().to_string());
        let named = |shown: usize| {
            let names: Vec<&str> = offered[..shown].iter().map(|s| s.name.as_str()).collect();
            format!("{intro}\n\n{count} Among them: {}, …", names.join(", "))
        };
        (1..offered.len())
            .rev()
            .map(named)
            .find(|text| DESCRIPTION_CAP.fits(text))
            .unwrap_or_else(|| format!("{intro}\n\n{count}"))
    }

    /// What a call without a name returns.
    pub fn list(&self) -> String {
        let mut out = String::new();
        let offered: Vec<&Skill> = self.offered().collect();
        if offered.is_empty() {
            out.push_str("There are no skills you can load.\n");
        } else {
            out.push_str(&format!(
                "{} skill(s). Load one with {TOOL} and its name.\n",
                offered.len()
            ));
            let mut group = None;
            for skill in &offered {
                if group != Some(skill.origin) {
                    group = Some(skill.origin);
                    out.push_str(&format!("\n{}\n", self.heading(skill.origin)));
                }
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

/// The tool: the list, one skill's instructions, or one of its files.
pub fn load_skill(scope: &Scope, call: &LoadSkillArgs, session: Option<&str>) -> Result<String> {
    let catalog = discover(scope);
    let Some(name) = call
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        if call.file.is_some() || call.line.is_some() {
            return Err(Error::invalid_args(
                "file and line read a file of one skill: give its name too",
            ));
        }
        return Ok(catalog.list());
    };
    let skill = find(&catalog, name)?;
    if !skill.model_invocable {
        return Err(Error::policy(format!(
            "skill \"{}\" is marked disable-model-invocation: only a person can start it",
            skill.name
        )));
    }
    match call
        .file
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(file) => read_file(scope, skill, file, call.line.unwrap_or(1)),
        None if call.line.is_some() => Err(Error::invalid_args(
            "line says where to start reading a file: give file too (SKILL.md for the instructions themselves)",
        )),
        None => render(
            scope,
            skill,
            call.arguments.as_deref().unwrap_or(""),
            session,
        ),
    }
}

/// The same text, for a person who started the skill through a prompt.
/// `disable-model-invocation` does not apply: this *is* the person.
pub fn prompt_text(
    scope: &Scope,
    name: &str,
    arguments: &str,
    session: Option<&str>,
) -> Result<String> {
    let catalog = discover(scope);
    let skill = find(&catalog, name)?;
    if !skill.user_invocable {
        return Err(Error::invalid_args(format!(
            "skill \"{}\" is marked user-invocable: false",
            skill.name
        )));
    }
    render(scope, skill, arguments, session)
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
                format!("no skill named \"{wanted}\": there are no skills")
            } else {
                format!(
                    "no skill named \"{wanted}\"; there are: {}",
                    known.join(", ")
                )
            })
        })
}

/// What the model is told about where an installed skill's files are, on
/// top of the path. On the Agent the difference matters: nothing it names
/// is next to the project.
fn installed_note(scope: &Scope) -> &'static str {
    match scope.machine.map(|m| m.role) {
        Some(Role::Agent) => {
            "[installed on the machine you run on, not on the project machine: its files are not in the workspace. Read one with this tool's file argument; to run a script against the project, write it into the workspace with apply_patch and run it there]\n"
        }
        _ => {
            "[installed on this machine, outside the workspace: read its files with this tool's file argument; exec_command can run its scripts by the path above]\n"
        }
    }
}

fn render(scope: &Scope, skill: &Skill, arguments: &str, session: Option<&str>) -> Result<String> {
    let raw = match (skill.origin, scope.project) {
        (Origin::Project, Some(root)) => {
            let resolved = path::resolve_read(root, &skill.file)?;
            std::fs::read_to_string(resolved.abs())
        }
        _ => std::fs::read_to_string(&skill.file_abs),
    }
    .map_err(|e| Error::invalid_args(format!("cannot read {}", skill.file)).with_source(e))?;
    let (front_text, body) = frontmatter::split(&raw);
    let front = front_text
        .and_then(|text| frontmatter::parse(text).ok())
        .unwrap_or_default();
    // Line numbers the model can hand to `read_file` or `file`: lines of
    // the file, not of the body.
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
    if skill.origin == Origin::Installed {
        head.push_str(installed_note(scope));
    }
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
    if skill.kind == Kind::Skill {
        let listing = dir::list(&skill.dir_abs);
        if !listing.files.is_empty() {
            head.push_str(&format!(
                "[other files in this skill's directory: {}{}; read one with this tool's file argument]\n",
                listing.files.join(", "),
                if listing.more { ", and more" } else { "" }
            ));
        }
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
        let next = body_starts + shown + 1;
        out.push_str(&match skill.origin {
            Origin::Project => format!(
                "\n[cut after {shown} lines of the skill's text; read_file {} from line {next} for the rest]\n",
                skill.file
            ),
            Origin::Installed => format!(
                "\n[cut after {shown} lines of the skill's text; {TOOL} name={} file=SKILL.md line={next} for the rest]\n",
                skill.name
            ),
        });
    }
    Ok(out)
}

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

/// One of a skill's files, from line `line` on, at most [`MAX_BODY_BYTES`]
/// of it.
fn read_file(scope: &Scope, skill: &Skill, file: &str, line: u64) -> Result<String> {
    if skill.kind == Kind::Command {
        return Err(Error::invalid_args(format!(
            "\"{}\" is a command: one file, {}, with no directory of files of its own",
            skill.name, skill.file
        )));
    }
    if line == 0 {
        return Err(Error::invalid_args("line counts from 1"));
    }
    // A project's skill is workspace files: the workspace's read policy
    // applies first, exactly as it would to `read_file`.
    if let (Origin::Project, Some(root)) = (skill.origin, scope.project) {
        path::resolve_read(root, &format!("{}/{file}", skill.dir))?;
    }
    let refused = |e: dir::ReadError| {
        let what = format!("{file} {e}");
        match e {
            dir::ReadError::NotRelative | dir::ReadError::Outside | dir::ReadError::Hidden => {
                Error::policy(format!(
                    "{what}: only files inside the skill's own directory, and never ones whose name starts with a dot (they often hold a script's secrets), are read"
                ))
            }
            dir::ReadError::NotText => Error::invalid_args(match scope.machine.map(|m| m.role) {
                Some(Role::Agent) if skill.origin == Origin::Installed => format!(
                    "{what}: it is on the machine you run on and can only be passed on as text"
                ),
                _ => format!(
                    "{what}: use it where it is with exec_command: {}",
                    skill.dir_abs.join(file).display()
                ),
            }),
            dir::ReadError::NotFound | dir::ReadError::NotAFile => Error::invalid_args(format!(
                "{what}; loading the skill without file lists what is there"
            )),
            _ => Error::invalid_args(what),
        }
    };
    let real = dir::resolve(&skill.dir_abs, file).map_err(refused)?;
    let text = dir::read_text(&real, MAX_FILE_BYTES).map_err(refused)?;

    let total = text.lines().count() as u64;
    let start = usize::try_from(line - 1).unwrap_or(usize::MAX);
    if line > total.max(1) {
        return Err(Error::invalid_args(format!(
            "{file} has {total} line(s); line {line} is past its end"
        )));
    }
    let offset: usize = text.split_inclusive('\n').take(start).map(str::len).sum();
    let rest = &text[offset..];
    let mut kept = Cap::Bytes(MAX_BODY_BYTES).keep(rest);
    // One line longer than the whole budget: its start is all there is
    // room for, and the next part starts at the line after it.
    let mut long_line = false;
    if kept.len() < rest.len() && !kept.ends_with('\n') {
        long_line = true;
        kept = crate::mcp::truncate_bytes(rest, MAX_BODY_BYTES);
    }
    let lines = (kept.lines().count() as u64).max(1);
    let last = line + lines - 1;
    let mut out = format!(
        "[skill {} -- {file} in {}, {} bytes, {total} line(s); lines {line}-{last}]\n{kept}",
        skill.name,
        skill.dir,
        text.len()
    );
    if kept.len() < rest.len() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if long_line {
            out.push_str(&format!(
                "[line {line} is longer than {MAX_BODY_BYTES} bytes and was cut; "
            ));
        } else {
            out.push_str(&format!("[cut after line {last}; "));
        }
        out.push_str(&format!(
            "{TOOL} name={} file={file} line={} for the rest]\n",
            skill.name,
            last + 1
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

        let catalog = discover(&Scope::project(&root));
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
        let catalog = discover(&Scope::project(&root));
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
        let catalog = discover(&Scope::project(&root));
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
        let catalog = discover(&Scope::project(&root));
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
        let catalog = discover(&Scope::project(&root));
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
        let catalog = discover(&Scope::project(&root));
        let text = render(&Scope::project(&root), &catalog.skills[0], "", None).unwrap();
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

        let catalog = discover(&Scope::project(&root));
        assert!(catalog.skills.is_empty());
        assert_eq!(catalog.skipped.len(), 1);
        let err = load_skill(
            &Scope::project(&root),
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
            discover(&Scope::project(&root)).description(),
            format!("{INTRO}\n\nThis workspace has no skills right now.")
        );

        write(&root, ".claude/skills/deploy/SKILL.md", DEPLOY);
        let text = discover(&Scope::project(&root)).description();
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
        let catalog = discover(&Scope::project(&root));
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
        assert_eq!(text, discover(&Scope::project(&root)).description());
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
        let text = load_skill(&Scope::project(&root), &call, Some("sess-1")).unwrap();
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
        let catalog = discover(&Scope::project(&root));
        assert!(!catalog.description().contains("release"));
        assert!(catalog.description().contains("background"));
        assert!(catalog.list().contains("Only a person can start these"));

        let refused = load_skill(
            &Scope::project(&root),
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
            prompt_text(&Scope::project(&root), "release", "v1.2", None)
                .unwrap()
                .contains("Tag v1.2.")
        );
        assert_eq!(
            prompt_text(&Scope::project(&root), "background", "", None)
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
            &Scope::project(&root),
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
            load_skill(&Scope::project(&root), &LoadSkillArgs::default(), None)
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
            &Scope::project(&root),
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
            load_skill(&Scope::project(&root), &LoadSkillArgs::default(), None)
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
                &Scope::project(&root),
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

    // -- installed skills (P48) --

    use crate::config::MachineSkills;
    use crate::mcp::machine_skills::{Machine, Role};

    fn machine(home: &Path, role: Role, hidden: &[&str]) -> Machine {
        let config = MachineSkills {
            enabled: true,
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
        };
        Machine::new(&config, home, role).unwrap()
    }

    fn skill(description: &str) -> String {
        format!(
            "---\ndescription: {description}\n---\nBody of {description}. See ${{CLAUDE_SKILL_DIR}}/reference.md\n"
        )
    }

    /// A project and a home side by side, the way the Runtime sees them.
    fn both(name: &str) -> (TestDir, PathBuf, PathBuf) {
        let base = workspace(name);
        let root = base.join("ws");
        let home = base.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        (base, root, home)
    }

    #[test]
    fn installed_skills_follow_the_projects_and_win_a_name_as_they_do_natively() {
        let (_base, root, home) = both("installed");
        write(
            &root,
            ".claude/skills/deploy/SKILL.md",
            &skill("Project deploy"),
        );
        write(
            &root,
            ".claude/skills/lint/SKILL.md",
            &skill("Project lint"),
        );
        write(
            &home,
            ".claude/skills/deploy/SKILL.md",
            &skill("Personal deploy"),
        );
        write(&home, ".agents/skills/pdf/SKILL.md", &skill("Fill PDFs"));
        // How the `skills` CLI installs: the same skill again, through a link.
        fs::create_dir_all(home.join(".claude/skills")).unwrap();
        std::os::unix::fs::symlink(
            home.join(".agents/skills/pdf"),
            home.join(".claude/skills/pdf"),
        )
        .unwrap();
        // Codex's own bundle is not the user's.
        write(
            &home,
            ".codex/skills/.system/imagegen/SKILL.md",
            &skill("Images"),
        );
        write(&home, ".claude/commands/fix.md", "Fix $ARGUMENTS\n");
        let m = machine(&home, Role::Runtime, &[]);
        let catalog = discover(&Scope::project(&root).with_machine(Some(&m)));

        let found: Vec<(&str, Origin)> = catalog
            .skills
            .iter()
            .map(|s| (s.name.as_str(), s.origin))
            .collect();
        assert_eq!(
            found,
            [
                ("lint", Origin::Project),
                ("deploy", Origin::Installed),
                ("fix", Origin::Installed),
                ("pdf", Origin::Installed),
            ]
        );
        // The project's deploy lost, and says to whom. The linked copy of
        // pdf is one skill found twice, not a loser worth a line.
        assert_eq!(catalog.skipped.len(), 1, "{:?}", catalog.skipped);
        assert_eq!(catalog.skipped[0].file, ".claude/skills/deploy/SKILL.md");
        assert!(
            catalog.skipped[0].reason.contains(
                &home
                    .join(".claude/skills/deploy/SKILL.md")
                    .display()
                    .to_string()
            ),
            "{:?}",
            catalog.skipped
        );
        let text = catalog.description();
        let project = text.find("Skills in this workspace:\n- lint").expect(&text);
        let installed = text
            .find("Installed on this machine:\n- deploy")
            .expect(&text);
        assert!(project < installed, "{text}");
    }

    #[test]
    fn a_hidden_installed_skill_is_as_if_not_installed() {
        let (_base, root, home) = both("hidden");
        write(
            &root,
            ".claude/skills/deploy/SKILL.md",
            &skill("Project deploy"),
        );
        write(
            &home,
            ".claude/skills/deploy/SKILL.md",
            &skill("Personal deploy"),
        );
        write(&home, ".claude/skills/noise/SKILL.md", &skill("Noise"));
        let m = machine(&home, Role::Runtime, &["deploy", "noise"]);
        let catalog = discover(&Scope::project(&root).with_machine(Some(&m)));
        assert_eq!(names(&catalog), ["deploy"]);
        assert_eq!(catalog.skills[0].origin, Origin::Project);
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
        assert!(!catalog.list().contains("noise"));
    }

    fn call(name: &str, file: Option<&str>, line: Option<u64>) -> LoadSkillArgs {
        LoadSkillArgs {
            name: Some(name.into()),
            file: file.map(str::to_string),
            line,
            ..Default::default()
        }
    }

    #[test]
    fn an_installed_skills_own_files_are_read_and_nothing_around_them() {
        let (_base, root, home) = both("files");
        let dir = home.join(".claude/skills/pdf");
        write(&dir, "SKILL.md", &skill("Fill PDFs"));
        write(&dir, "reference.md", "# Fields\n");
        write(&dir, "scripts/fill.py", "print('fill')\n");
        write(&dir, ".env", "TOKEN=x");
        fs::write(dir.join("blank.pdf"), [0xff, 0xfe, 0x00]).unwrap();
        write(&home, "notes.txt", "not this skill's");
        let m = machine(&home, Role::Runtime, &[]);
        let scope = Scope::project(&root).with_machine(Some(&m));

        let text = load_skill(&scope, &call("pdf", None, None), None).unwrap();
        let shown_dir = dir.display().to_string();
        assert!(
            text.contains(&format!("${{CLAUDE_SKILL_DIR}} is {shown_dir}]")),
            "{text}"
        );
        assert!(
            text.contains("[installed on this machine, outside the workspace"),
            "{text}"
        );
        assert!(
            text.contains("[other files in this skill's directory: blank.pdf, reference.md, scripts/fill.py; read one with this tool's file argument]"),
            "{text}"
        );
        // The placeholder is the absolute path: exec_command runs from the
        // workspace, where a relative one would point nowhere.
        assert!(
            text.contains(&format!("See {shown_dir}/reference.md")),
            "{text}"
        );

        let file = load_skill(&scope, &call("pdf", Some("scripts/fill.py"), None), None).unwrap();
        assert!(
            file.starts_with(&format!("[skill pdf -- scripts/fill.py in {shown_dir}, 14 bytes, 1 line(s); lines 1-1]\nprint('fill')\n")),
            "{file}"
        );

        let refused = |f: &str| load_skill(&scope, &call("pdf", Some(f), None), None).unwrap_err();
        assert_eq!(refused(".env").code(), ErrorCode::Policy);
        assert_eq!(refused("../../../notes.txt").code(), ErrorCode::Policy);
        assert_eq!(
            refused(&home.join("notes.txt").display().to_string()).code(),
            ErrorCode::Policy
        );
        assert_eq!(refused("missing.md").code(), ErrorCode::InvalidArgs);
        let binary = refused("blank.pdf");
        assert_eq!(binary.code(), ErrorCode::InvalidArgs);
        assert!(
            binary
                .message()
                .contains(&format!("exec_command: {shown_dir}/blank.pdf")),
            "{}",
            binary.message()
        );
    }

    #[test]
    fn a_long_file_comes_back_in_parts_that_join_up() {
        let (_base, root, home) = both("parts");
        let dir = home.join(".agents/skills/big");
        write(&dir, "SKILL.md", &skill("Big"));
        let whole: String = (1..=5000)
            .map(|n| format!("row {n}: some reference text\n"))
            .collect();
        write(&dir, "table.md", &whole);
        let m = machine(&home, Role::Runtime, &[]);
        let scope = Scope::project(&root).with_machine(Some(&m));

        let mut joined = String::new();
        let mut line = 1;
        for _ in 0..10 {
            let part =
                load_skill(&scope, &call("big", Some("table.md"), Some(line)), None).unwrap();
            assert!(part.len() < MAX_BODY_BYTES + 512);
            let (head, rest) = part.split_once('\n').unwrap();
            assert!(head.contains(&format!("lines {line}-")), "{head}");
            match rest.rsplit_once("[cut after line ") {
                Some((text, note)) => {
                    joined.push_str(text);
                    let next = note.split("line=").nth(1).unwrap();
                    line = next.split(' ').next().unwrap().parse().unwrap();
                }
                None => {
                    joined.push_str(rest);
                    break;
                }
            }
        }
        assert_eq!(joined, whole);

        let err = |c: LoadSkillArgs| load_skill(&scope, &c, None).unwrap_err().code();
        assert_eq!(
            err(call("big", Some("table.md"), Some(5001))),
            ErrorCode::InvalidArgs
        );
        assert_eq!(
            err(call("big", Some("table.md"), Some(0))),
            ErrorCode::InvalidArgs
        );
        assert_eq!(err(call("big", None, Some(3))), ErrorCode::InvalidArgs);
        let nameless = LoadSkillArgs {
            file: Some("table.md".into()),
            ..Default::default()
        };
        assert_eq!(err(nameless), ErrorCode::InvalidArgs);
    }

    #[test]
    fn an_installed_skill_cut_short_says_to_go_on_with_its_file() {
        let (_base, root, home) = both("cutinstalled");
        let body: String = (1..=4000)
            .map(|n| format!("step {n}: do the thing carefully\n"))
            .collect();
        write(
            &home,
            ".claude/skills/long/SKILL.md",
            &format!("---\ndescription: Long.\n---\n{body}"),
        );
        let m = machine(&home, Role::Runtime, &[]);
        let scope = Scope::project(&root).with_machine(Some(&m));
        let text = load_skill(&scope, &call("long", None, None), None).unwrap();
        let note = text.lines().last().unwrap();
        assert!(
            note.contains("load_skill name=long file=SKILL.md line="),
            "{note}"
        );
        // That line is the first one the text did not show.
        let shown = text.lines().filter(|l| l.starts_with("step ")).count();
        let next = format!("line={}", shown + 3 + 1);
        assert!(note.contains(&next), "{note}");
    }

    #[test]
    fn a_command_is_one_file_with_nothing_else_to_read() {
        let (_base, root, home) = both("command");
        write(
            &home,
            ".claude/commands/fix.md",
            "---\ndescription: Fix.\n---\nFix it\n",
        );
        write(
            &home,
            ".claude/commands/other.md",
            "---\ndescription: Other.\n---\nx\n",
        );
        let m = machine(&home, Role::Runtime, &[]);
        let scope = Scope::project(&root).with_machine(Some(&m));
        let text = load_skill(&scope, &call("fix", None, None), None).unwrap();
        assert!(!text.contains("other files"), "{text}");
        let err = load_skill(&scope, &call("fix", Some("other.md"), None), None).unwrap_err();
        assert!(err.message().contains("is a command"), "{}", err.message());
    }

    #[test]
    fn on_the_agent_the_words_say_the_files_are_not_next_to_the_project() {
        let base = workspace("agentwords");
        let home = base.join("home");
        let dir = home.join(".claude/skills/pdf");
        write(&dir, "SKILL.md", &skill("Fill PDFs"));
        fs::write(dir.join("blank.pdf"), [0xff, 0xfe, 0x00]).unwrap();
        let m = machine(&home, Role::Agent, &[]);
        let scope = Scope {
            project: None,
            machine: Some(&m),
            wording: &RUNTIME,
        };
        let text = load_skill(&scope, &call("pdf", None, None), None).unwrap();
        assert!(text.contains("not on the project machine"), "{text}");
        let binary = load_skill(&scope, &call("pdf", Some("blank.pdf"), None), None).unwrap_err();
        assert!(
            binary.message().contains("can only be passed on as text"),
            "{}",
            binary.message()
        );
    }

    #[test]
    fn past_the_limit_it_is_installed_skills_that_are_left_out() {
        let (_base, root, home) = both("limit");
        for n in 0..3 {
            write(
                &root,
                &format!(".claude/skills/proj-{n}/SKILL.md"),
                &skill("Project"),
            );
        }
        for n in 0..MAX_SKILLS {
            write(
                &home,
                &format!(".agents/skills/mine-{n:03}/SKILL.md"),
                &skill("Mine"),
            );
        }
        let m = machine(&home, Role::Runtime, &[]);
        let catalog = discover(&Scope::project(&root).with_machine(Some(&m)));
        assert_eq!(catalog.skills.len(), MAX_SKILLS);
        assert!(catalog.more);
        assert_eq!(names(&catalog)[..3], ["proj-0", "proj-1", "proj-2"]);
        let text = catalog.description();
        assert!(
            DESCRIPTION_CAP.fits(&text),
            "{}",
            DESCRIPTION_CAP.measure(&text)
        );
        // The project's are described; the rest may be only named, or only
        // counted -- but never at the project's expense.
        assert!(text.contains("- proj-0: Project"), "{text}");
    }

    /// The same skill copied into two installed directories is one skill;
    /// two different ones under one name are still a loser worth naming.
    #[test]
    fn an_identical_copy_is_one_skill_and_a_different_one_is_named() {
        let (_base, root, home) = both("copies");
        write(
            &home,
            ".claude/skills/cloudflare/SKILL.md",
            &skill("Deploy to Cloudflare"),
        );
        write(
            &home,
            ".codex/skills/cloudflare/SKILL.md",
            &skill("Deploy to Cloudflare"),
        );
        write(
            &home,
            ".claude/skills/lint/SKILL.md",
            &skill("Lint, Claude's"),
        );
        write(
            &home,
            ".codex/skills/lint/SKILL.md",
            &skill("Lint, Codex's"),
        );
        let m = machine(&home, Role::Runtime, &[]);
        let catalog = discover(&Scope::project(&root).with_machine(Some(&m)));
        assert_eq!(names(&catalog), ["cloudflare", "lint"]);
        assert_eq!(catalog.skipped.len(), 1, "{:?}", catalog.skipped);
        assert!(
            catalog.skipped[0]
                .file
                .ends_with("/.codex/skills/lint/SKILL.md")
        );
    }

    /// When not even every name fits, as many as fit are named -- a count
    /// alone gives the model no reason to look. Measured: 97 installed
    /// skills on one real machine came out as a bare count before this.
    #[test]
    fn past_every_name_fitting_the_ones_that_fit_are_still_named() {
        let (_base, root, home) = both("names");
        write(
            &root,
            ".claude/skills/project-first/SKILL.md",
            &skill("Project"),
        );
        for n in 0..97 {
            let name = format!("an-installed-skill-with-a-long-name-{n:02}");
            write(
                &home,
                &format!(".agents/skills/{name}/SKILL.md"),
                &skill("Installed"),
            );
        }
        let m = machine(&home, Role::Runtime, &[]);
        let text = discover(&Scope::project(&root).with_machine(Some(&m))).description();
        assert!(
            DESCRIPTION_CAP.fits(&text),
            "{}",
            DESCRIPTION_CAP.measure(&text)
        );
        let tail = text
            .split(
                "This workspace has 98 skills; call this without a name to list them. Among them: ",
            )
            .nth(1)
            .expect(&text);
        assert!(
            tail.starts_with("project-first, an-installed-skill-with-a-long-name-00, "),
            "{tail}"
        );
        assert!(tail.ends_with(", …"), "{tail}");
        assert!(tail.split(", ").count() > 20, "{tail}");
    }
}

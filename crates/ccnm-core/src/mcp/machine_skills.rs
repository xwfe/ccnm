//! Skills installed for this machine's account, as opposed to the ones a
//! project carries (P48).
//!
//! Where they are is the natives' own convention: `~/.claude/skills`
//! (Claude Code), `~/.agents/skills` (Codex, and the `skills` CLI's store --
//! it symlinks each one into `~/.claude/skills` too), `~/.codex/skills`
//! (Codex's older home), and `~/.claude/commands`. Nothing here decides
//! what a skill *is*; `skills` reads these the way it reads a project's.
//!
//! Two servers use this, one per machine. On the Runtime Node the MCP server
//! adds the executor account's installed skills to `load_skill`, next to the
//! project's. On the Agent Node a small server of its own
//! ([`crate::mcp::agent_skills`]) offers the Agent account's, because the
//! Runtime cannot read the Agent's disk and the natives, as ccnm starts
//! them, cannot either (toexec `evidence/v3-parity/machine-skills/`).
//!
//! Deleting the feature is deleting this file, `agent_skills.rs`, the
//! `machine` half of [`crate::mcp::skills::Scope`] and the config section.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::MachineSkills;

/// In the order that wins a name, before any project directory: natively
/// an installed skill beats a project's of the same name (measured on Claude
/// Code 2.1.278, toexec `evidence/v3-parity/machine-skills/`).
const SKILL_DIRS: [&str; 3] = [".claude/skills", ".agents/skills", ".codex/skills"];
const COMMAND_DIR: &str = ".claude/commands";
/// A skills directory is a list somebody curates; past this many entries it
/// is something else, and scanning it is a cost every call pays.
const MAX_ENTRIES_PER_DIR: usize = 500;

/// Which machine this is, for the words the model reads: a script of an
/// installed skill on the Runtime runs right where the project is; one on
/// the Agent does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Runtime,
    Agent,
}

#[derive(Debug, Clone)]
pub struct Machine {
    /// Canonical.
    home: PathBuf,
    hidden: BTreeSet<String>,
    pub role: Role,
}

impl Machine {
    /// `None` when this machine shares none, or has no home to look in.
    pub fn new(config: &MachineSkills, home: &Path, role: Role) -> Option<Machine> {
        if !config.enabled {
            return None;
        }
        Some(Machine {
            home: home.canonicalize().ok()?,
            hidden: config.hidden.clone(),
            role,
        })
    }

    pub fn is_hidden(&self, name: &str) -> bool {
        self.hidden.contains(name)
    }

    /// `<home>/<dir>/<name>/SKILL.md` for every entry of every skills
    /// directory, as absolute paths, skills directories in precedence order.
    pub fn skill_candidates(&self) -> Vec<String> {
        SKILL_DIRS
            .iter()
            .flat_map(|dir| {
                let base = self.home.join(dir);
                entries(&base)
                    .into_iter()
                    .map(move |name| base.join(name).join("SKILL.md"))
            })
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// Every `.md` under `~/.claude/commands`, as deep as a project's are
    /// looked for.
    pub fn command_candidates(&self, depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        commands(&self.home.join(COMMAND_DIR), depth, &mut out);
        out.into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// The real file behind a candidate. `Ok(None)`: nothing there.
    ///
    /// Symlinks are followed -- `~/.claude/skills/x -> ~/.agents/skills/x`
    /// is how the `skills` CLI installs every one -- but a skill whose
    /// directory turns out to be the filesystem root or this home itself is
    /// refused: reading "a file of this skill" would then mean reading the
    /// whole home.
    pub fn resolve(&self, candidate: &str) -> Result<Option<PathBuf>, String> {
        let real = match Path::new(candidate).canonicalize() {
            Ok(real) => real,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let dir = real.parent().unwrap_or(Path::new("/"));
        if dir.parent().is_none() || self.home.starts_with(dir) {
            return Err(format!(
                "its directory resolves to {}, which would open more than the skill; not offered",
                dir.display()
            ));
        }
        Ok(Some(real))
    }
}

/// Entry names of a directory, sorted, dotfiles left out (`.system` is
/// Codex's own bundle and `.DS_Store` is Finder's), at most
/// [`MAX_ENTRIES_PER_DIR`].
fn entries(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names.truncate(MAX_ENTRIES_PER_DIR);
    names
}

fn commands(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    for name in entries(dir) {
        let path = dir.join(&name);
        if path.is_dir() {
            commands(&path, depth - 1, out);
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccnm_testdir::TestDir;
    use std::fs;

    fn home(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-machine-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    #[test]
    fn off_means_none_and_on_lists_every_directory_in_order() {
        let h = home("order");
        write(&h, ".agents/skills/b/SKILL.md");
        write(&h, ".claude/skills/a/SKILL.md");
        write(&h, ".codex/skills/c/SKILL.md");
        write(&h, ".codex/skills/.system/imagegen/SKILL.md");
        write(&h, ".claude/commands/fix.md");
        write(&h, ".claude/commands/team/review.md");
        let off = MachineSkills {
            enabled: false,
            ..Default::default()
        };
        assert!(Machine::new(&off, &h, Role::Runtime).is_none());

        let m = Machine::new(&MachineSkills::default(), &h, Role::Runtime).unwrap();
        let shown: Vec<String> = m
            .skill_candidates()
            .iter()
            .map(|c| c.trim_start_matches(&*h.to_string_lossy()).to_string())
            .collect();
        assert_eq!(
            shown,
            [
                "/.claude/skills/a/SKILL.md",
                "/.agents/skills/b/SKILL.md",
                "/.codex/skills/c/SKILL.md"
            ]
        );
        let commands = m.command_candidates(3);
        assert_eq!(commands.len(), 2);
        assert!(commands[0].ends_with("/.claude/commands/fix.md"));
        assert!(commands[1].ends_with("/.claude/commands/team/review.md"));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_skill_is_followed_and_one_that_opens_the_home_is_refused() {
        let h = home("links");
        write(&h, ".agents/skills/real/SKILL.md");
        fs::create_dir_all(h.join(".claude/skills")).unwrap();
        std::os::unix::fs::symlink(h.join(".agents/skills/real"), h.join(".claude/skills/real"))
            .unwrap();
        // A "skill" whose directory is the home itself.
        write(&h, "SKILL.md");
        std::os::unix::fs::symlink(&*h, h.join(".claude/skills/everything")).unwrap();
        let m = Machine::new(&MachineSkills::default(), &h, Role::Agent).unwrap();

        let linked = h.join(".claude/skills/real/SKILL.md");
        assert_eq!(
            m.resolve(&linked.to_string_lossy()).unwrap(),
            Some(h.join(".agents/skills/real/SKILL.md"))
        );
        let everything = h.join(".claude/skills/everything/SKILL.md");
        let err = m.resolve(&everything.to_string_lossy()).unwrap_err();
        assert!(err.contains("would open more than the skill"), "{err}");
        assert_eq!(
            m.resolve(&h.join("missing/SKILL.md").to_string_lossy())
                .unwrap(),
            None
        );
    }
}

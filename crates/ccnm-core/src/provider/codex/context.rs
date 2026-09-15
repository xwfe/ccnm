//! Root-only projection. Override priority (including empty override files) was
//! measured with Codex 0.153.4. Never copy local Agent config to the Runtime.
use crate::error::{Error, Result};
use crate::provider::context::{self as shared, CODEX_CAP, Cap, Project};
use std::path::Path;

pub fn find(root: &Path, budget: Cap) -> Result<Option<Project>> {
    for file in ["AGENTS.override.md", "AGENTS.md"] {
        let path = root.join(file);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(
                    Error::internal("cannot inspect Codex project instructions").with_source(e)
                );
            }
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(Error::policy(
                "Codex root instructions must be regular files, not symlinks",
            ));
        }
        let target = path.canonicalize()?;
        if !target.starts_with(root.canonicalize()?) {
            return Err(Error::policy(
                "Codex instructions escaped the Runtime workspace",
            ));
        }
        let bytes = std::fs::read(target)?;
        let text = String::from_utf8_lossy(&bytes);
        return Ok(Some(Project {
            source: file,
            bytes: text.len(),
            text: budget.keep(&text).into(),
        }));
    }
    Ok(None)
}
pub fn budget(workspace: &str) -> Cap {
    let worst = Project {
        source: "AGENTS.override.md",
        bytes: usize::MAX,
        text: String::new(),
    };
    CODEX_CAP.minus(&instructions(workspace, Some(&worst)))
}
pub fn instructions(workspace: &str, project: Option<&Project>) -> String {
    shared::render(
        project.map_or("AGENTS.md", |p| p.source),
        workspace,
        project,
        &[],
    )
}

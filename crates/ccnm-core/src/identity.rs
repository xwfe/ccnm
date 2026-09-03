//! `.ccnm-workspace-id`: proof that two views are of the same project.
//!
//! The file sits in the source root on the home machine. The work machine
//! reads it through the SMB mount, the runner reads it from the local
//! filesystem, and doctor refuses to proceed unless all three agree
//! (design doc section 26). This is what catches a mount that silently
//! fell off, a mount of the wrong share, or an ssh alias pointing at the
//! wrong machine.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use uuid::Uuid;

use crate::error::{Error, ErrorCode, Result};

pub const FILE_NAME: &str = ".ccnm-workspace-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(Uuid);

impl WorkspaceId {
    pub fn generate() -> Self {
        WorkspaceId(Uuid::new_v4())
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

impl FromStr for WorkspaceId {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Uuid::parse_str(text.trim()).map(WorkspaceId).map_err(|e| {
            Error::new(
                ErrorCode::WrongWorkspace,
                format!("workspace id is not a UUID: {:?}", text.trim()),
            )
            .with_source(e)
        })
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

/// `Ok(None)` when the file does not exist; a malformed file is an error
/// because a half-written or hand-edited id must not pass as "no id".
pub fn read(root: &Path) -> Result<Option<WorkspaceId>> {
    let file = path(root);
    match std::fs::read_to_string(&file) {
        Ok(text) => text.parse().map(Some).map_err(|e: Error| {
            Error::new(e.code(), format!("{}: {}", file.display(), e.message()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!("cannot read {}", file.display()),
        )
        .with_source(e)),
    }
}

/// Create the id file. Refuses to overwrite: changing a workspace's id
/// would orphan every session and mount that knows the old one.
pub fn init(root: &Path) -> Result<WorkspaceId> {
    if !root.is_dir() {
        return Err(Error::config(format!(
            "{} is not a directory on this machine",
            root.display()
        )));
    }
    let file = path(root);
    if let Some(existing) = read(root)? {
        return Err(Error::new(
            ErrorCode::Policy,
            format!(
                "{} already exists with id {existing}; refusing to overwrite",
                file.display()
            ),
        ));
    }
    let id = WorkspaceId::generate();
    // Write-then-rename so a reader on the other side never sees a partial
    // file through SMB.
    let tmp = root.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, format!("{id}\n"))
        .and_then(|()| std::fs::rename(&tmp, &file))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::config(format!("cannot write {}", file.display())).with_source(e)
        })?;
    tracing::info!(id = %id, path = %file.display(), "workspace identity created");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-identity-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_then_read_roundtrip_and_refuse_overwrite() {
        let root = temp_root("roundtrip");
        assert_eq!(read(&root).unwrap(), None);
        let id = init(&root).unwrap();
        assert_eq!(read(&root).unwrap(), Some(id));
        assert_eq!(
            std::fs::read_to_string(path(&root)).unwrap(),
            format!("{id}\n")
        );
        let err = init(&root).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Policy);
        assert!(err.message().contains(&id.to_string()), "{err}");
        assert!(
            std::fs::read_dir(&root).unwrap().count() == 1,
            "no temp file left behind"
        );
    }

    #[test]
    fn malformed_file_is_wrong_workspace_not_none() {
        let root = temp_root("malformed");
        std::fs::write(path(&root), "hello\n").unwrap();
        let err = read(&root).unwrap_err();
        assert_eq!(err.code(), ErrorCode::WrongWorkspace);
        assert!(err.message().contains(FILE_NAME), "{err}");
    }

    #[test]
    fn init_needs_a_directory() {
        let err = init(Path::new("/nonexistent/ccnm-root")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
    }

    #[test]
    fn id_parses_with_surrounding_whitespace() {
        let id = WorkspaceId::generate();
        let parsed: WorkspaceId = format!("  {id}\n").parse().unwrap();
        assert_eq!(parsed, id);
    }
}

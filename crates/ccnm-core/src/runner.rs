//! The home-runner role: what `ccnm` does when the work machine reaches
//! back over ssh as the restricted account. Phase 1 has only `health`,
//! which reports what that account can see. Nothing here writes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Reported;
use crate::identity;
use crate::payload::{PROTOCOL, Protocol};

/// Existence and kind of a path, as seen by whoever ran the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathStatus {
    pub exists: bool,
    pub is_dir: bool,
}

impl PathStatus {
    pub fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => PathStatus {
                exists: true,
                is_dir: meta.is_dir(),
            },
            Err(_) => PathStatus {
                exists: false,
                is_dir: false,
            },
        }
    }

    pub fn is_ok(self) -> bool {
        self.exists && self.is_dir
    }

    pub fn describe(self) -> &'static str {
        match (self.exists, self.is_dir) {
            (true, true) => "directory",
            (true, false) => "exists but is not a directory",
            (false, _) => "missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthRequest {
    pub protocol: u32,
    pub root: PathBuf,
    pub runtime_root: PathBuf,
}

impl HealthRequest {
    pub fn new(root: impl Into<PathBuf>, runtime_root: impl Into<PathBuf>) -> Self {
        HealthRequest {
            protocol: PROTOCOL,
            root: root.into(),
            runtime_root: runtime_root.into(),
        }
    }
}

impl Protocol for HealthRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    pub protocol: u32,
    pub ccnm_version: String,
    /// Account the runner executed as; should be the restricted one.
    pub user: String,
    pub root: PathStatus,
    pub runtime_root: PathStatus,
    /// The workspace id as read from the local filesystem, `None` if the
    /// file is absent, `Err` if unreadable or malformed.
    pub identity: Reported<Option<String>>,
}

impl Protocol for HealthReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

pub fn health(req: &HealthRequest) -> HealthReport {
    HealthReport {
        protocol: PROTOCOL,
        ccnm_version: crate::VERSION.to_string(),
        user: std::env::var("USER").unwrap_or_else(|_| "?".to_string()),
        root: PathStatus::of(&req.root),
        runtime_root: PathStatus::of(&req.runtime_root),
        identity: identity::read(&req.root)
            .map(|id| id.map(|id| id.to_string()))
            .map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_reports_local_view() {
        let dir = std::env::temp_dir().join(format!("ccnm-runner-health-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();

        let req = HealthRequest::new(&root, dir.join("runtime"));
        let rep = health(&req);
        assert_eq!(rep.protocol, PROTOCOL);
        assert_eq!(rep.ccnm_version, crate::VERSION);
        assert!(rep.root.is_ok());
        assert!(!rep.runtime_root.exists);
        assert_eq!(rep.identity, Ok(None));

        let id = identity::init(&root).unwrap();
        let rep = health(&req);
        assert_eq!(rep.identity, Ok(Some(id.to_string())));

        // Survives the JSON trip, Result included.
        let json = serde_json::to_vec(&rep).unwrap();
        let back: HealthReport = crate::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn path_status_words() {
        assert_eq!(PathStatus::of(Path::new("/")).describe(), "directory");
        assert_eq!(
            PathStatus::of(Path::new("/nonexistent-ccnm")).describe(),
            "missing"
        );
    }
}

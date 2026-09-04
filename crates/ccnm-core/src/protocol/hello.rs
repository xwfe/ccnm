//! `ccnm internal hello`: the smallest possible round trip. Either machine
//! answers it; the caller learns which build is installed there, who it
//! ran as, and (optionally) whether a path exists from that side.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::payload::{PROTOCOL, Protocol};

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
pub struct HelloRequest {
    pub protocol: u32,
    /// A path the caller wants looked at from the answering side, e.g. the
    /// workspace root on the runtime host.
    #[serde(default)]
    pub root: Option<PathBuf>,
}

impl HelloRequest {
    pub fn new(root: Option<PathBuf>) -> Self {
        HelloRequest {
            protocol: PROTOCOL,
            root,
        }
    }
}

impl Protocol for HelloRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReport {
    pub protocol: u32,
    pub ccnm_version: String,
    /// Account the command ran as (`$USER`).
    pub user: String,
    /// `os/arch` of the answering binary.
    pub platform: String,
    /// The answering binary's own path, so the caller can see where the
    /// remote shell actually found it.
    pub exe: Option<PathBuf>,
    /// What the answering side found at the path the request named.
    ///
    /// `None` means two different things and callers have to keep them
    /// apart: the request did not ask about a path, or the answer came
    /// from a build that predates the question. Serde already treats a
    /// missing field of `Option` type as `None` without `#[serde(default)]`
    /// -- measured, not assumed -- so a reply from an older ccnm decodes
    /// and arrives here rather than failing as a malformed message.
    pub root: Option<PathStatus>,
}

impl Protocol for HelloReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// Answer a hello about this machine. Read-only.
pub fn answer(req: &HelloRequest) -> HelloReport {
    HelloReport {
        protocol: PROTOCOL,
        ccnm_version: crate::VERSION.to_string(),
        user: std::env::var("USER").unwrap_or_else(|_| "?".to_string()),
        platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        exe: std::env::current_exe().ok(),
        root: req.root.as_deref().map(PathStatus::of),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_reports_this_build_and_the_requested_path() {
        let rep = answer(&HelloRequest::new(None));
        assert_eq!(rep.protocol, PROTOCOL);
        assert_eq!(rep.ccnm_version, crate::VERSION);
        assert!(rep.platform.contains('/'));
        assert!(rep.exe.is_some());
        assert_eq!(rep.root, None);

        let rep = answer(&HelloRequest::new(Some(PathBuf::from("/"))));
        assert!(rep.root.unwrap().is_ok());
        let rep = answer(&HelloRequest::new(Some(PathBuf::from("/nonexistent-ccnm"))));
        assert_eq!(rep.root.unwrap().describe(), "missing");

        // Survives the JSON trip.
        let json = serde_json::to_vec(&rep).unwrap();
        let back: HelloReport = crate::protocol::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn request_without_root_decodes_from_older_shape() {
        // `root` is optional on the wire so a request that omits it (or a
        // caller that predates it) still parses.
        let req: HelloRequest = serde_json::from_str(r#"{"protocol":1}"#).unwrap();
        assert_eq!(req.root, None);
    }
}

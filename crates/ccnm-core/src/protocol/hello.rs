//! `ccnm internal hello`: the smallest possible round trip. Either machine
//! answers it; the caller learns which build is installed there, who it
//! ran as, and (optionally) whether a path exists from that side.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::payload::{PROTOCOL, Protocol, WIRE_LEVEL};

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
    /// The answering build's [`WIRE_LEVEL`]. `None` from a build that
    /// predates the field (before P64), which is itself the answer: it is
    /// not this build, whatever its version number says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<u32>,
}

impl HelloReport {
    /// What the answering build said about its internal protocols, when
    /// that is not what this build speaks; `None` when the two agree.
    ///
    /// Only worth asking once the version numbers already match: two
    /// different numbers are a plainer thing to report.
    pub fn other_wire(&self) -> Option<String> {
        match self.wire {
            Some(level) if level == WIRE_LEVEL => None,
            Some(level) => Some(format!("up to {level}")),
            None => Some("an older set (its hello does not say which)".to_string()),
        }
    }
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
        wire: Some(WIRE_LEVEL),
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
        assert_eq!(rep.wire, Some(WIRE_LEVEL));
        assert_eq!(rep.other_wire(), None);

        let rep = answer(&HelloRequest::new(Some(PathBuf::from("/"))));
        assert!(rep.root.unwrap().is_ok());
        let rep = answer(&HelloRequest::new(Some(PathBuf::from("/nonexistent-ccnm"))));
        assert_eq!(rep.root.unwrap().describe(), "missing");

        // Survives the JSON trip.
        let json = serde_json::to_vec(&rep).unwrap();
        let back: HelloReport = crate::protocol::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    /// A reply from before P64 has no `wire`. It still decodes -- an older
    /// build must be named as one, not reported as a malformed message --
    /// and it never compares equal to this build.
    #[test]
    fn a_reply_without_a_wire_level_decodes_and_is_not_this_build() {
        let old: HelloReport = crate::protocol::payload::decode_json(
            br#"{"protocol":1,"ccnm_version":"0.9.0","user":"me","platform":"macos/aarch64","exe":null,"root":null}"#,
        )
        .unwrap();
        assert_eq!(old.wire, None);
        assert!(old.other_wire().unwrap().contains("older"));
        let behind = HelloReport {
            wire: Some(WIRE_LEVEL - 1),
            ..old
        };
        assert_eq!(
            behind.other_wire().unwrap(),
            format!("up to {}", WIRE_LEVEL - 1)
        );
    }

    /// `WIRE_LEVEL` is only worth comparing if it moves when the protocols
    /// do. Read the numbers out of the source rather than list them here: a
    /// list is one more thing to forget in the same change.
    #[test]
    fn the_wire_level_has_not_been_passed_by_a_protocol_number() {
        fn scan(dir: &Path, found: &mut Vec<(String, u32)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    scan(&path, found);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                for line in std::fs::read_to_string(&path).unwrap().lines() {
                    let Some(rest) = line.trim().strip_prefix("pub const ") else {
                        continue;
                    };
                    let Some((name, value)) = rest.split_once(": u32 = ") else {
                        continue;
                    };
                    if !name.ends_with("PROTOCOL") {
                        continue;
                    }
                    let value = value.trim_end_matches(';').parse().unwrap_or_else(|_| {
                        panic!("{name} in {} is not a plain number", path.display())
                    });
                    found.push((name.to_string(), value));
                }
            }
        }
        let mut found = Vec::new();
        scan(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        // The scan has to be finding them at all for the comparison to mean
        // anything: these two are the oldest and, today, the newest.
        assert!(
            found.contains(&("PROTOCOL".to_string(), PROTOCOL)),
            "{found:?}"
        );
        assert!(
            found.contains(&(
                "CLEANUP_PROTOCOL".to_string(),
                crate::cleanup::CLEANUP_PROTOCOL
            )),
            "{found:?}"
        );
        let highest = found.iter().max_by_key(|(_, value)| *value).unwrap();
        assert!(
            WIRE_LEVEL >= highest.1,
            "{} is {}, past WIRE_LEVEL {WIRE_LEVEL}: raise WIRE_LEVEL in the same change",
            highest.0,
            highest.1
        );
    }

    #[test]
    fn request_without_root_decodes_from_older_shape() {
        // `root` is optional on the wire so a request that omits it (or a
        // caller that predates it) still parses.
        let req: HelloRequest = serde_json::from_str(r#"{"protocol":1}"#).unwrap();
        assert_eq!(req.root, None);
    }
}

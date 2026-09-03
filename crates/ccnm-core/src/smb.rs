//! SMB on macOS through the system tools only (design doc section 39).
//!
//! Work side: `mount -t smbfs` to mount, `umount` to unmount, and
//! `smbutil statshares -m <path> -f Json` to ask whether a path is an SMB
//! mount. Home side: `sharing -l` to see what the machine exports.
//!
//! ccnm never parses `mount(8)` text or guesses from a directory being
//! non-empty; when Apple changes a format, these tools change with it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::is_token;
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, Output};

/// What `mount_mode = "coherence"` means. Each option is documented in
/// `man mount_smbfs` on macOS 15 (verified 2026-09-03):
/// no data or metadata caching so Claude always reads what the home
/// machine has now, no password prompt so a missing credential fails
/// instead of hanging, soft so a dead server returns errors instead of
/// wedging every Read, nobrowse so Finder leaves it alone.
pub const MOUNT_OPTIONS: &str = "nodatacache,nomdatacache,nopassprompt,soft,nobrowse";

/// `//user@host/share`, built only from validated tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmbUrl(String);

impl SmbUrl {
    pub fn new(user: &str, host: &str, share: &str) -> Result<SmbUrl> {
        for (what, value) in [("user", user), ("host", host), ("share", share)] {
            if !is_token(value) {
                return Err(Error::config(format!(
                    "SMB {what} must match [A-Za-z0-9._-]+, got \"{value}\""
                )));
            }
        }
        Ok(SmbUrl(format!("//{user}@{host}/{share}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SmbUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn mount_cmd(url: &SmbUrl, mountpoint: &Path) -> Cmd {
    Cmd::new("mount")
        .args(["-t", "smbfs", "-o", MOUNT_OPTIONS])
        .arg(url.as_str())
        .arg(mountpoint)
        .timeout(Duration::from_secs(60))
}

pub fn unmount_cmd(mountpoint: &Path) -> Cmd {
    Cmd::new("umount")
        .arg(mountpoint)
        .timeout(Duration::from_secs(30))
}

pub fn statshares_cmd(mountpoint: &Path) -> Cmd {
    Cmd::new("smbutil")
        .args(["statshares", "-m"])
        .arg(mountpoint)
        .args(["-f", "Json"])
        .timeout(Duration::from_secs(15))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountStatus {
    pub mounted: bool,
    pub detail: String,
}

/// `smbutil statshares -m` exits 0 with JSON for an SMB mount and 64
/// (EX_USAGE, "Invalid parameter") for a path that is not one. Verified on
/// macOS 15 / Darwin 25.3. Anything else is a real failure.
pub fn parse_statshares(out: &Output) -> Result<MountStatus> {
    match out.exit_code {
        Some(0) => Ok(MountStatus {
            mounted: true,
            detail: describe_statshares(&out.stdout),
        }),
        Some(64) => Ok(MountStatus {
            mounted: false,
            detail: "not an SMB mount".to_string(),
        }),
        code => Err(Error::new(
            ErrorCode::Mount,
            format!(
                "smbutil statshares failed (exit {}): {}",
                code.map_or_else(|| "signal".to_string(), |c| c.to_string()),
                out.stderr_lossy().trim()
            ),
        )),
    }
}

/// Attribute names `smbutil statshares` prints in its human-readable table.
/// The JSON key names have not been checked against a real mount yet, so
/// this is best effort: anything found is shown, nothing is required.
const INTERESTING: [&str; 4] = ["SERVER_NAME", "SHARE_NAME", "SMB_VERSION", "MOUNT_PATH"];

fn describe_statshares(stdout: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(stdout) else {
        return "mounted (smbfs)".to_string();
    };
    let mut fields: Vec<(String, String)> = Vec::new();
    collect_fields(&value, &mut fields);
    if fields.is_empty() {
        "mounted (smbfs)".to_string()
    } else {
        let joined: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("mounted, {}", joined.join(", "))
    }
}

fn collect_fields(value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                let upper = key.to_ascii_uppercase();
                if let Value::String(s) = val
                    && INTERESTING.contains(&upper.as_str())
                    && !out.iter().any(|(k, _)| *k == upper)
                {
                    out.push((upper, s.clone()));
                }
                collect_fields(val, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_fields(v, out)),
        _ => {}
    }
}

pub fn sharing_list_cmd() -> Cmd {
    Cmd::new("sharing")
        .arg("-l")
        .timeout(Duration::from_secs(15))
}

/// One entry of `sharing -l`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharePoint {
    pub name: String,
    pub path: PathBuf,
    /// Name inside the `smb: { ... }` block, which is what clients see and
    /// may differ from the share point's own name.
    pub smb_name: Option<String>,
    pub smb_shared: bool,
}

/// Parse `sharing -l`. The format (verified on macOS 15) is
///
/// ```text
/// name:        <share point name>
/// path:        /some/path
///     smb:     {
///              name:    <smb name>
///              shared:  1
///     }
/// ```
///
/// Lines are `key:<tabs>value`; a value of `{` opens a block named by its
/// key and `}` closes it.
pub fn parse_sharing_list(text: &str) -> Vec<SharePoint> {
    let mut points: Vec<SharePoint> = Vec::new();
    let mut block: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line == "}" {
            block = None;
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if value == "{" {
            block = Some(key.to_string());
            continue;
        }
        match (block.as_deref(), key) {
            (None, "name") => points.push(SharePoint {
                name: value.to_string(),
                ..SharePoint::default()
            }),
            (None, "path") => {
                if let Some(point) = points.last_mut() {
                    point.path = PathBuf::from(value);
                }
            }
            (Some("smb"), "name") => {
                if let Some(point) = points.last_mut() {
                    point.smb_name = Some(value.to_string());
                }
            }
            (Some("smb"), "shared") => {
                if let Some(point) = points.last_mut() {
                    point.smb_shared = value == "1";
                }
            }
            _ => {}
        }
    }
    points
}

/// Find the share point clients would reach as `share`.
pub fn find_share<'a>(points: &'a [SharePoint], share: &str) -> Option<&'a SharePoint> {
    points
        .iter()
        .find(|p| p.smb_name.as_deref() == Some(share))
        .or_else(|| points.iter().find(|p| p.name == share))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_is_built_from_tokens_only() {
        assert_eq!(
            SmbUrl::new("fodelf", "home.tail.ts.net", "xshun")
                .unwrap()
                .as_str(),
            "//fodelf@home.tail.ts.net/xshun"
        );
        assert!(SmbUrl::new("a b", "h", "s").is_err());
        assert!(SmbUrl::new("u", "h/../x", "s").is_err());
        assert!(SmbUrl::new("u", "h", "-s").is_err());
    }

    #[test]
    fn mount_cmd_uses_system_mount_with_coherence_options() {
        let url = SmbUrl::new("u", "h", "s").unwrap();
        assert_eq!(
            mount_cmd(&url, Path::new("/Users/Shared/cc-workspaces/x")).display(),
            "mount -t smbfs -o nodatacache,nomdatacache,nopassprompt,soft,nobrowse //u@h/s /Users/Shared/cc-workspaces/x"
        );
        assert_eq!(unmount_cmd(Path::new("/m")).display(), "umount /m");
        assert_eq!(
            statshares_cmd(Path::new("/m")).display(),
            "smbutil statshares -m /m -f Json"
        );
    }

    #[test]
    fn statshares_exit_codes() {
        let not_mounted = Output::exited(64, "[\n\n]");
        assert_eq!(
            parse_statshares(&not_mounted).unwrap(),
            MountStatus {
                mounted: false,
                detail: "not an SMB mount".into()
            }
        );

        let mounted = Output::exited(
            0,
            r#"[{"share":"xshun","attributes":{"SERVER_NAME":"home.local","SMB_VERSION":"SMB_3.1.1","noise":1}}]"#,
        );
        let status = parse_statshares(&mounted).unwrap();
        assert!(status.mounted);
        assert_eq!(
            status.detail,
            "mounted, SERVER_NAME=home.local, SMB_VERSION=SMB_3.1.1"
        );

        let odd = Output::exited(0, "not json");
        assert_eq!(parse_statshares(&odd).unwrap().detail, "mounted (smbfs)");

        let mut broken = Output::exited(1, "");
        broken.stderr = b"smbutil: something\n".to_vec();
        let err = parse_statshares(&broken).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Mount);
        assert!(err.message().contains("something"), "{err}");
    }

    /// Captured from `sharing -l` on this machine (macOS 15), plus a second
    /// share point with an SMB name that differs from its own name.
    const SHARING: &str = "\n\t\t\tList of Share Points\nname:\t\t“fodelf”的公共文件夹\npath:\t\t/Users/fodelf/Public\n\tsmb:\t{\n    \t\tname:\t“fodelf”的公共文件夹\n    \t\tshared:\t1\n    \t\tguest access:\t1\n    \t\tread-only:\t0\n    \t\tsealed:\t0\n\t}\n\nname:\t\tcc-xshun\npath:\t\t/Users/Shared/cc-workspaces/xshun\n\tafp:\t{\n    \t\tname:\tcc-xshun\n    \t\tshared:\t0\n\t}\n\tsmb:\t{\n    \t\tname:\txshun\n    \t\tshared:\t1\n\t}\n";

    #[test]
    fn sharing_list_parses_blocks() {
        let points = parse_sharing_list(SHARING);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].path, PathBuf::from("/Users/fodelf/Public"));
        assert_eq!(points[0].smb_name.as_deref(), Some("“fodelf”的公共文件夹"));
        assert!(points[0].smb_shared);
        assert_eq!(points[1].name, "cc-xshun");
        assert_eq!(points[1].smb_name.as_deref(), Some("xshun"));
        assert!(points[1].smb_shared, "afp shared:0 must not clobber smb");

        let found = find_share(&points, "xshun").unwrap();
        assert_eq!(
            found.path,
            PathBuf::from("/Users/Shared/cc-workspaces/xshun")
        );
        assert!(
            find_share(&points, "cc-xshun").is_some(),
            "falls back to point name"
        );
        assert!(find_share(&points, "nope").is_none());
    }

    #[test]
    fn sharing_list_empty_and_garbage() {
        assert!(parse_sharing_list("").is_empty());
        assert!(parse_sharing_list("\n\t\t\tList of Share Points\n").is_empty());
    }
}

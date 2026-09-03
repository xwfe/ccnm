//! Read-only look at `tailscale status --json` to say whether the work
//! machine is reachable directly or only through a DERP relay. Never
//! blocking: SSH decides reachability, this only explains latency.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::claude::is_executable;
use crate::error::{Error, ErrorCode, Result};
use crate::process::Cmd;

pub fn locate(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = path_var {
        candidates.extend(std::env::split_paths(path).map(|dir| dir.join("tailscale")));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/tailscale"));
    candidates.push(PathBuf::from("/usr/local/bin/tailscale"));
    candidates.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    candidates.into_iter().find(|p| is_executable(p))
}

pub fn locate_from_env() -> Option<PathBuf> {
    locate(std::env::var_os("PATH").as_deref())
}

pub fn status_cmd(bin: &Path) -> Cmd {
    Cmd::new(bin)
        .args(["status", "--json"])
        .timeout(Duration::from_secs(10))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `CurAddr` is set: packets go straight to this endpoint.
    Direct(String),
    /// Traffic is relayed through this DERP region. Every MCP round trip
    /// pays the relay's latency, so doctor warns.
    Relay(String),
    /// No traffic has flowed yet, so Tailscale has not chosen a path.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub host_name: String,
    pub online: bool,
    pub route: Route,
}

impl Peer {
    pub fn describe(&self) -> String {
        if !self.online {
            return format!("{} is offline", self.host_name);
        }
        match &self.route {
            Route::Direct(addr) => format!("direct via {addr}"),
            Route::Relay(region) => format!("relayed through DERP {region}"),
            Route::Unknown => "online, path not chosen yet (no traffic)".to_string(),
        }
    }
}

/// Fields of `tailscale status --json` this module reads. Verified against
/// the CLI on 2026-09-03.
#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Peer", default)]
    peer: HashMap<String, RawPeer>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
    #[serde(rename = "CurAddr", default)]
    cur_addr: String,
    #[serde(rename = "Relay", default)]
    relay: String,
    #[serde(rename = "Active", default)]
    active: bool,
}

/// Look `target` (the HostName from `ssh -G`) up among the peers. Matches
/// the peer's HostName, its MagicDNS name with or without the trailing
/// dot, the first label of that name, or one of its Tailscale IPs.
pub fn find_peer(status_json: &[u8], target: &str) -> Result<Option<Peer>> {
    let status: Status = serde_json::from_slice(status_json).map_err(|e| {
        Error::new(
            ErrorCode::Internal,
            "tailscale status --json is not the expected JSON",
        )
        .with_source(e)
    })?;
    if status.backend_state != "Running" {
        return Err(Error::new(
            ErrorCode::Internal,
            format!(
                "tailscale is not running (state {:?})",
                status.backend_state
            ),
        ));
    }
    let target = target.trim_end_matches('.').to_ascii_lowercase();
    let peer = status.peer.into_values().find(|p| {
        let dns = p.dns_name.trim_end_matches('.').to_ascii_lowercase();
        let first_label = dns.split('.').next().unwrap_or("").to_string();
        p.host_name.eq_ignore_ascii_case(&target)
            || dns == target
            || (!first_label.is_empty() && first_label == target)
            || p.ips.iter().any(|ip| ip.eq_ignore_ascii_case(&target))
    });
    Ok(peer.map(|p| Peer {
        host_name: p.host_name,
        online: p.online,
        route: if !p.cur_addr.is_empty() {
            Route::Direct(p.cur_addr)
        } else if p.active && !p.relay.is_empty() {
            Route::Relay(p.relay)
        } else {
            Route::Unknown
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like real output from 2026-09-03, values invented.
    const STATUS: &str = r#"{
  "Version": "1.90.0",
  "BackendState": "Running",
  "Self": {"HostName": "fodelf", "DNSName": "fodelf.taila864e6.ts.net.", "Online": true},
  "Peer": {
    "nodekey:aaa": {"HostName": "workmac", "DNSName": "workmac.taila864e6.ts.net.", "TailscaleIPs": ["100.1.1.1"], "Online": true, "CurAddr": "203.0.113.7:41641", "Relay": "tok", "Active": true},
    "nodekey:bbb": {"HostName": "hpsrv", "DNSName": "hpsrv.taila864e6.ts.net.", "TailscaleIPs": ["100.2.2.2"], "Online": true, "CurAddr": "", "Relay": "tok", "Active": true},
    "nodekey:ccc": {"HostName": "WIN11", "DNSName": "win11.taila864e6.ts.net.", "TailscaleIPs": ["100.3.3.3"], "Online": false, "CurAddr": "", "Relay": "", "Active": false},
    "nodekey:ddd": {"HostName": "idle", "DNSName": "idle.taila864e6.ts.net.", "TailscaleIPs": ["100.4.4.4"], "Online": true, "CurAddr": "", "Relay": "sfo", "Active": false}
  }
}"#;

    #[test]
    fn matches_by_hostname_dns_label_or_ip() {
        for target in [
            "workmac",
            "WorkMac",
            "workmac.taila864e6.ts.net",
            "workmac.taila864e6.ts.net.",
            "100.1.1.1",
        ] {
            let peer = find_peer(STATUS.as_bytes(), target).unwrap().unwrap();
            assert_eq!(peer.host_name, "workmac", "{target}");
            assert_eq!(peer.describe(), "direct via 203.0.113.7:41641");
        }
        assert!(find_peer(STATUS.as_bytes(), "nobody").unwrap().is_none());
    }

    #[test]
    fn route_classification() {
        let relay = find_peer(STATUS.as_bytes(), "hpsrv").unwrap().unwrap();
        assert_eq!(relay.route, Route::Relay("tok".into()));
        assert_eq!(relay.describe(), "relayed through DERP tok");

        let offline = find_peer(STATUS.as_bytes(), "win11").unwrap().unwrap();
        assert!(!offline.online);
        assert_eq!(offline.describe(), "WIN11 is offline");

        let idle = find_peer(STATUS.as_bytes(), "idle").unwrap().unwrap();
        assert_eq!(idle.route, Route::Unknown);
    }

    #[test]
    fn not_running_and_garbage() {
        let err = find_peer(br#"{"BackendState":"Stopped","Peer":{}}"#, "x").unwrap_err();
        assert!(err.message().contains("Stopped"), "{err}");
        assert!(find_peer(b"nope", "x").is_err());
    }

    #[test]
    fn status_cmd_shape() {
        assert_eq!(
            status_cmd(Path::new("/opt/homebrew/bin/tailscale")).display(),
            "/opt/homebrew/bin/tailscale status --json"
        );
    }
}

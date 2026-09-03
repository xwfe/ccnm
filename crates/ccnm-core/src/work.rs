//! The work-controller role: what `ccnm` does on the work machine when the
//! home launcher calls it over ssh. Phase 1: `probe` (read-only, for
//! doctor) and `mount` / `unmount` (the explicit state changes).
//!
//! The work machine has no config file. Everything it needs arrives in the
//! request; everything it learned goes back in the report, errors
//! included, so doctor can render one row per fact.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::claude::{self, AuthStatus};
use crate::error::{Error, ErrorCode, Reported, Result};
use crate::identity;
use crate::payload::{PROTOCOL, Protocol};
use crate::process::ProcessRunner;
use crate::runner::{HealthReport, HealthRequest, PathStatus};
use crate::smb::{self, MountStatus, SmbUrl};
use crate::ssh::{Master, ResolvedSsh, Ssh};

/// What the work-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`claude::locate`] found one.
    pub claude: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub protocol: u32,
    pub root: PathBuf,
    pub runtime_root: PathBuf,
    /// Alias in this machine's `~/.ssh/config` for the home runner.
    pub home_alias: String,
    pub claude_config_dir: Option<PathBuf>,
}

impl Protocol for ProbeRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeReport {
    pub path: Option<PathBuf>,
    pub version: Reported<String>,
    pub auth: Reported<AuthStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub protocol: u32,
    pub ccnm_version: String,
    pub root: PathStatus,
    /// Workspace id as read through the mount.
    pub identity: Reported<Option<String>>,
    pub mount: Reported<MountStatus>,
    /// What `ssh -G <home_alias>` resolves to on this machine.
    pub home_ssh: Reported<ResolvedSsh>,
    /// The home runner's own report, fetched over the reverse ssh.
    pub health: Reported<HealthReport>,
    pub claude: ClaudeReport,
}

impl Protocol for ProbeReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// Everything doctor wants to know about this machine, in one round trip.
/// Read-only: no mount, no master connection, no file written.
pub fn probe(req: &ProbeRequest, tools: &Tools<'_>) -> ProbeReport {
    let mount = tools
        .runner
        .run(&smb::statshares_cmd(&req.root))
        .and_then(|out| smb::parse_statshares(&out))
        .map_err(Into::into);

    let (home_ssh, health) = match Ssh::new(&req.home_alias, &tools.control_dir) {
        Err(e) => (
            Err(e.into()),
            Err(Error::new(
                ErrorCode::HomeUnreachable,
                "not attempted: home alias is invalid",
            )
            .into()),
        ),
        Ok(ssh) => {
            let home_ssh = ssh.resolve(tools.runner).map_err(Into::into);
            let health = ssh
                .check_control_path()
                .and_then(|()| {
                    ssh.call_ccnm::<_, HealthReport>(
                        tools.runner,
                        Master::Reuse,
                        &["runner", "health"],
                        &HealthRequest::new(&req.root, &req.runtime_root),
                        Duration::from_secs(30),
                        ErrorCode::HomeUnreachable,
                    )
                })
                .map_err(Into::into);
            (home_ssh, health)
        }
    };

    ProbeReport {
        protocol: PROTOCOL,
        ccnm_version: crate::VERSION.to_string(),
        root: PathStatus::of(&req.root),
        identity: identity::read(&req.root)
            .map(|id| id.map(|id| id.to_string()))
            .map_err(Into::into),
        mount,
        home_ssh,
        health,
        claude: probe_claude(tools, req.claude_config_dir.as_deref()),
    }
}

fn probe_claude(tools: &Tools<'_>, config_dir: Option<&Path>) -> ClaudeReport {
    let Some(bin) = &tools.claude else {
        let missing = Error::new(
            ErrorCode::Version,
            "claude not found: looked in PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
        );
        return ClaudeReport {
            path: None,
            version: Err((&missing).into()),
            auth: Err(missing.into()),
        };
    };
    let version = tools
        .runner
        .run(&claude::version_cmd(bin, config_dir))
        .and_then(|out| claude::parse_version(&out))
        .map_err(Into::into);
    let auth = tools
        .runner
        .run(&claude::auth_status_cmd(bin, config_dir))
        .and_then(|out| claude::parse_auth(&out))
        .map_err(Into::into);
    ClaudeReport {
        path: Some(bin.clone()),
        version,
        auth,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRequest {
    pub protocol: u32,
    pub mountpoint: PathBuf,
    pub share: String,
    pub smb_user: String,
    /// Its resolved HostName is the SMB server address, so ssh and SMB can
    /// never point at different machines.
    pub home_alias: String,
}

impl Protocol for MountRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountReport {
    pub protocol: u32,
    pub url: String,
    pub already_mounted: bool,
    pub status: MountStatus,
}

impl Protocol for MountReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

/// Mount the share at `mountpoint`. Idempotent: an existing SMB mount there
/// is reported, not remounted. Refuses to mount over a non-empty directory.
pub fn mount(req: &MountRequest, tools: &Tools<'_>) -> Result<MountReport> {
    // A state change is allowed here, unlike in probe, so make sure the
    // socket directory exists for the masters that later phases will start.
    std::fs::create_dir_all(&tools.control_dir).map_err(|e| {
        Error::config(format!("cannot create {}", tools.control_dir.display())).with_source(e)
    })?;
    let ssh = Ssh::new(&req.home_alias, &tools.control_dir)?;
    let resolved = ssh.resolve(tools.runner)?;
    let url = SmbUrl::new(&req.smb_user, &resolved.hostname, &req.share)?;

    let before = smb::parse_statshares(&tools.runner.run(&smb::statshares_cmd(&req.mountpoint))?)?;
    if before.mounted {
        return Ok(MountReport {
            protocol: PROTOCOL,
            url: url.to_string(),
            already_mounted: true,
            status: before,
        });
    }

    prepare_mountpoint(&req.mountpoint)?;

    let out = tools.runner.run(&smb::mount_cmd(&url, &req.mountpoint))?;
    if !out.success() {
        return Err(Error::new(
            ErrorCode::Mount,
            format!(
                "mount -t smbfs {url} {} failed (exit {:?}): {}\nif that is an authentication error: this machine's Keychain has no password for {}@{}; connect once in Finder (Go > Connect to Server, smb://{}) and tick \"Remember this password\". nopassprompt makes ccnm fail here instead of hanging on a prompt.",
                req.mountpoint.display(),
                out.exit_code,
                out.stderr_lossy().trim(),
                req.smb_user,
                resolved.hostname,
                resolved.hostname
            ),
        ));
    }

    let after = smb::parse_statshares(&tools.runner.run(&smb::statshares_cmd(&req.mountpoint))?)?;
    if !after.mounted {
        return Err(Error::new(
            ErrorCode::Mount,
            format!(
                "mount exited 0 but smbutil does not see an SMB mount at {}",
                req.mountpoint.display()
            ),
        ));
    }
    tracing::info!(url = %url, mountpoint = %req.mountpoint.display(), "mounted");
    Ok(MountReport {
        protocol: PROTOCOL,
        url: url.to_string(),
        already_mounted: false,
        status: after,
    })
}

/// The mountpoint must be a directory and empty. `/Users/Shared` is
/// world-writable on macOS, so creating it needs no sudo.
fn prepare_mountpoint(mountpoint: &Path) -> Result<()> {
    match std::fs::metadata(mountpoint) {
        Ok(meta) if !meta.is_dir() => Err(Error::new(
            ErrorCode::Mount,
            format!("{} exists and is not a directory", mountpoint.display()),
        )),
        Ok(_) => {
            let mut entries = std::fs::read_dir(mountpoint)?;
            if entries.next().is_some() {
                return Err(Error::new(
                    ErrorCode::Mount,
                    format!(
                        "{} is not empty and not an SMB mount; refusing to mount over existing files",
                        mountpoint.display()
                    ),
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(mountpoint)
            .map_err(|e| {
                Error::new(
                    ErrorCode::Mount,
                    format!("cannot create mountpoint {}", mountpoint.display()),
                )
                .with_source(e)
            }),
        Err(e) => Err(Error::new(
            ErrorCode::Mount,
            format!("cannot stat {}", mountpoint.display()),
        )
        .with_source(e)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmountRequest {
    pub protocol: u32,
    pub mountpoint: PathBuf,
}

impl Protocol for UnmountRequest {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmountReport {
    pub protocol: u32,
    pub was_mounted: bool,
}

impl Protocol for UnmountReport {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

pub fn unmount(req: &UnmountRequest, tools: &Tools<'_>) -> Result<UnmountReport> {
    let status = smb::parse_statshares(&tools.runner.run(&smb::statshares_cmd(&req.mountpoint))?)?;
    if !status.mounted {
        return Ok(UnmountReport {
            protocol: PROTOCOL,
            was_mounted: false,
        });
    }
    let out = tools.runner.run(&smb::unmount_cmd(&req.mountpoint))?;
    if !out.success() {
        return Err(Error::new(
            ErrorCode::Mount,
            format!(
                "umount {} failed (exit {:?}): {}\nsomething still has the mount open: a shell cd'd into it, an editor, or a running Claude session",
                req.mountpoint.display(),
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        ));
    }
    tracing::info!(mountpoint = %req.mountpoint.display(), "unmounted");
    Ok(UnmountReport {
        protocol: PROTOCOL,
        was_mounted: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};

    fn temp(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-work-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    fn health_json(root: &Path, id: Option<&str>) -> String {
        let rep = HealthReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            root: PathStatus::of(root),
            runtime_root: PathStatus {
                exists: true,
                is_dir: true,
            },
            identity: Ok(id.map(String::from)),
        };
        serde_json::to_string(&rep).unwrap()
    }

    #[test]
    fn probe_collects_every_fact_in_one_report() {
        let dir = temp("probe");
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let id = identity::init(&root).unwrap();

        let fake = FakeRunner::new();
        // Call order: statshares, ssh -G, ssh runner health, claude --version, claude auth status.
        fake.push(Output::exited(0, "[{\"SERVER_NAME\":\"home\"}]"));
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, health_json(&root, Some(&id.to_string()))));
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
        fake.push(Output::exited(0, r#"{"loggedIn":true,"email":"me@x"}"#));

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
        };
        let req = ProbeRequest {
            protocol: PROTOCOL,
            root: root.clone(),
            runtime_root: dir.join("runtime"),
            home_alias: "ccnm-home".into(),
            claude_config_dir: Some(PathBuf::from("/x/claude")),
        };
        let rep = probe(&req, &tools);

        assert_eq!(rep.ccnm_version, crate::VERSION);
        assert!(rep.root.is_ok());
        assert_eq!(rep.identity, Ok(Some(id.to_string())));
        assert_eq!(
            rep.mount.as_ref().unwrap().detail,
            "mounted, SERVER_NAME=home"
        );
        assert_eq!(rep.home_ssh.as_ref().unwrap().target(), "ccrun@home.ts");
        assert_eq!(rep.health.as_ref().unwrap().user, "ccrun");
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert!(rep.claude.auth.as_ref().unwrap().logged_in);

        let calls = fake.calls();
        assert_eq!(calls.len(), 5);
        assert!(calls[0].display().starts_with("smbutil statshares -m"));
        assert_eq!(calls[1].display(), "ssh -G ccnm-home");
        let reverse = calls[2].display();
        assert!(
            reverse.contains("ControlMaster=no"),
            "doctor path must not start a master: {reverse}"
        );
        assert!(
            reverse.contains("-T ccnm-home ccnm runner health --payload"),
            "{reverse}"
        );
        assert!(
            calls[3]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );

        // Nothing was written by probe.
        assert!(
            !control(&dir).exists(),
            "probe must not create the control dir"
        );

        let json = serde_json::to_vec(&rep).unwrap();
        let back: ProbeReport = crate::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn probe_records_failures_instead_of_aborting() {
        let dir = temp("probe-fail");
        let fake = FakeRunner::new();
        fake.push(Output::exited(64, "[\n\n]")); // not mounted
        fake.push(Output::exited(0, "hostname home.ts\n"));
        let mut unreachable = Output::exited(255, "");
        unreachable.stderr =
            b"ssh: connect to host home.ts port 22: Operation timed out\n".to_vec();
        fake.push(unreachable);

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let req = ProbeRequest {
            protocol: PROTOCOL,
            root: dir.join("missing-root"),
            runtime_root: dir.join("runtime"),
            home_alias: "ccnm-home".into(),
            claude_config_dir: None,
        };
        let rep = probe(&req, &tools);
        assert!(!rep.root.exists);
        assert_eq!(rep.identity, Ok(None));
        assert!(!rep.mount.unwrap().mounted);
        let health_err = rep.health.unwrap_err();
        assert_eq!(health_err.code(), ErrorCode::HomeUnreachable);
        assert!(health_err.message.contains("Operation timed out"));
        assert_eq!(rep.claude.path, None);
        assert_eq!(rep.claude.version.unwrap_err().code(), ErrorCode::Version);
        assert_eq!(fake.calls().len(), 3, "no claude calls without a binary");
    }

    #[test]
    fn mount_flow_builds_url_from_ssh_hostname() {
        let dir = temp("mount");
        let mountpoint = dir.join("mp");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.tail.ts.net\nuser ccrun\n"));
        fake.push(Output::exited(64, "")); // not mounted yet
        fake.push(Output::exited(0, "")); // mount ok
        fake.push(Output::exited(0, "[{\"SERVER_NAME\":\"home\"}]")); // now mounted

        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let req = MountRequest {
            protocol: PROTOCOL,
            mountpoint: mountpoint.clone(),
            share: "xshun".into(),
            smb_user: "fodelf".into(),
            home_alias: "ccnm-home".into(),
        };
        let rep = mount(&req, &tools).unwrap();
        assert_eq!(rep.url, "//fodelf@home.tail.ts.net/xshun");
        assert!(!rep.already_mounted);
        assert!(rep.status.mounted);
        assert!(mountpoint.is_dir(), "mountpoint created");
        assert!(
            control(&dir).is_dir(),
            "control dir created by the mutating command"
        );
        assert_eq!(
            fake.calls()[2].display(),
            format!(
                "mount -t smbfs -o {} //fodelf@home.tail.ts.net/xshun {}",
                smb::MOUNT_OPTIONS,
                mountpoint.display()
            )
        );
    }

    #[test]
    fn mount_is_idempotent_and_refuses_nonempty_dirs() {
        let dir = temp("mount-guard");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname h\n"));
        fake.push(Output::exited(0, "[]")); // already mounted
        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let req = MountRequest {
            protocol: PROTOCOL,
            mountpoint: dir.join("mp"),
            share: "s".into(),
            smb_user: "u".into(),
            home_alias: "h".into(),
        };
        assert!(mount(&req, &tools).unwrap().already_mounted);
        assert_eq!(fake.calls().len(), 2, "no mount attempted");

        let busy = dir.join("busy");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("file"), "x").unwrap();
        fake.push(Output::exited(0, "hostname h\n"));
        fake.push(Output::exited(64, ""));
        let req = MountRequest {
            mountpoint: busy,
            ..req
        };
        let err = mount(&req, &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Mount);
        assert!(err.message().contains("not empty"), "{err}");
    }

    #[test]
    fn mount_failure_explains_keychain() {
        let dir = temp("mount-auth");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        fake.push(Output::exited(64, ""));
        let mut failed = Output::exited(77, "");
        failed.stderr =
            b"mount_smbfs: server rejected the connection: Authentication error\n".to_vec();
        fake.push(failed);
        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let req = MountRequest {
            protocol: PROTOCOL,
            mountpoint: dir.join("mp"),
            share: "xshun".into(),
            smb_user: "fodelf".into(),
            home_alias: "h".into(),
        };
        let err = mount(&req, &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Mount);
        assert!(err.message().contains("Authentication error"), "{err}");
        assert!(err.message().contains("smb://home.ts"), "{err}");
    }

    #[test]
    fn unmount_flow() {
        let dir = temp("unmount");
        let fake = FakeRunner::new();
        fake.push(Output::exited(64, ""));
        let tools = Tools {
            runner: &fake,
            control_dir: control(&dir),
            claude: None,
        };
        let req = UnmountRequest {
            protocol: PROTOCOL,
            mountpoint: dir.join("mp"),
        };
        assert!(!unmount(&req, &tools).unwrap().was_mounted);

        fake.push(Output::exited(0, "[]"));
        fake.push(Output::exited(0, ""));
        assert!(unmount(&req, &tools).unwrap().was_mounted);
        assert_eq!(
            fake.calls()[2].display(),
            format!("umount {}", dir.join("mp").display())
        );

        fake.push(Output::exited(0, "[]"));
        let mut busy = Output::exited(1, "");
        busy.stderr = b"umount: Resource busy\n".to_vec();
        fake.push(busy);
        let err = unmount(&req, &tools).unwrap_err();
        assert!(err.message().contains("Resource busy"), "{err}");
    }
}

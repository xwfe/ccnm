//! The home-launcher role's state-changing commands: `ccnm workspace init`,
//! `ccnm mount`, `ccnm unmount`. Doctor lives in its own module and stays
//! read-only; these are the commands it tells you to run.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::Resolved;
use crate::error::{Error, ErrorCode, Result};
use crate::identity::{self, WorkspaceId};
use crate::payload::PROTOCOL;
use crate::process::ProcessRunner;
use crate::ssh::{Master, Ssh};
use crate::work::{MountReport, MountRequest, UnmountReport, UnmountRequest};

pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on the home machine.
    pub control_dir: PathBuf,
}

/// Write `.ccnm-workspace-id` into the source root on this machine.
pub fn workspace_init(resolved: &Resolved<'_>) -> Result<WorkspaceId> {
    identity::init(&resolved.workspace.root)
}

/// Ask the work machine to mount this workspace's share.
pub fn mount(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<MountReport> {
    let ssh = work_ssh(resolved, env)?;
    let req = MountRequest {
        protocol: PROTOCOL,
        mountpoint: resolved.workspace.root.clone(),
        share: resolved.workspace.share.clone(),
        smb_user: resolved.smb_user.to_string(),
        home_alias: resolved.home_alias.to_string(),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Auto,
        &["work", "mount"],
        &req,
        Duration::from_secs(120),
        ErrorCode::WorkUnreachable,
    )
}

pub fn unmount(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<UnmountReport> {
    let ssh = work_ssh(resolved, env)?;
    let req = UnmountRequest {
        protocol: PROTOCOL,
        mountpoint: resolved.workspace.root.clone(),
    };
    ssh.call_ccnm(
        env.runner,
        Master::Auto,
        &["work", "unmount"],
        &req,
        Duration::from_secs(60),
        ErrorCode::WorkUnreachable,
    )
}

/// These commands may change state, so they also make sure the socket
/// directory exists and start a master that later commands reuse.
fn work_ssh(resolved: &Resolved<'_>, env: &Env<'_>) -> Result<Ssh> {
    std::fs::create_dir_all(&env.control_dir).map_err(|e| {
        Error::config(format!("cannot create {}", env.control_dir.display())).with_source(e)
    })?;
    let ssh = Ssh::new(resolved.work_ssh, &env.control_dir)?;
    ssh.check_control_path()?;
    Ok(ssh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::process::{FakeRunner, Output};
    use crate::smb::MountStatus;

    const CONFIG: &str = "version = 1\n[hosts.work]\nssh = \"work\"\n[hosts.home_runner]\nssh_from_work = \"ccnm-home\"\nsmb_user = \"fodelf\"\n[workspaces.xshun]\nwork_host = \"work\"\nroot = \"/Users/Shared/cc-workspaces/xshun\"\nruntime_root = \"/Users/Shared/cc-runtime/xshun\"\nshare = \"xshun\"\n";

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &std::path::Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    #[test]
    fn mount_sends_the_right_request_with_a_persistent_master() {
        let config = Config::parse(CONFIG).unwrap();
        let resolved = config.workspace("xshun").unwrap();
        let dir = std::env::temp_dir().join(format!("ccnm-home-mount-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));

        let fake = FakeRunner::new();
        let reply = MountReport {
            protocol: PROTOCOL,
            url: "//fodelf@home/xshun".into(),
            already_mounted: false,
            status: MountStatus {
                mounted: true,
                detail: "mounted (smbfs)".into(),
            },
        };
        fake.push(Output::exited(0, serde_json::to_string(&reply).unwrap()));

        let env = Env {
            runner: &fake,
            control_dir: control(&dir),
        };
        let rep = mount(&resolved, &env).unwrap();
        assert_eq!(rep, reply);
        assert!(control(&dir).is_dir());

        let call = &fake.calls()[0];
        let text = call.display();
        assert!(text.contains("ControlMaster=auto"), "{text}");
        assert!(
            text.contains("-T work ccnm work mount --payload "),
            "{text}"
        );
        let wire = call.args.last().unwrap().to_string_lossy().into_owned();
        let sent: MountRequest = crate::payload::decode(&wire).unwrap();
        assert_eq!(sent.share, "xshun");
        assert_eq!(sent.smb_user, "fodelf");
        assert_eq!(sent.home_alias, "ccnm-home");
        assert_eq!(
            sent.mountpoint,
            PathBuf::from("/Users/Shared/cc-workspaces/xshun")
        );
    }

    #[test]
    fn unmount_reports_unreachable_work() {
        let config = Config::parse(CONFIG).unwrap();
        let resolved = config.workspace("xshun").unwrap();
        let fake = FakeRunner::new();
        let mut down = Output::exited(255, "");
        down.stderr = b"ssh: connect to host work port 22: No route to host\n".to_vec();
        fake.push(down);
        let env = Env {
            runner: &fake,
            control_dir: control(std::path::Path::new("ccnm-home-unmount")),
        };
        let err = unmount(&resolved, &env).unwrap_err();
        assert_eq!(err.code(), ErrorCode::WorkUnreachable);
        assert!(err.message().contains("No route to host"), "{err}");
    }
}

//! Bounded candidate paths, metadata and OS access checks. Never read secrets.
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{Finding, PROBE_TIMEOUT, Severity};
use crate::error::{Error, ErrorCode, Result};
use crate::process::{Cmd, ProcessRunner, SystemRunner};
use crate::provider::AgentProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Missing,
    Inaccessible,
    Accessible,
    Unknown,
}

/// Symlinks (including dangling ones) and metadata errors are not absence.
/// `/bin/test -r` consults this process identity's OS permissions/ACL, without
/// opening a FIFO or reading a byte of the candidate file.
pub fn access(path: &Path, runner: &dyn ProcessRunner) -> Access {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Access::Unknown;
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let meta = match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Access::Missing,
            Err(_) => return Access::Unknown,
        };
        if meta.file_type().is_symlink()
            || (ancestor == path && !meta.is_file())
            || (ancestor != path && !meta.is_dir())
        {
            return Access::Unknown;
        }
    }
    match runner.run(
        &Cmd::new("/bin/test")
            .arg("-r")
            .arg(path)
            .timeout(PROBE_TIMEOUT),
    ) {
        Ok(out) if !out.timed_out && out.exit_code == Some(0) => Access::Accessible,
        Ok(out) if !out.timed_out && out.exit_code == Some(1) => Access::Inaccessible,
        _ => Access::Unknown,
    }
}

/// Snapshot only the locally declared path references; never enumerate homes.
pub fn findings(home: &Path, runner: &dyn ProcessRunner) -> Vec<Finding> {
    let refs = local_references();
    findings_with(home, &refs, runner)
}

pub fn local_references() -> Vec<(String, Option<OsString>)> {
    AgentProvider::ALL
        .iter()
        .map(|p| {
            (
                p.credentials().config_env.to_string(),
                std::env::var_os(p.credentials().config_env),
            )
        })
        .chain(std::iter::once((
            "XDG_CONFIG_HOME".into(),
            std::env::var_os("XDG_CONFIG_HOME"),
        )))
        .collect()
}

pub fn findings_with(
    home: &Path,
    references: &[(String, Option<OsString>)],
    runner: &dyn ProcessRunner,
) -> Vec<Finding> {
    let get = |name: &str| {
        references
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.as_ref())
    };
    AgentProvider::ALL.into_iter().map(|provider| {
        let metadata = provider.credentials();
        let name = format!("No {} credential", metadata.agent_name);
        let mut dirs = metadata.config_directories(home, get(metadata.config_env).map(|v| v.as_os_str()));
        dirs.extend(metadata.additional_directories.iter().map(|p| home.join(p)));
        if let (Some(rel), Some(xdg)) = (metadata.xdg_config_directory, get("XDG_CONFIG_HOME").filter(|p| !p.is_empty())) {
            dirs.push(PathBuf::from(xdg).join(rel));
        }
        let invalid = !home.is_absolute() || std::fs::metadata(home).is_err()
            || dirs.iter().any(|dir| !dir.is_absolute() || dir.components().any(|c| c == std::path::Component::ParentDir));
        let paths: BTreeSet<_> = dirs.into_iter().flat_map(|dir| metadata.files.iter().map(move |file| dir.join(file)))
            .chain(metadata.containers.iter().map(|p| home.join(p))).collect();
        let observed: Vec<_> = paths.iter().map(|path| access(path, runner)).collect();
        if invalid || observed.contains(&Access::Unknown) {
            Finding::fail(&name, "known credential accessibility is unknown (private paths and values withheld)",
                "verify the Runtime execution identity, local path references and OS permissions; unknown does not mean absent")
        } else if observed.contains(&Access::Accessible) {
            Finding::fail(&name, "the Runtime identity can access a known Agent credential file or container (private paths withheld)",
                "use a separate execution identity with OS-denied access; do not copy or remove the Agent's login to make an audit pass")
        } else {
            Finding::ok(&name, "known credential paths are absent or OS access is denied; other locations and credential services are not proved")
        }
    }).collect()
}

pub fn runtime_gate(home: &Path, runner: &dyn ProcessRunner) -> Result<()> {
    super::environment::validate_runtime_names(std::env::vars_os().map(|(k, _)| k))?;
    if findings(home, runner)
        .iter()
        .any(|f| f.severity == Severity::Fail)
    {
        return Err(Error::policy(
            "Runtime Agent credential access is present or unknown; exec refused before spawn (private paths and values withheld)",
        ));
    }
    Ok(())
}

pub fn effective_uid(runner: &dyn ProcessRunner) -> Result<u32> {
    let out = runner.run(&Cmd::new("/usr/bin/id").arg("-u").timeout(PROBE_TIMEOUT))?;
    if out.success()
        && let Ok(uid) = out.stdout_lossy().trim().parse()
    {
        return Ok(uid);
    }
    Err(Error::new(
        ErrorCode::Auth,
        "cannot establish the Agent execution identity",
    ))
}

pub fn private_home(path: &Path, files: &[&str]) -> Result<()> {
    private_home_for(path, files, effective_uid(&SystemRunner)?)
}

pub fn private_home_for(path: &Path, files: &[&str], uid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let refused = || {
        Error::new(
            ErrorCode::Auth,
            "dedicated Agent home must be private, owned by the execution identity and free of symlinks; independently log in with the official CLI on the Agent Node",
        )
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(refused());
    }
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(|_| refused())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(refused());
        }
    }
    let meta = std::fs::metadata(path).map_err(|_| refused())?;
    if meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(refused());
    }
    for file in files {
        match std::fs::symlink_metadata(path.join(file)) {
            Ok(meta)
                if meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.uid() == uid
                    && meta.mode() & 0o077 == 0 => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(refused()),
        }
    }
    Ok(())
}

//! Agent-local profile paths. Never serialize this registry into a wire DTO.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::identifier;
use crate::error::{Error, Result};
use crate::provider::AgentProvider;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfiles {
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    provider: AgentProvider,
    directory: PathBuf,
}

pub struct ResolvedProfile {
    provider: AgentProvider,
    directory: PathBuf,
}

impl ResolvedProfile {
    /// Agent-local consumption only; not part of AgentIdentity or binding.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Explicit local preflight, separate from parsing or planning. No login
    /// or credential content read; P3 must additionally probe the official CLI.
    pub fn validate_private_directory(&self) -> Result<()> {
        crate::safety::credentials::private_home(&self.directory, self.provider.credentials().files)
    }
}

pub(crate) fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| c == Component::ParentDir)
        && !path.as_os_str().as_encoded_bytes().contains(&0)
}

impl AgentProfiles {
    pub fn parse(text: &str) -> Result<Self> {
        // TOML errors can quote paths/values. Do not return the private source.
        let profiles: Self = toml::from_str(text).map_err(|_| {
            Error::config("invalid Agent-local profiles configuration (private content withheld)")
        })?;
        let mut directories = std::collections::BTreeSet::new();
        for (name, profile) in &profiles.profiles {
            identifier(name)?;
            if name == "default" || !absolute(&profile.directory) {
                return Err(Error::config(
                    "Agent-local profile cannot replace default or use a non-absolute/parent-traversing directory (path withheld)",
                ));
            }
            if !directories.insert(profile.directory.clone()) {
                return Err(Error::config(
                    "Agent-local profiles must not alias the same private directory",
                ));
            }
        }
        Ok(profiles)
    }

    pub(crate) fn load_local() -> Result<Self> {
        let path = crate::paths::agent_profiles_path()?;
        if !absolute(&path) {
            return Err(Error::config(
                "Agent-local profiles path must be absolute and unambiguous (value withheld)",
            ));
        }
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => {
                return Err(Error::config(
                    "cannot inspect Agent-local profiles configuration",
                ));
            }
            Ok(meta) => {
                use std::os::unix::fs::MetadataExt;
                let uid = crate::safety::credentials::effective_uid(&crate::SystemRunner)?;
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || meta.uid() != uid
                    || meta.mode() & 0o077 != 0
                {
                    return Err(Error::config(
                        "Agent-local profiles configuration must be a private regular file owned by this execution identity",
                    ));
                }
            }
        }
        let text = std::fs::read_to_string(path)
            .map_err(|_| Error::config("cannot read Agent-local profiles configuration"))?;
        Self::parse(&text)
    }

    pub(crate) fn resolve(
        &self,
        provider: AgentProvider,
        reference: &str,
        home: &Path,
        xdg: Option<&Path>,
    ) -> Result<ResolvedProfile> {
        identifier(reference)?;
        if !absolute(home)
            || xdg
                .filter(|p| !p.as_os_str().is_empty())
                .is_some_and(|p| !absolute(p))
        {
            return Err(Error::config(
                "Agent-local HOME/XDG path is invalid (value withheld)",
            ));
        }
        let directory = if reference == "default" {
            match provider {
                AgentProvider::Claude => home.join(".claude"),
                AgentProvider::Codex => {
                    crate::paths::codex_home_in(home, xdg.filter(|p| !p.as_os_str().is_empty()))
                }
            }
        } else {
            let profile = self.profiles.get(reference).ok_or_else(|| {
                Error::config("unknown Agent-local profile reference; no fallback to default")
            })?;
            if profile.provider != provider {
                return Err(Error::config(
                    "profile provider differs from the Agent instance",
                ));
            }
            if profile.directory == home.join(".claude")
                || profile.directory
                    == crate::paths::codex_home_in(home, xdg.filter(|p| !p.as_os_str().is_empty()))
            {
                return Err(Error::config(
                    "named profile must not alias an existing default; use the explicit default reference instead",
                ));
            }
            profile.directory.clone()
        };
        Ok(ResolvedProfile {
            provider,
            directory,
        })
    }
}

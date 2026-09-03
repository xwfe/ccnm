//! Where ccnm keeps its own files on this machine.
//!
//! The design doc fixes these as `~/.config/ccnm/config.toml` and
//! `~/.local/state/ccnm/`. That is the XDG layout, not macOS
//! `~/Library/Application Support`, so this module resolves XDG variables
//! itself instead of asking a platform-dirs crate that would pick the
//! Library path on a Mac.

use std::env;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// `$XDG_CONFIG_HOME/ccnm/config.toml`, defaulting to `~/.config/ccnm/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_path_in(
        &home_dir()?,
        env_path("XDG_CONFIG_HOME").as_deref(),
    ))
}

/// `$XDG_STATE_HOME/ccnm`, defaulting to `~/.local/state/ccnm`.
pub fn state_dir() -> Result<PathBuf> {
    Ok(state_dir_in(
        &home_dir()?,
        env_path("XDG_STATE_HOME").as_deref(),
    ))
}

pub(crate) fn config_path_in(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    xdg_or(xdg_config_home, home, ".config").join("ccnm/config.toml")
}

pub(crate) fn state_dir_in(home: &Path, xdg_state_home: Option<&Path>) -> PathBuf {
    xdg_or(xdg_state_home, home, ".local/state").join("ccnm")
}

/// XDG says a variable that is unset, empty, or relative must be ignored.
fn xdg_or(xdg: Option<&Path>, home: &Path, fallback: &str) -> PathBuf {
    match xdg {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(fallback),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf> {
    env_path("HOME").ok_or_else(|| {
        Error::config("HOME is not set, so ~/.config/ccnm/config.toml cannot be located")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_design_doc() {
        let home = Path::new("/Users/me");
        assert_eq!(
            config_path_in(home, None),
            PathBuf::from("/Users/me/.config/ccnm/config.toml")
        );
        assert_eq!(
            state_dir_in(home, None),
            PathBuf::from("/Users/me/.local/state/ccnm")
        );
    }

    #[test]
    fn absolute_xdg_override_wins() {
        let home = Path::new("/Users/me");
        assert_eq!(
            config_path_in(home, Some(Path::new("/opt/cfg"))),
            PathBuf::from("/opt/cfg/ccnm/config.toml")
        );
        assert_eq!(
            state_dir_in(home, Some(Path::new("/var/state"))),
            PathBuf::from("/var/state/ccnm")
        );
    }

    #[test]
    fn relative_xdg_override_is_ignored() {
        let home = Path::new("/Users/me");
        assert_eq!(
            config_path_in(home, Some(Path::new("cfg"))),
            PathBuf::from("/Users/me/.config/ccnm/config.toml")
        );
    }
}

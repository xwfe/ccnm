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

/// The config this machine is using, honouring `CCNM_CONFIG` exactly as
/// the CLI's `--config` does.
///
/// The MCP runtime reads its own machine's config to decide what the
/// runtime account is allowed to do, and it has to find the same file the
/// user would see from a shell. Two different answers to "which config"
/// is how a safety setting ends up applied to nothing.
pub fn effective_config_path() -> Result<PathBuf> {
    match env::var_os("CCNM_CONFIG").filter(|v| !v.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => config_path(),
    }
}

/// `$XDG_CONFIG_HOME/ccnm/config.toml`, defaulting to `~/.config/ccnm/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_path_in(
        &home_dir()?,
        env_path("XDG_CONFIG_HOME").as_deref(),
    ))
}

/// The three things that live under the state root, and nothing else.
///
/// ```text
/// ~/.local/state/ccnm/
/// ├── sessions/<session-id>/   one Claude session: its mcp.json, its
/// │                            settings, the output exec_command kept
/// ├── workspaces/<name>/       one project, for as long as it exists:
/// │                            metadata, the remote root, projected rules
/// └── cache/                   rebuildable, safe to delete
/// ```
///
/// Everything ccnm writes goes here. Not the user's project, and not
/// `~/.claude`: a tool that edits the developer's own Claude
/// configuration is a tool they cannot reason about (design doc section
/// 21), and one that leaves files in the repository shows up in their
/// `git status`.
///
/// The split is by lifetime. A session directory is finished when the
/// session is, and can be removed wholesale; a workspace directory
/// outlives any number of sessions.
pub fn sessions_dir(state: &Path) -> PathBuf {
    state.join("sessions")
}

/// One session's directory. `id` is filtered, not trusted: it names a
/// directory and arrives from another machine.
pub fn session_dir(state: &Path, id: &str) -> PathBuf {
    sessions_dir(state).join(safe_name(id, "session"))
}

pub fn workspaces_dir(state: &Path) -> PathBuf {
    state.join("workspaces")
}

/// One workspace's long-lived directory.
pub fn workspace_dir(state: &Path, name: &str) -> PathBuf {
    workspaces_dir(state).join(safe_name(name, "workspace"))
}

/// Rebuildable state. Nothing here is ever required.
pub fn cache_dir(state: &Path) -> PathBuf {
    state.join("cache")
}

/// A single path segment that can only ever be a single path segment.
///
/// Filtering rather than escaping: a `..` or a `/` cannot survive, so
/// there is no traversal to get right, and a name that was already safe
/// is unchanged.
pub fn safe_name(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(64)
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        fallback.to_string()
    } else {
        cleaned
    }
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

pub fn home_dir() -> Result<PathBuf> {
    env_path("HOME").ok_or_else(|| {
        Error::config("HOME is not set, so ~/.config/ccnm/config.toml cannot be located")
    })
}

/// What the remote login shell would make of a `~/...` path, so doctor
/// can look at the same file the other machine will invoke. Only a
/// leading `~/` (or bare `~`) is expanded; `~user/...` is left alone.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
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
    fn expand_home_only_touches_a_leading_tilde() {
        let home = Path::new("/Users/me");
        assert_eq!(
            expand_home("~/.local/bin/ccnm", home),
            PathBuf::from("/Users/me/.local/bin/ccnm")
        );
        assert_eq!(expand_home("~", home), PathBuf::from("/Users/me"));
        assert_eq!(expand_home("/opt/ccnm", home), PathBuf::from("/opt/ccnm"));
        assert_eq!(expand_home("~bob/ccnm", home), PathBuf::from("~bob/ccnm"));
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

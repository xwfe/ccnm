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

/// Agent-local registry of profile paths; never relative to CCNM_CONFIG.
pub fn agent_profiles_path() -> Result<PathBuf> {
    Ok(config_path()?.with_file_name("profiles.toml"))
}

pub fn codex_home() -> Result<PathBuf> {
    Ok(codex_home_in(
        &home_dir()?,
        env_path("XDG_CONFIG_HOME").as_deref(),
    ))
}

pub(crate) fn codex_home_in(home: &Path, xdg: Option<&Path>) -> PathBuf {
    config_path_in(home, xdg)
        .with_file_name("agents")
        .join("codex")
}

/// What lives under the state root, and nothing else.
///
/// ```text
/// ~/.local/state/ccnm/
/// ├── sessions/<session-id>/   one Claude session: its mcp.json, its
/// │                            settings, the output exec_command kept
/// ├── workspaces/<name>/       one project, for as long as it exists:
/// │                            metadata, the remote root, projected rules
/// ├── cache/                   rebuildable, safe to delete
/// └── controller.sock          the Agent Node's login-session
///                              controller, while it is running
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

/// Where `apply_patch` records a commit it is part way through.
///
/// Neither lifetime above. A journal exists for the few microseconds a
/// patch spends renaming files, and the one case it is written for is the
/// one where the process does not come back to delete it — so the next
/// patch has to find it even though it belongs to a session that is gone.
/// That rules out `sessions/`, and it is not workspace metadata either.
pub fn patches_dir(state: &Path) -> PathBuf {
    state.join("patches")
}

/// The controller's socket (see [`crate::controller`]).
///
/// Directly under the state root rather than in a subdirectory because
/// `sun_path` is only 104 bytes on macOS, and because it belongs to
/// neither lifetime above: it exists exactly while the controller is
/// running, which is neither a session nor a workspace.
pub fn controller_socket(state: &Path) -> PathBuf {
    state.join("controller.sock")
}

/// 64-bit FNV-1a: a stable file name for something that is not one.
///
/// Not a security boundary and not collision-free, so no caller may treat
/// equal names as equal inputs: a collision has to cost a shared file, never
/// a mix-up. The write guard's lock file shares (two roots would exclude each
/// other, the safe direction); the RPC start-key index keeps the original
/// strings in the file and compares them.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
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

/// `$XDG_CONFIG_HOME`, defaulting to `~/.config`: where `systemctl --user`
/// looks for the units an account installed itself (`systemd/user/`).
pub fn config_home() -> Result<PathBuf> {
    Ok(xdg_or(
        env_path("XDG_CONFIG_HOME").as_deref(),
        &home_dir()?,
        ".config",
    ))
}

pub(crate) fn config_path_in(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    xdg_or(xdg_config_home, home, ".config").join("ccnm/config.toml")
}

pub(crate) fn state_dir_in(home: &Path, xdg_state_home: Option<&Path>) -> PathBuf {
    xdg_or(xdg_state_home, home, ".local/state").join("ccnm")
}

/// The variables that moved this process's config or state off the
/// defaults, with the values it is honouring, for a process that will not
/// inherit them: the controller launchd starts (F8). Empty when every
/// location is the default.
///
/// `config` is the file this invocation was given (`--config`, which is
/// also how `CCNM_CONFIG` arrives), made absolute because launchd starts
/// the controller in `/`. An XDG value ccnm ignores is left out, so the
/// controller ignores the same thing this process did.
pub fn location_overrides(config: Option<&Path>) -> Result<Vec<(&'static str, PathBuf)>> {
    let mut vars = Vec::new();
    if let Some(config) = config {
        vars.push(("CCNM_CONFIG", std::path::absolute(config)?));
    }
    for name in ["XDG_CONFIG_HOME", "XDG_STATE_HOME"] {
        if let Some(dir) = env_path(name).filter(|dir| dir.is_absolute()) {
            vars.push((name, dir));
        }
    }
    Ok(vars)
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

/// What the account running this process can say about a directory it was
/// pointed at.
///
/// Four answers, because collapsing them is how F1 happened: `is_dir()`
/// says `false` both for a path that is not there and for one this account
/// may not look at, and on a Runtime Node the second is the normal case --
/// the project belongs to the Runtime Executor, the person typing is the
/// Operator, and Debian gives new accounts a 0700 home. P62 had `ccnm run`
/// refuse a project that was there, and doctor report `cannot stat`.
#[derive(Debug)]
pub enum Seen {
    Dir,
    /// Something is there and it is not a directory.
    NotDir,
    Missing,
    /// Permission denied on the way there. Says nothing about whether the
    /// directory exists: the account that owns it has to answer that.
    Hidden,
    /// Any other failure to look (an I/O error, a symlink loop).
    Unreadable(std::io::Error),
}

pub fn see_dir(path: &Path) -> Seen {
    use std::io::ErrorKind;
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Seen::Dir,
        Ok(_) => Seen::NotDir,
        Err(e) => match e.kind() {
            ErrorKind::NotFound => Seen::Missing,
            // A file where a parent directory should be.
            ErrorKind::NotADirectory => Seen::NotDir,
            ErrorKind::PermissionDenied => Seen::Hidden,
            _ => Seen::Unreadable(e),
        },
    }
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

    /// The four answers stay four. `Hidden` is the one `is_dir()` used to
    /// fold into "no": a directory behind a parent this account may not
    /// enter, which is every project in a 0700 home that is not its own.
    #[test]
    fn a_directory_behind_a_closed_door_is_hidden_not_missing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ccnm-paths-{}-seen", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _tidy = ccnm_testdir::TestDir::adopt(dir.clone());
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("proj")).unwrap();
        std::fs::write(dir.join("file"), b"x").unwrap();

        assert!(matches!(see_dir(&home.join("proj")), Seen::Dir));
        assert!(matches!(see_dir(&dir.join("file")), Seen::NotDir));
        assert!(matches!(see_dir(&dir.join("file/below")), Seen::NotDir));
        assert!(matches!(see_dir(&dir.join("nope")), Seen::Missing));

        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o000)).unwrap();
        let there = see_dir(&home.join("proj"));
        let absent = see_dir(&home.join("never-made"));
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        // root is never refused, so there is no closed door to look at.
        if matches!(there, Seen::Dir) {
            eprintln!("skipped: this account is not refused by directory permissions");
            return;
        }
        assert!(matches!(there, Seen::Hidden), "{there:?}");
        // And it cannot tell a project that is there from one that is not:
        // which is exactly why it must not answer.
        assert!(matches!(absent, Seen::Hidden), "{absent:?}");
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

//! tmux on the work machine: the thing that keeps an interactive Claude
//! alive when the terminal it was started from goes away (design doc
//! section 23).
//!
//! ```text
//! 家庭机 shell → ssh -t → work: tmux attach → claude → ssh stdio MCP → home
//! ```
//!
//! Two rules decide everything in this module.
//!
//! **The controller starts the server, nobody else.** A tmux server
//! inherits the security session of whoever forked it, and passes that on
//! to every process it starts. Measured on 2026-09-04: a server started
//! from an ssh session runs `Background`, and so does a Claude inside it,
//! which on a machine that keeps its credentials in the Keychain means a
//! logged-in machine reporting "not logged in" (section 21). Started by
//! the LaunchAgent controller, the server is `Aqua` and so is Claude. The
//! supervisor measures this from the inside and writes it down, so the
//! answer is a fact about the running session rather than an inference.
//!
//! **ccnm gets its own server**, `tmux -L ccnm`, never the user's default
//! one. A session must not disappear because someone typed `tmux
//! kill-server`, and ccnm must never be the reason someone's own tmux
//! dies.
//!
//! Behaviour verified against tmux 3.7c before this was written: a command
//! given as several arguments is executed directly, not through a shell
//! (so nothing here needs quoting), and the server puts itself in its own
//! process group, so `launchctl bootout` of the controller does not take
//! running sessions with it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, ErrorCode, Result};
use crate::paths;
use crate::process::Cmd;

/// `tmux -L ccnm`: ccnm's own server, separate from the user's.
pub const SOCKET: &str = "ccnm";

/// Every ccnm tmux session is named `ccnm-<workspace>`.
pub const PREFIX: &str = "ccnm-";

/// The tmux session environment variable holding the ccnm session id.
pub const SESSION_VAR: &str = "CCNM_SESSION";

/// Field separator for `list-sessions -F`. A tab cannot appear in a
/// session name (tmux replaces it) and none of the other fields are text.
const SEP: char = '\t';

const TIMEOUT: Duration = Duration::from_secs(20);

/// One session per workspace: `ccnm run` twice on the same workspace
/// attaches to what is already there instead of starting a second Claude
/// on the same project.
pub fn session_name(workspace: &str) -> String {
    // safe_name allows a dot; tmux reads `session.window` as an address,
    // so a workspace called `a.b` would name a session nobody can target.
    let safe = paths::safe_name(workspace, "workspace").replace('.', "-");
    format!("{PREFIX}{safe}")
}

/// The workspace a session name came from; `None` for a name ccnm did not
/// make. Best effort, for display: [`session_name`] is not reversible for
/// a workspace whose name needed cleaning. Code that must be sure computes
/// [`session_name`] from the workspace it wants and compares.
pub fn workspace_of(session: &str) -> Option<&str> {
    session.strip_prefix(PREFIX)
}

/// Find tmux the way [`crate::claude::locate`] finds Claude: the given
/// `PATH` first, then the usual install directories. launchd's `PATH` does
/// not have `/opt/homebrew/bin` in it, so the last two candidates are what
/// actually find it under the controller.
pub fn locate(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = path_var {
        candidates.extend(std::env::split_paths(path).map(|dir| dir.join("tmux")));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/tmux"));
    candidates.push(PathBuf::from("/usr/local/bin/tmux"));
    candidates.push(PathBuf::from("/usr/bin/tmux"));
    candidates
        .into_iter()
        .find(|p| crate::claude::is_executable(p))
}

pub fn locate_from_env() -> Option<PathBuf> {
    locate(std::env::var_os("PATH").as_deref())
}

/// The error every caller gives when tmux is not installed. Interactive
/// sessions are the only feature that needs it, so it names that feature
/// and the one command that fixes it.
pub fn missing() -> Error {
    Error::dependency(
        "tmux is not installed on the work machine, and an interactive session needs it to outlive the terminal that started it\non work: brew install tmux\n(`ccnm run <workspace> --print \"<prompt>\"` needs no tmux)",
    )
}

/// A located tmux binary. Every command goes to ccnm's own socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tmux {
    bin: PathBuf,
}

impl Tmux {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Tmux { bin: bin.into() }
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    fn base(&self) -> Cmd {
        Cmd::new(&self.bin).args(["-L", SOCKET]).timeout(TIMEOUT)
    }

    /// `tmux -V`, to prove the binary works before anything depends on it.
    pub fn version_cmd(&self) -> Cmd {
        Cmd::new(&self.bin).arg("-V").timeout(TIMEOUT)
    }

    /// Start `inner` detached, in a new session named `name`, tagged with
    /// the ccnm session id.
    ///
    /// `-d` returns as soon as the session exists, which is what makes
    /// this safe to run from the controller: the controller answers the
    /// request and goes back to listening while Claude runs on.
    ///
    /// `inner`'s arguments are passed straight through — tmux execs the
    /// command directly when it is given as several arguments — so a
    /// payload with `=` or `-` in it needs no quoting.
    ///
    /// The id rides in the session's own environment so that anything
    /// later — status, a second `ccnm run` — can get from a live tmux
    /// session back to its session directory without scanning for it.
    pub fn new_session_cmd(&self, name: &str, cwd: &Path, ccnm_session: &str, inner: &Cmd) -> Cmd {
        self.base()
            .args(["new-session", "-d", "-s", name])
            .arg("-c")
            .arg(cwd)
            .args(["-e", &format!("{SESSION_VAR}={ccnm_session}")])
            .arg(&inner.program)
            .args(&inner.args)
    }

    /// `tmux show-environment -t <name> CCNM_SESSION`.
    pub fn session_id_cmd(&self, name: &str) -> Cmd {
        self.base()
            .args(["show-environment", "-t", name, SESSION_VAR])
    }

    /// `tmux has-session`: exit 0 when the session is live.
    pub fn has_session_cmd(&self, name: &str) -> Cmd {
        self.base().args(["has-session", "-t", name])
    }

    /// The command that hands this terminal to Claude. Never given a
    /// timeout: it lasts as long as the person using it wants.
    pub fn attach_cmd(&self, name: &str) -> Cmd {
        Cmd::new(&self.bin).args(["-L", SOCKET, "attach-session", "-t", name])
    }

    pub fn kill_cmd(&self, name: &str) -> Cmd {
        self.base().args(["kill-session", "-t", name])
    }

    /// The server's own pid, which is the evidence that a session is
    /// backed by a process rather than by a name in a list.
    pub fn server_pid_cmd(&self) -> Cmd {
        self.base().args(["display-message", "-p", "#{pid}"])
    }

    pub fn list_cmd(&self) -> Cmd {
        self.base().args([
            "list-sessions",
            "-F",
            &format!(
                "#{{session_name}}{SEP}#{{session_created}}{SEP}#{{session_attached}}{SEP}#{{session_windows}}"
            ),
        ])
    }
}

/// One live tmux session, as `list-sessions` describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Live {
    pub name: String,
    /// Unix seconds.
    pub created: u64,
    /// How many terminals are attached right now.
    pub attached: u32,
    pub windows: u32,
}

impl Live {
    pub fn workspace(&self) -> Option<&str> {
        workspace_of(&self.name)
    }
}

/// Parse [`Tmux::list_cmd`] output. A line that does not fit the format is
/// skipped rather than failing the whole listing: this is a status
/// display, and one odd session must not hide the others.
pub fn parse_list(text: &str) -> Vec<Live> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split(SEP);
            let name = f.next()?.to_string();
            let created = f.next()?.parse().ok()?;
            let attached = f.next()?.parse().ok()?;
            let windows = f.next()?.parse().ok()?;
            Some(Live {
                name,
                created,
                attached,
                windows,
            })
        })
        .collect()
}

/// `CCNM_SESSION=<id>` out of `show-environment` output. `-CCNM_SESSION`
/// (tmux's way of saying "unset here") and anything else give `None`.
pub fn parse_session_id(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(&format!("{SESSION_VAR}=")))
        .map(str::to_string)
        .filter(|id| !id.is_empty())
}

/// "no server running on ..." is how tmux says "nothing is up", on stderr
/// with a non-zero exit. That is an empty list, not an error.
pub fn no_server(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("no server running") || s.contains("error connecting to")
}

/// The error for a session that is not there. `NotReady` rather than
/// `Internal`: nothing is broken, there is just nothing to attach to.
pub fn no_session(name: &str) -> Error {
    Error::new(
        ErrorCode::NotReady,
        format!(
            "no live session {name} on the work machine\nstart one: ccnm run {}",
            workspace_of(name).unwrap_or("<workspace>")
        ),
    )
}

/// Refuse to make a name tmux would interpret. Session names cannot
/// contain `.` or `:` (tmux uses them to address windows and panes), and
/// [`paths::safe_name`] already strips both — this is the assertion that
/// keeps it true if that ever changes.
pub fn check_name(name: &str) -> Result<()> {
    if name.contains(['.', ':']) || name.is_empty() {
        return Err(Error::internal(format!(
            "{name} is not usable as a tmux session name"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_derived_from_the_workspace_and_are_tmux_safe() {
        assert_eq!(session_name("xshun"), "ccnm-xshun");
        // safe_name replaces what tmux would read as an address.
        let odd = session_name("a.b:c/d");
        assert!(check_name(&odd).is_ok(), "{odd}");
        assert_eq!(workspace_of(&session_name("xshun")), Some("xshun"));
        assert_eq!(workspace_of("someone-elses"), None);
        assert!(check_name("has.dot").is_err());
        assert!(check_name("").is_err());
    }

    #[test]
    fn the_command_line_targets_ccnms_own_socket() {
        let tmux = Tmux::new("/opt/homebrew/bin/tmux");
        let inner = Cmd::new("/Users/me/.local/bin/ccnm").args([
            "internal",
            "supervise",
            "--payload",
            "eyJ4IjoxfQ",
        ]);
        let cmd = tmux.new_session_cmd("ccnm-xshun", Path::new("/tmp/ws"), "abc-123", &inner);
        let line = cmd.display();
        assert!(line.contains("-L ccnm"), "{line}");
        assert!(line.contains("new-session -d -s ccnm-xshun"), "{line}");
        assert!(line.contains("-c /tmp/ws"), "{line}");
        assert!(line.contains("-e CCNM_SESSION=abc-123"), "{line}");
        // The inner command survives whole, argument by argument.
        assert!(
            line.ends_with("/Users/me/.local/bin/ccnm internal supervise --payload eyJ4IjoxfQ"),
            "{line}"
        );
        assert!(tmux.attach_cmd("ccnm-xshun").display().contains("-L ccnm"));
    }

    #[test]
    fn list_output_parses_and_odd_lines_are_skipped() {
        let text = "ccnm-xshun\t1788496263\t0\t1\nccnm-other\t1788496264\t2\t3\ngarbage\n";
        let live = parse_list(text);
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].name, "ccnm-xshun");
        assert_eq!(live[0].created, 1_788_496_263);
        assert_eq!(live[0].attached, 0);
        assert_eq!(live[1].attached, 2);
        assert_eq!(live[1].windows, 3);
        assert_eq!(live[0].workspace(), Some("xshun"));
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn the_session_id_comes_back_out_of_the_tmux_environment() {
        assert_eq!(
            parse_session_id("CCNM_SESSION=0b4c7a1e-2d3f\n").as_deref(),
            Some("0b4c7a1e-2d3f")
        );
        // tmux prints `-VAR` for a variable that is unset in the session.
        assert_eq!(parse_session_id("-CCNM_SESSION\n"), None);
        assert_eq!(parse_session_id("CCNM_SESSION=\n"), None);
        assert_eq!(parse_session_id(""), None);
    }

    #[test]
    fn an_empty_server_is_not_an_error() {
        assert!(no_server("no server running on /tmp/tmux-501/ccnm"));
        assert!(no_server(
            "error connecting to /tmp/tmux-501/ccnm (No such file)"
        ));
        assert!(!no_server("session not found: ccnm-xshun"));
    }

    #[test]
    fn locate_prefers_the_path_and_falls_back_to_homebrew() {
        // Nothing on an empty PATH; the fallbacks are absolute, so on this
        // machine the answer is whatever is installed.
        let found = locate(None);
        if let Some(p) = found {
            assert!(p.ends_with("tmux"), "{}", p.display());
        }
    }
}

//! The work-controller: the process that runs inside the work machine's
//! **login session**, and the socket the ssh side reaches it through.
//!
//! # Why a second process at all
//!
//! An ssh session on macOS is not in the user's login session. It cannot
//! read the login Keychain, and Claude Code keeps its OAuth there. Measured
//! on the real work machine (macOS, Claude Code 2.1.259, 2026-09-03), the
//! same user running the same two commands two ways:
//!
//! ```text
//!                            launchctl managername   security(1)   claude
//! ssh session                Background              exit 36       "Not logged in · Please run /login"
//! LaunchAgent in gui/<uid>   Aqua                    exit 0        "OAuth session expired and could not be refreshed"
//! ```
//!
//! Two different diagnoses with two different fixes: the first says "log
//! in", which is wrong and would send the user round in circles, because
//! the machine *is* logged in. Only the second is the truth about the
//! machine. So ccnm never asks an ssh session about Claude — it asks the
//! controller, and if there is no controller it reports "not verified"
//! rather than a lie (design doc section 21).
//!
//! Starting Claude from the controller is the same story: a Claude started
//! from ssh would inherit the session that cannot see its own credentials.
//!
//! # Shape
//!
//! ```text
//! work machine, GUI login
//!   launchd gui/<uid>
//!     └── ccnm internal work-controller        LaunchAgent, so: Aqua
//!           └── listens on ~/.local/state/ccnm/controller.sock
//!
//! home machine ── ssh ──> work machine, ssh session (Background)
//!                           └── ccnm internal probe
//!                                 └── connect(controller.sock), one JSON line each way
//! ```
//!
//! The wire format is the control protocol of design doc section 8 minus
//! the base64: no shell parses this, so the JSON travels as-is, one line
//! per message, `protocol` checked on both ends.
//!
//! # What guards the socket
//!
//! File permissions, and nothing else. The socket is `0600` inside a `0700`
//! state directory, so only this Unix account can connect — the same
//! contract `SSH_AUTH_SOCK` runs on. That is exactly the right strength:
//! anyone who can connect could equally run `claude` themselves, since they
//! *are* the account. There is no token to steal because there is no token.
//!
//! # What this never does
//!
//! Read a credential. Not the Keychain, not `.credentials.json`, not even
//! to prove that it could — that proof is worth less than the invariant it
//! would break. Everything ccnm says about the login is a quote from
//! `claude auth status --json`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::claude::{self, ClaudeReport};
use crate::error::{Error, ErrorCode, ErrorReport, Reported, Result};
use crate::process::{Cmd, Output, ProcessRunner};
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::payload::{self, PROTOCOL, Protocol};
use crate::session::{self, SuperviseRequest};
use crate::tmux;

/// launchd label for the agent. Also the basename of its plist and what
/// every `launchctl` line in an error message names.
pub const LABEL: &str = "dev.ccnm.work-controller";

/// A message cannot be longer than this. A client that never sends a
/// newline must not be able to grow the controller's memory.
const MAX_MESSAGE: u64 = 64 * 1024;

/// How long to wait for the other end of an accepted connection. Long
/// enough for a slow disk, short enough that a wedged client cannot hold
/// the single-threaded loop.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// `sun_path` is 104 bytes on macOS and 108 on Linux. Bind fails with a
/// bare "invalid argument" when the path is longer, which is an hour of
/// confusion; this turns it into a sentence.
const MAX_SOCKET_PATH: usize = 100;

/// What the controller answers about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// Which build is listening, as whom. A long-lived agent easily ends
    /// up older than the binary on disk, and that has to be visible.
    pub hello: HelloReport,
    /// The listening process, so a stuck controller can be found without
    /// guessing which of the ccnm processes it is.
    pub pid: u32,
    /// `launchctl managername`: the security session this process is in.
    /// `Aqua` is the GUI login session; an ssh session reports
    /// `Background`. Only `Aqua` can reach the login Keychain.
    pub manager: Reported<String>,
}

impl Context {
    pub fn of(runner: &dyn ProcessRunner) -> Context {
        Context {
            hello: hello::answer(&HelloRequest::new(None)),
            pid: std::process::id(),
            manager: runner
                .run(&managername_cmd())
                .and_then(|out| parse_managername(&out))
                .map_err(Into::into),
        }
    }

    /// Whether this process is in the GUI login session, the only context
    /// where Claude can reach its credentials.
    ///
    /// Measured values are `Aqua` for a LaunchAgent in `gui/<uid>` and
    /// `Background` for an ssh session. `StandardIO`, `System` and
    /// `LoginWindow` also exist and are equally unable to prompt for
    /// Keychain access, so anything that is not `Aqua` is treated as not a
    /// login session.
    pub fn login_session(&self) -> bool {
        self.manager.as_deref() == Ok("Aqua")
    }

    /// One line for a doctor row.
    pub fn describe(&self) -> String {
        let manager = match &self.manager {
            Ok(name) => name.as_str(),
            Err(_) => "unknown session",
        };
        format!(
            "ccnm {} as {}, pid {}, {manager}",
            self.hello.ccnm_version, self.hello.user, self.pid
        )
    }
}

/// `launchctl managername`. Absolute path because a LaunchAgent's `PATH`
/// is whatever launchd hands it, not a login shell's.
pub fn managername_cmd() -> Cmd {
    Cmd::new("/bin/launchctl")
        .arg("managername")
        .timeout(Duration::from_secs(5))
}

/// A non-zero exit or empty output has to stay an error. Treating it as an
/// unknown session name would be harmless; treating it as "not Aqua" and
/// therefore as a diagnosis about the machine would not.
pub fn parse_managername(out: &Output) -> Result<String> {
    let name = out.stdout_lossy().trim().to_string();
    if !out.success() || name.is_empty() {
        return Err(Error::internal(format!(
            "launchctl managername failed (exit {:?}): {}",
            out.exit_code,
            out.stderr_lossy().trim()
        )));
    }
    Ok(name)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub body: RequestBody,
}

impl Request {
    pub fn new(body: RequestBody) -> Request {
        Request {
            protocol: PROTOCOL,
            body,
        }
    }
}

impl Protocol for Request {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case")]
pub enum RequestBody {
    /// Who is listening, and in which security session.
    Hello,
    /// `claude --version` and, unless the caller says otherwise,
    /// `claude auth status --json`, run here.
    ClaudeAuth {
        /// `CLAUDE_CONFIG_DIR` for the call, from the home machine's
        /// config. `None` means Claude's own default (design doc
        /// section 21).
        #[serde(default)]
        config_dir: Option<PathBuf>,
        /// How much to ask. A caller that has already seen this
        /// controller is *not* in a login session sends
        /// [`claude::Ask::VersionOnly`], because the login answer from
        /// here would be as worthless as the ssh session's.
        #[serde(default)]
        ask: claude::Ask,
    },
    /// Start the session whose directory this is. The one request that
    /// exists because of the login session: the process started here is
    /// Claude's ancestor, so Claude inherits it.
    Start { session_dir: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub body: ReplyBody,
}

impl Response {
    fn new(body: ReplyBody) -> Response {
        Response {
            protocol: PROTOCOL,
            body,
        }
    }

    fn error(err: &Error) -> Response {
        Response::new(ReplyBody::Error(err.into()))
    }
}

impl Protocol for Response {
    fn protocol(&self) -> u32 {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
pub enum ReplyBody {
    Hello(Context),
    Claude(ClaudeReport),
    /// The supervisor's pid. Claude is its child; the session directory's
    /// `exit` file says when it is done.
    Started {
        pid: u32,
    },
    /// The request was understood but could not be answered. Sent instead
    /// of hanging up, so the caller gets a code and a sentence rather than
    /// an unexplained EOF.
    Error(ErrorReport),
}

/// What the controller needs from its environment.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// The `claude` binary as found in *this* process's environment. The
    /// controller's `PATH` comes from launchd, so the lookup happens here
    /// rather than on the ssh side.
    pub claude: Option<PathBuf>,
    /// tmux, found the same way and for the same reason. Only interactive
    /// sessions need it; `None` is not an error until one is asked for.
    pub tmux: Option<PathBuf>,
    /// This binary, to run as the supervisor of each session.
    pub exe: PathBuf,
}

/// Answer one request. Pure apart from the commands it runs, so the
/// behaviour is testable without a socket.
pub fn answer(req: &Request, tools: &Tools<'_>) -> Response {
    match &req.body {
        RequestBody::Hello => Response::new(ReplyBody::Hello(Context::of(tools.runner))),
        RequestBody::ClaudeAuth { config_dir, ask } => {
            Response::new(ReplyBody::Claude(claude::report(
                tools.claude.as_deref(),
                config_dir.as_deref(),
                tools.runner,
                *ask,
            )))
        }
        RequestBody::Start { session_dir } => match start_session(session_dir, tools) {
            Ok(pid) => Response::new(ReplyBody::Started { pid }),
            Err(e) => Response::error(&e),
        },
    }
}

/// Start a session and return without waiting for it.
///
/// The directory must already hold a readable `session.json`: that is
/// what makes it a session rather than any path a client cares to name.
///
/// Print mode spawns the supervisor here. Interactive mode spawns tmux,
/// which spawns the supervisor — and *that* is the whole reason the
/// controller starts it: a tmux server hands its own security session to
/// everything it runs, so a server forked here is one Claude can read its
/// credentials in, and a server someone forked from an ssh session is not
/// (see [`crate::tmux`]).
fn start_session(session_dir: &Path, tools: &Tools<'_>) -> Result<u32> {
    let Some(claude_bin) = &tools.claude else {
        return Err(Error::new(
            ErrorCode::Version,
            "claude not found in the controller's environment; it looked in launchd's PATH, ~/.local/bin, ~/.claude/local, /usr/local/bin, /opt/homebrew/bin",
        ));
    };
    let dir = session::Dir::at(session_dir);
    let spec = session::load(&dir)?;
    let req = SuperviseRequest::new(session_dir.to_path_buf(), claude_bin.clone());
    let supervisor = supervisor_cmd(&tools.exe, &req)?;
    if !spec.mode.is_interactive() {
        let pid = spawn_detached(&supervisor, &dir.supervisor_log())?;
        tracing::info!(session = %spec.id, pid, "started supervisor");
        return Ok(pid);
    }

    let tmux = tmux::Tmux::new(tools.tmux.clone().ok_or_else(tmux::missing)?);
    let name = tmux::session_name(&spec.workspace);
    tmux::check_name(&name)?;
    // One session per workspace. Starting a second Claude on the same
    // project behind the same name is not something to resolve silently:
    // the caller attaches to what is there or stops it first.
    if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "a session named {name} is already running on this machine\nattach to it: ccnm attach {}\nor end it: ccnm stop {}",
                spec.workspace, spec.workspace
            ),
        ));
    }
    let out = tools
        .runner
        .run(&tmux.new_session_cmd(&name, &spec.cwd, &spec.id, &supervisor))?;
    if !out.success() {
        return Err(Error::internal(format!(
            "tmux new-session failed (exit {:?}): {}",
            out.exit_code,
            out.stderr_lossy().trim()
        )));
    }
    let pid = server_pid(&tmux, tools)?;
    tracing::info!(session = %spec.id, %name, server_pid = pid, "started tmux session");
    Ok(pid)
}

/// The tmux server's pid: proof that the session is backed by a process,
/// and the one number that identifies the server across every session.
fn server_pid(tmux: &tmux::Tmux, tools: &Tools<'_>) -> Result<u32> {
    let out = tools.runner.run(&tmux.server_pid_cmd())?;
    out.stdout_lossy()
        .trim()
        .parse()
        .map_err(|_| Error::internal("tmux did not report a server pid"))
}

/// `ccnm internal supervise --payload <SuperviseRequest>`.
pub fn supervisor_cmd(exe: &Path, req: &SuperviseRequest) -> Result<Cmd> {
    let wire = payload::encode(req)?;
    Ok(Cmd::new(exe).args(["internal", "supervise", "--payload", &wire]))
}

/// Start `cmd` in its own process group with its output in `log`, and
/// let it go.
///
/// Its own process group, because launchd kills the agent's whole group
/// when the agent is booted out — which `ccnm work-controller install`
/// does on every upgrade — and a session must not die of its controller
/// being replaced (design doc section 23). A thread waits on the child so
/// finished supervisors do not pile up as zombies; the wait is all it does.
fn spawn_detached(cmd: &Cmd, log: &Path) -> Result<u32> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let out = std::fs::File::create(log)?;
    let err = out.try_clone()?;
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .process_group(0);
    let mut child = command.spawn().map_err(|e| {
        Error::internal(format!("cannot spawn {}", cmd.program.to_string_lossy())).with_source(e)
    })?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// The controller's listening socket. Removes the socket file when
/// dropped; see [`Listener::bind`] for why that cannot be relied on.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

impl Listener {
    /// Claim `path`: refuse if another controller is live there, clear the
    /// socket if it is a corpse, then bind `0600`.
    ///
    /// The corpse case is the normal one, not an edge case. `launchctl
    /// bootout` and a logout both SIGTERM the agent, and a signalled
    /// process runs no destructor, so the socket file outlives it. If bind
    /// simply failed on an existing file, the controller would never come
    /// back after the first restart.
    ///
    /// Connecting is the only way to tell the two apart: the file says
    /// nothing about whether anyone is accepting.
    pub fn bind(path: &Path) -> Result<Listener> {
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(Error::config(format!(
                "socket path is {} bytes, over the {MAX_SOCKET_PATH} a unix socket allows: {}\nset XDG_STATE_HOME to something shorter",
                path.as_os_str().len(),
                path.display()
            )));
        }
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(Error::new(
                    ErrorCode::Policy,
                    format!(
                        "another work controller is already listening on {}\nask it instead of starting a second one, or stop it with: launchctl bootout gui/$(id -u)/{}",
                        path.display(),
                        crate::controller::LABEL
                    ),
                ));
            }
            // Nothing there yet: the normal first start.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Something is there but nobody answers: a socket left behind.
            Err(_) => {
                std::fs::remove_file(path).map_err(|e| {
                    Error::internal(format!(
                        "cannot clear the stale socket at {}",
                        path.display()
                    ))
                    .with_source(e)
                })?;
                tracing::debug!(path = %path.display(), "removed a stale controller socket");
            }
        }
        // A directory ccnm creates is its own, so it gets 0700. One that
        // already exists is the user's, and is left exactly as it is: the
        // lock that matters is the socket's own 0600 below, and silently
        // re-permissioning somebody's directory (or failing on /tmp, which
        // cannot be chmodded at all) is not a thing a tool should do to
        // get a socket open.
        if let Some(dir) = path.parent()
            && !dir.exists()
        {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let listener = UnixListener::bind(path).map_err(|e| {
            Error::internal(format!("cannot listen on {}", path.display())).with_source(e)
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!(path = %path.display(), "work controller listening");
        Ok(Listener {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept one connection and answer it.
    ///
    /// One at a time on purpose. Every request is short — two `claude`
    /// calls at worst — and a single-threaded loop has no shared state to
    /// get wrong. When something long-running arrives (starting Claude in
    /// a tmux session), it will start a child and return, not hold the
    /// loop.
    pub fn serve_one(&self, tools: &Tools<'_>) -> Result<()> {
        let (stream, _) = self.listener.accept().map_err(|e| {
            Error::internal(format!("cannot accept on {}", self.path.display())).with_source(e)
        })?;
        handle(stream, tools)
    }

    /// Answer requests until the listener itself fails.
    ///
    /// A bad connection is logged and dropped; only a broken listener ends
    /// the loop. A client that sends nonsense or hangs up mid-request must
    /// not take the controller down with it, because a Claude session on
    /// the other side depends on this process still being here.
    pub fn serve_forever(&self, tools: &Tools<'_>) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().map_err(|e| {
                Error::internal(format!("cannot accept on {}", self.path.display())).with_source(e)
            })?;
            if let Err(e) = handle(stream, tools) {
                tracing::warn!(error = %e, "controller dropped a connection");
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn handle(stream: UnixStream, tools: &Tools<'_>) -> Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let response = match read_message::<Request>(&stream) {
        Ok(req) => {
            tracing::debug!(?req.body, "controller request");
            answer(&req, tools)
        }
        Err(e) => Response::error(&e),
    };
    write_message(&stream, &response)
}

/// One JSON line in. Bounded, and a missing newline is an error rather
/// than a wait for more.
fn read_message<T: serde::de::DeserializeOwned + Protocol>(stream: &UnixStream) -> Result<T> {
    let mut line = Vec::new();
    // `Read::take` by UFCS: `stream.take(..)` would resolve to the
    // by-value `UnixStream` impl and fail to move out of the borrow.
    let bounded = Read::take(stream, MAX_MESSAGE);
    let read = BufReader::new(bounded).read_until(b'\n', &mut line)?;
    if read as u64 == MAX_MESSAGE && !line.ends_with(b"\n") {
        return Err(Error::invalid_args(format!(
            "message is longer than the {MAX_MESSAGE} byte limit"
        )));
    }
    payload::decode_json(&line)
}

fn write_message<T: Serialize>(mut stream: &UnixStream, value: &T) -> Result<()> {
    let json = payload::to_json(value)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Ask the controller one question.
///
/// `timeout` covers each read and write, not the whole exchange; a
/// `ClaudeAuth` runs two 20 second commands on the other side, so it needs
/// more than the default.
pub fn call(path: &Path, body: RequestBody, timeout: Duration) -> Result<ReplyBody> {
    let stream = UnixStream::connect(path).map_err(|e| not_listening(path, &e))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write_message(&stream, &Request::new(body))?;
    let response: Response = read_message(&stream)?;
    match response.body {
        ReplyBody::Error(report) => Err(report.into()),
        body => Ok(body),
    }
}

/// [`RequestBody::Hello`], typed: who is listening and in which session.
pub fn context(path: &Path) -> Result<Context> {
    match call(path, RequestBody::Hello, Duration::from_secs(10))? {
        ReplyBody::Hello(ctx) => Ok(ctx),
        other => Err(unexpected(&other)),
    }
}

/// [`RequestBody::ClaudeAuth`], typed. Up to two `claude` invocations
/// happen on the other side, each with its own 20 second timeout.
pub fn claude_auth(
    path: &Path,
    config_dir: Option<&Path>,
    ask: claude::Ask,
) -> Result<ClaudeReport> {
    let body = RequestBody::ClaudeAuth {
        config_dir: config_dir.map(Path::to_path_buf),
        ask,
    };
    match call(path, body, Duration::from_secs(60))? {
        ReplyBody::Claude(rep) => Ok(rep),
        other => Err(unexpected(&other)),
    }
}

/// [`RequestBody::Start`], typed: the supervisor's pid.
pub fn start(path: &Path, session_dir: &Path) -> Result<u32> {
    let body = RequestBody::Start {
        session_dir: session_dir.to_path_buf(),
    };
    match call(path, body, Duration::from_secs(20))? {
        ReplyBody::Started { pid } => Ok(pid),
        other => Err(unexpected(&other)),
    }
}

fn unexpected(body: &ReplyBody) -> Error {
    Error::new(
        ErrorCode::Version,
        format!("the controller answered a different request than the one asked: {body:?}"),
    )
}

/// The failure users will actually hit, so it says which of the two
/// situations this is and what to type.
///
/// `NotReady` rather than a FAIL code: a missing controller means nothing
/// about the work machine has been *disproven*. Doctor renders it as SKIP,
/// which blocks READY without claiming something is broken.
fn not_listening(path: &Path, err: &std::io::Error) -> Error {
    let (what, fix) = if err.kind() == std::io::ErrorKind::NotFound {
        (
            "no socket at",
            "install it on the work machine: ccnm work-controller install".to_string(),
        )
    } else {
        (
            "nothing is listening on",
            format!(
                "the controller is installed but not running; on the work machine: launchctl kickstart -k gui/$(id -u)/{LABEL}"
            ),
        )
    };
    Error::new(
        ErrorCode::NotReady,
        format!(
            "{what} {}\nthe work controller is the process that answers from the work machine's login session; an ssh session cannot read the login Keychain, so Claude's login cannot be checked without it\n{fix}",
            path.display()
        ),
    )
    .with_source(std::io::Error::new(err.kind(), err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};
    use std::thread;

    /// Socket paths are length-limited, so tests stay under /tmp rather
    /// than in a long temp_dir() path.
    fn socket(test: &str) -> PathBuf {
        let dir = PathBuf::from(format!("/tmp/ccnm-ctl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{test}.sock"));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn tools<'a>(fake: &'a FakeRunner, claude: bool) -> Tools<'a> {
        Tools {
            runner: fake,
            claude: claude.then(|| PathBuf::from("/opt/homebrew/bin/claude")),
            tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
            exe: PathBuf::from("/Users/me/.local/bin/ccnm"),
        }
    }

    #[test]
    fn start_refuses_without_claude_or_without_a_session() {
        let fake = FakeRunner::new();
        let req = Request::new(RequestBody::Start {
            session_dir: PathBuf::from("/nonexistent"),
        });
        let ReplyBody::Error(report) = answer(&req, &tools(&fake, false)).body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::Version);
        assert!(report.message.contains("claude not found"), "{report}");

        // With a claude but no session.json there, nothing is started.
        let ReplyBody::Error(report) = answer(&req, &tools(&fake, true)).body else {
            panic!("expected an error reply")
        };
        assert!(report.message.contains("session.json"), "{report}");
        assert!(fake.calls().is_empty(), "nothing ran");
    }

    /// A session directory with a real `session.json` in the given mode.
    fn session_dir(test: &str, mode: session::Mode) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccnm-ctl-sess-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = session::Spec {
            protocol: PROTOCOL,
            id: "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d".into(),
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/xshun"),
            home_alias: "xdwmbp".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            mode,
            timeout_secs: 0,
            cwd: dir.join("cwd"),
        };
        std::fs::write(
            session::Dir::at(&dir).meta(),
            serde_json::to_string(&spec).unwrap(),
        )
        .unwrap();
        dir
    }

    /// An interactive session must be started *by tmux*, from here: a tmux
    /// server hands its own security session to everything it runs, so the
    /// one forked by this process (the login session's) is the only one
    /// Claude can read its credentials in.
    #[test]
    fn an_interactive_session_is_started_through_tmux_with_the_supervisor_inside() {
        let dir = session_dir("interactive", session::Mode::Interactive { prompt: None });
        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "")); // has-session: not running
        fake.push(Output::exited(0, "")); // new-session
        fake.push(Output::exited(0, "4242\n")); // display-message: server pid

        let req = Request::new(RequestBody::Start {
            session_dir: dir.clone(),
        });
        let ReplyBody::Started { pid } = answer(&req, &tools(&fake, true)).body else {
            panic!("expected a started reply")
        };
        assert_eq!(pid, 4242, "the tmux server's pid is what comes back");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].display().contains("has-session -t ccnm-xshun"));
        let new = calls[1].display();
        assert!(
            new.contains("-L ccnm new-session -d -s ccnm-xshun"),
            "{new}"
        );
        assert!(new.contains("-e CCNM_SESSION=0b4c7a1e"), "{new}");
        assert!(
            new.contains("/Users/me/.local/bin/ccnm internal supervise --payload "),
            "{new}"
        );
    }

    /// Two Claudes on one project behind one name is not something to
    /// resolve by guessing.
    #[test]
    fn a_second_interactive_session_for_the_same_workspace_is_refused() {
        let dir = session_dir("second", session::Mode::Interactive { prompt: None });
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: already there
        let req = Request::new(RequestBody::Start { session_dir: dir });
        let ReplyBody::Error(report) = answer(&req, &tools(&fake, true)).body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::NotReady);
        assert!(report.message.contains("already running"), "{report}");
        assert!(report.message.contains("ccnm attach xshun"), "{report}");
        assert_eq!(fake.calls().len(), 1, "nothing was started");
    }

    #[test]
    fn without_tmux_an_interactive_session_says_how_to_get_it() {
        let dir = session_dir("notmux", session::Mode::Interactive { prompt: None });
        let fake = FakeRunner::new();
        let mut tools = tools(&fake, true);
        tools.tmux = None;
        let req = Request::new(RequestBody::Start { session_dir: dir });
        let ReplyBody::Error(report) = answer(&req, &tools).body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::Dependency);
        assert!(report.message.contains("brew install tmux"), "{report}");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn the_supervisor_is_this_binary_with_one_payload() {
        let req = SuperviseRequest::new(
            PathBuf::from("/Users/me/.local/state/ccnm/sessions/s1"),
            PathBuf::from("/opt/homebrew/bin/claude"),
        );
        let cmd = supervisor_cmd(Path::new("/Users/me/.local/bin/ccnm"), &req).unwrap();
        let text = cmd.display();
        assert!(
            text.starts_with("/Users/me/.local/bin/ccnm internal supervise --payload "),
            "{text}"
        );
        assert_eq!(cmd.args.len(), 4);
        let back: SuperviseRequest = payload::decode(cmd.args[3].to_str().unwrap()).unwrap();
        assert_eq!(back, req);
    }

    /// The real spawn, with `true` standing in for the supervisor: the
    /// child ends up in its own process group, its pid comes back, and the
    /// controller does not wait for it.
    #[test]
    fn spawn_detached_returns_at_once_and_reaps_later() {
        let dir = std::env::temp_dir().join(format!("ccnm-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let cmd = Cmd::new("/bin/sh").args(["-c", "echo started; sleep 0.2"]);
        let started = std::time::Instant::now();
        let pid = spawn_detached(&cmd, &log).unwrap();
        assert!(pid > 0);
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "spawn must not wait for the child"
        );
        // The reaper thread waits; give it the 200 ms, then the log is complete.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "started\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The property that keeps a running session alive across a
    /// controller restart: the child is not in this process's group, so
    /// launchd killing the agent's group on bootout does not reach it.
    #[test]
    fn a_detached_child_is_in_its_own_process_group() {
        let dir = std::env::temp_dir().join(format!("ccnm-pgid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cmd = Cmd::new("/bin/sleep").arg("1");
        let pid = spawn_detached(&cmd, &dir.join("log")).unwrap();

        let pgid_of = |pid: u32| -> String {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "pgid=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let child = pgid_of(pid);
        let ours = pgid_of(std::process::id());
        assert!(!child.is_empty(), "ps could not see the child");
        assert_ne!(child, ours, "the child shares this process's group");
        assert_eq!(
            child,
            pid.to_string(),
            "the child should lead its own group"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hello_reports_the_security_session_it_is_in() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "Aqua\n"));
        let rep = answer(&Request::new(RequestBody::Hello), &tools(&fake, false));
        let ReplyBody::Hello(ctx) = rep.body else {
            panic!("wrong reply")
        };
        assert_eq!(ctx.manager.as_deref(), Ok("Aqua"));
        assert!(ctx.login_session());
        assert!(ctx.describe().contains("Aqua"), "{}", ctx.describe());
        assert_eq!(fake.calls()[0].display(), "/bin/launchctl managername");

        // An ssh session, and anything else, is not a login session.
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "Background\n"));
        let ctx = Context::of(&fake);
        assert!(!ctx.login_session());
        assert!(ctx.describe().contains("Background"));
    }

    #[test]
    fn a_missing_launchctl_is_reported_not_guessed() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(127, ""));
        let ctx = Context::of(&fake);
        assert!(ctx.manager.is_err(), "{:?}", ctx.manager);
        assert!(
            !ctx.login_session(),
            "an unknown session must not pass as a login session"
        );
        assert!(ctx.describe().contains("unknown session"));
    }

    #[test]
    fn claude_auth_runs_both_commands_with_the_config_dir() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
        fake.push(Output::exited(0, r#"{"loggedIn":true,"email":"me@x"}"#));
        let req = Request::new(RequestBody::ClaudeAuth {
            config_dir: Some(PathBuf::from("/x/claude")),
            ask: claude::Ask::Everything,
        });
        let rep = answer(&req, &tools(&fake, true));
        let ReplyBody::Claude(claude) = rep.body else {
            panic!("wrong reply")
        };
        assert_eq!(claude.version, Ok("2.1.259".into()));
        assert!(claude.auth.unwrap().logged_in);
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        for call in &calls {
            assert!(
                call.env
                    .iter()
                    .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude"),
                "{}",
                call.display()
            );
        }
    }

    /// A caller that already knows this controller is in the wrong session
    /// asks for the version only, and `claude auth status` is not run.
    #[test]
    fn version_only_does_not_run_the_auth_command() {
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
        let req = Request::new(RequestBody::ClaudeAuth {
            config_dir: None,
            ask: claude::Ask::VersionOnly,
        });
        let rep = answer(&req, &tools(&fake, true));
        let ReplyBody::Claude(claude) = rep.body else {
            panic!("wrong reply")
        };
        assert_eq!(claude.version, Ok("2.1.259".into()));
        assert_eq!(claude.auth.unwrap_err().code(), ErrorCode::NotReady);
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{:?}", calls[0].display());
        assert!(
            !calls[0].display().contains("auth"),
            "{}",
            calls[0].display()
        );
    }

    /// An older caller's request has no `ask` field; the default must be
    /// the full question, or a controller in the right session would
    /// quietly stop reporting the login.
    #[test]
    fn a_request_without_ask_still_asks_everything() {
        let req: Request = serde_json::from_str(
            r#"{"protocol":1,"body":{"request":"claude-auth","config_dir":null}}"#,
        )
        .unwrap();
        assert_eq!(
            req.body,
            RequestBody::ClaudeAuth {
                config_dir: None,
                ask: claude::Ask::Everything
            }
        );
    }

    /// The round trip that matters: a client on one side, the loop on the
    /// other, JSON both ways.
    #[test]
    fn a_request_and_its_reply_cross_the_socket() {
        let path = socket("roundtrip");
        let listener = Listener::bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the socket must not be reachable by other accounts"
        );

        let served = thread::spawn(move || {
            let fake = FakeRunner::new();
            fake.push(Output::exited(0, "Aqua\n"));
            listener.serve_one(&tools(&fake, false)).unwrap();
            // Dropped here, at the end of the closure.
        });
        let ctx = context(&path).unwrap();
        assert_eq!(ctx.manager.as_deref(), Ok("Aqua"));
        assert_eq!(ctx.hello.ccnm_version, crate::VERSION);
        served.join().unwrap();
        assert!(!path.exists(), "the listener must clean up its socket");
    }

    #[test]
    fn a_stale_socket_is_reclaimed_but_a_live_one_is_not() {
        let path = socket("stale");
        // What a SIGTERMed controller leaves behind: the file, no listener.
        std::fs::write(&path, b"").unwrap();
        let listener = Listener::bind(&path).expect("a corpse must not block a restart");

        let err = Listener::bind(&path).expect_err("a live controller must not be replaced");
        assert_eq!(err.code(), ErrorCode::Policy);
        assert!(err.message().contains("already listening"), "{err}");
        assert!(err.message().contains(LABEL), "{err}");
        drop(listener);
    }

    #[test]
    fn a_dead_controller_and_a_missing_one_get_different_advice() {
        let path = socket("absent");
        let err = context(&path).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("no socket at"), "{err}");
        assert!(err.message().contains("work-controller install"), "{err}");

        std::fs::write(&path, b"").unwrap();
        let err = context(&path).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("nothing is listening"), "{err}");
        assert!(err.message().contains("kickstart"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// A client that speaks nonsense gets a coded answer, and the loop
    /// survives to serve the next one.
    #[test]
    fn garbage_is_answered_with_an_error_and_does_not_kill_the_loop() {
        let path = socket("garbage");
        let listener = Listener::bind(&path).unwrap();
        let served = thread::spawn(move || {
            let fake = FakeRunner::new();
            fake.push(Output::exited(0, "Aqua\n"));
            let tools = tools(&fake, false);
            listener.serve_one(&tools).unwrap();
            listener.serve_one(&tools).unwrap();
        });

        let stream = UnixStream::connect(&path).unwrap();
        write_message(&stream, &serde_json::json!({"nope": 1})).unwrap();
        let response: Response = read_message(&stream).unwrap();
        let ReplyBody::Error(report) = response.body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::Version);
        drop(stream);

        // Same listener, next client, still working.
        assert!(context(&path).is_ok());
        served.join().unwrap();
    }

    #[test]
    fn an_oversized_message_is_refused_before_it_is_parsed() {
        let path = socket("oversized");
        let listener = Listener::bind(&path).unwrap();
        let served = thread::spawn(move || {
            let fake = FakeRunner::new();
            listener.serve_one(&tools(&fake, false)).unwrap();
        });

        let stream = UnixStream::connect(&path).unwrap();
        // The peer stops reading at the cap and replies, so the tail of
        // this write goes nowhere. Without a timeout that is a deadlock.
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let huge = vec![b'x'; (MAX_MESSAGE + 1024) as usize];
        let _ = (&stream).write_all(&huge);
        let response: Response = read_message(&stream).unwrap();
        let ReplyBody::Error(report) = response.body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::InvalidArgs);
        assert!(report.message.contains("longer than"), "{report}");
        served.join().unwrap();
    }

    #[test]
    fn a_protocol_mismatch_is_a_version_error() {
        let path = socket("protocol");
        let listener = Listener::bind(&path).unwrap();
        let served = thread::spawn(move || {
            let fake = FakeRunner::new();
            listener.serve_one(&tools(&fake, false)).unwrap();
        });

        let stream = UnixStream::connect(&path).unwrap();
        write_message(
            &stream,
            &serde_json::json!({"protocol": PROTOCOL + 1, "body": {"request": "hello"}}),
        )
        .unwrap();
        let response: Response = read_message(&stream).unwrap();
        let ReplyBody::Error(report) = response.body else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code(), ErrorCode::Version);
        served.join().unwrap();
    }

    #[test]
    fn a_socket_path_too_long_for_the_os_says_so() {
        let long = PathBuf::from(format!("/tmp/{}/controller.sock", "x".repeat(120)));
        let err = Listener::bind(&long).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("unix socket allows"), "{err}");
    }
}

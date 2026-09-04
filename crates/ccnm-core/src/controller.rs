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
fn parse_managername(out: &Output) -> Result<String> {
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
    /// `claude --version` and `claude auth status --json`, run here.
    ClaudeAuth {
        /// `CLAUDE_CONFIG_DIR` for the call, from the home machine's
        /// config. `None` means Claude's own default (design doc
        /// section 21).
        #[serde(default)]
        config_dir: Option<PathBuf>,
    },
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
}

/// Answer one request. Pure apart from the commands it runs, so the
/// behaviour is testable without a socket.
pub fn answer(req: &Request, tools: &Tools<'_>) -> Response {
    match &req.body {
        RequestBody::Hello => Response::new(ReplyBody::Hello(Context::of(tools.runner))),
        // Everything, because this is the one place where the answer about
        // the login is worth having.
        RequestBody::ClaudeAuth { config_dir } => Response::new(ReplyBody::Claude(claude::report(
            tools.claude.as_deref(),
            config_dir.as_deref(),
            tools.runner,
            claude::Ask::Everything,
        ))),
    }
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

/// [`RequestBody::ClaudeAuth`], typed. Two `claude` invocations happen on
/// the other side, each with its own 20 second timeout.
pub fn claude_auth(path: &Path, config_dir: Option<&Path>) -> Result<ClaudeReport> {
    let body = RequestBody::ClaudeAuth {
        config_dir: config_dir.map(Path::to_path_buf),
    };
    match call(path, body, Duration::from_secs(60))? {
        ReplyBody::Claude(rep) => Ok(rep),
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
        }
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

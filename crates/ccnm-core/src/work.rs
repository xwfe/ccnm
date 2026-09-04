//! What `ccnm` does on the work machine when the home launcher calls it
//! over ssh: `probe` (read-only, for doctor) and `work-run` (create a
//! session, have the controller start it, wait for the result).
//!
//! This code runs in an **ssh session**, which is not the login session.
//! Anything that needs the login session — asking Claude about its
//! credentials, starting it — is forwarded to [`crate::controller`]
//! rather than done here. Everything else (writing the session files,
//! waiting, reading the output) is done here: same account, same disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::claude::{self, ClaudeReport};
use crate::controller;
use crate::error::{Error, ErrorCode, ErrorReport, Reported, Result};
use crate::mcp;
use crate::paths;
use crate::process::ProcessRunner;
use crate::protocol::PROTOCOL;
use crate::protocol::hello::{self, HelloReport, HelloRequest};
use crate::protocol::mcp::{ProbeReport as McpProbeReport, ServePayload};
use crate::protocol::payload;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::protocol::run::{
    AttachRequest, PurgeReport, PurgeRequest, ResultReport, ResultRequest, RunReport, RunRequest,
    StartReport, StartRequest, StatusReport, StatusRequest, StopReport, StopRequest,
};
use crate::protocol::{self};
use crate::session::{self, Mode, Spec};
use crate::ssh::{Master, Ssh};
use crate::tmux;

/// What the work-side code needs from its environment. Injected so tests
/// can script every external command and decide whether `claude` exists.
pub struct Tools<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// This machine's state root; sessions and workspace dirs go under it.
    pub state: PathBuf,
    /// Where ControlPath sockets live on this machine.
    pub control_dir: PathBuf,
    /// The `claude` binary, if [`claude::locate`] found one. Only used as
    /// a fallback for the version when no controller is running; the
    /// controller finds its own, in launchd's environment.
    pub claude: Option<PathBuf>,
    /// The controller's socket on this machine.
    pub controller: PathBuf,
    /// tmux, for interactive sessions. Found in *this* (ssh) environment,
    /// which is enough to talk to a server the controller started: the
    /// socket belongs to the user, not to a session.
    pub tmux: Option<PathBuf>,
}

impl Tools<'_> {
    /// tmux or the one error that says how to get it.
    fn tmux(&self) -> Result<tmux::Tmux> {
        self.tmux
            .clone()
            .map(tmux::Tmux::new)
            .ok_or_else(tmux::missing)
    }
}

/// Grace beyond the session's own timeout before giving up on the `exit`
/// file. The supervisor kills Claude at the session timeout and writes
/// the file right after; if that has not happened this much later, the
/// supervisor itself is gone.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// Start a print-mode session and wait for its result.
///
/// Refuses without a controller in a login session — the same rule doctor
/// applies, for the same reason: a Claude started from anywhere else
/// cannot read its own credentials, and the failure it would produce
/// ("not logged in") is a lie about the machine.
pub fn run(req: &RunRequest, tools: &Tools<'_>) -> Result<RunReport> {
    let ctx = controller::context(&tools.controller)?;
    if !ctx.login_session() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "the work controller answers from {}, not from a login session, so a Claude it started could not read its credentials\nrun on work: ccnm work-controller install",
                ctx.describe()
            ),
        ));
    }
    let ssh = Ssh::new(&req.home_alias, &tools.control_dir)?.with_ccnm_bin(&req.home_ccnm_bin);
    greet(&ssh, &req.workspace, &req.root, tools)?;
    let cwd = paths::workspace_dir(&tools.state, &req.workspace);
    std::fs::create_dir_all(&cwd)?;
    let spec = Spec {
        protocol: PROTOCOL,
        id: session::new_id(),
        workspace: req.workspace.clone(),
        root: req.root.clone(),
        home_alias: req.home_alias.clone(),
        home_ccnm_bin: req.home_ccnm_bin.clone(),
        claude_config_dir: req.claude_config_dir.clone(),
        permission_mode: req.permission_mode,
        mode: Mode::Print {
            prompt: req.prompt.clone(),
        },
        timeout_secs: req.timeout_secs,
        cwd,
    };
    let dir = session::create(&tools.state, &spec, &ssh)?;
    let pid = controller::start(&tools.controller, dir.path())?;
    let outcome =
        session::wait_for_outcome(&dir, Duration::from_secs(req.timeout_secs) + EXIT_GRACE)?;

    let stdout = std::fs::read(dir.stdout()).unwrap_or_default();
    let result = claude::parse_print(&stdout).ok();
    let stdout_tail = if result.is_some() {
        String::new()
    } else {
        tail(&stdout)
    };
    let stderr_tail = tail(&std::fs::read(dir.stderr()).unwrap_or_default());
    Ok(RunReport {
        protocol: PROTOCOL,
        session: spec.id,
        session_dir: dir.path().to_path_buf(),
        controller: ctx,
        pid,
        outcome,
        result,
        stdout_tail,
        stderr_tail,
    })
}

/// Start an interactive session, or report the one that is already there.
///
/// Unlike [`run`] this returns as soon as the session exists: the session
/// outlives the ssh call that made it, which is the whole point of putting
/// it in tmux (design doc section 23). What comes back is what the home
/// machine needs to attach.
pub fn start(req: &StartRequest, tools: &Tools<'_>) -> Result<StartReport> {
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    tmux::check_name(&name)?;

    // Already up: attach to that, do not start a second Claude on the same
    // project. This path needs no controller, which is deliberate -- being
    // put back into a running session must not depend on the controller
    // being healthy right now.
    if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
        let session = live_session_id(&tmux, tools, &name);
        let dir = session
            .as_ref()
            .map(|id| paths::session_dir(&tools.state, id));
        // A session's root is fixed when it starts: it is in the payload
        // the MCP transport was spawned with, and nothing can repoint it.
        // So a live session whose root is not the one being asked for is
        // working somewhere the config no longer names -- and if that path
        // has since been moved away, one where every tool fails for
        // reasons that sound like something else.
        //
        // The old session is not what was asked for, so it is ended and a
        // new one started. Not refused: "your session is wrong, fix it
        // yourself" is a worse answer than doing the obvious thing and
        // saying so. What it must not do is silently hand back the stale
        // one, which is how an afternoon disappears.
        let stale_root = dir
            .as_ref()
            .map(session::Dir::at)
            .and_then(|d| session::load(&d).ok())
            .map(|spec| spec.root)
            .filter(|root| root != &req.root);
        if let Some(old) = stale_root {
            tracing::info!(
                old = %old.display(),
                new = %req.root.display(),
                "replacing a session bound to a different root"
            );
            // Before the kill, not after. The handshake talks to the other
            // machine, so it can fail for reasons that have nothing to do
            // with this workspace -- a link that blinked, a version that
            // does not match. Doing it afterwards means a blip ends a
            // running Claude, fails to start its replacement, and hands
            // back an error about version numbers to somebody who has
            // just lost their conversation.
            let (ctx, ssh) = preflight(req, tools)?;
            let out = tools.runner.run(&tmux.kill_cmd(&name))?;
            if !out.success() {
                return Err(Error::internal(format!(
                    "the running session for {} works in {}, which is not this workspace's root any more, and it could not be ended: {}",
                    req.workspace,
                    old.display(),
                    out.stderr_lossy().trim()
                )));
            }
            return start_fresh(req, tools, &tmux, name, ctx, ssh, Some(old));
        }
        let context = dir
            .as_ref()
            .and_then(|path| session::read_context(&session::Dir::at(path)));
        return Ok(StartReport {
            protocol: PROTOCOL,
            session,
            session_dir: dir,
            tmux_session: name,
            server_pid: server_pid(&tmux, tools)?,
            already_running: true,
            replaced: None,
            controller: None,
            context,
        });
    }

    let (ctx, ssh) = preflight(req, tools)?;
    start_fresh(req, tools, &tmux, name, ctx, ssh, None)
}

/// One round trip to the workspace machine before a session is built, to
/// answer the two questions that are cheap now and expensive later.
///
/// **Do the two binaries agree?** They have to be the same build: the
/// control protocol is versioned but the tools are not, so two builds
/// that still decode each other's messages can disagree about what a tool
/// does. `doctor` has always checked this, but `doctor` is what somebody
/// runs when they already suspect something. A session started against a
/// mismatched pair fails later, somewhere that does not mention versions.
///
/// **Is the project still there?** A moved or renamed root used to be
/// found out from inside the session, where the failure arrives as a
/// tool blaming the program it could not run. It costs one `stat` here.
///
/// One SSH round trip, paid once per session, to buy the two errors that
/// are worst to debug from the far end.
///
/// Measured on the real pair: **430-490 ms**, five runs, which is a whole
/// SSH handshake and not a round trip on an open connection. An earlier
/// version of this comment claimed 30 ms; that would be the cost with a
/// master already up, and nothing on this path ever starts one.
///
/// `Master::Off`, not `Reuse`. Nothing on this path ever *creates* a
/// master, so a ControlPath could only be used if some other command left
/// one lying around -- while the 104-byte `sun_path` limit it must fit
/// inside applies every time. Requiring it would mean a state directory
/// too long for that limit could no longer start a session at all, which
/// it always could before: the MCP transport sets `ControlPath=none` and
/// never had one. Refusing to work in order to be able to reuse something
/// that is usually not there is the wrong way round.
fn greet(ssh: &Ssh, workspace: &str, root: &Path, tools: &Tools<'_>) -> Result<()> {
    let hello: HelloReport = ssh.call_ccnm(
        tools.runner,
        Master::Off,
        &["internal", "hello"],
        &HelloRequest::new(Some(root.to_path_buf())),
        Duration::from_secs(30),
        ErrorCode::HomeUnreachable,
    )?;
    if hello.ccnm_version != crate::VERSION {
        return Err(Error::new(
            ErrorCode::Version,
            format!(
                "the workspace machine runs ccnm {}, this one runs {}; install the same build on both before starting a session",
                hello.ccnm_version,
                crate::VERSION
            ),
        ));
    }
    match hello.root {
        Some(status) if status.is_ok() => Ok(()),
        Some(status) => Err(Error::new(
            ErrorCode::WrongWorkspace,
            format!(
                "workspace `{workspace}` says its root is {}, and on that machine it is {}\nif the project moved: ccnm ws add {workspace} <new path> --replace",
                root.display(),
                status.describe()
            ),
        )),
        // No answer at all, from something calling itself the same
        // version. That is the case the version numbers cannot catch:
        // `VERSION` is the Cargo version, so every build of 0.1.0 compares
        // equal to every other, and during development different builds
        // carrying the same number is the normal state rather than the
        // exception. A missing field is the one piece of hard evidence
        // available that the two are not the same binary.
        None => Err(Error::new(
            ErrorCode::Version,
            format!(
                "the workspace machine reports ccnm {} like this one, but its reply is missing the project-root check, so the two are not the same build\ninstall this build there: scripts/deploy.sh <its alias>",
                hello.ccnm_version
            ),
        )),
    }
}

/// Everything that has to be true before a session can be built, checked
/// before anything is created *or destroyed*.
///
/// Order matters twice over. The controller is local and costs nothing,
/// so it goes first: no point spending a network round trip to find out
/// the LaunchAgent is not installed. And both of them come before the
/// `tmux kill` that replaces a stale session, because a handshake failing
/// afterwards would mean somebody's Claude was ended and not replaced,
/// for a reason -- a link that blinked, a version that does not match --
/// that has nothing to do with the session they just lost.
fn preflight(req: &StartRequest, tools: &Tools<'_>) -> Result<(controller::Context, Ssh)> {
    let ctx = controller::context(&tools.controller)?;
    if !ctx.login_session() {
        return Err(Error::new(
            ErrorCode::NotReady,
            format!(
                "the work controller answers from {}, not from a login session, so a Claude it started could not read its credentials\nrun on work: ccnm work-controller install",
                ctx.describe()
            ),
        ));
    }
    let ssh = Ssh::new(&req.home_alias, &tools.control_dir)?.with_ccnm_bin(&req.home_ccnm_bin);
    greet(&ssh, &req.workspace, &req.root, tools)?;
    Ok((ctx, ssh))
}

/// Create the session and have the controller start it. `replaced` is the
/// root of the session this one is taking over from, when there was one.
fn start_fresh(
    req: &StartRequest,
    tools: &Tools<'_>,
    _tmux: &tmux::Tmux,
    name: String,
    ctx: controller::Context,
    ssh: Ssh,
    replaced: Option<PathBuf>,
) -> Result<StartReport> {
    let cwd = paths::workspace_dir(&tools.state, &req.workspace);
    std::fs::create_dir_all(&cwd)?;
    let spec = Spec {
        protocol: PROTOCOL,
        id: session::new_id(),
        workspace: req.workspace.clone(),
        root: req.root.clone(),
        home_alias: req.home_alias.clone(),
        home_ccnm_bin: req.home_ccnm_bin.clone(),
        claude_config_dir: req.claude_config_dir.clone(),
        permission_mode: req.permission_mode,
        mode: Mode::Interactive {
            prompt: req.prompt.clone(),
        },
        // Not used interactively: nothing kills this session on a clock.
        timeout_secs: 0,
        cwd,
    };
    let dir = session::create(&tools.state, &spec, &ssh)?;
    let server_pid = controller::start(&tools.controller, dir.path())?;
    Ok(StartReport {
        protocol: PROTOCOL,
        session: Some(spec.id),
        session_dir: Some(dir.path().to_path_buf()),
        tmux_session: name,
        server_pid,
        already_running: false,
        replaced,
        controller: Some(ctx),
        // The supervisor writes this from inside tmux a moment from now;
        // `ccnm status` is where it shows up.
        context: wait_for_context(&dir),
    })
}

/// Give the supervisor a moment to record which security session it is in.
///
/// Bounded and best effort: this is evidence for a status line, and a
/// session that is up must not be reported as failed because a `launchctl`
/// call was slow.
fn wait_for_context(dir: &session::Dir) -> Option<session::Context> {
    const WAIT: Duration = Duration::from_secs(5);
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        if let Some(measured) = session::read_context(dir) {
            return Some(measured);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Hand this process's terminal to the workspace's session.
///
/// Runs under `ssh -t`, so "this process's terminal" is the one on the
/// home machine. Returns tmux's own exit code: 0 both when the person
/// detaches and when Claude ends.
pub fn attach(req: &AttachRequest, tools: &Tools<'_>) -> Result<i32> {
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    if !tools.runner.run(&tmux.has_session_cmd(&name))?.success() {
        return Err(tmux::no_session(&name));
    }
    let captured = crate::process::run_attached(&tmux.attach_cmd(&name))?;
    Ok(captured.exit_code.unwrap_or(1))
}

/// End the workspace's session: tmux kills the supervisor, which kills
/// Claude, which drops the ssh transport its MCP server was on.
pub fn stop(req: &StopRequest, tools: &Tools<'_>) -> Result<StopReport> {
    let tmux = tools.tmux()?;
    let name = tmux::session_name(&req.workspace);
    let out = tools.runner.run(&tmux.kill_cmd(&name))?;
    let stderr = out.stderr_lossy();
    if !out.success() && !tmux::no_server(&stderr) && !stderr.contains("can't find session") {
        return Err(Error::internal(format!(
            "tmux kill-session failed (exit {:?}): {}",
            out.exit_code,
            stderr.trim()
        )));
    }
    Ok(StopReport {
        protocol: PROTOCOL,
        tmux_session: name,
        killed: out.success(),
    })
}

/// Every live ccnm session on this machine, with what is known about each.
pub fn status(req: &StatusRequest, tools: &Tools<'_>) -> StatusReport {
    let (tmux_version, sessions) = match tools.tmux() {
        Err(e) => (Err(e.into()), Vec::new()),
        Ok(tmux) => {
            let version = tools
                .runner
                .run(&tmux.version_cmd())
                .and_then(|out| {
                    if out.success() {
                        Ok(out.stdout_lossy().trim().replace("tmux ", ""))
                    } else {
                        Err(Error::dependency(format!(
                            "tmux -V failed: {}",
                            out.stderr_lossy().trim()
                        )))
                    }
                })
                .map_err(Into::into);
            (
                version,
                live_sessions(&tmux, tools, req.workspace.as_deref()),
            )
        }
    };
    StatusReport {
        protocol: PROTOCOL,
        tmux: tmux_version,
        sessions,
    }
}

fn live_sessions(
    tmux: &tmux::Tmux,
    tools: &Tools<'_>,
    only: Option<&str>,
) -> Vec<protocol::run::LiveSession> {
    let wanted = only.map(tmux::session_name);
    let Ok(out) = tools.runner.run(&tmux.list_cmd()) else {
        return Vec::new();
    };
    tmux::parse_list(&out.stdout_lossy())
        .into_iter()
        .filter(|live| wanted.as_ref().is_none_or(|name| &live.name == name))
        .map(|live| {
            let session = live_session_id(tmux, tools, &live.name);
            let dir = session
                .as_ref()
                .map(|id| session::Dir::at(paths::session_dir(&tools.state, id)));
            let context = dir.as_ref().and_then(session::read_context);
            let tools_up = dir.as_ref().and_then(|dir| transport_alive(dir, tools));
            protocol::run::LiveSession {
                workspace: live.workspace().map(str::to_string),
                tmux_session: live.name,
                session,
                created: live.created,
                attached: live.attached,
                context,
                tools: tools_up,
            }
        })
        .collect()
}

/// Delete ccnm's own bookkeeping for a workspace: the session records and
/// the directory Claude ran in.
///
/// **Never the project.** The root is the one thing here ccnm did not
/// create, and it is not even looked at. Everything removed is under this
/// machine's `~/.local/state/ccnm`.
pub fn purge(req: &PurgeRequest, tools: &Tools<'_>) -> PurgeReport {
    let mut removed = Vec::new();
    let mut sessions = Vec::new();

    let dir = paths::sessions_dir(&tools.state);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let session_dir = session::Dir::at(entry.path());
            let Ok(spec) = session::load(&session_dir) else {
                continue;
            };
            if spec.workspace != req.workspace {
                continue;
            }
            if std::fs::remove_dir_all(session_dir.path()).is_ok() {
                removed.push(session_dir.path().display().to_string());
                sessions.push(spec.id);
            }
        }
    }

    let workspace_dir = paths::workspace_dir(&tools.state, &req.workspace);
    if workspace_dir.is_dir() && std::fs::remove_dir_all(&workspace_dir).is_ok() {
        removed.push(workspace_dir.display().to_string());
    }

    PurgeReport {
        protocol: PROTOCOL,
        removed,
        sessions,
    }
}

/// What a session produced, for a caller that was not there when it
/// finished.
pub fn result(req: &ResultRequest, tools: &Tools<'_>) -> Result<ResultReport> {
    let (id, dir, started) = match &req.session {
        Some(id) => {
            let dir = session::Dir::at(paths::session_dir(&tools.state, id));
            if !dir.meta().is_file() {
                return Err(Error::new(
                    ErrorCode::NotReady,
                    format!("no session {id} on this machine"),
                ));
            }
            let started = started_at(&dir);
            (id.clone(), dir, started)
        }
        None => newest_session(&tools.state, &req.workspace)?,
    };
    let spec = session::load(&dir)?;
    let stdout = std::fs::read(dir.stdout()).unwrap_or_default();
    let result = claude::parse_print(&stdout).ok();
    Ok(ResultReport {
        protocol: PROTOCOL,
        session: id,
        session_dir: dir.path().to_path_buf(),
        mode: if spec.mode.is_interactive() {
            "interactive".into()
        } else {
            "print".into()
        },
        started,
        outcome: session::read_outcome(&dir)?,
        result,
        stdout_tail: tail(&stdout),
        stderr_tail: tail(&std::fs::read(dir.stderr()).unwrap_or_default()),
    })
}

/// The workspace's most recent **print** session, by when its directory
/// was made.
///
/// Print only, because that is what this command is for. An interactive
/// session's output went to a terminal as it happened and there is nothing
/// stored to hand back; naming one explicitly still works, and says
/// "still running" or how it ended.
fn newest_session(state: &Path, workspace: &str) -> Result<(String, session::Dir, u64)> {
    let sessions = paths::sessions_dir(state);
    let mut best: Option<(String, session::Dir, u64)> = None;
    let entries = std::fs::read_dir(&sessions).map_err(|e| {
        Error::new(
            ErrorCode::NotReady,
            format!("no sessions on this machine yet ({})", sessions.display()),
        )
        .with_source(e)
    })?;
    for entry in entries.flatten() {
        let dir = session::Dir::at(entry.path());
        let Ok(spec) = session::load(&dir) else {
            continue;
        };
        if spec.workspace != workspace || spec.mode.is_interactive() {
            continue;
        }
        let started = started_at(&dir);
        if best.as_ref().is_none_or(|(_, _, best)| started > *best) {
            best = Some((spec.id, dir, started));
        }
    }
    best.ok_or_else(|| {
        Error::new(
            ErrorCode::NotReady,
            format!(
                "no `--print` session for workspace {workspace} on this machine\nan interactive session prints to its own terminal; `ccnm attach {workspace}` goes back to it"
            ),
        )
    })
}

/// Unix seconds the session directory was created, or 0 if that cannot be
/// read. Only used for ordering and display.
fn started_at(dir: &session::Dir) -> u64 {
    std::fs::metadata(dir.meta())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Is this session's MCP transport still running?
///
/// The transport is one ssh, started by Claude from the session's
/// `mcp.json`, and it is every tool the model has. When it dies Claude
/// does not restart it: the terminal keeps working, the model keeps
/// answering, and it quietly has nothing to reach the project with — the
/// worst kind of failure, because it looks like a working session. So
/// `ccnm status` looks for the process by the exact payload that session's
/// `mcp.json` names, which is unique to it.
///
/// `None` means the question could not be answered (no mcp.json, `ps`
/// unavailable), never "no".
fn transport_alive(dir: &session::Dir, tools: &Tools<'_>) -> Option<bool> {
    let payload = transport_payload(dir)?;
    let out = tools
        .runner
        .run(&crate::process::Cmd::new("/bin/ps").args(["-Awwo", "command="]))
        .ok()?;
    Some(out.stdout_lossy().contains(&payload))
}

/// The `--payload` argument out of a session's `mcp.json`.
fn transport_payload(dir: &session::Dir) -> Option<String> {
    let text = std::fs::read_to_string(dir.mcp_config()).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let args = config
        .pointer(&format!("/mcpServers/{}/args", mcp::server::SERVER_NAME))?
        .as_array()?;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg.as_str() == Some("--payload") {
            return it.next()?.as_str().map(str::to_string);
        }
    }
    None
}

/// The ccnm session id a live tmux session was tagged with.
fn live_session_id(tmux: &tmux::Tmux, tools: &Tools<'_>, name: &str) -> Option<String> {
    let out = tools.runner.run(&tmux.session_id_cmd(name)).ok()?;
    out.success()
        .then(|| tmux::parse_session_id(&out.stdout_lossy()))
        .flatten()
}

fn server_pid(tmux: &tmux::Tmux, tools: &Tools<'_>) -> Result<u32> {
    let out = tools.runner.run(&tmux.server_pid_cmd())?;
    Ok(out.stdout_lossy().trim().parse().unwrap_or(0))
}

/// The last 2 KiB, on a character boundary. Enough to see why, never the
/// whole thing: the whole thing is in the session directory.
fn tail(bytes: &[u8]) -> String {
    const KEEP: usize = 2048;
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= KEEP {
        return text.into_owned();
    }
    let mut start = text.len() - KEEP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}

/// Everything doctor wants to know about this machine, in one round trip.
/// Read-only: no master connection, no file written. The MCP handshake
/// starts a server on the home runtime and shuts it down again before
/// returning (design doc section 4).
pub fn probe(req: &ProbeRequest, tools: &Tools<'_>) -> ProbeReport {
    let (home_ssh, home_hello, mcp) = match Ssh::new(&req.home_alias, &tools.control_dir)
        .map(|ssh| ssh.with_ccnm_bin(&req.home_ccnm_bin))
    {
        Err(e) => (
            Err(e.into()),
            Err(Error::new(
                ErrorCode::HomeUnreachable,
                "not attempted: home alias is invalid",
            )
            .into()),
            None,
        ),
        Ok(ssh) => {
            let home_ssh = ssh.resolve(tools.runner).map_err(Into::into);
            let home_hello = ssh
                .check_control_path()
                .and_then(|()| {
                    ssh.call_ccnm::<_, HelloReport>(
                        tools.runner,
                        Master::Reuse,
                        &["internal", "hello"],
                        &HelloRequest::new(Some(req.root.clone())),
                        Duration::from_secs(30),
                        ErrorCode::HomeUnreachable,
                    )
                })
                .map_err(Into::into);
            // Only worth the round trips if the plain reverse ssh worked.
            let mcp = (req.mcp_calls > 0 && home_hello.is_ok())
                .then(|| mcp_handshake(req, &ssh).map_err(Into::into));
            (home_ssh, home_hello, mcp)
        }
    };

    let (controller, claude) = ask_about_claude(tools, req.claude_config_dir.as_deref());
    ProbeReport {
        protocol: PROTOCOL,
        hello: hello::answer(&HelloRequest::new(None)),
        controller: Some(controller),
        claude,
        home_ssh,
        home_hello,
        mcp,
        // Read-only, like everything else here: tmux is asked its version
        // and which sessions exist, and nothing is started or stopped.
        terminal: Some(status(
            &StatusRequest {
                protocol: PROTOCOL,
                workspace: Some(req.workspace.clone()),
            },
            tools,
        )),
    }
}

/// Claude's login state, from the only context whose answer means
/// anything.
///
/// With a controller, everything about Claude comes from it: not just the
/// login but the binary and version too, because the controller's `PATH`
/// is launchd's, and that is the `claude` a session would really start.
///
/// Without one, the version is still worth reporting — it needs no
/// credential — but the login is left as `CCNM_E_NOT_READY`. This session
/// *can* run `claude auth status`; the point is that its answer would be
/// wrong, and a wrong row sends the user to log in on a machine that is
/// already logged in.
fn ask_about_claude(
    tools: &Tools<'_>,
    config_dir: Option<&Path>,
) -> (Reported<controller::Context>, ClaudeReport) {
    match controller::context(&tools.controller) {
        Ok(ctx) => {
            // A controller that is not in a login session is asked only
            // for the version. Its answer about the login would be no
            // better than this session's, and the rule holds everywhere:
            // do not run a command whose result has to be thrown away.
            let ask = if ctx.login_session() {
                claude::Ask::Everything
            } else {
                claude::Ask::VersionOnly
            };
            let claude = controller::claude_auth(&tools.controller, config_dir, ask)
                .unwrap_or_else(|e| ClaudeReport {
                    path: None,
                    version: Err((&e).into()),
                    auth: Err(e.into()),
                });
            (Ok(ctx), claude)
        }
        Err(missing) => {
            let mut claude = claude::report(
                tools.claude.as_deref(),
                config_dir,
                tools.runner,
                claude::Ask::VersionOnly,
            );
            claude.auth = Err(ErrorReport::new(
                ErrorCode::NotReady,
                format!(
                    "not checked: no work controller to ask, and this ssh session's answer would be wrong\n{}",
                    missing.message()
                ),
            ));
            (Err(missing.into()), claude)
        }
    }
}

fn mcp_handshake(req: &ProbeRequest, ssh: &Ssh) -> Result<McpProbeReport> {
    let wire = payload::encode(&ServePayload::new(
        &req.workspace,
        req.root.clone(),
        &format!("probe-{}", uuid::Uuid::new_v4().hyphenated()),
    ))?;
    let cmd = ssh.mcp_transport_cmd(&wire)?;
    mcp::probe::probe(
        &cmd,
        req.mcp_calls,
        Duration::from_secs(30) + Duration::from_millis(500) * req.mcp_calls,
        ErrorCode::HomeUnreachable,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::process::{Cmd, FakeRunner, Output};
    use crate::protocol::hello::PathStatus;

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

    fn hello_json(root_ok: bool) -> String {
        let rep = HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from("/Users/ccrun/.local/bin/ccnm")),
            root: Some(PathStatus {
                exists: root_ok,
                is_dir: root_ok,
            }),
        };
        serde_json::to_string(&rep).unwrap()
    }

    fn request() -> ProbeRequest {
        ProbeRequest {
            protocol: PROTOCOL,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/ccrun/Projects/xshun"),
            home_alias: "ccnm-home".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: Some(PathBuf::from("/x/claude")),
            mcp_calls: 0,
        }
    }

    /// A socket path no controller is on, so the probe takes the
    /// no-controller branch.
    fn absent_socket(test: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/ccnm-absent-{}-{test}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn probe_collects_every_fact_in_one_report() {
        let dir = temp("probe");
        let fake = FakeRunner::new();
        // Call order: ssh -G, ssh internal hello, claude --version. No
        // `claude auth status`: with no controller its answer would be
        // wrong, so it is not asked at all.
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        fake.push(Output::exited(0, "2.1.259 (Claude Code)\n"));

        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
            tmux: None,
            controller: absent_socket("probe"),
        };
        let rep = probe(&request(), &tools);

        assert_eq!(rep.hello.ccnm_version, crate::VERSION);
        assert_eq!(rep.home_ssh.as_ref().unwrap().target(), "ccrun@home.ts");
        let home = rep.home_hello.as_ref().unwrap();
        assert_eq!(home.user, "ccrun");
        assert!(home.root.unwrap().is_ok());
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.claude.auth.as_ref().unwrap_err().code(),
            ErrorCode::NotReady,
            "an unaskable login must not be reported as logged out"
        );
        assert_eq!(
            rep.controller
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .code(),
            ErrorCode::NotReady
        );
        assert_eq!(rep.mcp, None, "mcp_calls = 0 means no handshake");

        let calls = fake.calls();
        assert_eq!(
            calls.len(),
            3,
            "{:?}",
            calls.iter().map(Cmd::display).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].display(), "ssh -G ccnm-home");
        let reverse = calls[1].display();
        assert!(
            reverse.contains("ControlMaster=no"),
            "doctor path must not start a master: {reverse}"
        );
        assert!(
            reverse.contains("-T ccnm-home ~/.local/bin/ccnm internal hello --payload"),
            "{reverse}"
        );
        // The hello asked the home side to look at the workspace root.
        let wire = calls[1].args.last().unwrap().to_string_lossy().into_owned();
        let sent: HelloRequest = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(
            sent.root,
            Some(PathBuf::from("/Users/ccrun/Projects/xshun"))
        );
        assert!(
            calls[2]
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
        let back: ProbeReport = crate::protocol::payload::decode_json(&json).unwrap();
        assert_eq!(back, rep);
    }

    #[test]
    fn probe_records_failures_instead_of_aborting() {
        let dir = temp("probe-fail");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        let mut unreachable = Output::exited(255, "");
        unreachable.stderr =
            b"ssh: connect to host home.ts port 22: Operation timed out\n".to_vec();
        fake.push(unreachable);

        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: absent_socket("probe-fail"),
        };
        let rep = probe(
            &ProbeRequest {
                mcp_calls: 5,
                ..request()
            },
            &tools,
        );
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::HomeUnreachable);
        assert!(err.message.contains("Operation timed out"));
        assert_eq!(rep.mcp, None, "no MCP attempt after a failed hello");
        assert_eq!(rep.claude.path, None);
        assert_eq!(rep.claude.version.unwrap_err().code(), ErrorCode::Version);
        assert_eq!(fake.calls().len(), 2, "no claude calls without a binary");
    }

    #[test]
    fn missing_home_binary_is_a_version_error_naming_the_path() {
        let dir = temp("probe-127");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\n"));
        fake.push(Output::exited(127, ""));
        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: absent_socket("probe-127"),
        };
        let rep = probe(&request(), &tools);
        let err = rep.home_hello.unwrap_err();
        assert_eq!(err.code(), ErrorCode::Version);
        assert!(err.message.contains("~/.local/bin/ccnm"), "{}", err.message);
    }

    fn run_request(prompt: &str) -> RunRequest {
        RunRequest {
            protocol: PROTOCOL,
            workspace: "fixture".into(),
            root: PathBuf::from("/Users/bing/ccnm-fixture"),
            home_alias: "xdwmbp".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::AcceptEdits,
            prompt: prompt.into(),
            timeout_secs: 5,
        }
    }

    /// Two builds that still decode each other's control messages can
    /// still disagree about what a tool does, so a session is not built
    /// until both sides say the same version. `doctor` has always
    /// checked this; `doctor` is not what somebody runs before they
    /// suspect anything.
    #[test]
    fn a_session_is_not_started_against_a_different_build() {
        let dir = temp("greet-version");
        let fake = FakeRunner::new();
        let mut other = HelloReport {
            protocol: PROTOCOL,
            ccnm_version: crate::VERSION.to_string(),
            user: "ccrun".into(),
            platform: "macos/aarch64".into(),
            exe: None,
            root: Some(PathStatus {
                exists: true,
                is_dir: true,
            }),
        };
        other.ccnm_version = format!("{}-and-a-half", crate::VERSION);
        fake.push(Output::exited(0, serde_json::to_string(&other).unwrap()));
        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: dir.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        let err = greet(&ssh, "fixture", Path::new("/Users/bing/fixture"), &tools)
            .expect_err("a mismatched pair must not get a session");
        assert_eq!(err.code(), ErrorCode::Version);
        // Both versions, because "install the same build" is useless
        // without knowing which two builds are in play.
        assert!(err.message().contains(&other.ccnm_version), "{err}");
        assert!(err.message().contains(crate::VERSION), "{err}");
    }

    /// A running session must not be destroyed by a check that can fail
    /// for reasons of its own. The handshake talks to the other machine;
    /// if it ran after the kill, a link that blinked would end somebody's
    /// Claude, fail to start its replacement, and hand back an error
    /// about version numbers to a person who has just lost their
    /// conversation.
    #[test]
    fn a_stale_session_is_not_killed_until_the_handshake_has_passed() {
        let dir = temp("stale-greet");
        let socket = PathBuf::from(format!("/tmp/ccnm-sg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let id = "5f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";
        let sdir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(sdir.path()).unwrap();
        let spec = Spec {
            protocol: PROTOCOL,
            id: id.into(),
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/somewhere-else"),
            home_alias: "home".into(),
            home_ccnm_bin: "ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            mode: Mode::Interactive { prompt: None },
            timeout_secs: 0,
            cwd: dir.clone(),
        };
        std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();

        // A controller that answers, so the preflight gets past it and
        // the failure under test really is the handshake.
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Aqua\n"));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: None,
                tmux: None,
                exe: PathBuf::from("/nonexistent"),
            };
            listener.serve_one(&tools).unwrap();
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        fake.push(Output::exited(255, "")); // the handshake: link is down

        let mut tools = tmux_tools(&fake, &dir, "stale-greet");
        tools.controller = socket.clone();
        let err = start(&start_request(), &tools).expect_err("the handshake failed");
        served.join().unwrap();
        let _ = std::fs::remove_file(&socket);

        assert_eq!(err.code(), ErrorCode::HomeUnreachable, "{err}");
        let ran: Vec<String> = fake.calls().iter().map(|cmd| cmd.display()).collect();
        assert!(
            !ran.iter().any(|line| line.contains("kill-session")),
            "a failed handshake must not have ended the session: {ran:?}"
        );
    }

    /// A state directory too long for macOS's 104-byte `sun_path` could
    /// always start a session -- the MCP transport names no socket. The
    /// handshake must not change that, so it names no socket either: on
    /// this path nothing ever creates a master, so a ControlPath could
    /// only be used if some other command happened to leave one, while
    /// the length limit would apply every single time.
    #[test]
    fn a_state_directory_too_long_for_a_socket_still_starts_a_session() {
        let deep = std::env::temp_dir().join("x".repeat(crate::ssh::CONTROL_PATH_MAX_LEN));
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            runner: &fake,
            state: deep.clone(),
            control_dir: deep.join("control"),
            claude: None,
            tmux: None,
            controller: deep.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        greet(&ssh, "fixture", Path::new("/Users/bing/fixture"), &tools)
            .expect("a long state directory is not a reason to refuse a session");

        let line = fake.calls()[0].display();
        assert!(line.contains("ControlPath=none"), "{line}");
        assert!(line.contains("ControlMaster=no"), "{line}");
        // And the path that would not fit never appears.
        assert!(!line.contains(&deep.display().to_string()), "{line}");
    }

    /// A project that moved used to be found out from inside the session,
    /// where it arrives as a tool blaming the program it could not run.
    /// One stat before anything starts, and the message says how to
    /// repoint the workspace.
    #[test]
    fn a_root_that_is_gone_is_refused_before_a_session_exists() {
        let dir = temp("greet-root");
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, hello_json(false)));
        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: dir.join("nope.sock"),
        };
        let ssh = Ssh::new("xdwmbp", &tools.control_dir).unwrap();
        let err = greet(&ssh, "fixture", Path::new("/Users/bing/moved"), &tools)
            .expect_err("a missing root must not get a session");
        assert_eq!(err.code(), ErrorCode::WrongWorkspace);
        assert!(err.message().contains("/Users/bing/moved"), "{err}");
        assert!(err.message().contains("missing"), "{err}");
        assert!(err.message().contains("ccnm ws add fixture"), "{err}");
    }

    fn start_request() -> StartRequest {
        StartRequest {
            protocol: PROTOCOL,
            workspace: "xshun".into(),
            root: PathBuf::from("/Users/bing/xshun"),
            home_alias: "ccnm-home".into(),
            home_ccnm_bin: "~/.local/bin/ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            prompt: None,
        }
    }

    fn tmux_tools<'a>(fake: &'a FakeRunner, dir: &Path, test: &str) -> Tools<'a> {
        Tools {
            runner: fake,
            state: dir.to_path_buf(),
            control_dir: control(dir),
            claude: None,
            tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
            controller: absent_socket(test),
        }
    }

    /// `ccnm run` on a workspace that already has a session means "put me
    /// back into it". That must not need the controller: being let back in
    /// cannot depend on the component that starts things being healthy.
    #[test]
    fn start_on_a_live_session_reports_it_without_asking_the_controller() {
        let dir = temp("start-live");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.context(),
            r#"{"manager":"Background","keychain":true}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n"))); // show-environment
        fake.push(Output::exited(0, "4242\n")); // display-message

        let rep = start(&start_request(), &tmux_tools(&fake, &dir, "start-live")).unwrap();
        assert!(rep.already_running);
        assert_eq!(rep.tmux_session, "ccnm-xshun");
        assert_eq!(rep.session.as_deref(), Some(id));
        assert_eq!(rep.server_pid, 4242);
        assert_eq!(
            rep.context
                .as_ref()
                .map(session::Context::describe)
                .as_deref(),
            Some("Background, keychain reachable")
        );
        assert!(rep.controller.is_none(), "no controller was needed");
        assert!(
            rep.summary().contains("already running"),
            "{}",
            rep.summary()
        );
    }

    /// With nothing running, starting one needs the controller, and the
    /// same login-session rule print mode has: a Claude started anywhere
    /// else cannot read its own credentials.
    #[test]
    fn start_without_a_controller_creates_nothing() {
        let dir = temp("start-none");
        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "")); // has-session: nothing there
        let err = start(&start_request(), &tmux_tools(&fake, &dir, "start-none")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    /// A session's root is fixed when it starts. Being handed back into
    /// one that works somewhere else is how a moved project turns into an
    /// hour of tools failing for reasons that sound like something else --
    /// so the stale one is ended and a new one started, and the report
    /// says which root was left behind.
    #[test]
    fn start_replaces_a_live_session_bound_to_a_different_root() {
        let dir = temp("moved-root");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let sdir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(sdir.path()).unwrap();
        let spec = Spec {
            protocol: PROTOCOL,
            id: id.into(),
            workspace: "xshun".into(),
            // Where it was when it started.
            root: PathBuf::from("/Users/bing/xshun"),
            home_alias: "home".into(),
            home_ccnm_bin: "ccnm".into(),
            claude_config_dir: None,
            permission_mode: crate::config::PermissionMode::default(),
            mode: Mode::Interactive { prompt: None },
            timeout_secs: 0,
            cwd: dir.clone(),
        };
        std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "")); // has-session: live
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));

        // The request says somewhere else -- the project was moved.
        let mut req = start_request();
        req.root = PathBuf::from("/Users/bing/moved/xshun");
        // No controller, so the preflight fails. The stale session is
        // still running afterwards: nothing is torn down until a
        // replacement is known to be possible. This used to kill first
        // and discover the problem second, which cost the person their
        // conversation and gave them an unrelated error for it.
        let err = start(&req, &tmux_tools(&fake, &dir, "moved-root")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady, "{err}");
        let ran: Vec<String> = fake.calls().iter().map(|cmd| cmd.display()).collect();
        assert!(
            !ran.iter().any(|line| line.contains("kill-session")),
            "nothing may be killed before the preflight passes: {ran:?}"
        );
    }

    /// The same rule print mode has, and for the same reason: a Claude
    /// started outside the login session cannot read its own credentials,
    /// and the failure it produces is a lie about the machine.
    #[test]
    fn start_refuses_a_controller_outside_the_login_session() {
        let dir = temp("start-bg");
        let socket = PathBuf::from(format!("/tmp/ccnm-ws-bg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Background\n"));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                tmux: Some(PathBuf::from("/opt/homebrew/bin/tmux")),
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(1, "")); // has-session: nothing running
        let mut tools = tmux_tools(&fake, &dir, "start-bg");
        tools.controller = socket;
        let err = start(&start_request(), &tools).unwrap_err();
        served.join().unwrap();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("Background"), "{err}");
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn without_tmux_every_interactive_command_says_how_to_get_it() {
        let dir = temp("no-tmux");
        let fake = FakeRunner::new();
        let mut tools = tmux_tools(&fake, &dir, "no-tmux");
        tools.tmux = None;
        let err = start(&start_request(), &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Dependency);
        assert!(err.message().contains("brew install tmux"), "{err}");

        let stop_err = stop(
            &StopRequest {
                protocol: PROTOCOL,
                workspace: "xshun".into(),
            },
            &tools,
        )
        .unwrap_err();
        assert_eq!(stop_err.code(), ErrorCode::Dependency);

        // Status reports it as a row rather than failing: it is a status
        // command, and "tmux is not installed" is the status.
        let rep = status(
            &StatusRequest {
                protocol: PROTOCOL,
                workspace: None,
            },
            &tools,
        );
        assert!(rep.sessions.is_empty());
        assert!(
            rep.render().contains("brew install tmux"),
            "{}",
            rep.render()
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn stopping_what_is_not_running_is_not_an_error() {
        let dir = temp("stop-none");
        let fake = FakeRunner::new();
        // Exactly what tmux 3.7c says, on stderr, with exit 1.
        fake.push(Output {
            stderr: b"can't find session: ccnm-xshun\n".to_vec(),
            ..Output::exited(1, "")
        });
        let rep = stop(
            &StopRequest {
                protocol: PROTOCOL,
                workspace: "xshun".into(),
            },
            &tmux_tools(&fake, &dir, "stop-none"),
        )
        .unwrap();
        assert!(!rep.killed);
        assert_eq!(rep.tmux_session, "ccnm-xshun");
    }

    /// Status is about live sessions, and what it says about each one is
    /// measured, not assumed: the security session comes from the file the
    /// supervisor wrote from inside it.
    #[test]
    fn status_lists_live_sessions_with_what_was_measured_about_them() {
        let dir = temp("status");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.context(),
            r#"{"manager":"Background","keychain":true}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.mcp_config(),
            r#"{"mcpServers":{"ccnm":{"command":"/usr/bin/ssh","args":["-T","home","ccnm","internal","mcp-serve","--payload","eyJwIjoxfQ"]}}}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "tmux 3.7c\n")); // -V
        fake.push(Output::exited(
            0,
            "ccnm-xshun\t1788496263\t1\t1\nccnm-other\t1788496264\t0\t1\n",
        ));
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        // ps: the transport for that payload is running.
        fake.push(Output::exited(
            0,
            "/usr/bin/ssh -T home ccnm internal mcp-serve --payload eyJwIjoxfQ\nlogin -pf me\n",
        ));
        // The other session is tagged, but its directory has no mcp.json,
        // so the transport question cannot be put at all.
        fake.push(Output::exited(0, "CCNM_SESSION=no-such-session\n"));

        let rep = status(
            &StatusRequest {
                protocol: PROTOCOL,
                workspace: None,
            },
            &tmux_tools(&fake, &dir, "status"),
        );
        assert_eq!(rep.tmux, Ok("3.7c".into()));
        assert_eq!(rep.sessions.len(), 2);
        assert_eq!(rep.sessions[0].workspace.as_deref(), Some("xshun"));
        assert_eq!(rep.sessions[0].session.as_deref(), Some(id));
        assert_eq!(
            rep.sessions[0].context.as_ref().unwrap().describe(),
            "Background, keychain reachable"
        );
        assert_eq!(rep.sessions[0].tools, Some(true));
        assert_eq!(rep.sessions[1].session.as_deref(), Some("no-such-session"));
        assert_eq!(rep.sessions[1].context, None);
        assert_eq!(
            rep.sessions[1].tools, None,
            "a question that could not be put is unknown, never a guessed no"
        );
        let text = rep.render();
        assert!(
            text.contains(
                "ccnm-xshun  xshun  1 attached  tools connected  (Background, keychain reachable)"
            ),
            "{text}"
        );
        assert!(
            text.contains("ccnm-other  other  detached  tools unknown  (context unknown)"),
            "{text}"
        );
    }

    /// The interruption `--print` cannot survive: the ssh carrying the
    /// call dies, the session runs on and writes its answer, and without
    /// this the answer is on another machine with no way to ask for it.
    #[test]
    fn result_finds_the_last_print_session_and_reads_what_it_wrote() {
        let dir = temp("result");
        let write_session = |id: &str, mode: Mode, stdout: Option<&str>| {
            let sdir = session::Dir::at(paths::session_dir(&dir, id));
            std::fs::create_dir_all(sdir.path()).unwrap();
            let spec = Spec {
                protocol: PROTOCOL,
                id: id.to_string(),
                workspace: "fixture".into(),
                root: PathBuf::from("/Users/bing/ccnm-fixture"),
                home_alias: "home".into(),
                home_ccnm_bin: "ccnm".into(),
                claude_config_dir: None,
                permission_mode: crate::config::PermissionMode::default(),
                mode,
                timeout_secs: 600,
                cwd: dir.clone(),
            };
            std::fs::write(sdir.meta(), serde_json::to_string(&spec).unwrap()).unwrap();
            if let Some(text) = stdout {
                std::fs::write(sdir.stdout(), text).unwrap();
                std::fs::write(
                    sdir.exit(),
                    r#"{"exit_code":0,"timed_out":false,"duration_ms":4200}"#,
                )
                .unwrap();
            }
            sdir
        };

        let older = write_session(
            "11111111-1111-4111-8111-111111111111",
            Mode::Print {
                prompt: "old".into(),
            },
            Some(r#"{"is_error":false,"result":"the older answer","num_turns":1}"#),
        );
        // Make the wanted one newer by a clear margin.
        std::thread::sleep(Duration::from_millis(1100));
        write_session(
            "22222222-2222-4222-8222-222222222222",
            Mode::Print {
                prompt: "new".into(),
            },
            Some(r#"{"is_error":false,"result":"the answer nobody heard","num_turns":3}"#),
        );
        // Newest of all, and not what `ccnm result` is for.
        std::thread::sleep(Duration::from_millis(1100));
        write_session(
            "33333333-3333-4333-8333-333333333333",
            Mode::Interactive { prompt: None },
            None,
        );

        let fake = FakeRunner::new();
        let tools = tmux_tools(&fake, &dir, "result");
        let rep = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.session, "22222222-2222-4222-8222-222222222222");
        assert_eq!(rep.mode, "print");
        assert_eq!(
            rep.result.as_ref().unwrap().result.as_deref(),
            Some("the answer nobody heard")
        );
        assert!(rep.outcome.as_ref().unwrap().ok());
        assert!(
            rep.summary().contains("exited 0 in 4.2 s"),
            "{}",
            rep.summary()
        );

        // Naming one explicitly reaches an older session, and an
        // interactive one.
        let older_id = "11111111-1111-4111-8111-111111111111";
        let rep = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some(older_id.into()),
            },
            &tools,
        )
        .unwrap();
        assert_eq!(
            rep.result.unwrap().result.as_deref(),
            Some("the older answer")
        );
        assert_eq!(rep.session_dir, older.path());

        // "Most recent" is by time, not by whatever order the directory
        // happens to be read in: touch the older one and it wins.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(older.meta(), std::fs::read_to_string(older.meta()).unwrap()).unwrap();
        let rep = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.session, older_id, "newest by mtime, not by read order");

        let rep = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some("33333333-3333-4333-8333-333333333333".into()),
            },
            &tools,
        )
        .unwrap();
        assert_eq!(rep.mode, "interactive");
        assert!(rep.outcome.is_none(), "still running");

        let missing = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: Some("44444444-4444-4444-8444-444444444444".into()),
            },
            &tools,
        )
        .unwrap_err();
        assert_eq!(missing.code(), ErrorCode::NotReady);
    }

    /// A workspace with only interactive sessions has nothing stored to
    /// hand back, and the error says where the output actually went.
    #[test]
    fn result_without_a_print_session_says_where_to_look_instead() {
        let dir = temp("result-none");
        std::fs::create_dir_all(paths::sessions_dir(&dir)).unwrap();
        let fake = FakeRunner::new();
        let err = result(
            &ResultRequest {
                protocol: PROTOCOL,
                workspace: "fixture".into(),
                session: None,
            },
            &tmux_tools(&fake, &dir, "result-none"),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("ccnm attach fixture"), "{err}");
    }

    /// A session whose transport died is the worst failure this system
    /// has: the terminal works, the model answers, and every tool it has
    /// is gone. The status line has to say so and say what to do.
    #[test]
    fn a_session_that_lost_its_tools_says_how_to_get_them_back() {
        let dir = temp("tools-down");
        let id = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d";
        let session_dir = session::Dir::at(paths::session_dir(&dir, id));
        std::fs::create_dir_all(session_dir.path()).unwrap();
        std::fs::write(
            session_dir.mcp_config(),
            r#"{"mcpServers":{"ccnm":{"command":"/usr/bin/ssh","args":["--payload","eyJwIjoxfQ"]}}}"#,
        )
        .unwrap();

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "tmux 3.7c\n"));
        fake.push(Output::exited(0, "ccnm-xshun\t1788496263\t0\t1\n"));
        fake.push(Output::exited(0, format!("CCNM_SESSION={id}\n")));
        // ps: everything else on the machine, but not that transport.
        fake.push(Output::exited(0, "/usr/bin/ssh -T home something else\n"));

        let rep = status(
            &StatusRequest {
                protocol: PROTOCOL,
                workspace: None,
            },
            &tmux_tools(&fake, &dir, "tools-down"),
        );
        assert_eq!(rep.sessions[0].tools, Some(false));
        let text = rep.render();
        assert!(text.contains("TOOLS DOWN"), "{text}");
        assert!(text.contains("/mcp -> ccnm -> Reconnect"), "{text}");
    }

    #[test]
    fn run_refuses_without_a_controller_and_creates_nothing() {
        let dir = temp("run-none");
        let tools = Tools {
            runner: &FakeRunner::new(),
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: absent_socket("run-none"),
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    #[test]
    fn run_refuses_a_controller_outside_the_login_session() {
        let dir = temp("run-bg");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-bg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Background\n"));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                tmux: None,
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap();
        });
        let tools = Tools {
            runner: &FakeRunner::new(),
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: socket,
        };
        let err = run(&run_request("x"), &tools).unwrap_err();
        served.join().unwrap();
        assert_eq!(err.code(), ErrorCode::NotReady);
        assert!(err.message().contains("Background"), "{err}");
        assert!(!dir.join("sessions").exists(), "no session may be created");
    }

    /// The whole print-mode path with everything real except Claude: a
    /// real socket, the real Start handler, a real detached spawn of a
    /// stand-in supervisor, the real wait, the real parse. The stand-in
    /// records the argv it was given and finishes the session the way the
    /// real supervisor would: by writing `exit` last.
    #[test]
    fn run_starts_a_session_through_the_controller_and_brings_back_the_result() {
        let dir = temp("run-ok");
        let socket = PathBuf::from(format!("/tmp/ccnm-wr-ok-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let sessions = dir.join("sessions");
        let supervisor = dir.join("fake-supervisor");
        std::fs::write(
            &supervisor,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nfor s in {sessions}/*/; do\n  printf '{{\"is_error\":false,\"result\":\"hi from claude\",\"num_turns\":1}}' > \"$s/stdout\"\n  : > \"$s/stderr\"\n  printf '{{\"exit_code\":0,\"timed_out\":false,\"duration_ms\":42}}' > \"$s/exit.tmp\"\n  mv \"$s/exit.tmp\" \"$s/exit\"\ndone\n",
                argv = dir.join("supervisor-argv").display(),
                sessions = sessions.display(),
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&supervisor, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let listener = crate::controller::Listener::bind(&socket).unwrap();
        let served = std::thread::spawn({
            let supervisor = supervisor.clone();
            move || {
                let inner = FakeRunner::new();
                inner.push(Output::exited(0, "Aqua\n"));
                let tools = crate::controller::Tools {
                    runner: &inner,
                    claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                    tmux: None,
                    exe: supervisor,
                };
                listener.serve_one(&tools).unwrap(); // hello
                listener.serve_one(&tools).unwrap(); // start
            }
        });

        // The version-and-root handshake `run` makes before it builds a
        // session.
        let caller = FakeRunner::new();
        caller.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            runner: &caller,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: None,
            tmux: None,
            controller: socket,
        };
        let rep = run(&run_request("fix the failing test"), &tools).unwrap();
        served.join().unwrap();

        assert!(rep.outcome.ok(), "{:?}", rep.outcome);
        assert_eq!(rep.outcome.duration_ms, 42);
        let result = rep.result.expect("a parsed result");
        assert_eq!(result.result.as_deref(), Some("hi from claude"));
        assert!(rep.stdout_tail.is_empty(), "no tail when the result parsed");
        assert!(rep.pid > 0);
        assert!(rep.controller.login_session());

        // The session directory is the report's, named by the id, with
        // the inputs Claude would have been started with.
        assert_eq!(rep.session_dir, sessions.join(&rep.session));
        let session_dir = crate::session::Dir::at(&rep.session_dir);
        let spec = crate::session::load(&session_dir).unwrap();
        assert_eq!(spec.workspace, "fixture");
        assert_eq!(spec.cwd, dir.join("workspaces/fixture"));
        assert!(
            spec.cwd.is_dir(),
            "Claude's cwd must exist before it starts"
        );
        assert!(session_dir.mcp_config().exists());
        assert!(session_dir.settings().exists());
        assert!(session_dir.supervisor_log().exists());

        // The supervisor got exactly one payload naming this session and
        // the controller's claude, not this session's.
        let argv = std::fs::read_to_string(dir.join("supervisor-argv")).unwrap();
        let mut lines = argv.lines();
        assert_eq!(lines.next(), Some("internal"));
        assert_eq!(lines.next(), Some("supervise"));
        assert_eq!(lines.next(), Some("--payload"));
        let req: crate::session::SuperviseRequest =
            crate::protocol::payload::decode(lines.next().unwrap()).unwrap();
        assert_eq!(req.session_dir, rep.session_dir);
        assert_eq!(req.claude_bin, PathBuf::from("/opt/homebrew/bin/claude"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail(b"short"), "short");
        let long = format!("{}中文结尾", "x".repeat(3000));
        let t = tail(long.as_bytes());
        assert!(t.starts_with("..."));
        assert!(t.ends_with("中文结尾"));
        assert!(t.len() <= 2048 + 3 + 3, "{}", t.len());
    }

    /// The point of the whole phase: when a controller is listening, every
    /// question about Claude goes to it, and this ssh session runs no
    /// `claude` at all -- not even the version, because the controller's
    /// PATH is the one Claude will really be started from.
    #[test]
    fn with_a_controller_claude_is_asked_there_and_not_here() {
        let dir = temp("probe-controller");
        let socket = PathBuf::from(format!("/tmp/ccnm-wp-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::controller::Listener::bind(&socket).unwrap();

        // The controller's own environment: a login session, and a claude
        // that answers both questions.
        let served = std::thread::spawn(move || {
            let inner = FakeRunner::new();
            inner.push(Output::exited(0, "Aqua\n"));
            inner.push(Output::exited(0, "2.1.259 (Claude Code)\n"));
            inner.push(Output::exited(
                0,
                r#"{"loggedIn":true,"email":"me@x","authMethod":"claude.ai"}"#,
            ));
            let tools = crate::controller::Tools {
                runner: &inner,
                claude: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                tmux: None,
                exe: PathBuf::from("/x/ccnm"),
            };
            listener.serve_one(&tools).unwrap(); // hello
            listener.serve_one(&tools).unwrap(); // claude-auth
            inner.calls()
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname home.ts\nuser ccrun\n"));
        fake.push(Output::exited(0, hello_json(true)));
        let tools = Tools {
            runner: &fake,
            state: dir.clone(),
            control_dir: control(&dir),
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
            tmux: None,
            controller: socket.clone(),
        };
        let rep = probe(&request(), &tools);

        let ctx = rep.controller.as_ref().unwrap().as_ref().unwrap();
        assert!(ctx.login_session(), "{ctx:?}");
        assert!(rep.claude.auth.as_ref().unwrap().logged_in);
        assert_eq!(rep.claude.version, Ok("2.1.259".into()));
        assert_eq!(
            rep.claude.path,
            Some(PathBuf::from("/opt/homebrew/bin/claude")),
            "the binary reported must be the controller's, not this session's"
        );
        let ssh_calls: Vec<String> = fake.calls().iter().map(Cmd::display).collect();
        assert_eq!(ssh_calls.len(), 2, "{ssh_calls:?}");
        assert!(
            !ssh_calls.iter().any(|c| c.contains("claude")),
            "the ssh session must not run claude itself: {ssh_calls:?}"
        );

        // ...and the controller ran exactly the two claude commands, with
        // the config dir from the request.
        let inner_calls = served.join().unwrap();
        assert_eq!(inner_calls.len(), 3);
        assert!(inner_calls[1].display().contains("--version"));
        assert!(inner_calls[2].display().contains("auth status"));
        assert!(
            inner_calls[2]
                .env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/x/claude")
        );
    }
}

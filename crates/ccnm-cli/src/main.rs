//! The single `ccnm` binary. Which role it plays (home launcher, work
//! controller, home MCP runtime) is decided by the subcommand; all logic
//! lives in ccnm-core.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ccnm_core::process::SystemRunner;
use ccnm_core::protocol::hello::{self, HelloRequest};
use ccnm_core::protocol::mcp::ServePayload;
use ccnm_core::protocol::payload;
use ccnm_core::protocol::probe::ProbeRequest;
use ccnm_core::protocol::run::{
    AttachRequest, ResultRequest, RunReport, RunRequest, StartRequest, StatusRequest, StopRequest,
};
use ccnm_core::{
    Config, Result, claude, controller, doctor, launchagent, launcher, mcp, paths, safety, session,
    tmux, work,
};

/// Terminal-native remote workspace runtime for Claude Code.
#[derive(Parser)]
#[command(name = "ccnm", version = ccnm_core::VERSION)]
struct Cli {
    /// Config file to use instead of ~/.config/ccnm/config.toml
    #[arg(long, global = true, env = "CCNM_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Debug logging on stderr (same as CCNM_LOG=debug)
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that this machine and a workspace are ready to use (read-only,
    /// never changes anything)
    Doctor {
        /// Workspace name from config.toml; omit to check only the config
        workspace: Option<String>,
    },
    /// Start a Claude session for a workspace on the work machine, and
    /// attach this terminal to it
    Run {
        /// Workspace name from config.toml
        workspace: String,
        /// What Claude opens with; without it the prompt starts empty
        prompt: Option<String>,
        /// Run one prompt non-interactively and print the result instead of
        /// attaching a terminal
        #[arg(long, value_name = "PROMPT", conflicts_with = "prompt")]
        print: Option<String>,
        /// Kill Claude after this many seconds (--print only)
        #[arg(long, default_value_t = 600, value_name = "SECONDS")]
        timeout: u64,
        /// Start the session but do not attach to it
        #[arg(long, conflicts_with = "print")]
        detached: bool,
    },
    /// Attach this terminal to a workspace's running session
    Attach {
        /// Workspace name from config.toml
        workspace: String,
    },
    /// What is running on the work machine
    Status {
        /// Workspace name from config.toml
        workspace: String,
        /// Every ccnm session on that machine, not just this workspace's
        #[arg(long)]
        all: bool,
    },
    /// What a session produced, for a `--print` run this terminal did not
    /// stay connected to
    Result {
        /// Workspace name from config.toml
        workspace: String,
        /// A session id; without one, the workspace's most recent session
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// End a workspace's session: Claude, its terminal and its MCP
    /// transport all go away
    Stop {
        /// Workspace name from config.toml
        workspace: String,
    },
    /// MCP transport diagnostics
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// The login-session controller. Run these ON the work machine, or
    /// over ssh to it: `ssh work ccnm work-controller install`
    WorkController {
        #[command(subcommand)]
        command: WorkControllerCommand,
    },
    /// Internal: invoked over ssh by the ccnm on the other machine
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Start one MCP session to the workspace's runtime, call
    /// workspace_info N times, report latency and prove a single server
    /// process answered; the server is shut down afterwards
    Probe {
        /// Workspace name from config.toml
        workspace: String,
        /// How many workspace_info calls to time
        #[arg(long, default_value_t = 100)]
        calls: u32,
        /// Spawn the server as a child of this process instead of going
        /// work -> ssh -> home, to measure the runtime without the network
        #[arg(long)]
        local: bool,
    },
}

#[derive(Subcommand)]
enum WorkControllerCommand {
    /// Install the LaunchAgent, start it, and check that it answers from
    /// the login session
    Install {
        /// Print the plist and the launchctl commands; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Is a controller listening, and in which security session?
    Status,
    /// Stop the controller and remove its LaunchAgent
    Uninstall,
}

/// Every internal command takes exactly one base64url payload (design doc
/// section 8) and answers with one JSON document on stdout.
#[derive(Subcommand)]
enum InternalCommand {
    /// Report this build, user and platform; answered by either machine
    Hello {
        #[arg(long)]
        payload: String,
    },
    /// Work-side doctor probe: Claude, reverse ssh, home hello, MCP
    Probe {
        #[arg(long)]
        payload: String,
    },
    /// Serve MCP on stdin/stdout for the workspace in the payload
    McpServe {
        #[arg(long)]
        payload: String,
    },
    /// Work-side run: create the session, have the controller start it,
    /// wait, report
    WorkRun {
        #[arg(long)]
        payload: String,
    },
    /// Work-side start of an interactive session; returns as soon as tmux
    /// has it
    WorkStart {
        #[arg(long)]
        payload: String,
    },
    /// Hand this terminal (an `ssh -t`) to the workspace's tmux session.
    /// The one internal command that answers with a terminal, not JSON
    Attach {
        #[arg(long)]
        payload: String,
    },
    /// Work-side stop of an interactive session
    WorkStop {
        #[arg(long)]
        payload: String,
    },
    /// Work-side list of live sessions
    WorkStatus {
        #[arg(long)]
        payload: String,
    },
    /// Work-side read of what a session produced
    WorkResult {
        #[arg(long)]
        payload: String,
    },
    /// Be Claude's parent for one session; started by the controller
    Supervise {
        #[arg(long)]
        payload: String,
    },
    /// Answer on the work machine's controller socket until killed.
    ///
    /// The one internal command with no `--payload`: it is started by
    /// launchd inside the login session, not by the other machine, so
    /// there is no request to carry (see ccnm_core::controller).
    WorkController,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match run(cli) {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{err}");
            exit_code(err.exit_code())
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    let config_path = || -> Result<PathBuf> {
        match &cli.config {
            Some(path) => Ok(path.clone()),
            None => paths::config_path(),
        }
    };

    match &cli.command {
        Command::Doctor { workspace } => {
            let env = doctor::Env {
                runner: &SystemRunner,
                control_dir: paths::state_dir()?.join("ssh"),
                home: paths::home_dir()?,
                audit: runtime_audit(&config_path()?, workspace.as_deref()),
            };
            let report = doctor::run(&config_path()?, workspace.as_deref(), &env);
            print!("{}", report.render());
            Ok(report.exit_code())
        }
        Command::Run {
            workspace,
            prompt,
            print,
            timeout,
            detached,
        } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let env = home_env()?;
            if let Some(prompt) = print {
                let rep = launcher::run_print(
                    &resolved,
                    &env,
                    prompt,
                    std::time::Duration::from_secs(*timeout),
                )?;
                return print_run_report(&rep);
            }
            let rep = launcher::start_interactive(&resolved, &env, prompt.as_deref())?;
            eprintln!("{}", rep.summary());
            if *detached {
                eprintln!("\nattach when you want it: ccnm attach {workspace}");
                return Ok(0);
            }
            attach(&resolved, &env, workspace)
        }
        Command::Attach { workspace } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            attach(&resolved, &home_env()?, workspace)
        }
        Command::Status { workspace, all } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let rep = launcher::status(&resolved, &home_env()?, *all)?;
            print!("{}", rep.render());
            Ok(0)
        }
        Command::Result { workspace, session } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let rep = launcher::result(&resolved, &home_env()?, session.as_deref())?;
            println!("{}", rep.summary());
            match &rep.result {
                Some(r) => {
                    println!("\n--- result ---");
                    println!("{}", r.result.as_deref().unwrap_or("").trim_end());
                }
                None if !rep.stdout_tail.is_empty() => {
                    println!("\n--- stdout (tail) ---\n{}", rep.stdout_tail.trim_end());
                }
                None => {}
            }
            if !rep.stderr_tail.trim().is_empty() {
                eprintln!("\n--- stderr (tail) ---\n{}", rep.stderr_tail.trim_end());
            }
            eprintln!("\nsession directory on work: {}", rep.session_dir.display());
            Ok(0)
        }
        Command::Stop { workspace } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let rep = launcher::stop(&resolved, &home_env()?)?;
            if rep.killed {
                println!("stopped {}", rep.tmux_session);
            } else {
                println!("nothing to stop: {} was not running", rep.tmux_session);
            }
            Ok(0)
        }
        Command::Mcp {
            command:
                McpCommand::Probe {
                    workspace,
                    calls,
                    local,
                },
        } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let env = home_env()?;
            let rep = if *local {
                launcher::mcp_probe_local(&resolved, &env, *calls)?
            } else {
                launcher::mcp_probe_remote(&resolved, &env, *calls)?
            };
            println!("{}", rep.summary());
            println!("{}", payload::to_json(&rep)?);
            Ok(if rep.single_process {
                0
            } else {
                ccnm_core::ErrorCode::Internal.exit_code()
            })
        }
        Command::WorkController { command } => work_controller(command),
        Command::Internal { command } => match command {
            InternalCommand::Hello { payload } => {
                let req: HelloRequest = payload::decode(payload)?;
                print_json(&hello::answer(&req))
            }
            InternalCommand::McpServe { payload } => {
                let req: ServePayload = payload::decode(payload)?;
                mcp::server::serve(&req)?;
                Ok(0)
            }
            InternalCommand::WorkController => {
                let socket = paths::controller_socket(&paths::state_dir()?);
                let listener = controller::Listener::bind(&socket)?;
                let tools = controller::Tools {
                    runner: &SystemRunner,
                    // Resolved here, in launchd's environment, because
                    // that is the PATH Claude will actually be started
                    // with.
                    claude: claude::locate_from_env(),
                    // Same reason as claude: launchd's PATH is not a login
                    // shell's, and the tmux server has to be started from
                    // here to be in the login session.
                    tmux: tmux::locate_from_env(),
                    exe: std::env::current_exe()?,
                };
                listener.serve_forever(&tools)?;
                Ok(0)
            }
            InternalCommand::Supervise { payload } => {
                let req: session::SuperviseRequest = payload::decode(payload)?;
                let outcome = session::supervise(&req)?;
                Ok(if outcome.ok() { 0 } else { 1 })
            }
            InternalCommand::Probe { payload } => {
                let req: ProbeRequest = payload::decode(payload)?;
                print_json(&work::probe(&req, &work_tools()?))
            }
            InternalCommand::WorkRun { payload } => {
                let req: RunRequest = payload::decode(payload)?;
                print_json(&work::run(&req, &work_tools()?)?)
            }
            InternalCommand::WorkStart { payload } => {
                let req: StartRequest = payload::decode(payload)?;
                print_json(&work::start(&req, &work_tools()?)?)
            }
            InternalCommand::Attach { payload } => {
                let req: AttachRequest = payload::decode(payload)?;
                work::attach(&req, &work_tools()?)
            }
            InternalCommand::WorkStop { payload } => {
                let req: StopRequest = payload::decode(payload)?;
                print_json(&work::stop(&req, &work_tools()?)?)
            }
            InternalCommand::WorkStatus { payload } => {
                let req: StatusRequest = payload::decode(payload)?;
                print_json(&work::status(&req, &work_tools()?))
            }
            InternalCommand::WorkResult { payload } => {
                let req: ResultRequest = payload::decode(payload)?;
                print_json(&work::result(&req, &work_tools()?)?)
            }
        },
    }
}

fn work_tools() -> Result<work::Tools<'static>> {
    let state = paths::state_dir()?;
    Ok(work::Tools {
        runner: &SystemRunner,
        control_dir: state.join("ssh"),
        claude: claude::locate_from_env(),
        tmux: tmux::locate_from_env(),
        controller: paths::controller_socket(&state),
        state,
    })
}

fn home_env() -> Result<launcher::Env<'static>> {
    Ok(launcher::Env {
        runner: &SystemRunner,
        control_dir: paths::state_dir()?.join("ssh"),
        current_exe: std::env::current_exe()?,
    })
}

/// Give this terminal to the work machine's tmux and stay out of the way
/// until it comes back.
///
/// Not `exec`: when the person detaches or Claude ends, there is one more
/// useful thing to say — whether the session is still running — and a
/// process that replaced itself with ssh cannot say it.
fn attach(
    resolved: &ccnm_core::config::Resolved<'_>,
    env: &launcher::Env<'_>,
    workspace: &str,
) -> Result<i32> {
    let cmd = launcher::attach_cmd(resolved, env)?;
    let captured = ccnm_core::process::run_attached(&cmd)?;
    let code = captured.exit_code.unwrap_or(1);
    match launcher::status(resolved, env, false) {
        Ok(rep) if !rep.sessions.is_empty() => {
            eprintln!("\nstill running on the work machine; back in with: ccnm attach {workspace}");
        }
        Ok(_) => eprintln!("\nthe session has ended"),
        // The session's own exit code is worth more than a failure to look
        // it up afterwards.
        Err(e) => eprintln!("\ncannot tell whether the session is still running: {e}"),
    }
    Ok(code)
}

/// The summary, then Claude's answer, then whatever went wrong. Exit 0
/// only when Claude ran to completion and did not report an error itself.
fn print_run_report(rep: &RunReport) -> Result<i32> {
    println!("{}", rep.summary());
    match &rep.result {
        Some(r) => {
            println!("\n--- result ---");
            println!("{}", r.result.as_deref().unwrap_or("").trim_end());
            if !r.permission_denials.is_empty() {
                eprintln!("\npermission denials:");
                for d in &r.permission_denials {
                    eprintln!("  {d}");
                }
            }
        }
        None if !rep.stdout_tail.is_empty() => {
            println!("\n--- stdout (tail) ---\n{}", rep.stdout_tail.trim_end());
        }
        None => {}
    }
    if !rep.stderr_tail.trim().is_empty() {
        eprintln!("\n--- stderr (tail) ---\n{}", rep.stderr_tail.trim_end());
    }
    eprintln!("\nsession directory on work: {}", rep.session_dir.display());
    let ok = rep.outcome.ok() && rep.result.as_ref().is_some_and(|r| !r.is_error);
    Ok(if ok { 0 } else { 1 })
}

/// `ccnm work-controller ...`, run on the work machine.
///
/// A controller that is running but not in a login session exits
/// `CCNM_E_NOT_READY` rather than 0: it answers, so nothing is broken, but
/// it cannot do the one job it exists for, and a green exit code there
/// would be the same lie this whole component was built to stop telling.
fn work_controller(command: &WorkControllerCommand) -> Result<i32> {
    let state = paths::state_dir()?;
    let socket = paths::controller_socket(&state);
    let plan = || -> Result<launchagent::Plan> {
        launchagent::Plan::new(
            &paths::home_dir()?,
            &state,
            &std::env::current_exe()?,
            &SystemRunner,
        )
    };

    match command {
        WorkControllerCommand::Install { dry_run } => {
            let plan = plan()?;
            println!("{}", plan.describe());
            if *dry_run {
                println!("\n--- {} ---\n{}", plan.plist_path.display(), plan.plist);
                return Ok(0);
            }
            let ctx = launchagent::install(&plan, &SystemRunner)?;
            println!("\nlistening: {}", ctx.describe());
            Ok(login_session_verdict(&ctx))
        }
        WorkControllerCommand::Status => {
            let ctx = controller::context(&socket)?;
            println!("{}", ctx.describe());
            println!("socket:    {}", socket.display());
            Ok(login_session_verdict(&ctx))
        }
        WorkControllerCommand::Uninstall => {
            for line in launchagent::uninstall(&plan()?, &SystemRunner)? {
                println!("{line}");
            }
            Ok(0)
        }
    }
}

fn login_session_verdict(ctx: &controller::Context) -> i32 {
    if ctx.login_session() {
        return 0;
    }
    eprintln!(
        "\nthis controller is NOT in a login session ({}), so Claude started from it\n\
         would not be able to read its own credentials.\n\
         two ways that happens:\n\
         - it was started by hand instead of by launchd: ccnm work-controller install\n\
         - nobody is logged in on the work machine's screen; log in there once",
        match &ctx.manager {
            Ok(name) => name.as_str(),
            Err(_) => "session unknown",
        }
    );
    ccnm_core::ErrorCode::NotReady.exit_code()
}

/// Replies to the other machine go on stdout as one JSON document.
/// Nothing else may ever be printed there (design doc section 8).
fn print_json<T: serde::Serialize>(value: &T) -> Result<i32> {
    println!("{}", payload::to_json(value)?);
    Ok(0)
}

fn exit_code(code: i32) -> ExitCode {
    // Every ErrorCode fits in a u8 (tested in ccnm-core); anything else is a
    // bug and 1 (CCNM_E_INTERNAL) is the honest answer.
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Logs go to stderr so stdout stays parseable (doctor tables, JSON
/// replies, MCP JSON-RPC later). Default level is warn; `-v` or
/// `CCNM_LOG=debug` opens it up.
fn init_logging(verbose: bool) {
    use tracing_subscriber::EnvFilter;

    let filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_env("CCNM_LOG").unwrap_or_else(|_| EnvFilter::new("warn"))
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Audit the account this machine's runtime runs as.
///
/// Done here rather than inside doctor so doctor stays a pure function of
/// its inputs. The config is loaded best-effort: without it there is no
/// declared runtime user, which the audit already treats as a failure.
fn runtime_audit(config_path: &std::path::Path, workspace: Option<&str>) -> safety::Audit {
    let expected = Config::load(config_path).ok().and_then(|config| {
        let name = workspace?;
        let resolved = config.workspace(name).ok()?;
        resolved.runtime.runtime_user.clone()
    });
    let home = paths::home_dir().unwrap_or_else(|_| std::path::PathBuf::from("/nonexistent"));
    safety::audit(expected.as_deref(), &home, &SystemRunner)
}

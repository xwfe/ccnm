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
use ccnm_core::{Config, Result, claude, controller, doctor, launcher, mcp, paths, safety, work};

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
    /// MCP transport diagnostics
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
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
            let env = launcher::Env {
                runner: &SystemRunner,
                control_dir: paths::state_dir()?.join("ssh"),
                current_exe: std::env::current_exe()?,
            };
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
                    // with later.
                    claude: claude::locate_from_env(),
                };
                listener.serve_forever(&tools)?;
                Ok(0)
            }
            InternalCommand::Probe { payload } => {
                let req: ProbeRequest = payload::decode(payload)?;
                let tools = work::Tools {
                    runner: &SystemRunner,
                    control_dir: paths::state_dir()?.join("ssh"),
                    claude: claude::locate_from_env(),
                };
                print_json(&work::probe(&req, &tools))
            }
        },
    }
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

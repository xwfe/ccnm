//! The single `ccnm` binary. Which role it plays (home launcher, work
//! controller, home MCP runtime) is decided by the subcommand; all logic
//! lives in ccnm-core.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ccnm_core::process::SystemRunner;
use ccnm_core::protocol::hello::{self, HelloRequest};
use ccnm_core::protocol::payload;
use ccnm_core::protocol::probe::ProbeRequest;
use ccnm_core::{Result, claude, doctor, paths, tailscale, work};

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
    /// Internal: invoked over ssh by the ccnm on the other machine
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
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
    /// Work-side doctor probe: Claude, reverse ssh, home hello
    Probe {
        #[arg(long)]
        payload: String,
    },
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
                tailscale: tailscale::locate_from_env(),
                home: paths::home_dir()?,
            };
            let report = doctor::run(&config_path()?, workspace.as_deref(), &env);
            print!("{}", report.render());
            Ok(report.exit_code())
        }
        Command::Internal { command } => match command {
            InternalCommand::Hello { payload } => {
                let req: HelloRequest = payload::decode(payload)?;
                print_json(&hello::answer(&req))
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

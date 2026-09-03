//! The single `ccnm` binary. Which role it plays (home launcher, work
//! controller, home runner) is decided by the subcommand; all logic lives in
//! ccnm-core.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ccnm_core::process::SystemRunner;
use ccnm_core::{Config, Result, claude, doctor, home, paths, payload, runner, tailscale, work};

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
    /// Manage a workspace's identity file on this machine
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Mount the workspace's SMB share on the work machine
    Mount { workspace: String },
    /// Unmount the workspace's SMB share on the work machine
    Unmount { workspace: String },
    /// Internal: invoked on the work machine by ccnm over ssh
    #[command(hide = true)]
    Work {
        #[command(subcommand)]
        command: WorkCommand,
    },
    /// Internal: invoked on the home machine as the restricted runner account
    #[command(hide = true)]
    Runner {
        #[command(subcommand)]
        command: RunnerCommand,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Create .ccnm-workspace-id in the workspace root
    Init { workspace: String },
}

#[derive(Subcommand)]
enum WorkCommand {
    Probe {
        #[arg(long)]
        payload: String,
    },
    Mount {
        #[arg(long)]
        payload: String,
    },
    Unmount {
        #[arg(long)]
        payload: String,
    },
}

#[derive(Subcommand)]
enum RunnerCommand {
    Health {
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
    let control_dir = || -> Result<PathBuf> { Ok(paths::state_dir()?.join("ssh")) };

    match &cli.command {
        Command::Doctor { workspace } => {
            let env = doctor::Env {
                runner: &SystemRunner,
                control_dir: control_dir()?,
                tailscale: tailscale::locate_from_env(),
            };
            let report = doctor::run(&config_path()?, workspace.as_deref(), &env);
            print!("{}", report.render());
            Ok(report.exit_code())
        }
        Command::Workspace {
            command: WorkspaceCommand::Init { workspace },
        } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let id = home::workspace_init(&resolved)?;
            println!(
                "{}: {id}",
                ccnm_core::identity::path(&resolved.workspace.root).display()
            );
            Ok(0)
        }
        Command::Mount { workspace } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let env = home::Env {
                runner: &SystemRunner,
                control_dir: control_dir()?,
            };
            let rep = home::mount(&resolved, &env)?;
            let how = if rep.already_mounted {
                "already mounted"
            } else {
                "mounted"
            };
            println!(
                "{how} {} at {} on {}\n{}",
                rep.url,
                resolved.workspace.root.display(),
                resolved.work_ssh,
                rep.status.detail
            );
            Ok(0)
        }
        Command::Unmount { workspace } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let env = home::Env {
                runner: &SystemRunner,
                control_dir: control_dir()?,
            };
            let rep = home::unmount(&resolved, &env)?;
            if rep.was_mounted {
                println!(
                    "unmounted {} on {}",
                    resolved.workspace.root.display(),
                    resolved.work_ssh
                );
            } else {
                println!(
                    "{} was not mounted on {}",
                    resolved.workspace.root.display(),
                    resolved.work_ssh
                );
            }
            Ok(0)
        }
        Command::Work { command } => {
            let tools = work::Tools {
                runner: &SystemRunner,
                control_dir: control_dir()?,
                claude: claude::locate_from_env(),
            };
            match command {
                WorkCommand::Probe { payload } => {
                    let req: work::ProbeRequest = payload::decode(payload)?;
                    print_json(&work::probe(&req, &tools))
                }
                WorkCommand::Mount { payload } => {
                    let req: work::MountRequest = payload::decode(payload)?;
                    print_json(&work::mount(&req, &tools)?)
                }
                WorkCommand::Unmount { payload } => {
                    let req: work::UnmountRequest = payload::decode(payload)?;
                    print_json(&work::unmount(&req, &tools)?)
                }
            }
        }
        Command::Runner {
            command: RunnerCommand::Health { payload },
        } => {
            let req: runner::HealthRequest = payload::decode(payload)?;
            print_json(&runner::health(&req))
        }
    }
}

/// Replies to the other machine go on stdout as one JSON document.
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
/// replies, hook JSON later). Default level is warn; `-v` or
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

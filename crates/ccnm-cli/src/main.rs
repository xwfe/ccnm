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
    AttachRequest, PurgeRequest, ResultRequest, RunReport, RunRequest, StartRequest, StatusRequest,
    StopRequest,
};
use ccnm_core::{
    Config, Error, Result, claude, configedit, controller, doctor, launchagent, launcher, mcp,
    paths, safety, session, tmux, work,
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
    /// Write the config: which ssh alias reaches the work machine, and
    /// which one reaches back here. Safe to run again.
    ///
    /// On the work machine give only --home: it needs to know how to
    /// reach the projects, and nothing about them
    Init {
        /// This machine's ssh alias for the work machine (the one running
        /// Claude Code). Omit on the work machine itself
        #[arg(long, value_name = "ALIAS")]
        work: Option<String>,
        /// The alias for the machine holding the projects, as the work
        /// machine reaches it
        #[arg(long, value_name = "ALIAS")]
        home: String,
    },
    /// Add, list and remove workspaces without editing the config by hand
    #[command(alias = "ws")]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
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
enum WorkspaceCommand {
    /// Point a name at a project directory on this machine
    Add {
        /// What to call it; this is the name every other command takes.
        /// Defaults to the directory's own name
        name: Option<String>,
        /// The project directory. Defaults to the current one
        path: Option<PathBuf>,
        /// Point an existing name at this directory instead of refusing
        #[arg(long)]
        replace: bool,
        /// Let exec_command run without a confined runtime account (see
        /// docs/production-safety.md)
        #[arg(long)]
        allow_unconfined_exec: bool,
        /// What Claude may do without asking
        #[arg(long, value_name = "MODE")]
        permission_mode: Option<String>,
    },
    /// Every workspace in the config, and whether its directory is here
    List,
    /// Forget a workspace. Ends its session first if one is running
    Remove {
        name: String,
        /// Also delete what ccnm kept for it on the work machine
        /// (session records and its Claude working directory)
        #[arg(long)]
        purge: bool,
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
    /// Work-side deletion of a workspace's session records
    WorkPurge {
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
    let cli = Cli::parse_from(with_default_subcommand(std::env::args_os().collect()));
    init_logging(cli.verbose);
    match run(cli) {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{err}");
            exit_code(err.exit_code())
        }
    }
}

/// `ccnm xshun` means `ccnm run xshun`.
///
/// Attaching to a workspace is the thing people do all day; the other
/// subcommands are for setting it up and looking at it. Making the common
/// one the default costs one word each time and reads like `ssh <host>`.
///
/// The rule is deliberately narrow: only when the first argument is a
/// plain word that is not a subcommand. Anything starting with `-` is left
/// alone, because a global flag can take a value (`--config FILE`) and
/// guessing which word is the workspace after that is how a CLI starts
/// doing something other than what was typed.
fn with_default_subcommand(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    use clap::CommandFactory as _;
    let Some(first) = args.get(1).and_then(|a| a.to_str()) else {
        return args;
    };
    if first.starts_with('-') {
        return args;
    }
    let command = Cli::command();
    let known = command
        .get_subcommands()
        .any(|sub| sub.get_name() == first || sub.get_all_aliases().any(|alias| alias == first))
        || first == "help";
    if known {
        return args;
    }
    let mut args = args;
    args.insert(1, std::ffi::OsString::from("run"));
    args
}

fn run(cli: Cli) -> Result<i32> {
    let config_path = || -> Result<PathBuf> {
        match &cli.config {
            Some(path) => Ok(path.clone()),
            None => paths::config_path(),
        }
    };

    match &cli.command {
        Command::Init { work, home } => init(&config_path()?, work.as_deref(), home),
        Command::Workspace { command } => workspace_command(&config_path()?, command),
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
            // Sitting at the work machine: this config knows how to reach
            // home and nothing about workspaces, so home is asked to start
            // the session and this terminal attaches to it locally.
            if let Some(home) = work_side(&config, workspace) {
                if print.is_some() {
                    return Err(Error::invalid_args(
                        "--print has to be run where the projects are; ssh there and run it",
                    ));
                }
                let env = home_env()?;
                launcher::start_from_work(home, workspace, &env)?;
                if *detached {
                    eprintln!("\nattach when you want it: ccnm attach {workspace}");
                    return Ok(0);
                }
                return work::attach(&attach_request(workspace), &work_tools()?);
            }
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
            // On the work machine the session is right here; attaching
            // needs the workspace name and nothing else.
            if work_side(&config, workspace).is_some() {
                return work::attach(&attach_request(workspace), &work_tools()?);
            }
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
            InternalCommand::WorkPurge { payload } => {
                let req: PurgeRequest = payload::decode(payload)?;
                print_json(&work::purge(&req, &work_tools()?))
            }
        },
    }
}

/// `ccnm init`: the two ssh aliases, written to the config.
///
/// Everything else has a default. Running it again is not an error and
/// not a rewrite: it reports what it changed, or that there was nothing
/// to change.
fn init(path: &std::path::Path, work: Option<&str>, home: &str) -> Result<i32> {
    let mut edit = configedit::Edit::open(path)?;
    let existed = edit.existed();
    let mut changes = configedit::Changes::default();
    // Without --work this is the work machine's own config: how to reach
    // the projects, and deliberately nothing else. The workspace list
    // lives on one machine, because two lists are two answers to "where
    // is this project".
    if let Some(work) = work {
        edit.set_host("work", "ssh", work, &mut changes);
    }
    edit.set_host("home", "ssh_from_work", home, &mut changes);
    edit.save(&changes)?;

    if !existed {
        println!("wrote {}", path.display());
    }
    report_changes(&changes, path);
    if !existed || changes.lines().iter().any(|l| l.contains("workspaces")) {
        println!();
    }
    let config = Config::load(path)?;
    if work.is_none() {
        println!("this machine will ask {home} for a workspace it does not know");
        println!("next, from here:");
        println!("  ccnm <workspace>       start it there, attach here");
    } else if config.workspaces.is_empty() {
        println!("next: cd to a project on this machine and run");
        println!("  ccnm workspace add <name>");
    }
    // ssh has to work before anything else can; say so plainly rather than
    // testing it here, where a slow or absent network would turn `init`
    // into something that hangs.
    match work {
        Some(work) => {
            println!("\nboth of these must work without a password:");
            println!("  ssh {work} true");
            println!("  ssh {work} 'ssh {home} true'");
        }
        None => {
            println!("\nthis must work without a password:");
            println!("  ssh {home} true");
        }
    }
    Ok(0)
}

fn workspace_command(path: &std::path::Path, command: &WorkspaceCommand) -> Result<i32> {
    match command {
        WorkspaceCommand::Add {
            name,
            path: root,
            replace,
            allow_unconfined_exec,
            permission_mode,
        } => {
            let root = match root {
                Some(path) => path.clone(),
                None => std::env::current_dir()?,
            };
            // Canonical, because a workspace root is compared against what
            // a running session was started with, and `.`, `~/x/../x` and
            // a symlinked path are all the same directory with three
            // different spellings.
            let root = root.canonicalize().map_err(|e| {
                ccnm_core::Error::new(
                    ccnm_core::ErrorCode::WrongWorkspace,
                    format!("{} is not a directory on this machine", root.display()),
                )
                .with_source(e)
            })?;
            let name = match name {
                Some(name) => name.clone(),
                None => name_from(&root)?,
            };
            check_collisions(path, &name, &root, *replace)?;
            let mode = permission_mode
                .as_deref()
                .map(parse_permission_mode)
                .transpose()?;
            let mut edit = configedit::Edit::open(path)?;
            let mut changes = configedit::Changes::default();
            edit.set_workspace(
                &name,
                &root,
                "work",
                mode,
                allow_unconfined_exec.then_some(true),
                &mut changes,
            );
            edit.save(&changes).map_err(|e| {
                if edit.existed() {
                    e
                } else {
                    ccnm_core::Error::config(format!(
                        "there is no config yet, so a workspace has nowhere to go\nrun this first: ccnm init --work <alias> --home <alias>\n({})",
                        e.message()
                    ))
                }
            })?;
            report_changes(&changes, path);
            println!("\ncheck it: ccnm doctor {name}");
            println!("use it:   ccnm {name}");
            Ok(0)
        }
        WorkspaceCommand::List => {
            let config = Config::load(path)?;
            if config.workspaces.is_empty() {
                println!("no workspaces in {}", path.display());
                println!("add one: cd to a project and run `ccnm workspace add <name>`");
                return Ok(0);
            }
            let width = config.workspaces.keys().map(String::len).max().unwrap_or(0);
            for (name, workspace) in &config.workspaces {
                let here = if workspace.root.is_dir() {
                    ""
                } else {
                    "   (not on this machine)"
                };
                println!("{name:width$}  {}{here}", workspace.root.display());
            }
            Ok(0)
        }
        WorkspaceCommand::Remove { name, purge } => remove_workspace(path, name, *purge),
    }
}

/// Forget a workspace, after ending anything of it that is still running.
///
/// Ending the session first is not optional: a session outlives the
/// config, so a workspace removed while one is up would leave a Claude
/// running against a project nothing points at any more, and no command
/// left that names it.
fn remove_workspace(path: &std::path::Path, name: &str, purge: bool) -> Result<i32> {
    // Best effort, and in this order: the session belongs to the config
    // entry that is about to go.
    if let Ok(config) = Config::load(path)
        && let Ok(resolved) = config.workspace(name)
    {
        match launcher::stop(&resolved, &home_env()?) {
            Ok(rep) if rep.killed => println!("stopped {}", rep.tmux_session),
            Ok(_) => {}
            Err(e) => eprintln!("could not reach the work machine to stop it: {e}"),
        }
        if purge {
            match launcher::purge(&resolved, &home_env()?) {
                Ok(rep) => {
                    for line in rep.removed {
                        println!("removed {line}");
                    }
                }
                Err(e) => eprintln!("could not clean up on the work machine: {e}"),
            }
        }
    }

    let mut edit = configedit::Edit::open(path)?;
    let mut changes = configedit::Changes::default();
    if !edit.remove_workspace(name, &mut changes) {
        println!("{name} is not in {}", path.display());
        return Ok(0);
    }
    edit.save(&changes)?;
    report_changes(&changes, path);
    Ok(0)
}

/// A workspace name from the directory's own name.
///
/// Refused rather than mangled when the directory has no usable ASCII in
/// it: a project called `我的项目` would come out as an empty name or some
/// stub of one, and a name people type all day should be one they chose.
fn name_from(root: &std::path::Path) -> Result<String> {
    let raw = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = paths::safe_name(&raw, "");
    if name.is_empty() {
        return Err(ccnm_core::Error::invalid_args(format!(
            "cannot make a workspace name out of {raw:?}; give one: ccnm ws add <name>"
        )));
    }
    Ok(name)
}

/// Two ways a new workspace can collide with one already in the config,
/// and neither is ccnm's decision to make quietly.
///
/// **The same name, somewhere else.** Repointing it is a real change:
/// `ccnm <name>` would open a different project, and a session running
/// against the old root gets ended and replaced the next time it is
/// started. Silently changing what a name means, while a session under
/// that name is running, is exactly the confusion this whole afternoon
/// was.
///
/// **The same directory, another name.** Two names for one project means
/// two tmux sessions and two Claudes editing the same files, each unaware
/// of the other. Nobody wants that; they want the name they already have.
fn check_collisions(
    config_path: &std::path::Path,
    name: &str,
    root: &std::path::Path,
    replace: bool,
) -> Result<()> {
    // A config that will not load has bigger problems, and `save` reports
    // them; there is nothing to compare against here.
    let Ok(config) = Config::load(config_path) else {
        return Ok(());
    };

    if let Some((other, _)) = config
        .workspaces
        .iter()
        .find(|(other, ws)| ws.root == root && other.as_str() != name)
    {
        return Err(ccnm_core::Error::invalid_args(format!(
            "{} is already the workspace `{other}`\nuse it:            ccnm {other}\nor rename it:      ccnm ws remove {other} && ccnm ws add {name}\ntwo names for one project means two Claudes editing the same files",
            root.display()
        )));
    }

    let Some(existing) = config.workspaces.get(name) else {
        return Ok(());
    };
    if existing.root == root || replace {
        return Ok(());
    }
    Err(ccnm_core::Error::invalid_args(format!(
        "workspace `{name}` already points at {}\nthis would point it at {} instead, and end any session running against the old one\npick one:\n  ccnm ws add {} {}   (a different name for this project)\n  ccnm ws add {name} --replace   (repoint the existing one)",
        existing.root.display(),
        root.display(),
        suggested_name(name, root),
        root.display(),
    )))
}

/// A name that will not collide, built from the directory above: two
/// projects called `web` become `web` and `other-web` rather than a
/// question about numbering.
fn suggested_name(name: &str, root: &std::path::Path) -> String {
    let parent = root
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| paths::safe_name(&n.to_string_lossy(), ""))
        .unwrap_or_default();
    if parent.is_empty() {
        format!("{name}-2")
    } else {
        format!("{parent}-{name}")
    }
}

fn parse_permission_mode(raw: &str) -> Result<ccnm_core::config::PermissionMode> {
    use ccnm_core::config::PermissionMode as M;
    // Spelled the way Claude Code spells them, and the way they appear in
    // the config file, so there is one spelling to remember.
    match raw {
        "acceptEdits" => Ok(M::AcceptEdits),
        "auto" => Ok(M::Auto),
        "bypassPermissions" => Ok(M::BypassPermissions),
        "manual" => Ok(M::Manual),
        "dontAsk" => Ok(M::DontAsk),
        "plan" => Ok(M::Plan),
        _ => Err(ccnm_core::Error::invalid_args(format!(
            "unknown permission mode {raw}; one of acceptEdits, auto, bypassPermissions, manual, dontAsk, plan"
        ))),
    }
}

fn report_changes(changes: &configedit::Changes, path: &std::path::Path) {
    if changes.is_empty() {
        println!("{} already says that", path.display());
        return;
    }
    for line in changes.lines() {
        println!("{line}");
    }
}

/// The home alias to delegate to, when this machine is the work machine.
///
/// The test is not "which machine am I" -- ccnm never tries to guess that
/// -- but "does this config define the workspace being asked for". A home
/// config does. A work config has no workspaces at all, only how to reach
/// home, so a name it does not know plus a home to ask is exactly the
/// work-side case. A home config with a typo'd workspace name still falls
/// through to the normal error, because it has no `ssh_from_work` to
/// single out.
fn work_side<'a>(config: &'a Config, workspace: &str) -> Option<&'a str> {
    if config.workspace(workspace).is_ok() {
        return None;
    }
    config.home_from_work()
}

fn attach_request(workspace: &str) -> AttachRequest {
    AttachRequest {
        protocol: ccnm_core::protocol::payload::PROTOCOL,
        workspace: workspace.to_string(),
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

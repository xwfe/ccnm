//! The single `ccnm` binary. Which role it plays (home launcher, work
//! controller, home MCP runtime) is decided by the subcommand; all logic
//! lives in ccnm-core.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ccnm_core::process::SystemRunner;
use ccnm_core::protocol::hello::{self, HelloRequest};
use ccnm_core::protocol::payload;
use ccnm_core::protocol::probe::ProbeRequest;
use ccnm_core::protocol::run::{
    AttachRequest, HistoryRequest, PurgeRequest, ResultRequest, RunReport, RunRequest,
    StartRequest, StatusRequest, StopRequest,
};
use ccnm_core::provider::AgentProvider;
use ccnm_core::{
    Config, Error, Lang, Result, configedit, controller, doctor, launchagent, launcher, mcp, paths,
    session, tmux, work,
};

/// Terminal-native remote workspace runtime for configured CLI agents.
#[derive(Parser)]
#[command(name = "ccnm", version = ccnm_core::VERSION)]
struct Cli {
    /// Config file to use instead of ~/.config/ccnm/config.toml
    #[arg(long, global = true, env = "CCNM_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Debug logging on stderr (same as CCNM_LOG=debug)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// What language ccnm says things to a person in: zh (the default) or en
    ///
    /// Also settable in config.toml, under `[ui]` as `lang = "en"`. The
    /// flag wins over CCNM_LANG, which wins over the config.
    ///
    /// Only what a person reads. Error codes, protocol fields, the MCP text
    /// the model reads, and the English ccnm itself matches in git/ssh/tmux
    /// output are all unaffected.
    #[arg(long, global = true, env = "CCNM_LANG", value_name = "LANG")]
    lang: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write the config: who this machine is, and the SSH alias it
    /// reaches the other node by. Safe to run again.
    ///
    /// Exactly one of the two, because each also says which node this
    /// machine is: --agent on the node holding the projects, --runtime on
    /// the node running Claude
    Init {
        /// This machine holds the projects; ALIAS is how it reaches the Agent Node
        #[arg(
            long,
            value_name = "ALIAS",
            conflicts_with = "runtime",
            required_unless_present = "runtime"
        )]
        agent: Option<String>,
        /// This machine runs the agent; ALIAS is how it reaches the Runtime Node
        #[arg(long, value_name = "ALIAS")]
        runtime: Option<String>,
    },
    /// Add, list and remove workspaces without editing the config by hand
    #[command(visible_alias = "ws")]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Check that this machine and a workspace are ready to use (read-only,
    /// never changes anything)
    #[command(visible_alias = "dr")]
    Doctor {
        /// Workspace name from config.toml; omit to check only the config
        workspace: Option<String>,
        /// Use another configured instance on the workspace's Agent Node
        #[arg(long, value_name = "INSTANCE", requires = "workspace")]
        agent: Option<String>,
    },
    /// Start the configured Agent session for a workspace on the Agent Node, and
    /// attach this terminal to it
    Run {
        /// Workspace name from config.toml
        workspace: String,
        /// Override the workspace's default Agent Instance on the same node
        #[arg(long, value_name = "INSTANCE")]
        agent: Option<String>,
        /// What the Agent opens with; without it the prompt starts empty
        prompt: Option<String>,
        /// Read what Claude opens with from stdin, to the end of input.
        /// Takes quotes and newlines, which a command line here would not
        #[arg(long, conflicts_with_all = ["prompt", "print"])]
        prompt_stdin: bool,
        /// Run one prompt non-interactively and print the result instead of
        /// attaching a terminal
        #[arg(long, value_name = "PROMPT", conflicts_with = "prompt")]
        print: Option<String>,
        /// Kill the Agent after this many seconds (--print only)
        #[arg(long, default_value_t = 600, value_name = "SECONDS")]
        timeout: u64,
        /// Start the session but do not attach to it
        #[arg(long, conflicts_with = "print")]
        detached: bool,
    },
    /// Attach this terminal to a workspace's running session
    #[command(visible_alias = "a")]
    Attach {
        /// Workspace name from config.toml
        workspace: String,
        #[arg(long, value_name = "INSTANCE")]
        agent: Option<String>,
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// What is running on the Agent Node; without a workspace, every
    /// managed project and the Runtime processes serving them
    #[command(visible_alias = "st")]
    Status {
        /// Workspace name from config.toml; omit it for all of them
        workspace: Option<String>,
        #[arg(long, value_name = "INSTANCE", requires = "workspace")]
        agent: Option<String>,
        #[arg(
            long,
            value_name = "ID",
            conflicts_with = "all",
            requires = "workspace"
        )]
        session: Option<String>,
        /// Every ccnm session on that machine, not just this workspace's
        #[arg(long, conflicts_with = "agent", requires = "workspace")]
        all: bool,
    },
    /// Every managed project, one line each: running or not, for how long,
    /// whether its tools are connected
    #[command(visible_alias = "ls")]
    List,
    /// Past and present sessions, newest first
    #[command(visible_alias = "logs")]
    Log {
        /// Only this workspace's sessions
        workspace: Option<String>,
        /// How many to show
        #[arg(short = 'n', long, default_value_t = 20, value_name = "N")]
        limit: u32,
    },
    /// What a session produced, for a `--print` run this terminal did not
    /// stay connected to
    #[command(visible_alias = "res")]
    Result {
        /// Workspace name from config.toml
        workspace: String,
        #[arg(long, value_name = "INSTANCE")]
        agent: Option<String>,
        /// A session id; without one, the workspace's most recent session
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// End a workspace's session: Agent, terminal and MCP transport
    /// transport all go away
    Stop {
        /// Workspace name from config.toml
        workspace: String,
        #[arg(long, value_name = "INSTANCE")]
        agent: Option<String>,
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Remove what ccnm kept for a workspace's finished sessions, on every
    /// machine, by the account that owns it. Only previews unless given the
    /// token a preview printed
    Cleanup {
        /// Workspace name from config.toml
        workspace: String,
        /// Carry out the preview that printed this token
        #[arg(long, value_name = "TOKEN")]
        apply: Option<String>,
    },
    /// Speak the machine protocol on stdin/stdout, for programs rather than
    /// people: stdout carries only protocol lines, logs go to stderr. The
    /// contract is in docs/protocol/
    Rpc,
    /// MCP transports: diagnose one, or serve a remote workspace to an
    /// external MCP Host
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// The login-session controller. Run these ON the Agent Node, or
    /// over ssh to it: `ssh work ccnm controller install`
    Controller {
        #[command(subcommand)]
        command: ControllerCommand,
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
        /// With runtime_user set, let exec_command run even though that
        /// account fails the isolation checks (see docs/production-safety.md);
        /// without runtime_user only running as root still needs it
        #[arg(long)]
        allow_unconfined_exec: bool,
        /// What Claude may do without asking
        #[arg(long, value_name = "MODE")]
        permission_mode: Option<String>,
        /// The node that runs this workspace's Agent. Needed only when the
        /// config has more than one candidate and none is called `agent`
        #[arg(long, value_name = "NODE")]
        agent_node: Option<String>,
    },
    /// Every workspace in the config, and whether its directory is here
    #[command(visible_alias = "ls")]
    List,
    /// Forget a workspace. Ends its session first if one is running
    #[command(visible_alias = "rm")]
    Remove {
        name: String,
        /// Also delete what ccnm kept for it on the Agent Node
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
        #[arg(long, value_name = "INSTANCE")]
        agent: Option<String>,
        /// How many workspace_info calls to time
        #[arg(long, default_value_t = 100)]
        calls: u32,
        /// Spawn the server as a child of this process instead of going
        /// work -> ssh -> home, to measure the runtime without the network
        #[arg(long)]
        local: bool,
    },
    /// Serve a remote workspace to an external MCP Host over stdio: this
    /// process becomes one ssh to the Runtime, which runs the real server.
    /// Put it in the Host's MCP config as the command to run
    Bridge {
        /// Workspace name as the **Runtime** knows it. It must have opted
        /// in with `external_mcp`; this machine keeps no workspace list
        workspace: String,
        /// Node from this machine's config to open the transport to.
        /// Optional when exactly one node has an ssh alias
        #[arg(long, value_name = "NODE")]
        node: Option<String>,
        /// How much to ask for. `coding` is refused unless the workspace
        /// allows it, and is never silently downgraded
        #[arg(long, default_value = "read", value_parser = ["read", "coding"])]
        mode: String,
    },
}

#[derive(Subcommand)]
enum ControllerCommand {
    /// Install the controller as a background service (a LaunchAgent on
    /// macOS, a systemd user service on Linux), start it, and check that it
    /// answers
    Install {
        /// Print the service file and the commands; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Is a controller listening, and how is it running?
    Status,
    /// Stop the controller and remove its service
    Uninstall,
}

/// Every internal command takes exactly one base64url payload and answers
/// with one JSON document on stdout (the internal control protocol;
/// docs/protocol/machine-protocol-v1.md section 11 sets it apart from the
/// public ones).
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
    /// Serve this Agent Node's installed skills on stdin/stdout, for the
    /// Claude Code or Codex of one remote session (P48). Started by them,
    /// on this machine, not over ssh
    AgentSkills {
        #[arg(long)]
        payload: String,
    },
    /// Answer what a workspace is, from this Runtime's own config. Read
    /// only: it starts nothing and creates no session
    RuntimeResolve {
        #[arg(long)]
        payload: String,
    },
    /// Answer what this Runtime Executor is and whether the project is
    /// usable by it. Read-only, and meaningful only because it runs as
    /// the account the Agent's transport lands on
    RuntimeAudit {
        #[arg(long)]
        payload: String,
    },
    /// Answer what a workspace's write guard looks like, from this
    /// account's own state directory. Takes nothing and writes nothing
    RuntimeGuard {
        #[arg(long)]
        payload: String,
    },
    /// Work-side relay of a write-guard question to the Runtime
    AgentGuard {
        #[arg(long)]
        payload: String,
    },
    /// List or remove what this Runtime account kept for sessions the
    /// Agent names. Never the write guard, never a session still served
    RuntimeCleanup {
        #[arg(long)]
        payload: String,
    },
    /// Work-side cleanup: this Agent's records of finished sessions, after
    /// asking the Runtime for its half
    AgentCleanup {
        #[arg(long)]
        payload: String,
    },
    /// Work-side run: create the session, have the controller start it,
    /// wait, report
    AgentRun {
        #[arg(long)]
        payload: String,
    },
    /// Work-side start of an interactive session; returns as soon as tmux
    /// has it
    AgentStart {
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
    AgentStop {
        #[arg(long)]
        payload: String,
    },
    /// Work-side list of live sessions
    AgentStatus {
        #[arg(long)]
        payload: String,
    },
    /// Work-side list of session records, finished ones included
    AgentHistory {
        #[arg(long)]
        payload: String,
    },
    /// Work-side read of what a session produced
    AgentResult {
        #[arg(long)]
        payload: String,
    },
    /// Work-side read of a slice of a finished session's retained output
    AgentOutput {
        #[arg(long)]
        payload: String,
    },
    /// Work-side deletion of a workspace's session records
    AgentPurge {
        #[arg(long)]
        payload: String,
    },
    /// Agent-side credential-stripping SSH stdio transport
    AgentTransport {
        #[arg(long)]
        payload: String,
    },
    /// Carry one accepted Machine API run to its end; started by `ccnm rpc`
    ///
    /// The session's owner (P63): a process of its own, so the run does
    /// not end with the connection that accepted it. What to run is read
    /// from the record the handle names.
    RpcRun {
        #[arg(long)]
        handle: String,
    },
    /// Be Claude's parent for one session; started by the controller
    Supervise {
        #[arg(long)]
        payload: String,
    },
    /// Answer on the Agent Node's controller socket until killed.
    ///
    /// The one internal command with no `--payload`: it is started by
    /// launchd inside the login session, not by the other machine, so
    /// there is no request to carry (see ccnm_core::controller).
    Controller,
}

fn main() -> ExitCode {
    let args = with_default_subcommand(std::env::args_os().collect());
    let cli = parse_in_ui_lang(args);
    init_logging(cli.verbose);
    let lang = match ui_lang(&cli) {
        Ok(lang) => lang,
        Err(err) => {
            eprintln!("{err}");
            return exit_code(err.exit_code());
        }
    };
    match run(cli, lang) {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{err}");
            exit_code(err.exit_code())
        }
    }
}

/// Parse the command line with the help text already in the right
/// language.
///
/// `--help` is answered by clap during parsing, so the language has to be
/// settled *before* that — which is why this reads the argument itself
/// instead of taking it off the parsed [`Cli`]. The same narrow scan the
/// repository already does for the default subcommand, for the same
/// reason.
///
/// Only the descriptions are swapped. Command names, flags and value
/// names stay as they are: they are what someone types.
fn parse_in_ui_lang(args: Vec<std::ffi::OsString>) -> Cli {
    use clap::{CommandFactory as _, FromArgMatches as _};
    let mut command = Cli::command();
    if help_lang(&args) == Lang::Zh {
        command = zh_help(command);
    }
    let matches = command.get_matches_from(args);
    match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    }
}

/// The language for help text, read straight off the command line.
///
/// Deliberately not the full resolution [`ui_lang`] does: the config file
/// is not consulted here, because reaching it means resolving `--config`
/// and loading a file before clap has validated anything, and a broken
/// config would then break `--help` — the one command someone runs *when*
/// things are broken. A config-set language still applies to everything
/// ccnm prints itself; only `--help` falls back to the default.
fn help_lang(args: &[std::ffi::OsString]) -> Lang {
    let mut chosen = std::env::var("CCNM_LANG")
        .ok()
        .and_then(|value| Lang::parse(&value));
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let Some(text) = arg.to_str() else { continue };
        if let Some(value) = text.strip_prefix("--lang=") {
            chosen = Lang::parse(value);
        } else if text == "--lang"
            && let Some(value) = rest.next().and_then(|v| v.to_str())
        {
            chosen = Lang::parse(value);
        }
    }
    // An unparseable value is not settled here: clap has not run yet, so
    // complaining now would pre-empt its own error for the same argument.
    // `ui_lang` refuses it a moment later, with the code and wording
    // every other bad argument gets.
    chosen.unwrap_or(Lang::Zh)
}

/// The language this machine says things to a person in.
///
/// `--lang` and `CCNM_LANG` are the same clap argument, so the flag
/// already wins over the variable; the config file is consulted only when
/// neither is set. The default is Chinese, which is the only place in
/// ccnm where that default lives.
///
/// A config that cannot be read is not an error here. `ccnm init` exists
/// precisely for the machine that has no config yet, and refusing to
/// print its guidance because the file that guidance creates is missing
/// would be a poor trade for one preference.
///
/// `LANG`/`LC_ALL` are deliberately not consulted — see the module docs
/// of [`ccnm_core::lang`]: ccnm parses English from `git`, `ssh`, `tmux`
/// and Codex, so locale must keep meaning what those programs mean by it.
fn ui_lang(cli: &Cli) -> Result<Lang> {
    if let Some(text) = &cli.lang {
        return Lang::parse(text).ok_or_else(|| {
            Error::invalid_args(format!(
                "--lang {text}: ccnm 只会说 zh（中文）和 en（English）"
            ))
        });
    }
    let from_config = config_path_for(cli)
        .and_then(|path| Config::load(&path))
        .ok()
        .and_then(|config| config.ui.lang);
    match from_config {
        Some(text) => Lang::parse(&text).ok_or_else(|| {
            Error::config(format!(
                "config.toml 里 [ui] lang = \"{text}\"：ccnm 只会说 zh（中文）和 en（English）"
            ))
        }),
        None => Ok(Lang::Zh),
    }
}

/// The same command tree with Chinese descriptions.
///
/// A table rather than a second set of doc comments on [`Cli`]: doc
/// comments are what `clap` derives from, and there is only one of them
/// per item. Anything not named here keeps the English from the derive,
/// so a subcommand added without a line in this function shows up in
/// English rather than disappearing.
fn zh_help(command: clap::Command) -> clap::Command {
    command
        .about("把 AI coding agent 和真实项目放在两台机器上跑")
        .mut_arg("config", |a| {
            a.help("用别的配置文件，而不是 ~/.config/ccnm/config.toml")
        })
        .mut_arg("verbose", |a| {
            a.help("往 stderr 打调试日志（等同 CCNM_LOG=debug）")
        })
        // Both, because this one's doc comment has blank lines and clap
        // therefore built a long_about from it: `-h` shows `help`,
        // `--help` shows `long_help`, and setting only one leaves the
        // other in English.
        .mut_arg("lang", |a| {
            a.help("说给人听的那些话用什么语言：zh（默认）或 en")
                .long_help(
                    "说给人听的那些话用什么语言：zh（默认）或 en\n\n\
                     也可以写在 config.toml 里：`[ui]` 下面 `lang = \"en\"`。\n\
                     命令行优先于 CCNM_LANG，CCNM_LANG 优先于配置。\n\n\
                     只管给人看的输出。错误码、协议字段、给模型的 MCP 文本，\n\
                     以及 ccnm 自己要去匹配的 git/ssh/tmux 英文，都跟它无关。",
                )
        })
        // 只能到这里为止。`--help`/`--version` 这两个参数是 clap 在
        // build 时才加进去的，这会儿 mut_arg 还找不到它们（试过，会
        // panic）；"Usage:"、"Options:"、"error:" 这些更是写死在 clap
        // 里的，4.x 没给任何接口。所以参数写错时，那句报错仍然是英文。
        .mut_subcommand("init", |c| {
            c.about("写配置：这台机器是谁，以及它用哪个 SSH alias 找到对面那台。可以重复跑")
                // `init` is the only subcommand whose doc comment has a
                // blank line, so clap derived a long_about for it -- and
                // `--help` shows long_about while `-h` shows about. Set
                // only one and `ccnm init --help`, the first command a new
                // user runs, silently stays English.
                .long_about(
                    "写配置：这台机器是谁，以及它用哪个 SSH alias 找到对面那台。可以重复跑\n\n\
                     --agent 和 --runtime 只能给一个，因为给哪个也就说明了这台机器是谁：\n\
                     放项目的那台给 --agent，跑 Claude 的那台给 --runtime",
                )
                .mut_arg("agent", |a| {
                    a.help("这台机器放项目；ALIAS 是它找到 Agent Node 的那个别名")
                })
                .mut_arg("runtime", |a| {
                    a.help("这台机器跑 agent；ALIAS 是它找到 Runtime Node 的那个别名")
                })
        })
        .mut_subcommand("workspace", |c| {
            c.about("加、列、删 workspace，不用手改配置文件")
                .mut_subcommand("add", |c| {
                    c.about("把一个名字指到这台机器上的某个项目目录")
                        .mut_arg("name", |a| {
                            a.help("叫它什么；之后每条命令都用这个名字。不给就用目录自己的名字")
                        })
                        .mut_arg("path", |a| a.help("项目目录。不给就是当前目录"))
                        .mut_arg("replace", |a| {
                            a.help("名字已经存在时，改指到这个目录，而不是报错")
                        })
                        .mut_arg("allow_unconfined_exec", |a| {
                            a.help("写了 runtime_user 时，那个账号没通过隔离检查也让 exec_command 跑（见 docs/production-safety.md）；没写 runtime_user 时只有以 root 运行才需要它")
                        })
                        .mut_arg("permission_mode", |a| a.help("Claude 不用问就能做的事"))
                        .mut_arg("agent_node", |a| {
                            a.help("哪个节点跑这个 workspace 的 Agent。配置里有不止一个候选、又没有叫 agent 的时候才需要给")
                        })
                })
                .mut_subcommand("list", |c| {
                    c.about("配置里有哪些 workspace，以及它们的目录在不在这台机器上")
                })
                .mut_subcommand("remove", |c| {
                    c.about("删掉一个 workspace。它要是还有会话在跑，先停掉")
                        .mut_arg("purge", |a| {
                            a.help("连 ccnm 给它存的东西一起删：会话记录和官方 CLI 的工作目录。项目本身永远不碰")
                        })
                })
        })
        .mut_subcommand("doctor", |c| {
            c.about("检查这台机器和某个 workspace 能不能用（只读，不改任何东西）")
                .mut_arg("workspace", |a| {
                    a.help("config.toml 里的 workspace 名字；不给就只查配置本身")
                })
                .mut_arg("agent", |a| {
                    a.help("用这个 workspace 的 Agent Node 上另一个配好的 instance")
                })
        })
        .mut_subcommand("run", |c| {
            c.about("在 Agent Node 上给这个 workspace 起一个 Agent 会话，然后把这个终端接上去")
                .mut_arg("workspace", |a| a.help("config.toml 里的 workspace 名字"))
                .mut_arg("prompt", |a| a.help("Agent 开场读什么；不给就是空的"))
                .mut_arg("agent", |a| {
                    a.help("不用这个 workspace 的默认 Agent Instance，改用同一节点上的另一个")
                })
                .mut_arg("prompt_stdin", |a| {
                    a.help("开场白从 stdin 读到结束。能带引号和换行，命令行上不行")
                })
                .mut_arg("print", |a| {
                    a.help("非交互地跑一句，把结果打出来，不接终端")
                })
                .mut_arg("timeout", |a| a.help("多少秒之后杀掉 Agent（只对 --print 有效）"))
                .mut_arg("detached", |a| a.help("把会话起起来，但不接上去"))
        })
        .mut_subcommand("attach", |c| {
            workspace_args(c.about("把这个终端接到某个 workspace 正在跑的会话上"))
        })
        .mut_subcommand("status", |c| {
            c.about("Agent Node 上现在跑着什么；不给项目名就看所有项目，连同 Runtime 上服务它们的进程")
                .mut_arg("workspace", |a| {
                    a.help("config.toml 里的 workspace 名字；不给就看全部")
                })
                .mut_arg("all", |a| a.help("那台机器上每一个 ccnm 会话，不只是这个 workspace 的"))
        })
        .mut_subcommand("list", |c| {
            c.about("所有项目一项一行：在不在跑、跑了多久、工具通不通")
        })
        .mut_subcommand("log", |c| {
            c.about("跑过的和正在跑的会话，最新的在前")
                .mut_arg("workspace", |a| a.help("只看这个 workspace 的"))
                .mut_arg("limit", |a| a.help("最多列几条"))
        })
        .mut_subcommand("result", |c| {
            workspace_args(c.about("某次会话产出了什么——给那种 --print 跑完、终端没一直连着的情况"))
                .mut_arg("session", |a| a.help("会话 id；不给就是这个 workspace 最近的那次"))
        })
        .mut_subcommand("stop", |c| {
            workspace_args(c.about("结束一个 workspace 的会话：Agent、终端和 MCP 通道一起没"))
        })
        .mut_subcommand("cleanup", |c| {
            c.about(
                "删掉 ccnm 为某个 workspace 已结束的会话留下的东西：每台机器上的，各由它所属的账号来删。不给预览打出的令牌就只预览",
            )
            .mut_arg("workspace", |a| a.help("config.toml 里的 workspace 名字"))
            .mut_arg("apply", |a| a.help("照打出这个令牌的那次预览去删"))
        })
        .mut_subcommand("rpc", |c| {
            c.about(
                "在 stdin/stdout 上说机器协议，给程序用不是给人用：stdout 上只有协议行，日志走 stderr。契约在 docs/protocol/",
            )
        })
        .mut_subcommand("mcp", |c| {
            c.about("MCP 通道：诊断一条，或者把远端 workspace 交给外部 MCP Host")
                .mut_subcommand("probe", |c| {
                    c.about(
                        "向这个 workspace 的 Runtime 起一条 MCP 会话，调 N 次 workspace_info，报延迟，并证明回答的是同一个 server 进程；跑完就关掉",
                    )
                    .mut_arg("workspace", |a| a.help("config.toml 里的 workspace 名字"))
                    .mut_arg("calls", |a| a.help("计时的 workspace_info 调用次数"))
                    .mut_arg("local", |a| {
                        a.help("把 server 起成这个进程的子进程，不走 work -> ssh -> home：量不含网络的那部分")
                    })
                })
                .mut_subcommand("bridge", |c| {
                    c.about(
                        "把远端 workspace 经 stdio 交给外部 MCP Host：这个进程就是一条通往 Runtime 的 ssh，真正的 server 跑在那边。写进 Host 的 MCP 配置里当启动命令",
                    )
                    .mut_arg("workspace", |a| {
                        a.help("**Runtime** 那边的 workspace 名字。它必须用 `external_mcp` 开放过；这台机器不存 workspace 列表")
                    })
                    .mut_arg("node", |a| {
                        a.help("这台机器配置里的哪个 node。只有一个 node 配了 ssh alias 时可以不给")
                    })
                    .mut_arg("mode", |a| {
                        a.help("要多大的权限。workspace 没开放 `coding` 就拒绝，不会悄悄降成 read")
                    })
                })
        })
        .mut_subcommand("controller", |c| {
            c.about("负责拉起 AI 的后台服务（controller）。这几条要在 Agent Node 上跑，或者 ssh 过去跑：`ssh work ccnm controller install`")
                .mut_subcommand("install", |c| {
                    c.about("把 controller 装成后台服务（macOS 是 LaunchAgent，Linux 是 systemd 用户服务）、起起来，并确认它回话")
                        .mut_arg("dry_run", |a| {
                            a.help("只打印服务文件和要跑的命令，什么都不改")
                        })
                })
                .mut_subcommand("status", |c| {
                    c.about("有没有 controller 在监听，它是怎么跑起来的")
                })
                .mut_subcommand("uninstall", |c| c.about("停掉 controller，删掉它的后台服务"))
        })
}

/// The three arguments `attach`, `status`, `result` and `stop` all share.
fn workspace_args(command: clap::Command) -> clap::Command {
    command.mut_arg("workspace", |a| a.help("config.toml 里的 workspace 名字"))
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

/// Say once what this workspace's switches cost, on the machine the
/// operator typed on.
///
/// Said for the workspace, not for this run: `allow_unattended_exec`
/// describes what interactive sessions here do, and a `--print` run that
/// swallowed the one warning would be the reason nobody ever saw it.
///
/// Best effort by design. A state directory this account cannot write is a
/// reason to say it again next time -- the marker never exists, so the
/// warning always prints -- and never a reason to fail a session over.
fn warn_accepted_risk(
    workspace: &str,
    unisolated_credentials: bool,
    unattended_exec: bool,
    lang: Lang,
) {
    let state = ccnm_core::paths::state_dir().unwrap_or_else(|_| PathBuf::from("/nonexistent"));
    let accepted = ccnm_core::safety::Accepted {
        unconfined_exec: false,
        unisolated_credentials,
        unattended_exec,
    };
    if let Some(text) = ccnm_core::safety::warn_accepted_once_in(&state, workspace, accepted, lang)
    {
        eprintln!("\n{text}\n");
    }
}

/// Which config file this invocation means: `--config` if given, else
/// the standard location. Shared with [`ui_lang`], which has to answer
/// the same question before `run` starts.
fn config_path_for(cli: &Cli) -> Result<PathBuf> {
    match &cli.config {
        Some(path) => Ok(path.clone()),
        None => paths::config_path(),
    }
}

fn run(cli: Cli, lang: Lang) -> Result<i32> {
    let config_path = || -> Result<PathBuf> { config_path_for(&cli) };

    match &cli.command {
        Command::Init { agent, runtime } => {
            init(&config_path()?, agent.as_deref(), runtime.as_deref(), lang)
        }
        Command::Workspace { command } => workspace_command(&config_path()?, command, lang),
        Command::Doctor { workspace, agent } => {
            let path = config_path()?;
            if let Some(workspace) = workspace {
                let config = Config::load(&path)?;
                // On the Agent Node the workspace lives elsewhere, so ask
                // the Runtime what it is and check the rest from here. The
                // whole public command used to be sent over instead, which
                // made the Runtime Executor dial back to this machine for a
                // diagnostic (P7.4 Batch D2).
                if let Some((runtime, node)) = agent_side(&config, workspace) {
                    let report =
                        agent_side_doctor(&path, workspace, agent.as_deref(), runtime, node)?;
                    print!("{}", report.render_in(lang));
                    return Ok(report.exit_code());
                }
            }
            let env = doctor::Env {
                runner: &SystemRunner,
                control_dir: paths::state_dir()?.join("ssh"),
                home: paths::home_dir()?,
            };
            let report = doctor::run_selected(&path, workspace.as_deref(), agent.as_deref(), &env);
            print!("{}", report.render_in(lang));
            Ok(report.exit_code())
        }
        Command::Run {
            workspace,
            agent,
            prompt,
            prompt_stdin,
            print,
            timeout,
            detached,
        } => {
            let config = Config::load(&config_path()?)?;
            // Sitting at the Agent Node: this config knows how to reach the
            // Runtime and nothing about workspaces. So ask the Runtime what
            // this workspace is, and start the session here -- Claude runs
            // on this machine either way, and the Runtime Executor has no
            // business running a launcher (P7.4 Batch C).
            if let Some((runtime, host)) = agent_side(&config, workspace) {
                if print.is_some() {
                    return Err(Error::invalid_args(
                        "--print has to be run where the projects are; ssh there and run it",
                    ));
                }
                let opening = opening_prompt(prompt.as_deref(), *prompt_stdin)?;
                let selected = local_instance_ref(&config, agent.as_deref())?;
                let env = launch_env()?;
                let authority = launcher::resolve_from_agent(
                    runtime,
                    &host.ccnm_bin(),
                    workspace,
                    agent.as_deref(),
                    &env,
                )?;
                warn_accepted_risk(
                    workspace,
                    authority.allow_unisolated_credentials,
                    authority.allow_unattended_exec,
                    lang,
                );
                let tools = agent_tools(config_path().ok().as_deref())?;
                let report = work::start(&start_request(&authority, opening), &tools)?;
                eprintln!("{}", report.summary_in(lang));
                if *detached {
                    eprintln!(
                        "\n{}",
                        lang.pick(
                            format!("想接上的时候：ccnm attach {workspace}"),
                            format!("attach when you want it: ccnm attach {workspace}"),
                        )
                    );
                    return Ok(0);
                }
                return work::attach(&attach_request(workspace, selected, None), &tools);
            }
            let resolved = config.workspace(workspace)?;
            warn_accepted_risk(
                workspace,
                resolved.workspace.allow_unisolated_credentials,
                resolved.workspace.allow_unattended_exec,
                lang,
            );
            let env = launch_env()?;
            if let Some(prompt) = print {
                let rep = launcher::run_print_with_agent(
                    &resolved,
                    &env,
                    prompt,
                    std::time::Duration::from_secs(*timeout),
                    agent.as_deref(),
                )?;
                return print_run_report(&rep, resolved.agent_node(), lang);
            }
            let opening = opening_prompt(prompt.as_deref(), *prompt_stdin)?;
            let rep = launcher::start_interactive_with_agent(
                &resolved,
                &env,
                opening.as_deref(),
                agent.as_deref(),
            )?;
            eprintln!("{}", rep.summary_in(lang));
            if *detached {
                eprintln!(
                    "\n{}",
                    lang.pick(
                        format!("想接上的时候：ccnm attach {workspace}"),
                        format!("attach when you want it: ccnm attach {workspace}"),
                    )
                );
                return Ok(0);
            }
            attach_selected(&resolved, &env, workspace, agent.as_deref(), None, lang)
        }
        Command::Attach {
            workspace,
            agent,
            session,
        } => {
            let config = Config::load(&config_path()?)?;
            if agent_side(&config, workspace).is_some() {
                return work::attach(
                    &attach_request(
                        workspace,
                        local_instance_ref(&config, agent.as_deref())?,
                        session.clone(),
                    ),
                    &agent_tools(config_path().ok().as_deref())?,
                );
            }
            let resolved = config.workspace(workspace)?;
            attach_selected(
                &resolved,
                &launch_env()?,
                workspace,
                agent.as_deref(),
                session.as_deref(),
                lang,
            )
        }
        Command::Status {
            workspace,
            agent,
            session,
            all,
        } => {
            let config = Config::load(&config_path()?)?;
            let Some(workspace) = workspace else {
                print!(
                    "{}",
                    overview(&config, config_path()?.as_path())?.render_status(lang)
                );
                return Ok(0);
            };
            if agent_side(&config, workspace).is_some() {
                let selected = local_instance_ref(&config, agent.as_deref())?;
                let req = StatusRequest {
                    protocol: if selected.is_some() {
                        ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
                    } else {
                        ccnm_core::protocol::payload::PROTOCOL
                    },
                    workspace: (!*all).then(|| workspace.to_string()),
                    agent: selected,
                    session: session.clone(),
                };
                print!(
                    "{}",
                    work::status_checked(&req, &agent_tools(config_path().ok().as_deref())?)?
                        .render_in(lang)
                );
                return Ok(0);
            }
            let resolved = config.workspace(workspace)?;
            let env = launch_env()?;
            let rep = launcher::status_selected(
                &resolved,
                &env,
                *all,
                agent.as_deref(),
                session.as_deref(),
            )?;
            print!("{}", rep.render_in(lang));
            // What the Agent's session list cannot show: a --print run, or
            // an external client, holding the tree (P60). Asked of the
            // Runtime Executor, whose state directory the guard is in.
            print!(
                "{}",
                ccnm_core::overview::render_guard(
                    &launcher::observe_guard(&resolved, &env, agent.as_deref()),
                    ccnm_core::overview::utc_offset(&SystemRunner),
                    lang,
                )
            );
            Ok(0)
        }
        Command::List => {
            let config = Config::load(&config_path()?)?;
            print!(
                "{}",
                overview(&config, config_path()?.as_path())?.render_list(lang)
            );
            Ok(0)
        }
        Command::Log { workspace, limit } => {
            let config = Config::load(&config_path()?)?;
            let entries = session_history(&config, workspace.as_deref(), *limit)?;
            print!(
                "{}",
                ccnm_core::overview::render_history(
                    &entries,
                    ccnm_core::overview::now_secs(),
                    ccnm_core::overview::utc_offset(&SystemRunner),
                    lang,
                )
            );
            Ok(0)
        }
        Command::Result {
            workspace,
            agent,
            session,
        } => {
            let config = Config::load(&config_path()?)?;
            if agent_side(&config, workspace).is_some() {
                let selected = local_instance_ref(&config, agent.as_deref())?;
                let req = ResultRequest {
                    protocol: if selected.is_some() {
                        ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
                    } else {
                        ccnm_core::protocol::payload::PROTOCOL
                    },
                    workspace: workspace.to_string(),
                    agent: selected,
                    session: session.clone(),
                };
                return print_result_report(
                    &work::result(&req, &agent_tools(config_path().ok().as_deref())?)?,
                    // The report was read off this machine's own disk.
                    config.this.as_deref().unwrap_or("this machine"),
                    lang,
                );
            }
            let resolved = config.workspace(workspace)?;
            let rep = launcher::result_selected(
                &resolved,
                &launch_env()?,
                session.as_deref(),
                agent.as_deref(),
            )?;
            print_result_report(&rep, resolved.agent_node(), lang)
        }
        Command::Stop {
            workspace,
            agent,
            session,
        } => {
            let config = Config::load(&config_path()?)?;
            if agent_side(&config, workspace).is_some() {
                let selected = local_instance_ref(&config, agent.as_deref())?;
                let req = StopRequest {
                    protocol: if selected.is_some() {
                        ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
                    } else {
                        ccnm_core::protocol::payload::PROTOCOL
                    },
                    workspace: workspace.to_string(),
                    agent: selected,
                    session: session.clone(),
                    assigned: false,
                };
                let rep = work::stop(&req, &agent_tools(config_path().ok().as_deref())?)?;
                println!(
                    "{}",
                    if rep.killed {
                        format!("stopped {}", rep.tmux_session)
                    } else {
                        format!("no session for {workspace}")
                    }
                );
                return Ok(0);
            }
            let resolved = config.workspace(workspace)?;
            let rep = launcher::stop_selected(
                &resolved,
                &launch_env()?,
                agent.as_deref(),
                session.as_deref(),
            )?;
            let session = &rep.tmux_session;
            if rep.killed {
                println!(
                    "{}",
                    lang.pick(format!("停了 {session}"), format!("stopped {session}"))
                );
            } else {
                println!(
                    "{}",
                    lang.pick(
                        format!("没什么可停的：{session} 本来就没在跑"),
                        format!("nothing to stop: {session} was not running"),
                    )
                );
            }
            Ok(0)
        }
        Command::Cleanup { workspace, apply } => {
            let config = Config::load(&config_path()?)?;
            let resolved = config.workspace(workspace)?;
            let (env, state) = (launch_env()?, paths::state_dir()?);
            match apply {
                None => {
                    let preview = ccnm_core::cleanup::preview(&resolved, &env, &state, false)?;
                    print!("{}", preview.render(lang));
                    Ok(0)
                }
                Some(token) => {
                    let applied = ccnm_core::cleanup::apply(&resolved, &env, &state, token)?;
                    print!("{}", applied.render(lang));
                    // Something planned was not removed, or a machine could
                    // not be asked: say so in the exit code too.
                    Ok(if applied.complete() {
                        0
                    } else {
                        ccnm_core::ErrorCode::NotReady.exit_code()
                    })
                }
            }
        }
        Command::Rpc => {
            // The store lives beside every other bit of ccnm state, so a
            // session started through the API is visible to the same
            // maintenance and cleanup as one started by hand.
            let path = config_path()?;
            let ctx = ccnm_core::rpc::Context {
                config_path: path.clone(),
                state: paths::state_dir()?,
                runs: std::sync::Arc::new(ccnm_core::rpc::session::SystemRuns {
                    config_path: path,
                }),
                runner: std::sync::Arc::new(SystemRunner),
                cursors: Default::default(),
            };
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            ccnm_core::rpc::serve(ctx, stdin.lock(), stdout.lock())?;
            Ok(0)
        }
        // One ssh whose stdin/stdout are the MCP stream. This process
        // does not proxy it: it `exec`s, so there is no supervisor left to
        // leak an orphan transport if the Host kills the bridge.
        Command::Mcp {
            command:
                McpCommand::Bridge {
                    workspace,
                    node,
                    mode,
                },
        } => {
            use std::os::unix::process::CommandExt as _;
            let request = ccnm_core::mcp::bridge::Request {
                workspace: workspace.clone(),
                node: node.clone(),
                mode: if mode == "coding" {
                    ccnm_core::runtime::ExternalMode::Coding
                } else {
                    ccnm_core::runtime::ExternalMode::Read
                },
            };
            let config = Config::load(&config_path()?)?;
            let cmd = ccnm_core::mcp::bridge::command(
                &config,
                &request,
                &ccnm_core::mcp::bridge::session_id(),
            )?;
            let mut process = cmd.process();
            Err(Error::internal("cannot exec the MCP bridge transport").with_source(process.exec()))
        }
        Command::Mcp {
            command:
                McpCommand::Probe {
                    workspace,
                    agent,
                    calls,
                    local,
                },
        } => {
            let config = Config::load(&config_path()?)?;
            // The Agent Node opens the transport itself: it already holds
            // the credential for the one direction that is allowed, and the
            // Runtime Executor must never dial back here (P7.4 Batch D2).
            if let Some((runtime, node)) = agent_side(&config, workspace) {
                if *local {
                    // --local means "serve the project on this machine",
                    // and the project is not here. Refusing beats quietly
                    // measuring the remote transport under the wrong name.
                    return Err(Error::new(
                        ccnm_core::ErrorCode::WrongWorkspace,
                        format!(
                            "--local probes a Runtime on this machine, and {workspace} is on another one\nrun it without --local, or run it where the project is"
                        ),
                    ));
                }
                let rep = agent_side_mcp_probe(
                    &config_path()?,
                    workspace,
                    agent.as_deref(),
                    runtime,
                    node,
                    *calls,
                )?;
                println!("{}", rep.summary());
                println!("{}", payload::to_json(&rep)?);
                return Ok(if rep.single_process {
                    0
                } else {
                    ccnm_core::ErrorCode::Internal.exit_code()
                });
            }
            let resolved = config.workspace(workspace)?;
            let env = launch_env()?;
            let rep = if *local {
                if resolved.agent_reference(agent.as_deref())?.is_some() {
                    return Err(Error::new(
                        ccnm_core::ErrorCode::NotReady,
                        "local MCP probe cannot resolve an Agent-private instance identity; use the remote probe",
                    ));
                }
                launcher::mcp_probe_local(&resolved, &env, *calls)?
            } else {
                launcher::mcp_probe_remote_selected(&resolved, &env, *calls, agent.as_deref())?
            };
            println!("{}", rep.summary());
            println!("{}", payload::to_json(&rep)?);
            Ok(if rep.single_process {
                0
            } else {
                ccnm_core::ErrorCode::Internal.exit_code()
            })
        }
        Command::Controller { command } => controller_command(command, cli.config.as_deref()),
        Command::Internal { command } => match command {
            InternalCommand::Hello { payload } => {
                let req: HelloRequest = payload::decode(payload)?;
                print_json(&hello::answer(&req))
            }
            InternalCommand::AgentTransport { payload } => {
                let req: session::transport::Request = payload::decode(payload)?;
                session::transport::exec(&req)?;
                Ok(0)
            }
            InternalCommand::AgentSkills { payload } => {
                mcp::agent_skills::serve(&ccnm_core::protocol::payload::decode(payload)?)?;
                Ok(0)
            }
            InternalCommand::McpServe { payload } => {
                // Two wire shapes, told apart by their protocol number: the
                // caller-supplied root the launcher still sends, and the
                // Runtime-authority open of P7.4 Batch B. Nothing falls
                // back — an unknown number is CCNM_E_VERSION.
                match ccnm_core::runtime::decode_serve(payload)? {
                    ccnm_core::runtime::ServeRequest::Legacy(req) => mcp::server::serve(&req)?,
                    ccnm_core::runtime::ServeRequest::Managed(req) => {
                        mcp::server::serve_managed(&req)?
                    }
                    // An external MCP client (P10). Same server, same
                    // tools; what it gets is decided here from this
                    // machine's own `external_mcp`, not from the payload.
                    ccnm_core::runtime::ServeRequest::External(req) => {
                        mcp::server::serve_external(&req)?
                    }
                }
                Ok(0)
            }
            InternalCommand::RuntimeResolve { payload } => {
                // The Agent Node asking what a workspace is. This runs as
                // the Runtime Executor, which is exactly the point: the
                // authority answers, and answering needs no outbound
                // connection of its own.
                let req: ccnm_core::runtime::ResolveRequest = payload::decode(payload)?;
                let config = Config::load(&config_path()?)?;
                print_json(&ccnm_core::runtime::resolve(&config, &req)?)
            }
            InternalCommand::RuntimeAudit { payload } => {
                // Doctor cannot answer this where it runs: its identity
                // checks judge the calling process. This one is called over
                // the Agent's ssh, so it runs as the account that will
                // really execute the tools.
                let req: ccnm_core::runtime::AuditRequest = payload::decode(payload)?;
                let config = Config::load(&config_path()?)?;
                print_json(&ccnm_core::runtime::audit(&config, &req, &SystemRunner)?)
            }
            InternalCommand::RuntimeGuard { payload } => {
                // Like the audit, meaningful only as the account the
                // Agent's ssh lands on: the guard is in *its* state
                // directory, the one `mcp-serve` takes it in.
                let req: ccnm_core::runtime::GuardRequest = payload::decode(payload)?;
                let config = Config::load(&config_path()?)?;
                print_json(&ccnm_core::runtime::guard(
                    &config,
                    &req,
                    &paths::state_dir()?,
                    &SystemRunner,
                )?)
            }
            InternalCommand::AgentGuard { payload } => {
                let req: ccnm_core::protocol::run::AgentGuardRequest = payload::decode(payload)?;
                print_json(&work::guard(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::RuntimeCleanup { payload } => {
                // As the account the Agent's ssh lands on: what it removes
                // is in *its* state directory, which is the point.
                let req: ccnm_core::cleanup::RuntimeCleanupRequest = payload::decode(payload)?;
                let config = Config::load(&config_path()?)?;
                print_json(&ccnm_core::cleanup::runtime(
                    &config,
                    &req,
                    &paths::state_dir()?,
                    &SystemRunner,
                )?)
            }
            InternalCommand::AgentCleanup { payload } => {
                let req: ccnm_core::cleanup::AgentCleanupRequest = payload::decode(payload)?;
                print_json(&ccnm_core::cleanup::agent(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::RpcRun { handle } => {
                ccnm_core::rpc::session::run_owned(&paths::state_dir()?, config_path()?, handle)?;
                Ok(0)
            }
            InternalCommand::Controller => {
                let socket = paths::controller_socket(&paths::state_dir()?);
                let listener = controller::Listener::bind(&socket)?;
                let controller_config_path = config_path()?;
                let tools = controller::Tools {
                    runner: &SystemRunner,
                    // Resolved here, in the service manager's environment
                    // (launchd or systemd), because that is the PATH
                    // Claude will actually be started with.
                    agents: ccnm_core::provider::AgentBinaries::discover(),
                    config: Config::load(&controller_config_path).unwrap_or_default(),
                    local: ccnm_core::instance::AgentLocal::load().ok(),
                    config_path: Some(controller_config_path),
                    // Same reason as agent: the service's PATH is not a
                    // login shell's, and the tmux server has to be started
                    // from here to be in the login session (macOS) and to
                    // outlive the ssh connection (both).
                    tmux: tmux::locate_from_env(),
                    exe: std::env::current_exe()?,
                    host: controller::Host::current(),
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
                print_json(&work::probe(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                ))
            }
            InternalCommand::AgentRun { payload } => {
                let req: RunRequest = payload::decode(payload)?;
                print_json(&work::run(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentStart { payload } => {
                let req: StartRequest = payload::decode(payload)?;
                print_json(&work::start(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::Attach { payload } => {
                let req: AttachRequest = payload::decode(payload)?;
                work::attach(&req, &agent_tools(config_path().ok().as_deref())?)
            }
            InternalCommand::AgentStop { payload } => {
                let req: StopRequest = payload::decode(payload)?;
                print_json(&work::stop(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentStatus { payload } => {
                let req: StatusRequest = payload::decode(payload)?;
                print_json(&work::status_checked(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentHistory { payload } => {
                let req: HistoryRequest = payload::decode(payload)?;
                print_json(&work::history(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentResult { payload } => {
                let req: ResultRequest = payload::decode(payload)?;
                print_json(&work::result(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentOutput { payload } => {
                let req: ccnm_core::protocol::run::OutputRequest = payload::decode(payload)?;
                print_json(&work::output(
                    &req,
                    &agent_tools(config_path().ok().as_deref())?,
                )?)
            }
            InternalCommand::AgentPurge { payload } => {
                let req: PurgeRequest = payload::decode(payload)?;
                work::purge(&req)?;
                Ok(0)
            }
        },
    }
}

/// `ccnm init`: the two ssh aliases, written to the config.
///
/// Everything else has a default. Running it again is not an error and
/// not a rewrite: it reports what it changed, or that there was nothing
/// to change.
fn init(
    path: &std::path::Path,
    agent: Option<&str>,
    runtime: Option<&str>,
    lang: Lang,
) -> Result<i32> {
    let mut edit = configedit::Edit::open(path)?;
    let existed = edit.existed();
    let mut changes = configedit::Changes::default();
    // One alias, and it names the *other* node: an ssh alias only means
    // something on the machine that dials it, so this file only ever
    // describes how this machine dials out. Which flag was given is also
    // what this machine is, so `this` comes from the same answer and
    // nobody has to state their own identity twice.
    let (this, other, alias) = match (agent, runtime) {
        (Some(alias), None) => ("runtime", "agent", alias),
        (None, Some(alias)) => ("agent", "runtime", alias),
        // clap's conflicts_with/required_unless_present make both of these
        // unreachable from the CLI; the arms exist so the function is
        // total for callers that are not clap.
        (Some(_), Some(_)) => {
            return Err(ccnm_core::Error::invalid_args(lang.pick(
                "--agent 和 --runtime 各自说明这台机器是谁，所以只能给一个\n  放项目的那台：      ccnm init --agent <alias>\n  跑 Claude 的那台：  ccnm init --runtime <alias>",
                "--agent and --runtime each say which node this machine is, so only one of them can be true here\n  on the machine holding the projects:  ccnm init --agent <alias>\n  on the machine running Claude:        ccnm init --runtime <alias>",
            )));
        }
        (None, None) => {
            return Err(ccnm_core::Error::invalid_args(lang.pick(
                "要给出另一台机器的别名：\n  放项目的那台：      ccnm init --agent <alias>\n  跑 Claude 的那台：  ccnm init --runtime <alias>",
                "give the alias for the other node:\n  on the machine holding the projects:  ccnm init --agent <alias>\n  on the machine running Claude:        ccnm init --runtime <alias>",
            )));
        }
    };
    edit.set_this(this, &mut changes);
    // An Agent Node says outright that the workspace list is elsewhere.
    // Without it a Runtime Node that has no projects yet is the same file,
    // and whichever way that is guessed, one of the two bounces requests
    // to a machine that bounces them back.
    if this == "agent" {
        edit.set_delegate(other, &mut changes);
    }
    // The node this machine is gets a table of its own even with nothing
    // in it: `this` has to name a [nodes.*] entry, and a config that
    // fails its own validation the moment it is written is not a config.
    edit.ensure_node(this, &mut changes);
    edit.set_node(other, "ssh", alias, &mut changes);
    edit.save(&changes)?;

    if !existed {
        let path = path.display();
        println!(
            "{}",
            lang.pick(format!("写好了 {path}"), format!("wrote {path}"))
        );
    }
    report_changes(&changes, path, lang);
    if !existed || changes.lines().iter().any(|l| l.contains("workspaces")) {
        println!();
    }
    let config = Config::load(path)?;
    if this == "agent" {
        println!(
            "{}",
            lang.pick(
                format!("这台是 Agent Node：碰到不认识的 workspace，它会去问 Runtime Node {alias}"),
                format!(
                    "this Agent Node will ask Runtime Node {alias} for a workspace it does not know"
                ),
            )
        );
        println!("{}", lang.pick("接着在这台上：", "next, from here:"));
        println!(
            "  {}",
            lang.pick(
                "ccnm <workspace>       在那边起会话，在这边接上",
                "ccnm <workspace>       start it there, attach here",
            )
        );
    } else if config.workspaces.is_empty() {
        println!(
            "{}",
            lang.pick(
                "接着：cd 到这台机器上的一个项目里，然后",
                "next: cd to a project on this machine and run",
            )
        );
        println!("  ccnm workspace add <name>");
    }
    // ssh has to work before anything else can; say so plainly rather than
    // testing it here, where a slow or absent network would turn `init`
    // into something that hangs.
    println!(
        "\n{}",
        lang.pick(
            "下面这条必须不用输密码就能过：",
            "this must work without a password:",
        )
    );
    println!("  ssh {alias} true");
    if this == "runtime" {
        println!(
            "\n{}",
            lang.pick(
                "反过来 Agent Node 也得够得到这台。那是它自己的配置，要在那边写：",
                "and the Agent Node must be able to reach back, which is its own\nconfig, written there:",
            )
        );
        // Placeholders stay English wherever they sit inside a command
        // somebody retypes. Mixing `<别名>` into one line and `<name>`
        // into the next is worse than either choice on its own.
        println!("  ssh {alias} ccnm init --runtime <this machine's alias>");
    }
    Ok(0)
}

/// `path` exactly as written, when that is unambiguous: absolute, with no
/// `.` or `..` in it. For a root this account is not allowed to resolve.
fn spelled_out(path: &std::path::Path) -> Option<PathBuf> {
    use std::path::Component;
    let plain = path
        .components()
        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    (path.is_absolute() && plain).then(|| path.components().collect())
}

fn workspace_command(
    path: &std::path::Path,
    command: &WorkspaceCommand,
    lang: Lang,
) -> Result<i32> {
    match command {
        WorkspaceCommand::Add {
            name,
            path: root,
            replace,
            allow_unconfined_exec,
            permission_mode,
            agent_node,
        } => {
            let root = match root {
                Some(path) => path.clone(),
                None => std::env::current_dir()?,
            };
            // Canonical, because a workspace root is compared against what
            // a running session was started with, and `.`, `~/x/../x` and
            // a symlinked path are all the same directory with three
            // different spellings.
            let mut unseen = false;
            let root = match root.canonicalize() {
                Ok(root) => root,
                // Registering a project is the Operator's job, and the
                // project is the Runtime Executor's: in the Executor's home,
                // behind a 0700 the Operator cannot see through. That is the
                // layout the manual recommends, and this used to refuse it
                // as "not a directory" (F1). What cannot be resolved is
                // taken as written -- but only when there is one way to read
                // it.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    unseen = true;
                    spelled_out(&root).ok_or_else(|| {
                        ccnm_core::Error::new(
                            ccnm_core::ErrorCode::WrongWorkspace,
                            format!(
                                "{} cannot be resolved from this account (Permission denied), and as written it is not an absolute path without `.` or `..`\ngive the full path the Runtime account uses for the project",
                                root.display()
                            ),
                        )
                    })?
                }
                Err(e) => {
                    return Err(ccnm_core::Error::new(
                        ccnm_core::ErrorCode::WrongWorkspace,
                        format!("{} is not a directory on this machine", root.display()),
                    )
                    .with_source(e));
                }
            };
            let name = match name {
                Some(name) => name.clone(),
                None => name_from(&root)?,
            };
            check_collisions(path, &name, &root, *replace)?;
            let mode = permission_mode
                .as_deref()
                .map(parse_permission_mode)
                .transpose()?;
            let (agent_node, runtime_node) = workspace_nodes(path, agent_node.as_deref())?;
            let mut edit = configedit::Edit::open(path)?;
            let mut changes = configedit::Changes::default();
            edit.set_workspace(
                &name,
                &root,
                &agent_node,
                mode,
                allow_unconfined_exec.then_some(true),
                &mut changes,
            );
            if let Some(node) = &runtime_node {
                edit.set_workspace_runtime_node(&name, node, &mut changes);
            }
            edit.save(&changes).map_err(|e| {
                if edit.existed() {
                    e
                } else {
                    ccnm_core::Error::config(format!(
                        "there is no config yet, so a workspace has nowhere to go\nrun this first: ccnm init --agent <alias> --runtime <alias>\n({})",
                        e.message()
                    ))
                }
            })?;
            report_changes(&changes, path, lang);
            if unseen {
                let who = std::env::var("USER").unwrap_or_else(|_| "this account".into());
                let root = root.display();
                println!(
                    "\n{}",
                    lang.pick(
                        format!(
                            "注意：{who} 没有权限看 {root}（Permission denied），所以按你写的路径登记，没有核对它在不在、也没有解析符号链接。\n      真正用这个目录的执行账号在开会话时核对；`ccnm doctor {name}` 的「workspace 根目录」一行是它的回答。"
                        ),
                        format!(
                            "note: {who} could not look at {root} (Permission denied), so it is registered as written: not checked to exist, symlinks not resolved.\n      the account that runs the tools checks it when a session opens; its answer is the `Workspace root` row of `ccnm doctor {name}`."
                        ),
                    )
                );
            }
            // Two labels padded to the same column, so the commands line
            // up. `pad` rather than hand-counted spaces, because the
            // Chinese labels are not the width their character count says.
            let (check, use_) = lang.pick(("查一下：", "开始用："), ("check it:", "use it:  "));
            println!("\n{} ccnm doctor {name}", crate::pad_label(check));
            println!("{} ccnm {name}", crate::pad_label(use_));
            Ok(0)
        }
        WorkspaceCommand::List => {
            let config = Config::load(path)?;
            if config.workspaces.is_empty() {
                let path = path.display();
                println!(
                    "{}",
                    lang.pick(
                        format!("{path} 里一个 workspace 都没有"),
                        format!("no workspaces in {path}"),
                    )
                );
                println!(
                    "{}",
                    lang.pick(
                        "加一个：cd 到项目目录里，然后 `ccnm workspace add <name>`",
                        "add one: cd to a project and run `ccnm workspace add <name>`",
                    )
                );
                return Ok(0);
            }
            // Workspace names are `[A-Za-z0-9_-]+` (config::check_name,
            // enforced when the config loads), so bytes, chars and columns
            // are the same number and `{name:width$}` is still right.
            let width = config.workspaces.keys().map(String::len).max().unwrap_or(0);
            for (name, workspace) in &config.workspaces {
                let here = match paths::see_dir(&workspace.root) {
                    paths::Seen::Dir => "",
                    // Not a claim this account can make (F1).
                    paths::Seen::Hidden => lang.pick(
                        "   （这个账号没权限看）",
                        "   (this account may not look at it)",
                    ),
                    _ => lang.pick("   （不在这台机器上）", "   (not on this machine)"),
                };
                println!("{name:width$}  {}{here}", workspace.root.display());
            }
            Ok(0)
        }
        WorkspaceCommand::Remove { name, purge } => remove_workspace(path, name, *purge, lang),
    }
}

/// Pad a short label to a fixed column by display width, so the commands
/// after it line up whichever language the label is in.
fn pad_label(label: &str) -> String {
    ccnm_core::lang::pad(label, 9)
}

/// Forget a workspace, after ending anything of it that is still running.
///
/// Ending the session first is not optional: a session outlives the
/// config, so a workspace removed while one is up would leave a Claude
/// running against a project nothing points at any more, and no command
/// left that names it.
fn remove_workspace(path: &std::path::Path, name: &str, purge: bool, lang: Lang) -> Result<i32> {
    // Best effort, and in this order: the session belongs to the config
    // entry that is about to go.
    if let Ok(config) = Config::load(path)
        && let Ok(resolved) = config.workspace(name)
    {
        match launcher::stop(&resolved, &launch_env()?) {
            Ok(rep) if rep.killed => {
                let session = &rep.tmux_session;
                println!(
                    "{}",
                    lang.pick(format!("停了 {session}"), format!("stopped {session}"))
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!(
                "{}",
                lang.pick(
                    format!("够不到 Agent Node，没能把会话停掉：{e}"),
                    format!("could not reach the Agent Node to stop it: {e}"),
                )
            ),
        }
        if purge {
            // The same cleanup `ccnm cleanup` previews, applied at once:
            // `--purge` is the confirmation. What it cannot remove keeps the
            // workspace in the config, because the config is the only thing
            // that says where the rest is.
            let kept =
                match ccnm_core::cleanup::purge(&resolved, &launch_env()?, &paths::state_dir()?) {
                    Ok(applied) => {
                        print!("{}", applied.render(lang));
                        (!applied.nothing_left()).then(String::new)
                    }
                    Err(e) => Some(e.to_string()),
                };
            if let Some(why) = kept {
                eprintln!(
                    "{}",
                    lang.pick(
                        format!("{why}\n{name} 还留在配置里：还有东西没清掉，删了配置就再也找不到它们。处理完再跑一次 `ccnm workspace remove {name} --purge`；只想忘掉这个 workspace、不清数据，就去掉 --purge"),
                        format!("{why}\n{name} stays in the config: something was not removed, and without the config nothing could find it again. Fix that and run `ccnm workspace remove {name} --purge` again, or drop --purge to forget the workspace and leave the data"),
                    )
                    .trim_start()
                );
                return Ok(ccnm_core::ErrorCode::NotReady.exit_code());
            }
        }
    }

    let mut edit = configedit::Edit::open(path)?;
    let mut changes = configedit::Changes::default();
    if !edit.remove_workspace(name, &mut changes) {
        let where_ = path.display();
        println!(
            "{}",
            lang.pick(
                format!("{where_} 里没有 {name}"),
                format!("{name} is not in {where_}"),
            )
        );
        return Ok(0);
    }
    edit.save(&changes)?;
    report_changes(&changes, path, lang);
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
/// The nodes a new workspace names: the Agent's, and the Runtime's when it
/// is not the default `runtime` (F24).
///
/// `workspace add` runs where the project is, so the Runtime is `this`. It
/// used to write `agent_node = "agent"` and leave `runtime_node` at
/// `runtime` whatever the file called its nodes, and a config with other
/// names -- the P62 one had hpsrv, fodelf and xdwmbp -- refused the change.
/// The Agent is still `agent` when there is one, so a config from
/// `ccnm init` gets what it always got; otherwise it is the one other node,
/// and with several it is the person's to name. Guessing there would
/// quietly send the workspace's sessions to the wrong machine.
fn workspace_nodes(
    config_path: &std::path::Path,
    asked: Option<&str>,
) -> Result<(String, Option<String>)> {
    // No config yet, or one that will not load: the old defaults, and
    // `save` says what is wrong, as it always has.
    let Ok(config) = Config::load(config_path) else {
        return Ok((asked.unwrap_or("agent").to_string(), None));
    };
    let this = config.this.as_deref();
    let runtime = this
        .filter(|node| *node != ccnm_core::config::DEFAULT_RUNTIME_NODE)
        .map(str::to_string);
    if let Some(node) = asked {
        return Ok((node.to_string(), runtime));
    }
    if config.nodes.contains_key("agent") {
        return Ok(("agent".to_string(), runtime));
    }
    let others: Vec<&str> = config
        .nodes
        .keys()
        .map(String::as_str)
        .filter(|node| Some(*node) != this)
        .collect();
    match others.as_slice() {
        [only] => Ok((only.to_string(), runtime)),
        [] => Err(ccnm_core::Error::config(format!(
            "{} has no node besides this one to run the Agent\nadd one first: ccnm init --agent <alias>",
            config_path.display()
        ))),
        several => Err(ccnm_core::Error::invalid_args(format!(
            "{} has more than one node that could run this workspace's Agent: {}\nsay which: --agent-node <node>",
            config_path.display(),
            several.join(", ")
        ))),
    }
}

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
    AgentProvider::current().parse_permission_mode(raw)
}

fn report_changes(changes: &configedit::Changes, path: &std::path::Path, lang: Lang) {
    if changes.is_empty() {
        let path = path.display();
        println!(
            "{}",
            lang.pick(
                format!("{path} 里已经是这么写的了"),
                format!("{path} already says that"),
            )
        );
        return;
    }
    // Not translated: each line is `added workspaces.demo` or
    // `nodes.agent.ssh = work-alias`. The words are config keys, and a
    // key rendered in Chinese is a key nobody can find in the file.
    for line in changes.lines() {
        println!("{line}");
    }
}

/// The Runtime Node to delegate to and its settings, when this machine is
/// an Agent Node that was asked for a workspace it does not define.
///
/// The first test is "does this config define the workspace being asked
/// for", because a Runtime Node config with a typo'd workspace name must
/// fall through to the normal error rather than be shipped over ssh. Only
/// a config with no workspace list at all -- which is what an Agent Node
/// keeps, deliberately -- has anywhere to forward the question to.
fn agent_side<'a>(
    config: &'a Config,
    workspace: &str,
) -> Option<(&'a str, &'a ccnm_core::config::Node)> {
    // A known but not-yet-executable instance workspace is not a missing
    // definition. Do not turn its refusal into delegation or legacy attach.
    if config.workspaces.contains_key(workspace) {
        return None;
    }
    config.runtime_from_agent()
}

/// The line Claude opens with: typed here, or read from stdin.
///
/// stdin exists because of the Agent Node. A prompt is free text, and
/// nothing that would need shell quoting is allowed on a remote command
/// line, so the Agent Node cannot put one in the `ccnm run` it sends
/// home -- it pipes the bytes down the same connection and passes
/// `--prompt-stdin`. The flag is not hidden: piping a prompt in is just as
/// useful by hand, and a heredoc keeps the newlines that a shell argument
/// would fight you over.
///
/// Empty input is refused rather than treated as "no prompt". An empty
/// prompt looks exactly like the bug this replaced -- a sentence typed on
/// the Agent Node, silently dropped, Claude opening with nothing -- and
/// the whole point is that that failure is now audible.
fn opening_prompt(prompt: Option<&str>, from_stdin: bool) -> Result<Option<String>> {
    if !from_stdin {
        return Ok(prompt.map(str::to_string));
    }
    use std::io::Read;
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    if text.trim().is_empty() {
        return Err(Error::invalid_args(
            "--prompt-stdin, but nothing arrived on stdin\nit reads the opening line from stdin:  echo 'fix the failing test' | ccnm <workspace> --prompt-stdin",
        ));
    }
    Ok(Some(text.trim_end().to_string()))
}

fn attach_request(
    workspace: &str,
    agent: Option<ccnm_core::instance::InstanceRef>,
    session: Option<String>,
) -> AttachRequest {
    let protocol = if agent.is_some() {
        ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
    } else {
        ccnm_core::protocol::payload::PROTOCOL
    };
    AttachRequest {
        protocol,
        workspace: workspace.to_string(),
        agent,
        session,
    }
}

/// The start the Agent Node makes for itself, from what the Runtime said.
///
/// Every field is the Runtime's answer, unedited: this side supplies only
/// the opening prompt, which is the one thing the Runtime never sees.
fn start_request(
    authority: &ccnm_core::runtime::ResolveReport,
    prompt: Option<String>,
) -> StartRequest {
    StartRequest {
        protocol: if authority.agent.is_some() {
            ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            ccnm_core::protocol::payload::PROTOCOL
        },
        provider: Default::default(),
        agent: authority.agent.clone(),
        workspace: authority.workspace.clone(),
        root: authority.root.clone(),
        runtime_node: authority.runtime_node.clone(),
        provider_config_dir: authority.provider_config_dir.clone(),
        permission_mode: authority.permission_mode,
        prompt,
        agent_tools: authority.agent_tools.clone(),
    }
}

fn local_instance_ref(
    config: &Config,
    agent: Option<&str>,
) -> Result<Option<ccnm_core::instance::InstanceRef>> {
    let Some(instance) = agent else {
        return Ok(None);
    };
    let node = config
        .this
        .as_ref()
        .ok_or_else(|| Error::config("--agent on the Agent Node requires `this`"))?;
    let reference = ccnm_core::instance::InstanceRef {
        node: node.clone(),
        instance: instance.to_string(),
    };
    reference.validate()?;
    Ok(Some(reference))
}

/// `ccnm doctor <workspace>` as run on the Agent Node.
///
/// Two questions cross, both inbound to the Runtime Executor: what is this
/// workspace, and what does that account say about itself. Everything else
/// -- controller, official CLI, login, tmux -- is here, and is checked
/// here. Nothing asks the Runtime to run a public command, so nothing makes
/// it dial back (P7.4 Batch D2).
fn agent_side_doctor(
    config_path: &std::path::Path,
    workspace: &str,
    agent: Option<&str>,
    runtime: &str,
    node: &ccnm_core::config::Node,
) -> Result<doctor::Report> {
    let env = launch_env()?;
    let authority = launcher::resolve_from_agent(runtime, &node.ccnm_bin(), workspace, agent, &env);
    let authority = match authority {
        Ok(authority) => authority,
        Err(e) => return Ok(doctor::from_agent(config_path, workspace, Err(e))),
    };
    let tools = agent_tools(Some(config_path))?;
    let probe = work::probe(&probe_request(&authority, 1), &tools);
    Ok(doctor::from_agent(
        config_path,
        workspace,
        Ok((&authority, &probe)),
    ))
}

/// `ccnm mcp probe <workspace>` as run on the Agent Node: one real MCP
/// session opened from here to the Runtime Executor.
fn agent_side_mcp_probe(
    config_path: &std::path::Path,
    workspace: &str,
    agent: Option<&str>,
    runtime: &str,
    node: &ccnm_core::config::Node,
    calls: u32,
) -> Result<ccnm_core::protocol::mcp::ProbeReport> {
    let env = launch_env()?;
    let authority =
        launcher::resolve_from_agent(runtime, &node.ccnm_bin(), workspace, agent, &env)?;
    let tools = agent_tools(Some(config_path))?;
    work::mcp_probe(&probe_request(&authority, calls), &tools)
}

/// The probe request an Agent-side diagnostic makes from the Runtime's own
/// answer. Every field is the Runtime's; this side adds only how many MCP
/// calls to make.
fn probe_request(authority: &ccnm_core::runtime::ResolveReport, mcp_calls: u32) -> ProbeRequest {
    ProbeRequest {
        protocol: if authority.agent.is_some() {
            ccnm_core::instance::INSTANCE_SESSION_PROTOCOL
        } else {
            ccnm_core::protocol::payload::PROTOCOL
        },
        provider: Default::default(),
        agent: authority.agent.clone(),
        workspace: authority.workspace.clone(),
        root: authority.root.clone(),
        runtime_node: authority.runtime_node.clone(),
        provider_config_dir: authority.provider_config_dir.clone(),
        mcp_calls,
    }
}

/// Is this the Agent Node's config: no workspaces of its own, and a
/// Runtime Node to ask about them?
fn is_agent_node(config: &Config) -> bool {
    config.workspaces.is_empty() && config.runtime_from_agent().is_some()
}

/// Every managed project, from whichever machine this is.
///
/// On the Agent Node that is its own tmux sessions: the workspace list and
/// the processes serving them are on the Runtime, and asking the Runtime
/// to look at itself would have it dial back here (P7.4 Batch D2).
fn overview(
    config: &Config,
    config_path: &std::path::Path,
) -> Result<ccnm_core::overview::Overview> {
    if is_agent_node(config) {
        let req = StatusRequest {
            protocol: ccnm_core::protocol::payload::PROTOCOL,
            workspace: None,
            agent: None,
            session: None,
        };
        let report = work::status_checked(&req, &agent_tools(Some(config_path))?)?;
        return Ok(ccnm_core::overview::from_agent_status(&report));
    }
    Ok(ccnm_core::overview::collect(
        config,
        &launch_env()?,
        &paths::state_dir()?,
    ))
}

/// Session records for `ccnm log`: one call per Agent Node, merged.
fn session_history(
    config: &Config,
    workspace: Option<&str>,
    limit: u32,
) -> Result<Vec<ccnm_core::protocol::run::HistoryEntry>> {
    let req = HistoryRequest {
        protocol: ccnm_core::protocol::payload::PROTOCOL,
        workspace: workspace.map(str::to_string),
        limit,
    };
    if is_agent_node(config) {
        return Ok(work::history(&req, &agent_tools(None)?)?.sessions);
    }
    let env = launch_env()?;
    let names: Vec<&str> = match workspace {
        Some(name) => vec![config.workspace(name)?.name],
        None => config.workspaces.keys().map(String::as_str).collect(),
    };
    let mut asked = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    for name in names {
        let resolved = config.workspace(name)?;
        let Ok(alias) = resolved.agent_ssh() else {
            continue;
        };
        if asked.insert(alias.to_string()) {
            entries.extend(launcher::history(&resolved, &env, workspace, limit)?.sessions);
        }
    }
    // An Agent Node can serve other Runtimes too; this lists what this
    // config manages.
    entries.retain(|e| config.workspaces.contains_key(&e.workspace));
    entries.sort_by_key(|e| std::cmp::Reverse(e.started));
    entries.truncate(limit as usize);
    Ok(entries)
}

/// The Agent Node's own view of the world.
///
/// A missing or broken config is not fatal here: `attach`, `status`,
/// `stop` and `result` answer from this machine's own session files and
/// never need one. The commands that do need it -- the ones that dial the
/// Runtime Node -- fail in `runtime_link` with a sentence naming the node
/// they could not find, which beats failing every command with a parse
/// error about a file most of them never read.
fn agent_tools(config_path: Option<&std::path::Path>) -> Result<work::Tools<'static>> {
    let state = paths::state_dir()?;
    let config = config_path
        .and_then(|p| Config::load(p).ok())
        .unwrap_or_default();
    Ok(work::Tools {
        config,
        local: ccnm_core::instance::AgentLocal::load().ok(),
        runner: &SystemRunner,
        control_dir: state.join("ssh"),
        agents: ccnm_core::provider::AgentBinaries::discover(),
        tmux: tmux::locate_from_env(),
        controller: paths::controller_socket(&state),
        state,
    })
}

fn launch_env() -> Result<launcher::Env<'static>> {
    Ok(launcher::Env {
        runner: &SystemRunner,
        control_dir: paths::state_dir()?.join("ssh"),
        current_exe: std::env::current_exe()?,
    })
}

/// Give this terminal to the Agent Node's tmux and stay out of the way
/// until it comes back.
///
/// Not `exec`: when the person detaches or Claude ends, there is one more
/// useful thing to say — whether the session is still running — and a
/// process that replaced itself with ssh cannot say it.
fn attach_selected(
    resolved: &ccnm_core::config::Resolved<'_>,
    env: &launcher::Env<'_>,
    workspace: &str,
    agent: Option<&str>,
    session: Option<&str>,
    lang: Lang,
) -> Result<i32> {
    let cmd = launcher::attach_cmd_selected(resolved, env, agent, session)?;
    let captured = ccnm_core::process::run_attached(&cmd)?;
    let code = captured.exit_code.unwrap_or(1);
    // Before the status lookup below: that is another ssh and up to ten
    // seconds, and every mouse move meanwhile would print as garbage.
    restore_terminal(&captured);
    match launcher::status_selected(resolved, env, false, agent, session) {
        Ok(rep) if !rep.sessions.is_empty() => {
            eprintln!(
                "\n{}",
                lang.pick(
                    format!("会话还在 Agent Node 上跑着，回去：ccnm attach {workspace}"),
                    format!(
                        "still running on the Agent Node; back in with: ccnm attach {workspace}"
                    ),
                )
            );
        }
        Ok(_) => eprintln!("\n{}", lang.pick("会话结束了", "the session has ended")),
        // The session's own exit code is worth more than a failure to look
        // it up afterwards.
        Err(e) => eprintln!(
            "\n{}",
            lang.pick(
                format!("说不好会话还在不在跑：{e}"),
                format!("cannot tell whether the session is still running: {e}"),
            )
        ),
    }
    Ok(code)
}

/// Switch off what the remote tmux client switched on, in case it never
/// could (see [`tmux::client_terminal_reset`]).
///
/// ssh exits 255 when the connection failed, and with no code when a
/// signal took it. But 255 also means it never connected, when tmux never
/// took the screen -- and leaving an alternate screen nobody entered moves
/// the cursor (Ghostty, like xterm, restores it unconditionally). So
/// "lost" also needs time: a failed connect gives up within the attach
/// ssh's `ConnectTimeout=10`, while an established one is not declared
/// dead before `ServerAliveInterval=15` x `ServerAliveCountMax=3` = 45s
/// (`Ssh::options`). A connection reset sooner than 30s leaves the
/// alternate screen up; the mouse is switched off either way.
fn restore_terminal(attach: &ccnm_core::process::Captured) {
    use std::io::{IsTerminal as _, Write as _};
    let mut out = std::io::stdout();
    if !out.is_terminal() {
        return;
    }
    let lost = matches!(attach.exit_code, None | Some(255))
        && attach.duration >= std::time::Duration::from_secs(30);
    let _ = out.write_all(tmux::client_terminal_reset(lost).as_bytes());
    let _ = out.flush();
}

/// The summary, then Claude's answer, then whatever went wrong. Exit 0
/// only when Claude ran to completion and did not report an error itself.
fn print_run_report(rep: &RunReport, agent_node: &str, lang: Lang) -> Result<i32> {
    println!("{}", rep.summary_in(lang));
    match &rep.result {
        Some(r) => {
            println!("\n--- result ---");
            println!("{}", r.text().unwrap_or("").trim_end());
            if !r.permission_denials().is_empty() {
                eprintln!("\npermission denials:");
                for d in r.permission_denials() {
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
    // By the node's own name. It said "on work" for every node, which was a
    // name from before nodes had names (F18).
    eprintln!(
        "\nsession directory on {agent_node}: {}",
        rep.session_dir.display()
    );
    let ok = rep.outcome.ok() && rep.result.as_ref().is_some_and(|r| !r.is_error());
    Ok(if ok { 0 } else { 1 })
}

/// What `ccnm result` prints, from whichever machine asked.
///
/// One function because the two machines get their report from different
/// places -- home over ssh, the Agent Node off its own disk -- and the
/// person reading it should not be able to tell which they are looking at.
/// Two copies of this drifted apart the moment one of them was edited.
///
/// Always exit 0: this reports on a session, it does not run one, and a
/// non-zero exit here would say "the lookup failed" about a lookup that
/// worked.
fn print_result_report(
    rep: &ccnm_core::protocol::run::ResultReport,
    agent_node: &str,
    lang: Lang,
) -> Result<i32> {
    println!("{}", rep.summary_in(lang));
    match &rep.result {
        Some(r) => {
            println!("\n--- result ---");
            println!("{}", r.text().unwrap_or("").trim_end());
        }
        None if !rep.stdout_tail.is_empty() => {
            println!("\n--- stdout (tail) ---\n{}", rep.stdout_tail.trim_end());
        }
        None => {}
    }
    if !rep.stderr_tail.trim().is_empty() {
        eprintln!("\n--- stderr (tail) ---\n{}", rep.stderr_tail.trim_end());
    }
    // By the node's own name. It said "on work" for every node, which was a
    // name from before nodes had names (F18).
    eprintln!(
        "\nsession directory on {agent_node}: {}",
        rep.session_dir.display()
    );
    Ok(0)
}

/// `ccnm controller ...`, run on the Agent Node.
///
/// A controller that is running but not in a login session exits
/// `CCNM_E_NOT_READY` rather than 0: it answers, so nothing is broken, but
/// it cannot do the one job it exists for, and a green exit code there
/// would be the same lie this whole component was built to stop telling.
///
/// `config` is `--config` / `CCNM_CONFIG`: the controller reads no config
/// of its own at install time, but the one launchd starts must read this
/// one, so it goes into the plist with the XDG locations (F8).
fn controller_command(command: &ControllerCommand, config: Option<&Path>) -> Result<i32> {
    match controller::Host::current() {
        controller::Host::MacOs => {}
        controller::Host::Linux => return systemd_controller_command(command, config),
        controller::Host::Other => {
            return Err(ccnm_core::Error::new(
                ccnm_core::ErrorCode::NotReady,
                "ccnm has no controller for this operating system yet; an Agent Node is macOS or Linux",
            ));
        }
    }
    let state = paths::state_dir()?;
    let socket = paths::controller_socket(&state);
    let plan = || -> Result<launchagent::Plan> {
        launchagent::Plan::new(
            &paths::home_dir()?,
            &state,
            &std::env::current_exe()?,
            paths::location_overrides(config)?,
            &SystemRunner,
        )
    };

    match command {
        ControllerCommand::Install { dry_run } => {
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
        ControllerCommand::Status => {
            let ctx = controller::context(&socket)?;
            println!("{}", ctx.describe());
            println!("socket:    {}", socket.display());
            Ok(login_session_verdict(&ctx))
        }
        ControllerCommand::Uninstall => {
            for line in launchagent::uninstall(&plan()?, &SystemRunner)? {
                println!("{line}");
            }
            Ok(0)
        }
    }
}

/// The Linux half of [`controller_command`]: a systemd user service in
/// place of the LaunchAgent (P74). Same three commands, same exit codes.
fn systemd_controller_command(command: &ControllerCommand, config: Option<&Path>) -> Result<i32> {
    use ccnm_core::systemd;
    let state = paths::state_dir()?;
    let socket = paths::controller_socket(&state);
    let plan = || -> Result<systemd::Plan> {
        systemd::Plan::new(
            &paths::config_home()?,
            &state,
            &std::env::current_exe()?,
            paths::location_overrides(config)?,
        )
    };
    let linger = |ctx: &controller::Context| {
        if ctx.linger == Some(false) {
            eprintln!("\n{}", systemd::linger_warning(&ctx.hello.user));
        }
    };
    match command {
        ControllerCommand::Install { dry_run } => {
            let plan = plan()?;
            println!("{}", plan.describe());
            if *dry_run {
                println!("\n--- {} ---\n{}", plan.unit_path.display(), plan.unit);
                return Ok(0);
            }
            let ctx = systemd::install(&plan, &SystemRunner)?;
            println!("\nlistening: {}", ctx.describe());
            linger(&ctx);
            Ok(login_session_verdict(&ctx))
        }
        ControllerCommand::Status => {
            let ctx = controller::context(&socket)?;
            println!("{}", ctx.describe());
            println!("socket:    {}", socket.display());
            linger(&ctx);
            Ok(login_session_verdict(&ctx))
        }
        ControllerCommand::Uninstall => {
            for line in systemd::uninstall(&plan()?, &SystemRunner)? {
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
         - it was started by hand instead of by launchd: ccnm controller install\n\
         - nobody is logged in on the Agent Node's screen; log in there once",
        match &ctx.manager {
            Ok(name) => name.as_str(),
            Err(_) => "session unknown",
        }
    );
    ccnm_core::ErrorCode::NotReady.exit_code()
}

/// Replies to the other machine go on stdout as one JSON document.
/// Nothing else may ever be printed there: the caller parses the whole of
/// stdout as that document, and a stray line reads as a version mismatch.
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

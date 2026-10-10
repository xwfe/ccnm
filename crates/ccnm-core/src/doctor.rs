//! `ccnm doctor [WORKSPACE]`: is this machine and workspace ready to use?
//!
//! Every check is one row: name, status, a line of detail. Five statuses:
//!
//! ```text
//! OK     verified
//! WARN   verified, with something worth reading; does not block READY
//! NOTE   not checked by design; does not block READY
//! SKIP   should have been checked and was not; blocks
//! FAIL   verified broken, with a CCNM_E_* code and a fix hint; blocks
//! ```
//!
//! NOTE and SKIP both mean "not checked", and the difference is whether
//! the answer could have been different. A NOTE row reads the same whatever
//! state the machines are in: the check does not apply to this
//! configuration, is outside what a read-only probe can prove, or is
//! answered by another row of the same report. A SKIP row is an unknown:
//! the other side did not answer, an older build did not report it, or a
//! step before it failed. Until P73 both were SKIP, so every real
//! configuration ended NOT READY with nothing left to fix.
//!
//! The exit code is the error code of the first FAIL row. With no FAIL but
//! at least one SKIP it is `CCNM_E_NOT_READY` (3): nothing is known to be
//! broken, but the workspace is not proven usable either, and `ccnm run`
//! must be able to tell those two apart. Only OK/WARN/NOTE rows exit 0.
//!
//! The Runtime Node checks what it can see on its own (config, the project
//! root, the ccnm binary the Agent Node will invoke back here, how the
//! work alias resolves), then makes one `ccnm internal probe` call to the
//! Agent Node and renders a row per fact it brings back: its ccnm,
//! the selected Agent and its login, and the reverse ssh's hello from this
//! machine. Checks ccnm cannot prove without a live session or external OS
//! policy are NOTE rows that say where the answer is instead.
//!
//! # Invariant: doctor is read-only
//!
//! Nothing in this module may install a binary, write a file, start an SSH
//! master, or leave a process behind. `ccnm run`, cron and CI call doctor
//! repeatedly; a check that fixes things as a side effect makes two runs
//! disagree and hides whether the environment was broken before doctor
//! ran. Every ssh here uses [`crate::ssh::Master::Reuse`]
//! (`ControlMaster=no`), which reuses an existing master but never creates
//! one.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{Config, Resolved, Topology};
use crate::error::{Error, ErrorCode, ErrorReport};
use crate::lang::Lang;
use crate::mcp::context;
use crate::paths;
use crate::process::{Cmd, ProcessRunner};
use crate::protocol::PROTOCOL;
use crate::protocol::hello::HelloReport;
use crate::protocol::probe::{ProbeReport, ProbeRequest};
use crate::provider::AgentProvider;
use crate::safety;
use crate::ssh::{Master, Ssh};

/// What doctor needs from its surroundings. Injected so tests can script
/// every external command.
pub struct Env<'a> {
    pub runner: &'a dyn ProcessRunner,
    /// Where ControlPath sockets live on this machine. Only read: doctor
    /// reuses a master if one exists and never creates the directory.
    pub control_dir: PathBuf,
    /// This user's home, for expanding the `~/` in a remote ccnm path.
    pub home: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    /// Not checked, by design: the row does not apply to this
    /// configuration, is outside what this read-only probe can prove, or is
    /// answered by another row of the same report. It would read the same
    /// whatever state the machines are in, so it does not block READY; the
    /// detail says why, and where the answer is.
    Note,
    /// Not performed, and the answer is unknown: a prerequisite failed, the
    /// other side did not answer, or its build does not report it. Blocks
    /// READY, but has no error code of its own; the report maps "only
    /// SKIPs" to [`ErrorCode::NotReady`].
    Skip,
    Fail(ErrorCode),
}

impl Status {
    fn label_in(&self, lang: Lang) -> &'static str {
        match self {
            Status::Ok => lang.pick("正常", "OK"),
            Status::Warn => lang.pick("注意", "WARN"),
            // 不 and 没 is the whole distinction: "doesn't check" against
            // "didn't get to check".
            Status::Note => lang.pick("不查", "NOTE"),
            Status::Skip => lang.pick("没查", "SKIP"),
            // "失败" and not "不行": the names next to it are nouns, and
            // `sudo 权限 不行` reads as a sentence about sudo rather than
            // as a verdict on the check.
            Status::Fail(_) => lang.pick("失败", "FAIL"),
        }
    }

    fn blocks(&self) -> bool {
        matches!(self, Status::Skip | Status::Fail(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Warn,
            detail: detail.into(),
        }
    }

    fn note(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Note,
            detail: detail.into(),
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Skip,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, err: &Error) -> Self {
        Check::fail_with(name, err.code(), err.message())
    }

    fn fail_report(name: &'static str, err: &ErrorReport) -> Self {
        Check::fail_with(name, err.code(), &err.message)
    }

    fn fail_with(name: &'static str, code: ErrorCode, detail: impl AsRef<str>) -> Self {
        Check {
            name,
            status: Status::Fail(code),
            detail: format!("{}: {}", code.name(), detail.as_ref()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// What was examined: a workspace name, or "config" when none was given.
    pub subject: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn ready(&self) -> bool {
        self.blocking_code().is_none()
    }

    /// The code the process should exit with.
    ///
    /// ```text
    /// any FAIL               -> the first FAIL's code
    /// no FAIL, any SKIP      -> CCNM_E_NOT_READY
    /// only OK / WARN / NOTE  -> none (exit 0)
    /// ```
    ///
    /// FAIL wins over SKIP regardless of row order: a real failure is more
    /// useful to act on than "could not check".
    pub fn blocking_code(&self) -> Option<ErrorCode> {
        let first_fail = self.checks.iter().find_map(|c| match c.status {
            Status::Fail(code) => Some(code),
            _ => None,
        });
        first_fail.or_else(|| {
            self.checks
                .iter()
                .any(|c| c.status.blocks())
                .then_some(ErrorCode::NotReady)
        })
    }

    pub fn exit_code(&self) -> i32 {
        self.blocking_code().map_or(0, ErrorCode::exit_code)
    }

    /// The doctor table, ending in READY or NOT READY (how to read it:
    /// docs/troubleshooting.md, "doctor 的表怎么读").
    ///
    /// English, which is what every caller inside this crate wants: the
    /// tests assert on it, and core has no business deciding what a person
    /// reads. The CLI picks the language and calls [`Report::render_in`].
    pub fn render(&self) -> String {
        self.render_in(Lang::En)
    }

    /// The same table, in `lang`.
    ///
    /// Only names, status words and the verdict line are translated. The
    /// detail column is not: it carries paths, versions, `ssh` command
    /// lines, config keys and text generated on the *other* machine, and
    /// half-translating a line like that produces something that is
    /// neither copy-pasteable nor readable.
    pub fn render_in(&self, lang: Lang) -> String {
        const NAME_WIDTH: usize = 24;
        const STATUS_WIDTH: usize = 7;

        let mut out = format!("ccnm doctor: {}\n\n", self.subject);
        for check in &self.checks {
            let mut lines = check.detail.lines();
            let first = lines.next().unwrap_or("");
            // Padded by terminal columns, not by `char` count: a CJK name
            // is two columns per character, and `{:<24}` would pad it as
            // if it were half as wide, shifting every later column right.
            // Identical to `{:<24}` for ASCII, so an English table is
            // byte-for-byte what it always was.
            let _ = writeln!(
                out,
                "{}{}{first}",
                crate::lang::pad(row_label(lang, check.name), NAME_WIDTH),
                crate::lang::pad(check.status.label_in(lang), STATUS_WIDTH)
            );
            for line in lines {
                let _ = writeln!(
                    out,
                    "{:width$}{line}",
                    "",
                    width = NAME_WIDTH + STATUS_WIDTH
                );
            }
        }

        let failed = self.count(|s| matches!(s, Status::Fail(_)));
        let skipped = self.count(|s| matches!(s, Status::Skip));
        let noted = self.count(|s| matches!(s, Status::Note));
        out.push('\n');
        if self.ready() && noted == 0 {
            out.push_str(lang.pick("可以用了\n", "READY\n"));
        } else if self.ready() {
            // Counted so that READY never hides that some rows were not
            // looked at; each of them says why.
            let _ = if lang == Lang::Zh {
                writeln!(out, "可以用了（{noted} 项不查，原因写在标“不查”的行里）")
            } else {
                writeln!(
                    out,
                    "READY ({noted} not checked by design; each NOTE row says why)"
                )
            };
        } else if lang == Lang::Zh {
            let _ = writeln!(out, "还不能用（{failed} 项失败，{skipped} 项没查）");
        } else {
            let _ = writeln!(out, "NOT READY ({failed} failed, {skipped} not checked)");
        }
        out
    }

    fn count(&self, pred: impl Fn(&Status) -> bool) -> usize {
        self.checks.iter().filter(|c| pred(&c.status)).count()
    }
}

/// Run every check this build can perform.
pub fn run(config_path: &Path, workspace: Option<&str>, env: &Env<'_>) -> Report {
    run_selected(config_path, workspace, None, env)
}

pub fn run_selected(
    config_path: &Path,
    workspace: Option<&str>,
    agent: Option<&str>,
    env: &Env<'_>,
) -> Report {
    let subject = workspace.unwrap_or("config").to_string();
    let mut checks = Vec::new();

    let config = match Config::load(config_path) {
        Ok(config) => {
            checks.push(Check::ok("Config", config_path.display().to_string()));
            config
        }
        Err(err) => {
            checks.push(Check::fail("Config", &err));
            return Report { subject, checks };
        }
    };

    match workspace {
        None => {
            let names: Vec<&str> = config.workspaces.keys().map(String::as_str).collect();
            let detail = if names.is_empty() {
                "none defined".to_string()
            } else {
                names.join(", ")
            };
            checks.push(Check::ok("Workspaces", detail));
        }
        Some(name) => match config.workspace(name) {
            Ok(resolved) => {
                checks.push(Check::ok("Workspace config", describe_workspace(&resolved)));
                checks.extend(workspace_checks(&resolved, agent, env));
            }
            Err(err) => checks.push(Check::fail("Workspace config", &err)),
        },
    }

    Report { subject, checks }
}

/// Doctor as run **on the Agent Node**, which holds no workspace list.
///
/// It used to send the whole public `ccnm doctor` to the Runtime over ssh
/// and print what came back. That made the Runtime Executor run a public
/// command, and that command then dialled *back* to this machine to probe
/// it -- an execution identity that is supposed to be inbound-only opening
/// an outbound connection, for a diagnostic (P7.4 Batch D2).
///
/// Now the two sides do what each can prove. This machine asks the Runtime
/// what the workspace is (`internal runtime-resolve`), probes itself
/// locally, and asks the Runtime Executor about itself over the same
/// inbound ssh the session uses. The rows are rendered by the same code the
/// Runtime-side table uses, so the same Runtime Executor cannot be
/// described two different ways depending on where somebody typed.
pub fn from_agent(
    config_path: &Path,
    workspace: &str,
    answer: crate::Result<(&crate::runtime::ResolveReport, &ProbeReport)>,
) -> Report {
    let subject = workspace.to_string();
    let mut checks = match Config::load(config_path) {
        Ok(_) => vec![Check::ok("Config", config_path.display().to_string())],
        Err(err) => vec![Check::fail("Config", &err)],
    };
    let (authority, rep) = match answer {
        Ok(pair) => pair,
        Err(err) => {
            // Without the Runtime's answer there is no workspace to check,
            // and this machine must not invent one from a local guess.
            checks.push(Check::fail("Workspace config", &err));
            checks.extend(
                [
                    "Agent ccnm",
                    "Controller",
                    "Reverse SSH",
                    "Runtime safety",
                    "exec_command",
                    "Remote MCP handshake",
                    "Workspace root",
                    "Terminal session",
                ]
                .into_iter()
                .map(|name| Check::skip(name, "not checked: the Runtime did not answer")),
            );
            checks.extend(not_yet_implemented());
            return Report { subject, checks };
        }
    };
    checks.push(Check::ok("Workspace config", describe_authority(authority)));
    let agent_node = authority
        .agent
        .as_ref()
        .map(|reference| reference.node.as_str())
        .unwrap_or("this machine");
    checks.extend(probe_rows(
        &Subject {
            workspace,
            root: &authority.root,
            runtime_node: &authority.runtime_node,
            agent_node,
            provider_config_dir: authority.provider_config_dir.as_deref(),
        },
        rep,
    ));
    checks.extend(not_yet_implemented());
    Report { subject, checks }
}

/// The Runtime's answer about a workspace, in one line, and said to be its
/// answer: this machine has no workspace list to disagree with it.
fn describe_authority(authority: &crate::runtime::ResolveReport) -> String {
    let agent = match &authority.agent {
        Some(reference) => format!("agent={}/{}", reference.node, reference.instance),
        None => "legacy agent_node selection".to_string(),
    };
    format!(
        "answered by {}: {agent}, root={}",
        authority.runtime_node,
        authority.root.display()
    )
}

/// The one line that says which machines a workspace spans, and how this
/// one dials them. Colocated workspaces dial nothing, so saying "ssh"
/// there would be a lie about the topology.
fn describe_workspace(r: &Resolved<'_>) -> String {
    let ws = r.workspace;
    if r.is_colocated() {
        return format!(
            "backend={} agent and project both on {} (ssh {}), native tools",
            ws.backend.as_str(),
            r.agent_node(),
            r.agent_ssh().unwrap_or("-"),
        );
    }
    if let Some(agent) = &ws.agent {
        return format!(
            "backend={} agent={}/{} (ssh {}), runtime_node={}",
            ws.backend.as_str(),
            agent.node,
            agent.instance,
            r.agent_ssh().unwrap_or("-"),
            ws.runtime_node,
        );
    }
    if r.agent.is_none() {
        // An empty `agent_node=` used to be printed here, which reads as a
        // missing setting rather than as the shape this workspace is.
        return format!(
            "backend={} no agent (external MCP clients bring their own), runtime_node={}",
            ws.backend.as_str(),
            ws.runtime_node,
        );
    }
    format!(
        "backend={} agent_node={} (ssh {}), runtime_node={}",
        ws.backend.as_str(),
        r.agent_node(),
        r.agent_ssh().unwrap_or("-"),
        ws.runtime_node,
    )
}

fn workspace_checks(r: &Resolved<'_>, agent: Option<&str>, env: &Env<'_>) -> Vec<Check> {
    let ws = r.workspace;
    // A workspace with no Agent is not a broken workspace: it exists for
    // external MCP clients, which bring their own. Everything below this
    // point is about an Agent session, so reporting those rows as failures
    // would be describing a correct configuration as an error -- which is
    // what this did before P12, all the way down to CCNM_E_INTERNAL.
    if r.agent.is_none() {
        return external_only_checks(r, env);
    }
    let selected = match r.agent_reference(agent) {
        Ok(selected) => selected,
        Err(error) => return vec![Check::fail("Agent selection", &error)],
    };
    if r.is_colocated() {
        return vec![Check::fail_with(
            "Agent topology",
            ErrorCode::NotReady,
            "colocated Agent execution is not enabled until its installed CLI path is verified",
        )];
    }

    // The local half of the report is only about this machine, so it is
    // only run when this machine is the one holding the project. In the
    // other two topologies the project, its CLAUDE.md, the ccnm that
    // serves it and the account it runs as are all on the far side, and
    // the probe reports them from there. Auditing this machine's account
    // instead would answer a question nobody asked, and answer it FAIL.
    let mut checks = Vec::new();
    let mut instance_project_row = None;
    if r.topology() == Topology::FromRuntime {
        checks.push(runtime_workspace(
            &ws.root,
            "its answer is the `Workspace root` row below",
            true,
        ));
        if ws.agent.is_some() {
            instance_project_row = Some(checks.len());
            checks.push(Check::skip(
                "Project instructions",
                "waiting for the selected provider's Runtime MCP handshake",
            ));
        } else {
            checks.push(project_instructions(r));
        }
        checks.push(runtime_ccnm(r, env));
        // The safety rows used to be added here, from an audit of this
        // process. They are not this machine's to answer: the account that
        // runs the tools is the one the Agent's transport lands on, so they
        // come back with the probe instead (`probe_rows`). Doing it here
        // would answer a question nobody asked -- "is the person typing
        // confined?" -- and answer it FAIL for every normal operator.
    } else {
        let why = format!("the project is on {}, not on this machine", ws.runtime_node);
        for name in ["Runtime workspace", "Project instructions", "Runtime ccnm"] {
            checks.push(Check::skip(name, &why));
        }
    }

    let ssh = match r.agent_ssh().and_then(|alias| {
        let ssh = Ssh::new(alias, &env.control_dir)?;
        ssh.check_control_path()?;
        Ok(ssh.with_ccnm_bin(r.require_agent()?.ccnm_bin()))
    }) {
        Ok(ssh) => ssh,
        Err(e) => {
            checks.push(Check::fail("Agent SSH", &e));
            checks.extend(skipped_after_agent_ssh());
            checks.extend(not_yet_implemented());
            return checks;
        }
    };
    let resolved = match ssh.resolve(env.runner) {
        Ok(resolved) => resolved,
        Err(e) => {
            checks.push(Check::fail_with(
                "Agent SSH",
                ErrorCode::AgentUnreachable,
                e.message(),
            ));
            checks.extend(skipped_after_agent_ssh());
            checks.extend(not_yet_implemented());
            return checks;
        }
    };

    let req = ProbeRequest {
        agent: selected.clone(),

        provider: Default::default(),
        protocol: if selected.is_some() { 3 } else { PROTOCOL },
        workspace: r.name.to_string(),
        root: ws.root.clone(),
        runtime_node: r.workspace.runtime_node.clone(),
        provider_config_dir: selected
            .is_none()
            .then(|| {
                r.agent
                    .and_then(|node| AgentProvider::current().config_dir(node))
                    .map(Path::to_path_buf)
            })
            .flatten(),
        // One real MCP session, shut down before the probe returns.
        mcp_calls: 1,
    };
    let timeout = Duration::from_secs(90);
    match ssh.call_ccnm::<_, ProbeReport>(
        env.runner,
        Master::Reuse,
        &["internal", "probe"],
        &req,
        timeout,
        ErrorCode::AgentUnreachable,
    ) {
        Ok(rep) => {
            let identity_matches = match (selected.as_ref(), rep.agent_identity.as_ref()) {
                (None, None) => true,
                (Some(reference), Some(identity)) => identity.reference() == *reference,
                _ => false,
            };
            if rep.rejected.is_some() {
                // The Agent Node refused the selection and said why. The
                // report carries no identity in that case, so comparing
                // identities would only restate the refusal as a mismatch
                // and drop the reason (F10). `probe_rows` reports it.
                checks.push(Check::ok("Agent SSH", resolved.target()));
                checks.extend(probe_rows(&Subject::of(r), &rep));
                checks.extend(not_yet_implemented());
                return checks;
            }
            // Versions before identities (F20). An Agent on another ccnm may
            // not read this request the way it was meant -- a 0.9.0 one
            // answered a 0.10.1 probe with no identity at all -- so the
            // identity comparison below would report the symptom and return
            // before the version row could name the cause.
            let version = version_row("Agent ccnm", &rep.hello, &agent_side(r.agent_node()));
            if !identity_matches && version.status != Status::Ok {
                checks.push(Check::ok("Agent SSH", resolved.target()));
                checks.push(version);
                checks.push(Check::skip(
                    "Agent selection",
                    "not compared: the Agent Node runs another ccnm build, which may not have read the selection",
                ));
                return checks;
            }
            if !identity_matches {
                checks.push(Check::fail_with(
                    "Agent selection",
                    ErrorCode::Version,
                    "Agent probe identity differs from the Runtime selection",
                ));
                return checks;
            }
            if let Some(identity) = &rep.agent_identity {
                checks.push(Check::ok(
                    "Agent selection",
                    format!(
                        "{}/{} ({}; profile {})",
                        identity.node,
                        identity.instance,
                        identity.provider.display_name(),
                        identity.profile_ref
                    ),
                ));
            }
            if let Some(index) = instance_project_row {
                checks[index] = selected_project_instructions(&rep);
            }
            checks.push(Check::ok("Agent SSH", resolved.target()));
            checks.extend(probe_rows(&Subject::of(r), &rep));
        }
        Err(e) => {
            checks.push(Check::fail("Agent SSH", &e));
            checks.extend(skipped_after_agent_ssh());
        }
    }

    checks.extend(not_yet_implemented());
    checks
}

fn selected_project_instructions(rep: &ProbeReport) -> Check {
    match &rep.mcp {
        Some(Ok(report)) => match &report.project_instructions {
            Some(detail) => Check::ok("Project instructions", detail),
            None => Check::warn(
                "Project instructions",
                "selected provider's MCP handshake reported no context marker",
            ),
        },
        Some(Err(error)) => Check::fail_report("Project instructions", error),
        None => Check::skip(
            "Project instructions",
            "selected provider's MCP handshake was not completed",
        ),
    }
}

/// The few workspace facts the probe rows render.
///
/// Built from this machine's config on the Runtime Node, and from the
/// Runtime's own answer on the Agent Node, which holds no workspace list.
/// One struct rather than two rendering paths: the two sides must agree
/// about the same Runtime Executor, and the cheapest way to guarantee that
/// is to give them the same code and different inputs.
pub(crate) struct Subject<'a> {
    pub workspace: &'a str,
    pub root: &'a Path,
    pub runtime_node: &'a str,
    pub agent_node: &'a str,
    /// The Agent-side provider config directory, for the login hint.
    pub provider_config_dir: Option<&'a Path>,
}

impl<'a> Subject<'a> {
    fn of(r: &'a Resolved<'a>) -> Subject<'a> {
        Subject {
            workspace: r.name,
            root: &r.workspace.root,
            runtime_node: &r.workspace.runtime_node,
            agent_node: r.agent_node(),
            provider_config_dir: r
                .agent
                .and_then(|node| AgentProvider::current().config_dir(node)),
        }
    }
}

/// How the version rows name the Agent Node: by the name the config gives
/// it. They used to say `work` whatever it was called (F26).
fn agent_side(node: &str) -> String {
    format!("the Agent Node {node}")
}

fn probe_rows(r: &Subject<'_>, rep: &ProbeReport) -> Vec<Check> {
    let agent_version = version_row("Agent ccnm", &rep.hello, &agent_side(r.agent_node));
    // Everything below about the Runtime is the Agent's account of it. An
    // Agent on another build parses that into its own structs and drops
    // what it does not know -- a 0.9.0 one drops `wire` -- so only the
    // version string survives the trip intact (F25).
    let agent_other_build = agent_version.status != Status::Ok;
    let mut checks = vec![agent_version];

    // Refused before anything was probed: one row with the Agent's reason,
    // and every row it would have filled says it was not checked. Not the
    // rows below, which would read the empty fields as facts -- an absent
    // reverse link as "same machine", above all (F10).
    if let Some(why) = &rep.rejected {
        checks.push(Check::fail_with(
            "Agent selection",
            why.code(),
            format!(
                "the Agent Node refused this Agent before probing anything: {}",
                why.message
            ),
        ));
        const REASON: &str = "not checked: the Agent Node refused the selected Agent";
        checks.push(Check::skip("Controller", REASON));
        checks.push(Check::skip(rep.provider.display_name(), REASON));
        checks.push(Check::skip(rep.provider.authentication_check(), REASON));
        checks.extend(
            [
                "Reverse SSH",
                "Runtime safety",
                "exec_command",
                "Remote MCP handshake",
                "Workspace root",
            ]
            .into_iter()
            .map(|name| Check::skip(name, REASON)),
        );
        // This one the refused probe did measure.
        checks.push(terminal_row(r, rep));
        return checks;
    }

    checks.push(controller_row(rep));

    checks.push(match &rep.agent.version {
        Ok(v) => {
            let path = rep
                .agent
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Check::ok(rep.provider.display_name(), format!("{v} ({path})"))
        }
        Err(e) => Check::fail_report(rep.provider.display_name(), e),
    });

    checks.push(auth_row(r, rep));

    // A colocated workspace dials nothing back, so the two rows about the
    // reverse link are not failures to report -- there is no reverse link
    // by design. The project is on the Agent Node, so its own hello is
    // what says whether the root is there.
    match &rep.runtime_ssh {
        None => {
            let why = format!(
                "agent and project are both on {}, so nothing dials back",
                r.agent_node
            );
            checks.push(Check::skip("Reverse SSH", &why));
            checks.push(Check::skip("Remote MCP handshake", &why));
            checks.push(root_row(r, &rep.hello, r.agent_node));
            checks.push(terminal_row(r, rep));
        }
        Some(_) => match &rep.runtime_hello {
            Some(Ok(h)) => {
                let reached = format!("{} as {}, ccnm {}", r.runtime_node, h.user, h.ccnm_version);
                checks.push(match version_row("Reverse SSH", h, "the Runtime Node") {
                    ok if ok.status == Status::Ok => Check::ok("Reverse SSH", reached),
                    _ if agent_other_build && h.ccnm_version == crate::VERSION => Check::skip(
                        "Reverse SSH",
                        format!(
                            "{reached}; build not compared: the Agent Node that relayed this runs another ccnm build and may have dropped what it does not know -- install the same build there first"
                        ),
                    ),
                    fail => fail,
                });
                // The Runtime Executor's own answer about itself and the
                // project, from the account the transport lands on.
                checks.extend(executor_rows(r, rep));
                checks.push(mcp_row(rep));
                checks.push(terminal_row(r, rep));
            }
            Some(Err(e)) => {
                checks.push(Check::fail_report("Reverse SSH", e));
                checks.extend(skipped_after_reverse_ssh());
            }
            None => {
                checks.push(Check::warn(
                    "Reverse SSH",
                    "that ccnm build reported an alias but no hello",
                ));
                checks.extend(skipped_after_reverse_ssh());
            }
        },
    }

    checks
}

/// Everything the Runtime Executor reported about itself: the safety
/// findings, the exec verdict, and whether the project is usable by it.
///
/// The rows are identical whoever ran doctor, because none of them is
/// computed here. When the far side could not be asked they are SKIPs that
/// say so -- an unknown Runtime must never read as a confined one.
fn executor_rows(r: &Subject<'_>, rep: &ProbeReport) -> Vec<Check> {
    match &rep.runtime_audit {
        Some(Ok(report)) => {
            let mut rows = runtime_safety_rows(report, rep.provider);
            rows.push(executor_root_row(r, &report.root, &report.audit.user));
            rows
        }
        Some(Err(e)) => {
            let mut rows = vec![Check::fail_report("Runtime safety", e)];
            rows.push(Check::skip(
                "Workspace root",
                "not checked: the Runtime Executor did not answer",
            ));
            rows.push(Check::skip(
                "exec_command",
                "not checked: the Runtime Executor did not answer",
            ));
            rows
        }
        None => ["Runtime safety", "Workspace root", "exec_command"]
            .into_iter()
            .map(|name| {
                Check::skip(
                    name,
                    "not checked: that ccnm build does not report the Runtime Executor's audit",
                )
            })
            .collect(),
    }
}

/// Whether the project is usable *by the identity that would run its
/// tools*, which is not the same question as whether a directory is there.
///
/// P7.3 met the difference on real hardware: a work tree owned by another
/// account, reachable through a world-writable parent, passed the old row
/// -- and every git command the Agent ran failed with `detected dubious
/// ownership` while the table stayed green.
fn executor_root_row(r: &Subject<'_>, root: &crate::runtime::RootStatus, user: &str) -> Check {
    use crate::runtime::GitStatus;
    const NAME: &str = "Workspace root";
    let path = r.root.display();
    if !root.present {
        return Check::fail_with(
            NAME,
            ErrorCode::WrongWorkspace,
            format!("{path} is missing for {user} on {}", r.runtime_node),
        );
    }
    if !root.is_dir {
        return Check::fail_with(
            NAME,
            ErrorCode::WrongWorkspace,
            format!("{path} exists but is not a directory for {user}"),
        );
    }
    match root.git {
        GitStatus::RefusedOwnership => Check::fail_with(
            NAME,
            ErrorCode::WrongWorkspace,
            format!(
                "{path} is a directory for {user}, but git refuses it: the repository belongs to another account\nfix: make the project directory owned by {user}, the identity that runs its tools"
            ),
        ),
        GitStatus::Unknown => Check::warn(
            NAME,
            format!("{path} is a directory for {user}; git could not be asked about it"),
        ),
        _ if !root.owned => Check::warn(
            NAME,
            format!(
                "{path} is a directory for {user} but is owned by another account; tools that check ownership will refuse it"
            ),
        ),
        GitStatus::Usable => Check::ok(
            NAME,
            format!("{path} is a git repository {user} can work in"),
        ),
        GitStatus::NotARepo => Check::ok(
            NAME,
            format!("{path} is a directory owned by {user} (not a git repository)"),
        ),
    }
}

/// Whether the project is where the workspace says it is, as reported by
/// whichever node is supposed to be holding it.
fn root_row(r: &Subject<'_>, h: &crate::protocol::hello::HelloReport, node: &str) -> Check {
    match h.root {
        Some(status) if status.is_ok() => Check::ok(
            "Workspace root",
            format!("{} is a directory for {}", r.root.display(), h.user),
        ),
        Some(status) => Check::fail_with(
            "Workspace root",
            ErrorCode::WrongWorkspace,
            format!(
                "{} is {} for {} on {}",
                r.root.display(),
                status.describe(),
                h.user,
                node
            ),
        ),
        None => Check::warn(
            "Workspace root",
            format!("{node}'s hello did not report the root"),
        ),
    }
}

fn skipped_after_reverse_ssh() -> Vec<Check> {
    [
        "Runtime safety",
        "exec_command",
        "Remote MCP handshake",
        "Workspace root",
        "Terminal session",
    ]
    .into_iter()
    .map(|name| Check::skip(name, "not checked: reverse SSH failed"))
    .collect()
}

/// tmux on the Agent Node, and whether this workspace has a session in
/// it right now.
///
/// No tmux is a WARN, not a FAIL: `--print` sessions do not need it, and
/// half the product works without it. A live session is reported with what
/// was measured about it, so "detached" reads as the normal state it is
/// rather than as something wrong.
fn terminal_row(r: &Subject<'_>, rep: &ProbeReport) -> Check {
    const NAME: &str = "Terminal session";
    let Some(status) = &rep.terminal else {
        return Check::skip(NAME, "not reported by that ccnm build");
    };
    let version = match &status.tmux {
        Ok(v) => v,
        Err(e) => return Check::warn(NAME, &e.message),
    };
    let wanted = crate::tmux::session_name(r.workspace);
    match status.sessions.iter().find(|s| s.tmux_session == wanted) {
        // The instance that was asked about has none, and the workspace's
        // one terminal belongs to another: not the same as "nothing is
        // running for this project" (F6).
        None if !status.other_instances.is_empty() => Check::ok(
            NAME,
            format!(
                "tmux {version}, no live session of this instance; {}'s terminal is a session of instance {}",
                r.workspace,
                status.other_instances.join(", ")
            ),
        ),
        None => Check::ok(
            NAME,
            format!("tmux {version}, no live session for {}", r.workspace),
        ),
        // A live session whose transport died is a WARN, not an OK: it
        // looks like it is working from every side except the one that
        // matters, and the fix is a thing a person has to type.
        Some(live) if live.tools == Some(false) => Check::warn(
            NAME,
            format!(
                "tmux {version}, {}\nthe session has lost its tools; in Claude: /mcp -> ccnm -> Reconnect",
                live.describe()
            ),
        ),
        Some(live) => Check::ok(NAME, format!("tmux {version}, {}", live.describe())),
    }
}

/// One MCP session over the reverse ssh: initialize, tools/list, and a
/// `workspace_info` that must come back from a single server process.
fn mcp_row(rep: &ProbeReport) -> Check {
    const NAME: &str = "Remote MCP handshake";
    match &rep.mcp {
        None => Check::skip(NAME, "not requested"),
        Some(Ok(m)) if !m.single_process => Check::fail_with(
            NAME,
            ErrorCode::Internal,
            format!(
                "the server's pid or call counter changed during {} call(s); the transport is not one persistent process",
                m.calls
            ),
        ),
        Some(Ok(m)) => Check::ok(NAME, m.summary()),
        Some(Err(e)) => Check::fail_report(NAME, e),
    }
}

/// What the Runtime Executor can reach, as reported by the Runtime
/// Executor.
///
/// These rows used to describe whoever ran `ccnm doctor`, because the audit
/// judges the process that calls it. P7.3 measured what that costs: the
/// same workspace, same build, same minute — 0 failed as `ccrun`, 7 failed
/// as the operator's own login, because the rows judged the typist. So the
/// audit is now asked over the Agent's ssh into the Runtime Executor and
/// arrives in the probe (P7.4 Batch D). Whoever types the command no longer
/// changes the answer.
///
/// One row per finding, because "the runtime is not confined" is not
/// something anyone can act on and "this account is in the admin group,
/// remove it" is.
///
/// A failure is a FAIL row, not a SKIP: nothing is unknown here. The
/// property was checked and it does not hold.
fn runtime_safety_rows(
    report: &crate::runtime::AuditReport,
    provider: AgentProvider,
) -> Vec<Check> {
    let audit = &report.audit;
    // A workspace that has accepted an unconfined runtime gets warnings,
    // not failures. The runtime will run its commands either way, and a
    // table that says NOT READY about a session that works is a table
    // people learn to ignore. The value is the Runtime's own -- it is the
    // one `exec_command`'s gate reads, not this machine's copy of it.
    let accepted = report.accepted();
    let mut rows: Vec<Check> = audit
        .findings
        .iter()
        .map(|finding| {
            let detail = match &finding.fix {
                Some(fix) => format!("{}\nfix: {fix}", finding.detail),
                None => finding.detail.clone(),
            };
            let name = safety_row_name(&finding.check);
            match finding.severity {
                safety::Severity::Ok => Check::ok(name, detail),
                safety::Severity::Warn => Check::warn(name, detail),
                // Accepted, so not a FAIL -- but never an OK either. The
                // property does not hold; somebody said they can live
                // with that, and the audit has to keep showing which.
                safety::Severity::Fail
                    if finding.is_agent_credential() && accepted.unisolated_credentials =>
                {
                    Check::warn(
                        name,
                        format!(
                            "{detail}\naccepted: this workspace sets allow_unisolated_credentials"
                        ),
                    )
                }
                safety::Severity::Fail
                    if accepted.unconfined_exec
                        && !finding.non_waivable()
                        && !finding.is_agent_credential() =>
                {
                    Check::warn(name, detail)
                }
                safety::Severity::Fail => Check::fail_with(name, ErrorCode::Policy, detail),
            }
        })
        .collect();
    // The verdict the runtime's own gate uses, so this table and the
    // session cannot disagree about whether commands will run.
    rows.push(if audit.confined() && audit.shared_account {
        // P78: allowed, and not because the account is isolated -- the
        // `Runtime user` row above says so, and this one must not read as
        // the opposite.
        Check::ok(
            "exec_command",
            format!(
                "allowed: commands run as {}, a shared account (no runtime_user), with everything it can reach",
                audit.user
            ),
        )
    } else if audit.confined() {
        Check::ok("exec_command", "the runtime account is confined")
    } else if audit.exec_allowed(accepted) {
        let mut why = String::from(
            "allowed, but the runtime is NOT confined: this workspace sets allow_unconfined_exec",
        );
        if accepted.unisolated_credentials {
            why.push_str(
                "\nand allow_unisolated_credentials: this account can read a known Agent login, and so can every command run here",
            );
        }
        Check::warn("exec_command", why)
    } else {
        Check::fail_with(
            "exec_command",
            ErrorCode::Policy,
            "refused until the runtime account is confined; see docs/production-safety.md",
        )
    });
    // Whether a session stops and asks. Shown both ways because "will this
    // interrupt me?" is a question with two useful answers, and because a
    // warning nobody ever sees the other half of reads like noise.
    //
    // Codex asks too since P71 (F21), but not the way Claude does: the Agent
    // launches it with `approval_mode = "prompt"` for the tools this Runtime
    // marks, and the person at the terminal can hand that back to Codex in
    // `/permissions` -- Full Access stops asking, Approve for me lets Codex's
    // own review decide (measured on 0.154.0, docs/research/probes/
    // p71-codex-approval.py). Claude's key holds in every permission mode.
    // That switch lasts one session since P72 (F27): Codex stores Approve for
    // me in the profile, and the launch now pins who answers over it.
    rows.push(if accepted.unattended_exec {
        Check::warn(
            "Command approval",
            "this workspace sets allow_unattended_exec: interactive sessions run every command without asking\nthe runtime account's own permissions and the workspace root are what still bound them",
        )
    } else if provider == AgentProvider::Codex {
        Check::ok(
            "Command approval",
            "interactive sessions ask before each exec_command; the person at the terminal can stop that for one session with Full Access or Approve for me in /permissions, and the next session asks again\n--print and ccnm mcp bridge never ask: nobody is waiting at either",
        )
    } else {
        Check::ok(
            "Command approval",
            "interactive sessions ask before each exec_command, in every permission mode\n--print and ccnm mcp bridge never ask: nobody is waiting at either",
        )
    });
    if !audit.shared_account {
        rows.push(opener_config_row(report));
    }
    rows
}

/// What in the project takes effect as whoever opens it, for a dedicated
/// account (P85).
///
/// The project belongs to that account -- the `Workspace root` row insists
/// on it -- so chmod and ACLs cannot keep it out of these files: an owner
/// changes both. What helps is knowing which are there before opening the
/// tree as yourself, so the row lists them: WARN while any is, OK when none
/// is. It never refuses; nothing here is wrong with the Runtime. A shared
/// account gets no row: its commands already run as you.
fn opener_config_row(report: &crate::runtime::AuditReport) -> Check {
    const NAME: &str = "Files that act as you";
    let user = &report.audit.user;
    let Some(present) = &report.opener_config else {
        return Check::note(
            NAME,
            "not checked: that Runtime's ccnm does not report them",
        );
    };
    if present.is_empty() {
        return Check::ok(
            NAME,
            format!(
                "none present (IDE tasks, Claude Code hooks and MCP servers, shell startup files); {user} can create them, and this row will list them"
            ),
        );
    }
    Check::warn(
        NAME,
        format!(
            "{user} can change these, and they take effect as whoever opens the project: IDE tasks, Claude Code hooks and MCP servers, shell startup files\npresent now: {}\nbefore opening this tree as yourself in an IDE, Claude Code or Codex, read them first; chmod and ACLs do not stop {user}, whose project it is\nanother account's git refuses this tree (dubious ownership), so its hooks do not run as that account; keep it so: no safe.directory for it\nif {user} is your own account, none of this crosses an account boundary",
            present.join(", ")
        ),
    )
}

/// A check's name as a person reads it, for display only.
///
/// [`Check::name`] itself stays English wherever it is stored: 45 tests
/// look rows up by it, and [`safety_row_name`] decides which finding is
/// which by comparing the same strings — against a report that came off
/// the wire from the other machine. Translating here, at the one place
/// the table is drawn, keeps both of those working.
///
/// Names with no entry render as themselves, so adding a check and
/// forgetting this table costs one English row in a Chinese table rather
/// than a panic. Three kinds are left alone on purpose: product names
/// (`Claude Code`, `Codex CLI`), the MCP tool name `exec_command`, and
/// `ccnm`'s own words — translating those would send someone searching
/// the docs for a term that appears nowhere.
fn row_label(lang: Lang, name: &str) -> &str {
    if lang == Lang::En {
        return name;
    }
    match name {
        "Config" => "配置文件",
        "Workspaces" => "workspace 列表",
        "Workspace config" => "workspace 配置",
        "Workspace root" => "workspace 根目录",
        "Runtime workspace" => "Runtime 上的项目",
        "Project instructions" => "项目指令",
        "Runtime ccnm" => "Runtime 的 ccnm",
        "Agent ccnm" => "Agent 的 ccnm",
        "Agent SSH" => "连 Agent 的 SSH",
        "Agent selection" => "选哪个 Agent",
        "Reverse SSH" => "反向 SSH",
        "Controller" => "Controller",
        "Runtime user" => "Runtime 执行身份",
        // These six are the names of checks that pass when the answer is
        // "no". In English the negative reads as a heading; in Chinese it
        // reads as a statement of fact, and on a WARN row it would then
        // contradict its own detail -- `不在 admin 组 | 注意 | this
        // account is in admin` tells a reader the opposite of the truth
        // about their own machine. Neutral nouns instead, so the name says
        // what was looked at and the status says how it came out.
        "Runs as root" => "root 身份",
        "No sudo" => "sudo 权限",
        "Not an admin" => "admin 组",
        "No SSH keys" => "SSH 私钥",
        "No Claude credential" => "Claude 凭据",
        "No Docker socket" => "Docker socket",
        "Anthropic egress" => "Anthropic 出口",
        "Runtime safety" => "Runtime 安全",
        "Command approval" => "命令审批",
        "Files that act as you" => "以你身份生效的文件",
        "Remote MCP handshake" => "远端 MCP 握手",
        "Terminal session" => "终端会话",
        "Native tool policy" => "本机工具策略",
        "Network isolation" => "网络隔离",
        other => other,
    }
}

/// `Check::name` is `&'static str` because every other row's name is a
/// literal. The audit's names are literals too, so they are mapped back
/// rather than leaked.
fn safety_row_name(check: &str) -> &'static str {
    match check {
        "Runs as root" => "Runs as root",
        "Runtime user" => "Runtime user",
        "No sudo" => "No sudo",
        "Not an admin" => "Not an admin",
        "No SSH keys" => "No SSH keys",
        "No Claude credential" => "No Claude credential",
        "No Docker socket" => "No Docker socket",
        "Anthropic egress" => "Anthropic egress",
        _ => "Runtime safety",
    }
}

/// OK when the other side runs this build, else CCNM_E_VERSION. Both
/// machines must run the same binary.
///
/// The same number is not the same build (F2): everything built from main
/// between two releases carries the last release's number. So a matching
/// number is followed by what each side says it can speak.
fn version_row(name: &'static str, hello: &HelloReport, side: &str) -> Check {
    if hello.ccnm_version != crate::VERSION {
        return Check::fail_with(
            name,
            ErrorCode::Version,
            format!(
                "{side} runs ccnm {}, this machine runs {}; install the same build on both",
                hello.ccnm_version,
                crate::VERSION
            ),
        );
    }
    if let Some(theirs) = hello.other_wire() {
        return Check::fail_with(
            name,
            ErrorCode::Version,
            format!(
                "{side} reports ccnm {} like this machine, but it is not the same build: {theirs}; this machine speaks up to {}\ninstall the same build on both",
                hello.ccnm_version,
                crate::protocol::payload::WIRE_LEVEL
            ),
        );
    }
    let exe = hello
        .exe
        .as_ref()
        .map(|p| format!(" at {}", p.display()))
        .unwrap_or_default();
    Check::ok(name, format!("{}{exe}", hello.ccnm_version))
}

/// The controller: is there one, and is it somewhere useful?
///
/// Three outcomes, and the middle one is why this row exists at all. A
/// controller running outside the login session answers every request and
/// is still useless, which no other row would have caught.
fn controller_row(rep: &ProbeReport) -> Check {
    const NAME: &str = "Controller";
    match &rep.controller {
        None => Check::skip(NAME, "that ccnm build does not have one"),
        Some(Err(e)) if e.code() == ErrorCode::NotReady => Check::skip(NAME, &e.message),
        Some(Err(e)) => Check::fail_report(NAME, e),
        Some(Ok(ctx)) if !ctx.login_session() => Check::fail_with(
            NAME,
            ErrorCode::NotReady,
            format!(
                "{}\nit answers, but not from a login session, so Claude started there could not read its own credentials\nrun on the Agent Node: ccnm controller install",
                ctx.describe()
            ),
        ),
        // Linux (P74): it works, but only while the account is logged in
        // somewhere -- the first logout takes the sessions with it.
        Some(Ok(ctx)) if ctx.linger == Some(false) => Check::warn(
            NAME,
            format!(
                "{}\n{}",
                ctx.describe(),
                crate::systemd::linger_warning(&ctx.hello.user)
            ),
        ),
        Some(Ok(ctx)) => Check::ok(NAME, ctx.describe()),
    }
}

/// Claude's login, and — just as important — whether the answer is worth
/// anything.
///
/// "Not logged in" only means that when it came from a login session.
/// From anywhere else it is the same false negative the controller exists
/// to remove, so it reads as SKIP pointing at the controller's own row.
///
/// A *positive* answer is trusted from anywhere: a session that could not
/// reach the credentials could not have found a login to report. The
/// error runs one way only.
fn auth_row(r: &Subject<'_>, rep: &ProbeReport) -> Check {
    let name = rep.provider.authentication_check();
    let from_login_session = matches!(&rep.controller, Some(Ok(ctx)) if ctx.login_session());
    match &rep.agent.auth {
        // OK is about what was checked, so the row says what that was when
        // it is less than "this login works" (F5).
        Ok(a) if a.logged_in => Check::ok(
            name,
            match rep.provider.login_caveat() {
                Some(caveat) => format!("{}\n{caveat}", a.describe()),
                None => a.describe(),
            },
        ),
        Ok(_) if !from_login_session => Check::skip(
            name,
            "a controller answered, but not from a login session, so \"not logged in\" here means nothing\nfix the Controller row first",
        ),
        Ok(_) => Check::fail_with(
            name,
            ErrorCode::Auth,
            rep.provider.auth_hint(r.provider_config_dir),
        ),
        // "Nobody asked the right process" is not a diagnosis about
        // Claude. SKIP still blocks READY, so nothing runs on the strength
        // of an unchecked login.
        Err(e) if e.code() == ErrorCode::NotReady => Check::skip(name, &e.message),
        Err(e) => Check::fail_report(name, e),
    }
}

/// Rows that depend on the probe, when the probe never happened.
/// The rows for a workspace that only external MCP clients open.
///
/// It reports what this machine can prove — the policy, the project
/// directory, the ccnm that will serve it — and marks the Agent half NOTE
/// by name. Two of those rows are worth reading rather than glossing:
///
/// * the Runtime **safety** verdict has to come from the account the tools
///   run as, and on the managed path it arrives with the Agent's probe.
///   There is no Agent here, so this machine cannot answer it and must not
///   answer it with its own audit: "is the operator confined?" is not the
///   question. It is not missing, though: `mcp-serve` takes it as that
///   account when a client connects and refuses a failing one. Before P83
///   these rows were SKIPs, which kept every such workspace NOT READY on
///   every machine for ever; no machine state could have changed that,
///   which is what NOTE means (P73).
/// * the write guard is not checked either. Asking would mean taking it,
///   and taking it is what a real session does.
fn external_only_checks(r: &Resolved<'_>, env: &Env<'_>) -> Vec<Check> {
    let ws = r.workspace;
    let mut checks = vec![Check::ok(
        "External MCP",
        format!(
            "external_mcp = \"{}\", instructions = \"{}\"; no Agent, so this workspace is opened by `ccnm mcp bridge {}` and never by ccnm itself",
            ws.external_mcp.as_str(),
            ws.external_instructions.as_str(),
            r.name,
        ),
    )];
    if r.topology() == Topology::FromRuntime {
        checks.push(runtime_workspace(
            &ws.root,
            "it answers when an external client opens the workspace through `ccnm mcp bridge`",
            false,
        ));
        checks.push(runtime_ccnm(r, env));
    } else {
        let why = format!("the project is on {}, not on this machine", ws.runtime_node);
        checks.push(Check::skip("Runtime workspace", &why));
        checks.push(Check::skip("Runtime ccnm", &why));
    }
    const NO_AGENT: &str =
        "not checked: this workspace has no Agent, so there is no Agent session to diagnose";
    const NO_TRANSPORT: &str = "not checked here: the verdict belongs to the account the tools run as, and this workspace has no Agent probe to bring it; it is taken when a client connects through `ccnm mcp bridge`, and an account that fails it is refused with CCNM_E_POLICY and the reason";
    checks.extend(
        [
            "Agent SSH",
            "Agent ccnm",
            "Controller",
            "Claude Code",
            "Claude authentication",
            "Reverse SSH",
            "Remote MCP handshake",
            "Terminal session",
            "Project instructions",
        ]
        .into_iter()
        .map(|name| Check::note(name, NO_AGENT)),
    );
    checks.extend(
        ["Runtime safety", "exec_command"]
            .into_iter()
            .map(|name| Check::note(name, NO_TRANSPORT)),
    );
    checks.extend(not_yet_implemented());
    checks
}

fn skipped_after_agent_ssh() -> Vec<Check> {
    const REASON: &str = "not checked: Agent SSH failed";
    [
        "Agent ccnm",
        "Controller",
        "Claude Code",
        "Claude authentication",
        "Reverse SSH",
        // The Runtime Executor is only reachable through the Agent, so a
        // broken control path leaves its verdict unknown -- which must read
        // as unknown, not as confined.
        "Runtime safety",
        "exec_command",
        "Remote MCP handshake",
        "Workspace root",
        "Terminal session",
    ]
    .into_iter()
    .map(|name| Check::skip(name, REASON))
    .collect()
}

/// What the project's own `CLAUDE.md` contributes to a session (the row
/// explained in docs/troubleshooting.md, "`Project instructions ... WARN`").
///
/// Checked here, on the runtime host, because this is the machine that has
/// the file and the machine the MCP server reads it from: the row is about
/// the same bytes the model will be given. Reading it is read-only, so it
/// belongs in doctor.
///
/// No CLAUDE.md is OK — most projects have none, and the model still gets
/// ccnm's own instructions. A file too big for the handshake is a WARN,
/// not a FAIL: the session works, the model just does not see all of it,
/// and that is exactly the kind of thing nobody discovers on their own.
fn project_instructions(r: &Resolved<'_>) -> Check {
    const NAME: &str = "Project instructions";
    let root = &r.workspace.root;
    let file = context::PROJECT_FILE;
    // The session reads this file as the account the tools run as. An
    // account that may not even look at the project has nothing to report
    // about it -- least of all "the session will run without it" (F1).
    if matches!(paths::see_dir(root), paths::Seen::Hidden) {
        return Check::skip(
            NAME,
            format!(
                "not checked: {} is not allowed to look at {}, and the session reads {file} as the account that runs the tools",
                whoami(),
                root.display()
            ),
        );
    }
    match context::find(root, context::budget(r.name, &context::named(root))) {
        Ok(None) => Check::ok(
            NAME,
            format!(
                "no {file} at {}; the session gets ccnm's own instructions only",
                root.display()
            ),
        ),
        Ok(Some(p)) if !p.truncated() => Check::ok(
            NAME,
            format!("{file}, {} bytes, all of it reaches the model", p.bytes),
        ),
        Ok(Some(p)) => Check::warn(
            NAME,
            format!(
                "{file} is {} bytes and only its first {} bytes reach the model: Claude Code keeps {} {} of the MCP handshake, and ccnm's own lines come first\nmove what the model does not need out of the root file; it can still read the whole thing with read_file {file}",
                p.bytes,
                p.included(),
                context::CLAUDE_CODE_CAP.limit(),
                context::CLAUDE_CODE_CAP.unit()
            ),
        ),
        Err(e) => Check::warn(
            NAME,
            format!(
                "{}\nthe session will run without the project's own instructions",
                e.message()
            ),
        ),
    }
}

/// The account doctor runs as, for a row that has to say whose view it is.
fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "this account".to_string())
}

/// The project root, as the account running doctor sees it.
///
/// That account is the Operator, and the project is the Runtime Executor's.
/// When the Operator may not look -- the project is in the Executor's home
/// and the home is 0700, which Debian has made the default -- this row used
/// to FAIL with `cannot stat` for a project that was fine (F1). It is a
/// SKIP: nothing was verified, and `answered_by` says where the account
/// that can look gives its answer.
/// `in_this_report`: whether the Runtime Executor's answer is a row of the
/// same report (the `Workspace root` row a probe brings back). Only then
/// is "this account may not look" a NOTE; without that row nobody here
/// answered whether the project is there, and that is a SKIP.
fn runtime_workspace(root: &Path, answered_by: &str, in_this_report: bool) -> Check {
    const NAME: &str = "Runtime workspace";
    match paths::see_dir(root) {
        paths::Seen::Dir => Check::ok(NAME, root.display().to_string()),
        paths::Seen::NotDir => Check::fail_with(
            NAME,
            ErrorCode::WrongWorkspace,
            format!("{} is not a directory", root.display()),
        ),
        paths::Seen::Missing => Check::fail_with(
            NAME,
            ErrorCode::WrongWorkspace,
            format!("{} does not exist on this machine", root.display()),
        ),
        paths::Seen::Hidden => {
            let detail = format!(
                "not checked: {} is not allowed to look at {} (Permission denied), so this account cannot say whether the project is there\nthe account that runs the tools can: {answered_by}",
                whoami(),
                root.display()
            );
            if in_this_report {
                Check::note(NAME, detail)
            } else {
                Check::skip(NAME, detail)
            }
        }
        paths::Seen::Unreadable(e) => Check::fail(
            NAME,
            &Error::internal(format!("cannot stat {}", root.display())).with_source(e),
        ),
    }
}

/// The Agent Node will run `<runtime ccnm_bin> internal ...` over ssh on
/// this machine. Look at that exact path now, as this user, so a missing
/// or stale install is reported here instead of as a cryptic exit 127
/// from the other side.
fn runtime_ccnm(r: &Resolved<'_>, env: &Env<'_>) -> Check {
    const NAME: &str = "Runtime ccnm";
    let configured = r.runtime.ccnm_bin();
    let path = paths::expand_home(&configured, &env.home);
    if !crate::process::is_executable(&path) {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                // Who dials in depends on the entry: the Agent Node on the
                // managed path, an external MCP client's bridge on the
                // other. Naming the Agent Node on a workspace that has no
                // Agent would send the reader looking for one.
                "{configured} is not an executable on this Runtime Node, but {} will invoke it over ssh\ninstall this build there: cp $(which ccnm) {}   (or set nodes.{}.ccnm_bin)",
                if r.agent.is_some() {
                    "the Agent Node"
                } else {
                    "an external MCP client's bridge"
                },
                path.display(),
                r.workspace.runtime_node
            ),
        );
    }
    let cmd = Cmd::new(&path)
        .arg("--version")
        .timeout(Duration::from_secs(10));
    let out = match env.runner.run(&cmd) {
        Ok(out) => out,
        Err(e) => return Check::fail(NAME, &e.with_code(ErrorCode::Version)),
    };
    let stdout = out.stdout_lossy();
    let version = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    if !out.success() || version.is_empty() {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                "{} --version failed (exit {:?}): {}",
                path.display(),
                out.exit_code,
                out.stderr_lossy().trim()
            ),
        );
    }
    if version != crate::VERSION {
        return Check::fail_with(
            NAME,
            ErrorCode::Version,
            format!(
                "{} is ccnm {version}, this one is {}; install the same build",
                path.display(),
                crate::VERSION
            ),
        );
    }
    Check::ok(NAME, format!("{version} at {}", path.display()))
}

/// Boundaries this read-only probe cannot prove by itself. NOTE, not SKIP:
/// no state of either machine would turn them into a verdict here.
fn not_yet_implemented() -> Vec<Check> {
    [
        (
            "Native tool policy",
            "not checked: only a live selected Agent session proves its effective tool set",
        ),
        (
            "Network isolation",
            "not checked: enforced outside ccnm; verify the Runtime execution identity's egress policy",
        ),
    ]
    .into_iter()
    .map(|(name, reason)| Check::note(name, reason))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, Output};
    use ccnm_testdir::TestDir;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    /// A per-test directory with `root/` (created only if `with_root`), a
    /// fake `home/.local/bin/ccnm` (created only if `with_bin`) and a
    /// config pointing at them.
    fn setup(test: &str, with_root: bool, with_bin: bool) -> (TestDir, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ccnm-doctor-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(control(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("root");
        if with_root {
            std::fs::create_dir_all(&root).unwrap();
        }
        if with_bin {
            let bin_dir = dir.join("home/.local/bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bin = bin_dir.join("ccnm");
            std::fs::write(&bin, format!("#!/bin/sh\necho ccnm {}\n", crate::VERSION)).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\n[workspaces.xshun]\nagent_node = \"agent\"\nroot = \"{}\"\n",
                root.display()
            ),
        )
        .unwrap();
        let sockets = control(&dir);
        (TestDir::adopt(dir).also(sockets), config)
    }

    /// ControlPath may expand to at most 103 bytes and macOS `temp_dir()`
    /// alone is about 60, so socket directories go under /tmp instead.
    fn control(dir: &Path) -> PathBuf {
        PathBuf::from("/tmp/ccnm-t").join(dir.file_name().unwrap())
    }

    fn env<'a>(fake: &'a FakeRunner, dir: &Path) -> Env<'a> {
        Env {
            runner: fake,
            control_dir: control(dir),
            home: dir.join("home"),
        }
    }

    /// What the Runtime Executor reports about a machine set up per
    /// docs/production-safety.md.
    fn confined_report() -> crate::runtime::AuditReport {
        crate::runtime::AuditReport {
            protocol: crate::runtime::OPEN_PROTOCOL,
            audit: confined_audit(),
            root: crate::runtime::RootStatus {
                present: true,
                is_dir: true,
                owned: true,
                git: crate::runtime::GitStatus::Usable,
            },
            allow_unconfined_exec: false,
            allow_unisolated_credentials: false,
            allow_unattended_exec: false,
            opener_config: Some(Vec::new()),
        }
    }

    /// The audit a machine set up per docs/production-safety.md produces.
    fn confined_audit() -> safety::Audit {
        safety::Audit {
            user: "ccrun".into(),
            findings: vec![safety::Finding {
                check: "Runtime user".into(),
                severity: safety::Severity::Ok,
                detail: "ccrun".into(),
                fix: None,
            }],
            shared_account: false,
        }
    }

    /// P78: a Runtime with no runtime_user reports a shared account. Its
    /// isolation rows arrive as warnings, so the table is READY, and the
    /// exec_command row says commands run -- as whom, and that it is not
    /// because the account is isolated. Before P78 the same machine was a
    /// FAIL on `Runtime user` and on `exec_command`.
    #[test]
    fn a_shared_account_is_ready_and_says_it_is_not_isolated() {
        let warn = |check: &str, detail: &str| safety::Finding {
            check: check.into(),
            severity: safety::Severity::Warn,
            detail: detail.into(),
            fix: None,
        };
        let report = crate::runtime::AuditReport {
            audit: safety::Audit {
                user: "bing".into(),
                findings: vec![
                    warn(
                        "Runtime user",
                        "no runtime_user is configured, so bing is used as a shared account",
                    ),
                    warn(
                        "Not an admin",
                        "this account is in admin, which is a route to root",
                    ),
                    warn(
                        "No Claude credential",
                        "the Runtime identity can access a known Agent credential file",
                    ),
                ],
                shared_account: true,
            },
            ..confined_report()
        };
        let rows = runtime_safety_rows(&report, AgentProvider::Claude);
        for row in &rows {
            assert!(!matches!(row.status, Status::Fail(_)), "{row:?}");
        }
        let exec = rows.iter().find(|r| r.name == "exec_command").unwrap();
        assert_eq!(exec.status, Status::Ok, "{exec:?}");
        assert!(exec.detail.contains("shared account"), "{exec:?}");
        assert!(exec.detail.contains("bing"), "{exec:?}");
        assert!(!exec.detail.contains("confined"), "{exec:?}");
    }

    /// The approval row answers "will this stop and ask me?", and it has to
    /// answer both ways: a warning whose opposite nobody ever sees reads
    /// like noise, and after the one-time notice has scrolled away this row
    /// is the only place the decision is still visible.
    #[test]
    fn the_approval_row_says_which_way_this_workspace_set_it() {
        let asking = runtime_safety_rows(&confined_report(), AgentProvider::Claude);
        let row = asking
            .iter()
            .find(|r| r.name == "Command approval")
            .expect("shown even when nothing was accepted");
        assert_eq!(row.status, Status::Ok, "{row:?}");
        assert!(
            row.detail.contains("ask before each exec_command"),
            "{row:?}"
        );

        let unattended = runtime_safety_rows(
            &crate::runtime::AuditReport {
                allow_unattended_exec: true,
                ..confined_report()
            },
            AgentProvider::Claude,
        );
        let row = unattended
            .iter()
            .find(|r| r.name == "Command approval")
            .unwrap();
        assert_eq!(
            row.status,
            Status::Warn,
            "accepted is never a pass: {row:?}"
        );
        assert!(row.detail.contains("allow_unattended_exec"), "{row:?}");
        // It is not an authorization, so it must not move the exec verdict.
        let exec = |rows: &[Check]| {
            rows.iter()
                .find(|r| r.name == "exec_command")
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(exec(&asking), exec(&unattended));
    }

    /// F21. On the P62 machines a managed Codex session ran `exec_command`
    /// without asking while this row said it would ask; P69 made the row say
    /// "does not ask". Since P71 it asks -- with the one difference from
    /// Claude the row has to name: the person can hand the decision back to
    /// Codex in `/permissions`. With `allow_unattended_exec` both say WARN.
    #[test]
    fn the_approval_row_says_how_codex_sessions_ask() {
        let (dir, config) = setup("approval-codex", true, true);
        let authority = authority(&dir.join("root"));
        let report = from_agent(&config, "xshun", Ok((&authority, &codex_probe())));
        let text = report.render();
        let approval = row(&report, "Command approval");
        assert_eq!(approval.status, Status::Ok, "{text}");
        assert!(
            approval.detail.contains("ask before each exec_command"),
            "{text}"
        );
        assert!(approval.detail.contains("/permissions"), "{text}");
        assert!(
            !approval.detail.contains("in every permission mode"),
            "{text}"
        );
        // P72 (F27): the switch is no longer remembered across sessions.
        assert!(
            approval.detail.contains("the next session asks again"),
            "{text}"
        );

        let claude = from_agent(&config, "xshun", Ok((&authority, &good_probe())));
        let approval = row(&claude, "Command approval");
        assert_eq!(approval.status, Status::Ok);
        assert!(approval.detail.contains("in every permission mode"));

        let unattended = runtime_safety_rows(
            &crate::runtime::AuditReport {
                allow_unattended_exec: true,
                ..confined_report()
            },
            AgentProvider::Codex,
        );
        let approval = unattended
            .iter()
            .find(|r| r.name == "Command approval")
            .unwrap();
        assert_eq!(approval.status, Status::Warn, "{approval:?}");
        assert!(
            approval.detail.contains("allow_unattended_exec"),
            "{approval:?}"
        );
    }

    /// An accepted risk is a WARN, never an OK.
    ///
    /// The property does not hold. Somebody said they can live with that,
    /// which is a reason not to refuse and never a reason to report green
    /// -- the row is the only place the decision stays visible after the
    /// one-time warning has scrolled away.
    #[test]
    fn an_accepted_credential_is_a_warning_that_says_so_not_a_pass() {
        let credential = safety::Finding {
            check: "No Claude credential".into(),
            severity: safety::Severity::Fail,
            detail: "the Runtime identity can access a known Agent credential".into(),
            fix: Some("use a separate execution identity".into()),
        };
        let report = crate::runtime::AuditReport {
            audit: safety::Audit {
                user: "bing".into(),
                findings: vec![credential.clone()],
                shared_account: false,
            },
            allow_unconfined_exec: true,
            allow_unisolated_credentials: true,
            ..confined_report()
        };
        let rows = runtime_safety_rows(&report, AgentProvider::Claude);
        let row = rows
            .iter()
            .find(|r| r.name == "No Claude credential")
            .expect("the row stays, under its own name");
        assert_eq!(row.status, Status::Warn, "{row:?}");
        assert!(
            row.detail.contains("allow_unisolated_credentials"),
            "{row:?}"
        );
        let exec = rows.iter().find(|r| r.name == "exec_command").unwrap();
        assert_eq!(exec.status, Status::Warn, "{exec:?}");
        assert!(exec.detail.contains("read a known Agent login"), "{exec:?}");

        // Without the switch that names credentials it is still a FAIL,
        // whatever else the workspace has accepted.
        let refused = crate::runtime::AuditReport {
            allow_unisolated_credentials: false,
            ..report
        };
        let rows = runtime_safety_rows(&refused, AgentProvider::Claude);
        let row = rows
            .iter()
            .find(|r| r.name == "No Claude credential")
            .unwrap();
        assert!(matches!(row.status, Status::Fail(_)), "{row:?}");
    }

    /// P85. A report as the wire carries it, so the same test reads an
    /// older Runtime's answer by leaving the field out.
    fn report_with(opener_config: Option<serde_json::Value>, shared: bool) -> Vec<Check> {
        let mut wire = serde_json::to_value(confined_report()).unwrap();
        match opener_config {
            Some(names) => wire["opener_config"] = names,
            None => {
                wire.as_object_mut().unwrap().remove("opener_config");
            }
        }
        wire["audit"]["shared_account"] = serde_json::json!(shared);
        let report = serde_json::from_value(wire).unwrap();
        runtime_safety_rows(&report, AgentProvider::Claude)
    }

    /// P85: with a dedicated account the project belongs to it, so what it
    /// writes into an IDE's tasks or Claude Code's hooks runs as the person
    /// who opens the tree. chmod cannot stop an owner, so the row says
    /// what is there and what to do -- a warning, never a refusal.
    #[test]
    fn a_dedicated_account_is_told_which_files_act_as_whoever_opens_the_project() {
        const NAME: &str = "Files that act as you";
        let rows = report_with(Some(serde_json::json!([".claude", ".vscode"])), false);
        let row = rows
            .iter()
            .find(|r| r.name == NAME)
            .expect("shown for a dedicated account");
        assert_eq!(row.status, Status::Warn, "{row:?}");
        assert!(row.detail.contains("ccrun"), "{row:?}");
        assert!(
            row.detail.contains("present now: .claude, .vscode"),
            "{row:?}"
        );
        assert!(row.detail.contains("safe.directory"), "{row:?}");
        // It cannot know who opens the tree: on a Runtime whose runtime_user
        // is the person's own account (the 2026-10-10 daily setup) "your git
        // refuses it" was simply false.
        assert!(!row.detail.contains("your git"), "{row:?}");
        assert!(
            row.detail.contains("if ccrun is your own account"),
            "{row:?}"
        );
        assert!(row.detail.contains("chmod"), "{row:?}");
        assert_eq!(row_label(Lang::Zh, NAME), "以你身份生效的文件");

        let none = report_with(Some(serde_json::json!([])), false);
        let row = none.iter().find(|r| r.name == NAME).unwrap();
        assert_eq!(row.status, Status::Ok, "nothing to read first: {row:?}");
        assert!(row.detail.contains("none present"), "{row:?}");
        assert!(row.detail.contains("can create"), "{row:?}");

        // A Runtime from before P85 did not say. Not a pass, but not a
        // reason to hold the table back either: the row never decides.
        let older = report_with(None, false);
        let row = older.iter().find(|r| r.name == NAME).unwrap();
        assert_eq!(row.status, Status::Note, "{row:?}");

        // A shared account already runs every command as you: these files
        // add nothing, and the row would be noise.
        let shared = report_with(Some(serde_json::json!([".vscode"])), true);
        assert!(shared.iter().all(|r| r.name != NAME), "{shared:?}");
    }

    fn unconfined_audit() -> safety::Audit {
        safety::Audit {
            user: "fodelf".into(),
            findings: vec![safety::Finding {
                check: "No sudo".into(),
                severity: safety::Severity::Fail,
                detail: "this account has passwordless sudo".into(),
                fix: Some("remove it from the sudoers file".into()),
            }],
            shared_account: false,
        }
    }

    fn hello(user: &str, version: &str, root_ok: Option<bool>) -> HelloReport {
        HelloReport {
            protocol: PROTOCOL,
            ccnm_version: version.into(),
            user: user.into(),
            platform: "macos/aarch64".into(),
            exe: Some(PathBuf::from(format!("/Users/{user}/.local/bin/ccnm"))),
            root: root_ok.map(|ok| crate::protocol::hello::PathStatus {
                exists: ok,
                is_dir: ok,
            }),
            wire: Some(crate::protocol::payload::WIRE_LEVEL),
        }
    }

    /// A controller answering from the login session, which is the only
    /// context whose answer about Claude counts.
    fn controller(manager: &str) -> crate::controller::Context {
        crate::controller::Context {
            hello: hello("me", crate::VERSION, None),
            pid: 4711,
            manager: Ok(manager.to_string()),
            platform: None,
            linger: None,
        }
    }

    fn good_probe() -> ProbeReport {
        use crate::provider::AgentReport;
        use crate::provider::AuthStatus;
        ProbeReport {
            agent_identity: None,
            provider: Default::default(),
            protocol: PROTOCOL,
            hello: hello("me", crate::VERSION, None),
            controller: Some(Ok(controller("Aqua"))),
            agent: AgentReport {
                path: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                version: Ok("2.1.259".into()),
                auth: Ok(AuthStatus {
                    logged_in: true,
                    auth_method: Some("claude.ai".into()),
                    email: Some("me@x".into()),
                    subscription_type: Some("max".into()),
                }),
            },
            rejected: None,
            runtime_ssh: Some(Ok(crate::ssh::ResolvedSsh {
                hostname: "runtime.t.ts.net".into(),
                user: "ccrun".into(),
                port: 22,
                identity_files: vec![],
                proxy_jump: None,
            })),
            runtime_hello: Some(Ok(hello("ccrun", crate::VERSION, Some(true)))),
            runtime_audit: Some(Ok(confined_report())),
            mcp: Some(Ok(crate::protocol::mcp::ProbeReport {
                connect_us: 190_000,
                server_name: "ccnm".into(),
                server_version: crate::VERSION.into(),
                instructions_bytes: 180,
                project_instructions: Some("no CLAUDE.md at the workspace root".into()),
                tools: vec!["workspace_info".into()],
                asks_user: vec![],
                tools_list_bytes: 412,
                calls: 1,
                call_p50_us: 22_000,
                call_p95_us: 22_000,
                call_max_us: 22_000,
                server_pid: 4242,
                single_process: true,
            })),
            terminal: Some(crate::protocol::run::StatusReport {
                records: vec![],
                other_instances: vec![],
                agent_identity: None,

                protocol: PROTOCOL,
                tmux: Ok("3.7c".into()),
                sessions: vec![crate::protocol::run::LiveSession {
                    agent_identity: None,

                    provider: Default::default(),
                    tmux_session: "ccnm-xshun".into(),
                    workspace: Some("xshun".into()),
                    session: Some("s-1".into()),
                    created: 1_788_496_263,
                    attached: 0,
                    context: Some(crate::session::Context {
                        manager: Some("Background".into()),
                        keychain: Some(true),
                    }),
                    tools: Some(true),
                }],
            }),
        }
    }

    fn row<'a>(report: &'a Report, name: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no row {name} in\n{}", report.render()))
    }

    /// A workspace that only external MCP clients open has no Agent, and
    /// that is a legal shape `validate()` accepts on purpose. P12 found
    /// doctor answering it with `CCNM_E_INTERNAL: workspace 'x' passed
    /// validation but its Agent Node is missing` — an internal-bug message
    /// for a correct configuration, which sends whoever reads it looking
    /// for a config error that is not there.
    ///
    /// What it must do instead: report the policy and the rows this machine
    /// can prove, mark the rows it cannot answer by design as NOTE, and --
    /// since P83 -- be READY. Before P83 the two safety rows were SKIPs and
    /// the verdict was NOT READY for every such workspace on every machine,
    /// for ever (seen on real hardware at v0.13.0).
    #[test]
    fn an_external_mcp_only_workspace_is_diagnosed_not_called_a_bug() {
        let dir = std::env::temp_dir().join(format!("ccnm-doctor-{}-extonly", std::process::id()));
        let _cleanup = TestDir::adopt(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let bin_dir = dir.join("home/.local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("ccnm");
        std::fs::write(&bin, format!("#!/bin/sh\necho ccnm {}\n", crate::VERSION)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "this = \"runtime\"\n[nodes.runtime]\nruntime_user = \"ccrun\"\n\
                 [workspaces.remote]\nroot = \"{}\"\nexternal_mcp = \"coding\"\n",
                root.display()
            ),
        )
        .unwrap();

        let fake = FakeRunner::new();
        // The one command this path runs: the local ccnm's own version.
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        let report = run(&config, Some("remote"), &env(&fake, &dir));
        let text = report.render();

        assert!(!text.contains("CCNM_E_INTERNAL"), "{text}");
        // Nothing failed and nothing is unknown: every row this machine
        // cannot answer is one no machine state would change, and says why.
        assert!(
            report
                .checks
                .iter()
                .all(|c| matches!(c.status, Status::Ok | Status::Warn | Status::Note)),
            "{text}"
        );
        assert_eq!(report.exit_code(), 0, "{text}");
        assert!(text.contains("READY ("), "{text}");
        assert!(!text.contains("NOT READY"), "{text}");
        let policy = row(&report, "External MCP");
        assert_eq!(policy.status, Status::Ok);
        assert!(
            policy.detail.contains("external_mcp = \"coding\""),
            "{text}"
        );
        assert!(policy.detail.contains("ccnm mcp bridge remote"), "{text}");
        // The rows this machine can prove are answered, not skipped.
        assert_eq!(row(&report, "Runtime workspace").status, Status::Ok);
        assert_eq!(row(&report, "Runtime ccnm").status, Status::Ok);
        // The Agent half does not apply (NOTE since P73: no Agent, nothing
        // to diagnose). The safety verdict is not answered with an audit of
        // whoever typed this either -- it belongs to the account the tools
        // run as -- but it is not missing: the server takes it when a client
        // connects and refuses a failing account. So NOTE (P83), saying so.
        for name in ["Agent SSH", "Controller", "Remote MCP handshake"] {
            assert_eq!(row(&report, name).status, Status::Note, "{name}: {text}");
        }
        for name in ["Runtime safety", "exec_command"] {
            let row = row(&report, name);
            assert_eq!(row.status, Status::Note, "{name}: {text}");
            assert!(
                row.detail
                    .contains("belongs to the account the tools run as"),
                "{name}: {text}"
            );
            assert!(
                row.detail.contains("when a client connects")
                    && row.detail.contains("CCNM_E_POLICY"),
                "{name}: {text}"
            );
        }
        // Only the local version check ran: there is nowhere to dial.
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].display().ends_with("ccnm --version"), "{calls:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same config through a managed entry point: it needs an Agent and
    /// now says so by name instead of failing as an internal error.
    #[test]
    fn a_managed_command_on_an_agentless_workspace_says_which_entry_to_use() {
        let config = Config::parse(
            "this = \"runtime\"\n[nodes.runtime]\n\
             [workspaces.remote]\nroot = \"/tmp\"\nexternal_mcp = \"read\"\n",
        )
        .unwrap();
        let resolved = config.workspace("remote").unwrap();
        assert!(resolved.agent.is_none());
        let error = resolved.agent_ssh().unwrap_err();
        assert_eq!(error.code(), ErrorCode::Config);
        assert!(error.message().contains("has no Agent"), "{error}");
        assert!(
            error.message().contains("ccnm mcp bridge remote"),
            "{error}"
        );
    }

    #[test]
    fn missing_config_is_the_only_row_and_exits_config() {
        let fake = FakeRunner::new();
        let report = run(
            Path::new("/nonexistent/config.toml"),
            Some("xshun"),
            &env(&fake, Path::new("/tmp")),
        );
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].status, Status::Fail(ErrorCode::Config));
        assert_eq!(report.exit_code(), 10);
        let text = report.render();
        assert!(text.contains("CCNM_E_CONFIG"), "{text}");
        assert!(
            text.ends_with("NOT READY (1 failed, 0 not checked)\n"),
            "{text}"
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn config_only_run_is_ready_and_lists_workspaces() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-valid.toml"),
            None,
            &env(&fake, Path::new("/tmp")),
        );
        assert!(report.ready(), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
        let text = report.render();
        assert!(
            text.contains("Workspaces              OK     xshun"),
            "{text}"
        );
        assert!(text.ends_with("\nREADY\n"), "{text}");
    }

    #[test]
    fn unknown_workspace_fails_config() {
        let fake = FakeRunner::new();
        let report = run(
            &fixture("config-valid.toml"),
            Some("other"),
            &env(&fake, Path::new("/tmp")),
        );
        assert_eq!(report.exit_code(), 10);
        assert!(
            row(&report, "Workspace config")
                .detail
                .contains("defined: xshun")
        );
    }

    /// The stop point of P7.4 Batch D: whoever types the command, the rows
    /// about the Runtime Executor say the same thing.
    ///
    /// The second operator here is one the old code would have judged --
    /// their own home holds a private key, in `~/.ssh` and in ccnm's config
    /// directory. Doctor used to audit that home and report it as the
    /// Runtime's, which is how the same workspace read 0 failed as `ccrun`
    /// and 7 failed as the operator's own login. Now nothing about the
    /// caller reaches these rows: they arrive in the probe, from the
    /// account the Agent's transport lands on.
    #[test]
    fn the_runtime_rows_do_not_depend_on_who_ran_doctor() {
        const RUNTIME_ROWS: [&str; 3] = ["Runtime user", "exec_command", "Workspace root"];
        let (dir, config) = setup("two-operators", true, true);

        // A second operator home, with everything the old local audit would
        // have failed on, plus the ccnm the control path needs to find.
        let loaded = dir.join("loaded-home");
        std::fs::create_dir_all(loaded.join(".ssh")).unwrap();
        std::fs::write(
            loaded.join(".ssh/id_ed25519"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic\n",
        )
        .unwrap();
        std::fs::create_dir_all(loaded.join(".config/ccnm/transport")).unwrap();
        std::fs::write(
            loaded.join(".config/ccnm/transport/runtime"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic\n",
        )
        .unwrap();
        let bin = loaded.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("ccnm"),
            format!("#!/bin/sh\necho ccnm {}\n", crate::VERSION),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("ccnm"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let executor_rows = |home: PathBuf| {
            let fake = FakeRunner::new();
            fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
            fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
            fake.push(Output::exited(
                0,
                serde_json::to_string(&good_probe()).unwrap(),
            ));
            let env = Env {
                runner: &fake,
                control_dir: control(&dir),
                home,
            };
            let report = run(&config, Some("xshun"), &env);
            RUNTIME_ROWS
                .iter()
                .map(|name| {
                    let row = row(&report, name);
                    (row.status.clone(), row.detail.clone())
                })
                .collect::<Vec<_>>()
        };
        let plain = executor_rows(dir.join("home"));
        assert_eq!(plain, executor_rows(loaded));
        // And they are the Runtime's answer, not a default: `ccrun` is what
        // the probe reported, and this machine's account is not called that.
        assert!(plain[0].1.contains("ccrun"), "{plain:?}");
    }

    /// The stop point of P7.4 Batch D2: the same Runtime Executor,
    /// described from both machines, says the same thing.
    ///
    /// The Agent Node has no workspace list, so its table is built from the
    /// Runtime's own answer plus a probe it ran itself. If the two sources
    /// could disagree, "run doctor over there" would become folklore about
    /// which machine tells the truth.
    #[test]
    fn both_diagnostic_sources_describe_the_runtime_executor_the_same_way() {
        const EXECUTOR_ROWS: [&str; 3] = ["Runtime user", "exec_command", "Workspace root"];
        let (dir, config) = setup("two-sources", true, true);
        let probe = good_probe();

        // The Runtime Node's own table, with the probe scripted in.
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));
        let from_runtime = run(&config, Some("xshun"), &env(&fake, &dir));

        // The Agent Node's, from the Runtime's answer and the same probe.
        // Only the first row differs by construction: it names whichever
        // config file the reader is holding.
        let authority = crate::runtime::ResolveReport {
            protocol: crate::runtime::OPEN_PROTOCOL,
            workspace: "xshun".into(),
            root: dir.join("root"),
            runtime_node: "runtime".into(),
            agent: None,
            provider_config_dir: None,
            permission_mode: Default::default(),
            allow_unisolated_credentials: false,
            allow_unattended_exec: false,
            agent_tools: Default::default(),
        };
        let from_agent = from_agent(&config, "xshun", Ok((&authority, &probe)));

        for name in EXECUTOR_ROWS {
            let there = row(&from_runtime, name);
            let here = row(&from_agent, name);
            assert_eq!(
                (&there.status, &there.detail),
                (&here.status, &here.detail),
                "{name} differs between the two sources"
            );
        }
    }

    /// The Runtime's answer for a workspace, as the Agent Node receives it.
    fn authority(root: &Path) -> crate::runtime::ResolveReport {
        crate::runtime::ResolveReport {
            protocol: crate::runtime::OPEN_PROTOCOL,
            workspace: "xshun".into(),
            root: root.to_path_buf(),
            runtime_node: "runtime".into(),
            agent: None,
            provider_config_dir: None,
            permission_mode: Default::default(),
            allow_unisolated_credentials: false,
            allow_unattended_exec: false,
            agent_tools: Default::default(),
        }
    }

    /// A probe from a Codex Agent.
    fn codex_probe() -> ProbeReport {
        ProbeReport {
            provider: AgentProvider::Codex,
            ..good_probe()
        }
    }

    /// What an Agent Node answers when it refuses the instance it was asked
    /// to probe -- here because the profile directory is not private -- as
    /// JSON, the way it arrives.
    fn refused_probe(why: &str) -> serde_json::Value {
        let refusal = serde_json::to_value(ErrorReport::new(ErrorCode::Policy, why)).unwrap();
        let mut probe = serde_json::to_value(good_probe()).unwrap();
        let fields = probe.as_object_mut().unwrap();
        fields.remove("agent_identity");
        fields.insert("rejected".into(), refusal.clone());
        fields.insert("controller".into(), serde_json::json!({"Err": refusal}));
        fields.insert(
            "claude".into(),
            serde_json::json!({"path": null, "version": {"Err": refusal}, "auth": {"Err": refusal}}),
        );
        for nothing in ["runtime_ssh", "runtime_hello", "runtime_audit", "mcp"] {
            fields.insert(nothing.into(), serde_json::Value::Null);
        }
        probe
    }

    /// F10, from the Runtime Node. On the P62 machine the Agent refused the
    /// selected instance (its `~/.claude` had the wrong mode) and the
    /// Operator's doctor said only "Agent probe identity differs from the
    /// Runtime selection": the refused probe carries no identity, and the
    /// comparison stopped there. The reason had made the trip; it was
    /// dropped on arrival.
    #[test]
    fn a_selection_the_agent_refused_is_reported_with_the_agents_reason() {
        const WHY: &str = "the profile directory for claude-main must be private (mode 700)";
        let (dir, _) = setup("refused-selection", true, true);
        let config = dir.join("instance.toml");
        std::fs::write(
            &config,
            format!(
                "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\n[workspaces.xshun]\nroot = \"{}\"\nagent = {{ node = \"agent\", instance = \"claude-main\" }}\n",
                dir.join("root").display()
            ),
        )
        .unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, refused_probe(WHY).to_string()));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let text = report.render();

        let selection = row(&report, "Agent selection");
        assert_eq!(selection.status, Status::Fail(ErrorCode::Policy), "{text}");
        assert!(selection.detail.contains(WHY), "{text}");
        assert!(!text.contains("identity differs"), "{text}");
        // The link itself worked, and says so; what was not probed says
        // why, rather than reading as the reverse link being absent.
        assert_eq!(row(&report, "Agent SSH").status, Status::Ok, "{text}");
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Skip, "{text}");
        assert!(reverse.detail.contains("refused"), "{text}");
        assert!(!text.contains("are both on"), "{text}");
        assert_eq!(report.exit_code(), ErrorCode::Policy.exit_code());
    }

    /// An instance workspace on the Runtime Node whose Agent answers the
    /// probe with `probe`.
    fn doctor_with_probe(test: &str, probe: serde_json::Value) -> Report {
        let (dir, _) = setup(test, true, true);
        let config = dir.join("instance.toml");
        std::fs::write(
            &config,
            format!(
                "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\n[workspaces.xshun]\nroot = \"{}\"\nagent = {{ node = \"agent\", instance = \"claude-main\" }}\n",
                dir.join("root").display()
            ),
        )
        .unwrap();
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, probe.to_string()));
        run(&config, Some("xshun"), &env(&fake, &dir))
    }

    /// F20. On the P62 machines a 0.10.1 Operator asked fodelf's 0.9.0
    /// Agent and got one failing row, `Agent probe identity differs from the
    /// Runtime selection`: the old Agent did not take the request the way
    /// this build meant it, its report came back without an identity, and
    /// that comparison ran first and returned. The rows that would have said
    /// "the two machines run different ccnm" were never reached.
    #[test]
    fn an_agent_on_another_version_is_named_before_any_identity_is_compared() {
        let mut probe = serde_json::to_value(good_probe()).unwrap();
        probe["hello"]["ccnm_version"] = "0.9.0".into();
        // 0.9.0 predates the field (P64).
        probe["hello"].as_object_mut().unwrap().remove("wire");
        let report = doctor_with_probe("old-agent", probe);
        let text = report.render();

        let version = row(&report, "Agent ccnm");
        assert_eq!(version.status, Status::Fail(ErrorCode::Version), "{text}");
        assert!(version.detail.contains("runs ccnm 0.9.0"), "{text}");
        assert!(!text.contains("identity differs"), "{text}");
        // Not a second verdict next to the real one: an Agent on another
        // build may not have read the selection at all.
        assert_eq!(
            row(&report, "Agent selection").status,
            Status::Skip,
            "{text}"
        );
        assert_eq!(row(&report, "Agent SSH").status, Status::Ok, "{text}");
        assert_eq!(report.exit_code(), ErrorCode::Version.exit_code());

        // The same number from another build is the same kind of answer.
        let mut probe = serde_json::to_value(good_probe()).unwrap();
        probe["hello"].as_object_mut().unwrap().remove("wire");
        let report = doctor_with_probe("old-build", probe);
        let text = report.render();
        assert_eq!(
            row(&report, "Agent ccnm").status,
            Status::Fail(ErrorCode::Version),
            "{text}"
        );
        assert!(!text.contains("identity differs"), "{text}");
    }

    /// F25, F26 (P62.4, 2026-10-04). A 0.9.0 Agent that read the selection
    /// fine relays the Runtime's hello through its own struct, which has no
    /// `wire`; the Runtime ran this very build, and `Reverse SSH` still said
    /// it was "not the same build ... older than this build". The version
    /// row named the Agent `work` though the node was fodelf.
    #[test]
    fn an_old_agent_relaying_the_runtime_does_not_make_the_runtime_look_old() {
        let probe = |agent_version: &str, agent_wire: Option<u32>, runtime_version: &str| {
            let mut probe = good_probe();
            probe.protocol = 3;
            probe.agent_identity = Some(crate::instance::AgentIdentity {
                node: "agent".into(),
                instance: "claude-main".into(),
                provider: AgentProvider::Claude,
                profile_ref: "default".into(),
            });
            probe.hello.ccnm_version = agent_version.into();
            probe.hello.wire = agent_wire;
            let mut runtime = hello("ccrun", runtime_version, Some(true));
            // What reaches this machine: dropped on the way, or never sent.
            runtime.wire = None;
            probe.runtime_hello = Some(Ok(runtime));
            serde_json::to_value(probe).unwrap()
        };

        let report = doctor_with_probe("old-relay", probe("0.9.0", None, crate::VERSION));
        let text = report.render();
        let agent = row(&report, "Agent ccnm");
        assert_eq!(agent.status, Status::Fail(ErrorCode::Version), "{text}");
        assert!(
            agent
                .detail
                .contains("the Agent Node agent runs ccnm 0.9.0"),
            "{text}"
        );
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Skip, "{text}");
        assert!(reverse.detail.contains("build not compared"), "{text}");
        assert!(
            reverse.detail.contains(&format!("ccnm {}", crate::VERSION)),
            "{text}"
        );
        assert!(!text.contains("not the same build"), "{text}");

        // A version string the Agent cannot have mangled still counts.
        let report = doctor_with_probe("old-relay-old-runtime", probe("0.9.0", None, "0.8.0"));
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Fail(ErrorCode::Version));
        assert!(
            reverse.detail.contains("runs ccnm 0.8.0"),
            "{}",
            reverse.detail
        );

        // Relayed by this same build, a missing `wire` is the Runtime's own.
        let report = doctor_with_probe(
            "same-relay",
            probe(
                crate::VERSION,
                Some(crate::protocol::payload::WIRE_LEVEL),
                crate::VERSION,
            ),
        );
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Fail(ErrorCode::Version));
        assert!(
            reverse.detail.contains("not the same build"),
            "{}",
            reverse.detail
        );
    }

    /// The comparison F20 moved behind the version stays where it was for
    /// the case it is for: the same build answering for another instance.
    #[test]
    fn the_same_build_answering_for_another_instance_is_still_refused() {
        let mut probe = good_probe();
        // A report that carries an identity speaks the instance protocol.
        probe.protocol = 3;
        probe.agent_identity = Some(crate::instance::AgentIdentity {
            node: "agent".into(),
            instance: "codex-main".into(),
            provider: AgentProvider::Codex,
            profile_ref: "default".into(),
        });
        let report = doctor_with_probe("other-instance", serde_json::to_value(probe).unwrap());
        let text = report.render();
        let selection = row(&report, "Agent selection");
        assert_eq!(selection.status, Status::Fail(ErrorCode::Version), "{text}");
        assert!(selection.detail.contains("identity differs"), "{text}");
    }

    /// F10, from the Agent Node. The same refused probe used to read
    /// "agent and project are both on agent, so nothing dials back" there:
    /// a probe that never looked at the reverse link leaves it `None`, and
    /// `None` also meant "same machine".
    #[test]
    fn a_probe_that_never_looked_is_not_mistaken_for_a_colocated_workspace() {
        const WHY: &str = "the profile directory for claude-main must be private (mode 700)";
        let (dir, config) = setup("refused-agent-side", true, true);
        let authority = authority(&dir.join("root"));
        let probe: ProbeReport = serde_json::from_value(refused_probe(WHY)).unwrap();
        let report = from_agent(&config, "xshun", Ok((&authority, &probe)));
        let text = report.render();
        assert!(!text.contains("are both on"), "{text}");
        assert!(text.contains(WHY), "{text}");
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Skip, "{text}");
        assert!(reverse.detail.contains("refused"), "{text}");
    }

    /// F5. On the P62 machine the refresh token of ccnm's Codex profile had
    /// been revoked; `codex login status` said "Logged in using ChatGPT",
    /// doctor passed that on as OK, and the session's first message was
    /// where it came out. doctor cannot verify a token -- it may not read
    /// the login file, and the one official command it has does not ask the
    /// server -- so the row has to say what it did look at.
    #[test]
    fn the_codex_login_row_says_it_only_saw_the_local_login() {
        let (dir, config) = setup("codex-auth-local", true, true);
        let authority = authority(&dir.join("root"));
        let mut probe = codex_probe();
        probe.agent.auth = Ok(crate::provider::AuthStatus {
            logged_in: true,
            auth_method: Some("ChatGPT".into()),
            email: None,
            subscription_type: None,
        });
        let report = from_agent(&config, "xshun", Ok((&authority, &probe)));
        let auth = row(&report, "Codex authentication");
        // Still OK: what it checks did pass. The claim is what changes.
        assert_eq!(auth.status, Status::Ok, "{auth:?}");
        assert!(auth.detail.starts_with("logged in via ChatGPT"), "{auth:?}");
        assert!(
            auth.detail.contains("local login state only")
                && auth.detail.contains("not checked with the server"),
            "{auth:?}"
        );

        // Claude's row is not given a caveat nobody measured.
        let report = from_agent(&config, "xshun", Ok((&authority, &good_probe())));
        let claude = row(&report, "Claude authentication");
        assert!(
            !claude.detail.contains("local login state only"),
            "{claude:?}"
        );
    }

    /// Without the Runtime's answer the Agent Node has no workspace to
    /// check, and it must say so rather than describing one from a guess.
    #[test]
    fn an_agent_side_report_without_the_runtimes_answer_checks_nothing() {
        let (dir, config) = setup("no-answer", true, true);
        let _ = dir;
        let report = from_agent(
            &config,
            "xshun",
            Err(Error::new(
                ErrorCode::RuntimeUnreachable,
                "ssh runtime: down",
            )),
        );
        assert_eq!(
            row(&report, "Workspace config").status,
            Status::Fail(ErrorCode::RuntimeUnreachable)
        );
        for name in ["Runtime safety", "exec_command", "Workspace root"] {
            assert_eq!(row(&report, name).status, Status::Skip, "{name}");
        }
        assert_ne!(report.exit_code(), 0);
    }

    #[test]
    fn an_unconfined_runtime_fails_the_exec_row_and_the_whole_report() {
        let (dir, config) = setup("unconfined", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        let mut probe = good_probe();
        probe.runtime_audit = Some(Ok(crate::runtime::AuditReport {
            audit: unconfined_audit(),
            ..confined_report()
        }));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let text = report.render();
        // The finding itself, with its fix, and the verdict the runtime's
        // own gate will reach.
        assert_eq!(
            row(&report, "No sudo").status,
            Status::Fail(ErrorCode::Policy)
        );
        assert!(row(&report, "No sudo").detail.contains("fix: "), "{text}");
        assert_eq!(
            row(&report, "exec_command").status,
            Status::Fail(ErrorCode::Policy)
        );
        assert!(
            row(&report, "exec_command")
                .detail
                .contains("docs/production-safety.md"),
            "{text}"
        );
        assert_eq!(report.exit_code(), ErrorCode::Policy.exit_code(), "{text}");
    }

    /// The project's CLAUDE.md is what tells the model the project's
    /// rules, and a file too long for the handshake is invisible from the
    /// outside: the session runs, it just quietly knows less. So the row
    /// reports how many of its bytes reach the model, and being too long
    /// warns rather than fails.
    #[test]
    fn the_project_claude_md_row_measures_what_reaches_the_model() {
        let (dir, path) = setup("claudemd", true, true);
        let config = Config::load(&path).unwrap();
        let r = config.workspace("xshun").unwrap();
        let file = dir.join("root/CLAUDE.md");

        let none = project_instructions(&r);
        assert_eq!(none.status, Status::Ok);
        assert!(none.detail.starts_with("no CLAUDE.md at"), "{none:?}");

        std::fs::write(&file, "- 提交要小\n").unwrap();
        let small = project_instructions(&r);
        assert_eq!(small.status, Status::Ok);
        assert_eq!(
            small.detail,
            "CLAUDE.md, 15 bytes, all of it reaches the model"
        );

        std::fs::write(&file, "- 一条规则\n".repeat(4000)).unwrap();
        let big = project_instructions(&r);
        assert_eq!(big.status, Status::Warn);
        assert!(big.detail.contains("only its first"), "{big:?}");
        assert!(big.detail.contains("read_file CLAUDE.md"), "{big:?}");

        // There, but unreadable: still not a failure, and it says what
        // the session will be missing.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        let bad = project_instructions(&r);
        assert_eq!(bad.status, Status::Warn);
        assert!(bad.detail.contains("CLAUDE.md"), "{bad:?}");
    }

    #[test]
    fn everything_good_is_ready_and_notes_what_doctor_does_not_check() {
        let (dir, config) = setup("good", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\nuser me\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe()).unwrap(),
        ));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let text = report.render();
        for name in [
            "Config",
            "Workspace config",
            "Runtime workspace",
            "Project instructions",
            "Runtime ccnm",
            "Agent SSH",
            "Agent ccnm",
            "Controller",
            "Claude Code",
            "Claude authentication",
            "Reverse SSH",
            "Remote MCP handshake",
            "Workspace root",
        ] {
            assert_eq!(row(&report, name).status, Status::Ok, "{name}:\n{text}");
        }
        assert_eq!(
            row(&report, "Controller").detail,
            format!("ccnm {} as me, pid 4711, Aqua", crate::VERSION)
        );
        assert_eq!(
            row(&report, "Remote MCP handshake").detail,
            "initialize in 190 ms, tools/list (1 tool, 412 B), instructions 180 B (no CLAUDE.md at the workspace root), workspace_info x1 p50 22 ms p95 22 ms max 22 ms, pid 4242 throughout"
        );
        assert_eq!(
            row(&report, "Runtime ccnm").detail,
            format!(
                "{} at {}",
                crate::VERSION,
                dir.join("home/.local/bin/ccnm").display()
            )
        );
        assert_eq!(row(&report, "Agent SSH").detail, "me@workmac");
        assert_eq!(
            row(&report, "Agent ccnm").detail,
            format!("{} at /Users/me/.local/bin/ccnm", crate::VERSION)
        );
        assert_eq!(
            row(&report, "Claude authentication").detail,
            "me@x via claude.ai (max)"
        );
        assert_eq!(
            row(&report, "Reverse SSH").detail,
            format!("runtime as ccrun, ccnm {}", crate::VERSION)
        );
        assert_eq!(
            row(&report, "Terminal session").detail,
            "tmux 3.7c, ccnm-xshun  xshun  detached  tools connected  (Background, keychain reachable)"
        );
        // The two rows no probe can prove: they read the same however
        // healthy the machines are, so they are noted and do not block (P73).
        // Before P73 this report was NOT READY (0 failed, 3 not checked) and
        // exited 3, which is what every real configuration printed; the
        // third was the Codex exec-server row, removed with the chain (P86).
        assert!(
            text.ends_with("READY (2 not checked by design; each NOTE row says why)\n"),
            "{text}"
        );
        assert_eq!(report.blocking_code(), None);
        assert_eq!(report.exit_code(), 0);
        for name in ["Native tool policy", "Network isolation"] {
            assert_eq!(row(&report, name).status, Status::Note, "{name}: {text}");
        }
        assert!(
            !report.checks.iter().any(|c| c.status == Status::Skip),
            "{text}"
        );

        // Read-only: no control dir, nothing new in root.
        assert!(!control(&dir).exists());
        assert_eq!(std::fs::read_dir(dir.join("root")).unwrap().count(), 0);
        // The version probe ran the expanded ~ path, ssh -G, then one
        // probe with ControlMaster=no carrying the workspace facts. Three
        // commands and no more: the runtime audit is passed in, not run
        // here, so doctor's command list is still only its own.
        let calls = fake.calls();
        assert_eq!(
            calls.len(),
            3,
            "{:?}",
            calls.iter().map(Cmd::display).collect::<Vec<_>>()
        );
        assert_eq!(
            calls[0].display(),
            format!("{} --version", dir.join("home/.local/bin/ccnm").display())
        );
        assert_eq!(
            calls[1].display(),
            "ssh -o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 -o ForwardAgent=no -o ClearAllForwardings=yes -G work"
        );
        let probe = calls[2].display();
        assert!(probe.contains("ControlMaster=no"), "{probe}");
        assert!(
            probe.contains("-T work ~/.local/bin/ccnm internal probe --payload "),
            "{probe}"
        );
        let wire = calls[2].args.last().unwrap().to_string_lossy().into_owned();
        let sent: ProbeRequest = crate::protocol::payload::decode(&wire).unwrap();
        assert_eq!(sent.workspace, "xshun");
        assert_eq!(sent.root, dir.join("root"));
        assert_eq!(sent.runtime_node, "runtime");
        assert_eq!(sent.mcp_calls, 1);
        assert_eq!(calls[2].timeout, Duration::from_secs(90));
    }

    #[test]
    fn mcp_handshake_from_more_than_one_process_is_a_failure() {
        let (dir, config) = setup("mcp-pid", true, true);
        let mut probe = good_probe();
        if let Some(Ok(m)) = probe.mcp.as_mut() {
            m.single_process = false;
            m.calls = 3;
        }
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let mcp = row(&report, "Remote MCP handshake");
        assert_eq!(mcp.status, Status::Fail(ErrorCode::Internal));
        assert!(
            mcp.detail.contains("not one persistent process"),
            "{}",
            mcp.detail
        );
    }

    #[test]
    fn unreachable_work_fails_once_and_skips_the_rest() {
        let (dir, config) = setup("unreachable", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let mut down = Output::exited(255, "");
        down.stderr = b"ssh: connect to host workmac port 22: Operation timed out\n".to_vec();
        fake.push(down);

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert!(
            row(&report, "Agent SSH")
                .detail
                .contains("Operation timed out")
        );
        assert_eq!(row(&report, "Claude Code").status, Status::Skip);
        assert_eq!(row(&report, "Workspace root").status, Status::Skip);
        let text = report.render();
        // 10: the two rows no probe can prove are NOTE since P73, and the
        // Codex exec-server row went with the chain (P86); everything
        // behind the failed ssh is still an unknown.
        assert!(
            text.ends_with("NOT READY (1 failed, 10 not checked)\n"),
            "{text}"
        );
    }

    #[test]
    fn agent_version_mismatch_logged_out_and_missing_root_are_named() {
        let (dir, config) = setup("mismatch", true, true);
        let mut probe = good_probe();
        probe.hello = hello("me", "0.0.1", None);
        probe.agent.auth = Ok(crate::provider::AuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            subscription_type: None,
        });
        probe.runtime_hello = Some(Ok(hello("ccrun", crate::VERSION, Some(false))));
        // The root row is the Runtime Executor's answer now, not the
        // hello's: it is about whether *that* account can use the project.
        probe.runtime_audit = Some(Ok(crate::runtime::AuditReport {
            root: crate::runtime::RootStatus {
                present: false,
                is_dir: false,
                owned: false,
                git: crate::runtime::GitStatus::Unknown,
            },
            ..confined_report()
        }));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let work = row(&report, "Agent ccnm");
        assert_eq!(work.status, Status::Fail(ErrorCode::Version));
        // Named the way the config names it (F26), not `work`.
        assert!(
            work.detail.contains("the Agent Node agent runs ccnm 0.0.1"),
            "{}",
            work.detail
        );
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Fail(ErrorCode::Auth));
        assert!(auth.detail.contains("claude auth login"), "{}", auth.detail);
        // The answer came from the login session, so the row says so
        // instead of hedging about the Keychain.
        assert!(auth.detail.contains("login session"), "{}", auth.detail);
        let root = row(&report, "Workspace root");
        assert_eq!(root.status, Status::Fail(ErrorCode::WrongWorkspace));
        assert!(
            root.detail.contains("is missing for ccrun on runtime"),
            "{}",
            root.detail
        );
        // First FAIL in table order decides.
        assert_eq!(report.exit_code(), 11);
    }

    /// F2. A build cut from main and the release it grew out of both call
    /// themselves 0.9.0, so on the P62 machines doctor passed a pair whose
    /// first session was then refused by the internal protocol. The number
    /// alone cannot tell them apart; what each side says it can speak does.
    #[test]
    fn the_same_version_number_from_a_different_build_is_named_not_passed() {
        let (dir, config) = setup("same-number", true, true);
        let answer = |probe: serde_json::Value| {
            let fake = FakeRunner::new();
            fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
            fake.push(Output::exited(0, "hostname workmac\n"));
            fake.push(Output::exited(0, probe.to_string()));
            run(&config, Some("xshun"), &env(&fake, &dir))
        };

        // What an Agent built before P64 answers: the right number, and
        // nothing about which internal protocols it speaks.
        let mut probe = serde_json::to_value(good_probe()).unwrap();
        probe["hello"].as_object_mut().unwrap().remove("wire");
        let report = answer(probe);
        let agent = row(&report, "Agent ccnm");
        assert_eq!(agent.status, Status::Fail(ErrorCode::Version));
        assert!(
            agent.detail.contains("not the same build"),
            "{}",
            agent.detail
        );
        assert_eq!(report.exit_code(), 11);

        // The Runtime Node's answer is held to the same thing, and says
        // which level it stopped at.
        let mut probe = serde_json::to_value(good_probe()).unwrap();
        probe["runtime_hello"]["Ok"]["wire"] = serde_json::json!(6);
        let report = answer(probe);
        assert_eq!(row(&report, "Agent ccnm").status, Status::Ok);
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Fail(ErrorCode::Version));
        assert!(
            reverse.detail.contains("not the same build") && reverse.detail.contains("up to 6"),
            "{}",
            reverse.detail
        );
    }

    /// No controller means nobody could ask Claude a question worth
    /// trusting. That has to read as "not checked", never as "logged out":
    /// the second sends someone to log in on a machine that already is.
    #[test]
    fn without_a_controller_the_login_is_unchecked_not_failed() {
        let (dir, config) = setup("no-controller", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Err(ErrorReport::new(
            ErrorCode::NotReady,
            "no socket at /Users/me/.local/state/ccnm/controller.sock\ninstall it on the Agent Node: ccnm controller install",
        )));
        probe.agent.auth = Err(ErrorReport::new(
            ErrorCode::NotReady,
            "not checked: no controller to ask, and this ssh session's answer would be wrong",
        ));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let controller = row(&report, "Controller");
        assert_eq!(controller.status, Status::Skip);
        assert!(
            controller.detail.contains("controller install"),
            "{}",
            controller.detail
        );
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Skip, "{}", auth.detail);
        // Claude itself was still found: the version needs no credential.
        assert_eq!(row(&report, "Claude Code").status, Status::Ok);
        // Unverified, so still not READY -- and not for an auth reason.
        assert_eq!(report.exit_code(), ErrorCode::NotReady.exit_code());
    }

    /// A controller in the wrong session answers everything and is still
    /// useless. Nothing else in the table would catch that.
    #[test]
    fn a_controller_outside_the_login_session_fails_its_row() {
        let (dir, config) = setup("bg-controller", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let controller = row(&report, "Controller");
        assert_eq!(controller.status, Status::Fail(ErrorCode::NotReady));
        assert!(
            controller.detail.contains("not from a login session"),
            "{}",
            controller.detail
        );
        assert!(
            controller.detail.contains("Background"),
            "{}",
            controller.detail
        );
    }

    /// Caught on the real Agent Node: with a controller in the wrong
    /// session, the auth row still claimed to have asked the login session
    /// and failed on the answer. That is the same false negative the
    /// controller exists to remove, told with more confidence.
    #[test]
    fn a_logged_out_answer_from_the_wrong_session_is_not_a_verdict() {
        let (dir, config) = setup("bg-auth", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));
        probe.agent.auth = Ok(crate::provider::AuthStatus {
            logged_in: false,
            auth_method: None,
            email: None,
            subscription_type: None,
        });

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let auth = row(&report, "Claude authentication");
        assert_eq!(auth.status, Status::Skip, "{}", auth.detail);
        assert!(auth.detail.contains("means nothing"), "{}", auth.detail);
        assert!(
            !auth.detail.contains("claude auth login"),
            "must not send the user to log in on this evidence: {}",
            auth.detail
        );
        // The controller's own row is where the fix is.
        assert_eq!(
            row(&report, "Controller").status,
            Status::Fail(ErrorCode::NotReady)
        );
    }

    /// The asymmetry: a session that could not reach the credentials could
    /// not have found a login to report, so a positive answer is trusted
    /// wherever it came from.
    #[test]
    fn a_logged_in_answer_is_trusted_from_any_session() {
        let (dir, config) = setup("bg-auth-ok", true, true);
        let mut probe = good_probe();
        probe.controller = Some(Ok(controller("Background")));

        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(row(&report, "Claude authentication").status, Status::Ok);
    }

    #[test]
    fn reverse_ssh_failure_is_reported_from_the_probe() {
        let (dir, config) = setup("reverse", true, true);
        let mut probe = good_probe();
        probe.runtime_hello = Some(Err(ErrorReport::new(
            ErrorCode::RuntimeUnreachable,
            "ssh runtime-alias: Permission denied (publickey)",
        )));
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(0, serde_json::to_string(&probe).unwrap()));

        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let reverse = row(&report, "Reverse SSH");
        assert_eq!(reverse.status, Status::Fail(ErrorCode::RuntimeUnreachable));
        assert!(
            reverse.detail.contains("Permission denied"),
            "{}",
            reverse.detail
        );
        assert_eq!(row(&report, "Remote MCP handshake").status, Status::Skip);
        assert_eq!(row(&report, "Workspace root").status, Status::Skip);
        assert_eq!(report.exit_code(), 21);
    }

    #[test]
    fn missing_home_ccnm_names_the_path_and_the_fix() {
        let (dir, config) = setup("no-bin", true, false);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "hostname workmac\n"));
        fake.push(Output::exited(
            0,
            serde_json::to_string(&good_probe()).unwrap(),
        ));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let bin = row(&report, "Runtime ccnm");
        assert_eq!(bin.status, Status::Fail(ErrorCode::Version));
        assert!(bin.detail.contains("~/.local/bin/ccnm"), "{}", bin.detail);
        assert!(bin.detail.contains("cp $(which ccnm)"), "{}", bin.detail);
        assert!(
            bin.detail.contains("nodes.runtime.ccnm_bin"),
            "{}",
            bin.detail
        );
        assert_eq!(report.exit_code(), 11);
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "no --version for a missing file");
        assert!(
            calls[0]
                .display()
                .starts_with("ssh -o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 -o ForwardAgent=no -o ClearAllForwardings=yes -G")
        );
    }

    #[test]
    fn stale_home_ccnm_is_a_version_failure() {
        let (dir, config) = setup("stale-bin", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, "ccnm 0.0.9\n"));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        let bin = row(&report, "Runtime ccnm");
        assert_eq!(bin.status, Status::Fail(ErrorCode::Version));
        assert!(bin.detail.contains("is ccnm 0.0.9"), "{}", bin.detail);
    }

    #[test]
    fn missing_root_is_wrong_workspace() {
        let (dir, config) = setup("no-root", false, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        fake.push(Output::exited(0, "hostname workmac\n"));
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.blocking_code(), Some(ErrorCode::WrongWorkspace));
        assert!(
            row(&report, "Runtime workspace")
                .detail
                .contains("does not exist on this machine")
        );
    }

    #[test]
    fn unresolvable_work_alias_is_work_unreachable() {
        let (dir, config) = setup("bad-alias", true, true);
        let fake = FakeRunner::new();
        fake.push(Output::exited(0, format!("ccnm {}\n", crate::VERSION)));
        let mut failed = Output::exited(255, "");
        failed.stderr = b"work: Name or service not known\n".to_vec();
        fake.push(failed);
        let report = run(&config, Some("xshun"), &env(&fake, &dir));
        assert_eq!(report.exit_code(), 20, "{}", report.render());
        assert!(
            row(&report, "Agent SSH")
                .detail
                .contains("Name or service not known")
        );
        assert_eq!(row(&report, "Reverse SSH").status, Status::Skip);
        assert_eq!(fake.calls().len(), 2, "no probe after ssh -G failed");
    }

    #[test]
    fn multi_line_detail_is_indented_under_the_detail_column() {
        let report = Report {
            subject: "x".into(),
            checks: vec![Check::fail("Config", &Error::config("line one\nline two"))],
        };
        let text = report.render();
        assert!(
            text.contains("Config                  FAIL   CCNM_E_CONFIG: line one\n"),
            "{text}"
        );
        assert!(
            text.contains("\n                               line two\n"),
            "{text}"
        );
    }

    fn report_of(statuses: &[Status]) -> Report {
        Report {
            subject: "x".into(),
            checks: statuses
                .iter()
                .map(|s| Check {
                    name: "row",
                    status: s.clone(),
                    detail: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn verdict_fail_beats_skip_whatever_the_order() {
        let report = report_of(&[Status::Skip, Status::Fail(ErrorCode::Mount), Status::Skip]);
        assert_eq!(report.blocking_code(), Some(ErrorCode::Mount));
        assert_eq!(report.exit_code(), 22);
        assert!(!report.ready());
        // The first FAIL decides when there are several.
        let report = report_of(&[
            Status::Fail(ErrorCode::Auth),
            Status::Fail(ErrorCode::Mount),
        ]);
        assert_eq!(report.exit_code(), 12);
    }

    #[test]
    fn verdict_skip_only_is_not_ready_3() {
        let report = report_of(&[Status::Ok, Status::Warn, Status::Skip]);
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);
        assert!(!report.ready());
        assert!(
            report
                .render()
                .ends_with("NOT READY (0 failed, 1 not checked)\n")
        );
    }

    #[test]
    fn verdict_warn_only_is_ready_0() {
        let report = report_of(&[Status::Ok, Status::Warn, Status::Warn]);
        assert_eq!(report.blocking_code(), None);
        assert_eq!(report.exit_code(), 0);
        assert!(report.ready());
        assert!(report.render().ends_with("\nREADY\n"));
    }

    /// P74: a Linux controller is OK as it answers; with linger off it is a
    /// WARN that says what logging out does and what to type.
    #[test]
    fn a_linux_controller_without_linger_is_a_warning_that_names_the_fix() {
        let ctx = |linger: Option<bool>| crate::controller::Context {
            hello: crate::protocol::hello::HelloReport {
                protocol: 1,
                ccnm_version: crate::VERSION.into(),
                user: "bing".into(),
                platform: "linux/x86_64".into(),
                exe: None,
                root: None,
                wire: None,
            },
            pid: 7,
            manager: Ok(crate::controller::SYSTEMD_SERVICE.into()),
            platform: Some(crate::controller::Host::Linux),
            linger,
        };
        let mut probe = good_probe();
        probe.controller = Some(Ok(ctx(Some(true))));
        let row = controller_row(&probe);
        assert_eq!(row.status, Status::Ok, "{row:?}");
        assert!(row.detail.contains("systemd user service"), "{row:?}");

        probe.controller = Some(Ok(ctx(Some(false))));
        let row = controller_row(&probe);
        assert_eq!(row.status, Status::Warn, "{row:?}");
        assert!(
            row.detail.contains("sudo loginctl enable-linger bing"),
            "{row:?}"
        );

        // Linger unknown (no logind): nothing to warn about.
        probe.controller = Some(Ok(ctx(None)));
        assert_eq!(controller_row(&probe).status, Status::Ok);
    }

    /// P73: a NOTE is "not checked, and could not have been" -- it does not
    /// block, but READY says how many there are so it never hides them.
    #[test]
    fn verdict_notes_are_ready_0_and_counted() {
        let report = report_of(&[Status::Ok, Status::Note, Status::Warn, Status::Note]);
        assert_eq!(report.blocking_code(), None);
        assert_eq!(report.exit_code(), 0);
        assert!(report.ready());
        assert!(
            report
                .render()
                .ends_with("\nREADY (2 not checked by design; each NOTE row says why)\n"),
            "{}",
            report.render()
        );
        let zh = report.render_in(Lang::Zh);
        assert!(
            zh.ends_with("\n可以用了（2 项不查，原因写在标“不查”的行里）\n"),
            "{zh}"
        );
        assert!(zh.contains("不查"), "{zh}");
    }

    /// A NOTE never covers for an unknown: one SKIP is still NOT READY, and
    /// the count is of SKIPs only.
    #[test]
    fn verdict_a_note_does_not_hide_a_skip() {
        let report = report_of(&[Status::Note, Status::Skip, Status::Note]);
        assert_eq!(report.blocking_code(), Some(ErrorCode::NotReady));
        assert_eq!(report.exit_code(), 3);
        assert!(
            report
                .render()
                .ends_with("NOT READY (0 failed, 1 not checked)\n")
        );
        assert!(
            report
                .render_in(Lang::Zh)
                .ends_with("还不能用（0 项失败，1 项没查）\n")
        );
    }

    /// The project in a home the Operator may not enter (F1): a NOTE when
    /// the Runtime Executor's answer is a row of the same report, a SKIP
    /// when nothing in the report answers it.
    #[test]
    fn a_root_this_account_may_not_look_at_is_noted_only_when_answered_elsewhere() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("ccnm-doctor-{}-hidden-root", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let parent = dir.join("executor-home");
        let root = parent.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let tidy = TestDir::adopt(dir.clone());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let hidden = matches!(paths::see_dir(&root), paths::Seen::Hidden);
        let answered =
            runtime_workspace(&root, "its answer is the `Workspace root` row below", true);
        let unanswered = runtime_workspace(&root, "it answers through `ccnm mcp bridge`", false);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(tidy);
        if !hidden {
            eprintln!("skipped: this account is not refused by directory permissions");
            return;
        }
        assert_eq!(answered.status, Status::Note, "{answered:?}");
        assert!(
            answered.detail.contains("is not allowed to look"),
            "{answered:?}"
        );
        assert_eq!(unanswered.status, Status::Skip, "{unanswered:?}");
    }

    #[test]
    fn verdict_ok_only_is_ready_0() {
        let report = report_of(&[Status::Ok, Status::Ok]);
        assert_eq!(report.exit_code(), 0);
        assert!(report.ready());
        // And an empty report has nothing blocking either.
        assert!(report_of(&[]).ready());
    }
}

//! `ccnm ls`, `ccnm status` without a workspace, and `ccnm log`: every
//! managed project at once.
//!
//! Three sources, each saying only what it can see:
//! - the Agent Node's tmux sessions (one `agent-status` per Agent Node),
//! - this machine's `ccnm internal mcp-serve` processes, from `ps`,
//! - this machine's write-guard markers.
//!
//! Putting them side by side is the point. On the morning this was
//! written each one looked fine alone: the Agent had a gld session, the
//! Runtime had a gld server, the guard said held. Only together did they
//! show the server belonged to a session the Agent had finished hours
//! earlier, and that it was why the new one had no tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::config::Config;
use crate::lang::{Lang, pad};
use crate::launcher::{self, Env};
use crate::process::{Cmd, ProcessRunner};
use crate::protocol::run::{HistoryEntry, LiveSession, SessionState};

/// One `ccnm internal mcp-serve` running on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub pid: u32,
    pub ppid: u32,
    /// Whether its parent is an sshd session, i.e. whether ending that
    /// parent hands the server a clean EOF.
    pub parent_is_sshd: bool,
    pub elapsed_secs: u64,
    pub workspace: String,
    pub session: String,
}

/// What a server is doing, as far as the Agent Node can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Serves the session the Agent's tmux is running now.
    Live,
    /// The Agent has this session on record and it has not finished: a
    /// `--print` run, which has no tmux session.
    Running,
    /// The Agent says this session is over. Nothing is served to anyone.
    Ended(SessionState),
    /// `ccnm doctor` or `ccnm mcp probe`; gone in seconds.
    Probe,
    /// An external MCP client through `ccnm mcp bridge`.
    External,
    /// The Agent could not be asked, or had no answer.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerView {
    pub server: Server,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceView {
    pub name: String,
    pub root: Option<PathBuf>,
    pub root_present: bool,
    /// The Agent Node's ssh alias; `None` on the Agent Node itself.
    pub agent: Option<String>,
    pub sessions: Vec<LiveSession>,
    pub servers: Vec<ServerView>,
    /// The session a `held` marker names, when it names no running server.
    pub guard_left_held: Option<LeftGuard>,
}

/// A write guard whose session is gone but whose marker still says `held`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftGuard {
    pub session: String,
    /// What that session could not stop. Some = kept on purpose, see
    /// [`Guard::abandoned`] -- and the recovery is not the same, because
    /// something that can write is still out there.
    pub abandoned: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentView {
    pub alias: Option<String>,
    /// `tmux -V` there, or why the Agent could not be asked.
    pub tmux: std::result::Result<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub agents: Vec<AgentView>,
    pub workspaces: Vec<WorkspaceView>,
    pub now: u64,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every managed workspace, from the machine whose config lists them.
pub fn collect(config: &Config, env: &Env<'_>, state: &Path) -> Overview {
    let servers = scan_servers(env.runner);
    let guards = scan_guards(state);
    let mut agents: BTreeMap<String, std::result::Result<Vec<LiveSession>, String>> =
        BTreeMap::new();
    let mut tmux_versions = Vec::new();
    let mut workspaces = Vec::new();
    for name in config.workspaces.keys() {
        let Ok(resolved) = config.workspace(name) else {
            continue;
        };
        let alias = resolved.agent_ssh().ok().map(str::to_string);
        if let Some(alias) = &alias
            && !agents.contains_key(alias)
        {
            let report = launcher::status_selected(&resolved, env, true, None, None);
            tmux_versions.push(AgentView {
                alias: Some(alias.clone()),
                tmux: match &report {
                    Ok(rep) => rep.tmux.clone().map_err(|e| e.message),
                    Err(e) => Err(e.to_string()),
                },
            });
            agents.insert(
                alias.clone(),
                report.map(|rep| rep.sessions).map_err(|e| e.to_string()),
            );
        }
        let sessions: Vec<LiveSession> = alias
            .as_ref()
            .and_then(|a| agents.get(a))
            .and_then(|r| r.as_ref().ok())
            .map(|all| {
                all.iter()
                    .filter(|s| s.workspace.as_deref() == Some(name.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let agent_error = alias
            .as_ref()
            .and_then(|a| agents.get(a))
            .and_then(|r| r.as_ref().err().cloned());
        let mine: Vec<Server> = servers
            .iter()
            .filter(|s| s.workspace == *name)
            .cloned()
            .collect();
        let views = mine
            .into_iter()
            .map(|server| {
                let verdict = judge(&server, &sessions, agent_error.as_deref(), || {
                    launcher::status_selected(&resolved, env, false, None, Some(&server.session))
                        .map_err(|e| e.to_string())
                        .and_then(|rep| {
                            rep.records
                                .first()
                                .map(|r| r.state)
                                .ok_or_else(|| "no record".to_string())
                        })
                });
                ServerView { server, verdict }
            })
            .collect::<Vec<_>>();
        let guard_left_held = guards
            .iter()
            .find(|g| g.workspace.as_deref() == Some(name.as_str()))
            .filter(|g| !servers.iter().any(|s| s.session == g.session))
            .map(|g| LeftGuard {
                session: g.session.clone(),
                abandoned: g.abandoned.clone(),
            });
        workspaces.push(WorkspaceView {
            name: name.clone(),
            root: Some(resolved.workspace.root.clone()),
            root_present: resolved.workspace.root.is_dir(),
            agent: alias,
            sessions,
            servers: views,
            guard_left_held,
        });
    }
    Overview {
        agents: tmux_versions,
        workspaces,
        now: now_secs(),
    }
}

/// The Agent Node's own view: its live sessions, grouped by workspace.
/// There is no Runtime here, so no servers and no guards.
pub fn from_agent_status(report: &crate::protocol::run::StatusReport) -> Overview {
    let mut by_name: BTreeMap<String, Vec<LiveSession>> = BTreeMap::new();
    for session in &report.sessions {
        by_name
            .entry(session.workspace.clone().unwrap_or_else(|| "-".into()))
            .or_default()
            .push(session.clone());
    }
    Overview {
        agents: vec![AgentView {
            alias: None,
            tmux: report.tmux.clone().map_err(|e| e.message),
        }],
        workspaces: by_name
            .into_iter()
            .map(|(name, sessions)| WorkspaceView {
                name,
                root: None,
                root_present: true,
                agent: None,
                sessions,
                servers: Vec::new(),
                guard_left_held: None,
            })
            .collect(),
        now: now_secs(),
    }
}

/// Decide what a server is doing. `lookup` asks the Agent about one
/// session, and is only called when the live sessions do not answer it.
fn judge(
    server: &Server,
    live: &[LiveSession],
    agent_error: Option<&str>,
    lookup: impl FnOnce() -> std::result::Result<SessionState, String>,
) -> Verdict {
    if server.session.starts_with("probe-") {
        return Verdict::Probe;
    }
    if server.session.starts_with("bridge-") {
        return Verdict::External;
    }
    if live
        .iter()
        .any(|s| s.session.as_deref() == Some(server.session.as_str()))
    {
        return Verdict::Live;
    }
    if let Some(error) = agent_error {
        return Verdict::Unknown(error.to_string());
    }
    match lookup() {
        Ok(state @ (SessionState::Completed | SessionState::Failed)) => Verdict::Ended(state),
        Ok(SessionState::Running | SessionState::Starting | SessionState::Stopping) => {
            Verdict::Running
        }
        Ok(SessionState::Unknown) => Verdict::Unknown("state unknown".into()),
        Err(error) => Verdict::Unknown(error),
    }
}

/// `ps` for every `ccnm internal mcp-serve`, whoever runs it.
pub fn scan_servers(runner: &dyn ProcessRunner) -> Vec<Server> {
    try_scan_servers(runner).unwrap_or_default()
}

/// [`scan_servers`], saying `None` when `ps` could not be asked. Anything
/// that removes a session's files on the strength of "nobody serves it"
/// needs this one: an empty list from a failed `ps` is not an answer.
pub fn try_scan_servers(runner: &dyn ProcessRunner) -> Option<Vec<Server>> {
    let cmd = Cmd::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,etime=,command="])
        .timeout(std::time::Duration::from_secs(10));
    match runner.run(&cmd) {
        Ok(out) if out.success() => Some(parse_servers(&out.stdout_lossy())),
        _ => None,
    }
}

fn parse_servers(ps: &str) -> Vec<Server> {
    let rows: Vec<(u32, u32, &str, &str)> = ps
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut field = || {
                let end = rest.find(char::is_whitespace)?;
                let (head, tail) = rest.split_at(end);
                rest = tail.trim_start();
                Some(head)
            };
            let pid = field()?.parse().ok()?;
            let ppid = field()?.parse().ok()?;
            let etime = field()?;
            Some((pid, ppid, etime, rest))
        })
        .collect();
    let commands: BTreeMap<u32, &str> = rows.iter().map(|r| (r.0, r.3)).collect();
    rows.iter()
        .filter_map(|&(pid, ppid, etime, command)| {
            let words: Vec<&str> = command.split_whitespace().collect();
            let at = words
                .windows(2)
                .position(|w| w == ["internal", "mcp-serve"])?;
            let payload = words[at..]
                .windows(2)
                .find(|w| w[0] == "--payload")
                .map(|w| w[1])?;
            let (workspace, session) = payload_names(payload)?;
            Some(Server {
                pid,
                ppid,
                parent_is_sshd: commands
                    .get(&ppid)
                    .is_some_and(|c| c.trim_start().starts_with("sshd")),
                elapsed_secs: parse_etime(etime)?,
                workspace,
                session,
            })
        })
        .collect()
}

/// The workspace and session a serve payload names, whatever its protocol
/// version: this reads other binaries' payloads, older ones included.
fn payload_names(payload: &str) -> Option<(String, String)> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let text = |key: &str| value.get(key)?.as_str().map(str::to_string);
    Some((text("workspace")?, text("session")?))
}

/// `ps`'s `etime`: `[[dd-]hh:]mm:ss`.
fn parse_etime(text: &str) -> Option<u64> {
    let (days, clock) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(days * 86400 + secs)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub session: String,
    pub workspace: Option<String>,
    /// What that session could not stop when it ended. Some = the guard was
    /// **kept on purpose**, not left behind by a crash: something that can
    /// still write the tree is out there, and clearing the marker would put
    /// a second writer beside it (P43).
    pub abandoned: Option<String>,
}

/// Every `held` marker in this machine's write-guard directory.
pub fn scan_guards(state: &Path) -> Vec<Guard> {
    let Ok(entries) = std::fs::read_dir(state.join("write-guards")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| parse_guard(&text))
        .collect()
}

fn parse_guard(text: &str) -> Option<Guard> {
    let mut words = text
        .lines()
        .next()?
        .strip_prefix("held ")?
        .split_whitespace();
    Some(Guard {
        session: words.next()?.to_string(),
        workspace: words.next().map(str::to_string),
        abandoned: text
            .lines()
            .skip(1)
            .find_map(|line| line.strip_prefix("abandoned "))
            .map(str::to_string),
    })
}

/// `ccnm status <workspace>`'s write-guard lines (P60): what the Runtime
/// Executor said about its own guard, or that it could not be asked.
///
/// Never a guess in either direction. "Could not ask" is said as such and
/// is not free; "free" says it is an observation, because the next session
/// still has to take the guard itself and someone may take it first.
pub fn render_guard(
    answer: &crate::Result<crate::protocol::run::AgentGuardReport>,
    offset_secs: i64,
    lang: Lang,
) -> String {
    use crate::mcp::write_guard::{Observed, Process, Reason};
    use crate::protocol::run::OwnerOnAgent;
    let report = match answer {
        Ok(report) => report,
        Err(e) => {
            return lang.pick(
                format!("写锁  问不到 Runtime（{e}）——这不等于空闲\n"),
                format!("guard  could not ask the Runtime ({e}); that does not mean free\n"),
            );
        }
    };
    let seen = &report.runtime.observation;
    let at = clock(seen.observed_at, offset_secs);
    let marker = format!("write-guards/{}.lock", seen.resource.id);
    let owner = seen
        .owner
        .as_ref()
        .map_or("-", |owner| short(&owner.session));
    let pid = seen
        .owner
        .as_ref()
        .and_then(|owner| Some((owner.pid?, owner.process?)))
        .map(|(pid, process)| match process {
            Process::Ccnm => lang.pick(
                format!("pid {pid} 还是 ccnm 进程"),
                format!("pid {pid} is still a ccnm process"),
            ),
            Process::Other => lang.pick(
                format!("pid {pid} 现在是别的程序（号被复用了）"),
                format!("pid {pid} is something else now (reused)"),
            ),
            Process::Gone => lang.pick(
                format!("pid {pid} 已经没了——这不等于它起的命令也没了"),
                format!("pid {pid} is gone -- which does not mean its children are"),
            ),
            Process::Unchecked => lang.pick(
                format!("pid {pid} 查不了（ps 跑不了）"),
                format!("pid {pid} could not be checked (no ps)"),
            ),
        });
    let legacy = seen.owner.as_ref().is_some_and(|owner| owner.legacy);
    let on_agent = match report.owner_on_agent {
        Some(OwnerOnAgent::State(state @ (SessionState::Completed | SessionState::Failed))) => {
            Some(lang.pick(
                format!(
                    "Agent 那边这个会话已经{}——占锁的多半是它留下的孤儿 mcp-serve",
                    state_word(state, lang)
                ),
                format!(
                    "the Agent says this session {} -- likely an orphan mcp-serve",
                    state_word(state, lang)
                ),
            ))
        }
        Some(OwnerOnAgent::State(state)) => Some(lang.pick(
            format!("Agent 那边这个会话：{}", state_word(state, lang)),
            format!("on the Agent: {}", state_word(state, lang)),
        )),
        Some(OwnerOnAgent::NotHere) => Some(lang.pick(
            "这台 Agent 没有这个会话的记录".to_string(),
            "this Agent has no record of it".to_string(),
        )),
        None => None,
    };
    let facts: Vec<String> = pid.into_iter().chain(on_agent).collect();
    let facts = if facts.is_empty() {
        String::new()
    } else {
        lang.pick(
            format!("\n       {}", facts.join("；")),
            format!("\n       {}", facts.join("; ")),
        )
    };
    let recover = lang.pick(
        "恢复见 docs/operations.md「写入 guard 残留」",
        "recovery: docs/operations.md, \"write guard left held\"",
    );
    let line = match (seen.state, seen.reason) {
        (Observed::Free, _) => lang.pick(
            format!("写锁  空闲（Runtime {at} 看的；不是预留，下一个会话仍要自己去拿）"),
            format!("guard  free (seen by the Runtime at {at}; not a reservation)"),
        ),
        (Observed::Held, _) if seen.owner.is_none() => lang.pick(
            format!("写锁  被占：有进程正持有，还没写上是谁（{at}）"),
            format!("guard  held by a process that has not written its name yet ({at})"),
        ),
        (Observed::Held, _) => lang.pick(
            format!("写锁  被占：会话 {owner} 正持有（{at}）{facts}"),
            format!("guard  held by session {owner} ({at}){facts}"),
        ),
        (Observed::Abandoned, _) => lang.pick(
            format!(
                "写锁  故意留着：会话 {owner} 结束时有 {} 停不掉，可能还在改这棵树；先收掉它们，{recover}{facts}",
                seen.leftovers.as_deref().unwrap_or("-")
            ),
            format!(
                "guard  kept on purpose: session {owner} ended with {} it could not stop; end those first, {recover}{facts}",
                seen.leftovers.as_deref().unwrap_or("-")
            ),
        ),
        (Observed::Unknown, Reason::LeftHeld) => lang.pick(
            format!(
                "写锁  说不清：标记说 {owner} 占着，但没有进程持锁（异常退出留下的{}），它起的命令可能还活着；新会话会被拒，{recover}{facts}",
                if legacy { "，旧格式标记，没记 pid" } else { "" }
            ),
            format!(
                "guard  unknown: the marker says {owner} holds it and no process does (interrupted{}); what it started may still run, new sessions are refused, {recover}{facts}",
                if legacy { ", old marker without a pid" } else { "" }
            ),
        ),
        (Observed::Unknown, Reason::Unreadable) => lang.pick(
            "写锁  说不清：Runtime 执行账号读不了自己的 write-guards/".to_string(),
            "guard  unknown: the Runtime Executor cannot read its own write-guards/".to_string(),
        ),
        (Observed::Unknown, Reason::LockQueryFailed) => lang.pick(
            "写锁  说不清：锁本身问不了".to_string(),
            "guard  unknown: the lock itself could not be asked".to_string(),
        ),
        (Observed::Unknown, _) => lang.pick(
            format!("写锁  说不清：标记内容不完整或不认识；新会话会被拒，{recover}"),
            format!(
                "guard  unknown: the marker is incomplete or not ours; new sessions are refused, {recover}"
            ),
        ),
    };
    let file = match seen.state {
        Observed::Free => String::new(),
        _ => lang.pick(
            format!("\n       标记文件：Runtime 执行账号的 {marker}"),
            format!("\n       marker: the Runtime Executor's {marker}"),
        ),
    };
    format!("{line}{file}\n")
}

/// How long, the way a person says it.
pub fn span(secs: u64, lang: Lang) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h, m) {
        (0, 0, 0) => lang.pick("不到 1 分钟".into(), "<1m".into()),
        (0, 0, m) => lang.pick(format!("{m} 分钟"), format!("{m}m")),
        (0, h, m) => lang.pick(format!("{h} 小时 {m} 分"), format!("{h}h {m}m")),
        (d, h, _) => lang.pick(format!("{d} 天 {h} 小时"), format!("{d}d {h}h")),
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn state_word(state: SessionState, lang: Lang) -> &'static str {
    match state {
        SessionState::Starting => lang.pick("启动中", "starting"),
        SessionState::Running => lang.pick("运行中", "running"),
        SessionState::Completed => lang.pick("正常结束", "completed"),
        SessionState::Failed => lang.pick("出错结束", "failed"),
        SessionState::Stopping => lang.pick("正在停", "stopping"),
        SessionState::Unknown => lang.pick("没有结束记录", "no end record"),
    }
}

/// Lines padded by display width, so Chinese cells line up.
pub(crate) fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|c| crate::lang::display_width(c))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let last = row.len().saturating_sub(1);
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    c.clone()
                } else {
                    pad(c, widths[i])
                }
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

impl WorkspaceView {
    fn orphans(&self) -> impl Iterator<Item = &ServerView> {
        self.servers
            .iter()
            .filter(|v| matches!(v.verdict, Verdict::Ended(_)))
    }

    fn state_cell(&self, lang: Lang) -> String {
        if let Some(s) = self.sessions.first() {
            let terminals = match s.attached {
                0 => lang.pick("没有终端接着".to_string(), "detached".to_string()),
                n => lang.pick(format!("{n} 个终端"), format!("{n} attached")),
            };
            return format!("{} · {terminals}", lang.pick("运行中", "running"));
        }
        if self.servers.iter().any(|v| v.verdict == Verdict::Running) {
            return lang.pick("--print 运行中", "--print running").into();
        }
        lang.pick("没在跑", "idle").into()
    }

    fn tools_cell(&self, lang: Lang) -> &'static str {
        match self.sessions.first().map(|s| s.tools) {
            Some(Some(true)) => lang.pick("通", "connected"),
            Some(Some(false)) => lang.pick("断了", "DOWN"),
            Some(None) => lang.pick("说不清", "unknown"),
            None => "",
        }
    }
}

impl Overview {
    /// `ccnm ls`: one line per workspace, problems underneath.
    pub fn render_list(&self, lang: Lang) -> String {
        if self.workspaces.is_empty() {
            return lang
                .pick("一个项目都没有在跑，配置里也没有\n", "no workspaces\n")
                .into();
        }
        let mut rows = vec![vec![
            lang.pick("项目", "WORKSPACE").to_string(),
            lang.pick("状态", "STATE").to_string(),
            lang.pick("已运行", "UP").to_string(),
            lang.pick("工具", "TOOLS").to_string(),
        ]];
        for ws in &self.workspaces {
            rows.push(vec![
                ws.name.clone(),
                ws.state_cell(lang),
                ws.sessions
                    .first()
                    .map(|s| span(self.now.saturating_sub(s.created), lang))
                    .unwrap_or_default(),
                ws.tools_cell(lang).to_string(),
            ]);
        }
        let mut out = table(&rows);
        let notes = self.notes(lang);
        if !notes.is_empty() {
            out.push('\n');
            for note in notes {
                out.push_str(&format!("! {note}\n"));
            }
            out.push_str(lang.pick("详情：ccnm status\n", "details: ccnm status\n"));
        }
        out
    }

    /// One sentence per thing that needs a person.
    fn notes(&self, lang: Lang) -> Vec<String> {
        let mut notes = Vec::new();
        for agent in &self.agents {
            if let Err(e) = &agent.tmux {
                let alias = agent.alias.as_deref().unwrap_or("Agent Node");
                notes.push(lang.pick(
                    format!("问不到 {alias} 上的会话：{e}"),
                    format!("cannot ask {alias} about its sessions: {e}"),
                ));
            }
        }
        for ws in &self.workspaces {
            let name = &ws.name;
            for orphan in ws.orphans() {
                let (pid, id) = (orphan.server.pid, short(&orphan.server.session));
                let up = span(orphan.server.elapsed_secs, lang);
                notes.push(lang.pick(
                    format!("{name}：Runtime 上还有旧会话 {id} 的 mcp-serve（pid {pid}，跑了 {up}），Agent 那边这个会话已经结束，它还占着写锁"),
                    format!("{name}: mcp-serve of old session {id} (pid {pid}, up {up}) still holds the write guard; the Agent says that session has ended"),
                ));
            }
            if let Some(held) = &ws.guard_left_held {
                let id = short(&held.session);
                notes.push(match &held.abandoned {
                    // 故意留下的：还有东西在跑，清掉 marker 就是放第二个写者
                    // 进同一棵树。措辞必须和"异常退出留下的"分开。
                    Some(what) => lang.pick(
                        format!("{name}：写锁是 {id} 结束时**故意留着**的——它有 {what} 停不掉，可能还在改这棵树；先把它们收掉再谈恢复"),
                        format!("{name}: the write guard was kept on purpose when {id} ended -- {what} it could not stop, possibly still writing this tree; end those before recovering"),
                    ),
                    None => lang.pick(
                        format!("{name}：写锁标着被 {id} 占着，但已经没有这个会话的 mcp-serve 了（异常退出留下的），新会话会被拒"),
                        format!("{name}: the write guard is left held by {id}, which has no mcp-serve any more; new sessions will be refused"),
                    ),
                });
            }
            if ws.sessions.first().is_some_and(|s| s.tools == Some(false))
                && ws.orphans().next().is_none()
            {
                notes.push(lang.pick(
                    format!("{name}：工具断了。在 Claude 里 /mcp → ccnm → Reconnect"),
                    format!("{name}: tools are down. In Claude: /mcp -> ccnm -> Reconnect"),
                ));
            }
        }
        notes
    }

    /// `ccnm status` with no workspace: everything known, per workspace.
    pub fn render_status(&self, lang: Lang) -> String {
        let mut out = String::new();
        for agent in &self.agents {
            let alias = agent.alias.as_deref().unwrap_or("");
            match &agent.tmux {
                Ok(v) => out.push_str(&lang.pick(
                    format!("Agent Node {alias}：tmux {v}\n"),
                    format!("Agent Node {alias}: tmux {v}\n"),
                )),
                Err(e) => out.push_str(&lang.pick(
                    format!("Agent Node {alias}：问不到（{e}）\n"),
                    format!("Agent Node {alias}: unreachable ({e})\n"),
                )),
            }
        }
        if self.workspaces.is_empty() {
            out.push_str(lang.pick("没有在跑的会话\n", "no live sessions\n"));
            return out;
        }
        // Busy ones first: that is what someone running this is looking for.
        let mut order: Vec<&WorkspaceView> = self.workspaces.iter().collect();
        order.sort_by_key(|ws| ws.sessions.is_empty() && ws.servers.is_empty());
        for ws in order {
            out.push('\n');
            out.push_str(&self.render_workspace(ws, lang));
        }
        out
    }

    fn render_workspace(&self, ws: &WorkspaceView, lang: Lang) -> String {
        let mut out = match &ws.root {
            Some(root) if !ws.root_present => lang.pick(
                format!("{}  {}（不在这台机器上）\n", ws.name, root.display()),
                format!("{}  {} (not on this machine)\n", ws.name, root.display()),
            ),
            Some(root) => format!("{}  {}\n", ws.name, root.display()),
            None => format!("{}\n", ws.name),
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for s in &ws.sessions {
            let up = span(self.now.saturating_sub(s.created), lang);
            let terminals = match s.attached {
                0 => lang.pick("没有终端接着".to_string(), "detached".to_string()),
                n => lang.pick(format!("{n} 个终端接着"), format!("{n} attached")),
            };
            rows.push(vec![
                lang.pick("会话", "session").into(),
                s.session.as_deref().map(short).unwrap_or("-").into(),
                lang.pick(
                    format!("运行中，已运行 {up}，{terminals}"),
                    format!("running for {up}, {terminals}"),
                ),
            ]);
            rows.push(vec![
                lang.pick("工具", "tools").into(),
                String::new(),
                match s.tools {
                    Some(true) => lang.pick("通", "connected").into(),
                    Some(false) => lang.pick(
                        "断了——在 Claude 里：/mcp → ccnm → Reconnect".to_string(),
                        s.provider.tools_down_hint().to_string(),
                    ),
                    None => lang.pick("说不清", "unknown").into(),
                },
            ]);
        }
        if ws.sessions.is_empty() {
            rows.push(vec![
                lang.pick("会话", "session").into(),
                String::new(),
                lang.pick("没在跑", "none").into(),
            ]);
        }
        for view in &ws.servers {
            let s = &view.server;
            let what = match &view.verdict {
                Verdict::Live => lang
                    .pick("服务着上面这个会话", "serving the session above")
                    .into(),
                Verdict::Running => lang.pick("--print 运行", "--print run").into(),
                Verdict::Probe => lang.pick("诊断用，几秒就退", "diagnostic probe").into(),
                Verdict::External => lang.pick("外部 MCP 客户端", "external MCP client").into(),
                Verdict::Ended(state) => lang.pick(
                    format!(
                        "孤儿：Agent 那边这个会话已经{}，它还占着写锁",
                        state_word(*state, lang)
                    ),
                    format!(
                        "ORPHAN: the Agent says this session {}, yet it holds the write guard",
                        state_word(*state, lang)
                    ),
                ),
                Verdict::Unknown(why) => lang.pick(
                    format!("跟 Agent 对不上（{why}）"),
                    format!("does not match the Agent ({why})"),
                ),
            };
            rows.push(vec![
                "Runtime".into(),
                short(&s.session).into(),
                lang.pick(
                    format!(
                        "mcp-serve pid {}，已跑 {}，{what}",
                        s.pid,
                        span(s.elapsed_secs, lang)
                    ),
                    format!(
                        "mcp-serve pid {}, up {}, {what}",
                        s.pid,
                        span(s.elapsed_secs, lang)
                    ),
                ),
            ]);
        }
        // Fixed columns rather than `table`: every workspace's block should
        // line up with the next one's. "Runtime" and an 8-character id are
        // the widest cells either column ever holds.
        for row in &rows {
            out.push_str(&format!(
                "  {} {} {}\n",
                pad(&row[0], 8),
                pad(&row[1], 8),
                row[2]
            ));
        }
        for orphan in ws.orphans() {
            let s = &orphan.server;
            if s.parent_is_sshd {
                out.push_str(&lang.pick(
                    format!("  → 新会话会被它挡住。结束它背后的 sshd 会话，mcp-serve 会正常收尾、释放锁：kill {}\n", s.ppid),
                    format!("  -> it blocks new sessions. End the sshd session behind it and mcp-serve exits cleanly: kill {}\n", s.ppid),
                ));
            } else {
                out.push_str(lang.pick(
                    "  → 新会话会被它挡住。恢复步骤见 docs/operations.md「写入 guard 残留」\n",
                    "  -> it blocks new sessions. Recovery: docs/operations.md, \"write guard left held\"\n",
                ));
            }
        }
        if let Some(held) = &ws.guard_left_held {
            let id = short(&held.session);
            out.push_str(&match &held.abandoned {
                Some(what) => lang.pick(
                    format!("  写锁  {id} 结束时故意留着的：有 {what} 停不掉，可能还在改这棵树。先收掉它们，再看 docs/operations.md「写入 guard 残留」\n"),
                    format!("  guard  kept on purpose when {id} ended: {what} it could not stop, possibly still writing. End those first; see docs/operations.md\n"),
                ),
                None => lang.pick(
                    format!("  写锁  标着被 {id} 占着，但没有对应的 mcp-serve：新会话会被拒，恢复见 docs/operations.md「写入 guard 残留」\n"),
                    format!("  guard  left held by {id} with no mcp-serve: new sessions are refused; see docs/operations.md\n"),
                ),
            });
        }
        out
    }
}

/// `ccnm log`: sessions, newest first.
///
/// `offset_secs` is this machine's UTC offset, so times read like the
/// clock on the wall; `ccnm` has no time-zone library and asks `date` once.
pub fn render_history(entries: &[HistoryEntry], now: u64, offset_secs: i64, lang: Lang) -> String {
    if entries.is_empty() {
        return lang
            .pick("还没有会话记录\n", "no sessions on record\n")
            .into();
    }
    let mut rows = vec![vec![
        lang.pick("开始", "STARTED").to_string(),
        lang.pick("时长", "DURATION").to_string(),
        lang.pick("项目", "WORKSPACE").to_string(),
        lang.pick("状态", "STATE").to_string(),
        lang.pick("会话", "SESSION").to_string(),
        lang.pick("开场", "OPENING").to_string(),
    ]];
    for e in entries {
        let duration = match (&e.outcome, e.state) {
            (Some(outcome), _) => span(outcome.duration_ms / 1000, lang),
            (None, SessionState::Running | SessionState::Starting | SessionState::Stopping) => {
                format!("{}+", span(now.saturating_sub(e.started), lang))
            }
            (None, _) => "-".into(),
        };
        let state = match &e.outcome {
            Some(o) if o.timed_out => lang.pick("超时被杀", "timed out").to_string(),
            // Before "failed to start": a stop is a session that did run.
            Some(o) if o.stopped => lang.pick("被停止", "stopped").to_string(),
            Some(o) if o.error.is_some() => lang.pick("没起来", "failed to start").to_string(),
            Some(o) if !o.ok() => match o.exit_code {
                Some(code) => lang.pick(format!("退出码 {code}"), format!("exit {code}")),
                None => lang.pick("被信号杀掉", "killed").to_string(),
            },
            _ => state_word(e.state, lang).to_string(),
        };
        let opening = match (e.mode.as_str(), &e.prompt) {
            ("print", Some(p)) => format!("--print {p}"),
            (_, Some(p)) => p.clone(),
            _ => String::new(),
        };
        rows.push(vec![
            clock(e.started, offset_secs),
            duration,
            e.workspace.clone(),
            state,
            short(&e.session).to_string(),
            opening,
        ]);
    }
    table(&rows)
}

/// `MM-DD HH:MM` for a Unix time at a fixed UTC offset.
pub fn clock(unix: u64, offset_secs: i64) -> String {
    let local = unix as i64 + offset_secs;
    let (days, secs) = (local.div_euclid(86400), local.rem_euclid(86400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!(
        "{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// This machine's UTC offset in seconds, from `date +%z` (`+0900`).
pub fn utc_offset(runner: &dyn ProcessRunner) -> i64 {
    let cmd = Cmd::new("date")
        .arg("+%z")
        .timeout(std::time::Duration::from_secs(5));
    runner
        .run(&cmd)
        .ok()
        .filter(|o| o.success())
        .and_then(|o| parse_offset(o.stdout_lossy().trim()))
        .unwrap_or(0)
}

fn parse_offset(text: &str) -> Option<i64> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(workspace: &str, session: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            r#"{{"protocol":1,"workspace":"{workspace}","root":"/r","session":"{session}"}}"#
        ))
    }

    fn live(workspace: &str, session: &str, tools: Option<bool>) -> LiveSession {
        LiveSession {
            provider: Default::default(),
            agent_identity: None,
            tmux_session: format!("ccnm-{workspace}"),
            workspace: Some(workspace.into()),
            session: Some(session.into()),
            created: 1_000,
            attached: 1,
            context: None,
            tools,
        }
    }

    #[test]
    fn ps_rows_become_servers_with_their_sshd_parent() {
        let ps = format!(
            "  28397 28263   12:04:10 sshd-session: bing@notty\n\
             28398 28397   12:04:10 /Users/bing/.local/bin/ccnm internal mcp-serve --payload {}\n\
             44978 44975      59:02 ccnm internal mcp-serve --payload {}\n\
             50000     1 1-02:00:00 vim internal mcp-serve notes.txt\n",
            payload("gld", "402638ca-old"),
            payload("xdo", "825d26af-new"),
        );
        let servers = parse_servers(&ps);
        assert_eq!(servers.len(), 2, "{servers:?}");
        assert_eq!(servers[0].workspace, "gld");
        assert_eq!(servers[0].session, "402638ca-old");
        assert_eq!(servers[0].elapsed_secs, 12 * 3600 + 4 * 60 + 10);
        assert!(servers[0].parent_is_sshd);
        assert!(
            !servers[1].parent_is_sshd,
            "parent 44975 is not in the list"
        );
    }

    #[test]
    fn etime_takes_all_three_shapes() {
        assert_eq!(parse_etime("00:05"), Some(5));
        assert_eq!(parse_etime("01:00:05"), Some(3605));
        assert_eq!(parse_etime("2-01:00:05"), Some(2 * 86400 + 3605));
        assert_eq!(parse_etime("garbage"), None);
    }

    #[test]
    fn guard_markers_name_session_and_workspace_and_released_is_not_one() {
        assert_eq!(
            parse_guard("held 402638ca gld\n"),
            Some(Guard {
                session: "402638ca".into(),
                workspace: Some("gld".into()),
                abandoned: None,
            })
        );
        // P43 起 marker 多了 pid，和一行「它结束时没停掉什么」。前者不该
        // 被当成 workspace，后者是"故意留着的"和"崩溃留下的"的分水岭。
        assert_eq!(
            parse_guard("held 402638ca gld pid 4242\nabandoned 2 command(s) (r-a, r-b)\n"),
            Some(Guard {
                session: "402638ca".into(),
                workspace: Some("gld".into()),
                abandoned: Some("2 command(s) (r-a, r-b)".into()),
            })
        );
        assert_eq!(parse_guard("released\n"), None);
    }

    /// The morning in the module docs: a server for a session the Agent
    /// finished is the one thing worth a person's attention.
    #[test]
    fn a_server_for_an_ended_session_is_an_orphan_and_a_live_one_is_not() {
        let server = |session: &str| Server {
            pid: 1,
            ppid: 2,
            parent_is_sshd: true,
            elapsed_secs: 0,
            workspace: "gld".into(),
            session: session.into(),
        };
        let sessions = [live("gld", "new", Some(false))];
        let never = || -> std::result::Result<SessionState, String> {
            panic!("a live session needs no lookup")
        };
        assert_eq!(judge(&server("new"), &sessions, None, never), Verdict::Live);
        assert_eq!(
            judge(&server("old"), &sessions, None, || Ok(
                SessionState::Completed
            )),
            Verdict::Ended(SessionState::Completed)
        );
        assert_eq!(
            judge(&server("print"), &sessions, None, || Ok(
                SessionState::Running
            )),
            Verdict::Running
        );
        assert!(matches!(
            judge(&server("old"), &sessions, Some("ssh: timeout"), never),
            Verdict::Unknown(_)
        ));
        assert_eq!(judge(&server("probe-x"), &[], None, never), Verdict::Probe);
    }

    #[test]
    fn the_list_says_what_to_do_about_an_orphan() {
        let overview = Overview {
            agents: vec![AgentView {
                alias: Some("fodelf".into()),
                tmux: Ok("3.7c".into()),
            }],
            workspaces: vec![
                WorkspaceView {
                    name: "gld".into(),
                    root: Some("/p/gld".into()),
                    root_present: true,
                    agent: Some("fodelf".into()),
                    sessions: vec![live("gld", "95d6516e-new", Some(false))],
                    servers: vec![ServerView {
                        server: Server {
                            pid: 28398,
                            ppid: 28397,
                            parent_is_sshd: true,
                            elapsed_secs: 12 * 3600,
                            workspace: "gld".into(),
                            session: "402638ca-old".into(),
                        },
                        verdict: Verdict::Ended(SessionState::Completed),
                    }],
                    guard_left_held: None,
                },
                WorkspaceView {
                    name: "ccnm".into(),
                    root: Some("/p/ccnm".into()),
                    root_present: true,
                    agent: Some("fodelf".into()),
                    sessions: vec![],
                    servers: vec![],
                    guard_left_held: None,
                },
            ],
            now: 1_000 + 13 * 60,
        };
        let list = overview.render_list(Lang::Zh);
        assert!(list.contains("13 分钟"), "{list}");
        assert!(list.contains("402638ca"), "{list}");
        assert!(list.contains("pid 28398"), "{list}");
        let status = overview.render_status(Lang::En);
        assert!(status.contains("ORPHAN"), "{status}");
        assert!(status.contains("kill 28397"), "{status}");
        // The busy workspace comes first, although it sorts after by name.
        assert!(status.find("gld").unwrap() < status.find("ccnm ").unwrap());
    }

    /// 每种写锁状态各说各的，问不到的时候说问不到——哪一种都不能读成"空闲"（AU-03）。
    #[test]
    fn the_guard_line_says_what_the_runtime_saw_and_nothing_more() {
        use crate::mcp::write_guard::{
            Observation, Observed, Owner, Process, Reason, Resource, ResourceKind,
        };
        use crate::protocol::run::{AgentGuardReport, OwnerOnAgent};
        let report = |state, reason, owner: Option<Owner>, leftovers: Option<&str>, on_agent| {
            Ok(AgentGuardReport {
                protocol: crate::runtime::GUARD_PROTOCOL,
                agent_identity: crate::instance::AgentIdentity {
                    node: "worker".into(),
                    instance: "claude-main".into(),
                    provider: crate::provider::AgentProvider::Claude,
                    profile_ref: "default".into(),
                },
                runtime: crate::runtime::GuardReport {
                    protocol: crate::runtime::GUARD_PROTOCOL,
                    workspace: "demo".into(),
                    observation: Observation {
                        state,
                        reason,
                        resource: Resource {
                            kind: ResourceKind::GitCommonDir,
                            id: "00aa11bb22cc33dd".into(),
                        },
                        owner,
                        leftovers: leftovers.map(str::to_string),
                        observed_at: 1_789_391_418,
                    },
                },
                owner_on_agent: on_agent,
            })
        };
        let owner = |pid, process, legacy| Owner {
            session: "402638ca-0000-4000-8000-000000000000".into(),
            workspace: Some("demo".into()),
            pid,
            process,
            legacy,
        };
        let say = |answer| render_guard(&answer, 9 * 3600, Lang::Zh);

        let free = say(report(Observed::Free, Reason::Released, None, None, None));
        assert!(free.contains("空闲") && free.contains("不是预留"), "{free}");
        assert!(!free.contains("write-guards/"), "{free}");

        let orphan = say(report(
            Observed::Held,
            Reason::LiveHolder,
            Some(owner(Some(4242), Some(Process::Ccnm), false)),
            None,
            Some(OwnerOnAgent::State(SessionState::Completed)),
        ));
        assert!(orphan.contains("被占：会话 402638ca 正持有"), "{orphan}");
        assert!(orphan.contains("pid 4242 还是 ccnm 进程"), "{orphan}");
        assert!(orphan.contains("孤儿 mcp-serve"), "{orphan}");
        assert!(
            orphan.contains("write-guards/00aa11bb22cc33dd.lock"),
            "{orphan}"
        );

        let kept = say(report(
            Observed::Abandoned,
            Reason::KeptOnPurpose,
            Some(owner(Some(4242), Some(Process::Gone), false)),
            Some("1 command(s) (r-e69a)"),
            None,
        ));
        assert!(
            kept.contains("故意留着") && kept.contains("r-e69a"),
            "{kept}"
        );
        assert!(kept.contains("不等于它起的命令也没了"), "{kept}");

        let left = say(report(
            Observed::Unknown,
            Reason::LeftHeld,
            Some(owner(None, None, true)),
            None,
            Some(OwnerOnAgent::NotHere),
        ));
        assert!(
            left.contains("说不清") && left.contains("旧格式标记"),
            "{left}"
        );
        assert!(left.contains("没有这个会话的记录"), "{left}");

        let unreadable = say(report(
            Observed::Unknown,
            Reason::Unreadable,
            None,
            None,
            None,
        ));
        assert!(
            unreadable.contains("读不了自己的 write-guards/"),
            "{unreadable}"
        );

        let unasked = say(Err(crate::Error::new(
            crate::ErrorCode::RuntimeUnreachable,
            "ssh runtime-alias: Connection refused",
        )));
        assert!(
            unasked.contains("问不到 Runtime") && unasked.contains("不等于空闲"),
            "{unasked}"
        );

        let en = render_guard(
            &report(Observed::Free, Reason::NeverTaken, None, None, None),
            0,
            Lang::En,
        );
        assert!(
            en.starts_with("guard  free") && en.contains("not a reservation"),
            "{en}"
        );
    }

    #[test]
    fn clock_and_offset() {
        // 2026-09-14T13:10:18Z is 22:10 in +0900.
        assert_eq!(clock(1_789_391_418, 9 * 3600), "09-14 22:10");
        assert_eq!(parse_offset("+0900"), Some(32400));
        assert_eq!(parse_offset("-0330"), Some(-12600));
        assert_eq!(parse_offset("UTC"), None);
    }

    /// The three ways a session that is not `ok` can have ended read as three
    /// different things (F4). A stop used to be recorded as the first of
    /// them, so a session stopped after seven minutes was listed as
    /// `failed to start`, `<1m`.
    #[test]
    fn the_log_keeps_stopped_apart_from_never_started_and_killed() {
        let entry = |session: &str, outcome: &str| HistoryEntry {
            session: session.into(),
            workspace: "demo".into(),
            instance: None,
            mode: "interactive".into(),
            prompt: None,
            started: 1_789_391_418,
            ended: Some(1_789_391_418 + 420),
            state: SessionState::Failed,
            outcome: Some(serde_json::from_str(outcome).unwrap()),
        };
        let entries = [
            entry(
                "aaaaaaaa-stopped",
                r#"{"exit_code":null,"timed_out":false,"duration_ms":420000,"error":null,"stopped":true}"#,
            ),
            entry(
                "bbbbbbbb-never",
                r#"{"exit_code":null,"timed_out":false,"duration_ms":0,"error":"cannot spawn claude"}"#,
            ),
            entry(
                "cccccccc-killed",
                r#"{"exit_code":null,"timed_out":false,"duration_ms":61000,"error":null}"#,
            ),
            // What a stop wrote before P64: still readable, still shown the
            // way it was -- the duration it never recorded cannot be made up.
            entry(
                "dddddddd-legacy",
                r#"{"exit_code":null,"timed_out":false,"duration_ms":0,"error":"stopped by ccnm after the managed terminal ended"}"#,
            ),
        ];
        let line = |text: &str, id: &str| {
            text.lines()
                .find(|l| l.contains(id))
                .unwrap_or_else(|| panic!("no row for {id} in\n{text}"))
                .to_string()
        };
        let en = render_history(&entries, 0, 0, Lang::En);
        let stopped = line(&en, "aaaaaaaa");
        assert!(
            stopped.contains("stopped") && stopped.contains("7m"),
            "{stopped}"
        );
        assert!(!stopped.contains("failed to start"), "{stopped}");
        let never = line(&en, "bbbbbbbb");
        assert!(
            never.contains("failed to start") && never.contains("<1m"),
            "{never}"
        );
        let killed = line(&en, "cccccccc");
        assert!(
            killed.contains("killed") && killed.contains("1m"),
            "{killed}"
        );
        assert!(line(&en, "dddddddd").contains("failed to start"));

        let zh = render_history(&entries, 0, 0, Lang::Zh);
        let stopped = line(&zh, "aaaaaaaa");
        assert!(
            stopped.contains("被停止") && stopped.contains("7 分钟"),
            "{stopped}"
        );
        assert!(line(&zh, "bbbbbbbb").contains("没起来"));
    }

    #[test]
    fn spans_read_like_speech() {
        assert_eq!(span(30, Lang::Zh), "不到 1 分钟");
        assert_eq!(span(13 * 60, Lang::Zh), "13 分钟");
        assert_eq!(span(3 * 3600 + 120, Lang::En), "3h 2m");
        assert_eq!(span(2 * 86400 + 7200, Lang::Zh), "2 天 2 小时");
    }
}

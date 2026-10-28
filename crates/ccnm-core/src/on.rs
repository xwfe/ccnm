//! `--on`: which AI machine, and which instance on it, a command for a
//! project goes to (P114).
//!
//! A project names one AI machine in the config, its default. `--on` picks
//! another for one command without renaming the project or touching its
//! directory, and the machine the last session started from here went to is
//! remembered, so `ccnm attach xdo` after `ccnm xdo --on b` goes to b. It is
//! still one project whichever machine serves it: one root, one write guard.
//!
//! Only the machine holding the projects moves a project. An AI machine
//! keeps no workspace list, so on it `--on` can only pick one of its own
//! instances ([`local_instance`]).

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::lang::Lang;

/// What `--on` named, resolved against the config of the machine holding
/// the projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// A `[nodes.*]` name; `None` keeps the project's own machine.
    pub node: Option<String>,
    /// An instance on that machine; `None` keeps the project's selection.
    pub instance: Option<String>,
}

/// Where a command for one project goes, with the config it runs against.
#[derive(Debug, Clone)]
pub struct Placement {
    /// The config with the project's Agent moved to [`Placement::node`].
    /// Everything downstream reads the machine from here, so nothing past
    /// this point needs to know `--on` exists.
    pub config: Config,
    pub node: String,
    pub instance: Option<String>,
    /// The machine the config itself names for the project.
    pub default_node: String,
    /// Chosen because the last session started from here went there, not
    /// because `--on` said so.
    pub remembered: bool,
}

impl Placement {
    pub fn moved(&self) -> bool {
        self.node != self.default_node
    }
}

impl Config {
    /// Read `--on` for `workspace`: `<machine>`, `<machine>/<instance>`, or
    /// `<instance>` on the project's own machine.
    ///
    /// A single name is a machine first. Machines are what this flag is
    /// for, and an instance that happens to share a machine's name is still
    /// reachable as `<machine>/<instance>`; the other order would make a
    /// typed machine name silently mean an instance nobody registered.
    pub fn on_target(&self, workspace: &str, on: &str, lang: Lang) -> Result<Target> {
        let resolved = self.workspace_in(workspace, lang)?;
        if let Some((machine, instance)) = on.split_once('/') {
            let node = self
                .machine(machine, lang)?
                .ok_or_else(|| Error::invalid_args(self.no_such_machine(machine, lang)))?;
            crate::instance::identifier(instance).map_err(|_| {
                Error::invalid_args(lang.pick(
                    format!("--on {on}：斜杠后面的实例名只能是字母、数字、- 和 _"),
                    format!("--on {on}: the instance after the slash may only be letters, digits, - and _"),
                ))
            })?;
            return Ok(Target {
                node: Some(node),
                instance: Some(instance.to_string()),
            });
        }
        if let Some(node) = self.machine(on, lang)? {
            return Ok(Target {
                node: Some(node),
                instance: None,
            });
        }
        // Not a machine, so an instance on the project's own one -- which
        // only a project that selects by instance has.
        if resolved.workspace.agent.is_none() || crate::instance::identifier(on).is_err() {
            return Err(Error::invalid_args(self.no_such_machine(on, lang)));
        }
        Ok(Target {
            node: None,
            instance: Some(on.to_string()),
        })
    }

    /// The `[nodes.*]` name `name` means: the name itself, or the ssh alias
    /// of exactly one node. `None` when it is neither.
    fn machine(&self, name: &str, lang: Lang) -> Result<Option<String>> {
        if self.this.as_deref() == Some(name) {
            return Err(Error::invalid_args(lang.pick(
                format!("--on {name} 是这台机器自己；AI 要跑在另一台上，项目在这台"),
                format!("--on {name} is this machine; the AI runs on another one, and the project is here"),
            )));
        }
        if let Some(node) = self.nodes.get(name) {
            if node.ssh.is_none() {
                return Err(Error::config(lang.pick(
                    format!("[nodes.{name}] 没写 ssh 别名，这台连不过去\n加一行：ssh = \"<别名>\""),
                    format!("[nodes.{name}] has no ssh alias, so this machine cannot reach it\nadd: ssh = \"<alias>\""),
                )));
            }
            return Ok(Some(name.to_string()));
        }
        let by_alias: Vec<&String> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.ssh.as_deref() == Some(name))
            .map(|(key, _)| key)
            .collect();
        match by_alias.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some((*only).clone())),
            several => {
                let several = several
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(Error::config(lang.pick(
                    format!("ssh 别名 {name} 同时是这几台：{several}；用节点名选"),
                    format!("ssh alias {name} belongs to several nodes: {several}; pick one by node name"),
                )))
            }
        }
    }

    /// The `[nodes.*]` name of the AI machine `name` means, by node name or
    /// ssh alias, for commands that name a machine and no project.
    pub fn machine_named(&self, name: &str, lang: Lang) -> Result<String> {
        self.machine(name, lang)?
            .ok_or_else(|| Error::invalid_args(self.no_such_machine(name, lang)))
    }

    /// Every machine `--on` can name from here, as `node (alias)`.
    pub fn machines(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|(key, _)| self.this.as_deref() != Some(key.as_str()))
            .filter_map(|(key, node)| {
                let alias = node.ssh.as_deref()?;
                Some(if alias == key {
                    key.clone()
                } else {
                    format!("{key} ({alias})")
                })
            })
            .collect()
    }

    fn no_such_machine(&self, name: &str, lang: Lang) -> String {
        let machines = self.machines().join(", ");
        lang.pick(
            format!(
                "没有叫 {name} 的机器。能选的：{machines}\n要加一台：ccnm init --agent <它的 ssh 别名>（不会替换现有的那台）"
            ),
            format!(
                "no machine called {name}. These can be picked: {machines}\nto add one: ccnm init --agent <its ssh alias> (keeps the ones already there)"
            ),
        )
    }

    /// This config with `workspace`'s Agent on `node` instead. Validated
    /// again: a move to the project's own machine, or to a node this one
    /// cannot dial, is refused here rather than three hops later.
    pub fn moved_to(&self, workspace: &str, node: &str) -> Result<Config> {
        let mut moved = self.clone();
        let ws = moved
            .workspaces
            .get_mut(workspace)
            .ok_or_else(|| Error::config(format!("workspace '{workspace}' is not defined")))?;
        match &mut ws.agent {
            Some(reference) => reference.node = node.to_string(),
            None => ws.agent_node = node.to_string(),
        }
        moved.validate()?;
        Ok(moved)
    }

    /// Where a command for `workspace` goes: `--on` if given; otherwise the
    /// machine remembered in `state` (when there is one and it is still in
    /// this config); otherwise the project's own.
    pub fn place(
        &self,
        workspace: &str,
        on: Option<&str>,
        state: Option<&Path>,
        lang: Lang,
    ) -> Result<Placement> {
        let default_node = self.workspace_in(workspace, lang)?.agent_node().to_string();
        let (node, instance, remembered) = match on {
            Some(on) => {
                let target = self.on_target(workspace, on, lang)?;
                (
                    target.node.unwrap_or_else(|| default_node.clone()),
                    target.instance,
                    false,
                )
            }
            None => match state.and_then(|state| remembered(state, workspace)) {
                Some(node)
                    if self.machine(&node, lang).ok().flatten().as_deref()
                        == Some(node.as_str()) =>
                {
                    (node, None, true)
                }
                _ => (default_node.clone(), None, false),
            },
        };
        let config = if node == default_node {
            self.clone()
        } else {
            self.moved_to(workspace, &node)?
        };
        Ok(Placement {
            config,
            node,
            instance,
            default_node,
            remembered,
        })
    }

    /// Every project moved to the machine its last session from here went
    /// to, for the commands that look at all of them (`ccnm ls`).
    pub fn placed_all(&self, state: &Path) -> Config {
        let mut placed = self.clone();
        for name in self.workspaces.keys() {
            if let Ok(placement) = placed.place(name, None, Some(state), Lang::En)
                && placement.moved()
            {
                placed = placement.config;
            }
        }
        placed
    }
}

/// On an AI machine: the instance `--on` names. That machine keeps no
/// workspace list, so it cannot send a project elsewhere -- only pick one of
/// its own instances, or name itself.
pub fn local_instance(config: &Config, on: &str, lang: Lang) -> Result<Option<String>> {
    let this = config.this.as_deref();
    let (machine, instance) = match on.split_once('/') {
        Some((machine, instance)) => (Some(machine), instance),
        None => (None, on),
    };
    let is_here = |name: &str| this == Some(name);
    match machine {
        Some(machine) if !is_here(machine) => Err(elsewhere(config, machine, lang)),
        Some(_) => Ok(Some(instance.to_string())),
        None if is_here(on) => Ok(None),
        None if config.agents.contains_key(on) => Ok(Some(on.to_string())),
        None if config.nodes.contains_key(on)
            || config.nodes.values().any(|n| n.ssh.as_deref() == Some(on)) =>
        {
            Err(elsewhere(config, on, lang))
        }
        None => Ok(Some(on.to_string())),
    }
}

fn elsewhere(config: &Config, machine: &str, lang: Lang) -> Error {
    let runtime = config
        .runtime_from_agent()
        .map_or("<项目那台>", |(alias, _)| alias);
    Error::invalid_args(lang.pick(
        format!(
            "在 AI 机器上，--on 只能选这台自己的实例；要换到 {machine}，去项目那台敲：ssh {runtime} 之后 ccnm <项目> --on {machine}"
        ),
        format!(
            "on an AI machine --on can only pick one of its own instances; to move to {machine}, type it where the projects are: ssh {runtime}, then ccnm <workspace> --on {machine}"
        ),
    ))
}

/// The file the last session's machine is remembered in.
fn record(state: &Path, workspace: &str) -> PathBuf {
    crate::paths::workspace_dir(state, workspace).join("on")
}

/// The machine the last session started from here for `workspace` went to,
/// when it was not the project's own.
pub fn remembered(state: &Path, workspace: &str) -> Option<String> {
    let text = std::fs::read_to_string(record(state, workspace)).ok()?;
    let node = text.trim();
    crate::instance::identifier(node).ok()?;
    Some(node.to_string())
}

/// Remember where `workspace`'s session went: `Some(node)` when it was not
/// the project's own machine, `None` to forget. Written whole and renamed
/// into place, so a reader never sees half a name.
pub fn remember(state: &Path, workspace: &str, node: Option<&str>) -> Result<()> {
    let path = record(state, workspace);
    let Some(node) = node else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::config(format!(
                "cannot forget where {workspace} last ran: {}",
                path.display()
            ))
            .with_source(e)),
            _ => Ok(()),
        };
    };
    let dir = path.parent().expect("a workspace directory");
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::config(format!("cannot create {}", dir.display())).with_source(e))?;
    let temp = dir.join(format!(".on.{}", std::process::id()));
    std::fs::write(&temp, format!("{node}\n"))
        .and_then(|()| std::fs::rename(&temp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            Error::config(format!(
                "cannot remember where {workspace} ran: {}",
                path.display()
            ))
            .with_source(e)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "this = \"runtime\"\n[nodes.runtime]\n[nodes.agent]\nssh = \"fodelf\"\n[nodes.hpsrv]\nssh = \"hpsrv-alias\"\n[workspaces.xdo]\nagent_node = \"agent\"\nroot = \"/p/xdo\"\n[workspaces.inst]\nagent = { node = \"agent\", instance = \"claude-main\" }\nroot = \"/p/inst\"\n";

    fn config() -> Config {
        Config::parse(TWO).unwrap()
    }

    fn temp(test: &str) -> ccnm_testdir::TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-on-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ccnm_testdir::TestDir::adopt(dir)
    }

    fn target(node: Option<&str>, instance: Option<&str>) -> Target {
        Target {
            node: node.map(str::to_string),
            instance: instance.map(str::to_string),
        }
    }

    #[test]
    fn a_machine_is_named_by_node_or_by_ssh_alias() {
        let c = config();
        assert_eq!(
            c.on_target("xdo", "hpsrv", Lang::En).unwrap(),
            target(Some("hpsrv"), None)
        );
        assert_eq!(
            c.on_target("xdo", "hpsrv-alias", Lang::En).unwrap(),
            target(Some("hpsrv"), None)
        );
        assert_eq!(
            c.on_target("xdo", "fodelf", Lang::En).unwrap(),
            target(Some("agent"), None)
        );
    }

    #[test]
    fn machine_slash_instance_picks_both() {
        assert_eq!(
            config()
                .on_target("inst", "hpsrv/codex-main", Lang::En)
                .unwrap(),
            target(Some("hpsrv"), Some("codex-main"))
        );
    }

    #[test]
    fn a_name_that_is_no_machine_is_an_instance_on_the_projects_own() {
        assert_eq!(
            config().on_target("inst", "codex-main", Lang::En).unwrap(),
            target(None, Some("codex-main"))
        );
    }

    /// A project that runs on a machine's default login has no instance to
    /// pick, so a name that is no machine is a typo, and the error lists
    /// what could have been meant.
    #[test]
    fn on_a_default_login_project_an_unknown_name_lists_the_machines() {
        let err = config().on_target("xdo", "hpsrvv", Lang::En).unwrap_err();
        let text = err.message();
        assert!(text.contains("no machine called hpsrvv"), "{text}");
        assert!(
            text.contains("agent (fodelf)") && text.contains("hpsrv (hpsrv-alias)"),
            "{text}"
        );
        assert!(text.contains("ccnm init --agent"), "{text}");
    }

    #[test]
    fn a_machine_name_wins_over_an_instance_of_the_same_name() {
        // `hpsrv` could be an instance on `agent`; it is the machine.
        assert_eq!(
            config().on_target("inst", "hpsrv", Lang::En).unwrap(),
            target(Some("hpsrv"), None)
        );
        // And the instance is still reachable, spelled in full.
        assert_eq!(
            config().on_target("inst", "agent/hpsrv", Lang::En).unwrap(),
            target(Some("agent"), Some("hpsrv"))
        );
    }

    #[test]
    fn this_machine_and_an_unknown_machine_before_a_slash_are_refused() {
        let c = config();
        let err = c.on_target("xdo", "runtime", Lang::En).unwrap_err();
        assert!(
            err.message().contains("is this machine"),
            "{}",
            err.message()
        );
        let err = c
            .on_target("inst", "nowhere/codex-main", Lang::En)
            .unwrap_err();
        assert!(
            err.message().contains("no machine called nowhere"),
            "{}",
            err.message()
        );
        let err = c.on_target("inst", "hpsrv/bad name", Lang::En).unwrap_err();
        assert!(
            err.message().contains("letters, digits"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn a_node_without_an_alias_cannot_be_picked() {
        let c = Config::parse(&format!("{TWO}[nodes.cold]\n")).unwrap();
        let err = c.on_target("xdo", "cold", Lang::En).unwrap_err();
        assert!(
            err.message().contains("has no ssh alias"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn an_alias_shared_by_two_nodes_is_ambiguous() {
        let c = Config::parse(&format!("{TWO}[nodes.twin]\nssh = \"fodelf\"\n")).unwrap();
        let err = c.on_target("xdo", "fodelf", Lang::En).unwrap_err();
        assert!(err.message().contains("several nodes"), "{}", err.message());
    }

    #[test]
    fn moving_changes_only_the_projects_machine() {
        let c = config();
        let moved = c.moved_to("xdo", "hpsrv").unwrap();
        assert_eq!(moved.workspace("xdo").unwrap().agent_node(), "hpsrv");
        assert_eq!(
            moved.workspace("xdo").unwrap().agent_ssh().unwrap(),
            "hpsrv-alias"
        );
        assert_eq!(moved.workspaces["xdo"].root, c.workspaces["xdo"].root);
        assert_eq!(moved.workspace("inst").unwrap().agent_node(), "agent");

        let moved = c.moved_to("inst", "hpsrv").unwrap();
        let reference = moved.workspaces["inst"].agent.as_ref().unwrap();
        assert_eq!(
            (reference.node.as_str(), reference.instance.as_str()),
            ("hpsrv", "claude-main")
        );
    }

    #[test]
    fn the_last_machine_is_remembered_and_forgotten() {
        let state = temp("remember");
        assert_eq!(remembered(state.path(), "xdo"), None);
        remember(state.path(), "xdo", Some("hpsrv")).unwrap();
        assert_eq!(remembered(state.path(), "xdo").as_deref(), Some("hpsrv"));
        assert_eq!(remembered(state.path(), "inst"), None);

        let c = config();
        let placed = c.place("xdo", None, Some(state.path()), Lang::En).unwrap();
        assert_eq!((placed.node.as_str(), placed.remembered), ("hpsrv", true));
        assert_eq!(
            placed.config.workspace("xdo").unwrap().agent_node(),
            "hpsrv"
        );
        // `--on` wins over the record.
        let placed = c
            .place("xdo", Some("fodelf"), Some(state.path()), Lang::En)
            .unwrap();
        assert_eq!((placed.node.as_str(), placed.remembered), ("agent", false));
        assert!(!placed.moved());
        // `ccnm ls` sees the project where it went.
        assert_eq!(
            c.placed_all(state.path())
                .workspace("xdo")
                .unwrap()
                .agent_node(),
            "hpsrv"
        );

        remember(state.path(), "xdo", None).unwrap();
        remember(state.path(), "xdo", None).unwrap();
        assert_eq!(remembered(state.path(), "xdo"), None);
    }

    /// A record naming a machine since removed from the config is ignored,
    /// not an error every command trips over.
    #[test]
    fn a_record_of_a_machine_no_longer_configured_is_ignored() {
        let state = temp("stale");
        remember(state.path(), "xdo", Some("gone")).unwrap();
        let placed = config()
            .place("xdo", None, Some(state.path()), Lang::En)
            .unwrap();
        assert_eq!((placed.node.as_str(), placed.remembered), ("agent", false));
    }

    #[test]
    fn on_an_ai_machine_only_its_own_instances_can_be_picked() {
        let agent = Config::parse(
            "this = \"agent\"\nruntime_node = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"xdwmbp\"\n[agents.codex-main]\nprovider = \"codex\"\nprofile_ref = \"main\"\n",
        )
        .unwrap();
        assert_eq!(
            local_instance(&agent, "codex-main", Lang::En)
                .unwrap()
                .as_deref(),
            Some("codex-main")
        );
        assert_eq!(local_instance(&agent, "agent", Lang::En).unwrap(), None);
        assert_eq!(
            local_instance(&agent, "agent/codex-main", Lang::En)
                .unwrap()
                .as_deref(),
            Some("codex-main")
        );
        let err = local_instance(&agent, "hpsrv/codex-main", Lang::En).unwrap_err();
        assert!(err.message().contains("ssh xdwmbp"), "{}", err.message());
        let err = local_instance(&agent, "xdwmbp", Lang::En).unwrap_err();
        assert!(
            err.message().contains("where the projects are"),
            "{}",
            err.message()
        );
    }
}

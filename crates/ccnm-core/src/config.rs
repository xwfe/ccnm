//! `~/.config/ccnm/config.toml`, the Runtime Node's source of truth for
//! workspace definitions.
//!
//! Secrets never live here (design doc section 5); SSH keys and Claude
//! OAuth stay with OpenSSH and Claude Code itself.
//!
//! Unknown keys are an error, not ignored. A typo like `runtime_hots` that
//! silently falls back to a default is exactly the drift doctor exists to
//! catch, so the parser refuses it up front.
//!
//! Two backends share the schema. `mcp-ssh` (the default and the only one
//! this build implements) needs nothing beyond nodes and `root`. The
//! `hybrid-smb` fallback (appendix A) additionally needs `share`,
//! `runtime_root`, `mount_mode` and the Runtime Node's `smb_user`; those
//! fields are rejected on an `mcp-ssh` workspace so a half-migrated config
//! cannot look valid.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The only `version = N` this binary understands.
///
/// It is no longer written, and no longer required: a config is what its
/// nodes and workspaces say, and a schema version nobody has ever needed
/// to bump is a line every reader has to wonder about. Old configs still
/// have it, so it is still accepted -- and still checked, because a file
/// that says `version = 2` was written for a ccnm this is not.
pub const SUPPORTED_VERSION: u32 = 1;

/// `workspaces.<name>.runtime_node` when the file does not say.
pub const DEFAULT_RUNTIME_NODE: &str = "runtime";

/// Where a remote ccnm is invoked when `nodes.<x>.ccnm_bin` is unset. The
/// `~` is expanded by the remote login shell, which is the one thing
/// every POSIX shell and fish agree on; a bare `ccnm` would depend on the
/// PATH of a non-interactive shell (design doc section 7).
pub const DEFAULT_CCNM_BIN: &str = "~/.local/bin/ccnm";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Absent in anything ccnm writes now; see [`SUPPORTED_VERSION`].
    #[serde(default)]
    pub version: Option<u32>,
    /// Which entry of `nodes` is the machine reading this file.
    ///
    /// Every `ssh` alias in the file is written from this node's point of
    /// view, so without it not one of them can be resolved. It also
    /// settles which role this machine plays in a workspace, which used to
    /// be inferred from which alias fields happened to be present -- an
    /// inference that once made a mistyped workspace name at the runtime
    /// look like an agent-only config and sent the request over ssh.
    #[serde(default)]
    pub this: Option<String>,
    /// This machine keeps no workspace list, and asks the named node about
    /// any workspace it is given.
    ///
    /// Set on an Agent Node and nowhere else. It is not the same question
    /// as "which node am I": a Runtime Node that has been set up but has
    /// no workspaces yet looks exactly like an Agent Node otherwise, and
    /// guessing wrong there sends the request over ssh to a machine that
    /// will send it straight back.
    #[serde(default)]
    pub runtime_node: Option<String>,
    #[serde(default)]
    pub nodes: BTreeMap<String, Node>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
    /// Definitions belong only to `this` node. Other nodes hold references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, crate::instance::AgentInstance>,
}

/// One physical or virtual machine. A node may carry one or more roles.
///
/// Every node a workspace names, other than [`Config::this`] itself, needs
/// an `ssh` alias: that is the one this machine dials to reach it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Alias in *this* machine's `~/.ssh/config` that reaches this node.
    ///
    /// One field, not one per direction: an alias only ever means
    /// something to the machine whose `~/.ssh/config` defines it, and that
    /// machine is the one reading this file. The far side has its own
    /// config with its own aliases, so nothing here has to describe how
    /// somebody else dials.
    ///
    /// Unset on [`Config::this`], which does not ssh to itself.
    #[serde(default)]
    pub ssh: Option<String>,
    /// Absolute path of the ccnm binary on this node, for the machine that
    /// sshes in. Unset means [`DEFAULT_CCNM_BIN`].
    #[serde(default)]
    pub ccnm_bin: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` for Claude Code on this host. Unset means Claude's
    /// own default (`~/.claude`) and whatever login is already there. A custom
    /// dir has its own credentials and needs its own `claude auth login`;
    /// ccnm never performs that login (design doc section 21).
    #[serde(default)]
    pub claude_config_dir: Option<PathBuf>,
    /// The dedicated account the MCP runtime must run as on this node
    /// (design doc section 18). ccnm never creates it and never switches
    /// to it; it checks that it is what the runtime is running as, and
    /// refuses `exec_command` when it is not.
    ///
    /// Unset is itself a failure on the Runtime Node: without it ccnm
    /// cannot tell the dedicated account from the developer's own.
    #[serde(default)]
    pub runtime_user: Option<String>,
    /// Hybrid only: account the Agent Node mounts the SMB share as.
    #[serde(default)]
    pub smb_user: Option<String>,
}

impl Node {
    /// The path to run on this node from another node.
    pub fn ccnm_bin(&self) -> String {
        match &self.ccnm_bin {
            Some(path) => path.to_string_lossy().into_owned(),
            None => DEFAULT_CCNM_BIN.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub backend: Backend,
    /// Key into `nodes`: where the AI coding agent runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::instance::InstanceRef>,
    /// Key into `nodes`: where the project lives and every tool runs.
    #[serde(default = "default_runtime_node")]
    pub runtime_node: String,
    /// Project root on the Runtime Node. With `mcp-ssh` it neither needs
    /// nor should exist on the Agent Node.
    pub root: PathBuf,
    #[serde(default)]
    pub claude_permission_mode: PermissionMode,
    /// Run `exec_command` for this workspace even though the runtime
    /// account is not confined (design doc section 18).
    ///
    /// Spelled out rather than shortened on purpose. The default refusal
    /// is the hard gate the design document asks for; this is the way to
    /// say "I know, this is a scratch project, go ahead", and every
    /// result of such a session says so.
    #[serde(default)]
    pub allow_unconfined_exec: bool,
    /// Hybrid only: where the restricted runner may write. Must not overlap
    /// `root`.
    #[serde(default)]
    pub runtime_root: Option<PathBuf>,
    /// Hybrid only: SMB share name the Agent Node mounts.
    #[serde(default)]
    pub share: Option<String>,
    /// Hybrid only.
    #[serde(default)]
    pub mount_mode: Option<MountMode>,
}

fn default_runtime_node() -> String {
    DEFAULT_RUNTIME_NODE.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// One persistent SSH stdio transport carrying MCP to a ccnm runtime
    /// on the Runtime Node. The primary architecture.
    #[default]
    McpSsh,
    /// SMB mount plus SSH runner (appendix A). Parsed so a config can name
    /// it; not implemented by this build.
    HybridSmb,
}

impl Backend {
    /// The value as written in config.toml.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::McpSsh => "mcp-ssh",
            Backend::HybridSmb => "hybrid-smb",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    /// Mount with `nodatacache,nomdatacache,nopassprompt,soft,nobrowse`
    /// (appendix A.12). The only mode the Hybrid design ever had.
    #[default]
    Coherence,
}

// The public config spelling remains Claude-compatible in phase one.
pub use crate::provider::PermissionMode;

/// A workspace together with both nodes it spans and the role-specific
/// fields validation has already proven present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub name: &'a str,
    pub workspace: &'a Workspace,
    pub agent: &'a Node,
    pub runtime: &'a Node,
    /// The node this config belongs to, for deciding which role this
    /// machine plays in this workspace.
    pub this: &'a str,
}

impl<'a> Resolved<'a> {
    pub fn agent_reference(
        &self,
        override_id: Option<&str>,
    ) -> Result<Option<crate::instance::InstanceRef>> {
        match (&self.workspace.agent, override_id) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(Error::config(
                "--agent requires an instance-selected workspace; legacy Claude workspaces keep their existing selection",
            )),
            (Some(reference), None) => Ok(Some(reference.clone())),
            (Some(reference), Some(id)) => {
                crate::instance::identifier(id)?;
                Ok(Some(crate::instance::InstanceRef {
                    node: reference.node.clone(),
                    instance: id.to_string(),
                }))
            }
        }
    }

    pub fn agent_node(&self) -> &str {
        self.workspace
            .agent
            .as_ref()
            .map_or(&self.workspace.agent_node, |reference| &reference.node)
    }

    /// True when the agent and the project are the same machine, so no
    /// MCP transport is dialled back and Claude works with its own native
    /// tools. See [`Topology`].
    pub fn is_colocated(&self) -> bool {
        self.agent_node() == self.workspace.runtime_node
    }

    /// Which machine this one is in this workspace.
    pub fn topology(&self) -> Topology {
        if self.is_colocated() {
            Topology::Colocated
        } else if self.this == self.workspace.runtime_node {
            Topology::FromRuntime
        } else if self.this == self.agent_node() {
            Topology::FromAgent
        } else {
            Topology::Bystander
        }
    }

    /// The alias this machine dials to reach the Agent Node.
    ///
    /// An error only when this machine *is* the Agent Node, which the
    /// caller should have handled by delegating instead of dialling.
    pub fn agent_ssh(&self) -> Result<&'a str> {
        self.agent.ssh.as_deref().ok_or_else(|| {
            Error::config(format!(
                "workspace '{}' runs the agent on '{}', which is this node, so there is nothing to ssh to",
                self.name, self.agent_node()
            ))
        })
    }
}

/// Where this machine sits relative to a workspace's two roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// `runtime -> agent -> runtime`: this machine holds the project and
    /// dials the agent, which dials an MCP transport back here.
    FromRuntime,
    /// `agent -> runtime`: this machine runs the agent but the workspace
    /// list lives on the runtime, so the whole launch is delegated there
    /// and comes back as [`Topology::FromRuntime`].
    FromAgent,
    /// `runtime -> agent`: agent and project are the same machine. Claude
    /// uses its own native tools, nothing dials back, and ccnm is only
    /// managing the session.
    Colocated,
    /// This machine is neither role: it launches a session on machines
    /// that are, and attaches to it.
    Bystander,
}

impl Config {
    /// The alias this machine uses to reach the machine holding the
    /// projects, and that host's other settings, when this is a Agent-side
    /// config.
    ///
    /// The [`Host`] comes back with the alias because `ccnm_bin` is on it:
    /// a caller that took only the alias would run the *default* path on
    /// the far side and ignore what the config said, which fails as
    /// "command not found" on the one machine whose ccnm is somewhere
    /// else.
    ///
    /// A Agent Node's config is the same file with the workspaces left
    /// out: it says how to reach home and nothing else, because the
    /// workspace list has exactly one home and duplicating it here is how
    /// the two copies start disagreeing about where a project is. That
    /// disagreement is not theoretical -- a session bound to a root the
    /// config no longer names is the failure this project has already
    /// spent an afternoon on.
    ///
    /// `None` unless this really is an agent-only config.
    ///
    /// The whole test is the top-level [`Config::runtime_node`], which the
    /// file states outright. Every version that inferred it instead got it
    /// wrong somewhere: on the presence of the reverse alias field, which
    /// made a mistyped workspace name on the runtime look like an agent
    /// and sent the request over ssh; then on the *absence* of the forward
    /// one, which stopped meaning anything once both directions were
    /// spelled `ssh`; then on having no workspaces, which is also true of
    /// a Runtime Node nobody has added a project to yet -- and that one
    /// bounces the request to a machine that bounces it straight back.
    pub fn runtime_from_agent(&self) -> Option<(&str, &Node)> {
        let name = self.runtime_node.as_deref()?;
        let node = self.nodes.get(name)?;
        Some((node.ssh.as_deref()?, node))
    }

    /// Read, parse and validate the file at `path`.
    pub fn load(path: &Path) -> Result<Config> {
        tracing::debug!(path = %path.display(), "loading config");
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::config(format!(
                    "config not found: {}\nwrite it with one command, on the machine you are on:\n  ccnm init --agent <alias>      here are the projects, that is where Claude runs\n  ccnm init --runtime <alias>    here runs Claude, that is where the projects are",
                    path.display()
                ))
            } else {
                Error::config(format!("cannot read config {}", path.display())).with_source(e)
            }
        })?;
        Config::parse(&text)
            .map_err(|e| Error::config(format!("{}: {}", path.display(), e.message())))
    }

    /// Parse and validate TOML text. Every validation problem is reported
    /// in one error so the user fixes the file once, not once per run.
    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text).map_err(|e| Error::config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Look up a workspace by name together with its nodes.
    pub fn workspace<'a>(&'a self, name: &'a str) -> Result<Resolved<'a>> {
        let workspace = self.workspaces.get(name).ok_or_else(|| {
            if self.workspaces.is_empty() {
                // A config with no workspaces and one way home is the
                // Agent Node's, on purpose: a project's root is
                // defined in exactly one place. So "not defined" is true
                // and unhelpful here -- it reads as "add it", and adding
                // it is the one thing this split exists to prevent. Say
                // where the list lives instead. The commands that work
                // from this side never reach this, because they answer
                // locally; what reaches it is `doctor <ws>` and
                // `mcp probe`, which need the definition.
                match self.runtime_from_agent() {
                    Some((runtime, _)) => Error::config(format!(
                        "workspace '{name}' is not defined on this node, and this node keeps no workspace list -- the projects are on Runtime Node {runtime}\nrun the commands that need the definition there:  ssh {runtime} ccnm doctor {name}"
                    )),
                    None => Error::config(format!(
                        "workspace '{name}' is not defined (no workspaces in config)"
                    )),
                }
            } else {
                let defined: Vec<&str> = self.workspaces.keys().map(String::as_str).collect();
                Error::config(format!(
                    "workspace '{name}' is not defined; defined: {}",
                    defined.join(", ")
                ))
            }
        })?;
        // validate() already guarantees all of these; a miss here is a bug.
        let bug = |what: &str| {
            Error::internal(format!(
                "workspace '{name}' passed validation but {what} is missing"
            ))
        };
        let agent_node = workspace
            .agent
            .as_ref()
            .map_or(&workspace.agent_node, |reference| &reference.node);
        let agent = self
            .nodes
            .get(agent_node)
            .ok_or_else(|| bug("its Agent Node"))?;
        let runtime = self
            .nodes
            .get(&workspace.runtime_node)
            .ok_or_else(|| bug("its Runtime Node"))?;
        Ok(Resolved {
            name,
            workspace,
            agent,
            runtime,
            this: self.this.as_deref().ok_or_else(|| bug("`this`"))?,
        })
    }

    fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();

        if let Some(version) = self.version
            && version != SUPPORTED_VERSION
        {
            problems.push(format!(
                "version = {version} is not supported; this ccnm understands version = {SUPPORTED_VERSION}, and no longer needs the line at all"
            ));
        }

        match &self.this {
            None if self.nodes.is_empty() && self.workspaces.is_empty() && self.agents.is_empty() => {}
            None => problems.push(
                "`this` is not set: every `ssh` alias in this file is written from one node's point of view, and without `this` there is no way to know whose\nadd the line `this = \"<node>\"` naming the entry of [nodes.*] that is this machine".to_string(),
            ),
            Some(this) if !self.nodes.contains_key(this) => problems.push(format!(
                "this = \"{this}\" does not match any [nodes.*] entry{}",
                match self.nodes.keys().next() {
                    Some(_) => format!(
                        "; defined: {}",
                        self.nodes.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                    None => ", and there are no nodes at all".to_string(),
                }
            )),
            Some(this) => {
                if self.nodes[this].ssh.is_some() {
                    problems.push(format!(
                        "nodes.{this}.ssh is set, but this = \"{this}\" says that node is this machine, which does not ssh to itself\nremove the line, or point `this` at the node this machine really is"
                    ));
                }
            }
        }

        // The node an agent delegates to has to be reachable from here,
        // and must not be this machine: a config that asks itself about a
        // workspace it does not have is the loop this field exists to
        // prevent.
        if let Some(name) = &self.runtime_node {
            if Some(name.as_str()) == self.this.as_deref() {
                // One problem, not two: a node that is this machine is not
                // also missing an alias, it must not have one.
                problems.push(format!(
                    "runtime_node = \"{name}\" is this node, so it says to ask this machine about workspaces it does not have\nset it to the node that keeps the workspace list, or remove it and define the workspaces here"
                ));
            } else {
                match self.nodes.get(name) {
                    None => problems.push(format!(
                        "runtime_node = \"{name}\" does not match any [nodes.*] entry"
                    )),
                    Some(node) if node.ssh.is_none() => problems.push(format!(
                        "runtime_node = \"{name}\" names a node without an `ssh` alias to reach it by"
                    )),
                    Some(_) => {}
                }
            }
            if !self.workspaces.is_empty() {
                problems.push(
                    "runtime_node says this machine keeps no workspace list, but [workspaces.*] is not empty\na project's root is defined on exactly one machine; keep the list or keep the delegation, not both"
                        .to_string(),
                );
            }
        }

        for (name, node) in &self.nodes {
            let at = format!("nodes.{name}");
            check_name(&at, name, &mut problems);
            for (field, value) in [("ssh", &node.ssh), ("smb_user", &node.smb_user)] {
                if let Some(value) = value {
                    check_token(&format!("{at}.{field}"), value, &mut problems);
                }
            }
            if let Some(dir) = &node.claude_config_dir {
                check_absolute(&format!("{at}.claude_config_dir"), dir, &mut problems);
            }
            if let Some(bin) = &node.ccnm_bin {
                let at = format!("{at}.ccnm_bin");
                if check_absolute(&at, bin, &mut problems) && !is_remote_path(bin) {
                    problems.push(format!(
                        "{at} must contain only [A-Za-z0-9._/-] so the remote shell never has to quote it, got \"{}\"",
                        bin.display()
                    ));
                }
            }
        }

        for (name, ws) in &self.workspaces {
            let at = format!("workspaces.{name}");
            check_name(&at, name, &mut problems);
            // Every node a workspace names, except the one this machine
            // is, has to be dialable from here. When both roles land on
            // the same node it is one machine and one alias, so it is
            // checked once rather than reported twice.
            let agent_node = ws
                .agent
                .as_ref()
                .map_or(&ws.agent_node, |reference| &reference.node);
            let agent_role = if ws.agent.is_some() {
                "agent.node"
            } else {
                "agent_node"
            };
            let roles: &[(&str, &String)] = if *agent_node == ws.runtime_node {
                &[(agent_role, agent_node)]
            } else {
                &[(agent_role, agent_node), ("runtime_node", &ws.runtime_node)]
            };
            for (role, node_name) in roles {
                match self.nodes.get(*node_name) {
                    None => problems.push(format!(
                        "{at}.{role} = \"{node_name}\" does not match any [nodes.*] entry"
                    )),
                    Some(node)
                        if node.ssh.is_none()
                            && Some(node_name.as_str()) != self.this.as_deref() =>
                    {
                        problems.push(format!(
                            "{at}.{role} = \"{node_name}\" is another machine, so [nodes.{node_name}] needs an `ssh` alias this one can dial it with"
                        ));
                    }
                    Some(_) => {}
                }
            }
            let runtime = self.nodes.get(&ws.runtime_node);
            let root_ok = check_absolute(&format!("{at}.root"), &ws.root, &mut problems);

            match ws.backend {
                Backend::McpSsh => {
                    for (field, present) in [
                        ("share", ws.share.is_some()),
                        ("mount_mode", ws.mount_mode.is_some()),
                        ("runtime_root", ws.runtime_root.is_some()),
                    ] {
                        if present {
                            problems.push(format!(
                                "{at}.{field} is only valid with backend = \"hybrid-smb\"; the mcp-ssh runtime has no mount"
                            ));
                        }
                    }
                }
                Backend::HybridSmb => {
                    match &ws.share {
                        None => problems.push(format!(
                            "{at}.share is required with backend = \"hybrid-smb\""
                        )),
                        Some(share) if share.trim().is_empty() => {
                            problems.push(format!("{at}.share must be the SMB share name"));
                        }
                        Some(share) => check_token(&format!("{at}.share"), share, &mut problems),
                    }
                    match &ws.runtime_root {
                        None => problems.push(format!(
                            "{at}.runtime_root is required with backend = \"hybrid-smb\""
                        )),
                        Some(runtime_root) => {
                            let ok = check_absolute(
                                &format!("{at}.runtime_root"),
                                runtime_root,
                                &mut problems,
                            );
                            if root_ok && ok && overlaps(&ws.root, runtime_root) {
                                problems.push(format!(
                                    "{at}.runtime_root must not overlap root: the runner would get write access to source"
                                ));
                            }
                        }
                    }
                    if let Some(host) = runtime
                        && host.smb_user.is_none()
                    {
                        problems.push(format!(
                            "{at}.runtime_node = \"{}\" names a host without `smb_user`, which backend = \"hybrid-smb\" needs to mount the share",
                            ws.runtime_node
                        ));
                    }
                }
            }
        }

        crate::instance::validate_config(self, &mut problems);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::config(problems.join("\n")))
        }
    }
}

/// Names end up in tmux session names, session ids and state paths, so
/// keep them to characters that are safe everywhere.
fn check_name(at: &str, name: &str, problems: &mut Vec<String>) {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !ok {
        problems.push(format!(
            "{at}: name must be [A-Za-z0-9][A-Za-z0-9_-]*, got \"{name}\""
        ));
    }
}

/// SSH aliases and share names travel through ssh command lines.
/// Restricting them to this set means they never need quoting anywhere.
pub(crate) fn is_token(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_token(at: &str, value: &str, problems: &mut Vec<String>) {
    if !is_token(value) {
        problems.push(format!(
            "{at} must match [A-Za-z0-9._-]+ and not start with '-', got \"{value}\""
        ));
    }
}

/// A path that will appear verbatim as one word of a remote ssh command.
fn is_remote_path(path: &Path) -> bool {
    path.to_str().is_some_and(|s| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    })
}

/// Absolute and free of `.` / `..` so that lexical comparisons between
/// paths mean what they look like.
fn check_absolute(at: &str, path: &Path, problems: &mut Vec<String>) -> bool {
    if !path.is_absolute() {
        problems.push(format!(
            "{at} must be an absolute path, got \"{}\"",
            path.display()
        ));
        return false;
    }
    let dotty = path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir));
    if dotty {
        problems.push(format!(
            "{at} must not contain \".\" or \"..\", got \"{}\"",
            path.display()
        ));
        return false;
    }
    true
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file an Agent Node keeps: who it is, that the projects are
    /// listed elsewhere, and the one alias it dials them by.
    const AGENT_SIDE: &str = "this = \"agent\"\nruntime_node = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"xdwmbp\"\n";

    /// A config with no workspace list is an Agent Node's, and a name it
    /// does not know is a question for the other side. A Runtime Node's
    /// config also names another node to dial, so keying on that alone
    /// would send every mistyped workspace name over ssh to be asked
    /// about.
    #[test]
    fn only_a_config_with_no_workspaces_delegates() {
        let alias = |c: &Config| c.runtime_from_agent().map(|(alias, _)| alias.to_string());

        let agent_side_cfg = Config::parse(AGENT_SIDE).unwrap();
        assert_eq!(alias(&agent_side_cfg).as_deref(), Some("xdwmbp"));

        let runtime_side_cfg =
            Config::parse("this = \"runtime\"\n[nodes.agent]\nssh = \"fodelf\"\n[nodes.runtime]\n")
                .unwrap();
        assert_eq!(alias(&runtime_side_cfg), None);

        // Nothing to pick.
        let empty = Config::parse("").unwrap();
        assert_eq!(alias(&empty), None);
        let two = Config::parse(
            "this = \"me\"\n[nodes.me]\n[nodes.a]\nssh = \"x\"\n[nodes.b]\nssh = \"y\"\n",
        )
        .unwrap();
        assert_eq!(alias(&two), None);
    }

    /// The three shapes ccnm supports, told apart by what the config says
    /// rather than by what the machine looks like.
    #[test]
    fn the_three_topologies_are_told_apart_by_this_and_the_two_roles() {
        let topo = |text: &str| {
            Config::parse(text)
                .unwrap()
                .workspace("x")
                .unwrap()
                .topology()
        };

        // runtime -> agent -> runtime: the projects are here, Claude is
        // over there, and the tools come back.
        assert_eq!(
            topo(
                "this = \"runtime\"\n[nodes.agent]\nssh = \"a\"\n[nodes.runtime]\n\
                 [workspaces.x]\nagent_node = \"agent\"\nroot = \"/p\"\n"
            ),
            Topology::FromRuntime
        );

        // agent -> runtime: this machine runs Claude but does not define
        // the workspace, so the launch is delegated and comes back.
        assert_eq!(
            topo(
                "this = \"agent\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"r\"\n\
                 [workspaces.x]\nagent_node = \"agent\"\nroot = \"/p\"\n"
            ),
            Topology::FromAgent
        );

        // runtime -> agent: one machine has both Claude and the project,
        // and this one is only launching. Nothing dials back.
        let colocated = Config::parse(
            "this = \"here\"\n[nodes.here]\n[nodes.box]\nssh = \"b\"\n\
             [workspaces.x]\nagent_node = \"box\"\nruntime_node = \"box\"\nroot = \"/p\"\n",
        )
        .unwrap();
        let r = colocated.workspace("x").unwrap();
        assert_eq!(r.topology(), Topology::Colocated);
        assert!(r.is_colocated());
        assert_eq!(r.agent_ssh().unwrap(), "b");
    }

    /// A node only needs an alias when it is somewhere else. Both roles on
    /// the machine reading the file is one alias fewer, not an error.
    #[test]
    fn a_workspace_whose_nodes_are_this_machine_needs_no_alias() {
        let config = Config::parse(
            "this = \"solo\"\n[nodes.solo]\n\
             [workspaces.x]\nagent_node = \"solo\"\nruntime_node = \"solo\"\nroot = \"/p\"\n",
        )
        .unwrap();
        let r = config.workspace("x").unwrap();
        assert_eq!(r.topology(), Topology::Colocated);
        // Nothing to dial, and the error says so rather than blaming the
        // config for a missing field it must not have.
        let err = r.agent_ssh().unwrap_err();
        assert!(err.message().contains("this node"), "{err}");
    }

    /// `this` names the one node that does not get an alias, so an alias
    /// on it means the file was written from somebody else's point of view.
    #[test]
    fn the_node_this_machine_is_must_not_carry_an_ssh_alias() {
        let err = parse_err("this = \"a\"\n[nodes.a]\nssh = \"itself\"\n");
        assert!(err.message().contains("does not ssh to itself"), "{err}");
    }

    /// Delegating to yourself is a request that leaves and comes straight
    /// back. Caught in the file, not at 2 a.m. over ssh.
    #[test]
    fn delegating_to_this_node_is_a_config_error() {
        let err = parse_err("this = \"a\"\nruntime_node = \"a\"\n[nodes.a]\n");
        assert!(err.message().contains("is this node"), "{err}");
    }

    /// The host comes back with the alias, because `ccnm_bin` is on it.
    /// A Agent Node whose home keeps ccnm somewhere other than the
    /// default is a supported, documented config; a caller handed only the
    /// alias would silently run the default path instead and fail with
    /// "command not found" on the one machine that was configured
    /// correctly.
    #[test]
    fn the_agent_side_lookup_carries_where_ccnm_lives_over_there() {
        let config = Config::parse(
            "this = \"agent\"\nruntime_node = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\nssh = \"xdwmbp\"\nccnm_bin = \"/opt/homebrew/bin/ccnm\"\n",
        )
        .unwrap();
        let (alias, host) = config.runtime_from_agent().unwrap();
        assert_eq!(alias, "xdwmbp");
        assert_eq!(host.ccnm_bin(), "/opt/homebrew/bin/ccnm");

        // Unset still means the default, as everywhere else.
        let plain = Config::parse(AGENT_SIDE).unwrap();
        assert_eq!(
            plain.runtime_from_agent().unwrap().1.ccnm_bin(),
            DEFAULT_CCNM_BIN
        );
    }
    /// On the Agent Node, "not defined" is true and points the wrong
    /// way: it reads as "so define it", and a second copy of a project's
    /// root on this machine is the exact thing the split exists to
    /// prevent -- one of them goes stale and a session binds to a
    /// directory that moved. So the error names the machine that does
    /// keep the list. Only `doctor <ws>` and `mcp probe` can reach it;
    /// everything else on the Agent Node answers locally.
    #[test]
    fn on_the_agent_node_an_unknown_name_says_where_the_list_is() {
        let agent_side_cfg = Config::parse(AGENT_SIDE).unwrap();
        let err = agent_side_cfg.workspace("xshun").unwrap_err();
        assert!(err.message().contains("xdwmbp"), "{err}");
        assert!(
            err.message().contains("ssh xdwmbp ccnm doctor xshun"),
            "{err}"
        );

        // No way home either: nothing to point at, so do not invent one.
        let neither = Config::parse("").unwrap();
        let err = neither.workspace("xshun").unwrap_err();
        assert!(!err.message().contains("ssh "), "{err}");
        assert!(err.message().contains("no workspaces in config"), "{err}");
    }

    use crate::error::ErrorCode;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn parse_err(text: &str) -> Error {
        Config::parse(text).expect_err("config should be rejected")
    }

    /// Minimal valid config with the given workspace body appended.
    fn with_workspace(body: &str) -> String {
        format!(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\n[workspaces.x]\n{body}\n"
        )
    }

    const VALID_WS: &str = "agent_node = \"agent\"\nroot = \"/a\"";

    #[test]
    fn valid_fixture_parses_with_defaults() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        assert_eq!(config.version, Some(1));
        let r = config.workspace("xshun").unwrap();
        assert_eq!(r.agent_ssh().unwrap(), "agent-alias");
        assert_eq!(r.agent.claude_config_dir, None);
        assert_eq!(r.agent.ccnm_bin(), "~/.local/bin/ccnm");
        assert_eq!(r.runtime.ccnm_bin(), "~/.local/bin/ccnm");
        assert_eq!(r.workspace.backend, Backend::McpSsh);
        assert_eq!(r.workspace.runtime_node, "runtime");
        assert_eq!(
            r.workspace.root,
            PathBuf::from("/Users/fodelf/Projects/xshun")
        );
        assert_eq!(r.workspace.share, None);
        assert_eq!(r.workspace.runtime_root, None);
        assert_eq!(
            r.workspace.claude_permission_mode,
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn optional_host_fields_are_read() {
        let config = Config::load(&fixture("config-custom-claude-dir.toml")).unwrap();
        let r = config.workspace("xshun").unwrap();
        assert_eq!(
            r.agent.claude_config_dir,
            Some(PathBuf::from("/Users/me/.ccnm/claude"))
        );
        assert_eq!(r.agent.ccnm_bin(), "/Users/me/bin/ccnm");
        assert_eq!(r.runtime.ccnm_bin(), "/Users/ccrun/.local/bin/ccnm");
        assert_eq!(r.workspace.runtime_node, "runtime");
    }

    #[test]
    fn hybrid_fixture_parses_but_needs_its_fields() {
        let config = Config::load(&fixture("config-hybrid.toml")).unwrap();
        let ws = &config.workspaces["legacy"];
        assert_eq!(ws.backend, Backend::HybridSmb);
        assert_eq!(ws.share.as_deref(), Some("legacy"));
        assert_eq!(ws.mount_mode, Some(MountMode::Coherence));
        assert_eq!(
            ws.runtime_root,
            Some(PathBuf::from("/Users/Shared/cc-runtime/legacy"))
        );

        let err = parse_err(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\n[workspaces.x]\nbackend = \"hybrid-smb\"\nagent_node = \"agent\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("share is required"), "{msg}");
        assert!(msg.contains("runtime_root is required"), "{msg}");
        assert!(msg.contains("without `smb_user`"), "{msg}");
    }

    #[test]
    fn hybrid_fields_are_rejected_on_mcp_ssh() {
        let err = parse_err(&with_workspace(
            "agent_node = \"agent\"\nroot = \"/a\"\nshare = \"x\"\nmount_mode = \"coherence\"\nruntime_root = \"/b\"",
        ));
        let msg = err.message();
        for field in ["share", "mount_mode", "runtime_root"] {
            assert!(
                msg.contains(&format!(
                    "{field} is only valid with backend = \"hybrid-smb\""
                )),
                "{field}: {msg}"
            );
        }
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let err = parse_err(&with_workspace(&format!("{VALID_WS}\nbackend = \"nfs\"")));
        assert!(err.message().contains("nfs"), "{err}");
    }

    #[test]
    fn unknown_field_is_rejected_with_its_name() {
        let err = Config::load(&fixture("config-unknown-field.toml")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("runtime_hots"), "{err}");
    }

    #[test]
    fn missing_file_names_the_path() {
        let err = Config::load(Path::new("/nonexistent/ccnm/config.toml")).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(
            err.message().contains("/nonexistent/ccnm/config.toml"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let err = parse_err("version = 2\n");
        assert!(err.message().contains("version = 2"), "{err}");
    }

    #[test]
    fn unknown_hosts_are_rejected() {
        let err = parse_err(&with_workspace(
            "agent_node = \"nope\"\nruntime_node = \"nada\"\nroot = \"/a\"",
        ));
        let msg = err.message();
        assert!(msg.contains("agent_node = \"nope\""), "{msg}");
        assert!(msg.contains("runtime_node = \"nada\""), "{msg}");
    }

    #[test]
    fn role_specific_fields_are_required() {
        let err = parse_err(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\n[nodes.runtime]\n[workspaces.x]\nagent_node = \"agent\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(
            msg.contains("agent_node = \"agent\" is another machine"),
            "{msg}"
        );
    }

    #[test]
    fn relative_and_dotty_paths_are_rejected() {
        let err = parse_err(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\nclaude_config_dir = \"relative/dir\"\nccnm_bin = \"bin/ccnm\"\n[nodes.runtime]\n[workspaces.x]\nagent_node = \"agent\"\nroot = \"/tmp/../x\"\n",
        );
        let msg = err.message();
        assert!(
            msg.contains("claude_config_dir must be an absolute path"),
            "{msg}"
        );
        assert!(msg.contains("ccnm_bin must be an absolute path"), "{msg}");
        assert!(msg.contains("root must not contain"), "{msg}");
    }

    #[test]
    fn ccnm_bin_must_be_a_remote_safe_path() {
        let err = parse_err(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\nccnm_bin = \"/Users/me/my tools/ccnm\"\n[nodes.runtime]\n",
        );
        assert!(err.message().contains("never has to quote"), "{err}");
        let ok = Config::parse(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\nccnm_bin = \"/opt/ccnm-0.1/bin/ccnm\"\n[nodes.runtime]\n",
        )
        .unwrap();
        assert_eq!(ok.nodes["agent"].ccnm_bin(), "/opt/ccnm-0.1/bin/ccnm");
    }

    #[test]
    fn hybrid_runtime_root_inside_root_is_rejected() {
        let err = parse_err(
            "version = 1\nthis = \"runtime\"\n[nodes.agent]\nssh = \"work\"\n[nodes.runtime]\nsmb_user = \"u\"\n[workspaces.x]\nbackend = \"hybrid-smb\"\nagent_node = \"agent\"\nroot = \"/Users/Shared/cc-workspaces/x\"\nruntime_root = \"/Users/Shared/cc-workspaces/x/target\"\nshare = \"x\"\n",
        );
        assert!(err.message().contains("must not overlap root"), "{err}");
    }

    #[test]
    fn bad_names_and_tokens_are_rejected() {
        let err = parse_err(
            "version = 1\n[nodes.\"my host\"]\nssh = \"-oProxyCommand=x\"\n[nodes.runtime]\n[workspaces.\"-x\"]\nagent_node = \"my host\"\nroot = \"/a\"\n",
        );
        let msg = err.message();
        assert!(msg.contains("nodes.my host: name must be"), "{msg}");
        assert!(msg.contains("nodes.my host.ssh must match"), "{msg}");
        assert!(msg.contains("workspaces.-x: name must be"), "{msg}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let err = parse_err("version = 3\n[workspaces.x]\nagent_node = \"nope\"\nroot = \"rel\"\n");
        let lines = err.message().lines().count();
        assert!(lines >= 4, "expected several problems, got:\n{err}");
    }

    #[test]
    fn unknown_workspace_lists_defined_ones() {
        let config = Config::load(&fixture("config-valid.toml")).unwrap();
        let err = config.workspace("other").unwrap_err();
        assert_eq!(err.code(), ErrorCode::Config);
        assert!(err.message().contains("defined: xshun"), "{err}");
    }

    #[test]
    fn permission_mode_cli_values_match_claude() {
        let modes = [
            (PermissionMode::AcceptEdits, "acceptEdits"),
            (PermissionMode::Auto, "auto"),
            (PermissionMode::BypassPermissions, "bypassPermissions"),
            (PermissionMode::Manual, "manual"),
            (PermissionMode::DontAsk, "dontAsk"),
            (PermissionMode::Plan, "plan"),
        ];
        for (mode, text) in modes {
            assert_eq!(mode.as_cli_value(), text);
            let toml = with_workspace(&format!("{VALID_WS}\nclaude_permission_mode = \"{text}\""));
            let config = Config::parse(&toml).unwrap();
            assert_eq!(config.workspaces["x"].claude_permission_mode, mode);
        }
    }
}

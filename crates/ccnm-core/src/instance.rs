//! Public Agent Instance identity and Agent-local profile resolution.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::{Backend, Config, PermissionMode};
use crate::error::{Error, ErrorCode, Result};
use crate::provider::AgentProvider;

pub mod profiles;
pub use profiles::{AgentProfiles, ResolvedProfile};

pub const INSTANCE_SESSION_PROTOCOL: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRef {
    pub node: String,
    pub instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInstance {
    pub provider: AgentProvider,
    pub profile_ref: String,
    /// Which model this instance runs, when the official CLI takes one.
    ///
    /// Agent-local, like `provider` and the profile: it never enters
    /// [`AgentIdentity`], the binding, or any wire message, and the Runtime
    /// has no opinion about it. The supervisor reads it from this registry
    /// again at launch, next to the profile directory.
    ///
    /// Codex only. ccnm starts Codex with `--ignore-user-config`, so the
    /// model in the CLI's own config file is deliberately not consulted --
    /// which left no way at all to choose one until this field existed.
    /// Claude's model selection is part of its own configuration and is not
    /// something ccnm passes, so setting this on a Claude instance is
    /// refused rather than ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentity {
    pub node: String,
    pub instance: String,
    pub provider: AgentProvider,
    pub profile_ref: String,
}

/// An immutable snapshot from the Runtime's workspace definition, not an
/// independently editable config on the Agent. Each side must verify it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub workspace: String,
    pub runtime_node: String,
    pub root: PathBuf,
    pub agent: AgentIdentity,
}

impl WorkspaceBinding {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.workspace)?;
        identifier(&self.runtime_node)?;
        self.agent.validate()?;
        if !profiles::absolute(&self.root) || self.runtime_node == self.agent.node {
            return Err(Error::config(
                "binding needs an absolute Runtime root and a supported remote topology",
            ));
        }
        Ok(())
    }
}

/// Technical support observed through the existing internal provider paths.
/// This is not authentication, production readiness or permission to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub ssh_mcp: bool,
    pub print: bool,
    pub interactive: bool,
    pub native_colocated: bool,
}

impl AgentProvider {
    pub fn instance_capabilities(self) -> Capabilities {
        match self {
            Self::Claude | Self::Codex => Capabilities {
                ssh_mcp: true,
                print: true,
                interactive: true,
                native_colocated: false,
            },
        }
    }
}

impl InstanceRef {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.node)?;
        identifier(&self.instance)
    }
}

impl AgentIdentity {
    pub fn reference(&self) -> InstanceRef {
        InstanceRef {
            node: self.node.clone(),
            instance: self.instance.clone(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.reference().validate()?;
        identifier(&self.profile_ref)
    }
}

/// Deliberately not Serialize or Debug: private paths cannot accidentally
/// become protocol or diagnostic fields alongside the public identity.
pub struct ResolvedAgent {
    identity: AgentIdentity,
    profile: ResolvedProfile,
    model: Option<String>,
}

pub struct AgentLocal {
    profiles: AgentProfiles,
    home: PathBuf,
    xdg: Option<PathBuf>,
}

impl AgentLocal {
    pub fn new(profiles: AgentProfiles, home: PathBuf, xdg: Option<PathBuf>) -> Result<Self> {
        if !profiles::absolute(&home)
            || xdg
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty())
                .is_some_and(|path| !profiles::absolute(path))
        {
            return Err(Error::config(
                "Agent-local HOME/XDG path is invalid (value withheld)",
            ));
        }
        Ok(Self {
            profiles,
            home,
            xdg,
        })
    }

    pub fn load() -> Result<Self> {
        let home = crate::paths::home_dir()?;
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self::new(AgentProfiles::load_local()?, home, xdg)
    }

    pub fn resolve(&self, config: &Config, reference: &InstanceRef) -> Result<ResolvedAgent> {
        config.resolve_instance(reference, &self.profiles, &self.home, self.xdg.as_deref())
    }
}
impl ResolvedAgent {
    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }
    pub fn profile(&self) -> &ResolvedProfile {
        &self.profile
    }
    /// The model this instance declares, if any. Agent-local: resolved here
    /// and consumed at launch, never sent anywhere.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Validate a node, instance or profile reference before it crosses an argv
/// or protocol boundary.
pub fn identifier(value: &str) -> Result<()> {
    if value.len() > 64
        || value.is_empty()
        || !value.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::config(
            "instance, node and profile references must be 1..64 characters of [A-Za-z0-9][A-Za-z0-9_-]* (value withheld)",
        ));
    }
    Ok(())
}

pub(crate) fn validate_config(config: &Config, problems: &mut Vec<String>) {
    for (name, instance) in &config.agents {
        if identifier(name).is_err() || identifier(&instance.profile_ref).is_err() {
            problems.push("invalid local Agent instance/profile reference".into());
        }
        // Codex takes `--model`; ccnm passes nothing of the sort to Claude,
        // whose model is part of its own configuration. Accepting the field
        // there would set something no launch ever reads.
        if instance.model.is_some() && instance.provider != AgentProvider::Codex {
            problems.push(format!(
                "agents.{name}.model is only supported by Codex instances"
            ));
        }
        if instance
            .model
            .as_deref()
            .is_some_and(|model| model.is_empty() || !crate::ssh::is_remote_safe(model))
        {
            problems.push(format!(
                "agents.{name}.model must be a non-empty value safe to pass on a command line"
            ));
        }
    }
    for (name, ws) in &config.workspaces {
        match &ws.agent {
            None if ws.agent_node.is_empty() => {
                problems.push(format!("workspaces.{name} requires agent_node or agent"))
            }
            Some(reference) => {
                if identifier(name).is_err() {
                    problems
                        .push("instance workspace names must be valid bounded identifiers".into());
                }
                if config.this.as_deref() != Some(ws.runtime_node.as_str()) {
                    problems.push(format!(
                        "workspaces.{name} instance root must be defined only on its Runtime Node"
                    ));
                }
                if reference.validate().is_err() {
                    problems.push(format!(
                        "workspaces.{name}.agent is not a valid node/instance reference"
                    ));
                }
                if !ws.agent_node.is_empty() {
                    problems.push(format!(
                        "workspaces.{name} cannot combine agent with legacy agent_node"
                    ));
                }
                if ws.claude_permission_mode != PermissionMode::default() {
                    problems.push(format!("workspaces.{name} cannot configure instance policy through claude_permission_mode"));
                }
                if config
                    .nodes
                    .get(&reference.node)
                    .is_some_and(|node| node.claude_config_dir.is_some())
                {
                    problems.push(format!("workspaces.{name} instance selection conflicts with legacy claude_config_dir; configure profiles on the Agent instead"));
                }
                if reference.node == ws.runtime_node || ws.backend != Backend::McpSsh {
                    problems.push(format!("workspaces.{name} instance mode is unsupported: only non-colocated SSH MCP has been measured"));
                }
                if config.this.as_deref() == Some(reference.node.as_str())
                    && !config.agents.contains_key(&reference.instance)
                {
                    problems.push(format!(
                        "workspaces.{name} references an unknown local instance"
                    ));
                }
            }
            None => {}
        }
    }
}

impl Config {
    /// Runtime authority only; this never loads profiles or assumes a remote
    /// registry exists. Unknown remote instances fail at the Agent resolver.
    pub fn instance_reference(&self, workspace: &str) -> Result<InstanceRef> {
        let ws = self
            .workspaces
            .get(workspace)
            .ok_or_else(|| Error::config("unknown workspace"))?;
        if self.this.as_deref() != Some(ws.runtime_node.as_str()) {
            return Err(Error::config(
                "only the authoritative Runtime may select a workspace instance",
            ));
        }
        ws.agent
            .clone()
            .ok_or_else(|| Error::config("workspace uses legacy agent_node, not an instance"))
    }

    pub fn resolve_identity(&self, reference: &InstanceRef) -> Result<AgentIdentity> {
        reference.validate()?;
        if self.this.as_deref() != Some(reference.node.as_str()) {
            return Err(Error::config("instance reference names another Agent Node"));
        }
        let instance = self
            .agents
            .get(&reference.instance)
            .ok_or_else(|| Error::config("unknown local Agent instance"))?;
        let identity = AgentIdentity {
            node: reference.node.clone(),
            instance: reference.instance.clone(),
            provider: instance.provider,
            profile_ref: instance.profile_ref.clone(),
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Pure planning; home/xdg are supplied by the Agent's local context.
    /// No directory is created or authenticated by this method.
    pub fn resolve_instance(
        &self,
        reference: &InstanceRef,
        profiles: &AgentProfiles,
        home: &Path,
        xdg: Option<&Path>,
    ) -> Result<ResolvedAgent> {
        let identity = self.resolve_identity(reference)?;
        let profile = profiles.resolve(identity.provider, &identity.profile_ref, home, xdg)?;
        let model = self
            .agents
            .get(&reference.instance)
            .and_then(|instance| instance.model.clone());
        Ok(ResolvedAgent {
            identity,
            profile,
            model,
        })
    }

    /// Check node authority before opening any Agent-private configuration.
    pub fn resolve_instance_local(&self, reference: &InstanceRef) -> Result<ResolvedAgent> {
        AgentLocal::load()?.resolve(self, reference)
    }

    pub fn bind_workspace(
        &self,
        workspace: &str,
        identity: &AgentIdentity,
    ) -> Result<WorkspaceBinding> {
        identity.validate()?;
        let expected = self.instance_reference(workspace)?;
        // The node, and only the node. Picking another instance on the same
        // Agent Node is a supported public override (`ccnm run --agent`),
        // so the Runtime authorizes the machine and leaves the instance to
        // the Agent's own registry, which is the authority for it.
        if expected.node != identity.node {
            return Err(Error::config(
                "Agent identity node does not match the Runtime workspace reference",
            ));
        }
        let ws = &self.workspaces[workspace];
        if ws.backend != Backend::McpSsh
            || expected.node == ws.runtime_node
            || !identity.provider.instance_capabilities().ssh_mcp
        {
            return Err(Error::new(
                ErrorCode::NotReady,
                "unsupported instance capability/topology",
            ));
        }
        let binding = WorkspaceBinding {
            workspace: workspace.into(),
            runtime_node: ws.runtime_node.clone(),
            root: ws.root.clone(),
            agent: identity.clone(),
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn verify_runtime_binding(&self, binding: &WorkspaceBinding) -> Result<()> {
        binding.validate()?;
        if self.bind_workspace(&binding.workspace, &binding.agent)? != *binding {
            return Err(Error::config(
                "binding differs from the authoritative Runtime workspace",
            ));
        }
        Ok(())
    }

    pub fn resolve_bound_instance(
        &self,
        binding: &WorkspaceBinding,
        profiles: &AgentProfiles,
        home: &Path,
        xdg: Option<&Path>,
    ) -> Result<ResolvedAgent> {
        binding.validate()?;
        identifier(&binding.runtime_node)?;
        // runtime_node is the Agent CLI's default delegation target, not a
        // second workspace registry or an allow-list of exactly one Runtime.
        if !self
            .nodes
            .get(&binding.runtime_node)
            .is_some_and(|node| node.ssh.is_some())
        {
            return Err(Error::config(
                "binding names a Runtime not reachable through this Agent's local nodes",
            ));
        }
        let resolved = self.resolve_instance(&binding.agent.reference(), profiles, home, xdg)?;
        if resolved.identity != binding.agent {
            return Err(Error::config(
                "Agent registry identity changed or the binding was substituted",
            ));
        }
        Ok(resolved)
    }
}

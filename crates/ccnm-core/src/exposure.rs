//! `~/.agents/mcp.json`: the tools and skills this Runtime's owner has
//! turned off (P47, toexec RFC-0001).
//!
//! Read from the HOME of the account the tools run as -- the Runtime
//! Executor -- because that is the process serving them. It only ever
//! narrows: `external_mcp`, the exec gate and a skill's own frontmatter
//! decide the most a session may have, and this file takes more away.
//!
//! It is **not** a restraint on the model. The executor account can write
//! its own HOME, and `exec_command` runs as that account, so a model that
//! may run commands may also edit this file for the next session -- the
//! same as it may edit `~/.config/ccnm/config.toml`. What stops a model is
//! the exec gate and `external_mcp`; this file is about keeping a session's
//! surface small.

use std::path::PathBuf;

use toexec_agents::{Policy, Rules, ToolRule};

use crate::error::{Error, Result};
use crate::mcp::server::SERVER_NAME;
use crate::session::MCP_TOOLS;

/// Where the file is for the account running this process.
pub fn path() -> Result<PathBuf> {
    Ok(toexec_agents::path_in(&crate::paths::home_dir()?))
}

/// The rules, or why they cannot be read. A missing file is no rules; a
/// broken one is an error, because treating it as missing would turn back
/// on exactly the tools somebody wrote it to turn off.
pub fn load() -> Result<Policy> {
    Policy::load(&path()?).map_err(|e| {
        Error::config(format!(
            "{e}; fix it or move it away (without it nothing is turned off)"
        ))
    })
}

/// This server's view of the file: `mcpServers.ccnm` plus the top-level
/// `skillOverrides`.
pub fn rules(policy: &Policy) -> Rules<'_> {
    policy.for_server(SERVER_NAME)
}

/// Names in `enabledTools` / `disabledTools` that are not tools of this
/// server. One in `disabledTools` means a tool somebody meant to turn off
/// is still on, so it is reported as a problem, not ignored.
pub fn unknown_tools(policy: &Policy) -> Vec<String> {
    rules(policy).unknown_tools(&MCP_TOOLS)
}

/// The tools this file takes away, each with the rule that did it.
pub fn hidden_tools(policy: &Policy) -> Vec<(&'static str, ToolRule)> {
    let rules = rules(policy);
    MCP_TOOLS
        .iter()
        .filter_map(|tool| rules.tool_rule(tool).map(|rule| (*tool, rule)))
        .collect()
}

/// Why a tool was refused, in the words a person can act on.
pub fn refusal(tool: &str, rule: ToolRule) -> String {
    let (key, how) = match rule {
        ToolRule::NotEnabled => ("enabledTools", "is not listed in"),
        ToolRule::Disabled => ("disabledTools", "is listed in"),
    };
    format!(
        "{tool} is turned off on this Runtime: it {how} mcpServers.{SERVER_NAME}.{key} in ~/.agents/mcp.json"
    )
}

/// One paragraph for `workspace_info`, or nothing when the file changes
/// nothing about tools.
pub fn summary(policy: &Policy) -> Option<String> {
    let hidden = hidden_tools(policy);
    let unknown = unknown_tools(policy);
    if hidden.is_empty() && unknown.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if !hidden.is_empty() {
        let names: Vec<&str> = hidden.iter().map(|(tool, _)| *tool).collect();
        lines.push(format!(
            "turned off by ~/.agents/mcp.json: {}",
            names.join(", ")
        ));
    }
    if !unknown.is_empty() {
        lines.push(format!(
            "names in ~/.agents/mcp.json that are not tools here: {}",
            unknown.join(", ")
        ));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str) -> Policy {
        Policy::parse(text).unwrap()
    }

    #[test]
    fn only_the_ccnm_entry_decides_tools() {
        let p = policy(
            r#"{"mcpServers": {"ccnm": {"disabledTools": ["exec_command", "exec_comand"]},
                                "gld": {"disabledTools": ["read_file"]}}}"#,
        );
        assert_eq!(hidden_tools(&p), vec![("exec_command", ToolRule::Disabled)]);
        assert_eq!(unknown_tools(&p), vec!["exec_comand".to_string()]);
        let text = summary(&p).unwrap();
        assert!(
            text.contains("turned off by ~/.agents/mcp.json: exec_command"),
            "{text}"
        );
        assert!(text.contains("exec_comand"), "{text}");
        assert!(
            refusal("exec_command", ToolRule::Disabled).contains("mcpServers.ccnm.disabledTools")
        );
        assert_eq!(summary(&policy("{}")), None);
    }

    #[test]
    fn an_allow_list_names_what_stays() {
        let p =
            policy(r#"{"mcpServers": {"ccnm": {"enabledTools": ["read_file", "load_skill"]}}}"#);
        let hidden: Vec<&str> = hidden_tools(&p).into_iter().map(|(t, _)| t).collect();
        assert_eq!(hidden.len(), MCP_TOOLS.len() - 2);
        assert!(!hidden.contains(&"read_file") && !hidden.contains(&"load_skill"));
        assert!(
            refusal("exec_command", ToolRule::NotEnabled)
                .contains("not listed in mcpServers.ccnm.enabledTools")
        );
    }
}

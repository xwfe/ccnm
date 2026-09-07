//! Claude-specific MCP document and tool permission injection.
use crate::error::Result;
use crate::mcp::server::SERVER_NAME;
use crate::process::Cmd;
use crate::session::{Dir, MCP_TOOLS, SSH_BIN, pretty};

/// Built-in tools that must never be available in a ccnm session (design
/// doc section 13). `--tools ""` already removes every built-in tool; this
/// deny list is the second lock, so that a future Claude that reads
/// `--tools` differently still cannot hand the model this machine's disk.
pub const NATIVE_TOOLS_DENIED: [&str; 6] = ["Read", "Edit", "Write", "Grep", "Glob", "Bash"];

pub fn mcp_config(cmd: &Cmd) -> serde_json::Value {
    let args: Vec<String> = cmd
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": SSH_BIN,
                "args": args,
            }
        }
    })
}

/// The `--settings` file: permission to call each ccnm tool without a
/// prompt (there is nobody to answer one in print mode), and the native
/// file and shell tools denied by name. Nothing else — the user's own
/// settings still load underneath this (design doc section 24).
///
/// `remote` is false for a colocated workspace, and then neither half
/// applies: there are no ccnm tools to allow, and denying the native ones
/// would leave Claude with no way to touch the project it is sitting on.
/// The deny list exists to stop the model reaching *this* machine's disk
/// when the project is on another one; when the project is this machine's
/// disk, it would only be in the way.
pub fn settings(remote: bool) -> serde_json::Value {
    if !remote {
        return serde_json::json!({ "permissions": {} });
    }
    let allow: Vec<String> = MCP_TOOLS
        .iter()
        .map(|t| format!("mcp__{SERVER_NAME}__{t}"))
        .collect();
    serde_json::json!({
        "permissions": {
            "allow": allow,
            "deny": NATIVE_TOOLS_DENIED,
        }
    })
}

pub(crate) fn write_session_files(dir: &Dir, transport: Option<&Cmd>) -> Result<()> {
    if let Some(cmd) = transport {
        std::fs::write(dir.mcp_config(), pretty(&mcp_config(cmd))?)?;
    }
    std::fs::write(dir.settings(), pretty(&settings(transport.is_some()))?)?;
    Ok(())
}

pub fn transport_payload(dir: &Dir) -> Option<String> {
    let text = std::fs::read_to_string(dir.mcp_config()).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let args = config
        .pointer(&format!("/mcpServers/{}/args", SERVER_NAME))?
        .as_array()?;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg.as_str() == Some("--payload") {
            return it.next()?.as_str().map(str::to_string);
        }
    }
    None
}

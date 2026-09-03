//! The versioned control protocol between the two ccnm binaries (design
//! doc section 8). This is *not* MCP: it only carries the launcher's
//! hello / probe / session-setup requests over an ssh command line. MCP
//! JSON-RPC goes straight through the ssh stdio once `mcp-serve` is up
//! (section 9).

pub mod hello;
pub mod mcp;
pub mod payload;
pub mod probe;

pub use payload::{PROTOCOL, Protocol};

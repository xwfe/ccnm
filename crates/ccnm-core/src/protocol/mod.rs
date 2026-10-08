//! The versioned control protocol between the two ccnm binaries. It is
//! internal, not a public contract: docs/protocol/machine-protocol-v1.md
//! section 11 sets the two apart. This is *not* MCP either: it only
//! carries the launcher's hello / probe / session-setup requests over an
//! ssh command line. MCP JSON-RPC goes straight through the ssh stdio once
//! `mcp-serve` is up.

pub mod hello;
pub mod mcp;
pub mod payload;
pub mod probe;
pub mod run;

pub use payload::{PROTOCOL, Protocol};

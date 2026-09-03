//! The MCP side of ccnm: the stdio server that runs on the home machine
//! (`ccnm internal mcp-serve`) and the client used to probe it.
//!
//! This is the only async code in the binary. Both entry points build a
//! current-thread tokio runtime, do their work inside one `block_on`, and
//! hand a plain `Result` back to synchronous callers (design doc section
//! 25). MCP JSON-RPC goes straight over stdin/stdout; the control
//! protocol's base64 payload is consumed once, before the first byte of
//! MCP (section 9).

pub mod probe;
pub mod server;

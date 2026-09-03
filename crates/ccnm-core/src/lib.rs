//! Core logic shared by every ccnm role (home launcher, work controller,
//! home MCP runtime). The CLI crate is a thin argument parser over this.

pub mod claude;
pub mod config;
pub mod controller;
pub mod doctor;
pub mod error;
pub mod launchagent;
pub mod launcher;
pub mod mcp;
pub mod paths;
pub mod process;
pub mod protocol;
pub mod safety;
pub mod ssh;
pub mod work;

pub use config::Config;
pub use error::{Error, ErrorCode, Result};
pub use process::{Cmd, Output, ProcessRunner, SystemRunner};

/// Version of the ccnm binary, shared by all crates in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! Core logic shared by every ccnm role (home launcher, controller,
//! home MCP runtime). The CLI crate is a thin argument parser over this.

pub mod claude;
pub mod cleanup;
pub mod config;
pub mod configedit;
pub mod controller;
pub mod doctor;
pub mod error;
pub mod instance;
pub mod lang;
pub mod launchagent;
pub mod launcher;
pub mod mcp;
pub mod overview;
pub mod paths;
mod platform;
pub mod process;
pub mod protocol;
pub mod provider;
pub mod quota;
pub mod rpc;
pub mod runtime;
pub mod safety;
pub mod session;
pub mod ssh;
pub mod systemd;
pub mod tmux;
pub mod work;

pub use config::Config;
pub use error::{Error, ErrorCode, Result};
pub use lang::Lang;
pub use platform::{keep_std_handles_to_this_process, reported_path};
pub use process::{Cmd, Output, ProcessRunner, SystemRunner};

/// Version of the ccnm binary, shared by all crates in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

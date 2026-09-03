//! Core logic shared by every ccnm role (home launcher, work controller,
//! home runner). The CLI crate is a thin argument parser over this.

pub mod claude;
pub mod config;
pub mod doctor;
pub mod error;
pub mod home;
pub mod identity;
pub mod paths;
pub mod payload;
pub mod process;
pub mod runner;
pub mod smb;
pub mod ssh;
pub mod tailscale;
pub mod work;

pub use config::Config;
pub use error::{Error, ErrorCode, Result};
pub use process::{Cmd, Output, ProcessRunner, SystemRunner};

/// Version of the ccnm binary, shared by all crates in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

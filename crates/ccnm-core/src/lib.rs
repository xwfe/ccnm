//! Core logic shared by every ccnm role (home launcher, work controller,
//! home runner). The CLI crate is a thin argument parser over this.

pub mod config;
pub mod error;
pub mod paths;
pub mod process;

pub use config::Config;
pub use error::{Error, ErrorCode, Result};
pub use process::{Cmd, Output, ProcessRunner, SystemRunner};

/// Version of the ccnm binary, shared by all crates in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

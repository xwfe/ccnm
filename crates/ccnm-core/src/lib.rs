//! Core logic shared by every ccnm role (home launcher, work controller,
//! home runner). The CLI crate is a thin argument parser over this.

/// Version of the ccnm binary, shared by all crates in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

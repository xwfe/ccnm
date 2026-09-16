//! Codex's own execution tools, run by the official `codex exec-server` on
//! the Runtime instead of through ccnm's seven MCP tools (P21–P24).
//!
//! The design and why each piece sits where it does is
//! `docs/plan/runtime-surfaces.md` section 12. In one line: exec-server is
//! not a boundary -- it does whatever sandbox the client sends -- so every
//! request is checked here first ([`policy`]), and the process is run and
//! outlived by a ccnm supervisor that holds the workspace write guard.

pub mod policy;
pub mod serve;

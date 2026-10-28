//! harnesscfg seeds and syncs the writable configuration files of AI coding
//! agent harnesses (Claude Code, Codex, OpenCode, OMP, …) from Nix templates.
//!
//! See `README.md` for the model; `docs/ARCHITECTURE.md` for the internals.

pub mod apply;
pub mod cli;
pub mod diff;
pub mod error;
pub mod format;
pub mod fsops;
pub mod lock;
pub mod manifest;
pub mod merge;
pub mod plan;
pub mod registry;

//! Subcommand implementations. Phase 0: most are stubs that compile and report
//! intent. `context` and `version` are fully implemented; `status` is partial.

pub mod context;
pub mod cwd_list;
pub mod diff;
pub mod install;
pub mod logs;
pub mod pair;
pub mod serve;
pub mod status;
pub mod uninstall;
pub mod usage;
pub mod version;

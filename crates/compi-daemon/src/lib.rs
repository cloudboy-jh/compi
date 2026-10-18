#[cfg(windows)]
pub mod conpty;
mod content_files;
pub mod daemon;
pub mod file_tree;
mod git;
pub mod launch;
pub mod metadata;
mod program_status;
mod prompt;
pub mod pty;
pub mod screen;
mod shell_integration;
#[cfg(windows)]
pub mod supervisor;
pub mod surface;
mod system_metrics;
pub mod terminal;
pub mod workspace;
mod workspace_store;
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(windows)]
pub mod conpty;
pub mod daemon;
pub mod file_tree;
pub mod launch;
pub mod metadata;
mod prompt;
pub mod pty;
pub mod screen;
mod shell_integration;
#[cfg(windows)]
pub mod supervisor;
pub mod surface;
pub mod terminal;
pub mod workspace;
mod workspace_store;
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

//! Client-side daemon transport, terminal replicas, and interaction logic.

pub mod client_state;
pub mod commands;
pub mod config;
pub mod connection;
#[cfg(windows)]
pub mod console;
pub mod font_catalog;
#[cfg(any(windows, target_os = "macos"))]
pub mod gui;
#[cfg(any(windows, target_os = "macos"))]
pub mod image_input;
pub mod input;
pub mod layout;
pub mod probe;
pub mod project_history;
mod replica;
pub mod selection;
pub mod theme;
pub mod theme_file;
mod theme_migration;
pub mod theme_store;
#[cfg(any(windows, target_os = "macos"))]
pub mod typography;
#[cfg(any(windows, target_os = "macos"))]
pub mod update_restore;
pub mod updates;
pub mod viewport;
#[cfg(any(windows, target_os = "macos"))]
pub mod window_host;

pub use compi_protocol::{DaemonClient, ServerEvent};
pub use replica::{MirrorApply, ScreenMirror};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

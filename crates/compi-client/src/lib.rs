//! Client-side daemon transport, terminal replicas, and interaction logic.

pub mod config;
#[cfg(windows)]
pub mod console;
#[cfg(any(windows, target_os = "macos"))]
pub mod gui;
pub mod input;
pub mod probe;
mod replica;
pub mod selection;
pub mod theme;
#[cfg(any(windows, target_os = "macos"))]
pub mod typography;
pub mod viewport;

pub use compi_protocol::{DaemonClient, ServerEvent};
pub use replica::{MirrorApply, ScreenMirror};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

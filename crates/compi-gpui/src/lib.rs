pub mod config;
#[cfg(any(windows, target_os = "macos"))]
pub mod typography;

#[cfg(any(windows, target_os = "macos"))]
pub mod gui;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

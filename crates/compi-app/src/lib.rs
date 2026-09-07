pub mod config;
#[cfg(any(windows, target_os = "macos"))]
pub mod typography;

#[cfg(any(windows, target_os = "macos"))]
pub mod gui;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

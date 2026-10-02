#[cfg(windows)]
pub mod installer;
#[cfg(windows)]
mod msi_actions;
#[cfg(windows)]
mod transaction;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

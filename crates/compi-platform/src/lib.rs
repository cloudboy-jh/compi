#[cfg_attr(unix, path = "identity_unix.rs")]
pub mod identity;
pub mod paths;
pub mod perf;
pub mod pipe;
#[cfg(windows)]
pub mod supervisor;
#[cfg(windows)]
pub mod wsl;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

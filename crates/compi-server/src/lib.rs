pub mod client;
#[cfg(windows)]
pub mod conpty;
#[cfg(windows)]
pub mod console;
pub mod daemon;
#[cfg_attr(unix, path = "identity_unix.rs")]
pub mod identity;
pub mod launch;
pub mod paths;
pub mod perf;
pub mod pipe;
pub mod probe;
pub mod pty;
pub mod screen;
pub mod session;
mod session_store;
#[cfg(windows)]
pub mod supervisor;
#[cfg(windows)]
pub mod wsl;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

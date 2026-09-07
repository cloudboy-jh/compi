//! Client-side daemon transport, terminal replicas, and interaction logic.

mod connection;
#[cfg(windows)]
pub mod console;
pub mod input;
pub mod probe;
mod replica;
pub mod selection;
pub mod theme;
pub mod viewport;

pub use connection::{DaemonClient, ServerEvent};
pub use replica::{MirrorApply, ScreenMirror};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

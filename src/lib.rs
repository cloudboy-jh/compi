#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod conpty;
#[cfg(windows)]
pub mod console;
#[cfg(windows)]
pub mod daemon;
pub mod frame;
#[cfg(windows)]
pub mod gui;
#[cfg(windows)]
pub mod identity;
#[cfg(windows)]
pub mod installer;
#[cfg(windows)]
pub mod perf;
#[cfg(windows)]
pub mod pipe;
pub mod protocol;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
mod session_store;
#[cfg(windows)]
pub mod supervisor;
pub mod terminal;
pub mod terminal_trace;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
pub mod wsl;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

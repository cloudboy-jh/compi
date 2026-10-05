//! `cargo dev`: rebuild the native client on save and swap it into a preview that stays
//! attached to an isolated development daemon, so dev shells survive every UI change.

mod cargo;
pub mod classify;
pub mod controller;
pub mod layout;
mod process;
mod runtime;
pub mod watch;

pub use runtime::{Options, run};

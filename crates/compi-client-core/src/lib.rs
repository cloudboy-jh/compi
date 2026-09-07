//! Disposable terminal replicas and pure client interaction, without platform I/O.
pub mod input;
mod replica;
pub mod selection;
pub mod theme;
pub mod viewport;

pub use replica::{MirrorApply, ScreenMirror};

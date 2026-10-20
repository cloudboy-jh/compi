//! The host's GPU for `TotalsSample::gpu`: the accelerator's statistics from IOKit on
//! macOS, performance counters on Windows, and nothing the daemon can read elsewhere.

#[cfg(any(windows, test))]
mod counters;
#[cfg(target_os = "macos")]
mod iokit;
#[cfg(windows)]
mod pdh;

#[cfg(target_os = "macos")]
pub use iokit::GpuReader;
#[cfg(windows)]
pub use pdh::GpuReader;

/// Linux and the BSDs have no vendor-neutral interface for GPU load.
#[cfg(not(any(target_os = "macos", windows)))]
pub struct GpuReader;

#[cfg(not(any(target_os = "macos", windows)))]
impl GpuReader {
    pub fn new() -> Self {
        Self
    }

    pub fn read(&mut self) -> Option<compi_protocol::system_metrics::GpuTotals> {
        None
    }
}

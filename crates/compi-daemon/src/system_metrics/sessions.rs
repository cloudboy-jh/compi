//! Which Compi surface a process runs in: on Unix, the PTY's session it belongs to; on
//! Windows, the job it is in; in WSL, the session its surface's shell leads.

use crate::surface::{Surface, live_surfaces};
use compi_protocol::SurfaceId;
use std::sync::Arc;

/// The current surfaces, read once for the processes a sample finds new.
pub struct SurfaceSessions(Vec<Arc<Surface>>);

impl SurfaceSessions {
    pub fn current() -> Self {
        Self(live_surfaces())
    }

    /// The surface whose PTY session the host process `pid` is in.
    #[cfg(unix)]
    pub fn surface_of(&self, pid: u32) -> Option<SurfaceId> {
        if self.0.is_empty() {
            return None;
        }
        // PID 0 would ask for this daemon's own session.
        let pid = libc::pid_t::try_from(pid).ok().filter(|&pid| pid > 0)?;
        let session = unsafe { libc::getsid(pid) };
        self.0
            .iter()
            .find(|surface| session > 0 && surface.session_id() == session)
            .map(|surface| surface.surface_id().clone())
    }

    /// The surface whose job the host process `pid` is in.
    #[cfg(windows)]
    pub fn surface_of(&self, pid: u32) -> Option<SurfaceId> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

        if self.0.is_empty() {
            return None;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
        self.0
            .iter()
            .find(|surface| surface.contains_process(&process))
            .map(|surface| surface.surface_id().clone())
    }

    /// The surfaces whose shell runs in WSL `distribution`, by that shell's PID, which is
    /// the session ID of every process the terminal runs there.
    #[cfg(windows)]
    pub fn wsl_shells(&self, distribution: &str) -> std::collections::HashMap<u32, SurfaceId> {
        self.0
            .iter()
            .filter_map(|surface| {
                let (shell_distribution, shell_pid) = surface.wsl_shell()?;
                (shell_distribution == distribution)
                    .then(|| (shell_pid, surface.surface_id().clone()))
            })
            .collect()
    }
}

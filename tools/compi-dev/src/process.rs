//! Platform process control for the preview client and dev daemon.

use std::io;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_DONE: AtomicBool = AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Acquire)
}

/// Tell a console-close handler that cleanup finished and the process may end.
pub fn shutdown_done() {
    SHUTDOWN_DONE.store(true, Ordering::Release);
}

/// Wait for `child` to exit, polling so it works for any child on every platform.
pub fn wait_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Ask a GUI process to close its windows so it flushes window state, then make sure it
/// is gone. Never used on the daemon.
pub fn close_child(child: &mut Child, grace: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    if request_close(child.id()) && wait_child(child, grace) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Same as [`close_child`] for a process this runner did not spawn.
pub fn close_pid(pid: u32, grace: Duration) {
    if request_close(pid) && wait_pid(pid, grace) {
        return;
    }
    terminate(pid);
    wait_pid(pid, Duration::from_secs(5));
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::{OsStr, c_void};
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Stdio;
    use windows::Win32::Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HWND,
        LPARAM, WAIT_OBJECT_0, WPARAM,
    };
    use windows::Win32::System::Console::{CTRL_C_EVENT, SetConsoleCtrlHandler};
    use windows::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
        PROCESS_CREATE_PROCESS, PROCESS_DUP_HANDLE, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetShellWindow, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
        WM_CLOSE,
    };
    use windows::core::{BOOL, PCWSTR, PWSTR};

    unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
        INTERRUPTED.store(true, Ordering::Release);
        if event != CTRL_C_EVENT {
            // Ctrl+Break, console close, logoff: Windows ends the process when this
            // returns, so give the main loop a moment to close the preview.
            let deadline = Instant::now() + Duration::from_millis(4500);
            while !SHUTDOWN_DONE.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        true.into()
    }

    pub fn install_interrupt_handler() {
        let _ = unsafe { SetConsoleCtrlHandler(Some(on_console_event), true) };
    }

    /// A running daemon this runner started.
    pub struct DaemonProcess {
        pid: u32,
        handle: OwnedHandle,
    }

    impl DaemonProcess {
        pub fn id(&self) -> u32 {
            self.pid
        }

        pub fn exit_status(&mut self) -> Option<String> {
            let handle = HANDLE(self.handle.as_raw_handle());
            if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0;
            let _ = unsafe { GetExitCodeProcess(handle, &mut code) };
            Some(format!("exit code {code:#x}"))
        }

        /// The daemon keeps running after the runner exits.
        pub fn release(self) {}
    }

    /// Start the daemon with no console and outside Cargo's job object. `cargo run`
    /// kills its job when Cargo dies abnormally (for example, the terminal tab closes);
    /// the dev daemon and its shells must outlive that, like a normally started daemon.
    /// The daemon is created as a child of the desktop shell process, which is how a
    /// process leaves a job that does not allow breakaway.
    pub fn spawn_daemon(
        program: &Path,
        args: &[&str],
        directory: &Path,
        log: File,
    ) -> io::Result<DaemonProcess> {
        match spawn_under_shell(program, args, directory, &log) {
            Ok(process) => Ok(process),
            // No desktop shell to adopt the daemon (e.g. a service session): start it
            // like the client does. It then ends if Cargo is killed.
            Err(_) => {
                let child = Command::new(program)
                    .args(args)
                    .current_dir(directory)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone()?)
                    .stderr(log)
                    .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
                    .spawn()?;
                let pid = child.id();
                let handle: OwnedHandle = child.into();
                Ok(DaemonProcess { pid, handle })
            }
        }
    }

    fn spawn_under_shell(
        program: &Path,
        args: &[&str],
        directory: &Path,
        log: &File,
    ) -> io::Result<DaemonProcess> {
        let shell_window = unsafe { GetShellWindow() };
        if shell_window.is_invalid() {
            return Err(io::Error::other("no desktop shell window"));
        }
        let mut shell_pid = 0;
        unsafe { GetWindowThreadProcessId(shell_window, Some(&mut shell_pid)) };
        let shell = unsafe {
            OpenProcess(
                PROCESS_CREATE_PROCESS | PROCESS_DUP_HANDLE,
                false,
                shell_pid,
            )
        }?;
        let shell = unsafe { OwnedHandle::from_raw_handle(shell.0) };
        let shell_handle = HANDLE(shell.as_raw_handle());

        // Standard handles must be valid in the adopting parent: lend it the log handle.
        let mut lent = HANDLE::default();
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(log.as_raw_handle()),
                shell_handle,
                &mut lent,
                0,
                true,
                DUPLICATE_SAME_ACCESS,
            )
        }?;
        let result = create_with_parent(program, args, directory, shell_handle, lent);
        // Take the lent handle back out of the shell process whatever happened.
        let _ = unsafe {
            DuplicateHandle(
                shell_handle,
                lent,
                HANDLE::default(),
                std::ptr::null_mut(),
                0,
                false,
                DUPLICATE_CLOSE_SOURCE,
            )
        };
        result
    }

    fn create_with_parent(
        program: &Path,
        args: &[&str],
        directory: &Path,
        parent: HANDLE,
        output: HANDLE,
    ) -> io::Result<DaemonProcess> {
        let mut size = 0;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut size) };
        let mut storage = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut size) }?;
        let inherited = [output];
        let result = (|| {
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
                    Some(&parent as *const HANDLE as *const c_void),
                    size_of::<HANDLE>(),
                    None,
                    None,
                )
            }?;
            // Inherit only the log handle, never the shell's other inheritable handles.
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(inherited.as_ptr() as *const c_void),
                    size_of_val(&inherited),
                    None,
                    None,
                )
            }?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdOutput = output;
            startup.StartupInfo.hStdError = output;
            startup.lpAttributeList = list;
            let mut command_line: Vec<u16> = std::iter::once(quote(program.as_os_str()))
                .chain(args.iter().map(|arg| quote(OsStr::new(arg))))
                .collect::<Vec<_>>()
                .join(OsStr::new(" ").encode_wide().collect::<Vec<_>>().as_slice());
            command_line.push(0);
            let directory: Vec<u16> = directory.as_os_str().encode_wide().chain([0]).collect();
            let mut information = PROCESS_INFORMATION::default();
            // A null environment gives the daemon this runner's environment, including
            // the isolated COMPI_DATA_DIR.
            unsafe {
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(command_line.as_mut_ptr())),
                    None,
                    None,
                    true,
                    EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
                    None,
                    PCWSTR(directory.as_ptr()),
                    &startup.StartupInfo,
                    &mut information,
                )
            }?;
            let _ = unsafe { CloseHandle(information.hThread) };
            Ok(DaemonProcess {
                pid: information.dwProcessId,
                handle: unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) },
            })
        })();
        unsafe { DeleteProcThreadAttributeList(list) };
        result.map_err(|error: windows::core::Error| {
            io::Error::from_raw_os_error(error.code().0 & 0xffff)
        })
    }

    /// Quote one command-line argument for the MSVC runtime's parser.
    fn quote(argument: &OsStr) -> Vec<u16> {
        let text: Vec<u16> = argument.encode_wide().collect();
        let plain = !text.is_empty()
            && !text.iter().any(|&unit| {
                unit == u16::from(b' ') || unit == u16::from(b'\t') || unit == u16::from(b'"')
            });
        if plain {
            return text;
        }
        let mut quoted = vec![u16::from(b'"')];
        let mut backslashes = 0;
        for unit in text {
            if unit == u16::from(b'\\') {
                backslashes += 1;
                continue;
            }
            let escape = if unit == u16::from(b'"') {
                backslashes * 2 + 1
            } else {
                backslashes
            };
            quoted.extend(std::iter::repeat_n(u16::from(b'\\'), escape));
            quoted.push(unit);
            backslashes = 0;
        }
        quoted.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
        quoted.push(u16::from(b'"'));
        quoted
    }

    pub fn spawn_client(command: &mut Command) -> io::Result<Child> {
        command.spawn()
    }

    fn windows_of(pid: u32, visible_only: bool) -> Vec<HWND> {
        struct Search {
            pid: u32,
            visible_only: bool,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn visit(window: HWND, search: LPARAM) -> BOOL {
            let search = unsafe { &mut *(search.0 as *mut Search) };
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
            if owner == search.pid
                && (!search.visible_only || unsafe { IsWindowVisible(window) }.as_bool())
            {
                search.found.push(window);
            }
            true.into()
        }
        let mut search = Search {
            pid,
            visible_only,
            found: Vec::new(),
        };
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
        search.found
    }

    /// Whether the process shows a top-level window: the preview is on screen.
    pub fn has_visible_window(pid: u32) -> bool {
        !windows_of(pid, true).is_empty()
    }

    /// Post WM_CLOSE to the process's visible windows, the same path as clicking ×.
    pub fn request_close(pid: u32) -> bool {
        let windows = windows_of(pid, true);
        for window in &windows {
            let _ = unsafe { PostMessageW(Some(*window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        !windows.is_empty()
    }

    fn open(pid: u32) -> Option<HANDLE> {
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }.ok()
    }

    pub fn wait_pid(pid: u32, timeout: Duration) -> bool {
        let Some(handle) = open(pid) else {
            return true;
        };
        let result = unsafe { WaitForSingleObject(handle, timeout.as_millis() as u32) };
        let _ = unsafe { CloseHandle(handle) };
        result == WAIT_OBJECT_0
    }

    pub fn terminate(pid: u32) {
        if let Some(handle) = open(pid) {
            let _ = unsafe { TerminateProcess(handle, 1) };
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;

    extern "C" fn on_signal(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::Release);
    }

    pub fn install_interrupt_handler() {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGHUP, handler);
        }
    }

    /// Own session: terminal signals for `cargo dev` never reach the child directly, so
    /// the runner controls shutdown order and the daemon outlives the terminal.
    fn new_session(command: &mut Command) {
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// A running daemon this runner started.
    pub struct DaemonProcess {
        child: Child,
    }

    impl DaemonProcess {
        pub fn id(&self) -> u32 {
            self.child.id()
        }

        pub fn exit_status(&mut self) -> Option<String> {
            self.child
                .try_wait()
                .ok()
                .flatten()
                .map(|status| status.to_string())
        }

        /// The daemon keeps running after the runner exits; reap it if it exits first.
        pub fn release(mut self) {
            std::thread::spawn(move || {
                let _ = self.child.wait();
            });
        }
    }

    pub fn spawn_daemon(
        program: &std::path::Path,
        args: &[&str],
        directory: &std::path::Path,
        log: std::fs::File,
    ) -> io::Result<DaemonProcess> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(directory)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        new_session(&mut command);
        Ok(DaemonProcess {
            child: command.spawn()?,
        })
    }

    pub fn spawn_client(command: &mut Command) -> io::Result<Child> {
        new_session(command);
        command.spawn()
    }

    /// GPUI on macOS has no external close request; SIGTERM ends it. Window state is
    /// persisted ~160 ms after every change, so at most the latest change is lost.
    pub fn request_close(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
    }

    pub fn wait_pid(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn terminate(pid: u32) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}

pub use platform::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn long_running() -> Command {
        #[cfg(windows)]
        {
            let mut command = Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        }
        #[cfg(unix)]
        {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
    }

    #[test]
    fn closing_a_windowless_child_falls_back_to_termination() {
        let mut child = spawn_client(long_running().stdout(std::process::Stdio::null())).unwrap();
        let started = Instant::now();
        close_child(&mut child, Duration::from_secs(2));
        assert!(child.try_wait().unwrap().is_some(), "child must be gone");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    // On Unix a foreign pid is never this process's child; a test child would linger as a
    // zombie and look alive, so the orphan path is exercised on Windows only.
    #[cfg(windows)]
    #[test]
    fn closing_a_foreign_pid_waits_until_it_is_gone() {
        let mut child = long_running()
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        close_pid(child.id(), Duration::from_secs(2));
        assert!(wait_child(&mut child, Duration::from_secs(5)));
    }
}

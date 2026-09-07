use crate::Result;
use crate::launch::LaunchDescription;
use std::io::{Read, Write};
#[cfg(unix)]
use std::time::{Duration, Instant};

pub type PtyReader = Box<dyn Read + Send>;
pub type PtyWriter = Box<dyn Write + Send>;

/// Owns the child and PTY independently of any client attachment.
pub struct PtySession {
    #[cfg(unix)]
    native: NativePty,
    // portable-pty currently resumes Windows children before exposing their
    // handle. Keep the suspended-before-job backend until its API can provide
    // atomic descendant ownership; assigning an already-running child races.
    #[cfg(windows)]
    native: crate::conpty::ConptySession,
}

impl PtySession {
    pub fn spawn(launch: &LaunchDescription, cols: i16, rows: i16) -> Result<Self> {
        dimensions(cols, rows)?;
        Ok(Self {
            #[cfg(unix)]
            native: NativePty::spawn(launch, cols, rows)?,
            #[cfg(windows)]
            native: crate::conpty::ConptySession::spawn(launch, cols, rows)?,
        })
    }

    pub fn take_io(&mut self) -> Result<(PtyWriter, PtyReader)> {
        #[cfg(unix)]
        {
            self.native.take_io()
        }
        #[cfg(windows)]
        {
            let (input, output) = self.native.take_io()?;
            Ok((Box::new(input), Box::new(output)))
        }
    }

    pub fn resize(&self, cols: i16, rows: i16) -> Result<()> {
        dimensions(cols, rows)?;
        #[cfg(unix)]
        self.native
            .master
            .as_ref()
            .ok_or("PTY is closed")?
            .resize(dimensions(cols, rows)?)?;
        #[cfg(windows)]
        self.native.resize_owned(cols, rows)?;
        Ok(())
    }

    pub fn wait(&mut self, milliseconds: u32) -> Result<Option<u32>> {
        #[cfg(unix)]
        {
            let deadline = Instant::now() + Duration::from_millis(milliseconds.into());
            loop {
                if let Some(status) = self.native.child.try_wait()? {
                    return Ok(Some(status.exit_code()));
                }
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(windows)]
        {
            self.native.wait(milliseconds)
        }
    }

    pub fn terminate(&mut self, exit_code: u32) -> Result<()> {
        #[cfg(unix)]
        {
            let _ = exit_code;
            self.native.terminate()
        }
        #[cfg(windows)]
        {
            self.native.terminate(exit_code)
        }
    }

    /// Terminate remaining jobs even after the shell exits, then unblock I/O.
    /// The output reader remains alive while ConPTY closes and drains output.
    pub fn close(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let result = self.native.terminate();
            self.native
                .stopped
                .store(true, std::sync::atomic::Ordering::Release);
            self.native.master.take();
            result
        }
        #[cfg(windows)]
        {
            let result = self.native.terminate(1);
            self.native.close_pseudoconsole();
            result
        }
    }
}

fn dimensions(cols: i16, rows: i16) -> Result<portable_pty::PtySize> {
    if cols <= 0 || rows <= 0 {
        return Err("terminal dimensions must be positive".into());
    }
    Ok(portable_pty::PtySize {
        cols: cols as u16,
        rows: rows as u16,
        pixel_width: 0,
        pixel_height: 0,
    })
}

#[cfg(unix)]
struct NativePty {
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    session_id: libc::pid_t,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    io_taken: bool,
    terminated: bool,
}

#[cfg(unix)]
impl NativePty {
    fn spawn(launch: &LaunchDescription, cols: i16, rows: i16) -> Result<Self> {
        let command = launch.command()?;
        let pair = portable_pty::native_pty_system().openpty(dimensions(cols, rows)?)?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or("native PTY has no file descriptor")?;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let session_id = child
            .process_id()
            .expect("native Unix child has a process ID") as libc::pid_t;
        Ok(Self {
            master: Some(pair.master),
            child,
            session_id,
            stopped: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            io_taken: false,
            terminated: false,
        })
    }

    fn take_io(&mut self) -> Result<(PtyWriter, PtyReader)> {
        if self.io_taken {
            return Err("PTY I/O was already taken".into());
        }
        let fd = self
            .master
            .as_ref()
            .ok_or("PTY is closed")?
            .as_raw_fd()
            .ok_or("native PTY has no file descriptor")?;
        let input = UnixIo {
            file: duplicate_fd(fd)?,
            stopped: self.stopped.clone(),
            drain_deadline: None,
        };
        let output = UnixIo {
            file: duplicate_fd(fd)?,
            stopped: self.stopped.clone(),
            drain_deadline: None,
        };
        self.io_taken = true;
        Ok((Box::new(input), Box::new(output)))
    }

    fn terminate(&mut self) -> Result<()> {
        if self.terminated {
            return Ok(());
        }
        // Interactive job control puts background jobs in separate groups.
        // Killing only -shell_pid misses them. Freeze every member of the
        // owned POSIX session before killing it, including orphaned jobs.
        let result = terminate_session(self.session_id);
        if result.is_ok() {
            self.terminated = true;
        }
        result
    }
}

#[cfg(unix)]
impl Drop for NativePty {
    fn drop(&mut self) {
        let _ = self.terminate();
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        // SIGKILL has already been sent; bounded polling avoids blocking Drop
        // indefinitely on a child stuck in an uninterruptible kernel operation.
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(unix)]
fn session_members(session_id: libc::pid_t) -> Result<Vec<libc::pid_t>> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,stat="])
        .output()?;
    if !output.status.success() {
        return Err("could not enumerate PTY session processes".into());
    }
    Ok(std::str::from_utf8(&output.stdout)?
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<libc::pid_t>().ok()?;
            let state = fields.next()?;
            // Zombies have exited; only their parent's wait remains.
            (pid > 1 && !state.starts_with('Z') && unsafe { libc::getsid(pid) } == session_id)
                .then_some(pid)
        })
        .collect())
}

#[cfg(unix)]
fn signal_member(pid: libc::pid_t, session_id: libc::pid_t, signal: libc::c_int) -> Result<()> {
    if unsafe { libc::getsid(pid) } != session_id {
        return Ok(());
    }
    if unsafe { libc::kill(pid, signal) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_session(session_id: libc::pid_t) -> Result<()> {
    let mut stopped = std::collections::HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let freeze_result = (|| -> Result<()> {
        loop {
            let members = session_members(session_id)?;
            let mut added = false;
            for pid in members {
                signal_member(pid, session_id, libc::SIGSTOP)?;
                added |= stopped.insert(pid);
            }
            if !added {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("PTY descendants did not quiesce for termination".into());
            }
        }
    })();
    // Always kill already frozen members, even if a later enumeration fails.
    let mut kill_error = None;
    for pid in stopped {
        if let Err(error) = signal_member(pid, session_id, libc::SIGKILL) {
            kill_error = Some(error);
        }
    }
    // Also kill the original group when enumeration failed before finding it.
    if unsafe { libc::getsid(session_id) } == session_id {
        let _ = unsafe { libc::kill(-session_id, libc::SIGKILL) };
    }
    freeze_result?;
    if let Some(error) = kill_error {
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if session_members(session_id)?.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("PTY descendants survived SIGKILL".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn duplicate_fd(fd: std::os::fd::RawFd) -> Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(duplicate) })
}

#[cfg(unix)]
struct UnixIo {
    file: std::fs::File,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    drain_deadline: Option<Instant>,
}

#[cfg(unix)]
impl UnixIo {
    fn poll(&self, events: libc::c_short) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let mut pollfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events,
            revents: 0,
        };
        if unsafe { libc::poll(&mut pollfd, 1, 50) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Read for UnixIo {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                // Drain final buffered output, but never let a process that
                // escaped the POSIX session keep the reader join alive.
                let deadline = self
                    .drain_deadline
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
                if Instant::now() >= *deadline {
                    return Ok(0);
                }
            }
            match self.file.read(bytes) {
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                        return Ok(0);
                    }
                    self.poll(libc::POLLIN)?;
                }
                result => return result,
            }
        }
    }
}

#[cfg(unix)]
impl Write for UnixIo {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY is closed",
                ));
            }
            match self.file.write(bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "PTY input stalled",
                        ));
                    }
                    self.poll(libc::POLLOUT)?;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

use crate::Result;
use crate::launch::LaunchDescription;
use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::iter::once;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

pub struct ConptySession {
    hpc: Option<HPCON>,
    process: OwnedHandle,
    job: OwnedHandle,
    input: Option<File>,
    output: Option<File>,
}

impl ConptySession {
    pub fn spawn(launch: &LaunchDescription, cols: i16, rows: i16) -> Result<Self> {
        if cols <= 0 || rows <= 0 {
            return Err("terminal dimensions must be positive".into());
        }
        let _ = launch.command()?;

        unsafe {
            let (pty_input, host_input) = anonymous_pipe()?;
            let (host_output, pty_output) = anonymous_pipe()?;

            let hpc = CreatePseudoConsole(
                COORD { X: cols, Y: rows },
                raw_handle(&pty_input),
                raw_handle(&pty_output),
                0,
            )?;

            let process_result = create_process(hpc, launch);
            drop(pty_input);
            drop(pty_output);

            let (process, thread) = match process_result {
                Ok(handles) => handles,
                Err(error) => {
                    ClosePseudoConsole(hpc);
                    return Err(error);
                }
            };

            let job = match create_kill_on_close_job(&process) {
                Ok(job) => job,
                Err(error) => {
                    terminate_and_wait(&process);
                    ClosePseudoConsole(hpc);
                    return Err(error);
                }
            };

            if ResumeThread(HANDLE(thread.as_raw_handle())) == u32::MAX {
                let error = windows::core::Error::from_thread();
                let _ = TerminateJobObject(HANDLE(job.as_raw_handle()), 1);
                let _ = WaitForSingleObject(HANDLE(process.as_raw_handle()), 5_000);
                ClosePseudoConsole(hpc);
                return Err(error.into());
            }
            drop(thread);

            Ok(Self {
                hpc: Some(hpc),
                process,
                job,
                input: Some(File::from(host_input)),
                output: Some(File::from(host_output)),
            })
        }
    }

    pub fn take_io(&mut self) -> Result<(File, File)> {
        let input = self.input.take().ok_or("ConPTY input was already taken")?;
        let output = self
            .output
            .take()
            .ok_or("ConPTY output was already taken")?;
        Ok((input, output))
    }

    pub fn resize_owned(&self, cols: i16, rows: i16) -> Result<()> {
        let hpc = self.hpc.ok_or("ConPTY is closed")?;
        if cols <= 0 || rows <= 0 {
            return Err("terminal dimensions must be positive".into());
        }
        unsafe { ResizePseudoConsole(hpc, COORD { X: cols, Y: rows })? };
        Ok(())
    }

    pub fn wait(&self, milliseconds: u32) -> Result<Option<u32>> {
        let process = HANDLE(self.process.as_raw_handle());
        let result = unsafe { WaitForSingleObject(process, milliseconds) };
        match result {
            WAIT_OBJECT_0 => {
                let mut exit_code = 0;
                unsafe { GetExitCodeProcess(process, &mut exit_code)? };
                Ok(Some(exit_code))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(windows::core::Error::from_thread().into()),
            other => Err(format!("unexpected process wait result: {other:?}").into()),
        }
    }

    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        unsafe { TerminateJobObject(HANDLE(self.job.as_raw_handle()), exit_code)? };
        Ok(())
    }

    pub fn close_pseudoconsole(&mut self) {
        if let Some(hpc) = self.hpc.take() {
            unsafe { ClosePseudoConsole(hpc) };
        }
    }
}

impl Drop for ConptySession {
    fn drop(&mut self) {
        // The launcher can exit before its descendants. Terminate the job
        // before closing the console, including startup/error-path drops.
        let _ = self.terminate(1);
        if matches!(self.wait(0), Ok(None)) {
            let _ = self.wait(5_000);
        }
        self.close_pseudoconsole();
    }
}

unsafe fn anonymous_pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, None, 0)? };
    Ok((unsafe { OwnedHandle::from_raw_handle(read.0) }, unsafe {
        OwnedHandle::from_raw_handle(write.0)
    }))
}

unsafe fn create_process(
    hpc: HPCON,
    launch: &LaunchDescription,
) -> Result<(OwnedHandle, OwnedHandle)> {
    let (executable, mut command, environment, cwd) = launch_parameters(launch)?;
    let mut attribute_bytes = 0_usize;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut attribute_bytes) };
    if attribute_bytes == 0 {
        return Err(windows::core::Error::from_thread().into());
    }

    let word_size = size_of::<usize>();
    let mut attribute_storage = vec![0_usize; attribute_bytes.div_ceil(word_size)];
    let attributes = LPPROC_THREAD_ATTRIBUTE_LIST(attribute_storage.as_mut_ptr().cast());
    if let Err(error) = unsafe {
        InitializeProcThreadAttributeList(Some(attributes), 1, None, &mut attribute_bytes)
    } {
        return Err(error.into());
    }
    let update_result = unsafe {
        UpdateProcThreadAttribute(
            attributes,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(hpc.0 as *const c_void),
            size_of::<HPCON>(),
            None,
            None,
        )
    };
    if let Err(error) = update_result {
        unsafe { DeleteProcThreadAttributeList(attributes) };
        return Err(error.into());
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = HANDLE::default();
    startup.StartupInfo.hStdOutput = HANDLE::default();
    startup.StartupInfo.hStdError = HANDLE::default();
    startup.lpAttributeList = attributes;

    let mut process_info = PROCESS_INFORMATION::default();
    let create_result = unsafe {
        CreateProcessW(
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            cwd.as_ref()
                .map_or(PCWSTR::null(), |cwd| PCWSTR(cwd.as_ptr())),
            &startup.StartupInfo,
            &mut process_info,
        )
    };

    unsafe { DeleteProcThreadAttributeList(attributes) };
    create_result?;

    Ok((
        unsafe { OwnedHandle::from_raw_handle(process_info.hProcess.0) },
        unsafe { OwnedHandle::from_raw_handle(process_info.hThread.0) },
    ))
}

unsafe fn create_kill_on_close_job(process: &OwnedHandle) -> Result<OwnedHandle> {
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
    let job = unsafe { OwnedHandle::from_raw_handle(job.0) };

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )?;
        AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(process.as_raw_handle()))?;
    }
    Ok(job)
}

fn quote_windows_argument(argument: &OsStr) -> Vec<u16> {
    let argument: Vec<u16> = argument.encode_wide().collect();
    if !argument.is_empty()
        && !argument
            .iter()
            .any(|character| matches!(*character, 9 | 10 | 11 | 13 | 32 | 34))
    {
        return argument;
    }
    let mut quoted = vec![34];
    let mut backslashes = 0;
    for character in argument {
        if character == 92 {
            backslashes += 1;
        } else if character == 34 {
            quoted.extend(std::iter::repeat_n(92, backslashes * 2 + 1));
            quoted.push(character);
            backslashes = 0;
        } else {
            quoted.extend(std::iter::repeat_n(92, backslashes));
            quoted.push(character);
            backslashes = 0;
        }
    }
    quoted.extend(std::iter::repeat_n(92, backslashes * 2));
    quoted.push(34);
    quoted
}

type LaunchParameters = (Vec<u16>, Vec<u16>, Vec<u16>, Option<Vec<u16>>);

fn launch_parameters(launch: &LaunchDescription) -> Result<LaunchParameters> {
    fn wide(value: &OsStr) -> Result<Vec<u16>> {
        let value: Vec<u16> = value.encode_wide().chain(once(0)).collect();
        if value[..value.len() - 1].contains(&0) {
            return Err("launch arguments must not contain NUL".into());
        }
        Ok(value)
    }
    let executable = wide(&launch.executable)?;
    let mut command = quote_windows_argument(&launch.executable);
    for argument in &launch.argv {
        wide(argument)?;
        command.push(32);
        command.extend(quote_windows_argument(argument));
    }
    command.push(0);
    let mut environment = std::collections::BTreeMap::new();
    for (key, value) in std::env::vars_os().chain(launch.env.iter().cloned()) {
        environment.insert(key.to_string_lossy().to_uppercase(), (key, value));
    }
    let mut block = Vec::new();
    for (_, (key, value)) in environment {
        let key = wide(&key)?;
        let value = wide(&value)?;
        block.extend_from_slice(&key[..key.len() - 1]);
        block.push(61);
        block.extend(value);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    let cwd = launch
        .cwd
        .as_ref()
        .map(|cwd| wide(cwd.as_os_str()))
        .transpose()?;
    Ok((executable, command, block, cwd))
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle() as RawHandle)
}

unsafe fn terminate_and_wait(process: &OwnedHandle) {
    let process = HANDLE(process.as_raw_handle());
    let _ = unsafe { TerminateProcess(process, 1) };
    let _ = unsafe { WaitForSingleObject(process, 5_000) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

    // Keep this regression with the suspended-before-job backend. A launcher
    // can create a child immediately and exit; owning just the launcher or
    // assigning it after resume cannot establish the same ownership guarantee.
    #[test]
    fn owns_immediately_spawned_descendant_after_launcher_exit() {
        let launch = LaunchDescription {
            executable: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
            argv: vec![
                "-NoLogo".into(), "-NoProfile".into(), "-Command".into(),
                "$p = Start-Process $env:ComSpec -ArgumentList '/c ping -n 300 127.0.0.1 > nul' -PassThru -WindowStyle Hidden; [Console]::WriteLine(('OWNED_PID_{0}_END' -f $p.Id))".into(),
            ],
            cwd: None,
            env: Vec::new(),
            metadata: None,
        };
        let mut pty = ConptySession::spawn(&launch, 80, 24).unwrap();
        let (input, mut output) = pty.take_io().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(count) = output.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                if sender.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut bytes = Vec::new();
        let descendant = loop {
            bytes.extend(
                receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap(),
            );
            let text = String::from_utf8_lossy(&bytes);
            if let Some((_, rest)) = text.split_once("OWNED_PID_")
                && let Some((pid, _)) = rest.split_once("_END")
                && let Ok(pid) = pid.parse::<u32>()
            {
                break unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
                    .map(|handle| unsafe { OwnedHandle::from_raw_handle(handle.0) })
                    .unwrap();
            }
        };
        while pty.wait(50).unwrap().is_none() {
            assert!(Instant::now() < deadline, "launcher did not exit");
        }
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(descendant.as_raw_handle()), 0) },
            WAIT_TIMEOUT
        );
        pty.terminate(137).unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(descendant.as_raw_handle()), 5_000) },
            WAIT_OBJECT_0
        );
        pty.close_pseudoconsole();
        drop(input);
        reader.join().unwrap();
    }
}

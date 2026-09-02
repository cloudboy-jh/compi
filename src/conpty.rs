use crate::Result;
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
    CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR, w};

pub struct ConptySession {
    hpc: Option<HPCON>,
    process: OwnedHandle,
    job: OwnedHandle,
    input: Option<File>,
    output: Option<File>,
}

impl ConptySession {
    pub fn spawn(cols: i16, rows: i16) -> Result<Self> {
        if cols <= 0 || rows <= 0 {
            return Err("terminal dimensions must be positive".into());
        }

        unsafe {
            let (pty_input, host_input) = anonymous_pipe()?;
            let (host_output, pty_output) = anonymous_pipe()?;

            let hpc = CreatePseudoConsole(
                COORD { X: cols, Y: rows },
                raw_handle(&pty_input),
                raw_handle(&pty_output),
                0,
            )?;

            let process_result = create_wsl_process(hpc);
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

    pub fn hpc_value(&self) -> Result<isize> {
        self.hpc
            .map(|hpc| hpc.0)
            .ok_or_else(|| "ConPTY is closed".into())
    }

    pub fn resize(hpc_value: isize, cols: i16, rows: i16) -> Result<()> {
        if cols <= 0 || rows <= 0 {
            return Err("terminal dimensions must be positive".into());
        }
        unsafe { ResizePseudoConsole(HPCON(hpc_value), COORD { X: cols, Y: rows })? };
        Ok(())
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
        if matches!(self.wait(0), Ok(None)) {
            let _ = self.terminate(1);
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

unsafe fn create_wsl_process(hpc: HPCON) -> Result<(OwnedHandle, OwnedHandle)> {
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

    let mut command: Vec<u16> = OsStr::new("wsl.exe --exec /bin/bash -i")
        .encode_wide()
        .chain(once(0))
        .collect();
    let mut process_info = PROCESS_INFORMATION::default();
    let create_result = unsafe {
        CreateProcessW(
            w!(r"C:\Windows\System32\wsl.exe"),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
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

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle() as RawHandle)
}

unsafe fn terminate_and_wait(process: &OwnedHandle) {
    let process = HANDLE(process.as_raw_handle());
    let _ = unsafe { TerminateProcess(process, 1) };
    let _ = unsafe { WaitForSingleObject(process, 5_000) };
}

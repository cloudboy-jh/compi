// The Start menu and Windows search open this wrapper; a console subsystem would flash a
// console window before the GUI client appears.
#![cfg_attr(windows, windows_subsystem = "windows")]

use compi_update::{InstallTarget, guard_client_launch, selected_executable};
fn main() {
    if let Err(error) = run() {
        report(&format!("Compi launch failed: {error}"));
        std::process::exit(1);
    }
}
fn run() -> compi_update::Result<()> {
    let target = InstallTarget::detect()?;
    let lock = guard_client_launch()?;
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let generation = if arguments
        .first()
        .is_some_and(|a| a == "--daemon-generation")
    {
        if arguments.len() < 2 {
            return Err(compi_update::Error(
                "--daemon-generation needs an exact payload version".into(),
            ));
        }
        let version = arguments
            .remove(1)
            .into_string()
            .map_err(|_| compi_update::Error("Invalid daemon generation".into()))?;
        arguments.remove(0);
        Some(version)
    } else {
        None
    };
    let executable = selected_executable(&target.root, generation.as_deref())?;
    drop(lock);
    let mut command = std::process::Command::new(executable);
    command.args(arguments);
    // The daemon is a console program; without a console here Windows would open one for it.
    #[cfg(windows)]
    if generation.is_some() {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|error| {
        compi_update::Error(format!("Cannot start selected client payload: {error}"))
    })?;
    Ok(())
}

/// A windowless wrapper has no console to print to, so failures are shown in a dialog.
#[cfg(windows)]
fn report(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(message), wide("Compi"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

#[cfg(not(windows))]
fn report(message: &str) {
    eprintln!("{message}");
}

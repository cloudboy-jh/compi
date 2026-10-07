#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use compi_setup::installer::InstallerOperation;

    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|mode| mode.starts_with("--msi-"))
    {
        let code = compi_setup::installer::run_msi_action(&arguments);
        if arguments.first().map(String::as_str) == Some("--msi-lock-worker")
            && let Ok(path) = std::env::current_exe()
        {
            schedule_self_delete(&path);
        }
        std::process::exit(code);
    }
    let mut args = arguments.iter();
    let mode = args.next().map(String::as_str);
    let operation = match mode {
        Some("--repair") => InstallerOperation::Repair,
        Some("--remove") | Some("--remove-worker") => InstallerOperation::Remove,
        _ => std::process::exit(2),
    };
    let Some(product_code) = args.next() else {
        std::process::exit(2);
    };
    let mut silent = false;
    let mut remove_data = false;
    let mut cancel_after = None;
    while let Some(option) = args.next() {
        match option.as_str() {
            "--silent" => silent = true,
            "--remove-data" if operation == InstallerOperation::Remove => remove_data = true,
            "--cancel-after-ms" => {
                cancel_after = args.next().and_then(|value| value.parse().ok());
                if cancel_after.is_none() {
                    std::process::exit(2);
                }
            }
            _ => std::process::exit(2),
        }
    }
    if mode == Some("--remove") {
        match relaunch_remove_worker(&arguments, silent) {
            Ok(code) => std::process::exit(code),
            Err(_) => std::process::exit(1),
        }
    }
    if silent {
        let code = compi_setup::installer::run_silent(
            None,
            Some(product_code.clone()),
            operation,
            remove_data,
            cancel_after,
        );
        if mode == Some("--remove-worker")
            && let Ok(path) = std::env::current_exe()
        {
            schedule_self_delete(&path);
        }
        std::process::exit(code);
    }
    if remove_data || cancel_after.is_some() {
        std::process::exit(2);
    }

    let delete_on_exit = (mode == Some("--remove-worker"))
        .then(|| std::env::current_exe().ok())
        .flatten();
    compi_setup::installer::run_product_action(product_code.clone(), operation);
    if let Some(path) = delete_on_exit {
        schedule_self_delete(&path);
    }
}

#[cfg(windows)]
fn relaunch_remove_worker(arguments: &[String], wait: bool) -> std::io::Result<i32> {
    let source = std::env::current_exe()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("Compi-removal-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let destination = directory.join("Compi-Setup.exe");
    std::fs::copy(source, &destination)?;
    let mut child = std::process::Command::new(destination)
        .arg("--remove-worker")
        .args(&arguments[1..])
        .spawn()?;
    if wait {
        Ok(child.wait()?.code().unwrap_or(1))
    } else {
        Ok(0)
    }
}

#[cfg(windows)]
fn schedule_self_delete(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;

    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let escaped_path = path.display().to_string().replace('\'', "''");
    let escaped_directory = path
        .parent()
        .map(|parent| parent.display().to_string().replace('\'', "''"));
    let command = format!(
        "for ($i = 0; $i -lt 50; $i++) {{ \
         Start-Sleep -Milliseconds 100; \
         Remove-Item -LiteralPath '{escaped_path}' -Force -ErrorAction SilentlyContinue; \
         if (-not (Test-Path -LiteralPath '{escaped_path}')) {{ break }} \
         }}"
    );
    let command = match escaped_directory {
        Some(directory) => format!(
            "{command}; Remove-Item -LiteralPath '{directory}' -ErrorAction SilentlyContinue"
        ),
        None => command,
    };
    let _ = std::process::Command::new(
        std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
    )
    .args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &command,
    ])
    .creation_flags(0x0800_0000)
    .spawn();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi maintenance only runs on Windows");
    std::process::exit(1);
}

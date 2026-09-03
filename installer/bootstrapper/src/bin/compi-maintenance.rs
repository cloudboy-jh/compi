#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use compi::installer::InstallerOperation;

    let mut args = std::env::args().skip(1);
    let mode = args.next();
    let operation = match mode.as_deref() {
        Some("--repair") => InstallerOperation::Repair,
        Some("--remove") | Some("--remove-worker") => InstallerOperation::Remove,
        _ => std::process::exit(2),
    };
    let Some(product_code) = args.next() else {
        std::process::exit(2);
    };
    if args.next().is_some() {
        std::process::exit(2);
    }

    if mode.as_deref() == Some("--remove") {
        if relaunch_remove_worker(&product_code).is_err() {
            std::process::exit(1);
        }
        return;
    }

    let delete_on_exit = (mode.as_deref() == Some("--remove-worker"))
        .then(|| std::env::current_exe().ok())
        .flatten();
    compi::installer::run_product_action(product_code, operation);
    if let Some(path) = delete_on_exit {
        schedule_self_delete(&path);
    }
}

#[cfg(windows)]
fn relaunch_remove_worker(product_code: &str) -> std::io::Result<()> {
    let source = std::env::current_exe()?;
    let destination =
        std::env::temp_dir().join(format!("Compi-Setup-{}.exe", std::process::id()));
    std::fs::copy(source, &destination)?;
    std::process::Command::new(destination)
        .args(["--remove-worker", product_code])
        .spawn()?;
    Ok(())
}

#[cfg(windows)]
fn schedule_self_delete(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;

    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let escaped_path = path.display().to_string().replace('\'', "''");
    let command = format!(
        "for ($i = 0; $i -lt 50; $i++) {{ \
         Start-Sleep -Milliseconds 100; \
         Remove-Item -LiteralPath '{escaped_path}' -Force -ErrorAction SilentlyContinue; \
         if (-not (Test-Path -LiteralPath '{escaped_path}')) {{ exit 0 }} \
         }}"
    );
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

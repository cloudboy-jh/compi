#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
static COMPI_MSI: &[u8] = include_bytes!("../payload/Compi.msi");

#[cfg(windows)]
fn main() {
    use compi_setup::installer::InstallerOperation;

    let args: Vec<_> = std::env::args().skip(1).collect();
    let operation = match args.first().map(String::as_str) {
        None | Some("--install") => InstallerOperation::Install,
        Some("--repair") => InstallerOperation::Repair,
        Some("--remove") => InstallerOperation::Remove,
        _ => std::process::exit(2),
    };
    let mut silent = false;
    let mut remove_data = false;
    let mut cancel_after = None;
    let mut options = args.iter().skip(1);
    while let Some(option) = options.next() {
        match option.as_str() {
            "--silent" => silent = true,
            "--remove-data" if operation == InstallerOperation::Remove => remove_data = true,
            "--cancel-after-ms" => {
                cancel_after = options.next().and_then(|value| value.parse().ok());
                if cancel_after.is_none() {
                    std::process::exit(2);
                }
            }
            _ => std::process::exit(2),
        }
    }
    if silent {
        std::process::exit(compi_setup::installer::run_silent(
            Some(COMPI_MSI),
            None,
            operation,
            remove_data,
            cancel_after,
        ));
    }
    if remove_data || cancel_after.is_some() {
        std::process::exit(2);
    }
    compi_setup::installer::run(COMPI_MSI, operation);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi Setup only runs on Windows");
    std::process::exit(1);
}

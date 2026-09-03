#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
static COMPI_MSI: &[u8] = include_bytes!("../payload/Compi.msi");

#[cfg(windows)]
fn main() {
    use compi::installer::InstallerOperation;

    let operation = match std::env::args().nth(1).as_deref() {
        None | Some("--install") => InstallerOperation::Install,
        Some("--repair") => InstallerOperation::Repair,
        Some("--remove") => InstallerOperation::Remove,
        Some(_) => std::process::exit(2),
    };
    compi::installer::run(COMPI_MSI, operation);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi Setup only runs on Windows");
    std::process::exit(1);
}

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    compi_probe::gui::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi only runs on Windows");
    std::process::exit(1);
}

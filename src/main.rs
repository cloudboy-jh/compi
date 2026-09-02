#[cfg(windows)]
fn main() {
    if let Err(error) = compi_probe::app::run() {
        eprintln!("compi-probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("compi-probe only runs on Windows");
    std::process::exit(1);
}

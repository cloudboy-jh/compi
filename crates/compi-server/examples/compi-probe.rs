#[cfg(any(windows, unix))]
fn main() {
    if let Err(error) = compi_server::probe::run() {
        eprintln!("compi development probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(windows, unix)))]
fn main() {
    eprintln!("Compi's development probe requires Windows or Unix");
    std::process::exit(1);
}

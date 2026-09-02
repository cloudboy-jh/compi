#[cfg(windows)]
fn main() {
    if let Err(error) = compi::app::run() {
        eprintln!("compi development probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi's development probe only runs on Windows");
    std::process::exit(1);
}

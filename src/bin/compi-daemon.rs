#[cfg(windows)]
fn main() {
    let mut args = std::env::args().skip(1);
    let instance = match args.next().as_deref() {
        None => None,
        Some("--instance") => match args.next() {
            Some(instance) if args.next().is_none() => Some(instance),
            _ => {
                eprintln!("compi-daemon: usage: compi-daemon [--instance <name>]");
                std::process::exit(2);
            }
        },
        Some(_) => {
            eprintln!("compi-daemon: usage: compi-daemon [--instance <name>]");
            std::process::exit(2);
        }
    };

    if let Err(error) = compi::daemon::run(instance.as_deref()) {
        eprintln!("compi-daemon: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("compi-daemon only runs on Windows");
    std::process::exit(1);
}

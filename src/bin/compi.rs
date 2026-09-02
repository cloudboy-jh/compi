#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    let mut args = std::env::args().skip(1);
    let instance = match args.next().as_deref() {
        None => None,
        Some("--instance") => match (args.next(), args.next()) {
            (Some(instance), None) if !instance.is_empty() => Some(instance),
            _ => std::process::exit(2),
        },
        _ => std::process::exit(2),
    };
    compi::gui::run(instance);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi only runs on Windows");
    std::process::exit(1);
}

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    let mut args = std::env::args().skip(1);
    let mut instance = None;
    let mut working_directory = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--instance" if instance.is_none() => {
                instance = args.next().filter(|value| !value.is_empty());
                if instance.is_none() {
                    std::process::exit(2);
                }
            }
            "--working-directory" if working_directory.is_none() => {
                working_directory = args.next().filter(|value| !value.is_empty());
                if working_directory.is_none() {
                    std::process::exit(2);
                }
            }
            _ if !argument.starts_with('-') && working_directory.is_none() => {
                working_directory = Some(argument);
            }
            _ => std::process::exit(2),
        }
    }
    compi::gui::run(instance, working_directory);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi only runs on Windows");
    std::process::exit(1);
}

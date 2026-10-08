const USAGE: &str = "\
Usage: cargo dev [--stop]

Builds the Compi client on every saved change and swaps it into a preview window
attached to the isolated `compi-dev` daemon. Shells survive client swaps.

  --stop    Close the preview and stop the dev daemon (ends dev shells)

While running: ⏎ reopens a closed preview, r⏎ restarts the dev daemon with the
current sources (ends dev shells), q⏎ or Ctrl+C quits and leaves the daemon running.";

fn main() {
    #[cfg(windows)]
    if let Some(path) = compi_protocol::wsl::launch_path() {
        // Bootstrap runs before the dev runner starts its watcher or workers.
        unsafe { std::env::set_var("PATH", path) };
    }
    let mut options = compi_dev::Options { stop: false };
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--stop" => options.stop = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument {other:?}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    if let Err(error) = compi_dev::run(options) {
        eprintln!("cargo dev: {error}");
        std::process::exit(1);
    }
}

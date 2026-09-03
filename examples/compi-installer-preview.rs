#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use compi::installer::PreviewState;

    let state = match std::env::args().nth(1).as_deref() {
        None | Some("ready") => PreviewState::Ready,
        Some("upgrade") => PreviewState::Upgrade,
        Some("installing") => PreviewState::Installing,
        Some("complete") => PreviewState::Complete,
        Some("error") => PreviewState::Error,
        Some("remove") => PreviewState::Remove,
        Some(_) => std::process::exit(2),
    };
    compi::installer::run_preview(state);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the Compi installer preview only runs on Windows");
    std::process::exit(1);
}

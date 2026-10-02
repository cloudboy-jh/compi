use compi_update::{InstallTarget, guard_client_launch, selected_executable};
fn main() {
    if let Err(error) = run() {
        eprintln!("Compi launch failed: {error}");
        std::process::exit(1);
    }
}
fn run() -> compi_update::Result<()> {
    let target = InstallTarget::detect()?;
    let lock = guard_client_launch()?;
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let generation = if arguments
        .first()
        .is_some_and(|a| a == "--daemon-generation")
    {
        if arguments.len() < 2 {
            return Err(compi_update::Error(
                "--daemon-generation needs an exact payload version".into(),
            ));
        }
        let version = arguments
            .remove(1)
            .into_string()
            .map_err(|_| compi_update::Error("Invalid daemon generation".into()))?;
        arguments.remove(0);
        Some(version)
    } else {
        None
    };
    let executable = selected_executable(&target.root, generation.as_deref())?;
    drop(lock);
    std::process::Command::new(executable)
        .args(arguments)
        .spawn()
        .map_err(|error| {
            compi_update::Error(format!("Cannot start selected client payload: {error}"))
        })?;
    Ok(())
}

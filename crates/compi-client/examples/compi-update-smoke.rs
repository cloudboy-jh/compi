//! Development-only driver for packaged update qualification, never distributed.
#[cfg(any(windows, target_os = "macos"))]
fn run() -> compi_client::Result<()> {
    use compi_client::window_host::{
        UpdateHost, request_update_abort, request_update_prepare, request_update_release,
        update_hosts,
    };
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        time::{Duration, Instant},
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("import-theme") if args.len() == 2 => {
            let (mut library, diagnostics) = compi_client::theme_store::ThemeLibrary::load();
            if !diagnostics.is_empty() {
                return Err(format!("Theme fixture could not load: {}", diagnostics.join("; ")).into());
            }
            let imported = library.import_file(std::path::Path::new(&args[1]))?;
            let identities: Vec<_> = imported.iter().map(|theme| theme.id()).collect();
            println!("{}", serde_json::to_string(&identities)?);
            Ok(())
        }
        Some("seed-shell") if args.len() == 3 => {
            use compi_protocol::{ClientMessage, DaemonClient, SurfaceStatus};
            let mut client = DaemonClient::connect(Some(&args[1]), Duration::from_secs(5))?;
            let workspace = client.workspace()?;
            let surfaces: Vec<_> = workspace.surfaces.iter()
                .filter(|surface| surface.status == SurfaceStatus::Running).collect();
            if surfaces.len() != 1 || surfaces[0].attached {
                return Err("Seed requires exactly one detached test shell; never steals attachment".into());
            }
            client.attach_surface(surfaces[0], surfaces[0].cols, surfaces[0].rows)?;
            client.request(ClientMessage::Input {
                data: format!("{}\r", args[2]).into_bytes(), latency_id: None,
            })?;
            client.request(ClientMessage::Detach)?;
            Ok(())
        }
        Some("hosts") if args.len() == 2 => {
            let hosts: Vec<_> = update_hosts()?.into_iter()
                .filter(|host| host.instance.as_deref() == Some(args[1].as_str())).collect();
            println!("{}", serde_json::to_string(&hosts)?);
            Ok(())
        }
        Some("prepare") if args.len() == 6 => {
            let handoff = PathBuf::from(&args[2]);
            let rollback_handoff = PathBuf::from(&args[3]);
            let hosts_path = PathBuf::from(&args[5]);
            if !handoff.is_absolute() || !rollback_handoff.is_absolute() || handoff == rollback_handoff
                || handoff.exists() || rollback_handoff.exists() || hosts_path.exists() {
                return Err("Smoke handoffs must be distinct new absolute owned paths".into());
            }
            let hosts: Vec<_> = update_hosts()?.into_iter()
                .filter(|host| host.instance.as_deref() == Some(args[1].as_str())).collect();
            if hosts.len() != 1 {
                return Err(format!("Expected exactly one test GUI host, found {}", hosts.len()).into());
            }
            request_update_prepare(&hosts[0], handoff.clone(), rollback_handoff.clone(), args[4].clone())?;
            let deadline = Instant::now() + Duration::from_secs(15);
            while !handoff.is_file() || !rollback_handoff.is_file() {
                if Instant::now() >= deadline {
                    request_update_abort(&hosts[0], handoff)?;
                    return Err("Test GUI did not flush both real exact-slot restore handoffs".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let result = (|| -> compi_client::Result<()> {
                let mut file = fs::OpenOptions::new().write(true).create_new(true).open(hosts_path)?;
                file.write_all(&serde_json::to_vec(&hosts)?)?;
                file.sync_all()?;
                println!("{}", serde_json::to_string(&hosts)?);
                Ok(())
            })();
            if result.is_err() { request_update_abort(&hosts[0], handoff)?; }
            result
        }
        Some(operation @ ("release" | "abort")) if args.len() == 3 => {
            let hosts: Vec<UpdateHost> = serde_json::from_slice(&fs::read(&args[1])?)?;
            let handoff = PathBuf::from(&args[2]);
            for host in hosts {
                if operation == "release" { request_update_release(&host, handoff.clone())?; }
                else { request_update_abort(&host, handoff.clone())?; }
            }
            Ok(())
        }
        Some("cancel-stage") if args.len() == 4 => {
            let target = compi_update::InstallTarget::detect()?;
            let mut config = compi_update::ReleaseConfig::compiled()?;
            config.current_version = args[3].clone();
            let release = compi_update::verify_manifest(&fs::read(&args[1])?, &config)?;
            let cancellation = compi_update::Cancellation::new();
            cancellation.cancel();
            match compi_update::prepare_local(&release, &target, &PathBuf::from(&args[2]),
                &config, &cancellation, |_| {}) {
                Err(error) if error.to_string().to_lowercase().contains("cancel") => {
                    println!("Observed real cancellation: {error}");
                    Ok(())
                }
                Err(error) => Err(format!("Expected cancellation, observed {error}").into()),
                Ok(_) => Err("Cancelled preparation unexpectedly produced an installable journal".into()),
            }
        }
        _ => Err("Usage: compi-update-smoke import-theme FILE | hosts INSTANCE | seed-shell INSTANCE COMMAND | prepare INSTANCE HANDOFF ROLLBACK_HANDOFF VERSION HOSTS | release HOSTS HANDOFF | abort HOSTS HANDOFF | cancel-stage METADATA ARTIFACT CURRENT_VERSION".into()),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    if let Err(error) = run() {
        eprintln!("Compi packaged-update diagnostic: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("Packaged GUI update qualification requires native Windows or Apple Silicon macOS");
    std::process::exit(1);
}

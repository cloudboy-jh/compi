use compi_update::*;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
fn value(arguments: &[String], name: &str) -> Result<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| Error(format!("Missing {name}")))
}
fn bounded<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 1024 * 1024 {
        return Err(Error("Worker input exceeds limit".into()));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error("Worker input exceeds limit".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn target_root(arguments: &[String]) -> Result<PathBuf> {
    let root = PathBuf::from(value(arguments, "--root")?);
    match root.canonicalize() {
        Ok(root) => Ok(root),
        #[cfg(target_os = "macos")]
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A crash between bundle renames can leave the destination absent.
            let name = root
                .file_name()
                .ok_or_else(|| Error("Recovery target has no app name".into()))?;
            let parent = root
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Compi update failed: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
 Some("apply")=>{let journal=PathBuf::from(value(&args,"--journal")?).canonicalize()?;let request:RelaunchRequest=bounded(Path::new(&value(&args,"--request")?))?;apply(&journal,&request,|event|eprintln!("{:?}: {}",event.phase,event.message))},
 Some("recover")=>{let root=target_root(&args)?;let kind=if root.extension().is_some_and(|e|e=="app") {InstallationKind::MacBundle}else{InstallationKind::PortableWindows};recover(&InstallTarget{root,kind})},
 Some("activate-msi")=>{let root=PathBuf::from(value(&args,"--root")?);activate_msi_payload(&root,&value(&args,"--version")?,&value(&args,"--task-version")?)?;Ok(())},
 Some("stage")=>{let root=target_root(&args)?;let metadata=PathBuf::from(value(&args,"--metadata")?);let artifact=PathBuf::from(value(&args,"--artifact")?);let mut config=ReleaseConfig::compiled()?;config.platform=value(&args,"--platform")?;config.current_version=value(&args,"--current-version")?;let kind=if config.platform=="macos-aarch64" {InstallationKind::MacBundle}else if config.platform=="windows-x86_64" {InstallationKind::PortableWindows}else{return Err(Error("Unsupported stage platform".into()));};let envelope:SignedManifest=bounded(&metadata)?;let verified=verify_manifest(&serde_json::to_vec(&envelope)?,&config)?;let prepared=prepare_local(&verified,&InstallTarget{root,kind},&artifact,&config,&Cancellation::new(),|event|eprintln!("{:?}: {}",event.phase,event.message))?;println!("{}",prepared.journal_path.display());Ok(())},
 _=>Err(Error("Usage: compi-update-worker apply --journal PATH --request PATH | recover --root PATH | activate-msi --root PATH --version VERSION --task-version VERSION | stage --root PATH --metadata PATH --artifact ZIP --platform PLATFORM --current-version VERSION".into()))}
}

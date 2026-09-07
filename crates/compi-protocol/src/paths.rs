use crate::Result;
use std::path::PathBuf;

pub fn data_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let directory =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?)
            .join("Compi");
    #[cfg(unix)]
    let directory = if let Some(path) = std::env::var_os("COMPI_DATA_DIR") {
        PathBuf::from(path)
    } else {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
        #[cfg(target_os = "macos")]
        let path = home.join("Library/Application Support/Compi");
        #[cfg(not(target_os = "macos"))]
        let path = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("compi");
        path
    };
    #[cfg(unix)]
    ensure_private_directory(&directory)?;
    #[cfg(windows)]
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
pub fn runtime_dir() -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let directory = if let Some(path) = std::env::var_os("COMPI_RUNTIME_DIR") {
        PathBuf::from(path)
    } else if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(path).join("compi")
    } else {
        PathBuf::from(format!("/tmp/compi-{uid}"))
    };
    ensure_private_directory(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn ensure_private_directory(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if !path.is_absolute() {
        return Err(format!("Compi directory must be absolute: {}", path.display()).into());
    }
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "Compi directory must be owned by the current user, not a symlink, and mode 0700: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

use crate::Result;
use std::ffi::OsString;
use std::path::PathBuf;

/// Per-user state directory. `COMPI_DATA_DIR` overrides the platform default on every
/// platform so isolated instances (for example `cargo dev`) never share installed state.
pub fn data_dir() -> Result<PathBuf> {
    let directory = resolve_data_dir(|name| std::env::var_os(name))?;
    #[cfg(unix)]
    ensure_private_directory(&directory)?;
    #[cfg(windows)]
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

// Environment lookup is injected so the override rules are testable without mutating
// process state.
fn resolve_data_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    if let Some(path) = env("COMPI_DATA_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(format!("Compi directory must be absolute: {}", path.display()).into());
        }
        return Ok(path);
    }
    #[cfg(windows)]
    let directory =
        PathBuf::from(env("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?).join("Compi");
    #[cfg(unix)]
    let directory = {
        let home = PathBuf::from(env("HOME").ok_or("HOME is not set")?);
        #[cfg(target_os = "macos")]
        let path = home.join("Library/Application Support/Compi");
        #[cfg(not(target_os = "macos"))]
        let path = env("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("compi");
        path
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(pairs: &[(&str, OsString)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn data_dir_override_wins_over_platform_default() {
        let isolated = std::env::temp_dir().join("compi-isolated-data");
        let native = std::env::temp_dir().join("native");
        let resolved = resolve_data_dir(environment(&[
            ("COMPI_DATA_DIR", isolated.clone().into()),
            ("LOCALAPPDATA", native.clone().into()),
            ("HOME", native.into()),
        ]))
        .unwrap();
        assert_eq!(resolved, isolated);
    }

    #[test]
    fn relative_data_dir_override_is_rejected() {
        let native = std::env::temp_dir().join("native");
        let error = resolve_data_dir(environment(&[
            ("COMPI_DATA_DIR", "relative/state".into()),
            ("LOCALAPPDATA", native.clone().into()),
            ("HOME", native.into()),
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("must be absolute"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_is_local_app_data() {
        let native = std::env::temp_dir().join("native");
        let resolved =
            resolve_data_dir(environment(&[("LOCALAPPDATA", native.clone().into())])).unwrap();
        assert_eq!(resolved, native.join("Compi"));
    }
}

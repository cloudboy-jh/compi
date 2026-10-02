use crate::{Cancellation, Result, UpdateEvent, UpdatePhase, err, event};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
const MAX_EXPANDED: u64 = 8 * 1024 * 1024 * 1024;
fn relative(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains(':') || name.contains('\0') {
        return Err(err("Unsafe archive entry name"));
    }
    let path = Path::new(name);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(err("Archive entry escapes staging"));
    }
    #[cfg(windows)]
    for component in path.components() {
        let part = component.as_os_str().to_string_lossy();
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.ends_with(['.', ' '])
            || part.bytes().any(|c| c < 32 || b"<>\"|?*".contains(&c))
            || matches!(
                stem.as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            )
        {
            return Err(err("Unsafe Windows archive filename"));
        }
    }
    Ok(path.to_path_buf())
}
fn link_target(entry: &Path, target: &str) -> Result<()> {
    if target.contains('\\') || target.contains(':') || target.contains('\0') {
        return Err(err("Unsafe archive symlink target"));
    }
    let mut depth = entry.parent().map(|p| p.components().count()).unwrap_or(0);
    for component in Path::new(target).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(err("Archive symlink escapes staging"));
                }
                depth -= 1;
            }
            _ => return Err(err("Absolute archive symlink rejected")),
        }
    }
    Ok(())
}
pub(crate) fn extract(
    package: &Path,
    destination: &Path,
    cancel: &Cancellation,
    progress: &mut impl FnMut(UpdateEvent),
) -> Result<()> {
    let file = std::fs::File::open(package)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| err(format!("Invalid package ZIP: {e}")))?;
    if zip.len() > 100000 {
        return Err(err("Archive contains too many entries"));
    }
    let mut names = HashSet::new();
    let mut links = HashSet::new();
    let mut entries = Vec::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        cancel.check()?;
        let entry = zip.by_index(index).map_err(|e| err(e.to_string()))?;
        let path = relative(entry.name())?;
        let key = if cfg!(windows) {
            path.to_string_lossy().to_lowercase()
        } else {
            path.to_string_lossy().into_owned()
        };
        if !names.insert(key) {
            return Err(err("Duplicate archive entry"));
        }
        let mode = entry.unix_mode().unwrap_or(0);
        let is_link = mode & 0o170000 == 0o120000;
        if mode & 0o170000 != 0 && !matches!(mode & 0o170000, 0o100000 | 0o040000 | 0o120000) {
            return Err(err("Special archive device entry rejected"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| err("Archive size overflow"))?;
        if total > MAX_EXPANDED {
            return Err(err("Expanded archive exceeds limit"));
        }
        if is_link {
            if entry.size() > 4096 {
                return Err(err("Symlink target exceeds limit"));
            }
            links.insert(path.clone());
        }
        entries.push((index, path, is_link, entry.is_dir(), mode));
    }
    for (_, path, _, _, _) in &entries {
        let mut parent = path.parent();
        while let Some(p) = parent {
            if links.contains(p) {
                return Err(err("Archive entry traverses a symlink"));
            }
            parent = p.parent();
        }
    }
    fs::create_dir(destination)?;
    let mut completed = 0u64;
    let mut pending_links = Vec::new();
    for (index, path, is_link, is_dir, mode) in entries {
        cancel.check()?;
        let mut entry = zip.by_index(index).map_err(|e| err(e.to_string()))?;
        let out = destination.join(&path);
        if is_link {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            link_target(&path, &target)?;
            pending_links.push((out, target));
            continue;
        }
        if is_dir {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(&out)?;
        let mut buffer = [0u8; 65536];
        let mut entry_bytes = 0;
        loop {
            cancel.check()?;
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            entry_bytes += n as u64;
            if entry_bytes > entry.size() {
                return Err(err("Expanded entry exceeds declared size"));
            }
            file.write_all(&buffer[..n])?;
            completed += n as u64;
            progress(event(
                UpdatePhase::Staging,
                completed,
                Some(total),
                "Extracting complete verified payload",
            ));
        }
        if entry_bytes != entry.size() {
            return Err(err("Truncated archive entry"));
        }
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
    }
    #[cfg(not(unix))]
    if !pending_links.is_empty() {
        return Err(err("Symlink packages are not supported on Windows"));
    }
    #[cfg(unix)]
    for (out, target) in pending_links {
        cancel.check()?;
        std::os::unix::fs::symlink(target, out)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_rejected() {
        for bad in ["../outside", "/absolute", "C:/file", "a\\b", "a/../b"] {
            assert!(relative(bad).is_err());
        }
        assert!(link_target(Path::new("Compi.app/Contents/link"), "../../../outside").is_err());
        assert!(link_target(Path::new("Compi.app/Contents/link"), "../Frameworks/Real").is_ok());
    }
    #[test]
    fn unsafe_archive_never_writes_outside_owned_staging() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("evil.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../outside.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"malicious").unwrap();
        zip.finish().unwrap();
        let stage = directory.path().join("stage");
        assert!(extract(&archive, &stage, &Cancellation::new(), &mut |_| {}).is_err());
        assert!(!directory.path().join("outside.txt").exists());
        assert!(!stage.exists());
    }
    #[test]
    fn cancelled_extraction_preserves_destination_absence() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("good.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("compi.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"payload").unwrap();
        zip.finish().unwrap();
        let cancel = Cancellation::new();
        cancel.cancel();
        let stage = directory.path().join("stage");
        assert!(extract(&archive, &stage, &cancel, &mut |_| {}).is_err());
        assert!(!stage.exists());
    }
}

//! The one-time note that a terminal's folder syncs with OneDrive.
//!
//! OneDrive uploads every file a build, package install or Git writes, which slows them
//! and can lock files while they are written. Compi says so once per user, the first
//! time a pane's folder is inside the user's OneDrive, and never again. Only Windows has
//! the OneDrive variables, so elsewhere nothing is ever inside.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Present once the note was shown; its contents are never read.
const MARKER_FILE: &str = "onedrive-note-shown";

pub const TITLE: &str = "This folder syncs with OneDrive";
pub const MESSAGE: &str = "Builds, package installs and Git run slower here and can clash with syncing. A folder outside OneDrive, such as C:\\dev, avoids that.";

/// This user's OneDrive folders, from the variables OneDrive sets for personal and work
/// accounts; none outside Windows.
static ROOTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|root| !root.is_empty())
        .collect()
});

/// Whether the note was shown, by this process or an earlier one. Without a data
/// directory it counts as shown: a note that can't be recorded would show every launch.
static SHOWN: LazyLock<AtomicBool> =
    LazyLock::new(|| AtomicBool::new(marker().is_none_or(|path| path.exists())));

fn marker() -> Option<PathBuf> {
    compi_protocol::paths::data_dir()
        .ok()
        .map(|data| data.join(MARKER_FILE))
}

/// Whether to show the note now that a pane reports `reported` as its folder: true
/// once per user, for the first folder inside OneDrive. Every window shares the answer.
pub fn claim(reported: &str) -> bool {
    if ROOTS.is_empty() || SHOWN.load(Ordering::Relaxed) || !within_onedrive(reported, &ROOTS) {
        return false;
    }
    if SHOWN.swap(true, Ordering::Relaxed) {
        return false;
    }
    // A failed write only means the note shows once more next launch.
    if let Some(path) = marker() {
        let _ = std::fs::write(path, b"");
    }
    true
}

/// Whether the folder a shell reports is inside one of `roots` (Windows paths).
pub fn within_onedrive(reported: &str, roots: &[String]) -> bool {
    windows_path(reported).is_some_and(|path| roots.iter().any(|root| path_is_within(&path, root)))
}

/// The Windows path of a reported folder: PowerShell's `/C:/x` and WSL's `/mnt/c/x`
/// are both `C:\x`. A folder only Linux has is no Windows path.
fn windows_path(reported: &str) -> Option<String> {
    let (drive, rest) = match reported.strip_prefix("/mnt/") {
        Some(mounted) => {
            let drive = mounted.chars().next()?;
            (drive, &mounted[drive.len_utf8()..])
        }
        None => {
            let mut chars = reported.strip_prefix('/')?.chars();
            let drive = chars.next()?;
            (drive, chars.as_str().strip_prefix(':')?)
        }
    };
    if !drive.is_ascii_alphabetic() || !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    Some(format!("{drive}:{}", rest.replace('/', "\\")))
}

/// The rule of `compi_protocol::wsl`: `path` is `root` or below it, in any case, with
/// either separator and any trailing one, so `OneDriveBackup` is not in `OneDrive`.
fn path_is_within(path: &str, root: &str) -> bool {
    let path = normalize(path);
    let root = normalize(root);
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|remainder| remainder.starts_with('\\'))
}

fn normalize(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_inside_onedrive_from_either_shell_are_found() {
        let roots = vec![
            r"C:\Users\me\OneDrive".to_owned(),
            r"C:\Users\me\OneDrive - Contoso\".to_owned(),
        ];
        for reported in [
            "/C:/Users/me/OneDrive",
            "/C:/Users/me/OneDrive/",
            "/C:/Users/me/OneDrive/Desktop/proj",
            "/c:/users/ME/onedrive/desktop",
            "/mnt/c/Users/me/OneDrive",
            "/mnt/c/Users/me/OneDrive/Documents/app",
            "/mnt/c/users/me/ONEDRIVE/x/",
            "/C:/Users/me/OneDrive - Contoso",
            "/mnt/c/Users/me/OneDrive - Contoso/repo",
        ] {
            assert!(within_onedrive(reported, &roots), "{reported}");
        }
    }

    #[test]
    fn folders_beside_or_outside_onedrive_are_not() {
        let roots = vec![r"C:\Users\me\OneDrive".to_owned()];
        for reported in [
            "/C:/Users/me/OneDriveBackup",
            "/C:/Users/me/OneDrive2/proj",
            "/mnt/c/Users/me/OneDrive.old",
            "/C:/Users/me",
            "/C:/dev/proj",
            "/D:/Users/me/OneDrive",
            "/mnt/d/Users/me/OneDrive",
            "/home/me/OneDrive",
            "/home/me/C:/Users/me/OneDrive",
            "/mnt/cdrom/Users/me/OneDrive",
            "/mnt/wsl/Users/me/OneDrive",
            "/",
        ] {
            assert!(!within_onedrive(reported, &roots), "{reported}");
        }
        assert!(!within_onedrive("/C:/Users/me/OneDrive", &[]));
    }

    #[test]
    fn a_root_written_with_forward_slashes_or_a_trailing_one_still_matches() {
        let roots = vec!["c:/users/me/onedrive/".to_owned()];
        assert!(within_onedrive("/C:/Users/me/OneDrive/proj", &roots));
        assert!(!within_onedrive("/C:/Users/me/OneDriveBackup", &roots));
    }
}

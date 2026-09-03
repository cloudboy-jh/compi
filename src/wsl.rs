use crate::Result;
use crate::protocol::WorkingDirectory;
use std::env;
use std::fs;
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_PINNED, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_UNPINNED,
};

const WSL_EXE: &str = r"C:\Windows\System32\wsl.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslLaunch {
    pub distribution: Option<String>,
    pub directory: String,
    pub metadata: Option<WorkingDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultDistribution {
    name: String,
    version: u32,
}

pub fn ensure_default_wsl2() -> Result<()> {
    default_wsl2_distribution().map(|_| ())
}

pub fn resolve_launch(working_directory: Option<&str>) -> Result<WslLaunch> {
    let Some(requested) = working_directory else {
        return Ok(WslLaunch {
            distribution: None,
            directory: "~".to_owned(),
            metadata: None,
        });
    };
    let distribution = default_wsl2_distribution()?;
    if requested.is_empty() {
        return Err("working directory must not be empty".into());
    }

    let windows_path = Path::new(requested).is_absolute() && !requested.starts_with('/');
    let resolved_wsl_path = if requested.starts_with('/') {
        requested.to_owned()
    } else if windows_path {
        let output = run_wsl([
            "--distribution",
            distribution.name.as_str(),
            "--exec",
            "wslpath",
            "-a",
            "-u",
            requested,
        ])?;
        checked_output(output, "could not translate the Windows working directory")?
    } else {
        return Err(format!(
            "working directory must be an absolute WSL or Windows path: {requested:?}"
        )
        .into());
    };

    if !resolved_wsl_path.starts_with('/') {
        return Err(format!(
            "WSL resolved the working directory to a non-absolute path: {resolved_wsl_path:?}"
        )
        .into());
    }
    let validation = run_wsl([
        "--distribution",
        distribution.name.as_str(),
        "--exec",
        "test",
        "-d",
        resolved_wsl_path.as_str(),
    ])?;
    if !validation.status.success() {
        return Err(format!(
            "working directory does not exist in WSL distribution {}: {resolved_wsl_path}",
            distribution.name
        )
        .into());
    }

    let warning_path = if windows_path {
        Some(PathBuf::from(requested))
    } else if is_mounted_windows_path(&resolved_wsl_path) {
        run_wsl([
            "--distribution",
            distribution.name.as_str(),
            "--exec",
            "wslpath",
            "-a",
            "-w",
            resolved_wsl_path.as_str(),
        ])
        .ok()
        .and_then(|output| checked_output(output, "could not inspect the Windows path").ok())
        .map(PathBuf::from)
    } else {
        None
    };
    let warning = warning_path
        .as_deref()
        .and_then(synchronized_directory_warning);
    Ok(WslLaunch {
        distribution: Some(distribution.name.clone()),
        directory: resolved_wsl_path.clone(),
        metadata: Some(WorkingDirectory {
            requested: requested.to_owned(),
            resolved_wsl_path,
            distribution: distribution.name,
            warning,
        }),
    })
}

fn default_wsl2_distribution() -> Result<DefaultDistribution> {
    let output = run_wsl(["--list", "--verbose"])?;
    if !output.status.success() {
        let error = decode_wsl_output(&output.stderr);
        return Err(format!("could not inspect WSL distributions: {}", error.trim()).into());
    }

    match parse_default_distribution(&output.stdout) {
        Some(distribution) if distribution.version == 2 => Ok(distribution),
        Some(distribution) => Err(format!(
            "the default WSL distribution {} uses WSL{}; Compi requires WSL2",
            distribution.name, distribution.version
        )
        .into()),
        None => Err("no default WSL distribution was found; Compi requires WSL2".into()),
    }
}

fn run_wsl<'a>(args: impl IntoIterator<Item = &'a str>) -> std::io::Result<Output> {
    Command::new(WSL_EXE).args(args).output()
}

fn checked_output(output: Output, context: &str) -> Result<String> {
    if !output.status.success() {
        let error = decode_wsl_output(&output.stderr);
        return Err(format!("{context}: {}", error.trim()).into());
    }
    let value = decode_wsl_output(&output.stdout).trim().to_owned();
    if value.is_empty() {
        return Err(format!("{context}: WSL returned an empty path").into());
    }
    Ok(value)
}

fn parse_default_distribution(output: &[u8]) -> Option<DefaultDistribution> {
    let output = decode_wsl_output(output);
    output.lines().find_map(|line| {
        let default = line.trim_start().strip_prefix('*')?.trim_start();
        let fields: Vec<_> = default.split_whitespace().collect();
        if fields.len() < 3 {
            return None;
        }
        let version = fields.last()?.parse().ok()?;
        let name = fields[..fields.len() - 2].join(" ");
        Some(DefaultDistribution { name, version })
    })
}

fn synchronized_directory_warning(path: &Path) -> Option<String> {
    let roots: Vec<PathBuf> = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"]
        .into_iter()
        .filter_map(env::var_os)
        .map(PathBuf::from)
        .collect();
    let cloud_attributes = fs::metadata(path)
        .map(|metadata| metadata.file_attributes())
        .unwrap_or_default();
    synchronized_directory_warning_from(path, &roots, cloud_attributes)
}

fn synchronized_directory_warning_from(
    path: &Path,
    known_roots: &[PathBuf],
    cloud_attributes: u32,
) -> Option<String> {
    let under_known_root = known_roots.iter().any(|root| path_is_within(path, root));
    let cloud_mask = FILE_ATTRIBUTE_PINNED.0
        | FILE_ATTRIBUTE_UNPINNED.0
        | FILE_ATTRIBUTE_RECALL_ON_OPEN.0
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0;
    if under_known_root || cloud_attributes & cloud_mask != 0 {
        Some(
            "This project is in a synchronized Windows directory; filesystem-heavy WSL workloads may be slower or conflict with synchronization."
                .to_owned(),
        )
    } else {
        None
    }
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = normalize_windows_path(path);
    let root = normalize_windows_path(root);
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|remainder| remainder.starts_with('\\'))
}

fn normalize_windows_path(path: &Path) -> String {
    path.as_os_str()
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
fn is_mounted_windows_path(path: &str) -> bool {
    let Some(remainder) = path.strip_prefix("/mnt/") else {
        return false;
    };
    let bytes = remainder.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic) && matches!(bytes.get(1), None | Some(b'/'))
}

fn decode_wsl_output(output: &[u8]) -> String {
    let (pairs, _) = output.as_chunks::<2>();
    if output.len() >= 2 && pairs.iter().any(|pair| pair[1] == 0) {
        let words: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(output).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_directory_preserves_fast_default_launch() {
        assert_eq!(
            resolve_launch(None).unwrap(),
            WslLaunch {
                distribution: None,
                directory: "~".to_owned(),
                metadata: None,
            }
        );
    }

    #[test]
    fn parses_utf8_default_wsl_distribution() {
        let output =
            b"  NAME            STATE           VERSION\r\n* Ubuntu Dev      Running         2\r\n";
        assert_eq!(
            parse_default_distribution(output),
            Some(DefaultDistribution {
                name: "Ubuntu Dev".to_owned(),
                version: 2,
            })
        );
    }

    #[test]
    fn parses_utf16_default_wsl_distribution() {
        let text = "  NAME      STATE      VERSION\r\n* Ubuntu    Stopped    2\r\n";
        let output: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            parse_default_distribution(&output),
            Some(DefaultDistribution {
                name: "Ubuntu".to_owned(),
                version: 2,
            })
        );
    }

    #[test]

    fn reports_missing_default_distribution() {
        assert_eq!(
            parse_default_distribution(b"  NAME      STATE      VERSION\r\n"),
            None
        );
    }
    #[test]
    fn detects_paths_below_synchronized_roots_case_insensitively() {
        assert!(path_is_within(
            Path::new(r"C:\Users\Dev\OneDrive\project"),
            Path::new(r"c:\users\dev\onedrive")
        ));
        assert!(!path_is_within(
            Path::new(r"C:\Users\Dev\OneDriveBackup\project"),
            Path::new(r"C:\Users\Dev\OneDrive")
        ));
    }

    #[test]
    fn warns_without_blocking_for_known_synchronized_roots() {
        let warning = synchronized_directory_warning_from(
            Path::new(r"C:\Users\Dev\OneDrive\project"),
            &[PathBuf::from(r"C:\Users\Dev\OneDrive")],
            0,
        );
        assert!(
            warning
                .as_deref()
                .is_some_and(|message| { message.contains("synchronized Windows directory") })
        );
    }

    #[test]
    fn distinguishes_mounted_windows_paths_from_linux_paths() {
        assert!(is_mounted_windows_path("/mnt/c/Users/dev/project"));
        assert!(is_mounted_windows_path("/mnt/d"));
        assert!(!is_mounted_windows_path("/home/dev/project"));
        assert!(!is_mounted_windows_path("/mnt/shared/project"));
    }
}

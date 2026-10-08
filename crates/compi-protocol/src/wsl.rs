use crate::Result;
use crate::WorkingDirectory;
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

/// The inherited Windows search path, with order and executable directories
/// preserved. Cargo's injected DLL directories are not shell command paths.
pub fn launch_path() -> Option<&'static std::ffi::OsStr> {
    static PATH: std::sync::LazyLock<Option<std::ffi::OsString>> = std::sync::LazyLock::new(|| {
        let path = env::var_os("PATH")?;
        let cargo_directory = env::var_os("CARGO")
            .and_then(|_| env::current_exe().ok())
            .and_then(|executable| executable.parent().map(Path::to_owned));
        clean_path(&path, cargo_directory.as_deref()).ok()
    });
    PATH.as_deref()
}

fn clean_path(
    path: &std::ffi::OsStr,
    cargo_directory: Option<&Path>,
) -> std::result::Result<std::ffi::OsString, env::JoinPathsError> {
    use std::os::windows::ffi::OsStrExt;
    let mut seen = std::collections::HashSet::new();
    let mut directories = Vec::new();
    for directory in env::split_paths(path) {
        if let Some(build_directory) = cargo_directory
            && (directory.starts_with(build_directory)
                || (directory.ends_with("lib")
                    && directory
                        .components()
                        .any(|part| part.as_os_str() == "rustlib")))
        {
            continue;
        }
        let mut key: Vec<u16> = directory
            .as_os_str()
            .encode_wide()
            .map(|character| match character {
                65..=90 => character + 32,
                47 => 92,
                _ => character,
            })
            .collect();
        while key.len() > 3 && key.last() == Some(&92) {
            key.pop();
        }
        if seen.insert(key) {
            directories.push(directory);
        }
    }
    env::join_paths(directories)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslLaunch {
    pub distribution: String,
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

pub fn resolve_launch(
    working_directory: Option<&str>,
    requested_distribution: Option<&str>,
) -> Result<WslLaunch> {
    let distribution = match requested_distribution {
        Some(name) => selected_wsl2_distribution(name)?,
        None => native_default_distribution().unwrap_or_else(default_wsl2_distribution)?,
    };
    let Some(requested) = working_directory else {
        return Ok(WslLaunch {
            distribution: distribution.name,
            directory: "~".to_owned(),
            metadata: None,
        });
    };
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
    // The WSL file provider validates ordinary paths without starting a second
    // Linux process. Fall back for paths the Windows provider cannot represent.
    let representable = resolved_wsl_path.split('/').all(|name| {
        let base = name.split('.').next().unwrap_or_default();
        name != "."
            && name != ".."
            && !name.ends_with(['.', ' '])
            && !name
                .bytes()
                .any(|byte| byte < 32 || b"\\:<>\"|?*".contains(&byte))
            && !["CON", "PRN", "AUX", "NUL"]
                .iter()
                .any(|reserved| base.eq_ignore_ascii_case(reserved))
            && !(base.len() == 4
                && (base.as_bytes()[..3].eq_ignore_ascii_case(b"COM")
                    || base.as_bytes()[..3].eq_ignore_ascii_case(b"LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9'))
    });
    let native_directory_valid = representable
        && Path::new(&format!(
            r"\\wsl.localhost\{}\{}",
            distribution.name,
            resolved_wsl_path.trim_start_matches('/').replace('/', "\\")
        ))
        .is_dir();
    if !native_directory_valid {
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
        distribution: distribution.name.clone(),
        directory: resolved_wsl_path.clone(),
        metadata: Some(WorkingDirectory {
            requested: requested.to_owned(),
            resolved_wsl_path,
            distribution: distribution.name,
            warning,
        }),
    })
}

/// Resolve a WSL path through Linux symlinks, including links into mounted Windows drives.
pub fn windows_path_for_wsl(path: &str, distribution: &str) -> Result<PathBuf> {
    let output = run_wsl([
        "--distribution",
        distribution,
        "--exec",
        "wslpath",
        "-a",
        "-w",
        "--",
        path,
    ])?;
    Ok(PathBuf::from(checked_output(
        output,
        "could not resolve the WSL directory",
    )?))
}

fn selected_wsl2_distribution(name: &str) -> Result<DefaultDistribution> {
    if let Some(Ok(distribution)) = native_default_distribution()
        && distribution.name.eq_ignore_ascii_case(name)
    {
        return Ok(distribution);
    }
    let output = run_wsl(["--list", "--verbose"])?;
    if !output.status.success() {
        return Err("could not inspect WSL distributions".into());
    }
    let listing = decode_wsl_output(&output.stdout);
    for line in listing.lines() {
        let fields: Vec<_> = line
            .trim_start()
            .trim_start_matches('*')
            .split_whitespace()
            .collect();
        if fields.len() >= 3 && fields[..fields.len() - 2].join(" ") == name {
            let version = fields[fields.len() - 1].parse::<u32>().unwrap_or(0);
            if version != 2 {
                return Err(format!("configured WSL distribution {name} must use WSL2").into());
            }
            return Ok(DefaultDistribution {
                name: name.into(),
                version,
            });
        }
    }
    Err(format!("configured WSL distribution {name} was not found").into())
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
/// Resolve only the WSL2 distribution, without starting Linux to validate a path.
/// File-tree access validates the requested directory through its WSL UNC share.
pub fn directory_distribution(requested_distribution: Option<&str>) -> Result<String> {
    let distribution = match requested_distribution {
        Some(name) => selected_wsl2_distribution(name)?,
        None => native_default_distribution().unwrap_or_else(default_wsl2_distribution)?,
    };
    Ok(distribution.name)
}

/// Installed WSL2 distributions that can host shells, in `wsl --list` order.
/// Docker Desktop's internal distributions are not user shells.
pub fn wsl2_distributions() -> Result<Vec<String>> {
    let output = run_wsl(["--list", "--verbose"])?;
    if !output.status.success() {
        let error = decode_wsl_output(&output.stderr);
        return Err(format!("could not inspect WSL distributions: {}", error.trim()).into());
    }
    Ok(parse_wsl2_distributions(&decode_wsl_output(&output.stdout)))
}

fn parse_wsl2_distributions(listing: &str) -> Vec<String> {
    listing
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line
                .trim_start()
                .trim_start_matches('*')
                .split_whitespace()
                .collect();
            if fields.len() < 3 || fields[fields.len() - 1] != "2" {
                return None;
            }
            let name = fields[..fields.len() - 2].join(" ");
            (!name.starts_with("docker-desktop")).then_some(name)
        })
        .collect()
}

fn native_default_distribution() -> Option<Result<DefaultDistribution>> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    const HKCU: isize = 0x8000_0001u32 as i32 as isize;
    const REG_SZ: u32 = 0x2;
    const REG_DWORD: u32 = 0x10;
    const ROOT: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Lxss";
    fn query_string(subkey: &str, value: &str) -> Option<String> {
        let subkey: Vec<_> = subkey.encode_utf16().chain([0]).collect();
        let value: Vec<_> = value.encode_utf16().chain([0]).collect();
        let mut size = 0;
        // Query the byte count first; distribution names are not bounded by GUID size.
        let status = unsafe {
            RegGetValueW(
                HKCU,
                subkey.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status != 0 || !(2..=1024).contains(&size) || size % 2 != 0 {
            return None;
        }
        let mut words = vec![0u16; (size / 2) as usize];
        let status = unsafe {
            RegGetValueW(
                HKCU,
                subkey.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                std::ptr::null_mut(),
                words.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 || size < 2 || size as usize > words.len() * 2 {
            return None;
        }
        let end = words.iter().position(|word| *word == 0)?;
        String::from_utf16(&words[..end]).ok()
    }
    let guid = query_string(ROOT, "DefaultDistribution")?;
    if guid.len() != 38
        || !guid.starts_with('{')
        || !guid.ends_with('}')
        || guid[1..37].bytes().enumerate().any(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte != b'-'
            } else {
                !byte.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    let subkey = format!("{ROOT}\\{guid}");
    let name = query_string(&subkey, "DistributionName")?;
    let mut version = 0u32;
    let mut size = 4u32;
    let subkey: Vec<_> = subkey.encode_utf16().chain([0]).collect();
    let value: Vec<_> = "Version".encode_utf16().chain([0]).collect();
    let status = unsafe {
        RegGetValueW(
            HKCU,
            subkey.as_ptr(),
            value.as_ptr(),
            REG_DWORD,
            std::ptr::null_mut(),
            (&mut version as *mut u32).cast(),
            &mut size,
        )
    };
    if status != 0 || size != 4 {
        return None;
    }
    if version != 2 {
        return Some(Err(format!(
            "the default WSL distribution {name} uses WSL{version}; Compi requires WSL2"
        )
        .into()));
    }
    Some(Ok(DefaultDistribution { name, version }))
}

fn run_wsl<'a>(args: impl IntoIterator<Item = &'a str>) -> std::io::Result<Output> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(WSL_EXE);
    command
        .args(args)
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    if let Some(path) = launch_path() {
        command.env("PATH", path);
    }
    command.output()
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
    fn launch_path_keeps_windows_commands_and_first_entry_precedence() {
        let path = std::ffi::OsStr::new(
            r"C:\Tools;C:\Windows\System32;c:/tools/;C:\Work\bin;C:\Windows\System32;C:\",
        );
        let actual = clean_path(path, None).unwrap();
        assert_eq!(
            env::split_paths(&actual).collect::<Vec<_>>(),
            vec![
                PathBuf::from(r"C:\Tools"),
                PathBuf::from(r"C:\Windows\System32"),
                PathBuf::from(r"C:\Work\bin"),
                PathBuf::from(r"C:\"),
            ]
        );
    }

    #[test]
    fn cargo_library_paths_do_not_leak_into_shell_search() {
        let path = std::ffi::OsStr::new(
            r"C:\repo\target\debug\deps;C:\repo\target\debug;C:\Rust\lib\rustlib\x64\lib;C:\Rust\bin;C:\Windows\System32",
        );
        let actual = clean_path(path, Some(Path::new(r"C:\repo\target\debug"))).unwrap();
        assert_eq!(
            env::split_paths(&actual).collect::<Vec<_>>(),
            vec![
                PathBuf::from(r"C:\Rust\bin"),
                PathBuf::from(r"C:\Windows\System32")
            ]
        );
        assert_eq!(
            env::split_paths(&clean_path(path, None).unwrap()).count(),
            5
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

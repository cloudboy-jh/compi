use crate::Result;
use std::fs;
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const MAX_PATH_BYTES: usize = 4096;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_SEARCH_ENTRIES: usize = 20_000;
const MAX_SEARCH_RESULTS: usize = 256;
const MAX_SEARCH_DEPTH: usize = 8;
const MAX_QUERY_BYTES: usize = 256;
const MAX_SCAN_TIME: Duration = Duration::from_secs(5);
const MAX_REPLY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub path: String,
    pub is_directory: bool,
}

struct Root {
    host: PathBuf,
    shell: String,
    #[cfg(windows)]
    distribution: String,
}

struct ScannedEntry {
    name: String,
    is_directory: bool,
}

/// List immediate children of an absolute path in the terminal's path namespace.
/// On Windows, the path is a WSL path and `distribution` selects its WSL2 share.
pub fn list_directory(path: &str, distribution: Option<&str>) -> Result<Vec<DirectoryEntry>> {
    let root = resolve_root(path, distribution)?;
    #[cfg(windows)]
    let mut root = root;
    let mut count = 0;
    let entries = scan_directory(
        #[cfg(windows)]
        &mut root.host,
        #[cfg(not(windows))]
        root.host.as_path(),
        &root.shell,
        #[cfg(windows)]
        &root.distribution,
        Instant::now(),
        &mut count,
        MAX_DIRECTORY_ENTRIES,
        false,
    )?;
    let mut bytes = 0;
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        // JSON can escape one byte into six characters (for example, a control byte).
        bytes += entry.name.len() * 6 + 96;
        if bytes > MAX_REPLY_BYTES {
            return Err(format!(
                "directory listing for {:?} exceeds its size limit",
                root.shell
            )
            .into());
        }
        result.push(DirectoryEntry {
            name: entry.name,
            is_directory: entry.is_directory,
        });
    }
    Ok(result)
}

/// Search descendant names and root-relative paths, including hidden entries.
/// The bounded result contains absolute terminal-native paths, never host UNC paths.
pub fn search_files(
    root: &str,
    distribution: Option<&str>,
    query: &str,
) -> Result<Vec<SearchEntry>> {
    if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.contains('\0') {
        return Err("search query must contain 1–256 bytes and no NUL".into());
    }
    let root = resolve_root(root, distribution)?;
    let needle = query.to_lowercase();
    let started = Instant::now();
    let mut count = 0;
    let mut matches = Vec::new();
    let mut result_bytes = 0;
    let mut pending = vec![(root.host, root.shell, String::new(), 0_usize)];

    while let Some((host, shell, relative, depth)) = pending.pop() {
        #[cfg(windows)]
        let mut host = host;
        let entries = scan_directory(
            #[cfg(windows)]
            &mut host,
            #[cfg(not(windows))]
            host.as_path(),
            &shell,
            #[cfg(windows)]
            &root.distribution,
            started,
            &mut count,
            MAX_SEARCH_ENTRIES,
            depth > 0,
        )?;
        let mut children = Vec::new();
        for entry in entries {
            if entry.is_directory && entry.name == ".git" {
                continue;
            }
            let relative_path = if relative.is_empty() {
                entry.name.clone()
            } else {
                format!("{relative}/{}", entry.name)
            };
            let found = relative_path.to_lowercase().contains(&needle);
            if !found && !(entry.is_directory && depth < MAX_SEARCH_DEPTH) {
                continue;
            }
            let shell_path = if shell == "/" {
                format!("/{name}", name = entry.name)
            } else {
                format!("{shell}/{name}", name = entry.name)
            };
            if shell_path.len() > MAX_PATH_BYTES {
                continue;
            }
            if found {
                result_bytes += shell_path.len() * 6 + 96;
                if result_bytes > MAX_REPLY_BYTES {
                    return Ok(matches);
                }
                matches.push(SearchEntry {
                    path: shell_path.clone(),
                    is_directory: entry.is_directory,
                });
                if matches.len() == MAX_SEARCH_RESULTS {
                    return Ok(matches);
                }
            }
            if entry.is_directory && depth < MAX_SEARCH_DEPTH && !found {
                children.push((host.join(&entry.name), shell_path, relative_path, depth + 1));
            }
        }
        pending.extend(children.into_iter().rev());
    }
    Ok(matches)
}

fn scan_directory(
    #[cfg(windows)] host: &mut PathBuf,
    #[cfg(not(windows))] host: &std::path::Path,
    shell: &str,
    #[cfg(windows)] distribution: &str,
    started: Instant,
    count: &mut usize,
    max_entries: usize,
    skip_unreadable: bool,
) -> Result<Vec<ScannedEntry>> {
    check_time(started)?;
    #[cfg(windows)]
    let directory = fs::read_dir(host.as_path());
    #[cfg(not(windows))]
    let directory = fs::read_dir(host);
    let directory = match directory {
        Ok(directory) => directory,
        #[cfg(windows)]
        Err(error)
            if matches!(error.raw_os_error(), Some(code)
                if code == windows::Win32::Foundation::ERROR_DIRECTORY.0 as i32
                    || code == windows::Win32::Foundation::ERROR_PATH_NOT_FOUND.0 as i32) =>
        {
            // Windows exposes a WSL symlink into a mounted drive as a file,
            // and descendants of that link as missing paths. Resolve only
            // failed listings, keeping ordinary directories on the native path.
            let resolved = compi_protocol::wsl::windows_path_for_wsl(shell, distribution)?;
            *host = resolved;
            match fs::read_dir(host.as_path()) {
                Ok(directory) => directory,
                Err(error)
                    if skip_unreadable
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                        ) =>
                {
                    return Ok(Vec::new());
                }
                Err(error) => {
                    return Err(format!("cannot open directory {shell:?}: {error}").into());
                }
            }
        }
        Err(error)
            if skip_unreadable
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(format!("cannot open directory {shell:?}: {error}").into()),
    };
    let mut entries = Vec::new();
    for item in directory {
        check_time(started)?;
        *count += 1;
        if *count > max_entries || entries.len() >= MAX_DIRECTORY_ENTRIES {
            return Err(format!("too many entries while reading {shell:?}").into());
        }
        let item = item.map_err(|error| format!("cannot read directory {shell:?}: {error}"))?;
        let Some(name) = item.file_name().into_string().ok() else {
            // Names not representable in protocol UTF-8 cannot be opened by the client.
            continue;
        };
        let is_directory = item
            .file_type()
            .map_err(|error| format!("cannot inspect {shell:?}/{name}: {error}"))?
            .is_dir();
        entries.push(ScannedEntry { name, is_directory });
    }
    entries.sort_unstable_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

fn check_time(started: Instant) -> Result<()> {
    if started.elapsed() >= MAX_SCAN_TIME {
        return Err("directory scan exceeded its time limit".into());
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains('\0')
        || !path.starts_with('/')
    {
        return Err(
            "directory path must be an absolute Unix path (up to 4096 bytes) without NUL".into(),
        );
    }
    Ok(())
}

#[cfg(unix)]
fn resolve_root(path: &str, distribution: Option<&str>) -> Result<Root> {
    validate_path(path)?;
    if distribution.is_some() {
        return Err("WSL distribution is not supported on this host".into());
    }
    Ok(Root {
        host: PathBuf::from(path),
        shell: match path.trim_end_matches('/') {
            "" => "/".to_owned(),
            trimmed => trimmed.to_owned(),
        },
    })
}

#[cfg(windows)]
fn resolve_root(path: &str, distribution: Option<&str>) -> Result<Root> {
    let segments = wsl_segments(path)?;
    // Explicit distributions are attached to the surface; their WSL2 status
    // need only be checked once. Never cache the default: it can change while
    // this daemon has terminals belonging to different distributions.
    static SELECTED: LazyLock<Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
    let name = if let Some(name) = distribution {
        validate_distribution(name)?;
        let mut selected = SELECTED
            .lock()
            .map_err(|_| "WSL distribution cache lock was poisoned")?;
        if !selected.contains(name) {
            compi_protocol::wsl::directory_distribution(Some(name))?;
            selected.insert(name.to_owned());
        }
        name.to_owned()
    } else {
        // The native registry lookup avoids a WSL CLI process on the common path.
        let name = compi_protocol::wsl::directory_distribution(None)?;
        validate_distribution(&name)?;
        name
    };
    let mut host = PathBuf::from(r"\\wsl.localhost");
    host.push(&name);
    for segment in &segments {
        host.push(segment);
    }
    let shell = if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    };
    Ok(Root {
        host,
        shell,
        distribution: name,
    })
}
#[cfg(windows)]
fn wsl_segments(path: &str) -> Result<Vec<&str>> {
    validate_path(path)?;
    let mut segments = Vec::new();
    for segment in path.split('/').filter(|part| !part.is_empty()) {
        if segment == ".."
            || segment.chars().any(|c| {
                matches!(c, '\\' | ':' | '*' | '?' | '<' | '>' | '|' | '"') || c.is_control()
            })
            || segment.ends_with(' ')
            || (segment.ends_with('.') && segment != ".")
        {
            return Err(
                "directory path contains a segment unsafe for WSL filesystem access".into(),
            );
        }
        if segment != "." {
            segments.push(segment);
        }
    }
    Ok(segments)
}

#[cfg(windows)]
fn validate_distribution(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name == "."
        || name == ".."
        || name.ends_with(' ')
        || name.ends_with('.')
        || name.chars().any(|c| {
            matches!(c, '/' | '\\' | ':' | '*' | '?' | '<' | '>' | '|' | '\"') || c.is_control()
        })
    {
        return Err("invalid WSL distribution name".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_paths_and_queries() {
        assert!(list_directory("relative/path", None).is_err());
        assert!(list_directory("/bad\0path", None).is_err());
        assert!(search_files("/", None, "").is_err());
        assert!(search_files("/", None, "bad\0query").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unsafe_wsl_share_and_path_segments() {
        for invalid in [
            "",
            "..",
            "Ubuntu\\evil",
            "Ubuntu/evil",
            "Ubuntu:",
            "Ubuntu.",
        ] {
            assert!(validate_distribution(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_distribution("Ubuntu Dev-π").is_ok());
        for invalid in [
            "/../etc",
            "/home\\user",
            "/foo:bar",
            "/name.",
            "/quote\"name",
        ] {
            assert!(wsl_segments(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(
            wsl_segments("//home/./π folder").unwrap(),
            ["home", "π folder"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn lists_hidden_names_directories_first_and_searches_relative_paths() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "compi-tree-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::create_dir(root.join("π folder")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".hidden"), b"").unwrap();
        fs::write(root.join("a file"), b"").unwrap();
        fs::write(root.join("π folder").join("Nested.txt"), b"").unwrap();
        fs::write(root.join(".git").join("Nested.txt"), b"").unwrap();
        let path = root.to_str().unwrap();
        assert_eq!(
            list_directory(path, None).unwrap(),
            vec![
                DirectoryEntry {
                    name: ".git".into(),
                    is_directory: true
                },
                DirectoryEntry {
                    name: "π folder".into(),
                    is_directory: true
                },
                DirectoryEntry {
                    name: ".hidden".into(),
                    is_directory: false
                },
                DirectoryEntry {
                    name: "a file".into(),
                    is_directory: false
                },
            ]
        );
        assert_eq!(
            search_files(path, None, "Π FOLDER/NESTED").unwrap(),
            vec![SearchEntry {
                path: format!("{path}/π folder/Nested.txt"),
                is_directory: false,
            }]
        );
        assert_eq!(
            search_files(path, None, "π folder").unwrap(),
            vec![SearchEntry {
                path: format!("{path}/π folder"),
                is_directory: true,
            }]
        );
        assert!(search_files(path, None, ".git/Nested").unwrap().is_empty());
        assert!(
            list_directory(&format!("{path}/missing"), None)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert!(list_directory(path, Some("Ubuntu")).is_err());
    }
}

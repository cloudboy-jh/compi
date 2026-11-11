//! Paths of content panes in their environment's syntax rather than this computer's:
//! `/` paths for WSL and Unix hosts; drive or UNC roots with either slash for Windows
//! hosts. `compi files`/`compi edit`/`compi diff` and terminal file links resolve paths
//! the same way.

/// The root of an absolute `path` (`/`, `C:\` or `\\server\share\`) and the rest of it.
fn root(path: &str, windows: bool) -> Option<(String, &str)> {
    if !windows {
        return path.strip_prefix('/').map(|rest| ("/".to_owned(), rest));
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return Some((format!("{}\\", &path[..2]), &path[3..]));
    }
    let mut parts = path
        .strip_prefix(['\\', '/'])?
        .strip_prefix(['\\', '/'])?
        .splitn(3, ['\\', '/']);
    let (server, share) = (parts.next()?, parts.next()?);
    (!server.is_empty() && !share.is_empty()).then(|| {
        (
            format!("\\\\{server}\\{share}\\"),
            parts.next().unwrap_or(""),
        )
    })
}

/// `path` made absolute against `base` and normalised lexically (`.`, `..` and repeated
/// separators removed). The error explains why a path cannot be resolved.
pub fn resolve(base: Option<&str>, path: &str, windows: bool) -> Result<String, String> {
    let joined;
    let (root, rest) = match root(path, windows) {
        Some(found) => found,
        None => {
            let base = base.ok_or_else(|| {
                format!(
                    "{path} is relative, but the pane's current directory is unknown; use an absolute path"
                )
            })?;
            let (root, base_rest) =
                root(base, windows).ok_or_else(|| format!("{base} is not an absolute path"))?;
            // A Windows path rooted without a drive stays on the base's drive or share.
            joined = if windows && path.starts_with(['\\', '/']) {
                path.to_owned()
            } else {
                format!("{base_rest}/{path}")
            };
            (root, joined.as_str())
        }
    };
    let mut parts = Vec::new();
    for part in rest.split(|c| c == '/' || windows && c == '\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Ok(root + &parts.join(if windows { "\\" } else { "/" }))
}

/// `path`, absolute in a pane's environment, relative to the work tree `repo` with `/`
/// separators; `None` outside it. Windows paths compare without case.
pub fn repo_relative(repo: &str, path: &str) -> Option<String> {
    let windows = repo.contains('\\');
    let normal = |text: &str| {
        let text = text.replace('\\', "/");
        if windows {
            text.to_ascii_lowercase()
        } else {
            text
        }
    };
    let top = normal(repo);
    let top = top.trim_end_matches('/');
    let full = path.replace('\\', "/");
    let rest = normal(&full).strip_prefix(top)?.strip_prefix('/')?.len();
    let relative = &full[full.len() - rest..];
    compi_protocol::is_repo_path(relative).then(|| relative.to_owned())
}

/// The work-tree file of repository path `path`, in `repo`'s path syntax.
pub fn work_tree_path(repo: &str, path: &str) -> String {
    if repo.contains('\\') {
        format!(
            "{}\\{}",
            repo.trim_end_matches('\\'),
            path.replace('/', "\\")
        )
    } else {
        format!("{}/{path}", repo.trim_end_matches('/'))
    }
}

/// A WSL path in Windows syntax: `/mnt/c/…` is `C:\…` (the share refuses drive
/// mounts), anything else `\\wsl.localhost\<distribution>\…`.
pub fn wsl_windows_path(distribution: &str, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/mnt/") {
        let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if let [letter] = drive.as_bytes()
            && letter.is_ascii_alphabetic()
        {
            return format!(
                r"{}:\{}",
                letter.to_ascii_uppercase() as char,
                tail.replace('/', "\\")
            );
        }
    }
    format!(
        r"\\wsl.localhost\{distribution}\{}",
        path.trim_start_matches('/').replace('/', "\\")
    )
}

/// The `file:` URL of an absolute path of this computer: `C:\a b\x.html` is
/// `file:///C:/a%20b/x.html`, `\\server\share\x` is `file://server/share/x`, and
/// `/a/b` is `file:///a/b`.
pub fn file_url(path: &str, windows: bool) -> String {
    let (host, path) = match path.strip_prefix(r"\\").filter(|_| windows) {
        Some(share) => {
            let (server, rest) = share.split_once('\\').unwrap_or((share, ""));
            (server, format!("/{rest}"))
        }
        None if windows => ("", format!("/{path}")),
        None => ("", path.to_owned()),
    };
    let mut url = format!("file://{host}");
    for byte in path.bytes() {
        match byte {
            b'\\' if windows => url.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_paths_are_relative_to_the_top_level() {
        assert_eq!(
            repo_relative("/home/me/proj", "/home/me/proj/src/a.rs").as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            repo_relative("/home/me/proj", "/home/me/project/a.rs"),
            None
        );
        assert_eq!(repo_relative("/home/me/proj", "/home/me/proj"), None);
        assert_eq!(repo_relative("/home/me/proj", "/home/me/Proj/a.rs"), None);
        assert_eq!(
            repo_relative(r"C:\Users\Me\Proj", r"c:\users\me\proj\Src\A.rs").as_deref(),
            Some("Src/A.rs")
        );
        assert_eq!(
            work_tree_path(r"C:\Users\Me\Proj", "src/a.rs"),
            r"C:\Users\Me\Proj\src\a.rs"
        );
        assert_eq!(
            work_tree_path("/home/me/proj/", "src/a.rs"),
            "/home/me/proj/src/a.rs"
        );
    }

    #[test]
    fn files_of_this_computer_have_file_urls() {
        assert_eq!(
            file_url(r"C:\Users\me\my app\index.html", true),
            "file:///C:/Users/me/my%20app/index.html"
        );
        assert_eq!(
            file_url(r"\\wsl.localhost\Ubuntu\home\me\app\index.html", true),
            "file://wsl.localhost/Ubuntu/home/me/app/index.html"
        );
        assert_eq!(
            file_url("/home/me/café/a#b?.html", false),
            "file:///home/me/caf%C3%A9/a%23b%3F.html"
        );
        // A backslash is part of a Unix name.
        assert_eq!(file_url(r"/tmp/a\b", false), "file:///tmp/a%5Cb");
    }
}

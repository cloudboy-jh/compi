//! Paths of content panes in their environment's syntax rather than this computer's:
//! `/` paths for WSL and Unix hosts; drive or UNC roots with either slash for Windows
//! hosts. `compi files`/`compi edit` and terminal file links resolve paths the same way.

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

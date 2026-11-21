//! Projects: the folder a tab or thread belongs to (the Git top level above where it
//! started, else that folder), the icon image the folder holds, and an index of its files
//! that fuzzy searches answer from.

mod checkout;

pub(crate) use checkout::environments;

use crate::content_files::{self, Failure};
use crate::file_tree::{self, Root};
use compi_protocol::{ContentEnvironment, ErrorCode, ImageFormat, Project, ProjectIconImage};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const MAX_PATH_BYTES: usize = 4096;
/// How long a folder's project is remembered.
const ROOT_TTL: Duration = Duration::from_secs(60);
/// Folders, and projects with an icon looked up, remembered at most.
const MAX_CACHED: usize = 512;

/// The icon files a project's folder may hold, the first found winning (T3 Code's list).
const ICON_FILES: [&str; 23] = [
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "app/favicon.ico",
    "app/favicon.png",
    "app/icon.svg",
    "app/icon.png",
    "app/icon.ico",
    "src/favicon.ico",
    "src/favicon.svg",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    ".idea/icon.svg",
    "Resources/AppIcon.icns",
    "AppIcon.icns",
];
/// Pages and route files whose icon links name the icon when no icon file does.
const ICON_PAGES: [&str; 7] = [
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];
/// The part of a page read for icon links.
const MAX_PAGE_BYTES: u64 = 128 * 1024;
/// An icon file larger than this is not sent.
const MAX_ICON_BYTES: u64 = 1024 * 1024;
const ICON_HIT_TTL: Duration = Duration::from_secs(10 * 60);
const ICON_MISS_TTL: Duration = Duration::from_secs(60);

/// A file index older than this is listed again on the next search.
const INDEX_TTL: Duration = Duration::from_secs(60);
/// Projects whose file index is kept; the one searched longest ago goes first.
const MAX_INDEXES: usize = 16;
const MAX_INDEX_FILES: usize = 200_000;
/// A folder walk shows what it has read every this many files until its first listing.
const PUBLISH_EVERY: usize = 2_000;
/// Folders a walk outside Git leaves out.
const SKIPPED_FOLDERS: [&str; 3] = [".git", "node_modules", "target"];
const MAX_QUERY_BYTES: usize = 256;
const MAX_RESULTS: usize = 100;

type RootCache = HashMap<(ContentEnvironment, String), (Instant, Option<Project>)>;
static ROOTS: LazyLock<Mutex<RootCache>> = LazyLock::new(Mutex::default);

/// The project `folder`, an absolute folder of `environment`, belongs to: the nearest of
/// it and its ancestors holding `.git` (a repository's folder, or the file of a worktree or
/// submodule), else the folder itself. `None` when `folder` is not an absolute folder this
/// host can reach there. Answers are remembered for a minute.
pub(crate) fn project_of(environment: &ContentEnvironment, folder: &str) -> Option<Project> {
    let folder = normalized(environment, folder)?;
    let key = (environment.clone(), folder);
    if let Some((at, project)) = ROOTS.lock().get(&key)
        && at.elapsed() < ROOT_TTL
    {
        return project.clone();
    }
    let project = find_project(environment, &key.1);
    remember_project(key, project.clone());
    project
}

fn remember_project(key: (ContentEnvironment, String), project: Option<Project>) {
    let mut cache = ROOTS.lock();
    if cache.len() >= MAX_CACHED {
        cache.retain(|_, (at, _)| at.elapsed() < ROOT_TTL);
        if cache.len() >= MAX_CACHED {
            cache.clear();
        }
    }
    cache.insert(key, (Instant::now(), project));
}

fn find_project(environment: &ContentEnvironment, folder: &str) -> Option<Project> {
    let host = share_root(environment, folder);
    if host.is_none() {
        // The Windows file provider cannot represent every valid Linux path.
        checkout::validate_folder(environment, folder).ok()?;
    }
    Some(root_of(environment, folder, host))
}

/// Validate independently of the tab cache: an added folder must exist right now.
pub(crate) fn resolve_folder(
    environment: &ContentEnvironment,
    folder: &str,
) -> Result<Project, Failure> {
    let folder = absolute_folder(environment, folder)?;
    checkout::validate_folder(environment, &folder)?;
    let project = root_of(environment, &folder, share_root(environment, &folder));
    remember_project((environment.clone(), folder), Some(project.clone()));
    Ok(project)
}

pub(crate) fn clone_repository(
    environment: ContentEnvironment,
    url: &str,
    destination: &str,
) -> Result<Project, Failure> {
    let root = absolute_folder(&environment, destination)?;
    checkout::clone_repository(&environment, url, &root)?;
    // A successful clone has its own .git, so its project root is the new folder itself.
    let project = Project { environment, root };
    remember_project(
        (project.environment.clone(), project.root.clone()),
        Some(project.clone()),
    );
    Ok(project)
}

fn absolute_folder(environment: &ContentEnvironment, folder: &str) -> Result<String, Failure> {
    normalized(environment, folder).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            "Enter an absolute folder path in the selected environment (without '..').".into(),
        )
    })
}

fn root_of(environment: &ContentEnvironment, folder: &str, host: Option<Root>) -> Project {
    let walked = host.and_then(|root| {
        root.host
            .ancestors()
            .position(|ancestor| ancestor.join(".git").symlink_metadata().is_ok())
    });
    let root = decide_root(environment, folder, walked, || match environment {
        ContentEnvironment::Host => None,
        ContentEnvironment::Wsl { .. } => {
            crate::git::work_tree_of(environment, folder).ok().flatten()
        }
    });
    Project {
        environment: environment.clone(),
        root,
    }
}

/// The project root of `folder`: its ancestor `walked` components up when a walk found
/// `.git` there. Else, in WSL, where Git in the distribution places it, since the walk
/// through the WSL share cannot follow a link out of the distribution (into a drive mount);
/// `ask_git` gives Git's top level and `folder`'s path below it, `None` outside a work
/// tree. Else the folder itself.
fn decide_root(
    environment: &ContentEnvironment,
    folder: &str,
    walked: Option<usize>,
    ask_git: impl FnOnce() -> Option<(String, String)>,
) -> String {
    if let Some(depth) = walked {
        return ancestor(environment, folder, depth);
    }
    match ask_git() {
        Some((top, prefix)) => git_root(environment, folder, &top, &prefix),
        None => folder.to_owned(),
    }
}

/// The top level Git reports for `folder`, named as `folder` names it: `folder` without the
/// components of `prefix`, its path below the top level (`a/b/`), when it ends with them.
/// Else `top` as Git gives it, with links resolved.
fn git_root(environment: &ContentEnvironment, folder: &str, top: &str, prefix: &str) -> String {
    let below: Vec<&str> = prefix.split('/').filter(|part| !part.is_empty()).collect();
    let segments: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    if segments.ends_with(&below) {
        ancestor(environment, folder, below.len())
    } else {
        top.to_owned()
    }
}

/// `folder` without its last `depth` components, in `environment`'s syntax.
fn ancestor(environment: &ContentEnvironment, folder: &str, depth: usize) -> String {
    match environment {
        ContentEnvironment::Host => Path::new(folder)
            .ancestors()
            .nth(depth)
            .and_then(Path::to_str)
            .unwrap_or(folder)
            .to_owned(),
        ContentEnvironment::Wsl { .. } => {
            let mut top = folder;
            for _ in 0..depth {
                top = &top[..top.rfind('/').unwrap_or(0)];
            }
            if top.is_empty() { "/" } else { top }.to_owned()
        }
    }
}

/// `folder` as an absolute folder of `environment` in its own syntax, without `.`
/// components or a trailing separator; `None` when it is not one. On a Windows host, a WSL
/// drive mount (`/mnt/c/x`) or a `file://`-style drive path (`/C:/x`) is the Windows folder
/// it names.
pub(crate) fn normalized(environment: &ContentEnvironment, folder: &str) -> Option<String> {
    if folder.is_empty() || folder.len() > MAX_PATH_BYTES || folder.contains('\0') {
        return None;
    }
    match environment {
        #[cfg(windows)]
        ContentEnvironment::Host => {
            use std::path::Component;
            let native = crate::launch::windows_directory(folder)?;
            let path: PathBuf = Path::new(&native).components().collect();
            if path.components().any(|part| part == Component::ParentDir) {
                return None;
            }
            let path = path.to_str()?;
            file_tree::resolve_host_root(path).ok()?;
            Some(path.to_owned())
        }
        #[cfg(not(windows))]
        ContentEnvironment::Host => unix_folder(folder),
        ContentEnvironment::Wsl { .. } => unix_folder(folder),
    }
}

fn unix_folder(folder: &str) -> Option<String> {
    if !folder.starts_with('/') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in folder.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            segment => segments.push(segment),
        }
    }
    Some(format!("/{}", segments.join("/")))
}

/// Where this host reads `folder`, a `normalized` folder of `environment`: like
/// `share_root`, except that a WSL folder the share cannot read because it lies behind a
/// link out of the distribution is read where WSL resolves it.
fn host_root(environment: &ContentEnvironment, folder: &str) -> Option<Root> {
    let root = share_root(environment, folder)?;
    #[cfg(windows)]
    if let ContentEnvironment::Wsl { distribution } = environment
        && root.distribution.is_some()
    {
        let readable = match fs::metadata(&root.host) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) => !file_tree::needs_wsl_resolution(&error),
        };
        if !readable && let Some(linked) = linked_folder(distribution, folder) {
            return file_tree::resolve_host_root(&linked).ok();
        }
    }
    Some(root)
}

/// The Windows path WSL resolves `folder` of `distribution` to, remembered for a minute,
/// failures too, since each answer starts a WSL process.
#[cfg(windows)]
fn linked_folder(distribution: &str, folder: &str) -> Option<String> {
    type Cache = HashMap<(String, String), (Instant, Option<String>)>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);
    let key = (distribution.to_owned(), folder.to_owned());
    if let Some((at, linked)) = CACHE.lock().get(&key)
        && at.elapsed() < ROOT_TTL
    {
        return linked.clone();
    }
    let linked = compi_protocol::wsl::windows_path_for_wsl(folder, distribution)
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok());
    let mut cache = CACHE.lock();
    if cache.len() >= MAX_CACHED {
        cache.clear();
    }
    cache.insert(key, (Instant::now(), linked.clone()));
    linked
}

/// Where this host reaches `folder`, a `normalized` folder of `environment`. A WSL folder on
/// a Windows drive is read on the drive itself, since the WSL share refuses drive mounts;
/// any other WSL folder through its distribution's share.
fn share_root(environment: &ContentEnvironment, folder: &str) -> Option<Root> {
    match environment {
        ContentEnvironment::Host => content_files::resolve(environment, folder).ok(),
        #[cfg(windows)]
        ContentEnvironment::Wsl { .. } => {
            if folder.starts_with("/mnt/")
                && let Some(native) = crate::launch::windows_directory(folder)
            {
                return file_tree::resolve_host_root(&native).ok();
            }
            content_files::resolve(environment, folder).ok()
        }
        #[cfg(not(windows))]
        ContentEnvironment::Wsl { .. } => None,
    }
}

/// `relative`, a `/`-separated path, below `root`.
fn below(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative.split('/').filter(|part| !part.is_empty()) {
        path.push(segment);
    }
    path
}

/// An icon found: the file it came from, as it was when read, and its image.
#[derive(Clone)]
struct FoundIcon {
    path: PathBuf,
    stamp: (u64, Option<SystemTime>),
    image: ProjectIconImage,
}

/// The icon image `project`'s folder holds: the first of `ICON_FILES` it holds, else the
/// first file an icon link of `ICON_PAGES` names, tried under `public/` and then as it is.
/// `None` when it holds none, or the one it holds is larger than 1 MiB or not an image
/// format clients draw. An icon found is remembered for 10 minutes, as long as its file is
/// unchanged, and the lack of one for a minute.
pub(crate) fn icon(project: &Project) -> Option<ProjectIconImage> {
    type Cache = HashMap<Project, (Instant, Option<FoundIcon>)>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);
    let cached = CACHE.lock().get(project).cloned();
    if let Some((at, found)) = cached {
        match found {
            None if at.elapsed() < ICON_MISS_TTL => return None,
            Some(found)
                if at.elapsed() < ICON_HIT_TTL
                    && fs::metadata(&found.path).is_ok_and(|metadata| {
                        (metadata.len(), metadata.modified().ok()) == found.stamp
                    }) =>
            {
                return Some(found.image);
            }
            _ => {}
        }
    }
    let found = find_icon(project);
    let mut cache = CACHE.lock();
    if cache.len() >= MAX_CACHED {
        cache.clear();
    }
    cache.insert(project.clone(), (Instant::now(), found.clone()));
    found.map(|found| found.image)
}

fn find_icon(project: &Project) -> Option<FoundIcon> {
    let folder = normalized(&project.environment, &project.root)?;
    let root = host_root(&project.environment, &folder)?.host;
    for relative in ICON_FILES {
        if let Some(found) = probe_icon(&root, relative) {
            return found;
        }
    }
    for page in ICON_PAGES {
        let Some(text) = read_page(&below(&root, page)) else {
            continue;
        };
        for link in icon_links(&text) {
            let Some(link) = link_path(link) else {
                continue;
            };
            for relative in [format!("public/{link}"), link] {
                if let Some(found) = probe_icon(&root, &relative) {
                    return found;
                }
            }
        }
    }
    None
}

/// `None` when `relative` below `root` is no icon file: missing, not a regular file, not
/// an image format clients draw (`.icns`), or not holding that format. `Some(None)` for
/// one too large to send.
fn probe_icon(root: &Path, relative: &str) -> Option<Option<FoundIcon>> {
    let format = ImageFormat::of_path(relative)?;
    let path = below(root, relative);
    let metadata = fs::metadata(&path).ok().filter(fs::Metadata::is_file)?;
    if metadata.len() > MAX_ICON_BYTES {
        return Some(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&path)
        .ok()?
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Some(None);
    }
    format.matches(&bytes).then(|| {
        Some(FoundIcon {
            path,
            stamp: (metadata.len(), metadata.modified().ok()),
            image: ProjectIconImage { format, bytes },
        })
    })
}

/// The first `MAX_PAGE_BYTES` of a regular file, as text.
fn read_page(path: &Path) -> Option<String> {
    fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_PAGE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The `href`s of icon links in `text`, in order: HTML or JSX `<link rel="icon" href=…>`
/// tags (`rel="shortcut icon"` too), then script objects such as
/// `{ rel: "icon", href: "/favicon.svg" }`, as route heads write them.
fn icon_links(text: &str) -> Vec<&str> {
    let mut links = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("<link") {
        let start = from + found + "<link".len();
        let end = lower[start..]
            .find('>')
            .map_or(lower.len(), |end| start + end);
        push_icon_link(attributes(&text[start..end], b'='), &mut links);
        from = end;
    }
    let mut open = None;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'{' => open = Some(index),
            b'}' => {
                if let Some(start) = open.take() {
                    push_icon_link(attributes(&text[start + 1..index], b':'), &mut links);
                }
            }
            _ => {}
        }
    }
    links
}

/// The `href` of a tag's or object's `attributes` when its `rel` names an icon.
fn push_icon_link<'a>(attributes: Vec<(String, &'a str)>, links: &mut Vec<&'a str>) {
    let icon = attributes.iter().any(|(key, value)| {
        key == "rel"
            && matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "icon" | "shortcut icon"
            )
    });
    if icon && let Some((_, href)) = attributes.iter().find(|(key, _)| key == "href") {
        links.push(href);
    }
}

/// The `key<separator>value` pairs of `segment`, keys lowercased: attributes of a tag
/// (`=`) or properties of a script object (`:`). Keys may be quoted; values are quoted or
/// run to the next space, comma or `>`.
fn attributes(segment: &str, separator: u8) -> Vec<(String, &str)> {
    let bytes = segment.as_bytes();
    let skip_space = |mut index: usize| {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        index
    };
    let quoted = |index: usize| {
        let quote = bytes[index];
        let end = index + 1 + segment[index + 1..].find(quote as char)?;
        Some((&segment[index + 1..end], end + 1))
    };
    let is_key = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let (key, next) = match bytes[index] {
            b'"' | b'\'' => match quoted(index) {
                Some(found) => found,
                None => break,
            },
            byte if is_key(byte) => {
                let end = index
                    + bytes[index..]
                        .iter()
                        .position(|byte| !is_key(*byte))
                        .unwrap_or(bytes.len() - index);
                (&segment[index..end], end)
            }
            _ => {
                index += 1;
                continue;
            }
        };
        index = skip_space(next);
        if bytes.get(index) != Some(&separator) {
            continue;
        }
        index = skip_space(index + 1);
        let (value, next) = match bytes.get(index) {
            Some(b'"' | b'\'' | b'`') => match quoted(index) {
                Some(found) => found,
                None => break,
            },
            Some(_) => {
                let end = index
                    + bytes[index..]
                        .iter()
                        .position(|byte| byte.is_ascii_whitespace() || b",>".contains(byte))
                        .unwrap_or(bytes.len() - index);
                (&segment[index..end], end)
            }
            None => break,
        };
        pairs.push((key.to_ascii_lowercase(), value));
        index = next;
    }
    pairs
}

/// An icon link's path inside the project: without its query, fragment and leading `/`.
/// `None` for a link elsewhere (another host, a drive, `data:`) or leaving the folder.
fn link_path(href: &str) -> Option<String> {
    let path = href.split(['?', '#']).next()?.trim();
    if path.starts_with("//") || path.contains([':', '\\']) {
        return None;
    }
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(path.to_owned())
}

/// A project's file list, as last read.
struct FileIndex {
    /// Paths relative to the project's folder, `/`-separated.
    files: Arc<Vec<String>>,
    /// When the last complete listing finished; `None` before the first.
    listed: Option<Instant>,
    /// A listing is being read.
    listing: bool,
    /// Files may have changed since the listing being read, or the last one, began.
    stale: bool,
    searched: Instant,
}

static INDEXES: LazyLock<Mutex<HashMap<Project, FileIndex>>> = LazyLock::new(Mutex::default);

/// Up to 100 files of `project` whose path below its folder holds `query`'s letters in
/// order, best first, as absolute paths of its environment, and whether its file list is
/// still being read. The list is read in the background on the first search, and again on
/// a search once it is a minute old or a turn of a chat in the project has ended; until a
/// listing ends, searches answer from the one before. An empty query finds nothing.
pub(crate) fn search_files(project: &Project, query: &str) -> Result<(Vec<String>, bool), Failure> {
    if query.len() > MAX_QUERY_BYTES || query.contains('\0') {
        return Err((
            ErrorCode::InvalidRequest,
            "search query must contain at most 256 bytes and no NUL".into(),
        ));
    }
    let folder = normalized(&project.environment, &project.root).ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            format!(
                "{:?} is not an absolute folder in {}",
                project.root, project.environment
            ),
        )
    })?;
    let (files, listing) = {
        let mut indexes = INDEXES.lock();
        if !indexes.contains_key(project)
            && indexes.len() >= MAX_INDEXES
            && let Some(oldest) = indexes
                .iter()
                .filter(|(_, index)| !index.listing)
                .min_by_key(|(_, index)| index.searched)
                .map(|(project, _)| project.clone())
        {
            indexes.remove(&oldest);
        }
        let index = indexes.entry(project.clone()).or_insert_with(|| FileIndex {
            files: Arc::default(),
            listed: None,
            listing: false,
            stale: false,
            searched: Instant::now(),
        });
        index.searched = Instant::now();
        if !index.listing
            && (index.stale || index.listed.is_none_or(|at| at.elapsed() >= INDEX_TTL))
        {
            index.listing = true;
            index.stale = false;
            let project = project.clone();
            let folder = folder.clone();
            thread::spawn(move || {
                let files = list_files(&project, &folder);
                if let Some(index) = INDEXES.lock().get_mut(&project) {
                    index.files = Arc::new(files);
                    index.listed = Some(Instant::now());
                    index.listing = false;
                }
            });
        }
        (index.files.clone(), index.listing)
    };
    let windows = cfg!(windows) && project.environment == ContentEnvironment::Host;
    let separator = if windows { '\\' } else { '/' };
    let paths = rank(&files, query, MAX_RESULTS)
        .into_iter()
        .map(|relative| {
            let relative = if windows {
                relative.replace('/', "\\")
            } else {
                relative.to_owned()
            };
            if folder.ends_with(separator) {
                format!("{folder}{relative}")
            } else {
                format!("{folder}{separator}{relative}")
            }
        })
        .collect();
    Ok((paths, listing))
}

/// Marks the file index of the project holding `folder` stale, so the next search lists
/// its files again.
pub(crate) fn files_changed(environment: &ContentEnvironment, folder: &str) {
    for (project, index) in INDEXES.lock().iter_mut() {
        let root = project.root.trim_end_matches(['/', '\\']);
        if project.environment == *environment
            && folder.starts_with(root)
            && (folder.len() == root.len() || folder[root.len()..].starts_with(['/', '\\']))
        {
            index.stale = true;
        }
    }
}

/// The files of `project`, whose `normalized` folder is `folder`: what Git lists in a
/// repository's folder, else a walk of the folder within the search limits, leaving out
/// `SKIPPED_FOLDERS`.
fn list_files(project: &Project, folder: &str) -> Vec<String> {
    let Some(root) = host_root(&project.environment, folder) else {
        return Vec::new();
    };
    if root.host.join(".git").symlink_metadata().is_ok()
        && let Ok(mut files) = crate::git::list_files(&project.environment, folder)
    {
        files.truncate(MAX_INDEX_FILES);
        return files;
    }
    let mut files = Vec::new();
    file_tree::walk_files(root, &SKIPPED_FOLDERS, &mut |path| {
        files.push(path);
        if files.len() % PUBLISH_EVERY == 0
            && let Some(index) = INDEXES.lock().get_mut(project)
            && index.listed.is_none()
        {
            index.files = Arc::new(files.clone());
        }
    });
    files
}

/// Up to `limit` of `files` holding `query`'s letters in order, ignoring case and spaces,
/// best first; ties go to the shorter path.
fn rank<'a>(files: &'a [String], query: &str, limit: usize) -> Vec<&'a str> {
    let query: Vec<char> = query
        .chars()
        .filter(|letter| !letter.is_whitespace())
        .map(fold)
        .collect();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut scorer = Scorer::default();
    let mut scored: Vec<(i32, &str)> = files
        .iter()
        .filter_map(|path| Some((scorer.score(path, &query)?, path.as_str())))
        .collect();
    let order = |a: &(i32, &str), b: &(i32, &str)| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.cmp(b.1))
    };
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit - 1, order);
        scored.truncate(limit);
    }
    scored.sort_unstable_by(order);
    scored.into_iter().map(|(_, path)| path).collect()
}

fn fold(letter: char) -> char {
    if letter.is_ascii() {
        letter.to_ascii_lowercase()
    } else {
        letter.to_lowercase().next().unwrap_or(letter)
    }
}

/// Every matched letter scores `MATCH`, more at the start of a path segment or a word
/// (after `-`, `_`, `.` or a space, at a capital after a small letter, at a digit after
/// a non-digit) and in the file name; a letter right after the previous match adds
/// `CONSECUTIVE`, and each letter skipped between matches costs `GAP`. Longer paths lose a
/// point per four letters.
const MATCH: i32 = 1;
const SEGMENT_START: i32 = 10;
const WORD_START: i32 = 7;
const IN_NAME: i32 = 4;
const CONSECUTIVE: i32 = 6;
const GAP: i32 = 1;
const UNMATCHED: i32 = i32::MIN / 2;

/// Scores paths against a query, reusing its buffers from path to path.
#[derive(Default)]
struct Scorer {
    letters: Vec<char>,
    folded: Vec<char>,
    bonus: Vec<i32>,
    previous: Vec<i32>,
    current: Vec<i32>,
}

impl Scorer {
    /// The best score of `query` (folded, without spaces) over `path`: the alignment of its
    /// letters scoring most. `None` when `path` does not hold them in order.
    fn score(&mut self, path: &str, query: &[char]) -> Option<i32> {
        let mut next = 0;
        for letter in path.chars() {
            if next < query.len() && fold(letter) == query[next] {
                next += 1;
            }
        }
        if next < query.len() {
            return None;
        }
        self.letters.clear();
        self.letters.extend(path.chars());
        self.folded.clear();
        self.folded.extend(self.letters.iter().copied().map(fold));
        let len = self.letters.len();
        let name_start = self
            .letters
            .iter()
            .rposition(|letter| matches!(letter, '/' | '\\'))
            .map_or(0, |separator| separator + 1);
        self.bonus.clear();
        for index in 0..len {
            let letter = self.letters[index];
            let mut bonus = MATCH;
            match index.checked_sub(1).map(|before| self.letters[before]) {
                None | Some('/' | '\\') => bonus += SEGMENT_START,
                Some(before)
                    if matches!(before, '-' | '_' | '.' | ' ')
                        || (before.is_lowercase() && letter.is_uppercase())
                        || (!before.is_ascii_digit() && letter.is_ascii_digit()) =>
                {
                    bonus += WORD_START;
                }
                Some(_) => {}
            }
            if index >= name_start {
                bonus += IN_NAME;
            }
            self.bonus.push(bonus);
        }
        self.previous.clear();
        self.previous.extend((0..len).map(|index| {
            if self.folded[index] == query[0] {
                self.bonus[index]
            } else {
                UNMATCHED
            }
        }));
        for &wanted in &query[1..] {
            self.current.clear();
            self.current.resize(len, UNMATCHED);
            // The best score of a match at least two letters back, less the letters between.
            let mut gapped = UNMATCHED;
            for index in 1..len {
                if index >= 2 {
                    gapped = gapped.max(self.previous[index - 2]) - GAP;
                }
                if self.folded[index] != wanted {
                    continue;
                }
                let best = (self.previous[index - 1] + CONSECUTIVE).max(gapped);
                if best > UNMATCHED / 2 {
                    self.current[index] = best + self.bonus[index];
                }
            }
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        let best = self.previous.iter().copied().max()?;
        (best > UNMATCHED / 2).then(|| best - (len / 4) as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "compi-project-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = below(&self.0, relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        fn dir(&self, relative: &str) -> String {
            let path = below(&self.0, relative);
            fs::create_dir_all(&path).unwrap();
            text(&path)
        }

        fn project(&self) -> Project {
            Project {
                environment: ContentEnvironment::Host,
                root: text(&self.0),
            }
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn a_folder_belongs_to_the_nearest_git_top_level_above_it() {
        let folder = Folder::new();
        let repo = folder.dir("repo");
        folder.dir("repo/.git");
        let deep = folder.dir("repo/src/deep");
        let worktree = folder.dir("worktree");
        folder.write("worktree/.git", b"gitdir: /elsewhere/.git/worktrees/x\n");
        let inside = folder.dir("worktree/crates");
        let host = ContentEnvironment::Host;
        let root = |path: &str| project_of(&host, path).unwrap().root;
        assert_eq!(root(&deep), repo);
        assert_eq!(root(&repo), repo);
        assert_eq!(root(&inside), worktree);
        // Outside any repository the folder is its own project, unless the temporary
        // folder itself lies inside one.
        let plain = folder.dir("plain/sub");
        if !folder
            .0
            .ancestors()
            .any(|ancestor| ancestor.join(".git").exists())
        {
            assert_eq!(root(&plain), plain);
        }
        assert_eq!(project_of(&host, "relative/folder"), None);
    }

    #[test]
    fn a_project_picker_folder_has_the_tab_identity_but_must_exist_now() {
        let folder = Folder::new();
        let repo = folder.dir("repo");
        folder.dir("repo/.git");
        let nested = folder.dir("repo/src");
        let host = ContentEnvironment::Host;
        let tab_project = project_of(&host, &nested).unwrap();
        assert_eq!(tab_project.root, repo);
        assert_eq!(resolve_folder(&host, &nested).unwrap(), tab_project);

        fs::remove_dir(&nested).unwrap();
        assert_eq!(
            resolve_folder(&host, &nested).unwrap_err().0,
            ErrorCode::InvalidRequest
        );
        folder.write("repo/src", b"now a file");
        assert_eq!(
            resolve_folder(&host, &nested).unwrap_err().0,
            ErrorCode::InvalidRequest
        );
        assert!(resolve_folder(&host, "relative").is_err());
    }

    #[test]
    fn wsl_roots_the_walk_misses_come_from_git() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        let folder = "/home/me/proj/compi/crates/daemon/src";
        let no_git = || -> Option<(String, String)> { panic!("the walk found the root") };
        // A walk that finds `.git` decides without Git.
        assert_eq!(
            decide_root(&wsl, folder, Some(3), no_git),
            "/home/me/proj/compi"
        );
        // Behind a link into a drive mount, Git's prefix names the root as the folder does.
        let linked = || {
            Some((
                "/mnt/c/Users/me/Proj/compi".to_owned(),
                "crates/daemon/src/".to_owned(),
            ))
        };
        assert_eq!(
            decide_root(&wsl, folder, None, linked),
            "/home/me/proj/compi"
        );
        let top = || Some(("/home/me/repo".to_owned(), String::new()));
        assert_eq!(
            decide_root(&wsl, "/home/me/repo", None, top),
            "/home/me/repo"
        );
        // A prefix the folder does not end with leaves Git's own top level.
        let elsewhere = || Some(("/mnt/c/repo".to_owned(), "other/src/".to_owned()));
        assert_eq!(decide_root(&wsl, folder, None, elsewhere), "/mnt/c/repo");
        // Outside any repository the folder is its own project.
        assert_eq!(decide_root(&wsl, folder, None, || None), folder);
    }

    #[test]
    fn icons_follow_the_lookup_order() {
        let folder = Folder::new();
        folder.write(
            "index.html",
            b"<html><head><link rel=\"icon\" href=\"/static/mark.png?v=2\"></head></html>",
        );
        folder.write("static/mark.png", PNG);
        // An icon link counts only once no well-known file is there.
        let found = icon(&folder.project()).unwrap();
        assert_eq!(found.format, ImageFormat::Png);
        assert_eq!(found.bytes, PNG);

        let folder = Folder::new();
        folder.write("index.html", b"<link rel='icon' href='/mark.png'>");
        folder.write("public/favicon.svg", SVG);
        folder.write("mark.png", PNG);
        let found = icon(&folder.project()).unwrap();
        assert_eq!(found.format, ImageFormat::Svg);
        assert_eq!(found.bytes, SVG);

        // A link is tried under public/ first.
        let folder = Folder::new();
        folder.write(
            "src/routes/__root.tsx",
            b"head: () => ({ links: [{ rel: \"icon\", type: \"image/png\", href: \"/mark.png\" }] })",
        );
        folder.write("mark.png", PNG);
        folder.write("public/mark.png", b"\x89PNG\r\n\x1a\npublic");
        assert_eq!(
            icon(&folder.project()).unwrap().bytes,
            b"\x89PNG\r\n\x1a\npublic"
        );

        // A root favicon wins over public/, and a file not holding its format is passed over.
        let folder = Folder::new();
        folder.write("favicon.svg", b"not an image");
        folder.write("favicon.ico", &[0, 0, 1, 0, 1, 0]);
        folder.write("public/favicon.svg", SVG);
        assert_eq!(icon(&folder.project()).unwrap().format, ImageFormat::Ico);

        let folder = Folder::new();
        folder.write("README.md", b"# no icon");
        assert_eq!(icon(&folder.project()), None);
    }

    #[test]
    fn icon_links_come_from_tags_and_route_objects() {
        let page = r#"<LINK href="/a.svg" REL="Shortcut Icon"/><link rel="stylesheet" href="/s.css">
            {rel:'icon',href:'/b.png'} { "rel": "apple-touch-icon", "href": "/c.png" }"#;
        assert_eq!(icon_links(page), ["/a.svg", "/b.png"]);
        assert_eq!(link_path("/x/y.png?v=1#z").as_deref(), Some("x/y.png"));
        assert_eq!(link_path("https://cdn/x.png"), None);
        assert_eq!(link_path("../x.png"), None);
        assert_eq!(link_path("//cdn/x.png"), None);
    }

    fn ranked<'a>(files: &'a [String], query: &str) -> Vec<&'a str> {
        rank(files, query, MAX_RESULTS)
    }

    fn paths(files: &[&str]) -> Vec<String> {
        files.iter().map(|file| (*file).to_owned()).collect()
    }

    #[test]
    fn fuzzy_search_favours_file_names_word_starts_and_short_paths() {
        let files = paths(&[
            "docs/web/research/kits/rust/src/notes.rs",
            "crates/compi-client/src/gui/workspace/drawers.rs",
            "crates/compi-daemon/src/workspace_store.rs",
            "crates/compi-daemon/src/workspace.rs",
            "README.md",
        ]);
        let found = ranked(&files, "wrkrs");
        assert_eq!(found[0], "crates/compi-daemon/src/workspace.rs");
        assert_eq!(found[1], "crates/compi-daemon/src/workspace_store.rs");
        assert!(!found.contains(&"README.md"));
        assert_eq!(found.len(), 4);

        // A match in the file name beats the same letters in a folder.
        let files = paths(&["main/lib.rs", "src/main.rs"]);
        assert_eq!(ranked(&files, "main"), ["src/main.rs", "main/lib.rs"]);
        // Case and spaces do not matter; an empty query finds nothing.
        assert_eq!(ranked(&files, "MA IN")[0], "src/main.rs");
        assert!(ranked(&files, " ").is_empty());
        assert!(ranked(&files, "zz").is_empty());
    }

    #[test]
    fn searches_answer_from_the_index_once_read() {
        let folder = Folder::new();
        folder.write("src/workspace.rs", b"");
        folder.write("node_modules/pkg/workspace.js", b"");
        folder.write("docs/notes.md", b"");
        let project = folder.project();
        let deadline = Instant::now() + Duration::from_secs(10);
        let paths = loop {
            let (paths, indexing) = search_files(&project, "wrk").unwrap();
            if !indexing {
                break paths;
            }
            assert!(Instant::now() < deadline, "the index was never read");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(paths, [text(&below(&folder.0, "src/workspace.rs"))]);
        assert_eq!(search_files(&project, "").unwrap().0, Vec::<String>::new());
        files_changed(&ContentEnvironment::Host, &text(&below(&folder.0, "src")));
        assert!(INDEXES.lock()[&project].stale);
    }
}

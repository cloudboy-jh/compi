//! Files of content panes: folder listings, bounded UTF-8 reads, and saves that never
//! overwrite changes made on disk after the editor read the file.

use crate::file_tree::{self, Root};
use compi_protocol::{
    ContentEnvironment, DirectoryEntry, ErrorCode, FileVersion, MAX_CONTENT_FILE_BYTES,
};
use parking_lot::{Condvar, Mutex};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A rejected request: its protocol error code and a message for the user.
pub type Failure = (ErrorCode, String);

/// Immediate children of an absolute folder, directories first.
pub fn list_directory(
    environment: &ContentEnvironment,
    path: &str,
) -> Result<Vec<DirectoryEntry>, Failure> {
    let entries = file_tree::list_root(resolve(environment, path)?).map_err(|error| {
        (
            ErrorCode::Internal,
            format!("could not list folder: {error}"),
        )
    })?;
    Ok(entries
        .into_iter()
        .map(|entry| DirectoryEntry {
            name: entry.name,
            is_directory: entry.is_directory,
        })
        .collect())
}

/// A UTF-8 text file of at most `MAX_CONTENT_FILE_BYTES`, with the version a save expects.
pub fn read_file(
    environment: &ContentEnvironment,
    path: &str,
) -> Result<(String, FileVersion), Failure> {
    let root = resolve(environment, path)?;
    let (opened_path, opened) = open(&root, &open_options(false));
    let mut file = opened.map_err(|error| open_failure(&root, &opened_path, error))?;
    let bytes = read_contents(&mut file, &root)?.ok_or_else(|| {
        (
            ErrorCode::InvalidRequest,
            format!(
                "{:?} is larger than the {} MiB file size limit",
                root.shell,
                MAX_CONTENT_FILE_BYTES / (1024 * 1024)
            ),
        )
    })?;
    let version = version_of(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| {
        (
            ErrorCode::InvalidRequest,
            format!("{:?} is not a UTF-8 text file", root.shell),
        )
    })?;
    Ok((text, version))
}

/// Save `text` if the file still holds `expected`, or create it when `expected` is `None`.
/// Returns the version of the saved bytes.
pub fn write_file(
    environment: &ContentEnvironment,
    path: &str,
    expected: Option<&FileVersion>,
    text: &str,
) -> Result<FileVersion, Failure> {
    if text.len() > MAX_CONTENT_FILE_BYTES {
        return Err((
            ErrorCode::InvalidRequest,
            format!(
                "files larger than the {} MiB file size limit cannot be saved",
                MAX_CONTENT_FILE_BYTES / (1024 * 1024)
            ),
        ));
    }
    let root = resolve(environment, path)?;
    match expected {
        Some(expected) => replace(&root, expected, text)?,
        None => create(&root, text)?,
    }
    Ok(version_of(text.as_bytes()))
}

fn resolve(environment: &ContentEnvironment, path: &str) -> Result<Root, Failure> {
    let root = match environment {
        #[cfg(unix)]
        ContentEnvironment::Host => file_tree::resolve_root(path, None),
        #[cfg(windows)]
        ContentEnvironment::Host => file_tree::resolve_host_root(path),
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => {
            Err("WSL files are only available on a Windows host".into())
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            file_tree::resolve_root(path, Some(distribution))
        }
    };
    root.map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))
}

fn create(root: &Root, text: &str) -> Result<(), Failure> {
    // Exclusive creation is atomic, so no lock is needed to keep two saves from both creating.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let (created, opened) = open(root, &options);
    let mut file = opened.map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => (
            ErrorCode::FileConflict,
            format!("{:?} already exists", root.shell),
        ),
        io::ErrorKind::NotFound => (
            ErrorCode::InvalidRequest,
            format!("the folder of {:?} does not exist", root.shell),
        ),
        _ => open_failure(root, &created, error),
    })?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        // A partial file would make every retry conflict with it.
        let _ = fs::remove_file(&created);
        return Err(save_failure(root, error));
    }
    #[cfg(unix)]
    if let Some(folder) = root.host.parent() {
        File::open(folder)
            .and_then(|folder| folder.sync_all())
            .map_err(|error| save_failure(root, error))?;
    }
    Ok(())
}

fn replace(root: &Root, expected: &FileVersion, text: &str) -> Result<(), Failure> {
    let conflict = |what: &str| {
        (
            ErrorCode::FileConflict,
            format!("{:?} {what} since it was opened", root.shell),
        )
    };
    let wsl = in_wsl(root);
    // Saves through a symlink replace its target and keep the link. On the WSL share the
    // handle follows links that WSL resolves; elsewhere the canonical path does.
    let target = if wsl {
        root.host.clone()
    } else {
        fs::canonicalize(&root.host).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => conflict("was deleted"),
            _ => open_failure(root, &root.host, error),
        })?
    };
    let _saving = SaveLock::acquire(target.clone());
    let options = open_options(true);
    let (opened_path, opened) = if wsl {
        open(root, &options)
    } else {
        (target.clone(), options.open(&target))
    };
    let mut file = opened.map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied if !is_folder(&opened_path) => (
            ErrorCode::Internal,
            format!("{:?} is read-only: {error}", root.shell),
        ),
        io::ErrorKind::NotFound => conflict("was deleted"),
        _ => open_failure(root, &opened_path, error),
    })?;
    let current = read_contents(&mut file, root)?;
    if current.is_none_or(|current| version_of(&current) != *expected) {
        return Err(conflict("changed on disk"));
    }
    if wsl {
        // Windows cannot carry a Linux file's mode and owner across a rename on the WSL
        // share: a replacement would lose the executable bit. Rewrite the checked file
        // through its handle instead; the version check above still guards the contents.
        write_in_place(file, text)
    } else {
        replace_file(&target, file, text)
    }
    .map_err(|error| save_failure(root, error))
}

/// Replace the file atomically through a sibling temporary file when the replacement can
/// keep the original's identity and metadata, otherwise overwrite it in place.
fn replace_file(target: &Path, file: File, text: &str) -> io::Result<()> {
    // A rename would split hard links.
    if link_count(&file)? > 1 {
        return write_in_place(file, text);
    }
    let Some((temporary, mut created)) = TemporaryFile::create(target)? else {
        return write_in_place(file, text);
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let original = file.metadata()?;
        let replacement = created.metadata()?;
        // Only root can give the replacement another user's or group's ownership.
        if replacement.uid() != original.uid() || replacement.gid() != original.gid() {
            drop(created);
            drop(temporary);
            return write_in_place(file, text);
        }
        created.set_permissions(original.permissions())?;
    }
    created.write_all(text.as_bytes())?;
    created.sync_all()?;
    drop(created);
    drop(file);
    temporary.replace(target)
}

/// Overwrite the file through its open handle, keeping its identity, owner and mode.
fn write_in_place(mut file: File, text: &str) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)?;
    file.sync_all()
}

/// A sibling of the saved file, removed unless it replaces the file.
struct TemporaryFile {
    path: PathBuf,
    replaced: bool,
}

impl TemporaryFile {
    /// `None` when the folder does not allow new files.
    fn create(target: &Path) -> io::Result<Option<(Self, File)>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = target.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the file has no parent folder")
        })?;
        let path = folder.join(format!(
            ".compi-save-{:x}-{:x}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&path) {
            Ok(file) => Ok(Some((
                Self {
                    path,
                    replaced: false,
                },
                file,
            ))),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(None),
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn replace(mut self, target: &Path) -> io::Result<()> {
        fs::rename(&self.path, target)?;
        self.replaced = true;
        match target.parent() {
            Some(folder) => File::open(folder)?.sync_all(),
            None => Ok(()),
        }
    }

    /// `ReplaceFileW` keeps the original's attributes, security descriptor and alternate
    /// streams, which a rename would discard.
    #[cfg(windows)]
    fn replace(mut self, target: &Path) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
        use windows::core::PCWSTR;
        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (target_name, replacement_name) = (wide(target), wide(&self.path));
        // SAFETY: both names are NUL-terminated and outlive the call; no backup is requested.
        unsafe {
            ReplaceFileW(
                PCWSTR(target_name.as_ptr()),
                PCWSTR(replacement_name.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))?;
        self.replaced = true;
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.replaced {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Serializes saves of one file within this daemon, so two saves cannot both pass the
/// version check before either writes.
struct SaveLock(PathBuf);

static SAVING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static SAVED: Condvar = Condvar::new();

impl SaveLock {
    fn acquire(path: PathBuf) -> Self {
        let mut saving = SAVING.lock();
        while saving.contains(&path) {
            SAVED.wait(&mut saving);
        }
        saving.push(path.clone());
        Self(path)
    }
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        SAVING.lock().retain(|path| *path != self.0);
        SAVED.notify_all();
    }
}

fn open_options(write: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    // Opening a FIFO for reading would block until a writer appears; the file type is
    // checked once it is open.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    options
}

/// Open the file, returning the path last tried for diagnosing a failure.
fn open(root: &Root, options: &OpenOptions) -> (PathBuf, io::Result<File>) {
    let opened = options.open(&root.host);
    #[cfg(windows)]
    if let (Err(error), Some(distribution)) = (&opened, root.distribution.as_deref())
        && file_tree::needs_wsl_resolution(error)
        && let Ok(resolved) = compi_protocol::wsl::windows_path_for_wsl(&root.shell, distribution)
    {
        let opened = options.open(&resolved);
        return (resolved, opened);
    }
    (root.host.clone(), opened)
}

/// The file's bytes, or `None` when it exceeds `MAX_CONTENT_FILE_BYTES`. Reads at most one
/// byte past the limit, even if the file grows while it is read.
fn read_contents(file: &mut File, root: &Root) -> Result<Option<Vec<u8>>, Failure> {
    let read_failure = |error: io::Error| {
        (
            ErrorCode::Internal,
            format!("could not read {:?}: {error}", root.shell),
        )
    };
    let metadata = file.metadata().map_err(read_failure)?;
    if metadata.is_dir() {
        return Err(folder_failure(root));
    }
    if !metadata.is_file() {
        return Err((
            ErrorCode::InvalidRequest,
            format!("{:?} is not a regular file", root.shell),
        ));
    }
    if metadata.len() > MAX_CONTENT_FILE_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONTENT_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(read_failure)?;
    Ok((bytes.len() <= MAX_CONTENT_FILE_BYTES).then_some(bytes))
}

fn version_of(bytes: &[u8]) -> FileVersion {
    FileVersion {
        len: bytes.len() as u64,
        sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn in_wsl(root: &Root) -> bool {
    #[cfg(windows)]
    {
        root.distribution.is_some()
    }
    #[cfg(unix)]
    {
        let _ = root;
        false
    }
}

#[cfg(unix)]
fn link_count(file: &File) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink())
}

#[cfg(windows)]
fn link_count(file: &File) -> io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the duration of the call and `information` is writable.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))?;
    Ok(information.nNumberOfLinks.into())
}

fn is_folder(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn folder_failure(root: &Root) -> Failure {
    (
        ErrorCode::InvalidRequest,
        format!("{:?} is a folder, not a file", root.shell),
    )
}

/// Opening a folder fails with `IsADirectory` on Unix and access denied on Windows.
fn open_failure(root: &Root, path: &Path, error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::IsADirectory || is_folder(path) {
        return folder_failure(root);
    }
    (
        ErrorCode::Internal,
        format!("could not open {:?}: {error}", root.shell),
    )
}

fn save_failure(root: &Root, error: io::Error) -> Failure {
    (
        ErrorCode::Internal,
        format!("could not save {:?}: {error}", root.shell),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "compi-content-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> (PathBuf, String) {
            let path = self.0.join(name);
            let text = path.to_str().unwrap().to_owned();
            (path, text)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const HOST: ContentEnvironment = ContentEnvironment::Host;

    #[test]
    fn read_version_is_accepted_by_a_save_and_a_stale_save_conflicts() {
        let folder = Folder::new();
        let (path, name) = folder.file("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        let (text, opened) = read_file(&HOST, &name).unwrap();
        assert_eq!(text, "fn main() {}\n");
        assert_eq!(opened.len, 13);
        assert_eq!(opened.sha256.len(), 64);

        let saved = write_file(&HOST, &name, Some(&opened), "fn main() { run(); }\n").unwrap();
        assert_eq!(read_file(&HOST, &name).unwrap().1, saved);
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main() { run(); }\n");

        let (code, message) = write_file(&HOST, &name, Some(&opened), "stale").unwrap_err();
        assert_eq!(code, ErrorCode::FileConflict);
        assert!(
            message.contains("changed on disk since it was opened"),
            "{message}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main() { run(); }\n");

        fs::remove_file(&path).unwrap();
        let (code, _) = write_file(&HOST, &name, Some(&saved), "gone").unwrap_err();
        assert_eq!(code, ErrorCode::FileConflict);
        assert!(!path.exists());
        let leftovers = fs::read_dir(&folder.0).unwrap().count();
        assert_eq!(leftovers, 0, "no temporary file may remain");
    }

    #[test]
    fn creating_a_file_fails_when_it_exists_or_its_folder_is_missing() {
        let folder = Folder::new();
        let (path, name) = folder.file("new.txt");
        let created = write_file(&HOST, &name, None, "first").unwrap();
        assert_eq!(
            read_file(&HOST, &name).unwrap(),
            ("first".to_owned(), created)
        );

        let (code, message) = write_file(&HOST, &name, None, "second").unwrap_err();
        assert_eq!(code, ErrorCode::FileConflict);
        assert!(message.contains("already exists"), "{message}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");

        let (_, missing) = folder.file("missing");
        let nested = Path::new(&missing).join("new.txt");
        let (code, _) = write_file(&HOST, nested.to_str().unwrap(), None, "x").unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn rejects_folders_oversized_and_non_utf8_files() {
        let folder = Folder::new();
        let (large, large_name) = folder.file("large.txt");
        fs::write(&large, vec![b'a'; MAX_CONTENT_FILE_BYTES + 1]).unwrap();
        let (code, message) = read_file(&HOST, &large_name).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(message.contains("1 MiB"), "{message}");

        let (binary, binary_name) = folder.file("binary.bin");
        fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        let (code, message) = read_file(&HOST, &binary_name).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(message.contains("not a UTF-8 text file"), "{message}");

        let (code, message) = read_file(&HOST, folder.0.to_str().unwrap()).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
        assert!(message.contains("is a folder"), "{message}");

        let oversized = "a".repeat(MAX_CONTENT_FILE_BYTES + 1);
        let (code, _) = write_file(&HOST, &binary_name, None, &oversized).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn saving_a_read_only_file_fails_without_replacing_it() {
        let folder = Folder::new();
        let (path, name) = folder.file("locked.txt");
        fs::write(&path, "kept").unwrap();
        let version = read_file(&HOST, &name).unwrap().1;
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions.clone()).unwrap();

        let result = write_file(&HOST, &name, Some(&version), "replaced");
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        let (code, message) = result.unwrap_err();
        assert_eq!(code, ErrorCode::Internal);
        assert!(message.contains("read-only"), "{message}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "kept");
    }

    #[test]
    fn lists_a_folder_directories_first() {
        let folder = Folder::new();
        fs::create_dir(folder.0.join("src")).unwrap();
        fs::write(folder.0.join("Cargo.toml"), "").unwrap();
        assert_eq!(
            list_directory(&HOST, folder.0.to_str().unwrap()).unwrap(),
            [
                DirectoryEntry {
                    name: "src".into(),
                    is_directory: true,
                },
                DirectoryEntry {
                    name: "Cargo.toml".into(),
                    is_directory: false,
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn saves_through_symlinks_and_keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let folder = Folder::new();
        let (target, target_name) = folder.file("run.sh");
        fs::write(&target, "echo one\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
        let (link, link_name) = folder.file("link.sh");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let version = read_file(&HOST, &link_name).unwrap().1;
        write_file(&HOST, &link_name, Some(&version), "echo two\n").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "echo two\n");
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o751);

        let version = read_file(&HOST, &target_name).unwrap().1;
        write_file(&HOST, &target_name, Some(&version), "echo three\n").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o751);
    }

    #[cfg(unix)]
    #[test]
    fn wsl_files_are_rejected_on_unix() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert_eq!(
            read_file(&wsl, "/etc/hostname").unwrap_err().0,
            ErrorCode::InvalidRequest
        );
    }

    #[cfg(windows)]
    #[test]
    fn host_paths_must_be_absolute_windows_paths() {
        for invalid in [
            "relative\\file.txt",
            "C:file.txt",
            "\\file.txt",
            "/home/user/file.txt",
            "\\\\?\\C:\\file.txt",
            "\\\\.\\COM1",
            "C:\\bad\0name",
        ] {
            assert_eq!(
                read_file(&HOST, invalid).unwrap_err().0,
                ErrorCode::InvalidRequest,
                "{invalid:?}"
            );
        }
        assert!(file_tree::resolve_host_root("\\\\server\\share\\file.txt").is_ok());
        assert!(file_tree::resolve_host_root("C:/Users/file.txt").is_ok());
    }
}

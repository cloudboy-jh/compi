//! Blocking image preparation for background workers. Managed originals outlive UI previews.

use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use compi_protocol::MAX_DECODED_IMAGE_BYTES;
use gpui::RenderImage;
use image::{DynamicImage, Frame, ImageDecoder, ImageFormat, RgbaImage};
use sha2::{Digest, Sha256};
use smallvec::smallvec;

const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const STORE_BYTES: u64 = 512 * 1024 * 1024;
const STORE_FILES: usize = 4096;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

pub enum ImageInput {
    File(PathBuf),
    Clipboard(Vec<u8>),
}

pub enum ImageTarget {
    /// A local POSIX shell (macOS). Windows sessions use `Wsl`.
    Native,
    Wsl {
        distribution: Option<String>,
    },
}

pub struct PreparedImage {
    /// Native, accessible original path, also used by inspector copy/save.
    pub path: PathBuf,
    /// One quoted POSIX shell argument; never includes an execution/newline suffix.
    pub quoted_path: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub byte_len: usize,
    pub thumbnail: Arc<RenderImage>,
}

pub fn prepare(input: ImageInput, target: ImageTarget) -> Result<PreparedImage, String> {
    validate_target(&target)?;
    let (original, bytes) = match input {
        ImageInput::File(path) => {
            path_text(&path)?;
            let path = native_source_path(path, &target)?;
            let bytes = read_encoded(&path)?;
            (Some(path), bytes)
        }
        ImageInput::Clipboard(bytes) => {
            check_encoded_len(bytes.len())?;
            (None, bytes)
        }
    };
    let (decoded, format) = decode(&bytes)?;
    let (width, height) = (decoded.width(), decoded.height());
    let thumbnail = if width > 160 || height > 100 {
        let thumbnail = decoded.thumbnail(160, 100);
        drop(decoded);
        thumbnail
    } else {
        decoded
    };
    // Only the compact thumbnail escapes this worker; never cache a hidden full-size decode.
    let thumbnail = render_image(thumbnail.into_rgba8());
    let path = match original {
        Some(path) => path,
        None => {
            let root = compi_protocol::paths::data_dir()
                .map_err(|error| format!("Cannot locate Compi image storage: {error}"))?
                .join("clipboard-images");
            persist(&root, &bytes, format)?
        }
    };
    let quoted_path = quote_posix(&target_path(&path, &target)?)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Image file name is not valid Unicode")?
        .to_owned();
    Ok(PreparedImage {
        path,
        quoted_path,
        name,
        width,
        height,
        byte_len: bytes.len(),
        thumbnail,
    })
}

pub fn load_full_image(path: &Path) -> Result<Arc<RenderImage>, String> {
    let bytes = read_encoded(path)?;
    let (decoded, _) = decode(&bytes)?;
    Ok(render_image(decoded.into_rgba8()))
}

pub fn clipboard_image(path: &Path) -> Result<gpui::Image, String> {
    let bytes = read_encoded(path)?;
    let (decoded, format) = decode(&bytes)?;
    drop(decoded);
    let format = match format {
        ImageFormat::Png => gpui::ImageFormat::Png,
        ImageFormat::Jpeg => gpui::ImageFormat::Jpeg,
        ImageFormat::WebP => gpui::ImageFormat::Webp,
        ImageFormat::Gif => gpui::ImageFormat::Gif,
        ImageFormat::Bmp => gpui::ImageFormat::Bmp,
        _ => return Err("Unsupported clipboard image format".into()),
    };
    Ok(gpui::Image::from_bytes(format, bytes))
}

/// Standard Windows bitmap clipboard formats are not exposed by GPUI's PNG/GIF path.
#[cfg(windows)]
pub fn clipboard_bitmap() -> Result<Option<Vec<u8>>, String> {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalSize(memory: *mut c_void) -> usize;
        fn GlobalLock(memory: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(memory: *mut c_void) -> i32;
    }
    // CF_DIBV5 preserves alpha; CF_DIB covers standard bitmap-only copy sources.
    let format = unsafe {
        [17, 8]
            .into_iter()
            .find(|format| IsClipboardFormatAvailable(*format) != 0)
    };
    let Some(format) = format else {
        return Ok(None);
    };
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("Clipboard is busy. Try pasting the image again.".into());
        }
        let result = (|| {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return Err("Cannot read the clipboard bitmap".into());
            }
            let length = GlobalSize(handle);
            if !(40..=MAX_ENCODED_BYTES - 14).contains(&length) {
                return Err("Clipboard bitmap is empty or exceeds 64 MiB".into());
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                return Err("Cannot lock the clipboard bitmap".into());
            }
            let result = packed_bitmap(std::slice::from_raw_parts(pointer.cast::<u8>(), length));
            GlobalUnlock(handle);
            result.map(Some)
        })();
        CloseClipboard();
        result
    }
}

#[cfg(any(windows, test))]
fn packed_bitmap(dib: &[u8]) -> Result<Vec<u8>, String> {
    if dib.len() < 40 {
        return Err("Clipboard bitmap header is incomplete".into());
    }
    let number = |at| u32::from_le_bytes(dib[at..at + 4].try_into().unwrap()) as usize;
    let header = number(0);
    if !(40..=dib.len()).contains(&header) {
        return Err("Unsupported clipboard bitmap header".into());
    }
    let bits = u16::from_le_bytes(dib[14..16].try_into().unwrap());
    let palette = if number(32) != 0 {
        number(32)
    } else if bits <= 8 {
        1usize << bits
    } else {
        0
    };
    let masks = if header == 40 {
        match number(16) {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let pixels = header
        .checked_add(masks)
        .and_then(|bytes| {
            palette
                .checked_mul(4)
                .and_then(|palette| bytes.checked_add(palette))
        })
        .filter(|bytes| *bytes <= dib.len())
        .ok_or("Clipboard bitmap palette is invalid")?;
    let size = dib
        .len()
        .checked_add(14)
        .filter(|size| *size <= MAX_ENCODED_BYTES)
        .ok_or("Clipboard bitmap exceeds 64 MiB")?;
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&(size as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&((pixels + 14) as u32).to_le_bytes());
    bytes.extend_from_slice(dib);
    Ok(bytes)
}

fn check_encoded_len(length: usize) -> Result<(), String> {
    if length == 0 {
        Err("The image is empty".into())
    } else if length > MAX_ENCODED_BYTES {
        Err("Image exceeds the 64 MiB encoded file limit".into())
    } else {
        Ok(())
    }
}

fn read_encoded(path: &Path) -> Result<Vec<u8>, String> {
    path_text(path)?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Cannot read image {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err("Choose a regular image file, not a directory or device".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A file replaced by a FIFO between metadata/open must not block a worker.
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_ENCODED_BYTES as u64 {
        return Err("Image must be a regular file no larger than 64 MiB".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining = (MAX_ENCODED_BYTES - bytes.len() + 1).min(buffer.len());
        let count = file
            .read(&mut buffer[..remaining])
            .map_err(|error| format!("Cannot read image {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        check_encoded_len(bytes.len() + count)?;
        bytes
            .try_reserve_exact(count)
            .map_err(|_| "Not enough memory to read this image")?;
        bytes.extend_from_slice(&buffer[..count]);
    }
    check_encoded_len(bytes.len())?;
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<(DynamicImage, ImageFormat), String> {
    check_encoded_len(bytes.len())?;
    let format = image::guess_format(bytes)
        .map_err(|_| "Not a recognized PNG, JPEG, WebP, GIF or BMP image".to_owned())?;
    extension(format)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_IMAGE_BYTES as u64);
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height)?;
    let output_bytes = decoder.total_bytes();
    // Include RGBA conversion when it needs a second full-size allocation.
    let rgba_bytes = u64::from(width) * u64::from(height) * 4;
    let conversion_bytes = if decoder.color_type() == image::ColorType::Rgba8 {
        0
    } else {
        rgba_bytes
    };
    if output_bytes.saturating_add(conversion_bytes) > MAX_DECODED_IMAGE_BYTES as u64 {
        return Err("Image pixel format exceeds the 64 MiB decoded allocation limit".into());
    }
    limits.reserve(output_bytes).map_err(decode_error)?;
    decoder.set_limits(limits).map_err(decode_error)?;
    // DynamicImage decodes the first GIF/WebP frame, not the entire animation.
    let decoded = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    Ok((decoded, format))
}

fn decode_error(error: image::ImageError) -> String {
    format!("Cannot decode image (maximum 8192 pixels per side, 64 MiB decoded): {error}")
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) * 4 > MAX_DECODED_IMAGE_BYTES as u64
    {
        Err("Image dimensions exceed 8192 pixels per side or the 64 MiB decoded limit".into())
    } else {
        Ok(())
    }
}

fn extension(format: ImageFormat) -> Result<&'static str, String> {
    match format {
        ImageFormat::Png => Ok("png"),
        ImageFormat::Jpeg => Ok("jpg"),
        ImageFormat::WebP => Ok("webp"),
        ImageFormat::Gif => Ok("gif"),
        ImageFormat::Bmp => Ok("bmp"),
        _ => Err("Supported image formats are PNG, JPEG, WebP, GIF and BMP".into()),
    }
}

fn render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    let pixels: &mut [u8] = rgba.as_mut();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(smallvec![Frame::new(rgba)]))
}

fn path_text(path: &Path) -> Result<&str, String> {
    let text = path.to_str().ok_or("Image path is not valid Unicode")?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err("Image paths cannot be empty or contain control characters".into());
    }
    Ok(text)
}

fn quote_posix(path: &str) -> Result<String, String> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err("Image paths cannot be empty or contain control characters".into());
    }
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

fn validate_target(target: &ImageTarget) -> Result<(), String> {
    match target {
        ImageTarget::Native => {
            #[cfg(windows)]
            return Err("Windows image insertion requires a WSL session".into());
        }
        ImageTarget::Wsl { distribution } => {
            if distribution
                .as_ref()
                .is_some_and(|name| name.is_empty() || name.chars().any(char::is_control))
            {
                return Err(
                    "WSL distribution cannot be empty or contain control characters".into(),
                );
            }
            #[cfg(not(windows))]
            return Err("WSL image targets are only available on Windows".into());
        }
    }
    Ok(())
}

fn native_source_path(path: PathBuf, target: &ImageTarget) -> Result<PathBuf, String> {
    #[cfg(windows)]
    if let ImageTarget::Wsl { distribution } = target
        && path_text(&path)?.starts_with('/')
    {
        let translated = run_wsl(
            distribution.as_deref(),
            &["wslpath", "-a", "-w", path_text(&path)?],
        )?;
        let native = PathBuf::from(translated);
        if !native.is_absolute() {
            return Err("WSL returned a non-absolute native image path".into());
        }
        return Ok(native);
    }
    #[cfg(not(windows))]
    let _ = target;
    // Do not copy/move dropped files, or resolve their symlink to a different display name.
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| format!("Cannot resolve image path: {error}"))
    }
}

fn target_path(path: &Path, target: &ImageTarget) -> Result<String, String> {
    match target {
        ImageTarget::Native => Ok(path_text(path)?.to_owned()),
        ImageTarget::Wsl { distribution } => {
            #[cfg(windows)]
            {
                let text = path_text(path)?;
                // wslpath does not understand Win32's extended-length drive/UNC prefixes.
                let normalized = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
                    format!(r"\\{unc}")
                } else {
                    text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
                };
                let resolved = run_wsl(
                    distribution.as_deref(),
                    &["wslpath", "-a", "-u", &normalized],
                )?;
                if !resolved.starts_with('/') {
                    return Err("WSL returned a non-absolute image path".into());
                }
                quote_posix(&resolved)?;
                run_wsl(distribution.as_deref(), &["test", "-f", &resolved])?;
                run_wsl(distribution.as_deref(), &["test", "-r", &resolved])?;
                Ok(resolved)
            }
            #[cfg(not(windows))]
            {
                let _ = distribution;
                Err("WSL image targets are only available on Windows".into())
            }
        }
    }
}

fn persist(root: &Path, bytes: &[u8], format: ImageFormat) -> Result<PathBuf, String> {
    private_directory(root)?;
    let _lock = store_lock(&root.join(".lock"))?;
    let path = root.join(format!(
        "{:x}.{}",
        Sha256::digest(bytes),
        extension(format)?
    ));
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
                return Err(format!(
                    "Existing managed image is inconsistent; no file was overwritten: {}",
                    path.display()
                ));
            }
            let mut existing = File::open(&path).map_err(|error| error.to_string())?;
            let mut remaining = bytes;
            let mut buffer = [0_u8; 16 * 1024];
            while !remaining.is_empty() {
                let count = remaining.len().min(buffer.len());
                existing
                    .read_exact(&mut buffer[..count])
                    .map_err(|error| error.to_string())?;
                if buffer[..count] != remaining[..count] {
                    return Err(format!(
                        "Existing managed image contents do not match; no file was overwritten: {}",
                        path.display()
                    ));
                }
                remaining = &remaining[count..];
            }
            if existing
                .read(&mut buffer[..1])
                .map_err(|error| error.to_string())?
                != 0
            {
                return Err(
                    "Existing managed image changed while reading; retry image paste".into(),
                );
            }
            return Ok(path);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut used = 0_u64;
    let mut count = 0_usize;
    for entry in fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name() == ".lock" {
            continue;
        }
        count += 1;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err(format!(
                "Unexpected entry in private image storage: {}",
                entry.path().display()
            ));
        }
        used = used.saturating_add(metadata.len());
        if count >= STORE_FILES || used.saturating_add(bytes.len() as u64) > STORE_BYTES {
            return Err(format!(
                "Clipboard image storage is full (512 MiB or 4096 files). Remove unneeded originals from {} and retry; Compi never deletes referenced images automatically.",
                root.display()
            ));
        }
    }
    let (temporary, mut file) = temporary_file(root)?;
    file.write_all(bytes)
        .map_err(|error| format!("Cannot save clipboard image: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Cannot flush clipboard image: {error}"))?;
    drop(file);
    // Unlike rename on Unix, hard_link never replaces an existing destination.
    fs::hard_link(&temporary.0, &path).map_err(|error| {
        format!("Cannot publish clipboard image without overwriting another file: {error}")
    })?;
    drop(temporary);
    #[cfg(unix)]
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot flush clipboard image directory: {error}"))?;
    Ok(path)
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn temporary_file(root: &Path) -> Result<(TemporaryFile, File), String> {
    for _ in 0..32 {
        let number = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".image-{}-{number}.tmp", std::process::id()));
        match private_file(&path, true, false) {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Cannot create private image file: {error}")),
        }
    }
    Err(format!(
        "Too many stale temporary image files in {}; remove .image-*.tmp files and retry",
        root.display()
    ))
}

fn private_directory(path: &Path) -> Result<(), String> {
    path_text(path)?;
    if !path.is_absolute() {
        return Err("Private image storage must use an absolute directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(format!(
                "Image storage must be a current-user directory with mode 0700, not a symlink: {}",
                path.display()
            ));
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::CreateDirectoryW;
        use windows::core::PCWSTR;
        let security = compi_protocol::identity::PipeSecurity::for_current_user()
            .map_err(|error| error.to_string())?;
        let wide = wide_path(path);
        if let Err(error) =
            unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(security.attributes())) }
            && error.code() != windows::Win32::Foundation::ERROR_ALREADY_EXISTS.to_hresult()
        {
            return Err(format!("Cannot create private image storage: {error}"));
        }
        use std::os::windows::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err("Image storage cannot be a symlink or reparse point".into());
        }
    }
    Ok(())
}

fn private_file(path: &Path, create_new: bool, exclusive: bool) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(!create_new)
            .create_new(create_new)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unsafe private image file ownership or permissions",
            ));
        }
        let _ = exclusive;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows::Win32::Storage::FileSystem::{
            CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        };
        use windows::core::PCWSTR;
        let security = compi_protocol::identity::PipeSecurity::for_current_user()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let wide = wide_path(path);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                if exclusive {
                    FILE_SHARE_MODE(0)
                } else {
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
                },
                Some(security.attributes()),
                if create_new { CREATE_NEW } else { OPEN_ALWAYS },
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        use std::os::windows::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Private image file cannot be a reparse point",
            ));
        }
        Ok(file)
    }
}

fn store_lock(path: &Path) -> Result<File, String> {
    let file = private_file(path, false, true)
        .map_err(|error| format!("Cannot lock clipboard image storage; another Compi may be saving an image. Retry paste: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Clipboard image storage is busy; retry image paste".into());
        }
    }
    Ok(file)
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn run_wsl(distribution: Option<&str>, args: &[&str]) -> Result<String, String> {
    use std::io::{Seek, SeekFrom};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(15);
    const OUTPUT_LIMIT: u64 = 32 * 1024;
    let root = compi_protocol::paths::data_dir()
        .map_err(|error| error.to_string())?
        .join("image-work");
    private_directory(&root)?;
    let (_temporary, mut output) = temporary_file(&root)?;
    // Windows keeps this shared handle alive but removes the name on final close,
    // including after a client crash. WSL output must not accumulate in the store.
    fs::remove_file(&_temporary.0)
        .map_err(|error| format!("Cannot make WSL image output temporary: {error}"))?;
    let mut command = Command::new(r"C:\Windows\System32\wsl.exe");
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    if let Some(distribution) = distribution {
        command.args(["--distribution", distribution]);
    }
    command
        .arg("--exec")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(
            output.try_clone().map_err(|error| error.to_string())?,
        ));
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start WSL image path resolution: {error}"))?;
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        let observed = (|| -> io::Result<_> {
            if output.metadata()?.len() > OUTPUT_LIMIT {
                return Err(io::Error::other("WSL image path output exceeds 32 KiB"));
            }
            child.try_wait()
        })();
        match observed {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let _ = child.kill();
                // Never wait indefinitely for a hung WSL service after requesting termination.
                let _ = child.try_wait();
                return Err(match result {
                    Err(error) => format!("Cannot resolve WSL image path: {error}"),
                    _ => "WSL image path resolution timed out after 15 seconds. Start the selected distribution and retry.".into(),
                });
            }
        }
    };
    if !status.success() {
        return Err(format!(
            "Image is not accessible in WSL distribution {} ({} failed). Check that the file is mounted and readable in that session.",
            distribution.unwrap_or("<default>"),
            args[0]
        ));
    }
    output
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    output
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err("WSL image path output exceeds 32 KiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "WSL returned a non-UTF-8 image path")?;
    // Strip only the utility's one line ending, never meaningful path spaces.
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    if text.chars().any(char::is_control) {
        return Err("WSL returned control characters in the image path".into());
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_clipboard_bitmap_keeps_pixel_orientation_and_color() {
        let mut dib = vec![0u8; 48];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&2i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib[40..48].copy_from_slice(&[255, 0, 0, 0, 0, 0, 255, 0]);
        let (image, _) = decode(&packed_bitmap(&dib).unwrap()).unwrap();
        let rgba = image.into_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(rgba.get_pixel(0, 1).0, [0, 0, 255, 255]);
    }

    #[test]
    fn paths_are_single_literal_arguments_and_reject_terminal_controls() {
        assert_eq!(
            quote_posix("/tmp/a 'quote' $(touch bad) $HOME.png").unwrap(),
            "'/tmp/a '\\''quote'\\'' $(touch bad) $HOME.png'"
        );
        for path in [
            "",
            "/tmp/a\nb.png",
            "/tmp/\0.png",
            "/tmp/\u{1b}[2J.png",
            "/tmp/a\u{85}.png",
        ] {
            assert!(quote_posix(path).is_err());
        }
    }

    #[test]
    fn corrupt_images_and_allocation_bombs_are_rejected() {
        assert!(decode(b"not an image").is_err());
        assert!(decode(b"\x89PNG\r\n\x1a\n").is_err());
        assert!(check_encoded_len(MAX_ENCODED_BYTES + 1).is_err());
        assert!(check_dimensions(8192, 8192).is_err());
        assert!(check_dimensions(u32::MAX, u32::MAX).is_err());
        assert!(check_dimensions(0, 1).is_err());
        let mut bmp = Cursor::new(Vec::new());
        RgbaImage::new(1, 1)
            .write_to(&mut bmp, ImageFormat::Bmp)
            .unwrap();
        let mut bmp = bmp.into_inner();
        bmp[18..22].copy_from_slice(&8192_u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&8192_u32.to_le_bytes());
        assert!(decode(&bmp).is_err());
    }

    #[test]
    fn managed_original_survives_handles_and_existing_data_is_not_replaced() {
        let root = std::env::temp_dir().join(format!(
            "compi-image-test-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut encoded = Cursor::new(Vec::new());
        RgbaImage::from_pixel(2, 1, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();
        let bytes = encoded.into_inner();
        let path = persist(&root, &bytes, ImageFormat::Png).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(persist(&root, &bytes, ImageFormat::Png).unwrap(), path);
        let full = load_full_image(&path).unwrap();
        assert_eq!(
            full.as_bytes(0).unwrap(),
            &[30, 20, 10, 255, 30, 20, 10, 255]
        );
        drop(full);
        assert_eq!(clipboard_image(&path).unwrap().bytes, bytes);
        fs::write(&path, b"do not overwrite").unwrap();
        assert!(persist(&root, &bytes, ImageFormat::Png).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"do not overwrite");
        fs::remove_dir_all(root).unwrap();
    }
}

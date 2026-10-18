//! Application icons for Monitor, read from the application itself without a window
//! system: on macOS the bundle's `.icns` named by its `Info.plist`, on Windows the
//! executable's icon. Linux has none yet. Results are cached per group, missing ones too.

use base64::Engine as _;
use compi_protocol::system_metrics::MAX_APP_ICON_BYTES;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// Cached groups before the cache starts over.
const CACHE_LIMIT: usize = 512;

/// The icon of `group_id` (an `AppGroup::id`) as base64 PNG, if one can be read.
pub fn app_icon(group_id: &str) -> Option<Arc<str>> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<Arc<str>>>>> =
        LazyLock::new(Mutex::default);
    if let Some(cached) = CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(group_id).cloned())
    {
        return cached;
    }
    let icon = platform::icon_png(group_id)
        .filter(|png| png.len() <= MAX_APP_ICON_BYTES)
        .map(|png| Arc::from(base64::engine::general_purpose::STANDARD.encode(png)));
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(group_id.to_owned(), icon.clone());
    }
    icon
}

/// From an `.icns` file, the smallest PNG at least `wanted` pixels wide, else the largest.
/// Each entry is a four-byte type and a big-endian length that includes its header;
/// modern entries hold PNG data, older ones raw or JPEG 2000 data, which are skipped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn icns_png(icns: &[u8], wanted: u32) -> Option<&[u8]> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if icns.get(..4)? != b"icns" {
        return None;
    }
    let end = (u32::from_be_bytes(icns.get(4..8)?.try_into().ok()?) as usize).min(icns.len());
    let mut offset = 8;
    let mut best: Option<(u32, &[u8])> = None;
    while offset + 8 <= end {
        let length = u32::from_be_bytes(icns[offset + 4..offset + 8].try_into().ok()?) as usize;
        if length < 8 || offset + length > end {
            break;
        }
        let data = &icns[offset + 8..offset + length];
        if data.starts_with(PNG)
            && let Some(width) = data.get(16..20)
        {
            let width = u32::from_be_bytes(width.try_into().ok()?);
            let better = match best {
                None => true,
                Some((current, _)) if current >= wanted => width >= wanted && width < current,
                Some((current, _)) => width > current,
            };
            if better {
                best = Some((width, data));
            }
        }
        offset += length;
    }
    best.map(|(_, data)| data)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;

    /// Icons larger than this file size are not read.
    const MAX_ICNS_BYTES: u64 = 16 * 1024 * 1024;

    pub fn icon_png(bundle: &str) -> Option<Vec<u8>> {
        let bundle = Path::new(bundle);
        if bundle.extension()? != "app" || !bundle.is_dir() {
            return None;
        }
        let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
        let info = info.as_dictionary()?;
        let name = ["CFBundleIconFile", "CFBundleIconName"]
            .iter()
            .find_map(|key| info.get(key)?.as_string())?;
        // Only a file name inside Resources, with or without its extension.
        if name.contains('/') || name.starts_with('.') {
            return None;
        }
        let file = if name.ends_with(".icns") {
            name.to_owned()
        } else {
            format!("{name}.icns")
        };
        let path = bundle.join("Contents/Resources").join(file);
        if std::fs::metadata(&path).ok()?.len() > MAX_ICNS_BYTES {
            return None;
        }
        let icns = std::fs::read(path).ok()?;
        super::icns_png(&icns, 64).map(<[u8]>::to_vec)
    }
}

#[cfg(windows)]
mod platform {
    //! The executable's first icon at 64 pixels, drawn into a 32-bit top-down bitmap.

    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
    };
    use windows::Win32::UI::Shell::SHDefExtractIconW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
    use windows::core::PCWSTR;

    const SIZE: u32 = 64;

    pub fn icon_png(executable: &str) -> Option<Vec<u8>> {
        let path = std::path::Path::new(executable);
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            || !path.is_file()
        {
            return None;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut icon = HICON::default();
        unsafe { SHDefExtractIconW(PCWSTR(wide.as_ptr()), 0, 0, Some(&mut icon), None, SIZE) }
            .ok()
            .ok()?;
        let pixels = icon_rgba(icon);
        let _ = unsafe { DestroyIcon(icon) };
        let (width, height, rgba) = pixels?;
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().ok()?.write_image_data(&rgba).ok()?;
        Some(png)
    }

    fn icon_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
        let mut info = ICONINFO::default();
        unsafe { GetIconInfo(icon, &mut info) }.ok()?;
        let result = (|| {
            let mut bitmap = BITMAP::default();
            let read = unsafe {
                GetObjectW(
                    HGDIOBJ(info.hbmColor.0),
                    std::mem::size_of::<BITMAP>() as i32,
                    Some((&raw mut bitmap).cast()),
                )
            };
            if read == 0 || info.hbmColor.is_invalid() {
                return None;
            }
            let (width, height) = (bitmap.bmWidth.max(0) as u32, bitmap.bmHeight.max(0) as u32);
            if width == 0 || height == 0 || width > 256 || height > 256 {
                return None;
            }
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; (width * height * 4) as usize];
            let context = unsafe { CreateCompatibleDC(None) };
            let lines = unsafe {
                GetDIBits(
                    context,
                    info.hbmColor,
                    0,
                    height,
                    Some(bgra.as_mut_ptr().cast()),
                    &mut header,
                    DIB_RGB_COLORS,
                )
            };
            let _ = unsafe { DeleteDC(context) };
            if lines != height as i32 {
                return None;
            }
            // Old icons carry no alpha: treat them as opaque.
            let opaque = bgra.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
                if opaque {
                    pixel[3] = 255;
                }
            }
            Some((width, height, bgra))
        })();
        unsafe {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        result
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    pub fn icon_png(_executable: &str) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &[u8; 4], width: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(width.to_be_bytes());
        png.extend(width.to_be_bytes());
        let mut entry = kind.to_vec();
        entry.extend(((png.len() + 8) as u32).to_be_bytes());
        entry.extend(png);
        entry
    }

    fn icns(entries: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = entries.concat();
        let mut file = b"icns".to_vec();
        file.extend(((body.len() + 8) as u32).to_be_bytes());
        file.extend(body);
        file
    }

    fn width(png: &[u8]) -> u32 {
        u32::from_be_bytes(png[16..20].try_into().unwrap())
    }

    #[test]
    fn icns_gives_the_smallest_png_wide_enough_else_the_largest() {
        let file = icns(&[
            entry(b"ic10", 1024),
            entry(b"ic12", 64),
            entry(b"ic07", 128),
        ]);
        assert_eq!(icns_png(&file, 64).map(width), Some(64));
        let small = icns(&[entry(b"icp4", 16), entry(b"icp5", 32)]);
        assert_eq!(icns_png(&small, 64).map(width), Some(32));
        // Raw entries are skipped; truncated or foreign files give nothing.
        let mut raw = b"is32".to_vec();
        raw.extend(12u32.to_be_bytes());
        raw.extend([1, 2, 3, 4]);
        assert_eq!(icns_png(&icns(&[raw]), 64), None);
        assert_eq!(icns_png(b"icns\0\0\0\x10ic07\0\0\x01\0", 64), None);
        assert_eq!(icns_png(b"not an icon", 64), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_system_app_has_an_icon_and_a_plain_executable_has_none() {
        let finder = app_icon("/System/Library/CoreServices/Finder.app").expect("Finder icon");
        let png = base64::engine::general_purpose::STANDARD
            .decode(finder.as_bytes())
            .unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(app_icon("/bin/zsh"), None);
        assert_eq!(app_icon("/nonexistent/Nothing.app"), None);
    }
}

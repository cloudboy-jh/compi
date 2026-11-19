//! `compi screenshot`: list the Windows app windows open on this PC, or save one as a
//! PNG an agent can read, in the caller's form of a temporary path (a Linux path when
//! run from WSL through the shell bridge). Needs no running Compi.

use super::human::{self, Fit};
use super::{CliError, Output};
use serde_json::{Value, json};
use std::ffi::c_void;
use std::path::PathBuf;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWINDOWATTRIBUTE, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowPlacement,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    PW_RENDERFULLCONTENT, WINDOWPLACEMENT, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

/// Where captures go, under `/tmp` in the caller's WSL distribution or under `%TEMP%`.
const FOLDER: &str = "compi-screenshots";

/// A top-level app window as the list shows it.
struct Window {
    /// The HWND's value.
    handle: usize,
    /// The executable's file name, `explorer.exe`.
    app: String,
    pid: u32,
    title: String,
    /// Physical pixels; a minimized window's restored size.
    width: i32,
    height: i32,
    minimized: bool,
}

impl Window {
    fn id(&self) -> String {
        format!("{:#x}", self.handle)
    }
}

pub(super) fn run(name: Option<&str>) -> Result<Output, CliError> {
    // Sizes, frame bounds and the bitmap are then physical pixels on every monitor.
    // SAFETY: takes no pointers; fails harmlessly when awareness is already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let windows = windows();
    let Some(name) = name else {
        return Ok(list(&windows));
    };
    let window = pick(&windows, name).ok_or_else(|| {
        CliError::new(
            1,
            "window_not_found",
            format!(
                "no window's id, title or app matches {name:?}; run compi screenshot to list them"
            ),
        )
    })?;
    if window.minimized {
        return Err(CliError::new(
            1,
            "window_minimized",
            format!(
                "{} ({}) is minimized; restore it, then run compi screenshot again",
                window.title, window.app
            ),
        ));
    }
    let image = capture(window)?;
    let (path, shown) = destination(&window.app)?;
    image
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|error| {
            CliError::new(
                1,
                "save_failed",
                format!("cannot save the screenshot to {}: {error}", path.display()),
            )
        })?;
    Ok(Output::Report {
        text: format!("{shown}\n"),
        value: json!({
            "path": shown,
            "window": {
                "id": window.id(),
                "app": window.app,
                "pid": window.pid,
                "title": window.title,
                "width": image.width(),
                "height": image.height(),
            },
        }),
    })
}

fn list(windows: &[Window]) -> Output {
    let rows: Vec<Vec<String>> = windows
        .iter()
        .map(|window| {
            let mut size = format!("{} × {}", window.width, window.height);
            if window.minimized {
                size.push_str(" minimized");
            }
            vec![
                window.id(),
                window.app.clone(),
                window.pid.to_string(),
                size,
                window.title.clone(),
            ]
        })
        .collect();
    let text = if rows.is_empty() {
        "No app windows are open\n".to_owned()
    } else {
        human::table(
            &[
                ("ID", Fit::Keep),
                ("APP", Fit::End),
                ("PID", Fit::Keep),
                ("SIZE", Fit::Keep),
                ("TITLE", Fit::End),
            ],
            &rows,
            human::width(),
        )
    };
    let value: Vec<Value> = windows
        .iter()
        .map(|window| {
            json!({
                "id": window.id(),
                "app": window.app,
                "pid": window.pid,
                "title": window.title,
                "width": window.width,
                "height": window.height,
                "minimized": window.minimized,
            })
        })
        .collect();
    Output::Readable {
        text,
        value: json!({ "windows": value }),
    }
}

/// The window `name` means: its id, then an app named `name` (with or without `.exe`),
/// then a title or app containing it, ignoring case. Within each, the frontmost window
/// that is not minimized, else the frontmost.
fn pick<'a>(windows: &'a [Window], name: &str) -> Option<&'a Window> {
    let needle = name.to_lowercase();
    let bare = needle.strip_suffix(".exe").unwrap_or(&needle);
    windows
        .iter()
        .filter_map(|window| {
            let app = window.app.to_lowercase();
            let rank = if window.id() == needle {
                0
            } else if app.strip_suffix(".exe").unwrap_or(&app) == bare {
                1
            } else if app.contains(&needle) || window.title.to_lowercase().contains(&needle) {
                2
            } else {
                return None;
            };
            Some(((rank, window.minimized), window))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, window)| window)
}

/// `app` as a file name: lower case, `.exe` dropped, every other run of characters
/// outside `a-z0-9` one `-`.
fn file_stem(app: &str) -> String {
    let app = app.to_lowercase();
    let mut stem = String::new();
    for character in app.strip_suffix(".exe").unwrap_or(&app).chars() {
        if character.is_ascii_alphanumeric() {
            stem.push(character);
        } else if !stem.is_empty() && !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-');
    if stem.is_empty() {
        "window".to_owned()
    } else {
        stem.to_owned()
    }
}

/// A new file for a capture of `app`, and that file as the caller names it: under the
/// caller's WSL `/tmp` as a Linux path when run from WSL, else under `%TEMP%`.
fn destination(app: &str) -> Result<(PathBuf, String), CliError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // "2026-11-18 14:05:09" becomes "20261118-140509".
    let stamp = crate::monitor::format_local_time(now)
        .replace(['-', ':'], "")
        .replace(' ', "-");
    let name = format!("{}-{stamp}", file_stem(app));
    let distribution = std::env::var("WSL_DISTRO_NAME")
        .ok()
        .filter(|name| !name.is_empty());
    let folder = match &distribution {
        Some(distribution) => compi_protocol::wsl::windows_path_for_wsl("/tmp", distribution)
            .map_err(|error| {
                CliError::new(
                    1,
                    "save_failed",
                    format!("cannot find /tmp in WSL distribution {distribution}: {error}"),
                )
            })?
            .join(FOLDER),
        None => std::env::temp_dir().join(FOLDER),
    };
    std::fs::create_dir_all(&folder).map_err(|error| {
        CliError::new(
            1,
            "save_failed",
            format!("cannot create {}: {error}", folder.display()),
        )
    })?;
    // A second capture of the same app within a second keeps the first.
    let file = (1..)
        .map(|count| match count {
            1 => format!("{name}.png"),
            _ => format!("{name}-{count}.png"),
        })
        .find(|file| !folder.join(file).exists())
        .expect("some numbered file name is free");
    let shown = match distribution {
        Some(_) => format!("/tmp/{FOLDER}/{file}"),
        None => folder.join(&file).display().to_string(),
    };
    Ok((folder.join(file), shown))
}

/// Visible, titled, uncloaked, unowned top-level windows that are not tool windows,
/// frontmost first.
fn windows() -> Vec<Window> {
    unsafe extern "system" fn collect(hwnd: HWND, handles: LPARAM) -> BOOL {
        // SAFETY: `handles` is the vector `EnumWindows` was given below, alive for the
        // whole enumeration.
        unsafe { (*(handles.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    let _ = unsafe {
        EnumWindows(
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HWND> as isize),
        )
    };
    handles.into_iter().filter_map(window).collect()
}

fn window(hwnd: HWND) -> Option<Window> {
    // SAFETY: every call takes the enumerated handle, which at worst has closed since
    // (and then fails), and pointers to locals sized as given.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            || GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid())
            || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return None;
        }
        let mut cloaked = 0u32;
        if attribute(hwnd, DWMWA_CLOAKED, &mut cloaked) && cloaked != 0 {
            return None;
        }
        let mut title = [0u16; 512];
        let length = usize::try_from(GetWindowTextW(hwnd, &mut title)).ok()?;
        let title = String::from_utf16_lossy(&title[..length]);
        if title.trim().is_empty() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let minimized = IsIconic(hwnd).as_bool();
        let rect = if minimized {
            let mut placement = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            GetWindowPlacement(hwnd, &mut placement).ok()?;
            placement.rcNormalPosition
        } else {
            frame(hwnd)?
        };
        Some(Window {
            handle: hwnd.0 as usize,
            app: app(pid).unwrap_or_else(|| "unknown".to_owned()),
            pid,
            title,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
            minimized,
        })
    }
}

/// Reads one DWM attribute of `T`'s size; whether it could.
///
/// # Safety
/// `T` must be the type DWM writes for `kind`.
unsafe fn attribute<T>(hwnd: HWND, kind: DWMWINDOWATTRIBUTE, value: &mut T) -> bool {
    // SAFETY: `value` is a live `T` of the size passed, per the caller.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            kind,
            (value as *mut T).cast::<c_void>(),
            size_of::<T>() as u32,
        )
    }
    .is_ok()
}

/// The window as it looks, without the invisible resize border and shadow; its whole
/// rectangle where DWM has no bounds for it.
fn frame(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: DWM writes a RECT for DWMWA_EXTENDED_FRAME_BOUNDS.
    if unsafe { attribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut rect) } {
        return Some(rect);
    }
    // SAFETY: `rect` is a live RECT.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(rect)
}

/// The file name of `pid`'s executable.
fn app(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed below; the buffer and its length match.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut path = [0u16; 1024];
        let mut length = path.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        named.ok()?;
        let path = String::from_utf16_lossy(&path[..length as usize]);
        path.rsplit('\\').next().map(str::to_owned)
    }
}

/// The window drawn by itself, as `PrintWindow` renders it even when covered, cropped
/// to its visible frame.
fn capture(window: &Window) -> Result<image::RgbImage, CliError> {
    let failed = |why: &str| {
        CliError::new(
            1,
            "capture_failed",
            format!("cannot capture {} ({}): {why}", window.title, window.app),
        )
    };
    let hwnd = HWND(window.handle as *mut c_void);
    let mut outer = RECT::default();
    // SAFETY: `outer` is a live RECT.
    unsafe { GetWindowRect(hwnd, &mut outer) }.map_err(|_| failed("the window has closed"))?;
    let visible = frame(hwnd).unwrap_or(outer);
    let (width, height) = (outer.right - outer.left, outer.bottom - outer.top);
    // `PrintWindow` draws the whole window rectangle; keep the frame inside it.
    let left = (visible.left - outer.left).clamp(0, width);
    let top = (visible.top - outer.top).clamp(0, height);
    let right = (visible.right - outer.left).clamp(left, width);
    let bottom = (visible.bottom - outer.top).clamp(top, height);
    if right == left || bottom == top {
        return Err(failed("it has no size"));
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative: rows run top-down.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: the DC and bitmap are released on every path below; `bits` points at the
    // bitmap's width × height 32-bit pixels while it lives.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return Err(failed("no memory device context"));
        }
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => bitmap,
            _ => {
                let _ = DeleteDC(dc);
                return Err(failed("no bitmap of its size"));
            }
        };
        let previous = SelectObject(dc, bitmap.into());
        let printed = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        let _ = GdiFlush();
        let image = printed.then(|| {
            let stride = width as usize * 4;
            let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), stride * height as usize);
            // BGRA rows; alpha is dropped, so the PNG is opaque.
            image::RgbImage::from_fn((right - left) as u32, (bottom - top) as u32, |x, y| {
                let at = (top as usize + y as usize) * stride + (left as usize + x as usize) * 4;
                image::Rgb([pixels[at + 2], pixels[at + 1], pixels[at]])
            })
        });
        SelectObject(dc, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        image.ok_or_else(|| failed("PrintWindow could not draw it"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(handle: usize, app: &str, title: &str, minimized: bool) -> Window {
        Window {
            handle,
            app: app.to_owned(),
            pid: 1,
            title: title.to_owned(),
            width: 800,
            height: 600,
            minimized,
        }
    }

    #[test]
    fn names_pick_an_id_then_an_app_then_a_title_frontmost_and_restored_first() {
        let windows = [
            window(0x10, "Code.exe", "compi - Visual Studio Code", false),
            window(0x20, "compi.exe", "work", true),
            window(0x30, "compi.exe", "build", false),
            window(0x40, "explorer.exe", "Downloads", false),
            window(0x50, "explorer.exe", "Documents", false),
        ];
        let picked = |name| pick(&windows, name).map(|window| window.handle);
        assert_eq!(picked("0x40"), Some(0x40));
        assert_eq!(picked("0X50"), Some(0x50));
        // An app named exactly beats a title containing the name, and a window that
        // is not minimized beats a minimized one in front of it.
        assert_eq!(picked("compi"), Some(0x30));
        assert_eq!(picked("COMPI.EXE"), Some(0x30));
        assert_eq!(picked("work"), Some(0x20));
        assert_eq!(picked("explorer"), Some(0x40));
        assert_eq!(picked("docu"), Some(0x50));
        assert_eq!(picked("plorer.e"), Some(0x40));
        assert_eq!(picked("visual studio"), Some(0x10));
        assert_eq!(picked("notepad"), None);
    }

    #[test]
    fn file_names_keep_lower_case_letters_digits_and_single_dashes() {
        assert_eq!(file_stem("explorer.exe"), "explorer");
        assert_eq!(file_stem("Code - Insiders.EXE"), "code-insiders");
        assert_eq!(file_stem("WindowsTerminal.exe"), "windowsterminal");
        assert_eq!(file_stem("_Ünïcode_.exe"), "n-code");
        assert_eq!(file_stem("..."), "window");
        assert_eq!(file_stem(""), "window");
    }
}

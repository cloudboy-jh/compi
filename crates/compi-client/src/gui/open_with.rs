//! Files in other applications: the system's default app for a file, the other apps it
//! suggests for its type, the system's own app chooser, and the file shown in File
//! Explorer or Finder. Lists are read on the UI thread (they come from the type's
//! registration, not the file); opening runs on a thread of its own.

use compi_protocol::ContentEnvironment;
use std::path::PathBuf;

/// The menu item that shows a file or folder in the system's file manager.
pub(super) const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else {
    "Reveal in File Explorer"
};

/// An application that opens a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct App {
    pub(super) name: String,
    /// Windows: the handler's name, its executable; macOS: the app bundle's path.
    id: String,
}

impl App {
    /// An app the user picked themselves (macOS: an app bundle).
    #[cfg(target_os = "macos")]
    pub(super) fn chosen(path: &std::path::Path) -> Self {
        Self {
            name: app_name(path),
            id: path.to_string_lossy().into_owned(),
        }
    }
}

/// The apps the system offers for a file.
#[derive(Clone, Debug, Default)]
pub(super) struct Apps {
    /// The default app's name, when the file's type has one.
    pub(super) default: Option<String>,
    /// The other apps it suggests, without the default.
    pub(super) others: Vec<App>,
}

/// What to do with a file.
#[derive(Clone, Debug)]
pub(super) enum Action {
    Default,
    With(App),
    /// The system's app chooser (Windows); it opens the file with the app picked.
    #[cfg(windows)]
    Choose,
    Reveal,
}

/// `path` of `environment` as this computer opens it: a WSL file through the
/// distribution's share, or its Windows drive under `/mnt`. None where the file is not
/// on this computer's file system.
pub(super) fn native_path(environment: &ContentEnvironment, path: &str) -> Option<PathBuf> {
    match environment {
        ContentEnvironment::Host => Some(PathBuf::from(path)),
        ContentEnvironment::Wsl { distribution } => {
            if !cfg!(windows) || !path.starts_with('/') {
                return None;
            }
            Some(PathBuf::from(crate::content_path::wsl_windows_path(
                distribution,
                path,
            )))
        }
    }
}

/// Where a path the system hands Compi lives (Explorer's Open with, Finder's Open
/// With): a path on a WSL share is that distribution's Linux path; any other is the
/// host's own.
pub(super) fn system_content(path: &str) -> (ContentEnvironment, String) {
    for share in [r"\\wsl.localhost\", r"\\wsl$\"] {
        let Some(rest) = path
            .get(..share.len())
            .filter(|start| start.eq_ignore_ascii_case(share))
            .map(|_| &path[share.len()..])
        else {
            continue;
        };
        let (distribution, rest) = rest.split_once('\\').unwrap_or((rest, ""));
        if !distribution.is_empty() {
            return (
                ContentEnvironment::Wsl {
                    distribution: distribution.to_owned(),
                },
                format!("/{}", rest.replace('\\', "/").trim_end_matches('/')),
            );
        }
    }
    (ContentEnvironment::Host, path.to_owned())
}

/// A file the system's picker returned, as a program in `environment` names it: a
/// Windows drive path is `/mnt/<drive>/…` in WSL, and a WSL share path is that
/// distribution's Linux path there; the host reads any of its own paths, shares
/// included. `None` when `environment` cannot reach it (another distribution's share, a
/// network path from WSL).
pub(super) fn picked_path(environment: &ContentEnvironment, picked: &str) -> Option<String> {
    let (place, path) = system_content(picked);
    match environment {
        ContentEnvironment::Host => Some(picked.to_owned()),
        wsl if place == *wsl => Some(path),
        ContentEnvironment::Wsl { .. } => {
            let bytes = picked.as_bytes();
            let drive = *bytes.first()?;
            (drive.is_ascii_alphabetic() && bytes.get(1) == Some(&b':')).then(|| {
                format!(
                    "/mnt/{}/{}",
                    drive.to_ascii_lowercase() as char,
                    picked[2..]
                        .trim_start_matches(['\\', '/'])
                        .replace('\\', "/")
                )
            })
        }
    }
}

/// The path of a `file://` URL (Finder's Open With sends these), percent-decoded and
/// without a trailing slash; None for any other URL.
pub(super) fn file_url_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let hex = |byte: u8| (byte as char).to_digit(16);
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index..] {
            [b'%', high, low, ..] if hex(high).is_some() && hex(low).is_some() => {
                decoded.push((hex(high)? * 16 + hex(low)?) as u8);
                index += 3;
            }
            [byte, ..] => {
                decoded.push(byte);
                index += 1;
            }
            [] => break,
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let trimmed = path.trim_end_matches('/');
    Some(if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    })
}

/// Runs `action` on `path` on a thread of its own, since opening can wait on the
/// system (the chooser waits for the user). The receiver gets the outcome.
pub(super) fn run(path: PathBuf, action: Action) -> async_channel::Receiver<Result<(), String>> {
    let (sender, receiver) = async_channel::bounded(1);
    // Should the thread not start, the sender is dropped and the receiver says so.
    let _ = std::thread::Builder::new()
        .name("compi-open-with".into())
        .spawn(move || {
            let _ = sender.send_blocking(perform(&path, &action));
        });
    receiver
}

#[cfg(windows)]
pub(super) use windows_apps::apps;

#[cfg(windows)]
use windows_apps::perform;

#[cfg(windows)]
mod windows_apps {
    use super::{Action, App, Apps};
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize, IDataObject,
    };
    use windows::Win32::UI::Shell::{
        ASSOC_FILTER_RECOMMENDED, ASSOCF_INIT_IGNOREUNKNOWN, ASSOCSTR_FRIENDLYAPPNAME,
        AssocQueryStringW, BHID_DataObject, IAssocHandler, IShellItem, OAIF_ALLOW_REGISTRATION,
        OAIF_EXEC, OPEN_AS_INFO_FLAGS, OPENASINFO, SHAssocEnumHandlers,
        SHCreateItemFromParsingName, SHOpenWithDialog, ShellExecuteW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HRESULT, HSTRING, PCWSTR, PWSTR, w};

    /// At most this many suggested apps; the chooser has the rest.
    const MAX_APPS: usize = 12;

    /// The default app and the suggested ones for `path`'s type. The UI thread has COM
    /// (GPUI initializes OLE there).
    pub(in crate::gui) fn apps(path: &Path) -> Apps {
        let Some(extension) = extension(path) else {
            return Apps::default();
        };
        let default = default_name(&extension);
        let mut others: Vec<App> = Vec::new();
        for handler in handlers(&extension) {
            // SAFETY: `handler` is a live handler from the system's enumeration.
            let (Some(id), Some(name)) = (unsafe { take(handler.GetName()) }, unsafe {
                take(handler.GetUIName())
            }) else {
                continue;
            };
            if default.as_deref() == Some(name.as_str())
                || others.iter().any(|other| other.name == name)
            {
                continue;
            }
            others.push(App { name, id });
            if others.len() == MAX_APPS {
                break;
            }
        }
        Apps { default, others }
    }

    /// Does `action` on a thread of its own, with COM for that thread.
    pub(super) fn perform(path: &Path, action: &Action) -> Result<(), String> {
        // SAFETY: called once on this new thread, undone below when it succeeded.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let result = match action {
            Action::Default => open_default(path),
            Action::With(app) => open_with(path, app),
            Action::Choose => choose(path),
            Action::Reveal => reveal(path),
        };
        if com {
            // SAFETY: balances the successful `CoInitializeEx` above, on the same thread.
            unsafe { CoUninitialize() };
        }
        result
    }

    /// `.ext`, which the association functions take.
    fn extension(path: &Path) -> Option<HSTRING> {
        let extension = path.extension()?.to_str()?;
        Some(HSTRING::from(format!(".{extension}")))
    }

    /// The name of the app that opens `extension` by default.
    fn default_name(extension: &HSTRING) -> Option<String> {
        let mut length = 0_u32;
        // SAFETY: a null buffer asks for the length, in characters with the NUL.
        unsafe {
            let _ = AssocQueryStringW(
                ASSOCF_INIT_IGNOREUNKNOWN,
                ASSOCSTR_FRIENDLYAPPNAME,
                extension,
                PCWSTR::null(),
                None,
                &mut length,
            );
        }
        if length <= 1 {
            return None;
        }
        let mut buffer = vec![0_u16; length as usize];
        // SAFETY: `buffer` holds `length` characters, as the call above asked for.
        unsafe {
            AssocQueryStringW(
                ASSOCF_INIT_IGNOREUNKNOWN,
                ASSOCSTR_FRIENDLYAPPNAME,
                extension,
                PCWSTR::null(),
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
            )
            .ok()
            .ok()?;
        }
        let end = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        let name = String::from_utf16_lossy(&buffer[..end]);
        (!name.trim().is_empty()).then_some(name)
    }

    /// The handlers the system recommends for `extension`.
    fn handlers(extension: &HSTRING) -> Vec<IAssocHandler> {
        // SAFETY: plain COM calls on the enumeration the system returns.
        let Ok(enumeration) = (unsafe { SHAssocEnumHandlers(extension, ASSOC_FILTER_RECOMMENDED) })
        else {
            return Vec::new();
        };
        let mut handlers = Vec::new();
        loop {
            let mut slot = [None];
            let mut fetched = 0_u32;
            // SAFETY: one slot, and `fetched` outlives the call. S_FALSE ends the list.
            let next = unsafe { enumeration.Next(&mut slot, Some(&raw mut fetched)) };
            match (next, slot[0].take()) {
                (Ok(()), Some(handler)) if fetched == 1 => handlers.push(handler),
                _ => break,
            }
        }
        handlers
    }

    /// A string the system allocated, freed.
    ///
    /// # Safety
    /// `string` must be a COM task allocation or null.
    unsafe fn take(string: windows::core::Result<PWSTR>) -> Option<String> {
        let string = string.ok()?;
        if string.is_null() {
            return None;
        }
        // SAFETY: the system returned a NUL-terminated string it allocated for us.
        let text = unsafe { string.to_string() }.ok();
        // SAFETY: freed once, with the allocator that made it.
        unsafe { CoTaskMemFree(Some(string.0 as *const _)) };
        text.filter(|text| !text.trim().is_empty())
    }

    fn wide(path: &Path) -> HSTRING {
        HSTRING::from(path.as_os_str())
    }

    fn open_default(path: &Path) -> Result<(), String> {
        // SAFETY: every string outlives the call.
        let instance = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &wide(path),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // Above 32 is success; SE_ERR_NOASSOC (31) means no app is set for the type.
        match instance.0 as isize {
            code if code > 32 => Ok(()),
            31 => choose(path),
            code => Err(format!("Windows could not open the file (error {code})")),
        }
    }

    fn open_with(path: &Path, app: &App) -> Result<(), String> {
        let extension = extension(path).ok_or("The file has no type to open it with")?;
        let handler = handlers(&extension)
            .into_iter()
            // SAFETY: `handler` is a live handler from the system's enumeration.
            .find(|handler| unsafe { take(handler.GetName()) }.as_deref() == Some(app.id.as_str()))
            .ok_or_else(|| format!("{} no longer opens this kind of file", app.name))?;
        // SAFETY: COM calls on objects that live through them; the item names a path
        // that outlives its parse.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&wide(path), None)
                .map_err(|error| format!("Cannot find the file: {}", error.message()))?;
            let data: IDataObject = item
                .BindToHandler(None, &BHID_DataObject)
                .map_err(|error| error.message().to_string())?;
            handler
                .Invoke(&data)
                .map_err(|error| format!("{} did not open: {}", app.name, error.message()))
        }
    }

    fn choose(path: &Path) -> Result<(), String> {
        let file = wide(path);
        let info = OPENASINFO {
            pcszFile: PCWSTR(file.as_ptr()),
            pcszClass: PCWSTR::null(),
            oaifInFlags: OPEN_AS_INFO_FLAGS(OAIF_ALLOW_REGISTRATION.0 | OAIF_EXEC.0),
        };
        // SAFETY: `info` and the path it points to outlive the call.
        match unsafe { SHOpenWithDialog(None::<HWND>, &info) } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(()),
            Err(error) => Err(error.message().to_string()),
        }
    }

    /// Explorer's `/select`, which (unlike `SHOpenFolderAndSelectItems`) also selects
    /// items on the WSL share.
    fn reveal(path: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        if !path.exists() {
            return Err(format!("{} no longer exists", path.display()));
        }
        // Explorer reads `/select,"<path>"` itself; Windows paths cannot hold quotes.
        let mut argument = std::ffi::OsString::from("/select,\"");
        argument.push(path.as_os_str());
        argument.push("\"");
        std::process::Command::new("explorer.exe")
            .raw_arg(argument)
            .spawn()
            .map(drop)
            .map_err(|error| format!("Cannot start File Explorer: {error}"))
    }
}

#[cfg(target_os = "macos")]
pub(super) use mac_apps::apps;

#[cfg(target_os = "macos")]
use mac_apps::{app_name, perform};

#[cfg(target_os = "macos")]
mod mac_apps {
    use super::{Action, App, Apps};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSArray, NSString, NSURL};
    use std::path::Path;
    use std::process::Command;

    /// At most this many suggested apps; Choose app… has the rest.
    const MAX_APPS: usize = 12;

    /// The default app and the others Launch Services lists for the file.
    pub(in crate::gui) fn apps(path: &Path) -> Apps {
        let Some(text) = path.to_str() else {
            return Apps::default();
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        // SAFETY: `NSWorkspace` methods of macOS 12 and later, given a live file URL;
        // both return autoreleased objects or nil.
        let (default, all) = unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let default: Option<Retained<NSURL>> =
                msg_send![workspace, URLForApplicationToOpenURL: &*url];
            let all: Option<Retained<NSArray<NSURL>>> =
                msg_send![workspace, URLsForApplicationsToOpenURL: &*url];
            (default, all)
        };
        let bundle = |url: &NSURL| url.path().map(|path| path.to_string());
        let default = default.as_deref().and_then(bundle);
        let mut others: Vec<App> = Vec::new();
        if let Some(all) = all {
            for index in 0..all.count() {
                let Some(id) = bundle(&all.objectAtIndex(index)) else {
                    continue;
                };
                if default.as_deref() == Some(id.as_str())
                    || others.iter().any(|other| other.id == id)
                {
                    continue;
                }
                others.push(App {
                    name: app_name(Path::new(&id)),
                    id,
                });
                if others.len() == MAX_APPS {
                    break;
                }
            }
        }
        Apps {
            default: default.map(|id| app_name(Path::new(&id))),
            others,
        }
    }

    /// An app bundle's name: its file name without `.app`.
    pub(super) fn app_name(bundle: &Path) -> String {
        bundle
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| bundle.display().to_string())
    }

    /// Does `action` through `open`, which says why when it cannot.
    pub(super) fn perform(path: &Path, action: &Action) -> Result<(), String> {
        let mut command = Command::new("/usr/bin/open");
        match action {
            Action::Default => {}
            Action::With(app) => {
                command.arg("-a").arg(&app.id);
            }
            Action::Reveal => {
                command.arg("-R");
            }
        }
        let output = command
            .arg("--")
            .arg(path)
            .output()
            .map_err(|error| format!("Cannot run open: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(if message.is_empty() {
                "macOS could not open the file".to_owned()
            } else {
                message
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picked_files_are_named_as_the_chats_environment_reaches_them() {
        let ubuntu = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert_eq!(
            picked_path(&ubuntu, r"C:\Users\me\My Notes\a.md").as_deref(),
            Some("/mnt/c/Users/me/My Notes/a.md")
        );
        assert_eq!(
            picked_path(&ubuntu, r"\\wsl.localhost\Ubuntu\home\me\a.rs").as_deref(),
            Some("/home/me/a.rs")
        );
        // Another distribution's files and network shares are out of WSL's reach.
        assert_eq!(picked_path(&ubuntu, r"\\wsl$\Debian\home\me\a.rs"), None);
        assert_eq!(picked_path(&ubuntu, r"\\server\share\a.md"), None);
        assert_eq!(
            picked_path(&ContentEnvironment::Host, r"\\wsl.localhost\Ubuntu\home\a").as_deref(),
            Some(r"\\wsl.localhost\Ubuntu\home\a")
        );
    }

    #[test]
    fn wsl_paths_open_through_their_drive_or_the_distribution_share() {
        use crate::content_path::wsl_windows_path;
        assert_eq!(
            wsl_windows_path("Ubuntu", "/mnt/c/Users/me/notes.md"),
            r"C:\Users\me\notes.md"
        );
        assert_eq!(wsl_windows_path("Ubuntu", "/mnt/d"), r"D:\");
        assert_eq!(
            wsl_windows_path("Ubuntu-24.04", "/home/me/a b.txt"),
            r"\\wsl.localhost\Ubuntu-24.04\home\me\a b.txt"
        );
        // Only a single letter under /mnt is a drive.
        assert_eq!(
            wsl_windows_path("Ubuntu", "/mnt/wsl/x"),
            r"\\wsl.localhost\Ubuntu\mnt\wsl\x"
        );
        assert_eq!(
            native_path(
                &ContentEnvironment::Wsl {
                    distribution: "Ubuntu".into()
                },
                "relative"
            ),
            None
        );
    }

    #[test]
    fn paths_the_system_hands_over_open_where_they_live() {
        let wsl = |distribution: &str| ContentEnvironment::Wsl {
            distribution: distribution.into(),
        };
        assert_eq!(
            system_content(r"\\wsl.localhost\Ubuntu\home\me\notes.md"),
            (wsl("Ubuntu"), "/home/me/notes.md".into())
        );
        assert_eq!(
            system_content(r"\\WSL$\Ubuntu-24.04\srv\"),
            (wsl("Ubuntu-24.04"), "/srv".into())
        );
        assert_eq!(
            system_content(r"\\wsl.localhost\Ubuntu"),
            (wsl("Ubuntu"), "/".into())
        );
        assert_eq!(
            system_content(r"C:\Users\me\notes.md"),
            (ContentEnvironment::Host, r"C:\Users\me\notes.md".into())
        );
        assert_eq!(
            system_content(r"\\server\share\a.txt").0,
            ContentEnvironment::Host
        );
        assert_eq!(
            file_url_path("file:///Users/me/My%20Notes/caf%C3%A9.md").as_deref(),
            Some("/Users/me/My Notes/café.md")
        );
        assert_eq!(
            file_url_path("file://localhost/Users/me/Project/").as_deref(),
            Some("/Users/me/Project")
        );
        assert_eq!(file_url_path("file:///").as_deref(), Some("/"));
        assert_eq!(file_url_path("file:///a%2").as_deref(), Some("/a%2"));
        assert_eq!(file_url_path("https://example.com/a"), None);
    }
}

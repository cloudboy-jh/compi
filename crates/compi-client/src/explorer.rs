//! Compi in File Explorer: Open with for the file types Compi opens, Choose another app
//! for any file, and Open in Compi on folders. Setup's package writes these
//! (`installer/Compi.wxs`); an update in the app does not run the package, so an
//! installed Compi writes any that are missing when it starts, and Setup's removal
//! deletes them.

/// The file types whose Open with list names Compi, as `Compi.wxs` lists them.
pub const EXTENSIONS: [&str; 93] = [
    "txt",
    "text",
    "log",
    "md",
    "markdown",
    "mdx",
    "rst",
    "csv",
    "tsv",
    "json",
    "jsonc",
    "json5",
    "toml",
    "yaml",
    "yml",
    "xml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    "lock",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "rs",
    "go",
    "py",
    "rb",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "java",
    "kt",
    "kts",
    "swift",
    "lua",
    "php",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "zig",
    "dart",
    "sql",
    "graphql",
    "proto",
    "tf",
    "nix",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "r",
    "jl",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "bmp",
    "ico",
    "svg",
    "pdf",
    "mp3",
    "wav",
    "flac",
    "ogg",
    "m4a",
    "mp4",
    "webm",
];

const PROGID: &str = r"Software\Classes\Compi.File";
const APPLICATION: &str = r"Software\Classes\Applications\compi.exe";
const FOLDER: &str = r"Software\Classes\Directory\shell\Compi";
const BACKGROUND: &str = r"Software\Classes\Directory\Background\shell\Compi";

/// One registry string under `HKEY_CURRENT_USER`: key, value name (`None` is the key's
/// default value) and data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entry {
    pub key: String,
    pub name: Option<&'static str>,
    pub value: String,
}

/// Every entry for the launcher `compi.exe` in `root`, the installation folder.
pub fn entries(root: &str) -> Vec<Entry> {
    let root = root.trim_end_matches('\\');
    let exe = format!(r"{root}\compi.exe");
    let icon = format!("{exe},0");
    let open = |argument: &str| format!("\"{exe}\" --open \"{argument}\"");
    let entry = |key: &str, name: Option<&'static str>, value: String| Entry {
        key: key.to_owned(),
        name,
        value,
    };
    let mut entries = vec![
        entry(APPLICATION, Some("FriendlyAppName"), "Compi".into()),
        entry(&format!(r"{APPLICATION}\DefaultIcon"), None, icon.clone()),
        entry(
            &format!(r"{APPLICATION}\shell\open\command"),
            None,
            open("%1"),
        ),
        entry(PROGID, None, "File".into()),
        entry(&format!(r"{PROGID}\DefaultIcon"), None, icon.clone()),
        entry(
            &format!(r"{PROGID}\shell\open"),
            Some("FriendlyAppName"),
            "Compi".into(),
        ),
        entry(&format!(r"{PROGID}\shell\open\command"), None, open("%1")),
    ];
    entries.extend(EXTENSIONS.iter().map(|extension| {
        entry(
            &format!(r"Software\Classes\.{extension}\OpenWithProgids"),
            Some("Compi.File"),
            "Compi".into(),
        )
    }));
    for folder in [FOLDER, BACKGROUND] {
        entries.push(entry(folder, None, "Open in Compi".into()));
        entries.push(entry(folder, Some("Icon"), icon.clone()));
        entries.push(entry(&format!(r"{folder}\command"), None, open("%V")));
    }
    entries
}

/// Writes the entries for `root` when Explorer does not already open files with its
/// `compi.exe`. Explorer rereads its associations afterwards.
#[cfg(windows)]
pub fn ensure_registered(root: &std::path::Path) -> Result<(), String> {
    let root = root
        .to_str()
        .ok_or("The installation folder is not valid Unicode")?;
    let entries = entries(root);
    let command = entries
        .iter()
        .find(|entry| entry.key == format!(r"{PROGID}\shell\open\command"))
        .expect("the ProgID has an open command");
    let complete = registry::read(&command.key, None).as_deref() == Some(command.value.as_str())
        && entries
            .iter()
            .all(|entry| registry::read(&entry.key, entry.name).is_some());
    if complete {
        return Ok(());
    }
    for entry in &entries {
        registry::write(&entry.key, entry.name, &entry.value)?;
    }
    registry::associations_changed();
    Ok(())
}

/// Deletes every entry: the keys Compi owns, and its value in each file type's list.
#[cfg(windows)]
pub fn unregister() {
    for key in [APPLICATION, PROGID, FOLDER, BACKGROUND] {
        registry::delete_tree(key);
    }
    for extension in EXTENSIONS {
        registry::delete_value(
            &format!(r"Software\Classes\.{extension}\OpenWithProgids"),
            "Compi.File",
        );
    }
    registry::associations_changed();
}

#[cfg(windows)]
mod registry {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
        RegSetKeyValueW,
    };
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use windows::core::{HSTRING, PCWSTR};

    fn name(name: Option<&str>) -> Option<HSTRING> {
        name.map(HSTRING::from)
    }

    fn pointer(name: &Option<HSTRING>) -> PCWSTR {
        name.as_ref()
            .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()))
    }

    pub(super) fn read(key: &str, value: Option<&str>) -> Option<String> {
        let key = HSTRING::from(key);
        let value = name(value);
        let mut data = vec![0u16; 1024];
        let mut size = (data.len() * 2) as u32;
        // SAFETY: `data` holds `size` bytes; Windows writes at most that and the size used.
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                pointer(&value),
                RRF_RT_REG_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&raw mut size),
            )
        };
        (read == ERROR_SUCCESS).then(|| {
            let length = (size as usize / 2).saturating_sub(1);
            String::from_utf16_lossy(&data[..length])
        })
    }

    pub(super) fn write(key: &str, value: Option<&str>, data: &str) -> Result<(), String> {
        let wide: Vec<u16> = data.encode_utf16().chain([0]).collect();
        let (key_name, value_name) = (HSTRING::from(key), name(value));
        // SAFETY: `wide` is a NUL-terminated string of the given byte length.
        let written = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key_name,
                pointer(&value_name),
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        };
        if written == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("Could not write {key}: error {}", written.0))
        }
    }

    pub(super) fn delete_tree(key: &str) {
        // SAFETY: a valid key name; a missing key is left as it is.
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    }

    pub(super) fn delete_value(key: &str, value: &str) {
        // SAFETY: valid key and value names; a missing value is left as it is.
        let _ = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(value),
            )
        };
    }

    pub(super) fn associations_changed() {
        // SAFETY: SHCNE_ASSOCCHANGED takes no items.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Setup's package writes for Explorer, read from `Compi.wxs`.
    fn package_entries() -> Vec<Entry> {
        let source = include_str!("../../../installer/Compi.wxs");
        let attribute = |line: &str, attribute: &str| {
            let start = line.find(&format!(" {attribute}=\""))? + attribute.len() + 3;
            let length = line[start..].find('"')?;
            Some(line[start..start + length].to_owned())
        };
        source
            .lines()
            .filter(|line| line.contains(r#"Key="Software\Classes\"#))
            .map(|line| {
                let name = attribute(line, "Name").map(|name| match name.as_str() {
                    "FriendlyAppName" => "FriendlyAppName",
                    "Icon" => "Icon",
                    "Compi.File" => "Compi.File",
                    other => panic!("unexpected value name {other}"),
                });
                Entry {
                    key: attribute(line, "Key").unwrap(),
                    name,
                    value: attribute(line, "Value")
                        .unwrap()
                        .replace("&quot;", "\"")
                        .replace("[INSTALLFOLDER]", r"C:\Compi\"),
                }
            })
            .collect()
    }

    #[test]
    fn an_update_writes_exactly_what_setup_writes() {
        let mut ours = entries(r"C:\Compi\");
        let mut package = package_entries();
        ours.sort();
        package.sort();
        assert_eq!(ours, package);
    }
}

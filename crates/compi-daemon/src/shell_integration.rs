use crate::Result;
use std::ffi::OsString;

const BRIDGE: &str = include_str!("../../../assets/compi-shell.sh");
const BASH_FUNCTIONS: [&str; 17] = [
    "compi",
    "_compi_cli_init",
    "_compi_windows_cli",
    "_compi_choose_directory",
    "_compi_report_cwd",
    "_compi_prompt_cwd",
    "_compi_prompt_ble_hook",
    "_compi_prompt_ble_precmd",
    "_compi_prompt_native_dispatch",
    "_compi_prompt_command_restore",
    "_compi_prompt_sync",
    "_compi_prompt_startup",
    "_compi_prompt_snapshot",
    "_compi_prompt_restore",
    "_compi_prompt_status",
    "_compi_prompt_rerun",
    "_compi_install_prompt_hook",
];
const EXPORT_FUNCTIONS: &str = r#"if [[ $1 == compi-installed ]]; then set -- "$HOME/.compi/shell/compi-shell.sh"; fi; . "$1" || exit; names='compi _compi_cli_init _compi_windows_cli _compi_choose_directory _compi_report_cwd _compi_prompt_cwd _compi_prompt_ble_hook _compi_prompt_ble_precmd _compi_prompt_native_dispatch _compi_prompt_command_restore _compi_prompt_sync _compi_prompt_startup _compi_prompt_snapshot _compi_prompt_restore _compi_prompt_status _compi_prompt_rerun _compi_install_prompt_hook'; export -f $names || exit; for name in $names; do printenv "BASH_FUNC_${name}%%" || exit; printf '\0'; done"#;

#[cfg(unix)]
pub(crate) fn shell_name(executable: &str) -> Option<&'static str> {
    match executable.rsplit('/').next()? {
        "bash" => Some("bash"),
        "zsh" => Some("zsh"),
        _ => None,
    }
}

#[cfg(unix)]
pub(crate) fn prepare_unix(
    executable: &std::ffi::OsStr,
    home: &std::path::Path,
    env: &mut Vec<(OsString, OsString)>,
    argv: &mut Vec<OsString>,
    login: bool,
) -> Result<()> {
    use std::os::unix::ffi::OsStringExt;
    let Some(name) = executable.to_str().and_then(shell_name) else {
        return Ok(());
    };
    if !home.is_absolute() || !home.is_dir() {
        return Err(format!(
            "Compi shell integration requires an existing absolute HOME: {}",
            home.display()
        )
        .into());
    }
    let directory = home.join(".compi/shell");
    install_unix(&directory)?;
    if name == "zsh" {
        let original = env
            .iter()
            .rev()
            .find(|(key, _)| key == "ZDOTDIR")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("ZDOTDIR"));
        env.push(("ZDOTDIR".into(), directory.into_os_string()));
        env.push((
            "COMPI_ORIGINAL_ZDOTDIR_PRESENT".into(),
            if original.is_some() { "1" } else { "0" }.into(),
        ));
        env.push((
            "COMPI_ORIGINAL_ZDOTDIR".into(),
            original.unwrap_or_default(),
        ));
    } else if login {
        let functions = exported_functions(executable, &directory.join("compi-shell.sh"))?;
        for (name, value) in BASH_FUNCTIONS.iter().zip(functions) {
            env.push((
                format!("BASH_FUNC_{name}%%").into(),
                OsString::from_vec(value),
            ));
        }
        let existing = env
            .iter()
            .rev()
            .find(|(key, _)| key == "PROMPT_COMMAND")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("PROMPT_COMMAND"));
        let mut prompt = existing.unwrap_or_default();
        if !prompt.is_empty() {
            prompt.push("; ");
        }
        prompt.push("_compi_prompt_cwd");
        env.push(("PROMPT_COMMAND".into(), prompt));
    } else {
        argv.insert(0, "--rcfile".into());
        argv.insert(1, directory.join("bashrc").into_os_string());
    }
    Ok(())
}

#[cfg(unix)]
fn install_unix(directory: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "cannot create Compi shell directory {}: {error}",
            directory.display()
        )
    })?;
    if std::fs::symlink_metadata(directory)?
        .file_type()
        .is_symlink()
    {
        return Err(format!(
            "Compi shell directory must not be a symbolic link: {}",
            directory.display()
        )
        .into());
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    for (name, content) in shell_files() {
        let path = directory.join(name);
        if std::fs::read(&path).ok().as_deref() == Some(content.as_bytes()) {
            if name == "compi" {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            }
            continue;
        }
        let mut temp = None;
        for attempt in 0..32 {
            let candidate = directory.join(format!(".{name}.{}.{}", std::process::id(), attempt));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => {
                    temp = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let (candidate, mut file) = temp.ok_or("cannot allocate temporary Compi shell file")?;
        let result = (|| -> Result<()> {
            use std::io::Write;
            file.set_permissions(std::fs::Permissions::from_mode(if name == "compi" {
                0o700
            } else {
                0o600
            }))?;
            file.write_all(content.as_bytes())?;
            drop(file);
            std::fs::rename(&candidate, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&candidate);
        }
        result.map_err(|error| {
            format!(
                "cannot install Compi shell file {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn shell_files() -> [(&'static str, &'static str); 7] {
    [
        ("compi-shell.sh", BRIDGE),
        (
            "compi",
            r#"#!/bin/sh
if [ -z "${COMPI_CLI-}" ] && [ -n "${COMPI_CLI_WINDOWS-}" ]; then
    COMPI_CLI=$(wslpath -u "$COMPI_CLI_WINDOWS") || exit 1
fi
if [ -z "${COMPI_CLI-}" ]; then
    echo 'compi: this shell has no Compi CLI context; use the installed application CLI' >&2
    exit 2
fi
if [ -n "${COMPI_CLI_WINDOWS-}" ]; then
    if [ -z "${COMPI_DATA_DIR-}" ] && [ -n "${COMPI_CLI_DATA_DIR_WINDOWS-}" ]; then
        COMPI_DATA_DIR=$(wslpath -u "$COMPI_CLI_DATA_DIR_WINDOWS") || exit 1
        export COMPI_DATA_DIR
    fi
    export WSLENV="${WSLENV:+$WSLENV:}COMPI_INSTANCE:COMPI_SURFACE_ID:WSL_DISTRO_NAME:COMPI_DATA_DIR/p:COMPI_SHELL_CWD"
fi
export COMPI_SHELL_CWD="$PWD"
exec "$COMPI_CLI" "$@"
"#,
        ),
        (
            "bashrc",
            r#". "$HOME/.compi/shell/compi-shell.sh"
[[ ! -f $HOME/.bashrc ]] || . "$HOME/.bashrc"
! _compi_prompt_startup || . "$HOME/.compi/prompt/compi.bash"
_compi_enable_prompt_cwd
"#,
        ),
        (
            ".zshenv",
            r#"typeset -g _compi_shell_dir=$ZDOTDIR
if [[ -o interactive ]]; then source "$_compi_shell_dir/compi-shell.sh"; fi
typeset -g _compi_zdotdir_set=${COMPI_ORIGINAL_ZDOTDIR_PRESENT:-0}
typeset -g _compi_user_zdotdir=${COMPI_ORIGINAL_ZDOTDIR:-$HOME}
if (( _compi_zdotdir_set )); then ZDOTDIR=$COMPI_ORIGINAL_ZDOTDIR; else unset ZDOTDIR; fi
[[ ! -f $_compi_user_zdotdir/.zshenv ]] || source "$_compi_user_zdotdir/.zshenv"
_compi_zdotdir_set=${+ZDOTDIR}
_compi_user_zdotdir=${ZDOTDIR:-$HOME}
ZDOTDIR=$_compi_shell_dir
unset COMPI_ORIGINAL_ZDOTDIR COMPI_ORIGINAL_ZDOTDIR_PRESENT
"#,
        ),
        (
            ".zprofile",
            r#"if (( _compi_zdotdir_set )); then ZDOTDIR=$_compi_user_zdotdir; else unset ZDOTDIR; fi
[[ ! -f $_compi_user_zdotdir/.zprofile ]] || source "$_compi_user_zdotdir/.zprofile"
_compi_zdotdir_set=${+ZDOTDIR}
_compi_user_zdotdir=${ZDOTDIR:-$HOME}
ZDOTDIR=$_compi_shell_dir
"#,
        ),
        (
            ".zshrc",
            r#"if (( _compi_zdotdir_set )); then ZDOTDIR=$_compi_user_zdotdir; else unset ZDOTDIR; fi
[[ ! -f $_compi_user_zdotdir/.zshrc ]] || source "$_compi_user_zdotdir/.zshrc"
_compi_zdotdir_set=${+ZDOTDIR}
_compi_user_zdotdir=${ZDOTDIR:-$HOME}
! _compi_prompt_startup || source "$HOME/.compi/prompt/compi.zsh"
_compi_enable_prompt_cwd
if [[ -o login ]]; then
    ZDOTDIR=$_compi_shell_dir
elif (( _compi_zdotdir_set )); then
    ZDOTDIR=$_compi_user_zdotdir
else
    unset ZDOTDIR
fi
"#,
        ),
        (
            ".zlogin",
            r#"if (( _compi_zdotdir_set )); then ZDOTDIR=$_compi_user_zdotdir; else unset ZDOTDIR; fi
[[ ! -f $_compi_user_zdotdir/.zlogin ]] || source "$_compi_user_zdotdir/.zlogin"
unset _compi_shell_dir _compi_user_zdotdir _compi_zdotdir_set
"#,
        ),
    ]
}

#[cfg(unix)]
fn exported_functions(
    executable: &std::ffi::OsStr,
    script: &std::path::Path,
) -> Result<Vec<Vec<u8>>> {
    let output = std::process::Command::new(executable)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            EXPORT_FUNCTIONS,
            "compi-shell",
        ])
        .arg(script)
        .env_remove("BASH_ENV")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cannot export Compi Bash functions: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    parse_exported_functions(&output.stdout)
}

fn parse_exported_functions(stdout: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut entries = stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
    if entries.last() == Some(&&[][..]) {
        entries.pop();
    }
    for entry in &mut entries {
        if entry.last() == Some(&b'\n') {
            *entry = &entry[..entry.len() - 1];
        }
    }
    if entries.len() != BASH_FUNCTIONS.len()
        || entries.iter().any(|entry| !entry.starts_with(b"() {"))
    {
        return Err("cannot read installed Compi Bash functions".into());
    }
    Ok(entries.into_iter().map(|entry| entry.to_vec()).collect())
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct BridgeStamp {
    size: u64,
    written: u64,
    created: u64,
    attributes: u32,
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct DefaultHomeIdentity {
    uid: u32,
    environment: u64,
    passwd: BridgeStamp,
    configuration: Option<BridgeStamp>,
}

#[cfg(windows)]
fn default_home_identity(distribution: &str, name: &[u16]) -> Option<DefaultHomeIdentity> {
    use std::hash::{Hash, Hasher};
    use std::os::windows::fs::MetadataExt;
    type Query = unsafe extern "system" fn(
        *const u16,
        *mut u32,
        *mut u32,
        *mut u32,
        *mut *mut *mut std::ffi::c_char,
        *mut u32,
    ) -> i32;
    static QUERY: std::sync::LazyLock<Option<Query>> = std::sync::LazyLock::new(|| {
        let module = unsafe {
            windows::Win32::System::LibraryLoader::LoadLibraryExW(
                windows::core::w!("wslapi.dll"),
                None,
                windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .ok()?;
        let address = unsafe {
            windows::Win32::System::LibraryLoader::GetProcAddress(
                module,
                windows::core::s!("WslGetDistributionConfiguration"),
            )
        };
        let Some(address) = address else {
            let _ = unsafe { windows::Win32::Foundation::FreeLibrary(module) };
            return None;
        };
        // Keep the system DLL loaded for the cached function's process lifetime.
        Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Query>(address) })
    });
    let query = (*QUERY)?;
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pointer: *mut std::ffi::c_void);
    }
    let (mut version, mut uid, mut flags, mut count) = (0, 0, 0, 0);
    let mut environment = std::ptr::null_mut();
    if unsafe {
        query(
            name.as_ptr(),
            &mut version,
            &mut uid,
            &mut flags,
            &mut environment,
            &mut count,
        )
    } < 0
    {
        return None;
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    flags.hash(&mut hash);
    if !environment.is_null() {
        for index in 0..count as usize {
            let value = unsafe { *environment.add(index) };
            if !value.is_null() {
                unsafe { std::ffi::CStr::from_ptr(value) }
                    .to_bytes()
                    .hash(&mut hash);
                unsafe { CoTaskMemFree(value.cast()) };
            }
        }
        unsafe { CoTaskMemFree(environment.cast()) };
    }
    if version != 2 {
        return None;
    }
    let stamp = |metadata: std::fs::Metadata| BridgeStamp {
        size: metadata.file_size(),
        written: metadata.last_write_time(),
        created: metadata.creation_time(),
        attributes: metadata.file_attributes(),
    };
    let root = std::path::PathBuf::from(format!(r"\\wsl.localhost\{distribution}\etc"));
    let passwd = stamp(std::fs::metadata(root.join("passwd")).ok()?);
    let configuration = match std::fs::metadata(root.join("wsl.conf")) {
        Ok(metadata) => Some(stamp(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    Some(DefaultHomeIdentity {
        uid,
        environment: hash.finish(),
        passwd,
        configuration,
    })
}

#[cfg(windows)]
struct WslBridge {
    distribution: String,
    home: Option<String>,
    distribution_wide: Vec<u16>,
    default_home_identity: Option<DefaultHomeIdentity>,
    directory: std::path::PathBuf,
    stamps: Option<[BridgeStamp; 7]>,
    functions: Option<Vec<OsString>>,
    /// Digest of the installed `~/.compi/wincmd.<digest>` directory.
    commands: Option<String>,
}

#[cfg(windows)]
fn bridge_stamps(directory: &std::path::Path) -> Option<[BridgeStamp; 7]> {
    use std::os::windows::fs::MetadataExt;
    let directory_metadata = std::fs::symlink_metadata(directory).ok()?;
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        return None;
    }
    let mut stamps = [BridgeStamp {
        size: 0,
        written: 0,
        created: 0,
        attributes: 0,
    }; 7];
    for ((name, _), stamp) in shell_files().into_iter().zip(&mut stamps) {
        let metadata = std::fs::metadata(directory.join(name)).ok()?;
        if !metadata.is_file() {
            return None;
        }
        *stamp = BridgeStamp {
            size: metadata.file_size(),
            written: metadata.last_write_time(),
            created: metadata.creation_time(),
            attributes: metadata.file_attributes(),
        };
    }
    Some(stamps)
}

#[cfg(windows)]
fn install_wsl(distribution: &str, home_override: Option<&str>) -> Result<std::path::PathBuf> {
    use base64::Engine;
    use std::io::Write;
    let mut command = wsl_command(Some(distribution), home_override);
    command.args(["/bin/sh", "-c", WSL_INSTALL]);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot install Compi shell bridge in WSL: {error}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or("WSL shell installer stdin unavailable")?;
        for (name, content) in shell_files() {
            writeln!(
                stdin,
                "{name}\t{}",
                base64::engine::general_purpose::STANDARD.encode(content)
            )?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "cannot install Compi shell bridge in WSL distribution {distribution} ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let directory = std::str::from_utf8(
        output
            .stdout
            .strip_suffix(&[0])
            .ok_or("WSL shell installer did not report its directory")?,
    )?;
    if !directory.starts_with('/') {
        return Err("WSL shell installer reported a non-absolute directory".into());
    }
    Ok(std::path::PathBuf::from(format!(
        r"\\wsl.localhost\{}\{}",
        distribution,
        directory.trim_start_matches('/').replace('/', "\\")
    )))
}

/// Commands found in the Windows search path that WSL appends to PATH, in
/// search order. Bash looks a missing command up in every one of those
/// directories through the Windows drive bridge, which makes failed lookups
/// and command-name completion slow. Compi links these commands into one Linux
/// directory, `~/.compi/wincmd.<digest>`, and the launch wrapper puts that
/// directory in place of the appended Windows directories.
#[cfg(windows)]
struct WindowsCommands {
    /// Searched directories with their last write time; any change rescans.
    directories: Vec<(std::path::PathBuf, Option<u64>)>,
    /// Each directory with the commands it supplies; earlier directories win.
    commands: Vec<(String, Vec<String>)>,
    digest: String,
}

#[cfg(windows)]
fn windows_commands() -> Option<std::sync::Arc<WindowsCommands>> {
    use std::os::windows::fs::MetadataExt;
    static INDEX: std::sync::Mutex<Option<std::sync::Arc<WindowsCommands>>> =
        std::sync::Mutex::new(None);
    let directories: Vec<_> = std::env::split_paths(compi_protocol::wsl::launch_path()?)
        .map(|directory| {
            let written = std::fs::metadata(&directory)
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|metadata| metadata.last_write_time());
            (directory, written)
        })
        .collect();
    let mut index = INDEX.lock().ok()?;
    if let Some(current) = index.as_ref()
        && current.directories == directories
    {
        return Some(current.clone());
    }
    let extensions = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".to_owned());
    let built = std::sync::Arc::new(index_windows_commands(directories, &extensions));
    *index = Some(built.clone());
    Some(built)
}

/// Extensionless files (scripts such as VS Code's `code`) and files with a
/// `PATHEXT` extension are commands; the first directory supplying a name
/// wins, ignoring case as Windows directories do.
#[cfg(windows)]
fn index_windows_commands(
    directories: Vec<(std::path::PathBuf, Option<u64>)>,
    extensions: &str,
) -> WindowsCommands {
    use sha2::Digest;
    let extensions: Vec<&str> = extensions
        .split(';')
        .filter_map(|extension| extension.strip_prefix('.'))
        .filter(|extension| !extension.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut commands = Vec::new();
    let mut digest = sha2::Sha256::new();
    // Launcher format version: a changed format must not reuse old indexes.
    digest.update(b"compi-wincmd-launchers-1\0");
    for (directory, written) in &directories {
        let (Some(_), Some(directory)) = (written, directory.to_str()) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| {
                    !kind.is_dir()
                        && !(kind.is_symlink()
                            && std::fs::metadata(entry.path()).is_ok_and(|target| target.is_dir()))
                })
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                !name.chars().any(char::is_control)
                    && match name.rsplit_once('.') {
                        Some((_, extension)) => extensions
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(extension)),
                        None => true,
                    }
            })
            .collect();
        names.sort_unstable();
        names.retain(|name| seen.insert(name.to_lowercase()));
        if names.is_empty() {
            continue;
        }
        digest.update(directory.as_bytes());
        digest.update([0]);
        for name in &names {
            digest.update(name.as_bytes());
            digest.update([0]);
        }
        digest.update([0]);
        commands.push((directory.to_owned(), names));
    }
    let digest = digest.finalize();
    WindowsCommands {
        directories,
        commands,
        digest: digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

#[cfg(windows)]
fn install_windows_commands(
    distribution: &str,
    home_override: Option<&str>,
    commands: &WindowsCommands,
) -> Result<()> {
    use std::io::Write;
    let mut command = wsl_command(Some(distribution), home_override);
    command.args(["/bin/sh", "-c", WSL_WINDOWS_COMMANDS]);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn()?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or("WSL command installer stdin unavailable")?;
        let mut input = format!("{}\n", commands.digest);
        for (directory, names) in &commands.commands {
            input.push_str("D\t");
            input.push_str(directory);
            input.push('\n');
            for name in names {
                input.push_str("C\t");
                input.push_str(name);
                input.push('\t');
                input.push_str(&name.replace('\'', r"'\''"));
                input.push('\n');
            }
        }
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "cannot link Windows commands in WSL distribution {distribution} ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// Launch wrapper prefix: `$1` is the installed command index digest (empty
/// without one) and is shifted off. Windows directories WSL appended to PATH
/// (those under its drive mount root) are replaced, at the position of the
/// first, by the linked command directory. Startup files run afterwards, so
/// directories they add are unaffected. Without an index, or with WSL's
/// `appendWindowsPath` disabled, PATH is unchanged.
#[cfg(windows)]
macro_rules! windows_path {
    () => {
        r#"commands=${1:+$HOME/.compi/wincmd.$1}
shift
root=
while read -r _ point type options _; do
    case "$type:$options" in drvfs:*|9p:*aname=drvfs*) root=${point%/*}/; break ;; esac
done < /proc/self/mounts
if [ -n "$root" ] && [ -n "$commands" ] && [ -d "$commands" ]; then
    set -f; IFS=:; path= windows=
    for entry in $PATH; do
        case $entry in
            "$root"[A-Za-z]|"$root"[A-Za-z]/*)
                [ -n "$windows" ] || path=${path:+$path:}$commands
                windows=1 ;;
            *) path=${path:+$path:}$entry ;;
        esac
    done
    unset IFS; set +f
    [ -z "$windows" ] || PATH=$path
fi
unset commands root point type options path windows entry
"#
    };
}

#[cfg(windows)]
pub(crate) fn prepare_wsl(
    distribution: Option<&str>,
    home_override: Option<&str>,
    prompt_command: Option<&str>,
    login: bool,
    argv: &mut Vec<OsString>,
) -> Result<()> {
    static BRIDGES: std::sync::Mutex<Vec<WslBridge>> = std::sync::Mutex::new(Vec::new());
    let default_distribution;
    let distribution = match distribution {
        Some(name) => name,
        None => {
            default_distribution = compi_protocol::wsl::directory_distribution(None)?;
            &default_distribution
        }
    };
    let mut bridges = BRIDGES
        .lock()
        .map_err(|_| "WSL bridge cache was poisoned")?;
    if let Some(index) = bridges.iter().position(|bridge| {
        bridge.distribution == distribution && bridge.home.as_deref() == home_override
    }) {
        let bridge = &bridges[index];
        if bridge.stamps.is_none()
            || bridge_stamps(&bridge.directory) != bridge.stamps
            || (home_override.is_none()
                && (bridge.default_home_identity.is_none()
                    || default_home_identity(distribution, &bridge.distribution_wide)
                        != bridge.default_home_identity))
        {
            bridges.remove(index);
        }
    }
    let index = match bridges.iter().position(|bridge| {
        bridge.distribution == distribution && bridge.home.as_deref() == home_override
    }) {
        Some(index) => index,
        None => {
            let directory = install_wsl(distribution, home_override)?;
            let stamps = bridge_stamps(&directory);
            let distribution_wide = if home_override.is_none() {
                distribution.encode_utf16().chain([0]).collect()
            } else {
                Vec::new()
            };
            let default_home_identity = if home_override.is_none() {
                default_home_identity(distribution, &distribution_wide)
            } else {
                None
            };
            bridges.push(WslBridge {
                distribution: distribution.to_owned(),
                home: home_override.map(str::to_owned),
                distribution_wide,
                default_home_identity,
                directory,
                stamps,
                functions: None,
                commands: None,
            });
            bridges.len() - 1
        }
    };
    let bridge = &mut bridges[index];
    if let Some(commands) = windows_commands()
        && bridge.commands.as_deref() != Some(commands.digest.as_str())
    {
        // An index directory is immutable, so an existing one (e.g. from before
        // a daemon restart) needs no WSL call. A failed refresh keeps any
        // earlier index; without one, the shell keeps WSL's Windows directories.
        let installed = bridge
            .directory
            .parent()
            .is_some_and(|root| root.join(format!("wincmd.{}", commands.digest)).is_dir());
        let result = if installed {
            Ok(())
        } else {
            install_windows_commands(distribution, home_override, &commands)
        };
        match result {
            Ok(()) => bridge.commands = Some(commands.digest.clone()),
            Err(error) => eprintln!("compi-daemon: {error}"),
        }
    }
    let commands = OsString::from(bridge.commands.as_deref().unwrap_or_default());
    if login {
        if bridge.functions.is_none() {
            let output = wsl_command(Some(distribution), home_override)
                .args([
                    "/bin/bash",
                    "--noprofile",
                    "--norc",
                    "-c",
                    EXPORT_FUNCTIONS,
                    "compi-shell",
                    "compi-installed",
                ])
                .output()?;
            if !output.status.success() {
                return Err(format!("cannot export Compi Bash functions in WSL ({}) from ~/.compi/shell/compi-shell.sh: {}", output.status, String::from_utf8_lossy(&output.stderr)).into());
            }
            let functions = BASH_FUNCTIONS
                .iter()
                .zip(parse_exported_functions(&output.stdout)?)
                .map(|(name, value)| {
                    let mut assignment = format!("BASH_FUNC_{name}%%=").into_bytes();
                    assignment.extend(value);
                    String::from_utf8(assignment).map(OsString::from)
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            bridge.functions = Some(functions);
        }
        argv.extend_from_slice(bridge.functions.as_ref().expect("functions were exported"));
        let prompt = prompt_command
            .filter(|value| !value.is_empty())
            .map(|value| format!("{value}\n_compi_prompt_cwd"))
            .unwrap_or_else(|| "_compi_prompt_cwd".to_owned());
        argv.push(format!("PROMPT_COMMAND={prompt}").into());
        let start = argv.len() - (BASH_FUNCTIONS.len() + 1);
        argv.splice(
            start..start,
            [
                "/bin/sh".into(),
                "-c".into(),
                concat!(windows_path!(), "exec \"$@\"").into(),
                "compi-shell".into(),
                commands,
                "/usr/bin/env".into(),
            ],
        );
        argv.push("/bin/bash".into());
    } else {
        argv.extend([
            "/bin/sh".into(),
            "-c".into(),
            concat!(
                windows_path!(),
                "exec /bin/bash --rcfile \"$HOME/.compi/shell/bashrc\" \"$@\""
            )
            .into(),
            "compi-shell".into(),
            commands,
        ]);
    }
    Ok(())
}

#[cfg(windows)]
fn wsl_command(distribution: Option<&str>, home_override: Option<&str>) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(r"C:\Windows\System32\wsl.exe");
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    if let Some(path) = compi_protocol::wsl::launch_path() {
        command.env("PATH", path);
    }
    if let Some(distribution) = distribution {
        command.args(["--distribution", distribution]);
    }
    command.arg("--exec");
    if let Some(home) = home_override {
        command.args(["/usr/bin/env", &format!("HOME={home}")]);
    }
    command
}

#[cfg(windows)]
const WSL_INSTALL: &str = r#"set -eu
umask 077
case "$HOME" in /*) ;; *) echo 'Compi requires an absolute WSL HOME' >&2; exit 1 ;; esac
dir="$HOME/.compi/shell"
mkdir -p "$dir"
if [ -L "$dir" ]; then echo 'Compi shell directory must not be a symbolic link' >&2; exit 1; fi
chmod 700 "$dir"
while IFS='	' read -r name encoded; do
    case "$name" in compi|compi-shell.sh|bashrc|.zshenv|.zprofile|.zshrc|.zlogin) ;; *) echo 'invalid Compi shell file' >&2; exit 1 ;; esac
    if [ -f "$dir/$name" ] && [ "$(base64 -w 0 "$dir/$name")" = "$encoded" ]; then
        if [ "$name" = compi ]; then chmod 700 "$dir/$name"; fi
        continue
    fi
    tmp=$(mktemp "$dir/.compi-shell.XXXXXXXX")
    trap 'rm -f "$tmp"' EXIT
    printf '%s' "$encoded" | base64 -d > "$tmp"
    if cmp -s "$tmp" "$dir/$name"; then rm "$tmp"; else mv -f "$tmp" "$dir/$name"; fi
    if [ "$name" = compi ]; then chmod 700 "$dir/$name"; fi
done
printf '%s\0' "$dir"
"#;

/// Installs `~/.compi/wincmd.<digest>` from `<digest>` followed by `D<TAB>dir`
/// lines, each followed by `C<TAB>name<TAB>quoted-name` lines. Each command is
/// a small local launcher rather than a symbolic link, so executable checks
/// (command completion stats every candidate) stay on the Linux filesystem.
/// Index directories are immutable, so running shells keep a valid one; the
/// four newest are retained.
#[cfg(windows)]
const WSL_WINDOWS_COMMANDS: &str = r##"set -eu
umask 077
case "$HOME" in /*) ;; *) echo 'Compi requires an absolute WSL HOME' >&2; exit 1 ;; esac
root="$HOME/.compi"
mkdir -p "$root"
IFS= read -r digest
case "$digest" in ''|*[!0-9a-f]*) echo 'invalid Windows command index' >&2; exit 1 ;; esac
target="$root/wincmd.$digest"
if [ ! -d "$target" ]; then
    staging=$(mktemp -d "$root/.wincmd.XXXXXXXX")
    trap 'rm -rf "$staging"' EXIT
    directory=
    while IFS='	' read -r kind name quoted; do
        case "$kind" in
            D) directory=$(wslpath -u "$name" 2>/dev/null | sed "s/'/'\\\\''/g") ;;
            C) [ -z "$directory" ] || printf "#!/bin/sh\nexec '%s/%s' \"\$@\"\n" "$directory" "$quoted" > "$staging/$name" ;;
        esac
    done
    chmod -R u+x "$staging"
    mv -T "$staging" "$target" 2>/dev/null || [ -d "$target" ]
fi
touch "$target"
ls -1dt "$root"/wincmd.* | tail -n +5 | while IFS= read -r old; do rm -rf "$old"; done
"##;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn home() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "compi-shell-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn bash_interactive_reads_user_rc_and_loads_bridge_without_rewriting_rc() {
        let home = home();
        let rc = home.join(".bashrc");
        std::fs::write(&rc, "COMPI_RC_MARKER=original\n").unwrap();
        let mut env = Vec::new();
        let mut args = vec!["-i".into()];
        prepare_unix("/bin/bash".as_ref(), &home, &mut env, &mut args, false).unwrap();
        let output = Command::new("/bin/bash")
            .args(&args)
            .args(["-c", "[ \"$COMPI_RC_MARKER\" = original ] && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null && [[ $- = *i* ]]"])
            .env("HOME", &home).envs(env)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(rc).unwrap(),
            "COMPI_RC_MARKER=original\n"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn bash_login_retains_login_profile_and_imports_bridge() {
        let home = home();
        let rc = home.join(".bash_profile");
        std::fs::write(&rc, "COMPI_LOGIN_MARKER=original\n").unwrap();
        let mut env = Vec::new();
        let mut args = vec!["-l".into(), "-i".into()];
        prepare_unix("/bin/bash".as_ref(), &home, &mut env, &mut args, true).unwrap();
        let output = Command::new("/bin/bash")
            .args(&args)
            .args(["-c", "[ \"$COMPI_LOGIN_MARKER\" = original ] && shopt -q login_shell && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null"])
            .env("HOME", &home).envs(env)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(rc).unwrap(),
            "COMPI_LOGIN_MARKER=original\n"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn zsh_login_preserves_dynamic_zdotdir_and_startup_order() {
        if !std::path::Path::new("/bin/zsh").is_file() {
            return;
        }
        let home = home();
        let original = home.join("original");
        let changed = home.join("changed");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&changed).unwrap();
        std::fs::write(
            original.join(".zshenv"),
            "COMPI_ORDER=env\nZDOTDIR=\"$HOME/changed\"\n",
        )
        .unwrap();
        std::fs::write(
            changed.join(".zprofile"),
            "COMPI_ORDER+=:profile\ncompi() { print -r -- user-defined; }\n",
        )
        .unwrap();
        std::fs::write(changed.join(".zshrc"), "COMPI_ORDER+=:rc\n").unwrap();
        std::fs::write(changed.join(".zlogin"), "COMPI_ORDER+=:login\n").unwrap();
        let mut env = vec![("ZDOTDIR".into(), original.as_os_str().into())];
        let mut args = vec!["-l".into(), "-i".into()];
        prepare_unix("/bin/zsh".as_ref(), &home, &mut env, &mut args, true).unwrap();
        let output = Command::new("/bin/zsh")
            .args(&args)
            .args(["-c", "[[ $COMPI_ORDER == env:profile:rc:login && $ZDOTDIR == $HOME/changed && $(compi) == user-defined ]]"])
            .env("HOME", &home).envs(env)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(changed.join(".zshrc")).unwrap(),
            "COMPI_ORDER+=:rc\n"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn zsh_without_custom_zdotdir_leaves_it_unset() {
        if !std::path::Path::new("/bin/zsh").is_file() || std::env::var_os("ZDOTDIR").is_some() {
            return;
        }
        let home = home();
        let mut env = Vec::new();
        let mut args = vec!["-l".into(), "-i".into()];
        prepare_unix("/bin/zsh".as_ref(), &home, &mut env, &mut args, true).unwrap();
        let output = Command::new("/bin/zsh")
            .args(&args)
            .args(["-c", "[[ ${+ZDOTDIR} -eq 0 && $+functions[compi] -eq 1 ]]"])
            .env("HOME", &home)
            .envs(env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn wsl_startup_preserves_bash_rc_and_imports_login_functions() {
        let wsl = r"C:\Windows\System32\wsl.exe";
        let home = Command::new(wsl)
            .args(["--exec", "/bin/mktemp", "-d"])
            .output()
            .unwrap();
        assert!(home.status.success());
        let home = String::from_utf8(home.stdout).unwrap().trim().to_owned();
        let setup = Command::new(wsl)
            .args(["--exec", "/usr/bin/env", &format!("HOME={home}"), "/bin/sh", "-c",
                "printf '%s\\n' 'COMPI_RC_MARKER=original' > \"$HOME/.bashrc\"; printf '%s\\n' 'COMPI_LOGIN_MARKER=original' > \"$HOME/.bash_profile\""])
            .output().unwrap();
        assert!(setup.status.success());
        for login in [false, true] {
            let mut args = Vec::new();
            prepare_wsl(None, Some(&home), None, login, &mut args).unwrap();
            let expected = if login {
                "COMPI_LOGIN_MARKER"
            } else {
                "COMPI_RC_MARKER"
            };
            let mut shell = Command::new(wsl);
            shell
                .args(["--exec", "/usr/bin/env", &format!("HOME={home}")])
                .args(&args);
            if login {
                shell.args(["-l", "-i"]);
            } else {
                shell.arg("-i");
            }
            // WSL's appended Windows directories are replaced by the linked
            // index, and Windows commands still resolve through it.
            let condition = if login {
                "[ \"$COMPI_LOGIN_MARKER\" = original ] && shopt -q login_shell && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null && [[ :$PATH: != *:/mnt/[a-z]/* && $(type -P cmd.exe) == \"$HOME\"/.compi/wincmd.*/cmd.exe ]]"
            } else {
                "[ \"$COMPI_RC_MARKER\" = original ] && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null && [[ :$PATH: != *:/mnt/[a-z]/* && $(type -P cmd.exe) == \"$HOME\"/.compi/wincmd.*/cmd.exe ]]"
            };
            let shell = shell.args(["-c", condition]).output().unwrap();
            assert!(
                shell.status.success(),
                "{expected}: {}",
                String::from_utf8_lossy(&shell.stderr)
            );
        }
        let original = Command::new(wsl)
            .args(["--exec", "/bin/cat", &format!("{home}/.bashrc")])
            .output()
            .unwrap();
        assert_eq!(original.stdout, b"COMPI_RC_MARKER=original\n");
        let cleanup = Command::new(wsl)
            .args(["--exec", "/bin/rm", "-rf", &home])
            .status()
            .unwrap();
        assert!(cleanup.success());
    }
}

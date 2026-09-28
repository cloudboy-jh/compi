use crate::Result;
use std::ffi::OsString;

const BRIDGE: &str = include_str!("../../../assets/compi-shell.sh");
const BASH_FUNCTIONS: [&str; 4] = [
    "compi",
    "_compi_choose_directory",
    "_compi_report_cwd",
    "_compi_prompt_cwd",
];
const EXPORT_FUNCTIONS: &str = r#"if [[ $1 == compi-installed ]]; then set -- "$HOME/.compi/shell/compi-shell.sh"; fi; . "$1" || exit; export -f compi _compi_choose_directory _compi_report_cwd _compi_prompt_cwd || exit; for name in compi _compi_choose_directory _compi_report_cwd _compi_prompt_cwd; do printenv "BASH_FUNC_${name}%%" || exit; printf '\0'; done"#;

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
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
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

fn shell_files() -> [(&'static str, &'static str); 6] {
    [
        ("compi-shell.sh", BRIDGE),
        (
            "bashrc",
            r#". "$HOME/.compi/shell/compi-shell.sh"
[[ ! -f $HOME/.bashrc ]] || . "$HOME/.bashrc"
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
pub(crate) fn prepare_wsl(
    distribution: Option<&str>,
    home_override: Option<&str>,
    prompt_command: Option<&str>,
    login: bool,
    argv: &mut Vec<OsString>,
) -> Result<()> {
    use base64::Engine;
    let mut command = wsl_command(distribution, home_override);
    command.args(["/bin/sh", "-c", WSL_INSTALL]);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot install Compi shell bridge in WSL: {error}"))?;
    {
        use std::io::Write;
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
            "cannot install Compi shell bridge in WSL distribution {} ({}): {}",
            distribution.unwrap_or("default"),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    if login {
        let output = wsl_command(distribution, home_override)
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
        for (name, value) in BASH_FUNCTIONS
            .iter()
            .zip(parse_exported_functions(&output.stdout)?)
        {
            let mut assignment = format!("BASH_FUNC_{name}%%=").into_bytes();
            assignment.extend(value);
            argv.push(OsString::from(String::from_utf8(assignment)?));
        }
        let prompt = prompt_command
            .filter(|value| !value.is_empty())
            .map(|value| format!("{value}\n_compi_prompt_cwd"))
            .unwrap_or_else(|| "_compi_prompt_cwd".to_owned());
        argv.push(format!("PROMPT_COMMAND={prompt}").into());
        let start = argv.len() - (BASH_FUNCTIONS.len() + 1);
        argv.insert(start, "/usr/bin/env".into());
        argv.push("/bin/bash".into());
    } else {
        argv.extend([
            "/bin/sh".into(),
            "-c".into(),
            "exec /bin/bash --rcfile \"$HOME/.compi/shell/bashrc\" \"$@\"".into(),
            "compi-shell".into(),
        ]);
    }
    Ok(())
}

#[cfg(windows)]
fn wsl_command(distribution: Option<&str>, home_override: Option<&str>) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(r"C:\Windows\System32\wsl.exe");
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
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
    case "$name" in compi-shell.sh|bashrc|.zshenv|.zprofile|.zshrc|.zlogin) ;; *) echo 'invalid Compi shell file' >&2; exit 1 ;; esac
    tmp=$(mktemp "$dir/.compi-shell.XXXXXXXX")
    trap 'rm -f "$tmp"' EXIT
    printf '%s' "$encoded" | base64 -d > "$tmp"
    if cmp -s "$tmp" "$dir/$name"; then rm "$tmp"; else mv -f "$tmp" "$dir/$name"; fi
done
"#;

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
            let condition = if login {
                "[ \"$COMPI_LOGIN_MARKER\" = original ] && shopt -q login_shell && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null"
            } else {
                "[ \"$COMPI_RC_MARKER\" = original ] && declare -F compi >/dev/null && declare -F _compi_prompt_cwd >/dev/null"
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

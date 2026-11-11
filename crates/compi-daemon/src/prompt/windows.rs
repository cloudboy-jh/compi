//! Prompt settings for Windows PowerShell on this host: what the POSIX `PROBE` and
//! `PREVIEW` scripts do in a Unix account, done natively. Records use the scripts'
//! formats so both share their parsers. Paths use `/`, as in `C:/Users/me`.
use super::target::{Target, run};
use compi_protocol::prompt::PromptProvider;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Windows PowerShell, which Compi starts.
pub(super) fn powershell() -> String {
    env_path("SystemRoot")
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        .to_string_lossy()
        .into_owned()
}

/// `name.exe` on the search path or where its installers put it, since this daemon's
/// search path may predate the installation.
fn find(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.exe");
    let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    directories.extend(
        [
            env_path("LOCALAPPDATA").map(|dir| dir.join(r"Programs\oh-my-posh\bin")),
            env_path("ProgramFiles").map(|dir| dir.join(r"starship\bin")),
            env_path("LOCALAPPDATA").map(|dir| dir.join(r"Microsoft\WinGet\Links")),
            env_path("USERPROFILE").map(|dir| dir.join(r"scoop\shims")),
            env_path("ProgramData").map(|dir| dir.join(r"chocolatey\bin")),
            env_path("USERPROFILE").map(|dir| dir.join(r".cargo\bin")),
        ]
        .into_iter()
        .flatten(),
    );
    directories
        .into_iter()
        .map(|directory| directory.join(&file))
        .find(|path| path.is_file())
}

/// Standard output of a program that succeeded, else empty.
fn output(target: &Target, program: &Path, args: &[&str]) -> String {
    let mut command = target.command(&program.to_string_lossy());
    command.args(args);
    match run(command, &[], TIMEOUT) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => String::new(),
    }
}

fn is_theme(name: &str) -> bool {
    [".omp.json", ".omp.yaml", ".omp.yml", ".omp.toml"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// A profile line that sets up a prompt provider or loads Compi's block.
fn startup_line(text: &str) -> bool {
    ((text.contains("oh-my-posh") || text.contains("starship")) && text.contains("init"))
        || text.contains(">>> compi prompt")
        || text.contains("<<< compi prompt")
        || text.contains(".compi/prompt/normal.")
}

/// Everything detection needs, as `PROBE` records.
pub(super) fn probe_output(target: &Target) -> Result<String, String> {
    let home = env_path("USERPROFILE").ok_or("USERPROFILE is not set")?;
    let home = slashed(&home);
    let mut records = vec![
        format!(
            "host\t{}",
            std::env::var("COMPUTERNAME").unwrap_or_default()
        ),
        format!("home\t{home}"),
        format!("login_shell\t{}", powershell()),
    ];
    let shell = output(
        target,
        Path::new(&powershell()),
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetFolderPath('MyDocuments'); Get-ExecutionPolicy; Import-Module PSReadLine -ErrorAction Stop; (Get-Command Get-PSReadLineKeyHandler -ErrorAction Stop).Parameters.ContainsKey('Chord')",
        ],
    );
    let mut lines = shell.lines().map(str::trim);
    if let Some(documents) = lines.next().filter(|line| !line.is_empty()) {
        let folder = format!("{}/WindowsPowerShell", documents.replace('\\', "/"));
        records.push(format!(
            "profile\t{folder}/Microsoft.PowerShell_profile.ps1"
        ));
        for profile in ["Microsoft.PowerShell_profile.ps1", "profile.ps1"] {
            let path = format!("{folder}/{profile}");
            let Ok(content) = std::fs::read(&path) else {
                continue;
            };
            let content = String::from_utf8_lossy(&content);
            for (index, text) in content.lines().enumerate() {
                let text = text.trim_start_matches('\u{feff}').trim();
                if startup_line(text) {
                    records.push(format!("startup\t{path}\t{}:{text}", index + 1));
                }
            }
        }
    }
    if let Some(policy) = lines.next().filter(|line| !line.is_empty()) {
        records.push(format!("execution_policy\t{policy}"));
    }
    if let Some(supported) = lines.next().filter(|line| !line.is_empty()) {
        records.push(format!("psreadline_chord\t{supported}"));
    }
    if let Some(binary) = find(PromptProvider::OhMyPosh.executable()) {
        let version = output(target, &binary, &["--version"]);
        records.push(format!(
            "provider\toh-my-posh\t{}\t{}",
            slashed(&binary),
            version.lines().next().unwrap_or("")
        ));
        let mut directories: Vec<PathBuf> = env_path("POSH_THEMES_PATH").into_iter().collect();
        if let Some(prefix) = binary.parent().and_then(Path::parent) {
            directories.push(prefix.join("themes"));
        }
        let cache = output(target, &binary, &["cache", "path"]);
        if let Some(cache) = cache.lines().next().filter(|line| !line.trim().is_empty()) {
            directories.push(Path::new(cache.trim()).join("themes"));
        }
        for directory in directories {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            let mut themes: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.file_name().to_str().is_some_and(is_theme))
                .map(|entry| slashed(&entry.path()))
                .collect();
            themes.sort();
            records.extend(themes.into_iter().map(|path| format!("theme\t{path}")));
        }
    }
    if let Some(binary) = find(PromptProvider::Starship.executable()) {
        let version = output(target, &binary, &["--version"]);
        records.push(format!(
            "provider\tstarship\t{}\t{}",
            slashed(&binary),
            version.lines().next().unwrap_or("")
        ));
        let presets = output(target, &binary, &["preset", "--list"]);
        records.extend(
            presets
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| format!("preset\t{name}")),
        );
    }
    let starship_config = env_path("STARSHIP_CONFIG")
        .map(|path| slashed(&path))
        .unwrap_or_else(|| format!("{home}/.config/starship.toml"));
    if Path::new(&starship_config).is_file() {
        records.push(format!("starship_config\t{starship_config}"));
    }
    let base = format!("{home}/.compi/prompt");
    if let Ok(state) = std::fs::read_to_string(format!("{base}/state.json")) {
        records.push(format!("state\t{}", state.trim()));
    }
    if let Ok(entries) = std::fs::read_dir(format!("{base}/backups")) {
        for entry in entries.flatten() {
            if let Ok(manifest) = std::fs::read_to_string(entry.path().join("manifest.json")) {
                records.push(format!("backup\t{}", manifest.trim()));
            }
        }
    }
    let mut text = records.join("\n");
    text.push('\n');
    Ok(text)
}

/// Renders each style given as `kind:value`, in `PREVIEW`'s output format.
pub(super) fn preview_output(
    target: &Target,
    provider: PromptProvider,
    binary: &str,
    cwd: &str,
    width: &str,
    home: &str,
    specs: &[String],
) -> Result<String, String> {
    let directory = format!("{home}/.compi/prompt/preview");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let temporary = format!("{directory}/preview-{}.toml", std::process::id());
    let cwd = if cwd == "-" || !Path::new(cwd).is_dir() {
        home
    } else {
        cwd
    };
    let render = |config: Option<&str>, right: bool| {
        let mut command = target.command(binary);
        command.current_dir(cwd);
        match provider {
            PromptProvider::OhMyPosh => {
                command.args(["print", if right { "right" } else { "primary" }]);
                if let Some(config) = config {
                    command.args(["--config", config]);
                }
                command.args(["--shell", "pwsh", "--escape=false", "--pwd", cwd]);
                command.args(["--status", "0", "--execution-time", "120"]);
                command.args(["--terminal-width", width]);
            }
            PromptProvider::Starship => {
                command.env("STARSHIP_CONFIG", config.unwrap_or(""));
                command.env("STARSHIP_SHELL", "");
                command.arg("prompt");
                if right {
                    command.arg("--right");
                }
                command.args([
                    "--status=0".to_owned(),
                    "--cmd-duration=120".to_owned(),
                    format!("--path={cwd}"),
                    format!("--terminal-width={width}"),
                ]);
            }
        }
        match run(command, &[], TIMEOUT) {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            _ => String::new(),
        }
    };
    let mut text = String::new();
    for spec in specs {
        let (kind, value) = spec.split_once(':').unwrap_or((spec, ""));
        let config = match (provider, kind) {
            (PromptProvider::OhMyPosh, "default") => Ok(None),
            (PromptProvider::Starship, "default") => std::fs::write(&temporary, b"")
                .map(|()| Some(temporary.as_str()))
                .map_err(|error| error.to_string()),
            (PromptProvider::Starship, "preset") => {
                let mut command = target.command(binary);
                command.args(["preset", value, "-o", &temporary]);
                match run(command, &[], TIMEOUT) {
                    Ok(output) if output.status.success() => Ok(Some(temporary.as_str())),
                    _ => Err(format!("unknown preset {value}")),
                }
            }
            _ if Path::new(value).is_file() => Ok(Some(value)),
            _ => Err(format!("{value} does not exist")),
        };
        match config {
            Ok(config) => {
                text.push('O');
                text.push_str(&render(config, false));
                text.push('\u{1f}');
                text.push_str(&render(config, true));
            }
            Err(problem) => {
                text.push('E');
                text.push_str(&problem);
            }
        }
        text.push('\u{1e}');
    }
    let _ = std::fs::remove_file(&temporary);
    Ok(text)
}

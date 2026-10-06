//! Shell prompt settings, run where Compi launches shells. Detection reads the
//! environment; previews render the real provider with each style as is;
//! every change is planned, backed up and applied only with the plan's token.
mod config;
mod target;

use compi_protocol::ErrorCode;
use compi_protocol::prompt::{
    MAX_PREVIEW_STYLES, PromptApplied, PromptBackup, PromptConfig, PromptEnvironment,
    PromptFileAction, PromptFileChange, PromptOperation, PromptPlan, PromptPreview, PromptProvider,
    PromptProviderInfo, PromptRender, PromptRequest, PromptResponse, PromptScope, PromptShell,
    PromptShellInfo, PromptStartupLine, PromptState, PromptStyle, PromptStyleEntry,
    PromptStyleRender,
};
use config::{
    block, config_extension, existing_omp_config, find_block, init_script, line_diff, live_script,
    scope_name, style_label, with_block, without_block,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use target::{FileOperation, Target};

type Failure = (ErrorCode, String);

const MAX_BACKUPS: usize = 20;
const FACTS_TTL: Duration = Duration::from_secs(60);

fn invalid(message: impl Into<String>) -> Failure {
    (ErrorCode::InvalidRequest, message.into())
}

fn internal(message: impl Into<String>) -> Failure {
    (ErrorCode::Internal, message.into())
}

pub(crate) fn handle(
    distribution: Option<String>,
    request: PromptRequest,
) -> Result<PromptResponse, Failure> {
    let target = Target::resolve(distribution.as_deref()).map_err(invalid)?;
    match request {
        PromptRequest::Detect => {
            let facts = probe(&target)?;
            Ok(PromptResponse::Detected {
                environment: environment(&target, &facts),
            })
        }
        PromptRequest::Preview {
            provider,
            styles,
            cwd,
            width,
        } => Ok(PromptResponse::Previewed {
            preview: preview(&target, provider, &styles, cwd.as_deref(), width)?,
        }),
        PromptRequest::Plan { operation } => {
            let facts = probe(&target)?;
            let prepared = prepare(&target, &facts, &operation)?;
            Ok(PromptResponse::Planned {
                plan: prepared.plan(),
            })
        }
        PromptRequest::Execute { operation, token } => {
            let facts = probe(&target)?;
            let prepared = prepare(&target, &facts, &operation)?;
            if prepared.token != token {
                return Err((
                    ErrorCode::RevisionConflict,
                    "These prompt files changed after you reviewed the change. Review it again."
                        .into(),
                ));
            }
            execute(&target, &facts, &prepared)?;
            let facts = probe(&target)?;
            Ok(PromptResponse::Executed {
                environment: environment(&target, &facts),
            })
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    id: String,
    created_ms: u64,
    description: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    path: String,
    /// File name inside the backup directory; `None` when the path did not exist.
    file: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Facts {
    host: String,
    home: String,
    login_shell: String,
    providers: Vec<(PromptProvider, String, String)>,
    windows: Vec<(PromptProvider, String)>,
    themes: Vec<String>,
    presets: Vec<String>,
    starship_config: Option<String>,
    startup: Vec<(String, u32, String)>,
    profile_sources_bashrc: Option<bool>,
    state: PromptState,
    state_error: Option<String>,
    backups: Vec<Manifest>,
}

impl Facts {
    fn binary(&self, provider: PromptProvider) -> Option<&str> {
        self.providers
            .iter()
            .find(|(candidate, _, _)| *candidate == provider)
            .map(|(_, path, _)| path.as_str())
    }

    fn base(&self) -> String {
        format!("{}/.compi/prompt", self.home)
    }

    fn startup_file(&self, shell: PromptShell) -> String {
        match shell {
            PromptShell::Bash => format!("{}/.bashrc", self.home),
            PromptShell::Zsh => format!("{}/.zshrc", self.home),
        }
    }
}

static FACTS: Mutex<Option<(Target, Instant, Facts)>> = Mutex::new(None);

/// Everything detection needs, gathered by one script in the target.
const PROBE: &str = r#"umask 077
printf 'host\t%s\n' "$(uname -n 2>/dev/null)"
printf 'home\t%s\n' "$HOME"
user=$(id -un 2>/dev/null)
shell=''
if command -v getent >/dev/null 2>&1; then shell=$(getent passwd "$user" 2>/dev/null | cut -d: -f7); fi
if [ -z "$shell" ] && command -v dscl >/dev/null 2>&1; then
    shell=$(dscl . -read "/Users/$user" UserShell 2>/dev/null | sed 's/^UserShell: *//')
fi
printf 'login_shell\t%s\n' "${shell:-${SHELL:-}}"
PATH="$PATH:$HOME/.local/bin:$HOME/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/home/linuxbrew/.linuxbrew/bin:/snap/bin"
for name in oh-my-posh starship; do
    for bin in $(command -v "$name.exe" 2>/dev/null); do printf 'windows\t%s\t%s\n' "$name" "$bin"; done
    bin=$(command -v "$name" 2>/dev/null) || continue
    case $bin in /mnt/[a-z]/*) printf 'windows\t%s\t%s\n' "$name" "$bin"; continue ;; /*) ;; *) continue ;; esac
    printf 'provider\t%s\t%s\t%s\n' "$name" "$bin" "$("$bin" --version 2>/dev/null | head -n 1)"
    case $name in
        oh-my-posh)
            cache=$("$bin" cache path 2>/dev/null)
            if [ -n "$cache" ]; then
                for theme in "$cache"/themes/*.omp.json "$cache"/themes/*.omp.yaml "$cache"/themes/*.omp.yml "$cache"/themes/*.omp.toml; do
                    if [ -f "$theme" ]; then printf 'theme\t%s\n' "$theme"; fi
                done
            fi
            ;;
        starship)
            "$bin" preset --list 2>/dev/null | while IFS= read -r preset; do
                if [ -n "$preset" ]; then printf 'preset\t%s\n' "$preset"; fi
            done
            ;;
    esac
done
config=${STARSHIP_CONFIG:-$HOME/.config/starship.toml}
if [ -f "$config" ]; then printf 'starship_config\t%s\n' "$config"; fi
for file in "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile" "$HOME/.zshrc" "$HOME/.zprofile"; do
    if [ -f "$file" ]; then
        grep -n -E -e '(oh-my-posh|starship)[^ ]*[" ]+init' -e '(>>>|<<<) compi prompt' -e '\.compi/prompt/normal\.' -- "$file" 2>/dev/null |
            while IFS= read -r line; do printf 'startup\t%s\t%s\n' "$file" "$line"; done
    fi
done
if [ -f "$HOME/.bash_profile" ]; then
    if grep -q 'bashrc' "$HOME/.bash_profile" 2>/dev/null; then printf 'profile_sources_bashrc\tyes\n'; else printf 'profile_sources_bashrc\tno\n'; fi
fi
dir=$HOME/.compi/prompt
if [ -f "$dir/state.json" ]; then printf 'state\t'; tr -d '\n' < "$dir/state.json"; printf '\n'; fi
for manifest in "$dir"/backups/*/manifest.json; do
    if [ -f "$manifest" ]; then printf 'backup\t'; tr -d '\n' < "$manifest"; printf '\n'; fi
done
exit 0"#;

fn provider_named(name: &str) -> Option<PromptProvider> {
    PromptProvider::ALL
        .into_iter()
        .find(|provider| provider.executable() == name)
}

fn parse_probe(output: &str) -> Facts {
    let mut facts = Facts::default();
    for line in output.lines() {
        let Some((key, rest)) = line.split_once('\t') else {
            continue;
        };
        match key {
            "host" => facts.host = rest.to_owned(),
            "home" => facts.home = rest.to_owned(),
            "login_shell" => facts.login_shell = rest.to_owned(),
            "provider" => {
                let mut fields = rest.splitn(3, '\t');
                if let (Some(provider), Some(path)) =
                    (fields.next().and_then(provider_named), fields.next())
                {
                    let version = fields.next().unwrap_or("").trim();
                    let version = version.strip_prefix("starship ").unwrap_or(version);
                    facts
                        .providers
                        .push((provider, path.to_owned(), version.to_owned()));
                }
            }
            "windows" => {
                if let Some((provider, path)) = rest
                    .split_once('\t')
                    .and_then(|(name, path)| Some((provider_named(name)?, path)))
                {
                    facts.windows.push((provider, path.to_owned()));
                }
            }
            "theme" => facts.themes.push(rest.to_owned()),
            "preset" => facts.presets.push(rest.to_owned()),
            "starship_config" => facts.starship_config = Some(rest.to_owned()),
            "startup" => {
                if let Some((path, numbered)) = rest.split_once('\t')
                    && let Some((number, text)) = numbered.split_once(':')
                    && let Ok(number) = number.parse()
                {
                    facts
                        .startup
                        .push((path.to_owned(), number, text.trim().to_owned()));
                }
            }
            "profile_sources_bashrc" => facts.profile_sources_bashrc = Some(rest == "yes"),
            "state" => match serde_json::from_str(rest) {
                Ok(state) => facts.state = state,
                Err(error) => facts.state_error = Some(error.to_string()),
            },
            "backup" => {
                if let Ok(manifest) = serde_json::from_str::<Manifest>(rest)
                    && valid_backup_id(&manifest.id)
                {
                    facts.backups.push(manifest);
                }
            }
            _ => {}
        }
    }
    facts
        .themes
        .sort_by_key(|path| style_label(path).to_lowercase());
    facts.themes.dedup();
    facts
        .backups
        .sort_by_key(|manifest| std::cmp::Reverse(manifest.created_ms));
    facts
}

fn valid_backup_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn probe(target: &Target) -> Result<Facts, Failure> {
    let output = target
        .sh_ok("could not inspect the shell environment", PROBE, &[], &[])
        .map_err(internal)?;
    let facts = parse_probe(&String::from_utf8_lossy(&output));
    if !facts.home.starts_with('/') {
        return Err(internal("the shell environment has no absolute HOME"));
    }
    *FACTS.lock() = Some((target.clone(), Instant::now(), facts.clone()));
    Ok(facts)
}

fn cached_facts(target: &Target) -> Result<Facts, Failure> {
    if let Some((cached, at, facts)) = &*FACTS.lock()
        && cached == target
        && at.elapsed() < FACTS_TTL
    {
        return Ok(facts.clone());
    }
    probe(target)
}

fn compi_shell() -> String {
    #[cfg(windows)]
    {
        "/bin/bash".into()
    }
    #[cfg(unix)]
    {
        crate::launch::default_shell()
    }
}

fn existing_omp_configs(facts: &Facts) -> Vec<String> {
    let mut configs = Vec::new();
    for (_, _, text) in &facts.startup {
        if text.contains("oh-my-posh")
            && let Some(path) = existing_omp_config(text, &facts.home)
            && !configs.contains(&path)
        {
            configs.push(path);
        }
    }
    configs
}

fn styles(facts: &Facts, provider: PromptProvider) -> Vec<PromptStyleEntry> {
    let mut styles = Vec::new();
    match provider {
        PromptProvider::OhMyPosh => {
            for path in existing_omp_configs(facts) {
                styles.push(PromptStyleEntry {
                    label: format!("{} (yours)", style_label(&path)),
                    style: PromptStyle::Existing { path },
                });
            }
            styles.push(PromptStyleEntry {
                style: PromptStyle::Default,
                label: "Oh My Posh default".into(),
            });
            styles.extend(facts.themes.iter().map(|path| PromptStyleEntry {
                label: style_label(path),
                style: PromptStyle::Theme { path: path.clone() },
            }));
        }
        PromptProvider::Starship => {
            if let Some(path) = &facts.starship_config {
                styles.push(PromptStyleEntry {
                    label: format!("{} (yours)", path.rsplit('/').next().unwrap_or(path)),
                    style: PromptStyle::Existing { path: path.clone() },
                });
            }
            styles.push(PromptStyleEntry {
                style: PromptStyle::Default,
                label: "Starship default".into(),
            });
            styles.extend(facts.presets.iter().map(|name| PromptStyleEntry {
                label: name.clone(),
                style: PromptStyle::Preset { name: name.clone() },
            }));
        }
    }
    styles
}

fn style_name(style: &PromptStyle) -> String {
    match style {
        PromptStyle::Default => "default".into(),
        PromptStyle::Theme { path } | PromptStyle::Existing { path } => style_label(path),
        PromptStyle::Preset { name } => name.clone(),
    }
}

fn environment(target: &Target, facts: &Facts) -> PromptEnvironment {
    let label = match target.distribution() {
        Some(name) => format!("WSL {name}"),
        None => facts.host.clone(),
    };
    let compi_shell = compi_shell();
    let compi = PromptShellInfo {
        shell: PromptShell::from_path(&compi_shell),
        path: compi_shell,
    };
    let login = PromptShellInfo {
        shell: PromptShell::from_path(&facts.login_shell),
        path: facts.login_shell.clone(),
    };
    let providers = PromptProvider::ALL
        .into_iter()
        .map(|provider| {
            let found = facts.providers.iter().find(|(p, _, _)| *p == provider);
            PromptProviderInfo {
                provider,
                path: found.map(|(_, path, _)| path.clone()),
                version: found
                    .map(|(_, _, version)| version.clone())
                    .filter(|version| !version.is_empty()),
                styles: if found.is_some() {
                    styles(facts, provider)
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    let startup_lines = facts
        .startup
        .iter()
        .map(|(path, line, text)| PromptStartupLine {
            path: path.clone(),
            line: *line,
            text: text.clone(),
            provider: if text.contains("oh-my-posh") {
                Some(PromptProvider::OhMyPosh)
            } else if text.contains("starship") {
                Some(PromptProvider::Starship)
            } else {
                None
            },
            managed: text.contains("compi prompt") || text.contains(".compi/prompt/"),
        })
        .collect();
    #[cfg(windows)]
    let distributions = compi_protocol::wsl::wsl2_distributions().unwrap_or_default();
    #[cfg(unix)]
    let distributions = Vec::new();
    PromptEnvironment {
        notes: notes(&label, facts, &compi, &login),
        target: label,
        distribution: target.distribution(),
        distributions,
        home: facts.home.clone(),
        compi_shell: compi,
        login_shell: login,
        providers,
        startup_lines,
        state: facts.state.clone(),
        backups: facts
            .backups
            .iter()
            .map(|manifest| PromptBackup {
                id: manifest.id.clone(),
                created_ms: manifest.created_ms,
                description: manifest.description.clone(),
                paths: manifest
                    .entries
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect(),
            })
            .collect(),
    }
}

fn notes(
    label: &str,
    facts: &Facts,
    compi: &PromptShellInfo,
    login: &PromptShellInfo,
) -> Vec<String> {
    let mut notes = Vec::new();
    if facts.providers.is_empty() {
        notes.push(format!(
            "Neither Oh My Posh nor Starship is installed in {label}. Install one there yourself (https://ohmyposh.dev or https://starship.rs), then refresh. Compi does not install prompt providers."
        ));
    }
    for (provider, path) in &facts.windows {
        if facts.binary(*provider).is_none() {
            notes.push(format!(
                "{} is installed on Windows ({path}), but shells in {label} need the Linux build installed inside the distribution.",
                provider.label()
            ));
        }
    }
    match (compi.shell, login.shell) {
        (None, _) => notes.push(format!(
            "Compi starts {} here. Prompt settings support Bash and Zsh, so Compi-shell prompts do not apply to it.",
            compi.path
        )),
        (Some(compi_shell), Some(login_shell)) if compi_shell != login_shell => {
            notes.push(format!(
                "Compi starts {} in {label}, but your login shell is {}. Applying changes Compi's {} prompt; turn on \"Also use outside Compi\" to change {} too.",
                compi_shell.name(),
                login_shell.name(),
                compi_shell.name(),
                login_shell.name()
            ));
        }
        _ => {}
    }
    if login.shell.is_none() {
        notes.push(format!(
            "The login shell {} is not Bash or Zsh, so login-shell prompts are unavailable.",
            if login.path.is_empty() {
                "(unknown)"
            } else {
                &login.path
            }
        ));
    }
    if login.shell == Some(PromptShell::Bash) && facts.profile_sources_bashrc == Some(false) {
        notes.push(
            "~/.bash_profile does not load ~/.bashrc, so login Bash shells will not pick up a login-shell prompt."
                .into(),
        );
    }
    for (scope, applied) in [
        ("Compi-shell", facts.state.compi.as_ref()),
        ("login-shell", facts.state.normal.as_ref()),
    ] {
        if let Some(applied) = applied
            && facts.binary(applied.config.provider).is_none()
        {
            notes.push(format!(
                "The {scope} prompt uses {}, which is no longer installed, so those shells keep their own prompt.",
                applied.config.provider.label()
            ));
        }
    }
    if let Some(error) = &facts.state_error {
        notes.push(format!(
            "~/.compi/prompt/state.json could not be read ({error}), so Compi treats prompts as not applied."
        ));
    }
    notes
}

fn validate_style(config: &PromptConfig) -> Result<(), Failure> {
    match (&config.provider, &config.style) {
        (_, PromptStyle::Default) => Ok(()),
        (PromptProvider::OhMyPosh, PromptStyle::Theme { path })
        | (_, PromptStyle::Existing { path }) => target::validate_path(path).map_err(invalid),
        (PromptProvider::Starship, PromptStyle::Preset { name })
            if !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)) =>
        {
            Ok(())
        }
        _ => Err(invalid(format!(
            "that style is not available for {}",
            config.provider.label()
        ))),
    }
}

/// The chosen style's configuration, which Compi copies unchanged.
fn style_config(target: &Target, binary: &str, config: &PromptConfig) -> Result<Vec<u8>, Failure> {
    validate_style(config)?;
    let context = "could not read the prompt style";
    match (&config.provider, &config.style) {
        (PromptProvider::OhMyPosh, PromptStyle::Default) => target
            .sh_ok(
                context,
                r#"exec "$1" config export --format json"#,
                &[binary],
                &[],
            )
            .map_err(internal),
        (
            PromptProvider::OhMyPosh,
            PromptStyle::Theme { path } | PromptStyle::Existing { path },
        ) => target
            .sh_ok(
                context,
                r#"if [ ! -f "$2" ]; then echo "$2 does not exist" >&2; exit 1; fi
exec "$1" config export --config "$2" --format json"#,
                &[binary, path],
                &[],
            )
            .map_err(internal),
        (PromptProvider::Starship, PromptStyle::Default) => Ok(Vec::new()),
        (PromptProvider::Starship, PromptStyle::Preset { name }) => target
            .sh_ok(context, r#"exec "$1" preset "$2""#, &[binary, name], &[])
            .map_err(internal),
        (PromptProvider::Starship, PromptStyle::Existing { path }) => target
            .read_files(std::slice::from_ref(path))
            .map_err(internal)?
            .pop()
            .flatten()
            .ok_or_else(|| invalid(format!("{path} does not exist"))),
        _ => Err(invalid("that style is not available for this provider")),
    }
}

fn provider_binary(facts: &Facts, provider: PromptProvider) -> Result<&str, Failure> {
    facts.binary(provider).ok_or_else(|| {
        invalid(format!(
            "{} is not installed in this environment. Install it yourself, then refresh.",
            provider.label()
        ))
    })
}

/// Renders each style given as `kind:value`. Per style it prints `O` and the
/// fields separated by US, or `E` and a reason; styles are separated by RS.
const PREVIEW: &str = r#"set -u
umask 077
provider=$1 binary=$2 cwd=$3 width=$4 shell=$5
shift 5
dir=$HOME/.compi/prompt/preview
mkdir -p "$dir" || exit 1
tmp=$(mktemp "$dir/preview.XXXXXX") || exit 1
trap 'rm -f "$tmp"' EXIT
if [ "$cwd" = - ] || ! cd "$cwd" 2>/dev/null; then cd "$HOME" || exit 1; fi
render() {
    case $provider in
        oh-my-posh)
            if [ -n "$config" ]; then
                "$binary" print "$1" --config "$config" --shell "$shell" --escape=false \
                    --pwd "$PWD" --status "$2" --execution-time "$3" --terminal-width "$width" 2>/dev/null
            else
                "$binary" print "$1" --shell "$shell" --escape=false \
                    --pwd "$PWD" --status "$2" --execution-time "$3" --terminal-width "$width" 2>/dev/null
            fi
            ;;
        starship)
            side=; if [ "$1" = right ]; then side=--right; fi
            STARSHIP_CONFIG=$config STARSHIP_SHELL= "$binary" prompt $side --status="$2" \
                --cmd-duration="$3" --path="$PWD" --terminal-width="$width" 2>/dev/null
            ;;
    esac
}
for spec in "$@"; do
    kind=${spec%%:*} value=${spec#*:} config= problem=
    case $provider:$kind in
        oh-my-posh:default) ;;
        starship:default) : > "$tmp"; config=$tmp ;;
        starship:preset) "$binary" preset "$value" -o "$tmp" 2>/dev/null && config=$tmp || problem="unknown preset $value" ;;
        *) if [ -f "$value" ]; then config=$value; else problem="$value does not exist"; fi ;;
    esac
    if [ -n "$problem" ]; then
        printf 'E%s\036' "$problem"
        continue
    fi
    printf 'O'; render primary 0 120; printf '\037'; render right 0 120; printf '\036'
done"#;

fn style_spec(provider: PromptProvider, style: &PromptStyle) -> Result<String, Failure> {
    validate_style(&PromptConfig {
        provider,
        style: style.clone(),
    })?;
    Ok(match style {
        PromptStyle::Default => "default:-".into(),
        PromptStyle::Theme { path } => format!("theme:{path}"),
        PromptStyle::Existing { path } => format!("existing:{path}"),
        PromptStyle::Preset { name } => format!("preset:{name}"),
    })
}

fn preview(
    target: &Target,
    provider: PromptProvider,
    styles: &[PromptStyle],
    cwd: Option<&str>,
    width: u16,
) -> Result<PromptPreview, Failure> {
    if styles.is_empty() || styles.len() > MAX_PREVIEW_STYLES {
        return Err(invalid(format!(
            "preview between 1 and {MAX_PREVIEW_STYLES} styles at a time"
        )));
    }
    let specs = styles
        .iter()
        .map(|style| style_spec(provider, style))
        .collect::<Result<Vec<_>, _>>()?;
    let facts = cached_facts(target)?;
    let binary = provider_binary(&facts, provider)?;
    let cwd = cwd
        .filter(|cwd| target::validate_path(cwd).is_ok())
        .unwrap_or("-");
    let width = width.clamp(20, 400).to_string();
    let shell = PromptShell::from_path(&compi_shell()).unwrap_or(PromptShell::Bash);
    let mut args = vec![provider.executable(), binary, cwd, &width, shell.name()];
    args.extend(specs.iter().map(String::as_str));
    let output = target.sh(PREVIEW, &args, &[]).map_err(internal)?;
    if !output.status.success() {
        return Err(internal(format!(
            "{} preview failed: {}",
            provider.label(),
            target::error_text(&output)
        )));
    }
    let renders = parse_preview(&String::from_utf8_lossy(&output.stdout), styles.len());
    Ok(PromptPreview { renders })
}

fn parse_preview(output: &str, count: usize) -> Vec<PromptStyleRender> {
    let mut records = output.split('\u{1e}');
    (0..count)
        .map(|_| {
            let record = records.next().unwrap_or("");
            if let Some(reason) = record.strip_prefix('E') {
                return PromptStyleRender {
                    success: PromptRender::default(),
                    error: Some(reason.to_owned()),
                };
            }
            let fields: Vec<&str> = record
                .strip_prefix('O')
                .unwrap_or("")
                .split('\u{1f}')
                .collect();
            let success = PromptRender {
                left: fields.first().copied().unwrap_or("").to_owned(),
                right: fields.get(1).copied().unwrap_or("").to_owned(),
            };
            let error = success
                .left
                .trim()
                .is_empty()
                .then(|| "the provider rendered nothing for this style".to_owned());
            PromptStyleRender { success, error }
        })
        .collect()
}

struct Change {
    path: String,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
    user: bool,
}

struct Prepared {
    summary: String,
    notes: Vec<String>,
    changes: Vec<Change>,
    /// Every managed file as it is now; each change backs all of them up, so a
    /// restore returns prompt settings to that moment.
    snapshot: Vec<(String, Option<Vec<u8>>)>,
    token: String,
}

impl Prepared {
    fn plan(&self) -> PromptPlan {
        PromptPlan {
            token: self.token.clone(),
            summary: self.summary.clone(),
            changes: self
                .changes
                .iter()
                .map(|change| PromptFileChange {
                    path: change.path.clone(),
                    action: match (&change.old, &change.new) {
                        (None, _) => PromptFileAction::Create,
                        (Some(_), Some(_)) => PromptFileAction::Replace,
                        (Some(_), None) => PromptFileAction::Delete,
                    },
                    user_file: change.user,
                    diff: change.user.then(|| {
                        line_diff(
                            &String::from_utf8_lossy(change.old.as_deref().unwrap_or_default()),
                            &String::from_utf8_lossy(change.new.as_deref().unwrap_or_default()),
                        )
                    }),
                })
                .collect(),
            notes: self.notes.clone(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// Desired content per path: `Some(None)` deletes; user files are marked.
struct Desired {
    files: Vec<(String, Option<Vec<u8>>, bool)>,
}

impl Desired {
    fn set(&mut self, path: String, content: Option<Vec<u8>>, user: bool) {
        self.files.retain(|(existing, _, _)| *existing != path);
        self.files.push((path, content, user));
    }

    /// Compi's config and init files for one scope; `None` removes them all.
    fn scope(
        &mut self,
        base: &str,
        scope: PromptScope,
        applied: Option<(&PromptConfig, &str, &[u8], &[PromptShell])>,
    ) {
        let name = scope_name(scope);
        for provider in PromptProvider::ALL {
            let content = applied
                .filter(|(config, ..)| config.provider == provider)
                .map(|(_, _, content, _)| content.to_vec());
            self.set(
                format!("{base}/{name}.{}", config_extension(provider)),
                content,
                false,
            );
        }
        for shell in PromptShell::ALL {
            let script = applied.filter(|(.., shells)| shells.contains(&shell)).map(
                |(config, binary, ..)| {
                    let path = format!("{base}/{name}.{}", config_extension(config.provider));
                    init_script(config.provider, binary, shell, &path).into_bytes()
                },
            );
            self.set(format!("{base}/{name}.{}", shell.name()), script, false);
        }
    }
}

/// Keeps the original time when nothing about an applied scope changes.
fn keep_applied_time(previous: Option<&PromptApplied>, applied: &mut PromptApplied) {
    if let Some(previous) = previous
        && previous.config == applied.config
        && previous.shell == applied.shell
        && previous.startup_file == applied.startup_file
    {
        applied.applied_at_ms = previous.applied_at_ms;
    }
}

fn prepare(
    target: &Target,
    facts: &Facts,
    operation: &PromptOperation,
) -> Result<Prepared, Failure> {
    let base = facts.base();
    let login = PromptShell::from_path(&facts.login_shell);
    let mut startup_files: Vec<String> = PromptShell::ALL
        .into_iter()
        .map(|shell| facts.startup_file(shell))
        .collect();
    if let Some(path) = facts
        .state
        .normal
        .as_ref()
        .and_then(|applied| applied.startup_file.clone())
        && !startup_files.contains(&path)
    {
        target::validate_path(&path).map_err(internal)?;
        startup_files.push(path);
    }
    let mut universe: Vec<String> = Vec::new();
    for scope in ["compi", "normal"] {
        for suffix in ["omp.json", "starship.toml", "bash", "zsh"] {
            universe.push(format!("{base}/{scope}.{suffix}"));
        }
    }
    for name in ["live.bash", "live.zsh", "state.json"] {
        universe.push(format!("{base}/{name}"));
    }
    universe.extend(startup_files.iter().cloned());
    if let PromptOperation::Restore { backup } = operation
        && let Some(manifest) = facts.backups.iter().find(|manifest| manifest.id == *backup)
    {
        for entry in &manifest.entries {
            if !universe.contains(&entry.path) {
                universe.push(entry.path.clone());
            }
        }
    }
    for path in &universe {
        target::validate_path(path).map_err(internal)?;
    }
    let contents = target.read_files(&universe).map_err(internal)?;
    let snapshot: Vec<(String, Option<Vec<u8>>)> = universe.into_iter().zip(contents).collect();
    let current = |path: &str| -> Option<Option<Vec<u8>>> {
        snapshot
            .iter()
            .find(|(candidate, _)| candidate == path)
            .map(|(_, content)| content.clone())
    };
    let startup_content = |path: &str| -> String {
        current(path)
            .flatten()
            .map(|content| String::from_utf8_lossy(&content).into_owned())
            .unwrap_or_default()
    };
    let mut desired = Desired { files: Vec::new() };
    let mut state = facts.state.clone();
    let mut notes = Vec::new();
    // Removes the login-shell prompt: its files and Compi's block in any startup file.
    let remove_outside = |desired: &mut Desired| -> bool {
        desired.scope(&base, PromptScope::Normal, None);
        let mut found = false;
        for path in &startup_files {
            let content = startup_content(path);
            if find_block(&content).is_some() {
                found = true;
                desired.set(
                    path.clone(),
                    Some(without_block(&content).into_bytes()),
                    true,
                );
            }
        }
        found
    };
    let summary;
    match operation {
        PromptOperation::Apply { config, outside } => {
            let binary = provider_binary(facts, config.provider)?;
            let content = style_config(target, binary, config)?;
            desired.scope(
                &base,
                PromptScope::Compi,
                Some((config, binary, &content, &PromptShell::ALL)),
            );
            let mut compi = PromptApplied {
                config: config.clone(),
                shell: None,
                startup_file: None,
                applied_at_ms: now_ms(),
            };
            keep_applied_time(state.compi.as_ref(), &mut compi);
            state.compi = Some(compi);
            let mut target_label = "Compi shells".to_owned();
            if *outside {
                let shell = login.ok_or_else(|| {
                    invalid(format!(
                        "the login shell {} is not Bash or Zsh",
                        facts.login_shell
                    ))
                })?;
                desired.scope(
                    &base,
                    PromptScope::Normal,
                    Some((config, binary, &content, &[shell])),
                );
                let startup = facts.startup_file(shell);
                for path in &startup_files {
                    if *path != startup {
                        let content = startup_content(path);
                        if find_block(&content).is_some() {
                            desired.set(
                                path.clone(),
                                Some(without_block(&content).into_bytes()),
                                true,
                            );
                        }
                    }
                }
                let content = startup_content(&startup);
                desired.set(
                    startup.clone(),
                    Some(with_block(&content, &block(shell)).into_bytes()),
                    true,
                );
                for (path, line, text) in &facts.startup {
                    if *path == startup
                        && !(text.contains("compi prompt") || text.contains(".compi/prompt/"))
                    {
                        notes.push(format!(
                            "{path} line {line} already sets up a prompt ({text}). Compi's block runs after it and takes over; that line is not changed."
                        ));
                    }
                }
                notes.push(
                    "Shells outside Compi pick this up when they start; open a new one to see it."
                        .into(),
                );
                let mut normal = PromptApplied {
                    config: config.clone(),
                    shell: Some(shell),
                    startup_file: Some(startup),
                    applied_at_ms: now_ms(),
                };
                keep_applied_time(state.normal.as_ref(), &mut normal);
                state.normal = Some(normal);
                target_label = format!("Compi shells and {} outside Compi", shell.name());
            } else {
                remove_outside(&mut desired);
                state.normal = None;
            }
            summary = format!(
                "Apply {} \u{b7} {} to {target_label}",
                config.provider.label(),
                style_name(&config.style),
            );
        }
        PromptOperation::Disable => {
            desired.scope(&base, PromptScope::Compi, None);
            let found = remove_outside(&mut desired);
            if !found && state == PromptState::default() {
                return Err(invalid("no prompt is applied"));
            }
            state = PromptState::default();
            summary = "Turn off the prompt".to_owned();
        }
        PromptOperation::Restore { backup } => {
            let manifest = facts
                .backups
                .iter()
                .find(|manifest| manifest.id == *backup)
                .ok_or_else(|| invalid("that backup no longer exists"))?;
            let directory = format!("{base}/backups/{}", manifest.id);
            let files: Vec<String> = manifest
                .entries
                .iter()
                .filter_map(|entry| entry.file.as_ref())
                .map(|file| format!("{directory}/{file}"))
                .collect();
            for file in &files {
                target::validate_path(file).map_err(internal)?;
            }
            let saved = target.read_files(&files).map_err(internal)?;
            let mut saved = saved.into_iter();
            let mut restored_state = None;
            for entry in &manifest.entries {
                target::validate_path(&entry.path).map_err(internal)?;
                let content = match &entry.file {
                    Some(_) => Some(
                        saved
                            .next()
                            .flatten()
                            .ok_or_else(|| internal("a file is missing from that backup"))?,
                    ),
                    None => None,
                };
                if entry.path.starts_with(&format!("{base}/")) {
                    if entry.path == format!("{base}/state.json") {
                        restored_state = Some(content.clone());
                    }
                    desired.set(entry.path.clone(), content, false);
                } else {
                    // Only Compi's block comes back; later edits to the rest of the file stay.
                    let saved_block = content
                        .as_deref()
                        .and_then(|content| find_block(&String::from_utf8_lossy(content)));
                    let current = startup_content(&entry.path);
                    let next = match saved_block {
                        Some(block) => with_block(&current, &block),
                        None => without_block(&current),
                    };
                    desired.set(entry.path.clone(), Some(next.into_bytes()), true);
                }
            }
            state = match restored_state {
                Some(Some(content)) => serde_json::from_slice(&content)
                    .map_err(|_| internal("the backup's prompt state is invalid"))?,
                Some(None) => PromptState::default(),
                None => state,
            };
            summary = format!("Restore prompt files from backup {}", manifest.id);
        }
    }
    let state_path = format!("{base}/state.json");
    let state_content = (state != PromptState::default())
        .then(|| serde_json::to_vec(&state).map_err(|error| internal(error.to_string())))
        .transpose()?;
    desired.set(state_path.clone(), state_content, false);
    for shell in PromptShell::ALL {
        desired.set(
            format!("{base}/live.{}", shell.name()),
            live_script(shell, &state, &base).map(String::into_bytes),
            false,
        );
    }
    let mut changes = Vec::new();
    for (path, new, user) in desired.files {
        let old = current(&path)
            .ok_or_else(|| internal(format!("{path} is not a prompt file Compi manages")))?;
        // An empty user file that never existed does not need creating.
        let unchanged = old == new || (user && old.is_none() && new.as_deref() == Some(b""));
        if !unchanged {
            changes.push(Change {
                path,
                old,
                new,
                user,
            });
        }
    }
    changes.sort_by(|left, right| right.user.cmp(&left.user).then(left.path.cmp(&right.path)));

    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(operation).unwrap_or_default());
    hasher.update(b"\0");
    let digest = |content: &Option<Vec<u8>>| match content {
        Some(content) => Sha256::digest(content).to_vec(),
        None => b"missing".to_vec(),
    };
    for change in &changes {
        hasher.update(change.path.as_bytes());
        hasher.update(digest(&change.old));
        if change.path == state_path {
            // The new state records when it was applied; the operation already defines it.
            hasher.update(change.new.is_some().to_string());
        } else {
            hasher.update(digest(&change.new));
        }
    }
    let token = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Prepared {
        summary,
        notes,
        changes,
        snapshot,
        token,
    })
}

/// Backs up every managed file, writes the plan, and bumps the reload token
/// so running Compi shells switch at their next prompt.
fn execute(target: &Target, facts: &Facts, prepared: &Prepared) -> Result<(), Failure> {
    if prepared.changes.is_empty() {
        return Ok(());
    }
    let base = facts.base();
    let now = now_ms();
    let mut operations = Vec::new();
    let id = format!("{now}");
    let directory = format!("{base}/backups/{id}");
    let mut entries = Vec::new();
    for (index, (path, old)) in prepared.snapshot.iter().enumerate() {
        let file = old.as_ref().map(|old| {
            let name = index.to_string();
            operations.push(FileOperation::Write {
                path: format!("{directory}/{name}"),
                content: old.clone(),
            });
            name
        });
        entries.push(ManifestEntry {
            path: path.clone(),
            file,
        });
    }
    let manifest = Manifest {
        id,
        created_ms: now,
        description: format!("Before: {}", prepared.summary),
        entries,
    };
    operations.push(FileOperation::Write {
        path: format!("{directory}/manifest.json"),
        content: serde_json::to_vec(&manifest).map_err(|error| internal(error.to_string()))?,
    });
    for change in &prepared.changes {
        operations.push(match (&change.new, change.user) {
            (Some(content), true) => FileOperation::WriteUser {
                path: change.path.clone(),
                content: content.clone(),
            },
            (Some(content), false) => FileOperation::Write {
                path: change.path.clone(),
                content: content.clone(),
            },
            (None, _) => FileOperation::Delete {
                path: change.path.clone(),
            },
        });
    }
    operations.push(FileOperation::Write {
        path: format!("{base}/reload"),
        content: format!("{now}\n").into_bytes(),
    });
    // Backups are newest first; drop the oldest beyond the limit.
    let total = facts.backups.len() + 1;
    for manifest in facts
        .backups
        .iter()
        .rev()
        .take(total.saturating_sub(MAX_BACKUPS))
    {
        operations.push(FileOperation::RemoveBackup {
            path: format!("{base}/backups/{}", manifest.id),
        });
    }
    target.apply(&operations).map_err(internal)?;
    *FACTS.lock() = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETUP: &str = r#"set -eu
home=$1
mkdir -p "$home/.local/bin" "$home/.cache/oh-my-posh/themes" "$home/themes"
cat > "$home/.local/bin/oh-my-posh" <<'EOF'
#!/bin/sh
case $1 in
    --version) echo 9.9.9 ;;
    cache) echo "$HOME/.cache/oh-my-posh" ;;
    config) if [ "$3" = --config ]; then cat "$4"; else echo '{"blocks":[]}'; fi ;;
    print) shift; echo "OMP $*" ;;
esac
EOF
cat > "$home/.local/bin/starship" <<'EOF'
#!/bin/sh
case $1 in
    --version) printf 'starship 1.2.3\nbranch:\n' ;;
    preset)
        if [ "$2" = --list ]; then printf 'pure-preset\nplain-text-symbols\n'
        elif [ "$2" != pure-preset ]; then exit 1
        elif [ "${3-}" = -o ]; then printf 'format = "$all"\n' > "$4"
        else printf 'format = "$all"\n'; fi ;;
    prompt) shift; echo "STAR $* config=$(cat "$STARSHIP_CONFIG")" ;;
esac
EOF
chmod +x "$home/.local/bin/oh-my-posh" "$home/.local/bin/starship"
echo '{"blocks":[{"type":"prompt","segments":[{"type":"path"}]}]}' > "$home/themes/mine.omp.json"
echo '{"blocks":[]}' > "$home/.cache/oh-my-posh/themes/cache-theme.omp.json"
for rc in .bashrc .zshrc; do
    printf 'export KEEP=1\neval "$(oh-my-posh init sh --config ~/themes/mine.omp.json)"\n' > "$home/$rc"
done"#;

    fn request(request: PromptRequest) -> Result<PromptResponse, Failure> {
        handle(None, request)
    }

    fn detect() -> PromptEnvironment {
        match request(PromptRequest::Detect).unwrap() {
            PromptResponse::Detected { environment } => environment,
            other => panic!("{other:?}"),
        }
    }

    fn preview(provider: PromptProvider, styles: Vec<PromptStyle>) -> PromptPreview {
        match request(PromptRequest::Preview {
            provider,
            styles,
            cwd: None,
            width: 80,
        })
        .unwrap()
        {
            PromptResponse::Previewed { preview } => preview,
            other => panic!("{other:?}"),
        }
    }

    fn plan(operation: &PromptOperation) -> PromptPlan {
        match request(PromptRequest::Plan {
            operation: operation.clone(),
        })
        .unwrap()
        {
            PromptResponse::Planned { plan } => plan,
            other => panic!("{other:?}"),
        }
    }

    fn execute(operation: &PromptOperation, token: &str) -> Result<PromptEnvironment, Failure> {
        request(PromptRequest::Execute {
            operation: operation.clone(),
            token: token.into(),
        })
        .map(|response| match response {
            PromptResponse::Executed { environment } => environment,
            other => panic!("{other:?}"),
        })
    }

    fn review_and_apply(operation: &PromptOperation) -> (PromptPlan, PromptEnvironment) {
        let plan = plan(operation);
        let environment = execute(operation, &plan.token).unwrap();
        (plan, environment)
    }

    fn read(path: &str) -> Option<String> {
        Target::resolve(None)
            .unwrap()
            .read_files(&[path.to_owned()])
            .unwrap()
            .pop()
            .flatten()
            .map(|content| String::from_utf8(content).unwrap())
    }

    fn scenario(home: &str) {
        let base = format!("{home}/.compi/prompt");
        let environment = detect();
        assert_eq!(environment.home, home);
        let omp = environment.provider(PromptProvider::OhMyPosh).unwrap();
        assert_eq!(omp.version.as_deref(), Some("9.9.9"));
        let mine = format!("{home}/themes/mine.omp.json");
        assert_eq!(
            omp.styles[0].style,
            PromptStyle::Existing { path: mine.clone() }
        );
        assert_eq!(omp.styles[0].label, "mine (yours)");
        assert!(omp.styles.iter().any(|style| style.label == "cache-theme"));
        let starship = environment.provider(PromptProvider::Starship).unwrap();
        assert_eq!(starship.version.as_deref(), Some("1.2.3"));
        assert!(starship.styles.iter().any(|style| style.style
            == PromptStyle::Preset {
                name: "pure-preset".into()
            }));
        assert!(
            environment
                .startup_lines
                .iter()
                .any(|line| line.provider == Some(PromptProvider::OhMyPosh) && !line.managed)
        );

        // A batch renders every style as is, in order, with per-style errors.
        let rows = preview(
            PromptProvider::OhMyPosh,
            vec![
                PromptStyle::Existing { path: mine.clone() },
                PromptStyle::Default,
                PromptStyle::Theme {
                    path: format!("{home}/themes/missing.omp.json"),
                },
            ],
        );
        assert_eq!(rows.renders.len(), 3);
        let row = &rows.renders[0];
        assert!(row.error.is_none(), "{row:?}");
        assert!(row.success.left.contains("primary --config") && row.success.left.contains(&mine));
        assert!(row.success.right.contains("right") && row.success.left.contains("--status 0"));
        assert!(!rows.renders[1].success.left.contains("--config"));
        assert!(
            rows.renders[2]
                .error
                .as_deref()
                .unwrap()
                .contains("does not exist")
        );
        let presets = preview(
            PromptProvider::Starship,
            vec![
                PromptStyle::Preset {
                    name: "pure-preset".into(),
                },
                PromptStyle::Preset {
                    name: "nope".into(),
                },
            ],
        );
        assert!(presets.renders[0].success.left.contains("config=format"));
        assert!(presets.renders[1].error.is_some());
        let too_many = request(PromptRequest::Preview {
            provider: PromptProvider::OhMyPosh,
            styles: vec![PromptStyle::Default; MAX_PREVIEW_STYLES + 1],
            cwd: None,
            width: 80,
        });
        assert!(matches!(too_many, Err((ErrorCode::InvalidRequest, _))));

        // Compi shells only: no user file changes; the style is copied as is.
        let config = PromptConfig {
            provider: PromptProvider::OhMyPosh,
            style: PromptStyle::Existing { path: mine.clone() },
        };
        let compi = PromptOperation::Apply {
            config: config.clone(),
            outside: false,
        };
        let (applied_plan, environment) = review_and_apply(&compi);
        assert!(applied_plan.changes.iter().all(|change| !change.user_file));
        assert_eq!(environment.state.compi.as_ref().unwrap().config, config);
        assert!(environment.state.normal.is_none());
        assert_eq!(
            read(&format!("{base}/compi.omp.json")).unwrap(),
            read(&mine).unwrap()
        );
        assert!(
            read(&format!("{base}/compi.zsh"))
                .unwrap()
                .contains("init zsh --config")
        );
        assert!(
            read(&format!("{base}/live.bash"))
                .unwrap()
                .contains("compi.bash")
        );
        assert!(read(&format!("{base}/reload")).is_some());
        // A reviewed plan is single-use; applying the same settings again changes nothing.
        assert_eq!(
            execute(&compi, &applied_plan.token).unwrap_err().0,
            ErrorCode::RevisionConflict
        );
        assert!(plan(&compi).changes.is_empty());

        if let Some(shell) = environment.login_shell.shell {
            let startup = format!("{home}/.{}rc", shell.name());
            let original = read(&startup).unwrap();
            let outside = PromptOperation::Apply {
                config: config.clone(),
                outside: true,
            };
            let (outside_plan, environment) = review_and_apply(&outside);
            let user: Vec<_> = outside_plan
                .changes
                .iter()
                .filter(|change| change.user_file)
                .collect();
            assert_eq!(user.len(), 1);
            assert_eq!(user[0].path, startup);
            assert!(
                user[0]
                    .diff
                    .as_deref()
                    .unwrap()
                    .contains("+# >>> compi prompt >>>")
            );
            assert!(
                outside_plan
                    .notes
                    .iter()
                    .any(|note| note.contains("already sets up a prompt"))
            );
            assert!(
                outside_plan
                    .summary
                    .contains(&format!("and {} outside Compi", shell.name()))
            );
            assert_eq!(
                environment.state.normal.as_ref().unwrap().shell,
                Some(shell)
            );
            assert_eq!(environment.state.compi.as_ref().unwrap().config, config);
            let with = read(&startup).unwrap();
            assert!(with.starts_with(&original) && with.contains(BLOCK_MARK));

            // Applying without `outside` removes only the login-shell prompt.
            let (inside_plan, environment) = review_and_apply(&compi);
            assert!(inside_plan.changes.iter().any(|change| change.user_file));
            assert_eq!(read(&startup).unwrap(), original);
            assert_eq!(read(&format!("{base}/normal.{}", shell.name())), None);
            assert!(environment.state.normal.is_none() && environment.state.compi.is_some());

            // Restoring keeps later edits to the rest of the file.
            let edited = format!("{original}alias later=1\n");
            Target::resolve(None)
                .unwrap()
                .apply(&[FileOperation::WriteUser {
                    path: startup.clone(),
                    content: edited.clone().into_bytes(),
                }])
                .unwrap();
            let restore = PromptOperation::Restore {
                backup: environment.backups[0].id.clone(),
            };
            let (_, environment) = review_and_apply(&restore);
            let restored = read(&startup).unwrap();
            assert!(restored.starts_with(&edited) && restored.contains(BLOCK_MARK));
            assert!(environment.state.normal.is_some());
        }

        // Turning off removes every managed prompt and any block.
        let (_, environment) = review_and_apply(&PromptOperation::Disable);
        assert_eq!(environment.state, PromptState::default());
        assert_eq!(read(&format!("{base}/compi.omp.json")), None);
        assert_eq!(read(&format!("{base}/state.json")), None);
        assert_eq!(read(&format!("{base}/live.bash")), None);
        if let Some(shell) = environment.login_shell.shell {
            let startup = read(&format!("{home}/.{}rc", shell.name())).unwrap();
            assert!(!startup.contains(BLOCK_MARK) && startup.contains("alias later=1"));
        }
        assert!(matches!(
            request(PromptRequest::Plan {
                operation: PromptOperation::Disable
            }),
            Err((ErrorCode::InvalidRequest, _))
        ));
    }

    const BLOCK_MARK: &str = "# >>> compi prompt >>>";

    #[test]
    fn detect_preview_apply_turn_off_and_restore_in_a_temporary_home() {
        let target = Target::resolve(None).unwrap();
        let home = target.sh_ok("mktemp", "mktemp -d", &[], &[]).unwrap();
        let home = String::from_utf8(home).unwrap().trim().to_owned();
        target.sh_ok("setup", SETUP, &[&home], &[]).unwrap();
        *target::TEST_ENV.lock() = vec![
            ("HOME".into(), home.clone()),
            ("PATH".into(), format!("{home}/.local/bin:/usr/bin:/bin")),
        ];
        let result = std::panic::catch_unwind(|| scenario(&home));
        target::TEST_ENV.lock().clear();
        let _ = target.sh(r#"rm -rf -- "$1""#, &[&home], &[]);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn preview_records_map_to_styles_in_order_even_when_output_stops_early() {
        let renders = parse_preview("Oleft\u{1f}right\u{1e}Ebad preset\u{1e}O\u{1f}\u{1e}", 4);
        assert_eq!(renders.len(), 4);
        assert_eq!(renders[0].success.left, "left");
        assert_eq!(renders[0].success.right, "right");
        assert_eq!(renders[0].error, None);
        assert_eq!(renders[1].error.as_deref(), Some("bad preset"));
        // An empty render and a missing record are reported, not shown blank.
        assert!(renders[2].error.is_some() && renders[3].error.is_some());
    }

    #[test]
    fn probe_output_parses_records_and_ignores_unknown_lines() {
        let facts = parse_probe(
            "host\tbox\nhome\t/h\nlogin_shell\t/usr/bin/zsh\nprovider\tstarship\t/b/starship\tstarship 1.2.3\n\
             windows\toh-my-posh\t/mnt/c/x/oh-my-posh.exe\ntheme\t/t/b.omp.json\ntheme\t/t/a.omp.json\n\
             startup\t/h/.zshrc\t12:eval \"$(starship init zsh)\"\nstate\tnot json\nnoise\n\
             backup\t{\"id\":\"1\",\"created_ms\":1,\"description\":\"d\",\"entries\":[]}\n\
             backup\t{\"id\":\"../x\",\"created_ms\":2,\"description\":\"d\",\"entries\":[]}\n",
        );
        assert_eq!(facts.binary(PromptProvider::Starship), Some("/b/starship"));
        assert_eq!(facts.providers[0].2, "1.2.3");
        assert_eq!(
            facts.windows,
            [(PromptProvider::OhMyPosh, "/mnt/c/x/oh-my-posh.exe".into())]
        );
        assert_eq!(facts.themes, ["/t/a.omp.json", "/t/b.omp.json"]);
        assert_eq!(
            facts.startup,
            [(
                "/h/.zshrc".into(),
                12,
                "eval \"$(starship init zsh)\"".into()
            )]
        );
        assert!(facts.state_error.is_some());
        // A backup id is a path component, so unsafe ids are dropped.
        assert_eq!(facts.backups.len(), 1);
        let notes = notes(
            "WSL Ubuntu",
            &facts,
            &PromptShellInfo {
                path: "/bin/bash".into(),
                shell: Some(PromptShell::Bash),
            },
            &PromptShellInfo {
                path: "/usr/bin/zsh".into(),
                shell: Some(PromptShell::Zsh),
            },
        );
        assert!(
            notes
                .iter()
                .any(|note| note.contains("Oh My Posh is installed on Windows"))
        );
        assert!(
            notes
                .iter()
                .any(|note| note.contains("your login shell is zsh"))
        );
        assert!(
            notes
                .iter()
                .any(|note| note.contains("state.json could not be read"))
        );
    }
}

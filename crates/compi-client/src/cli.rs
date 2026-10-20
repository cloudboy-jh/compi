//! Public, existing-daemon-only command interface. Presentation operations are routed
//! to the owning native host; observation and explicit input never acquire a view.
mod capture;
mod changes;
mod content;
mod diff;
mod help;
mod targets;
#[cfg(any(windows, target_os = "macos"))]
mod update;
mod view;

use crate::{DaemonClient, Result, arrangement, config, connection::ConnectionTarget};
use compi_protocol::{
    LayoutNode, MutationId, MutationRequest, PaneId, ScreenSnapshot, SplitAxis, SurfaceId,
    SurfaceInfo, SurfaceStatus, WorkspaceMutation, WorkspaceSession, WorkspaceSnapshot,
    WorkspaceTab,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const COMMANDS: &[&str] = &[
    "workspace",
    "tab",
    "pane",
    "window",
    "split",
    "files",
    "edit",
    "diff",
    "settings",
    "monitor",
    "layout",
    "run",
    "attach",
    "detach",
    "theme",
    "update",
    "changes",
    "help",
];
static NEXT_MUTATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct CliError {
    exit: i32,
    code: &'static str,
    message: String,
    /// Files a refused or cancelled close would have discarded; in the JSON error.
    unsaved_files: Vec<UnsavedFile>,
}
impl CliError {
    fn new(exit: i32, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            exit,
            code,
            message: message.into(),
            unsaved_files: Vec::new(),
        }
    }
    fn usage(message: impl Into<String>) -> Self {
        Self::new(2, "invalid_arguments", message)
    }
    fn target(message: impl Into<String>) -> Self {
        Self::new(3, "target_not_found_or_ambiguous", message)
    }
}
impl From<crate::Error> for CliError {
    fn from(error: crate::Error) -> Self {
        if let Some(daemon) = error.downcast_ref::<compi_protocol::DaemonError>() {
            use compi_protocol::ErrorCode;
            let (exit, code) = match daemon.code {
                ErrorCode::InvalidRequest => (2, "invalid_request"),
                ErrorCode::SurfaceNotFound => (3, "surface_not_found"),
                ErrorCode::AlreadyAttached => (5, "already_attached"),
                ErrorCode::NotAttached => (5, "not_attached"),
                ErrorCode::SurfaceUnavailable => (5, "surface_unavailable"),
                ErrorCode::Busy => (5, "busy"),
                ErrorCode::IncompatibleProtocol => (5, "incompatible_protocol"),
                ErrorCode::RevisionConflict => (6, "revision_conflict"),
                ErrorCode::FileConflict => (6, "file_conflict"),
                ErrorCode::StaleGeneration => (6, "stale_generation"),
                ErrorCode::StaleLifetime => (6, "stale_lifetime"),
                ErrorCode::MutationIdReused => (1, "mutation_id_reused"),
                ErrorCode::OutcomeUnknown => (1, "outcome_unknown"),
                ErrorCode::PersistenceUnavailable => (1, "persistence_unavailable"),
                ErrorCode::Internal => (1, "internal"),
            };
            return Self::new(exit, code, &daemon.message);
        }
        Self::new(1, "operation_failed", error.to_string())
    }
}

#[derive(Debug, Default)]
struct Options {
    instance: Option<String>,
    connect: Option<String>,
    workspace: Option<String>,
    tab: Option<String>,
    pane: Option<String>,
    window: Option<String>,
    surface: Option<String>,
    config: Option<PathBuf>,
    json: bool,
    help: bool,
    separator: bool,
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
    with: Vec<String>,
    args: Vec<String>,
}

/// Return `None` for legacy GUI invocations, or the completed CLI exit status.
/// Dispatch this before native GUI setup, including on headless Unix.
pub fn dispatch(raw: &[String]) -> Option<i32> {
    let version = raw
        .iter()
        .any(|arg| matches!(arg.as_str(), "--version" | "-V"))
        && raw
            .iter()
            .all(|arg| matches!(arg.as_str(), "--version" | "-V" | "--json"));
    let mut unknown = None;
    if !version {
        match classify(raw, shell_surface().is_some(), |word| {
            shell_path(word).is_ok_and(|path| path.is_dir())
        }) {
            Invocation::Gui => return None,
            Invocation::Cli => {}
            Invocation::Unknown(word) => unknown = Some(word),
        }
    }
    #[cfg(windows)]
    attach_parent_console();
    if version {
        let version = env!("CARGO_PKG_VERSION");
        if wants_json(raw) {
            println!("{}", json!({"ok": true, "result": {"version": version}}));
        } else {
            println!("compi {version}");
        }
        return Some(0);
    }
    watch_bridged_input();
    let json_output = wants_json(raw);
    let result = match unknown {
        Some(word) => Err(unknown_command(word)),
        None => parse(raw).and_then(|options| execute(&options)),
    };
    Some(match result {
        Ok(Output::Help(text)) => {
            if json_output {
                println!("{}", json!({"ok":true,"result":{"help":text}}));
            } else {
                print!("{text}");
            }
            0
        }
        Ok(Output::Value(value)) => {
            if json_output {
                println!("{}", json!({"ok": true, "result": value}));
            } else {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            }
            0
        }
        Ok(Output::Report { text, value }) => {
            if json_output {
                println!("{}", json!({"ok": true, "result": value}));
            } else {
                print!("{text}");
                let _ = io::stdout().flush();
            }
            0
        }
        Ok(Output::Capture(value)) => {
            if json_output {
                println!("{}", json!({"ok": true, "result": value}));
            } else {
                print!("{}", value["text"].as_str().unwrap());
                let _ = io::stdout().flush();
            }
            0
        }
        Err(error) => {
            if json_output {
                let mut detail =
                    json!({"code":error.code,"message":error.message,"exit_code":error.exit});
                if !error.unsaved_files.is_empty() {
                    detail["unsaved_files"] = json!(error.unsaved_files);
                }
                eprintln!("{}", json!({"ok": false, "error": detail}));
            } else {
                eprintln!("compi: {}", error.message);
            }
            error.exit
        }
    })
}

fn wants_json(raw: &[String]) -> bool {
    let mut args = raw.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--json" {
            return true;
        }
        if matches!(
            arg.as_str(),
            "--instance"
                | "--connect"
                | "--workspace"
                | "--tab"
                | "--pane"
                | "--window"
                | "--surface"
                | "--config"
                | "--cwd"
                | "--name"
                | "--panes"
                | "--layout"
                | "--index"
                | "--cols"
                | "--rows"
                | "--text"
                | "--key"
                | "--scope"
                | "--format"
                | "--with"
        ) {
            args.next();
        }
    }
    false
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    // AttachConsole can replace other standard handles when stdout is absent.
    // Preserve valid redirected input/error handles as well as redirected output.
    unsafe {
        let handles = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|kind| {
            (
                kind,
                GetStdHandle(kind)
                    .ok()
                    .filter(|handle| !handle.is_invalid()),
            )
        });
        if handles[1].1.is_some() {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            for (kind, handle) in handles {
                if let Some(handle) = handle {
                    let _ = SetStdHandle(kind, handle);
                }
            }
        }
    }
}

/// How `dispatch` handles a non-version invocation.
#[derive(Debug, PartialEq)]
enum Invocation<'a> {
    Cli,
    /// The native application, given GUI flags or a working directory.
    Gui,
    /// A bare word, inside a Compi shell, that is neither a command nor a directory.
    Unknown(&'a str),
}

/// Outside a Compi shell an unrecognised first word stays a GUI working directory,
/// as `compi PATH` always was. Inside one it is almost always a mistyped command.
fn classify<'a>(
    raw: &'a [String],
    in_shell: bool,
    is_directory: impl FnOnce(&str) -> bool,
) -> Invocation<'a> {
    let mut index = 0;
    while index < raw.len() {
        let arg = raw[index].as_str();
        if matches!(
            arg,
            "--instance"
                | "--connect"
                | "--workspace"
                | "--tab"
                | "--pane"
                | "--window"
                | "--surface"
                | "--config"
        ) {
            index += 2;
            continue;
        }
        if matches!(arg, "--json" | "--help" | "-h") {
            index += 1;
            continue;
        }
        return if COMMANDS.contains(&arg) {
            Invocation::Cli
        } else if in_shell
            && !arg.starts_with('-')
            && !arg.contains(['/', '\\'])
            && !is_directory(arg)
        {
            Invocation::Unknown(arg)
        } else {
            Invocation::Gui
        };
    }
    if raw
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        Invocation::Cli
    } else {
        Invocation::Gui
    }
}

fn unknown_command(word: &str) -> CliError {
    let hint = match suggestion(word) {
        Some(command) => format!("did you mean '{command}'? See"),
        None => "see".to_owned(),
    };
    CliError::usage(format!(
        "unknown command '{word}'; {hint} compi --help for commands"
    ))
}

/// The command closest to `word`, if close enough to be a typo of it.
fn suggestion(word: &str) -> Option<&'static str> {
    let limit = word.chars().count().max(3) / 3;
    COMMANDS
        .iter()
        .map(|command| (edit_distance(word, command), *command))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, command)| command)
}

/// Levenshtein distance that also counts an adjacent transposition as one edit.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let substitution = rows[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut distance = substitution.min(rows[i - 1][j] + 1).min(rows[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                distance = distance.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = distance;
        }
    }
    rows[a.len()][b.len()]
}

/// The surface of the Compi shell running this command, if any.
fn shell_surface() -> Option<String> {
    std::env::var("COMPI_SURFACE_ID")
        .ok()
        .filter(|surface| !surface.is_empty())
}

/// The instance a Compi shell exports for commands it runs; empty names the default
/// instance. GUI launches from the shell use it too, so they open that instance's
/// windows rather than the default instance's.
pub fn inherited_instance() -> Option<String> {
    std::env::var("COMPI_INSTANCE").ok()
}

fn parse(raw: &[String]) -> std::result::Result<Options, CliError> {
    let inherited_instance = inherited_instance();
    let mut options = Options {
        instance: inherited_instance.clone(),
        surface: shell_surface(),
        ..Default::default()
    };
    let mut explicit_instance = false;
    let mut explicit_surface = false;
    let mut global_flags = BTreeSet::new();
    let mut iterator = raw.iter().peekable();
    while let Some(arg) = iterator.next() {
        if arg == "--" {
            options.separator = true;
            options.args.extend(iterator.cloned());
            break;
        }
        match arg.as_str() {
            "--json" => options.json = true,
            "--help" | "-h" => options.help = true,
            "--instance" | "--connect" | "--workspace" | "--tab" | "--pane" | "--window"
            | "--surface" | "--config" => {
                if !global_flags.insert(arg.as_str()) {
                    return Err(CliError::usage(format!("duplicate {arg}")));
                }
                let value = iterator
                    .next()
                    .ok_or_else(|| CliError::usage(format!("{arg} requires a value")))?
                    .clone();
                match arg.as_str() {
                    "--instance" => {
                        options.instance = Some(value);
                        explicit_instance = true;
                    }
                    "--connect" => options.connect = Some(value),
                    "--workspace" => options.workspace = Some(value),
                    "--tab" => options.tab = Some(value),
                    "--pane" => options.pane = Some(value),
                    "--window" => options.window = Some(value),
                    "--surface" => {
                        options.surface = Some(value);
                        explicit_surface = true;
                    }
                    "--config" => options.config = Some(PathBuf::from(value)),
                    _ => unreachable!(),
                }
            }
            "--cwd" | "--name" | "--panes" | "--layout" | "--index" | "--cols" | "--rows"
            | "--text" | "--key" | "--scope" | "--format" => {
                let value = iterator
                    .next()
                    .ok_or_else(|| CliError::usage(format!("{arg} requires a value")))?
                    .clone();
                if options.values.insert(arg.clone(), value).is_some() {
                    return Err(CliError::usage(format!("duplicate {arg}")));
                }
            }
            "--with" => {
                options.with.push(
                    iterator
                        .next()
                        .ok_or_else(|| CliError::usage("--with requires a target"))?
                        .clone(),
                );
                while iterator
                    .peek()
                    .is_some_and(|target| !target.starts_with('-'))
                {
                    options.with.push(iterator.next().unwrap().clone());
                }
            }
            "--right" | "--down" | "--check" => {
                if !options.switches.insert(arg.clone()) {
                    return Err(CliError::usage(format!("duplicate {arg}")));
                }
            }
            _ if arg.starts_with('-') => {
                return Err(CliError::usage(format!(
                    "unknown option {arg}; use --help (run argv must follow --)"
                )));
            }
            _ => options.args.push(arg.clone()),
        }
    }
    if !explicit_surface
        && (options.connect.is_some()
            || (explicit_instance && options.instance != inherited_instance))
    {
        options.surface = None;
    }
    if options.connect.is_some() && !explicit_instance {
        options.instance = None;
    }
    Ok(options)
}

enum Output {
    Help(String),
    Value(Value),
    Capture(Value),
    /// Human text, or `value` under --json.
    Report {
        text: String,
        value: Value,
    },
}

fn execute(options: &Options) -> std::result::Result<Output, CliError> {
    let command = options.args.first().map(String::as_str).unwrap_or("help");
    let action = options.args.get(1).map(String::as_str);
    if options.help || command == "help" {
        return Ok(Output::Help(help::text(
            if command == "help" {
                action.unwrap_or("")
            } else {
                command
            },
            if command == "help" {
                options.args.get(2).map(String::as_str)
            } else {
                action
            },
        )));
    }
    validate(options, command, action)?;
    if command == "theme" {
        let (mut store, diagnostics) = crate::theme_store::ThemeLibrary::load();
        let path = shell_path(&options.args[2])?;
        let installed = store.import_file(&path).map_err(CliError::usage)?;
        return Ok(Output::Value(
            json!({"installed": installed.iter().map(|theme| theme.id()).collect::<Vec<_>>(), "diagnostics":diagnostics}),
        ));
    }
    if command == "update" {
        #[cfg(any(windows, target_os = "macos"))]
        return update::run(options);
        #[cfg(not(any(windows, target_os = "macos")))]
        return Err(CliError::new(
            1,
            "updates_unavailable",
            "compi update installs into the Windows and macOS app; update this server through its package or build",
        ));
    }
    if command == "changes" {
        return Ok(changes::run());
    }
    let config = config::load(options.config.as_deref(), config::FontOverrides::default());
    if command == "layout" && action == Some("list") {
        return Ok(Output::Value(
            json!({"builtins":arrangement::Builtin::ALL.map(|preset| preset.id()),"named":config.layout_presets}),
        ));
    }
    // Validate presets/counts before connecting, and certainly before mutation.
    if command == "split" && options.values.contains_key("--panes") {
        number(options, "--panes", 1, 256)?;
        preset(&config, required(options, "--layout")?)?;
    }
    if command == "tab"
        && action == Some("merge")
        && let Some(name) = options.values.get("--layout")
    {
        preset(&config, name)?;
    }
    let mut instance = options.instance.clone();
    if instance.is_none() && options.connect.is_none() {
        let daemons = DaemonClient::local_daemons()?;
        match daemons.as_slice() {
            // A lone endpoint that cannot be inspected is still selected; the
            // connection below reports why it cannot be used.
            [daemon] => instance = Some(daemon.instance.clone().unwrap_or_default()),
            [] => {
                return Err(CliError::new(
                    5,
                    "daemon_unavailable",
                    "no existing Compi daemon; open Compi first or select --instance/--connect",
                ));
            }
            _ => {
                return Err(CliError::target(format!(
                    "multiple daemon instances exist: {}; select --instance NAME (--instance '' selects the default)",
                    daemons
                        .iter()
                        .map(|daemon| {
                            let name = daemon.instance.as_deref().unwrap_or("(default)");
                            if daemon.status.is_some() {
                                name.to_owned()
                            } else {
                                format!("{name} (not inspectable; older or incompatible)")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    }
    let target = ConnectionTarget::from_options(
        instance.filter(|value| !value.is_empty()),
        options.connect.clone(),
    )
    .map_err(|error| CliError::usage(error.to_string()))?;
    if command == "window" {
        return presentation(&target, options, crate::cli_window::Action::List);
    }
    let mut client = target.connect_existing().map_err(|error| CliError::new(5, "daemon_unavailable", format!("cannot connect to existing daemon: {error}; open Compi first or select the correct instance")))?;
    let mut state = client.workspace()?;
    if command == "tab" && action == Some("move") {
        targets::validate_context(
            &state,
            &Options {
                tab: options.tab.clone(),
                pane: options.pane.clone(),
                surface: options.surface.clone(),
                ..Default::default()
            },
        )?;
    } else {
        targets::validate_context(&state, options)?;
    }
    let operation = match command {
        "workspace" => match action.unwrap() {
            "list" => {
                return Ok(Output::Value(
                    json!({"server_id":state.server_id,"revision":state.revision,"workspaces":state.sessions}),
                ));
            }
            "show" => return Ok(Output::Value(json!(targets::workspace(&state, options)?))),
            "create" => WorkspaceMutation::CreateSession {
                label: options.args[2].clone(),
            },
            "rename" => WorkspaceMutation::RenameSession {
                session_id: targets::workspace(&state, options)?.id.clone(),
                label: options.args[2].clone(),
            },
            "close" => {
                let session = targets::workspace(&state, options)?;
                confirm_close(
                    &target,
                    &state,
                    session
                        .tabs
                        .iter()
                        .flat_map(|tab| arrangement::leaves(&tab.layout)),
                )?;
                WorkspaceMutation::RemoveSession {
                    session_id: session.id.clone(),
                }
            }
            _ => unreachable!(),
        },
        "tab" => match action.unwrap() {
            "list" => {
                let selected = if options.workspace.is_some()
                    || options.tab.is_some()
                    || options.pane.is_some()
                    || options.surface.is_some()
                {
                    Some(targets::workspace(&state, options)?.id.clone())
                } else {
                    None
                };
                return Ok(Output::Value(json!(state.sessions.iter().filter(|session| selected.as_ref().is_none_or(|id| id == &session.id)).flat_map(|session| session.tabs.iter().map(move |tab| json!({"workspace_id":session.id,"workspace":session.label,"tab":tab}))).collect::<Vec<_>>())));
            }
            "open" => WorkspaceMutation::CreateTab {
                session_id: targets::workspace(&state, options)?.id.clone(),
                label: options
                    .values
                    .get("--name")
                    .cloned()
                    .unwrap_or_else(|| "Terminal".into()),
                cols: 80,
                rows: 24,
                working_directory: options.values.get("--cwd").cloned(),
            },
            "rename" => WorkspaceMutation::RenameTab {
                tab_id: targets::tab(&state, options)?.1.id.clone(),
                label: options.args[2].clone(),
            },
            "move" => {
                let source_options = Options {
                    tab: options.tab.clone(),
                    pane: options.pane.clone(),
                    surface: options.surface.clone(),
                    ..Default::default()
                };
                let (source_session, source_tab) = targets::tab(&state, &source_options)?;
                let session_id = if let Some(destination) = options.workspace.as_deref() {
                    let destination_options = Options {
                        workspace: Some(destination.to_owned()),
                        ..Default::default()
                    };
                    targets::workspace(&state, &destination_options)?.id.clone()
                } else {
                    source_session.id.clone()
                };
                WorkspaceMutation::MoveTab {
                    session_id,
                    tab_id: source_tab.id.clone(),
                    index: number(options, "--index", 0, usize::MAX)?,
                }
            }
            "close" => {
                let tab = targets::tab(&state, options)?.1;
                confirm_close(&target, &state, arrangement::leaves(&tab.layout))?;
                WorkspaceMutation::RemoveTab {
                    tab_id: tab.id.clone(),
                }
            }
            "focus" | "detach" => {
                let tab_id = targets::tab(&state, options)?.1.id.clone();
                let action = if action == Some("focus") {
                    crate::cli_window::Action::FocusTab { tab_id }
                } else {
                    crate::cli_window::Action::DetachTab { tab_id }
                };
                return presentation(&target, options, action);
            }
            "merge" => {
                let (session, tab) = targets::tab(&state, options)?;
                let mut sources = Vec::new();
                let mut layouts = vec![&tab.layout];
                for selector in &options.with {
                    let selected = Options {
                        workspace: Some(session.id.to_string()),
                        tab: Some(selector.clone()),
                        ..Default::default()
                    };
                    let source = targets::tab(&state, &selected)?.1;
                    if source.id == tab.id || sources.contains(&source.id) {
                        return Err(CliError::usage(
                            "merge sources must be distinct from the destination and each other",
                        ));
                    }
                    sources.push(source.id.clone());
                    layouts.push(&source.layout);
                }
                let combined = arrangement::combine(&layouts).unwrap();
                let layout = if let Some(name) = options.values.get("--layout") {
                    arrangement::arrange(
                        &combined,
                        preset(&config, name)?,
                        None,
                        arrangement::Transform::default(),
                    )
                } else {
                    combined
                };
                WorkspaceMutation::MergeTabs {
                    tab_id: tab.id.clone(),
                    sources,
                    layout,
                }
            }
            "unmerge" => WorkspaceMutation::SplitMergedTabs {
                tab_id: targets::tab(&state, options)?.1.id.clone(),
            },
            _ => unreachable!(),
        },
        "pane" => {
            if matches!(action, Some("list" | "show")) {
                // Pane rows carry each terminal's current program status.
                client.watch_program_status()?;
            }
            if action == Some("list") {
                let selected_tab =
                    if options.tab.is_some() || options.pane.is_some() || options.surface.is_some()
                    {
                        Some(targets::tab(&state, options)?.1.id.clone())
                    } else {
                        None
                    };
                let selected_workspace = if options.workspace.is_some() {
                    Some(targets::workspace(&state, options)?.id.clone())
                } else {
                    None
                };
                let selected_pane = if options.pane.is_some() {
                    targets::pane(&state, options)?.2.leaf_pane().cloned()
                } else {
                    None
                };
                let mut panes = Vec::new();
                for session in &state.sessions {
                    if selected_workspace
                        .as_ref()
                        .is_some_and(|id| id != &session.id)
                    {
                        continue;
                    }
                    for tab in &session.tabs {
                        if selected_tab.as_ref().is_some_and(|id| id != &tab.id) {
                            continue;
                        }
                        for leaf in arrangement::leaves(&tab.layout) {
                            if selected_pane
                                .as_ref()
                                .is_some_and(|selected| Some(selected) != leaf.leaf_pane())
                            {
                                continue;
                            }
                            panes.push(pane_row(&mut client, &state, session, tab, &leaf)?);
                        }
                    }
                }
                return Ok(Output::Value(json!(panes)));
            }
            let (session, tab, leaf) = targets::pane(&state, options)?;
            let pane_id = leaf
                .leaf_pane()
                .expect("targets resolve only leaf nodes")
                .clone();
            match action.unwrap() {
                "show" => {
                    return Ok(Output::Value(pane_row(
                        &mut client,
                        &state,
                        session,
                        tab,
                        &leaf,
                    )?));
                }
                "close" => {
                    confirm_close(&target, &state, [leaf])?;
                    WorkspaceMutation::RemovePane { pane_id }
                }
                "terminate" => {
                    let (_, surface_id) = targets::terminal(&leaf, "pane terminate")?;
                    WorkspaceMutation::EndSurface {
                        expected_lifetime: pane_surface(&state, &surface_id)?
                            .process_lifetime_id
                            .clone(),
                        surface_id,
                    }
                }
                "swap" => {
                    let other_options = Options {
                        pane: Some(options.with[0].clone()),
                        tab: Some(tab.id.to_string()),
                        ..Default::default()
                    };
                    let (_, _, other) = targets::pane(&state, &other_options)?;
                    let other = other.leaf_pane().expect("targets resolve only leaf nodes");
                    WorkspaceMutation::ArrangeTab {
                        tab_id: tab.id.clone(),
                        layout: arrangement::swap(&tab.layout, &pane_id, other).ok_or_else(
                            || CliError::usage("swap needs two distinct panes in the same tab"),
                        )?,
                    }
                }
                "capture" => {
                    let (_, surface_id) = targets::terminal(&leaf, "pane capture")?;
                    let surface = pane_surface(&state, &surface_id)?;
                    let scope = options
                        .values
                        .get("--scope")
                        .map(String::as_str)
                        .unwrap_or("screen");
                    let snapshot = client.inspect_surface(surface, scope == "scrollback")?;
                    let format = options
                        .values
                        .get("--format")
                        .map(String::as_str)
                        .unwrap_or("text");
                    let text = capture::render(&snapshot, scope == "scrollback", format == "ansi");
                    return Ok(Output::Capture(
                        json!({"pane_id":pane_id,"surface_id":surface_id,"sequence":snapshot.sequence,"cols":snapshot.cols,"rows":snapshot.rows,"scope":scope,"format":format,"text":text}),
                    ));
                }
                "send" => {
                    let (_, surface_id) = targets::terminal(&leaf, "pane send")?;
                    let surface = pane_surface(&state, &surface_id)?;
                    let snapshot = client.inspect_surface(surface, false)?;
                    let data = if let Some(text) = options.values.get("--text") {
                        literal_text(text, snapshot.modes.bracketed_paste)?
                    } else {
                        encode_key(required(options, "--key")?, &snapshot)?
                    };
                    let bytes = data.len();
                    client.send_surface_input(surface, data)?;
                    return Ok(Output::Report {
                        text: String::new(),
                        value: json!({"pane_id":pane_id,"surface_id":surface_id,"bytes_sent":bytes}),
                    });
                }
                operation => {
                    let action = match operation {
                        "focus" => crate::cli_window::Action::FocusPane { pane_id },
                        "resize" => {
                            // Content panes are not sized in cells; resize a terminal beside it.
                            let (_, surface_id) = targets::terminal(&leaf, "pane resize")?;
                            let cols = optional_number(options, "--cols", 20, i16::MAX as usize)?
                                .map(|cols| cols as i16);
                            let rows = optional_number(options, "--rows", 4, i16::MAX as usize)?
                                .map(|rows| rows as i16);
                            let (cols, rows) = match (cols, rows) {
                                (Some(cols), Some(rows)) => (cols, rows),
                                // An omitted dimension keeps the terminal's current size.
                                _ => {
                                    let surface = pane_surface(&state, &surface_id)?;
                                    (cols.unwrap_or(surface.cols), rows.unwrap_or(surface.rows))
                                }
                            };
                            crate::cli_window::Action::ResizePane {
                                pane_id,
                                cols,
                                rows,
                            }
                        }
                        "zoom" => crate::cli_window::Action::ZoomPane { pane_id },
                        "unzoom" => crate::cli_window::Action::UnzoomPane { pane_id },
                        "float" => crate::cli_window::Action::FloatPane { pane_id },
                        "dock" => crate::cli_window::Action::DockPane { pane_id },
                        _ => unreachable!(),
                    };
                    return presentation(&target, options, action);
                }
            }
        }
        "layout" => {
            let tab = targets::tab(&state, options)?.1;
            if action == Some("restore") && tab.previous_layout.is_none() && tab.merge.is_some() {
                WorkspaceMutation::SplitMergedTabs {
                    tab_id: tab.id.clone(),
                }
            } else {
                let layout = match action.unwrap() {
                    "mirror" => arrangement::apply_transform(
                        tab.layout.clone(),
                        arrangement::Transform {
                            mirror: true,
                            flip: false,
                        },
                    ),
                    "flip" => arrangement::apply_transform(
                        tab.layout.clone(),
                        arrangement::Transform {
                            mirror: false,
                            flip: true,
                        },
                    ),
                    "restore" => arrangement::restore(
                        tab.previous_layout.as_deref().ok_or_else(|| {
                            CliError::usage(
                                "tab has no previous arrangement or merged tabs to restore",
                            )
                        })?,
                        &tab.layout,
                    )
                    .ok_or_else(|| CliError::usage("previous arrangement cannot be restored"))?,
                    name => arrangement::arrange(
                        &tab.layout,
                        preset(&config, name)?,
                        None,
                        arrangement::Transform::default(),
                    ),
                };
                WorkspaceMutation::ArrangeTab {
                    tab_id: tab.id.clone(),
                    layout,
                }
            }
        }
        "split" => return split(&mut client, &mut state, options, &config),
        "files" | "edit" => return content::run(&mut client, &state, &target, options),
        "diff" => return diff::run(&mut client, &state, &target, options),
        "settings" | "monitor" => return view::run(&mut client, &state, &target, options),
        "attach" | "detach" | "run" => {
            let (_, _, pane_id, surface_id) = targets::terminal_pane(&state, options, command)?;
            let surface = pane_surface(&state, &surface_id)?.clone();
            if command == "detach" {
                client.detach_surface(&surface)?;
                return Ok(Output::Report {
                    text: "Detached.\n".into(),
                    value: json!({"pane_id":pane_id,"surface_id":surface_id,"detached":true}),
                });
            }
            if command == "attach" {
                if surface.attached {
                    return Err(CliError::new(
                        5,
                        "already_attached",
                        "pane already has a GUI or console attachment; select an unattached pane or explicitly detach its console first; no attachment is stolen",
                    ));
                }
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() || options.json {
                    return Err(CliError::usage(
                        "attach requires interactive stdin/stdout and cannot use --json; use pane capture for observation",
                    ));
                }
                #[cfg(windows)]
                crate::console::attach(client, surface)?;
                #[cfg(unix)]
                crate::probe::console::attach(client, surface)?;
                return Ok(Output::Report {
                    text: "Detached.\n".into(),
                    value: json!({"pane_id":pane_id,"surface_id":surface_id,"detached":true}),
                });
            }
            let metadata = client.surface_metadata(&surface)?;
            let executable=metadata.shell_executable.as_deref().ok_or_else(||CliError::usage("runtime shell identity is unavailable; use pane send with explicit text/key instead"))?;
            let shell = executable
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(executable)
                .trim_start_matches('-');
            if !matches!(
                shell,
                "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish" | "ash"
            ) {
                return Err(CliError::usage(format!(
                    "run does not support quoting for shell {shell}; use pane send with explicit text/key"
                )));
            }
            let snapshot = client.inspect_surface(&surface, false)?;
            if snapshot.modes.alternate_screen {
                return Err(CliError::usage(
                    "run refuses an alternate-screen application; return to the shell first, or explicitly use pane send",
                ));
            }
            if options.args[1..]
                .iter()
                .any(|arg| arg.chars().any(char::is_control))
            {
                return Err(CliError::usage(
                    "run argv cannot contain control characters that a terminal could interpret as keys; use explicit pane send if that input is intended",
                ));
            }
            let line = capture::quote_argv(&options.args[1..]);
            let mut data = literal_text(&line, snapshot.modes.bracketed_paste)?;
            data.push(b'\r');
            client.send_surface_input(&surface, data)?;
            return Ok(Output::Report {
                text: String::new(),
                value: json!({"pane_id":pane_id,"surface_id":surface_id,"command":line,"submitted":true}),
            });
        }
        _ => unreachable!(),
    };
    let launch = if matches!(operation, WorkspaceMutation::CreateTab { .. }) {
        Some(config.launch.clone().map_err(CliError::usage)?)
    } else {
        None
    };
    let receipt = submit(&mut client, &state, operation, launch)?;
    let after = client.workspace()?;
    Ok(changed(
        options,
        &state,
        &after,
        json!({"receipt":receipt,"workspace":after}),
    ))
}

/// What a change did, in one line; `--json` still returns every receipt and the
/// resulting workspace for scripts and agents.
fn changed(
    options: &Options,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    value: Value,
) -> Output {
    fn leaves<'a>(node: &'a LayoutNode, output: &mut Vec<(&'a PaneId, &'a SurfaceId)>) {
        match node {
            LayoutNode::Pane {
                pane_id,
                surface_id,
                ..
            } => output.push((pane_id, surface_id)),
            LayoutNode::Content { .. } => {}
            LayoutNode::Split { first, second, .. } => {
                leaves(first, output);
                leaves(second, output);
            }
        }
    }
    let mut panes = Vec::new();
    for tab in after.sessions.iter().flat_map(|session| &session.tabs) {
        leaves(&tab.layout, &mut panes);
    }
    // A pane is new when its shell did not exist before this change.
    let created: Vec<&str> = panes
        .into_iter()
        .filter(|(_, surface)| before.surface(surface).is_none())
        .map(|(pane, _)| pane.as_str())
        .collect();
    let command = options.args.first().map(String::as_str).unwrap_or_default();
    let action = options.args.get(1).map(String::as_str).unwrap_or_default();
    let text = match (command, action) {
        ("split", _) if created.is_empty() => "Arranged.".to_owned(),
        ("split", _) => match created.as_slice() {
            [pane] => format!("New pane {pane}"),
            panes => format!("{} new panes: {}", panes.len(), panes.join(", ")),
        },
        ("tab", "open") => match created.first() {
            Some(pane) => format!("New tab · pane {pane}"),
            None => "New tab.".to_owned(),
        },
        ("workspace", "create") => format!("Created workspace {}", options.args[2]),
        ("layout", _) => "Arranged.".to_owned(),
        _ => "Done.".to_owned(),
    };
    Output::Report {
        text: format!("{text}\n"),
        value,
    }
}

/// The surface behind a terminal pane, which the workspace lists beside its layout.
fn pane_surface<'a>(
    state: &'a WorkspaceSnapshot,
    surface_id: &SurfaceId,
) -> std::result::Result<&'a SurfaceInfo, CliError> {
    state
        .surface(surface_id)
        .ok_or_else(|| CliError::target("pane surface is missing"))
}

/// One `pane list`/`pane show` entry. Terminals report their surface, fresh runtime
/// metadata, program status and any view they carry; content panes have no process and
/// report what they show: files, Settings or Monitor.
fn pane_row(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    session: &WorkspaceSession,
    tab: &WorkspaceTab,
    leaf: &LayoutNode,
) -> std::result::Result<Value, CliError> {
    use crate::cli_window::PaneType;
    Ok(match leaf {
        LayoutNode::Pane {
            pane_id,
            surface_id,
            view,
        } => {
            let surface = pane_surface(state, surface_id)?;
            let program_status = client
                .program_status()
                .get(surface_id)
                .filter(|status| status.process_lifetime_id == surface.process_lifetime_id)
                .map_or(&[][..], |status| &status.records)
                .to_vec();
            let mut row = json!({"workspace_id":session.id,"tab_id":tab.id,"pane_id":pane_id,"type":PaneType::Terminal,"surface":surface,"metadata":client.surface_metadata(surface)?,"program_status":program_status});
            if let Some(view) = view {
                row["view"] = json!(view);
            }
            row
        }
        LayoutNode::Content { pane_id, content } => content_row(session, tab, pane_id, content),
        LayoutNode::Split { .. } => unreachable!("pane rows are built only for leaf nodes"),
    })
}

/// A content pane's `pane list`/`pane show` entry.
fn content_row(
    session: &WorkspaceSession,
    tab: &WorkspaceTab,
    pane_id: &PaneId,
    content: &compi_protocol::PaneContent,
) -> Value {
    let pane_type = crate::cli_window::PaneType::of_content(content);
    json!({"workspace_id":session.id,"tab_id":tab.id,"pane_id":pane_id,"type":pane_type,"content":content})
}

fn presentation(
    target: &ConnectionTarget,
    options: &Options,
    action: crate::cli_window::Action,
) -> std::result::Result<Output, CliError> {
    Ok(Output::Value(
        serde_json::to_value(crate::cli_window::execute(
            target,
            options.window.as_deref(),
            action,
        )?)
        .map_err(|e| CliError::new(1, "serialization_failed", e.to_string()))?,
    ))
}

/// Submit one change. A stale revision is retried when tabs and layouts are exactly as
/// this command read them, because the newer revision then only records shell status or
/// size updates (for example a window resizing panes a previous command created). Any
/// structural change still fails, so a concurrent edit is never overwritten.
fn submit(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    operation: WorkspaceMutation,
    launch: Option<compi_protocol::LaunchContext>,
) -> Result<compi_protocol::MutationReceipt> {
    let launch = launch.map(Box::new);
    let mut revision = state.revision;
    let mut attempts = 0;
    loop {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let result = client.submit_mutation(MutationRequest {
            server_id: state.server_id.clone(),
            expected_generation: state.server_generation.clone(),
            expected_revision: revision,
            mutation_id: MutationId::new(format!(
                "cli-{}-{stamp}-{}",
                std::process::id(),
                NEXT_MUTATION.fetch_add(1, Ordering::Relaxed)
            )),
            operation: operation.clone(),
            launch: launch.clone(),
        });
        let conflict = result.as_ref().err().is_some_and(|error| {
            error
                .downcast_ref::<compi_protocol::DaemonError>()
                .is_some_and(|daemon| daemon.code == compi_protocol::ErrorCode::RevisionConflict)
        });
        attempts += 1;
        if !conflict || attempts >= 5 {
            return result;
        }
        let current = client.workspace()?;
        if current.server_generation != state.server_generation
            || current.sessions != state.sessions
        {
            return result;
        }
        revision = current.revision;
    }
}

/// Whether a person can answer a prompt on stdin. From a WSL terminal, Windows gives the
/// GUI-subsystem CLI no console, so the Compi shell bridge (`_compi_windows_cli` in
/// assets/compi-shell.sh) runs it on pipes and sets the internal COMPI_CLI_TERMINAL=1.
/// That is honoured only together with the WSL context the bridge always passes; it is
/// not a confirmation bypass and is not documented for users.
fn interactive() -> bool {
    io::stdin().is_terminal() || bridged_terminal()
}

fn bridged_terminal() -> bool {
    cfg!(windows)
        && std::env::var_os("COMPI_CLI_TERMINAL").is_some_and(|value| value == "1")
        && std::env::var_os("WSL_DISTRO_NAME").is_some_and(|name| !name.is_empty())
        && std::env::var_os("COMPI_SHELL_CWD").is_some()
}

/// Ctrl-C in the WSL shell bridge cannot reach a Windows program without a console;
/// the bridge ends the relay instead, which breaks this process's stdin pipe. Watch for
/// that without reading, since a pending read on the bridge's pipe holds back output,
/// and end the way Ctrl-C ends a console program.
fn watch_bridged_input() {
    #[cfg(windows)]
    if bridged_terminal() {
        use windows::Win32::System::{
            Console::{GetStdHandle, STD_INPUT_HANDLE},
            Pipes::PeekNamedPipe,
        };
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                // SAFETY: the standard input handle stays open for the process lifetime.
                let open = unsafe {
                    GetStdHandle(STD_INPUT_HANDLE)
                        .and_then(|handle| PeekNamedPipe(handle, None, 0, None, None, None))
                };
                if open.is_err() {
                    std::process::exit(130);
                }
            }
        });
    }
}

/// One answer typed at a prompt.
fn read_answer() -> std::result::Result<String, CliError> {
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliError::new(1, "io_error", error.to_string()))?;
    Ok(answer)
}

/// A file with unsaved edits in a content pane that a close would remove.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct UnsavedFile {
    pane_id: PaneId,
    path: String,
}

/// The files open windows report unsaved in the panes among `closing` (content panes and
/// terminals' Files views), each once even when several windows show the pane.
fn unsaved_files(
    closing: &[PaneId],
    windows: &[crate::cli_window::WindowInfo],
) -> Vec<UnsavedFile> {
    windows
        .iter()
        .flat_map(|window| &window.panes)
        .filter(|pane| closing.contains(&pane.pane_id))
        .flat_map(|pane| {
            pane.unsaved_files.iter().map(|path| UnsavedFile {
                pane_id: pane.pane_id.clone(),
                path: path.clone(),
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Ask once before a close that would end running processes or discard edits that open
/// windows hold unsaved in its content panes or terminals' Files views. With no GUI host
/// running nothing is unsaved. There is no bypass: scripts terminate panes and save
/// files first.
fn confirm_close(
    target: &ConnectionTarget,
    state: &WorkspaceSnapshot,
    leaves: impl IntoIterator<Item = LayoutNode>,
) -> std::result::Result<(), CliError> {
    let mut running = Vec::new();
    let mut closing = Vec::new();
    for leaf in leaves {
        match leaf {
            LayoutNode::Pane {
                pane_id,
                surface_id,
                view,
            } => {
                if state.surface(&surface_id).is_some_and(|surface| {
                    matches!(
                        surface.status,
                        SurfaceStatus::Starting | SurfaceStatus::Running
                    )
                }) {
                    running.push(surface_id);
                }
                if view.is_some_and(|view| view.content.folder().is_some()) {
                    closing.push(pane_id);
                }
            }
            LayoutNode::Content { pane_id, .. } => closing.push(pane_id),
            LayoutNode::Split { .. } => {}
        }
    }
    let unsaved = if closing.is_empty() {
        Vec::new()
    } else {
        let windows = crate::cli_window::windows(target).map_err(|error| {
            CliError::new(
                1,
                "operation_failed",
                format!("cannot check open windows for unsaved edits: {error}; nothing was closed"),
            )
        })?;
        unsaved_files(&closing, &windows.unwrap_or_default())
    };
    if running.is_empty() && unsaved.is_empty() {
        return Ok(());
    }
    let refused = |exit, code, message: String| CliError {
        unsaved_files: unsaved.clone(),
        ..CliError::new(exit, code, message)
    };
    let processes = format!(
        "end {} running process(es) [{}]",
        running.len(),
        running
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !interactive() {
        let mut effects = Vec::new();
        if !running.is_empty() {
            effects.push(processes);
        }
        if !unsaved.is_empty() {
            effects.push(format!(
                "discard unsaved changes to {}",
                unsaved
                    .iter()
                    .map(|file| format!("{} (pane {})", file.path, file.pane_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Err(refused(
            4,
            "confirmation_required",
            format!(
                "close would {}; interactive confirmation is required. Scripts must explicitly use pane terminate and save files in their window first; no --yes/--force bypass",
                effects.join(" and ")
            ),
        ));
    }
    if !unsaved.is_empty() {
        eprintln!("Unsaved changes:");
        for file in &unsaved {
            eprintln!("  pane {}  {}", file.pane_id, file.path);
        }
    }
    let prompt = match (running.is_empty(), unsaved.is_empty()) {
        (true, _) => "Close and discard unsaved changes? [y/N] ".to_owned(),
        (false, true) => format!("Close will {processes}. Type close to confirm: "),
        (false, false) => {
            format!("Close will {processes} and discard unsaved changes. Type close to confirm: ")
        }
    };
    eprint!("{prompt}");
    io::stderr()
        .flush()
        .map_err(|e| CliError::new(1, "io_error", e.to_string()))?;
    let answer = read_answer()?;
    let answer = answer.trim();
    let confirmed = if running.is_empty() {
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    } else {
        answer == "close"
    };
    if !confirmed {
        return Err(refused(
            4,
            "cancelled",
            "close cancelled; nothing was closed".to_owned(),
        ));
    }
    Ok(())
}

fn preset<'a>(
    config: &'a config::LoadedConfig,
    name: &str,
) -> std::result::Result<arrangement::Preset<'a>, CliError> {
    if let Some(builtin) = arrangement::Builtin::parse(name) {
        return Ok(arrangement::Preset::Builtin(builtin));
    }
    let shape = config
        .layout_presets
        .get(name)
        .ok_or_else(|| CliError::usage(format!("unknown layout preset {name}; use layout list")))?;
    shape.validate().map_err(CliError::usage)?;
    Ok(arrangement::Preset::Named(shape))
}

fn required<'a>(options: &'a Options, name: &str) -> std::result::Result<&'a str, CliError> {
    options
        .values
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| CliError::usage(format!("{name} is required")))
}
fn number(
    options: &Options,
    name: &str,
    min: usize,
    max: usize,
) -> std::result::Result<usize, CliError> {
    let value = required(options, name)?.parse::<usize>().map_err(|_| {
        CliError::usage(format!(
            "{name} requires an integer between {min} and {max}"
        ))
    })?;
    if !(min..=max).contains(&value) {
        return Err(CliError::usage(format!(
            "{name} requires an integer between {min} and {max}"
        )));
    }
    Ok(value)
}
fn optional_number(
    options: &Options,
    name: &str,
    min: usize,
    max: usize,
) -> std::result::Result<Option<usize>, CliError> {
    if options.values.contains_key(name) {
        number(options, name, min, max).map(Some)
    } else {
        Ok(None)
    }
}

fn literal_text(text: &str, bracketed: bool) -> std::result::Result<Vec<u8>, CliError> {
    if bracketed && text.contains("\x1b[201~") {
        return Err(CliError::usage(
            "text contains the bracketed-paste terminator; refusing to reinterpret literal text as keys",
        ));
    }
    let mut data = Vec::with_capacity(text.len() + if bracketed { 12 } else { 0 });
    if bracketed {
        data.extend_from_slice(b"\x1b[200~");
    }
    data.extend_from_slice(text.as_bytes());
    if bracketed {
        data.extend_from_slice(b"\x1b[201~");
    }
    Ok(data)
}

fn encode_key(value: &str, snapshot: &ScreenSnapshot) -> std::result::Result<Vec<u8>, CliError> {
    let mut modifiers = crate::input::Modifiers::default();
    let lower = value.to_ascii_lowercase();
    let mut key = lower.as_str();
    loop {
        if let Some(rest) = key.strip_prefix("ctrl-") {
            modifiers.control = true;
            key = rest;
        } else if let Some(rest) = key.strip_prefix("alt-") {
            modifiers.alt = true;
            key = rest;
        } else if let Some(rest) = key.strip_prefix("shift-") {
            modifiers.shift = true;
            key = rest;
        } else {
            break;
        }
    }
    let known = matches!(
        key,
        "enter"
            | "tab"
            | "escape"
            | "space"
            | "backspace"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "insert"
            | "delete"
            | "pageup"
            | "pagedown"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
    );
    if !known && !(modifiers.control && key.chars().count() == 1) {
        return Err(CliError::usage(format!(
            "unsupported key {value}; use pane send --help"
        )));
    }
    crate::input::encode_keystroke(
        &crate::input::Key {
            modifiers,
            key,
            key_char: None,
        },
        snapshot.modes.application_cursor,
        None,
        snapshot.modes.keyboard_protocol_flags,
    )
    .ok_or_else(|| CliError::usage(format!("unsupported key {value}")))
}

fn validate(
    options: &Options,
    command: &str,
    action: Option<&str>,
) -> std::result::Result<(), CliError> {
    let (min, max, values, switches): (usize, usize, &[&str], &[&str]) = match (command, action) {
        ("workspace", Some("list" | "show" | "close")) => (2, 2, &[], &[]),
        ("workspace", Some("create" | "rename")) => (3, 3, &[], &[]),
        ("tab", Some("list" | "focus" | "detach" | "close" | "unmerge")) => (2, 2, &[], &[]),
        ("tab", Some("open")) => (2, 2, &["--cwd", "--name"], &[]),
        ("tab", Some("rename")) => (3, 3, &[], &[]),
        ("tab", Some("move")) => (2, 2, &["--index"], &[]),
        ("tab", Some("merge")) => (2, 2, &["--layout"], &[]),
        (
            "pane",
            Some(
                "list" | "show" | "focus" | "zoom" | "unzoom" | "float" | "dock" | "close"
                | "terminate",
            ),
        ) => (2, 2, &[], &[]),
        ("pane", Some("swap")) => (2, 2, &[], &[]),
        ("pane", Some("resize")) => (2, 2, &["--cols", "--rows"], &[]),
        ("pane", Some("send")) => (2, 2, &["--text", "--key"], &[]),
        ("pane", Some("capture")) => (2, 2, &["--scope", "--format"], &[]),
        ("split", _) => (
            1,
            1,
            &["--cwd", "--panes", "--layout"],
            &["--right", "--down"],
        ),
        ("files" | "edit" | "diff" | "settings" | "monitor", _) => (1, 2, &[], &[]),
        ("layout", Some(_)) => (2, 2, &[], &[]),
        ("run", _) => (2, usize::MAX, &[], &[]),
        ("attach" | "detach", _) => (1, 1, &[], &[]),
        ("window", Some("list")) => (2, 2, &[], &[]),
        ("theme", Some("install")) => (3, 3, &[], &[]),
        ("update", None) => (1, 1, &[], &["--check"]),
        ("changes", None) => (1, 1, &[], &[]),
        _ => {
            return Err(CliError::usage(format!(
                "unknown or missing {command} action; run compi {command} --help"
            )));
        }
    };
    if !(min..=max).contains(&options.args.len()) {
        return Err(CliError::usage(format!(
            "invalid arguments; run compi {command} {} --help",
            action.unwrap_or("")
        )));
    }
    for option in options.values.keys() {
        if !values.contains(&option.as_str()) {
            return Err(CliError::usage(format!(
                "{option} is not valid for this command"
            )));
        }
    }
    for option in &options.switches {
        if !switches.contains(&option.as_str()) {
            return Err(CliError::usage(format!(
                "{option} is not valid for this command"
            )));
        }
    }
    if command == "run" && !options.separator {
        return Err(CliError::usage(
            "run requires -- before COMMAND and its arguments",
        ));
    }
    let with_expected =
        command == "tab" && action == Some("merge") || command == "pane" && action == Some("swap");
    if with_expected && options.with.is_empty() {
        return Err(CliError::usage("--with is required"));
    }
    if !with_expected && !options.with.is_empty() {
        return Err(CliError::usage("--with is not valid for this command"));
    }
    if command == "pane" && action == Some("swap") && options.with.len() != 1 {
        return Err(CliError::usage(
            "pane swap requires exactly one --with target",
        ));
    }
    if command == "pane" && action == Some("capture") {
        if options
            .values
            .get("--scope")
            .is_some_and(|scope| !matches!(scope.as_str(), "screen" | "scrollback"))
        {
            return Err(CliError::usage("--scope must be screen or scrollback"));
        }
        if options
            .values
            .get("--format")
            .is_some_and(|format| !matches!(format.as_str(), "text" | "ansi"))
        {
            return Err(CliError::usage("--format must be text or ansi"));
        }
    }
    if command == "pane"
        && action == Some("send")
        && options.values.contains_key("--text") == options.values.contains_key("--key")
    {
        return Err(CliError::usage(
            "send requires exactly one of --text or --key",
        ));
    }
    if command == "theme" && options.connect.is_some() {
        return Err(CliError::usage(
            "theme install operates on the local app library; --connect is not supported",
        ));
    }
    if command == "update" && options.connect.is_some() {
        return Err(CliError::usage(
            "update installs into this computer's Compi; --connect is not supported",
        ));
    }
    if command == "changes" && options.connect.is_some() {
        return Err(CliError::usage(
            "changes reports this computer's Compi; --connect is not supported",
        ));
    }
    if command == "tab" && action == Some("move") {
        number(options, "--index", 0, usize::MAX)?;
    }
    if command == "pane" && action == Some("resize") {
        if !options.values.contains_key("--cols") && !options.values.contains_key("--rows") {
            return Err(CliError::usage(
                "resize requires at least one of --cols or --rows",
            ));
        }
        optional_number(options, "--cols", 20, i16::MAX as usize)?;
        optional_number(options, "--rows", 4, i16::MAX as usize)?;
    }
    if command == "split" {
        if options.switches.contains("--right") && options.switches.contains("--down") {
            return Err(CliError::usage("choose --right or --down, not both"));
        }
        if options.values.contains_key("--panes") != options.values.contains_key("--layout") {
            return Err(CliError::usage(
                "split count requires both --panes TOTAL and --layout PRESET",
            ));
        }
        if options.values.contains_key("--panes") && (!options.switches.is_empty()) {
            return Err(CliError::usage(
                "--right/--down cannot be combined with count/layout splitting",
            ));
        }
    }
    Ok(())
}

/// Resolve a path argument as the invoking shell names it; relative WSL paths are
/// relative to that shell's directory, not to this Windows process's.
fn shell_path(path: &str) -> std::result::Result<PathBuf, CliError> {
    #[cfg(windows)]
    if let Ok(distribution) = std::env::var("WSL_DISTRO_NAME") {
        let native = PathBuf::from(path);
        // Drive-qualified and UNC paths are already Windows-native. Relative
        // Linux files are resolved against the invoking shell, not WSL's home.
        if !native.is_absolute() || path.starts_with('/') {
            let absolute = if path.starts_with('/') {
                path.to_owned()
            } else {
                let cwd=std::env::var("COMPI_SHELL_CWD").map_err(|_|CliError::usage("relative WSL path needs shell cwd; invoke through the compi shell bridge or use an absolute Unix path"))?;
                format!("{}/{path}", cwd.trim_end_matches('/'))
            };
            return compi_protocol::wsl::windows_path_for_wsl(&absolute, &distribution)
                .map_err(CliError::from);
        }
    }
    Ok(PathBuf::from(path))
}

fn split(
    client: &mut DaemonClient,
    state: &mut WorkspaceSnapshot,
    options: &Options,
    config: &config::LoadedConfig,
) -> std::result::Result<Output, CliError> {
    let before = state.clone();
    let mut receipts = Vec::new();
    if let Some(total) = optional_number(options, "--panes", 1, compi_protocol::MAX_TAB_PANES)? {
        let tab = targets::tab(state, options)?.1;
        let current = arrangement::leaves(&tab.layout).len();
        missing_count(current, total)?;
        let selected_preset = preset(config, required(options, "--layout")?)?;
        let launch = if total > current {
            Some(config.launch.clone().map_err(CliError::usage)?)
        } else {
            None
        };
        let layout = arrangement::plan_growth(&tab.layout, selected_preset, total);
        receipts.push(submit(
            client,
            state,
            WorkspaceMutation::GrowTab {
                tab_id: tab.id.clone(),
                layout,
                working_directory: options.values.get("--cwd").cloned(),
            },
            launch,
        )?);
    } else {
        let (_, _, pane_id, surface_id) = targets::terminal_pane(state, options, "split")?;
        let surface = pane_surface(state, &surface_id)?;
        let axis = if options.switches.contains("--down") {
            SplitAxis::Vertical
        } else {
            SplitAxis::Horizontal
        };
        receipts.push(submit(
            client,
            state,
            split_operation(pane_id, surface, axis, options)?,
            Some(config.launch.clone().map_err(CliError::usage)?),
        )?);
    }
    *state = client.workspace()?;
    Ok(changed(
        options,
        &before,
        state,
        json!({"receipts":receipts,"workspace":state}),
    ))
}

fn missing_count(current: usize, total: usize) -> std::result::Result<usize, CliError> {
    total.checked_sub(current).ok_or_else(||CliError::usage(format!("requested total {total} is below existing pane count {current}; split never deletes panes")))
}
fn split_operation(
    pane_id: compi_protocol::PaneId,
    surface: &SurfaceInfo,
    axis: SplitAxis,
    options: &Options,
) -> std::result::Result<WorkspaceMutation, CliError> {
    let (cols, rows) = match axis {
        SplitAxis::Horizontal => ((surface.cols - 1) / 2, surface.rows),
        SplitAxis::Vertical => (surface.cols, (surface.rows - 1) / 2),
    };
    if cols < 20 || rows < 4 {
        return Err(CliError::usage(
            "pane is too small to split (minimum children: 20 columns by 4 rows); enlarge the pane first",
        ));
    }
    Ok(WorkspaceMutation::SplitPane {
        pane_id,
        axis,
        cols,
        rows,
        working_directory: options.values.get("--cwd").cloned(),
        geometry: compi_protocol::SplitGeometry {
            width: f32::from(surface.cols),
            height: f32::from(surface.rows),
            min_width: 20.0,
            min_height: 4.0,
            divider: 1.0,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_total_never_removes_existing_panes() {
        assert_eq!(missing_count(4, 3).unwrap_err().exit, 2);
        assert_eq!(missing_count(4, 4).unwrap(), 0);
        assert_eq!(missing_count(4, 6).unwrap(), 2);
    }
    #[test]
    fn literal_text_does_not_add_enter_or_rewrite_newlines() {
        assert_eq!(literal_text("a\nb\r\n", false).unwrap(), b"a\nb\r\n");
        assert_eq!(
            literal_text("a\nb", true).unwrap(),
            b"\x1b[200~a\nb\x1b[201~"
        );
        assert!(literal_text("\x1b[201~echo bad", true).is_err());
    }
    #[test]
    fn literal_json_text_is_not_an_output_mode_flag() {
        let args = ["pane", "send", "--text", "--json"].map(str::to_owned);
        assert!(!wants_json(&args));
        let options = parse(&args).unwrap();
        assert_eq!(options.values["--text"], "--json");
        assert!(!options.json);
    }
    #[test]
    fn conflicting_explicit_target_flags_are_rejected() {
        let args = ["--pane", "first", "--pane", "second", "pane", "show"].map(str::to_owned);
        assert_eq!(parse(&args).unwrap_err().exit, 2);
    }
    #[test]
    fn unknown_words_are_commands_only_inside_a_compi_shell() {
        let check = |args: &[&str], in_shell: bool, expected: Invocation| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            let invocation = classify(&args, in_shell, |word| word == "project");
            assert_eq!(invocation, expected, "{args:?} in_shell={in_shell}");
        };
        for in_shell in [true, false] {
            check(&["files"], in_shell, Invocation::Cli);
            check(&["--instance", "dev", "edit"], in_shell, Invocation::Cli);
            check(&["--help"], in_shell, Invocation::Cli);
            check(&[], in_shell, Invocation::Gui);
            check(&["project"], in_shell, Invocation::Gui);
            check(&["../missing"], in_shell, Invocation::Gui);
            check(&["--theme", "dracula", "fiiles"], in_shell, Invocation::Gui);
        }
        check(&["fiiles"], false, Invocation::Gui);
        check(&["fiiles"], true, Invocation::Unknown("fiiles"));
        check(
            &["--json", "--instance", "dev", "edti"],
            true,
            Invocation::Unknown("edti"),
        );
    }
    #[test]
    fn unknown_commands_suggest_only_close_commands() {
        assert_eq!(suggestion("fiiles"), Some("files"));
        assert_eq!(suggestion("edti"), Some("edit"));
        assert_eq!(suggestion("dif"), Some("diff"));
        assert_eq!(suggestion("workspce"), Some("workspace"));
        assert_eq!(suggestion("xyz"), None);
        assert_eq!(suggestion("elephant"), None);
        let error = unknown_command("fiiles");
        assert_eq!(error.exit, 2);
        assert_eq!(
            error.message,
            "unknown command 'fiiles'; did you mean 'files'? See compi --help for commands"
        );
        assert_eq!(
            unknown_command("xyz").message,
            "unknown command 'xyz'; see compi --help for commands"
        );
    }
    #[test]
    fn update_accepts_only_check_and_json() {
        let accepted = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            assert_eq!(classify(&args, true, |_| false), Invocation::Cli);
            parse(&args).and_then(|options| {
                let command = options.args[0].clone();
                validate(&options, &command, options.args.get(1).map(String::as_str))?;
                Ok(options)
            })
        };
        let options = accepted(&["update", "--check", "--json"]).unwrap();
        assert!(options.json && options.switches.contains("--check"));
        assert!(accepted(&["--json", "update"]).is_ok());
        for rejected in [
            &["update", "now"][..],
            &["update", "--right"],
            &["update", "--check", "--check"],
            &["--connect", "host", "update"],
            &["tab", "list", "--check"],
        ] {
            let error = accepted(rejected).unwrap_err();
            assert_eq!(error.exit, 2, "{rejected:?}");
        }
    }
    #[test]
    fn view_commands_take_at_most_one_argument() {
        let validated = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            assert_eq!(classify(&args, true, |_| false), Invocation::Cli);
            parse(&args).and_then(|options| {
                let command = options.args[0].clone();
                validate(&options, &command, options.args.get(1).map(String::as_str))
            })
        };
        for accepted in [
            &["files"][..],
            &["files", "../docs"],
            &["edit"],
            &["--json", "edit", "src/main.rs:12:5"],
            &["--pane", "pane-1", "edit", "--", "-notes.md"],
            &["diff"],
            &["--json", "diff", "src/layout.rs:40"],
            &["settings"],
            &["settings", "terminal"],
            &["--json", "monitor", "processes"],
        ] {
            assert!(validated(accepted).is_ok(), "{accepted:?}");
        }
        for rejected in [
            &["edit", "a.rs", "b.rs"][..],
            &["files", "--right"],
            &["edit", "a.rs", "--cwd", "/tmp"],
            &["diff", "a.rs", "b.rs"],
            &["diff", "--staged"],
            &["settings", "terminal", "fonts"],
            &["monitor", "--page", "cpu"],
        ] {
            assert_eq!(validated(rejected).unwrap_err().exit, 2, "{rejected:?}");
        }
    }
    #[test]
    fn close_reports_unsaved_files_of_closing_files_from_every_window() {
        use crate::cli_window::{PaneInfo, PaneType, TargetIdentity, WindowInfo};
        let pane = |id: &str, pane_type, unsaved: &[&str]| PaneInfo {
            pane_id: PaneId::new(id),
            pane_type,
            surface_id: None,
            cols: None,
            rows: None,
            focused: false,
            floating: false,
            zoomed: false,
            unsaved_files: unsaved.iter().map(|path| (*path).to_owned()).collect(),
        };
        let window = |id: &str, panes| WindowInfo {
            window_id: id.to_owned(),
            target: TargetIdentity {
                instance: None,
                connect: None,
            },
            server_id: None,
            selected_tab: None,
            visible_tabs: Vec::new(),
            panes,
            busy: false,
        };
        let file = |pane: &str, path: &str| UnsavedFile {
            pane_id: PaneId::new(pane),
            path: path.to_owned(),
        };
        let closing = [PaneId::new("content-a"), PaneId::new("term")];

        let one = [window(
            "primary",
            vec![
                pane("content-a", PaneType::Content, &["/src/a.rs"]),
                pane("content-b", PaneType::Content, &["/src/b.rs"]),
            ],
        )];
        assert_eq!(
            unsaved_files(&closing, &one),
            [file("content-a", "/src/a.rs")]
        );

        let clean = [window(
            "primary",
            vec![pane("content-a", PaneType::Content, &[])],
        )];
        assert!(unsaved_files(&closing, &clean).is_empty());
        assert!(unsaved_files(&closing, &[]).is_empty());

        let several = [
            window(
                "primary",
                vec![pane("content-a", PaneType::Content, &["/src/a.rs"])],
            ),
            window(
                "aux",
                vec![pane(
                    "content-a",
                    PaneType::Content,
                    &["/src/c.rs", "/src/a.rs"],
                )],
            ),
        ];
        assert_eq!(
            unsaved_files(&closing, &several),
            [
                file("content-a", "/src/a.rs"),
                file("content-a", "/src/c.rs")
            ]
        );

        // A terminal's Files view holds unsaved edits too.
        let terminal = [window(
            "primary",
            vec![pane("term", PaneType::Terminal, &["/src/t.rs"])],
        )];
        assert_eq!(
            unsaved_files(&closing, &terminal),
            [file("term", "/src/t.rs")]
        );
    }

    #[test]
    fn content_rows_report_files_settings_and_monitor_panes() {
        use compi_protocol::{
            ContentEnvironment, FolderContent, MonitorContent, PaneContent, SessionId,
            SettingsContent, SettingsSection, TabId,
        };
        let files = PaneContent::Files(FolderContent {
            environment: ContentEnvironment::Host,
            root: "/work".into(),
            tabs: Vec::new(),
            active: None,
        });
        let settings = PaneContent::Settings(SettingsContent {
            section: SettingsSection::Terminal,
        });
        let leaf = |pane: &str, content: &PaneContent| LayoutNode::Content {
            pane_id: PaneId::new(pane),
            content: content.clone(),
        };
        let tab = WorkspaceTab {
            id: TabId::new("tab-1"),
            label: String::new(),
            layout: LayoutNode::Split {
                axis: SplitAxis::Horizontal,
                ratio: 0.5,
                first: Box::new(leaf("files", &files)),
                second: Box::new(leaf("settings", &settings)),
            },
            previous_layout: None,
            merge: None,
        };
        let session = WorkspaceSession {
            id: SessionId::new("session-1"),
            label: "Default".into(),
            tabs: vec![tab.clone()],
        };

        let row = content_row(&session, &tab, &PaneId::new("files"), &files);
        assert_eq!(row["type"], "content");
        assert_eq!(row["content"]["kind"], "files");
        assert_eq!(row["content"]["root"], "/work");
        let row = content_row(&session, &tab, &PaneId::new("settings"), &settings);
        assert_eq!(row["type"], "settings");
        assert_eq!(row["pane_id"], "settings");
        assert_eq!(
            row["content"],
            json!({"kind": "settings", "section": "terminal"})
        );
        let monitor = PaneContent::Monitor(MonitorContent::default());
        let row = content_row(&session, &tab, &PaneId::new("monitor"), &monitor);
        assert_eq!(row["type"], "monitor");
        assert_eq!(
            row["content"],
            json!({"kind": "monitor", "page": "overview"})
        );
    }
}

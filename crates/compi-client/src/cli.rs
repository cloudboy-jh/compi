//! Public, existing-daemon-only command interface. Presentation operations are routed
//! to the owning native host; observation and explicit input never acquire a view.
mod capture;
mod changes;
mod help;
mod targets;
#[cfg(any(windows, target_os = "macos"))]
mod update;

use crate::{DaemonClient, Result, arrangement, config, connection::ConnectionTarget};
use compi_protocol::{
    LayoutNode, MutationId, MutationRequest, PaneId, ScreenSnapshot, SplitAxis, SurfaceId,
    SurfaceInfo, SurfaceStatus, WorkspaceMutation, WorkspaceSnapshot,
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
}
impl CliError {
    fn new(exit: i32, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            exit,
            code,
            message: message.into(),
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
    if !version && !recognizes(raw) {
        return None;
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
    let result = parse(raw).and_then(|options| execute(&options));
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
                eprintln!(
                    "{}",
                    json!({"ok": false, "error": {"code":error.code,"message":error.message,"exit_code":error.exit}})
                );
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

fn recognizes(raw: &[String]) -> bool {
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
        return COMMANDS.contains(&arg);
    }
    raw.iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
}

fn parse(raw: &[String]) -> std::result::Result<Options, CliError> {
    let inherited_instance = std::env::var("COMPI_INSTANCE").ok();
    let mut options = Options {
        instance: inherited_instance.clone(),
        surface: std::env::var("COMPI_SURFACE_ID")
            .ok()
            .filter(|s| !s.is_empty()),
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
        let path = theme_path(&options.args[2])?;
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
                    &state,
                    session.tabs.iter().flat_map(|tab| {
                        arrangement::leaves(&tab.layout)
                            .into_iter()
                            .map(|(_, surface)| surface)
                    }),
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
                confirm_close(
                    &state,
                    arrangement::leaves(&tab.layout)
                        .into_iter()
                        .map(|(_, surface)| surface),
                )?;
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
                    Some(targets::pane(&state, options)?.2)
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
                        for (pane_id, surface_id) in arrangement::leaves(&tab.layout) {
                            if selected_pane
                                .as_ref()
                                .is_some_and(|selected| selected != &pane_id)
                            {
                                continue;
                            }
                            let surface = state
                                .surface(&surface_id)
                                .ok_or_else(|| CliError::target("pane surface is missing"))?;
                            panes.push(json!({"workspace_id":session.id,"tab_id":tab.id,"pane_id":pane_id,"surface":surface,"metadata":client.surface_metadata(surface)?}));
                        }
                    }
                }
                return Ok(Output::Value(json!(panes)));
            }
            let (session, tab, pane_id, surface_id) = targets::pane(&state, options)?;
            let surface = state
                .surface(&surface_id)
                .ok_or_else(|| CliError::target("pane surface is missing"))?;
            match action.unwrap() {
                "show" => {
                    return Ok(Output::Value(
                        json!({"workspace_id":session.id,"tab_id":tab.id,"pane_id":pane_id,"surface":surface,"metadata":client.surface_metadata(surface)?}),
                    ));
                }
                "close" => {
                    confirm_close(&state, [surface_id.clone()])?;
                    WorkspaceMutation::RemovePane { pane_id }
                }
                "terminate" => WorkspaceMutation::EndSurface {
                    surface_id,
                    expected_lifetime: surface.process_lifetime_id.clone(),
                },
                "swap" => {
                    let other_options = Options {
                        pane: Some(options.with[0].clone()),
                        tab: Some(tab.id.to_string()),
                        ..Default::default()
                    };
                    let other = targets::pane(&state, &other_options)?.2;
                    WorkspaceMutation::ArrangeTab {
                        tab_id: tab.id.clone(),
                        layout: arrangement::swap(&tab.layout, &pane_id, &other).ok_or_else(
                            || CliError::usage("swap needs two distinct panes in the same tab"),
                        )?,
                    }
                }
                "capture" => {
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
                        "resize" => crate::cli_window::Action::ResizePane {
                            pane_id,
                            cols: optional_number(options, "--cols", 20, i16::MAX as usize)?
                                .map_or(surface.cols, |cols| cols as i16),
                            rows: optional_number(options, "--rows", 4, i16::MAX as usize)?
                                .map_or(surface.rows, |rows| rows as i16),
                        },
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
        "attach" | "detach" | "run" => {
            let (_, _, pane_id, surface_id) = targets::pane(&state, options)?;
            let surface = state
                .surface(&surface_id)
                .ok_or_else(|| CliError::target("pane surface is missing"))?
                .clone();
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
            } => output.push((pane_id, surface_id)),
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

fn confirm_close(
    state: &WorkspaceSnapshot,
    surfaces: impl IntoIterator<Item = compi_protocol::SurfaceId>,
) -> std::result::Result<(), CliError> {
    let running = surfaces
        .into_iter()
        .filter(|id| {
            state.surface(id).is_some_and(|surface| {
                matches!(
                    surface.status,
                    SurfaceStatus::Starting | SurfaceStatus::Running
                )
            })
        })
        .collect::<Vec<_>>();
    if running.is_empty() {
        return Ok(());
    }
    if !interactive() {
        return Err(CliError::new(
            4,
            "confirmation_required",
            "close would end running processes; interactive confirmation is required. Scripts must explicitly use pane terminate first; no --yes/--force bypass",
        ));
    }
    eprint!(
        "Close will end {} running process(es) [{}]. Type close to confirm: ",
        running.len(),
        running
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    io::stderr()
        .flush()
        .map_err(|e| CliError::new(1, "io_error", e.to_string()))?;
    let answer = read_answer()?;
    if answer.trim() != "close" {
        return Err(CliError::new(
            4,
            "cancelled",
            "close cancelled; no processes were ended",
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

fn theme_path(path: &str) -> std::result::Result<PathBuf, CliError> {
    #[cfg(windows)]
    if let Ok(distribution) = std::env::var("WSL_DISTRO_NAME") {
        let native = PathBuf::from(path);
        // Drive-qualified and UNC paths are already Windows-native. Relative
        // Linux files are resolved against the invoking shell, not WSL's home.
        if !native.is_absolute() || path.starts_with('/') {
            let absolute = if path.starts_with('/') {
                path.to_owned()
            } else {
                let cwd=std::env::var("COMPI_SHELL_CWD").map_err(|_|CliError::usage("relative WSL theme path needs shell cwd; invoke through the compi shell bridge or use an absolute Unix path"))?;
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
        let (_, _, pane_id, surface_id) = targets::pane(state, options)?;
        let surface = state
            .surface(&surface_id)
            .ok_or_else(|| CliError::target("pane surface is missing"))?;
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
    fn update_accepts_only_check_and_json() {
        let accepted = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            assert!(recognizes(&args));
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
}

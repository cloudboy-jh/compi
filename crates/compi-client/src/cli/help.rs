//! The CLI's help: one catalogue of every command and action, printed as a grouped
//! overview (`compi help`), a command's actions (`compi help pane`, or `compi pane`) or
//! one action's details (`compi pane list --help`), and returned whole under `--json`.

use super::human;
use serde_json::{Value, json};

/// One command, or one action of a command, as help describes it.
pub(super) struct Entry {
    pub(super) command: &'static str,
    /// `None` for a command without actions, or a command's own form beside them
    /// (`notes FILE` beside `notes search`).
    pub(super) action: Option<&'static str>,
    /// After `compi `; further forms on following lines.
    usage: &'static str,
    /// One line for lists.
    summary: &'static str,
    detail: &'static str,
    example: &'static str,
}

/// Commands by what they work on, in the order the overview lists them.
const GROUPS: &[(&str, &[&str])] = &[
    ("Workspaces and tabs", &["workspace", "tab"]),
    ("Panes", &["pane", "split", "layout", "window"]),
    ("Terminals", &["run", "attach", "detach"]),
    (
        "Views",
        &[
            "files", "edit", "notes", "diff", "browse", "settings", "monitor",
        ],
    ),
    ("Palette", &["command"]),
    ("Compi", &["theme", "update", "changes", "help"]),
];

/// Every command and action. Each command of `super::COMMANDS` has at least one.
pub(super) const ENTRIES: &[Entry] = &[
    Entry {
        command: "workspace",
        action: Some("list"),
        usage: "workspace list",
        summary: "List workspaces with their IDs",
        detail: "List persistent workspaces and stable IDs.",
        example: "compi --instance dev --json workspace list",
    },
    Entry {
        command: "workspace",
        action: Some("show"),
        usage: "workspace show",
        summary: "Show one workspace's tabs and panes",
        detail: "Show the selected workspace, tabs and panes.",
        example: "compi --workspace work workspace show",
    },
    Entry {
        command: "workspace",
        action: Some("create"),
        usage: "workspace create NAME",
        summary: "Create an empty workspace",
        detail: "Create an empty persistent workspace.",
        example: "compi workspace create work",
    },
    Entry {
        command: "workspace",
        action: Some("rename"),
        usage: "workspace rename NAME",
        summary: "Rename a workspace",
        detail: "Rename the selected workspace without changing its ID.",
        example: "compi --workspace work workspace rename project",
    },
    Entry {
        command: "workspace",
        action: Some("close"),
        usage: "workspace close",
        summary: "Close a workspace, asking first",
        detail: "Close the workspace. Running processes and unsaved edits in open windows require interactive confirmation; scripts must explicitly terminate panes and save files first.",
        example: "compi --workspace work workspace close",
    },
    Entry {
        command: "tab",
        action: Some("list"),
        usage: "tab list",
        summary: "List tabs with their IDs",
        detail: "List tabs in the selected workspace; outside shell context an omitted workspace lists all tabs.",
        example: "compi --workspace work --json tab list",
    },
    Entry {
        command: "tab",
        action: Some("open"),
        usage: "tab open [--name NAME] [--cwd PATH]",
        summary: "Open a tab with a new shell",
        detail: "Open a tab with the configured shell, in the selected workspace.",
        example: "compi --workspace work tab open --name build --cwd /home/me/project",
    },
    Entry {
        command: "tab",
        action: Some("rename"),
        usage: "tab rename NAME",
        summary: "Rename a tab",
        detail: "Rename the selected tab, retaining its stable ID.",
        example: "compi --tab work/build tab rename tests",
    },
    Entry {
        command: "tab",
        action: Some("focus"),
        usage: "tab focus",
        summary: "Show a tab in its window",
        detail: "Focus the existing tab in its native window; does not steal its terminal attachment.",
        example: "compi --tab work/tests tab focus",
    },
    Entry {
        command: "tab",
        action: Some("move"),
        usage: "tab move --index N [--workspace DEST]",
        summary: "Reorder a tab or move it to another workspace",
        detail: "Reorder the selected tab at zero-based index N, or move it to another workspace. --tab identifies the source independently of destination --workspace.",
        example: "compi --tab work/tests tab move --workspace archive --index 0",
    },
    Entry {
        command: "tab",
        action: Some("detach"),
        usage: "tab detach",
        summary: "Move a tab to a new window",
        detail: "Transfer the selected tab's existing view to a new native window.",
        example: "compi --tab work/tests tab detach",
    },
    Entry {
        command: "tab",
        action: Some("close"),
        usage: "tab close",
        summary: "Close a tab, asking first",
        detail: "Close the selected tab, confirming interactively if any process is running or an open window holds unsaved edits in its content panes.",
        example: "compi --tab work/tests tab close",
    },
    Entry {
        command: "tab",
        action: Some("merge"),
        usage: "tab merge --with SOURCE... [--with SOURCE...] [--layout PRESET]",
        summary: "Merge other tabs' panes into a tab",
        detail: "Merge source tabs from the same workspace into the selected tab, preserving shells and pane IDs. An optional built-in or named preset arranges the combined panes.",
        example: "compi --tab work/build tab merge --with work/tests --layout grid",
    },
    Entry {
        command: "tab",
        action: Some("unmerge"),
        usage: "tab unmerge",
        summary: "Split a merged tab back into its tabs",
        detail: "Restore the surviving tabs recorded by a merge, retaining stable IDs.",
        example: "compi --tab work/build tab unmerge",
    },
    Entry {
        command: "pane",
        action: Some("list"),
        usage: "pane list",
        summary: "List panes with their type, state and IDs",
        detail: "List each pane's ID and type: terminal panes with surface IDs, process state and fresh runtime metadata; content panes with the folder and files they show. Narrow with --workspace, --tab or --pane.",
        example: "compi --tab work/build --json pane list",
    },
    Entry {
        command: "pane",
        action: Some("show"),
        usage: "pane show",
        summary: "Show one pane's details",
        detail: "Show the selected pane and fresh runtime metadata without attaching or resizing.",
        example: "compi --pane pane-id --json pane show",
    },
    Entry {
        command: "pane",
        action: Some("focus"),
        usage: "pane focus",
        summary: "Focus a pane in its window",
        detail: "Focus the pane in its existing native window.",
        example: "compi --pane pane-id pane focus",
    },
    Entry {
        command: "pane",
        action: Some("resize"),
        usage: "pane resize [--cols N] [--rows N]",
        summary: "Resize a terminal pane's columns and rows",
        detail: "Resize the selected terminal pane's native presentation. At least one dimension is required; the omitted dimension retains its actual current value. Minimum 20 columns and 4 rows. Content panes are not sized in cells; resize a terminal beside one instead.",
        example: "compi --pane pane-id pane resize --cols 100",
    },
    Entry {
        command: "pane",
        action: Some("swap"),
        usage: "pane swap --with OTHER",
        summary: "Swap two panes in a tab",
        detail: "Exchange two panes in the same tab without restarting processes.",
        example: "compi --pane pane-id pane swap --with other-pane-id",
    },
    Entry {
        command: "pane",
        action: Some("zoom"),
        usage: "pane zoom",
        summary: "Fill the tab with one pane",
        detail: "Maximize the pane within its tab's native presentation.",
        example: "compi --pane pane-id pane zoom",
    },
    Entry {
        command: "pane",
        action: Some("unzoom"),
        usage: "pane unzoom",
        summary: "Return a zoomed pane to its place",
        detail: "Restore the tab's normal pane presentation.",
        example: "compi --pane pane-id pane unzoom",
    },
    Entry {
        command: "pane",
        action: Some("float"),
        usage: "pane float",
        summary: "Float a pane above its tab",
        detail: "Float the existing pane in its tab.",
        example: "compi --pane pane-id pane float",
    },
    Entry {
        command: "pane",
        action: Some("dock"),
        usage: "pane dock",
        summary: "Return a floating pane to its place",
        detail: "Return a floating pane to its existing docked location.",
        example: "compi --pane pane-id pane dock",
    },
    Entry {
        command: "pane",
        action: Some("close"),
        usage: "pane close",
        summary: "Close a pane, asking first",
        detail: "Remove the selected pane, confirming interactively before ending a running process or discarding a content pane's unsaved edits in an open window.",
        example: "compi --pane pane-id pane close",
    },
    Entry {
        command: "pane",
        action: Some("terminate"),
        usage: "pane terminate",
        summary: "End a terminal pane's process, keeping the pane",
        detail: "Explicitly terminate this terminal pane's process lifetime; preserve pane and surface IDs.",
        example: "compi --pane pane-id pane terminate",
    },
    Entry {
        command: "pane",
        action: Some("send"),
        usage: "pane send (--text TEXT | --key KEY)",
        summary: "Type text or one key into a terminal pane",
        detail: "Send literal text with no implicit Enter, or one explicit key, to a terminal pane. Keys: enter, tab, escape, space, backspace, arrows, home/end, insert/delete, pageup/pagedown, f1..f12; ctrl-/alt-/shift- modifiers supported.",
        example: "compi --pane pane-id pane send --text 'echo hello'; compi --pane pane-id pane send --key enter",
    },
    Entry {
        command: "pane",
        action: Some("capture"),
        usage: "pane capture [--scope screen|scrollback] [--format text|ansi]",
        summary: "Print a terminal pane's screen or history",
        detail: "Capture a terminal pane observationally: screen is the default; scrollback includes retained history and the current screen. Text is the default; ANSI preserves text styling and hyperlinks. Soft wraps join, hard breaks and spaces remain. Never attaches or resizes.",
        example: "compi --pane pane-id pane capture --scope scrollback --format ansi",
    },
    Entry {
        command: "split",
        action: None,
        usage: "split [--right | --down] [--cwd PATH]\n       compi split --panes TOTAL --layout PRESET [--cwd PATH]",
        summary: "Split a terminal pane, or fill a tab to N panes",
        detail: "Split the selected terminal pane, or fill the selected tab to TOTAL panes and arrange it. Reuses all existing shells and IDs; a lower total is rejected before mutation. Built-ins and named configured presets are accepted.",
        example: "compi split --panes 4 --layout grid",
    },
    Entry {
        command: "layout",
        action: None,
        usage: "layout list | PRESET | mirror | flip | restore",
        summary: "Arrange a tab's panes",
        detail: "Arrange the selected tab without restarting processes. Presets come from built-ins and configured layout_presets. Restore returns to the previous arrangement, or restores original tabs immediately after a merge.",
        example: "compi --tab work/build layout main-side",
    },
    Entry {
        command: "window",
        action: Some("list"),
        usage: "window list",
        summary: "List windows and the panes they show",
        detail: "List native window slot IDs, visible tab IDs, and pane presentation state, with each pane's type (terminal or content), the unsaved_files of a content pane or a terminal's Files view, and a Browser pane's page (url, title, the loading address and the error of a failed load), for the selected connection.",
        example: "compi --instance dev --json window list",
    },
    Entry {
        command: "run",
        action: None,
        usage: "run -- COMMAND [ARG...]",
        summary: "Type a command into a terminal pane and press Enter",
        detail: "Safely shell-quote argv and explicitly submit Enter to the selected pane. Supported POSIX shells only; alternate-screen applications and argv control characters that could become terminal keys are rejected. Does not wait for command completion.",
        example: "compi run -- printf '%s\\n' 'hello world'",
    },
    Entry {
        command: "attach",
        action: None,
        usage: "attach",
        summary: "Attach this console to a pane",
        detail: "Interactively attach an unattached pane to this console. An existing GUI or console attachment is never stolen. Press Ctrl-] to detach.",
        example: "compi --pane pane-id attach",
    },
    Entry {
        command: "detach",
        action: None,
        usage: "detach",
        summary: "Release this console's attachment",
        detail: "Release a console attachment on the selected pane without ending its process. GUI attachments are not detached; use tab detach for native window transfer.",
        example: "compi --pane pane-id detach",
    },
    Entry {
        command: "files",
        action: None,
        usage: "files [DIR]",
        summary: "Browse a folder in the terminal's pane",
        detail: "Browse DIR (default: the shell's current directory) in the selected terminal's own pane, in that terminal's environment (host or its WSL distribution). The Files view replaces what the pane shows; the shell keeps running underneath at its size, and Cmd/Ctrl+Shift+E or Esc returns to it. Relative DIR resolves against the shell's directory; . and .. are resolved without touching the filesystem. Files the view already shows stay open as tabs. From a content pane (--pane), that pane shows DIR.",
        example: "compi files ../docs",
    },
    Entry {
        command: "edit",
        action: None,
        usage: "edit [FILE[:LINE[:COL]]]",
        summary: "Open a file in the terminal's pane",
        detail: "Open FILE in the selected terminal's Files view, in place of the shell, and put the cursor on LINE (default 1; COL is accepted and ignored). The view keeps the files it has open; its folder stays when it holds FILE, else becomes the shell's directory when that holds FILE, else FILE's folder. From a content pane (--pane), that pane opens FILE and keeps its folder. A missing or unreadable FILE is reported by the editor. An existing folder without :LINE, or no FILE, browses it like files. With no Compi window showing the tab the view still opens and a note says it was not shown.",
        example: "compi edit src/main.rs:42",
    },
    Entry {
        command: "notes",
        action: None,
        usage: "notes [FILE|FOLDER] [--edit]",
        summary: "Open a Markdown note to read",
        detail: "Open a Markdown FILE in the existing Files/editor view in Read mode (formatted); --edit opens it in Edit mode instead. Either becomes the file's remembered mode in that window. A FOLDER, or no argument, browses like compi files. Paths resolve in the selected pane's environment against its current directory, like edit. Notes remain ordinary .md/.markdown files, with no notebook database or persisted index. Notebook search/backlinks recursively read at most 4,096 notes, 2,048 directories, 32,768 entries, 64 directory levels and 32 MiB of text, with a 15-second scan budget; .git, target and node_modules are skipped and directory symlinks are not followed. Incomplete scans report warnings.",
        example: "compi notes ./notes/plan.md",
    },
    Entry {
        command: "notes",
        action: Some("search"),
        usage: "notes search QUERY [FOLDER]",
        summary: "Search Markdown notes",
        detail: "Search Markdown contents, filenames and heading titles under FOLDER (default: the pane's current directory), without a GUI. Matches are case-insensitive and print FILE:LINE:TEXT. --json returns root, environment, query, hits, partial and warnings. Unreadable files and scan limits are visible warnings, not silently complete results.",
        example: "compi --json notes search 'project plan' ./notes",
    },
    Entry {
        command: "notes",
        action: Some("backlinks"),
        usage: "notes backlinks FILE [FOLDER]",
        summary: "List notes that link to a note",
        detail: "List source lines containing wiki links to FILE in FOLDER (default: the pane's current directory), without a GUI. FILE resolves relative to the notebook root, with optional Markdown extension and unique basename/title fallback; ambiguous or missing targets fail. [[target#heading|label]] links count, but inline, fenced and indented code do not. --json returns root, environment, file, hits, partial and warnings.",
        example: "compi --json notes backlinks decisions ./notes",
    },
    Entry {
        command: "diff",
        action: None,
        usage: "diff [PATH[:LINE[:COL]]]\n       compi diff COMMIT [PATH[:LINE[:COL]]]",
        summary: "Show a file's changes or a commit",
        detail: "Show PATH's uncommitted change as a diff tab in the selected terminal's Files view, in place of the shell, scrolled to LINE of the new side (COL is accepted and ignored). The unstaged change (work tree against the index) wins over the staged one (index against HEAD). PATH resolves like edit; the view's folder stays when it lies in the repository and holds PATH, else becomes the repository's top level. From a content pane (--pane) inside the repository, that pane shows the diff. Without PATH, the repository of the shell's directory lists its changes in the view's Changes sidebar. With COMMIT (a revision such as HEAD~1, a branch or an id), the diff is what that commit changed in PATH against its first parent, read-only; without PATH its changed files show in a section of their own at the top of the Changes sidebar. A lone argument is PATH when that file has uncommitted changes, else COMMIT when Git knows it. --json adds repo, commit (the full id), short and summary for a commit, with path and line, or changes (the count). Exits 1 with not_a_repository outside a Git work tree, unknown_commit when COMMIT names no commit, and no_changes when PATH has no uncommitted change (or names no commit) or COMMIT did not change PATH. Git is only read: files, the index and other Git state never change.",
        example: "compi diff HEAD~1 src/main.rs",
    },
    Entry {
        command: "browse",
        action: None,
        usage: "browse [URL] [--down]",
        summary: "Open a web page beside the terminal",
        detail: "Open URL in a Browser pane beside the selected terminal (below it with --down); the shell stays as it is. URL is an http, https or file URL, or about:blank; without a scheme, http:// is assumed, so localhost:3000 opens http://localhost:3000. Text pasted around a link (quotes, <...>, a Markdown link, a Local: prefix, trailing punctuation) is dropped. Without URL the pane opens on its home page (local servers your terminals printed, recent sites) with the keyboard in its address field, where a link can be pasted without shell quoting. From a Browser pane (--pane), that pane goes to URL. The page is the system web view's (WebView2 on Windows, WKWebView on macOS), with one persistent profile per instance. --json returns {pane_id, url, shown, not_shown, receipt, workspace}.",
        example: "compi browse 'localhost:3000/search?q=x'",
    },
    Entry {
        command: "settings",
        action: None,
        usage: "settings [SECTION]",
        summary: "Show Settings in the terminal's pane",
        detail: "Show Settings in the selected terminal's own pane, in place of the shell, which keeps running underneath at its size; Cmd/Ctrl+Shift+E or Esc returns to it. SECTION is appearance, interface, notifications, terminal, keyboard, monitor, updates or advanced; without it the pane keeps the section it shows, else Appearance. From a Settings pane (--pane), that pane changes section.",
        example: "compi settings terminal",
    },
    Entry {
        command: "monitor",
        action: None,
        usage: "monitor [PAGE] [--source SOURCE]",
        summary: "Show Monitor in the terminal's pane",
        detail: "Show Monitor in the selected terminal's own pane, in place of the shell, which keeps running underneath at its size; Cmd/Ctrl+Shift+E or Esc returns to it. PAGE is overview, processes, cpu, memory, gpu, disk or network; without it the pane keeps the page it shows, else Overview. --source host|wsl:NAME shows that source (see compi monitor sources); without it the pane keeps the source it shows, else the daemon's host. From a Monitor pane (--pane), that pane changes page or source. monitor totals, list and inspect print Monitor's data for scripts with no window, monitor sources lists the sources and monitor quit quits a process; see each one's --help.",
        example: "compi monitor processes",
    },
    Entry {
        command: "monitor",
        action: Some("totals"),
        usage: "monitor totals [--history 1m|5m|15m|1h] [--source SOURCE]",
        summary: "Print the machine's CPU, memory, disk and network",
        detail: "Print the host's totals from the daemon, with no window: CPU, memory used and available, swap, GPU, disk read/write and network down/up of the newest sample. --history adds every sample of that span ending at the newest (one every 2 seconds, up to an hour). --source host|wsl:NAME reads that source instead of the daemon's host (see compi monitor sources); an unknown one exits 2 with unknown_source. --json returns {source, host, samples} with the protocol's source, host info and samples, oldest first; only the newest carries the per-core, volume and interface detail. Exits 1 with no_sample when the daemon has sampled nothing within 5 seconds.",
        example: "compi --json monitor totals --history 5m",
    },
    Entry {
        command: "monitor",
        action: Some("list"),
        usage: "monitor list [--group] [--search TEXT] [--source SOURCE]",
        summary: "Print running processes, busiest first",
        detail: "Print the host's processes from the daemon, with no window, busiest first as Monitor's Processes table sorts them: PID, name, CPU (share of the whole machine), memory and user. Waits for the first sample with CPU values (about half a second). --search keeps processes whose name, command line, executable, user or PID contains every word, ignoring case. --group shows one row per app with its process count and sums; a search naming the app keeps all its processes. --source host|wsl:NAME lists that source's processes instead of the daemon's host (see compi monitor sources); an unknown one exits 2 with unknown_source. --json returns {source, sampled_at_ms, logical_cpus, apps_known, processes} (each process with its details and cpu_share) and, with --group, groups (id, name, pids and sums). Exits 1 with no_sample when no sample arrives within 5 seconds.",
        example: "compi monitor list --group --search safari",
    },
    Entry {
        command: "monitor",
        action: Some("sources"),
        usage: "monitor sources",
        summary: "List the machines Monitor can watch",
        detail: "List what Monitor can watch from the daemon, one per line as --source names it: host first, then, on a Windows daemon, wsl:NAME for each WSL 2 distribution. --json returns {sources} in the protocol's shape ({\"type\":\"host\"}, {\"type\":\"wsl\",\"distribution\":NAME}).",
        example: "compi monitor sources",
    },
    Entry {
        command: "monitor",
        action: Some("inspect"),
        usage: "monitor inspect PID [--source SOURCE]",
        summary: "Print one process in detail",
        detail: "Print one process from a sample of the daemon's host (or --source host|wsl:NAME), with what the sample leaves out: name, PID, start time and how long it has run, user, state, working directory, executable, every argument, parent, CPU (share of the whole machine), memory and, when it runs in a Compi terminal, that terminal's surface and pane. --json returns the process as monitor list gives it, plus source, sampled_at_ms, inspection {state, working_directory, arguments, arguments_cut}, surface and pane_id. Exits 1 with no_process when no process has that PID, process_gone when it exits before it is read, and 2 with unknown_source for an unknown source.",
        example: "compi monitor inspect 4120",
    },
    Entry {
        command: "monitor",
        action: Some("quit"),
        usage: "monitor quit PID [--force] [--source SOURCE]",
        summary: "Quit a process, asking first",
        detail: "Ask a process of the daemon's host (or --source host|wsl:NAME) to quit, as Monitor's Quit does: SIGTERM, or on Windows a close request to its windows. --force ends it at once (SIGKILL, or TerminateProcess), and unsaved work in it is lost. Asks first, naming the process and whether it runs in a Compi terminal; without a terminal to ask on it exits 4 with confirmation_required, and there is no bypass. The daemon signals only the process the PID named when it was looked up, never its group. Prints what was sent; --json returns {source, pid, start_time, name, force, outcome}. Exits 1 with no_process (no such PID), process_gone (it exited, or the PID was reused, first), permission_denied, no_window (Windows: nothing to close; use --force) or refused (the daemon will not signal it, with the reason); 2 with unknown_source for an unknown source; 4 with cancelled when the answer is not yes.",
        example: "compi monitor quit 4120",
    },
    Entry {
        command: "command",
        action: Some("list"),
        usage: "command list",
        summary: "List palette commands and shortcuts",
        detail: "List every palette command: its ID, name and this platform's shortcut with configured keybindings applied, with no window needed. --json returns {platform, commands} with each command's id, name, category, shortcut (null when unbound), default_shortcut and description (null when it has none). Run one with command run ID.",
        example: "compi --json command list",
    },
    Entry {
        command: "command",
        action: Some("run"),
        usage: "command run ID",
        summary: "Run a palette command in its window",
        detail: "Run the palette command ID (see command list) in the Compi window that shows the selected pane's tab (--window selects another), with that pane focused first, as if chosen from the palette there. A command that asks first, such as Close pane, opens its dialog in the window; there is no bypass. A command disabled there exits 1 with command_disabled and the palette's reason; an unknown ID exits 2. --json returns {command, name, pane_id, window_id}.",
        example: "compi command run open_settings",
    },
    Entry {
        command: "theme",
        action: Some("install"),
        usage: "theme install FILE.json",
        summary: "Install a theme file",
        detail: "Validate and install a theme family in the local app library without launching a window. --connect is rejected. The WSL shell bridge resolves Linux files against the invoking shell's directory.",
        example: "compi theme install ./my-theme.json",
    },
    Entry {
        command: "update",
        action: None,
        usage: "update [--check]",
        summary: "Check for and install an update",
        detail: "Check for a newer Compi. --check only reports it. Otherwise download and verify it, list the shells a restart would end (marking this shell), and ask Restart now? [y/N]; only y restarts. Without an interactive terminal it stops after the download with exit 4; restart from a terminal or Settings → Updates. A running Compi window performs the restart, saving and reopening its windows; with none open, Compi opens first. --connect is rejected; there is no --yes/--force bypass.",
        example: "compi update --check",
    },
    Entry {
        command: "changes",
        action: None,
        usage: "changes",
        summary: "Print what changed in this version",
        detail: "Print the running version and what changed in it, from the notes What's New shows: the headline, the other highlights, then each section under its title. Needs no running Compi. --connect is rejected.",
        example: "compi --json changes",
    },
    Entry {
        command: "help",
        action: None,
        usage: "help [COMMAND [ACTION]]",
        summary: "List every command, or one command's actions",
        detail: "List every command grouped by what it works on, with global options and exit codes. With COMMAND, list its actions; with COMMAND ACTION, show its details (the same as compi COMMAND ACTION --help). --json returns the text as help and the entries listed as commands, each with command, action, group, usage, summary, description and example.",
        example: "compi help pane",
    },
];

const GLOBAL_OPTIONS: &[(&str, &str)] = &[
    (
        "--instance NAME",
        "Use that Compi instance ('' is the default one)",
    ),
    (
        "--connect [USER@]HOST[:PORT]",
        "Use the Compi daemon on another machine over SSH",
    ),
    ("--workspace ID|NAME", "Select a workspace"),
    ("--tab ID|WORKSPACE/NAME", "Select a tab"),
    ("--pane ID|SURFACE_ID", "Select a pane"),
    ("--window ID", "Select a window"),
    ("--surface ID", "Act as if run from that terminal"),
    ("--config PATH", "Read that configuration file"),
    (
        "--json",
        "Print one line of JSON: {ok, result} or {ok, error}",
    ),
    ("--help, -h", "Show help for the command given"),
];

const EXIT_CODES: &str = "0 success · 1 failed · 2 invalid arguments · 3 missing or ambiguous target · 4 confirmation required or cancelled · 5 daemon or window unavailable · 6 changed meanwhile (revision, generation or lifetime)";

const OUTPUT: &str = "In a terminal, lists print as tables and details as name: value lines. Piped or redirected, they print JSON. Scripts and agents should pass --json, which prints the same one line either way.";

const CONTEXT: &str = "Inside a Compi shell, COMPI_INSTANCE and COMPI_SURFACE_ID select its instance and pane. Outside one, only a running daemon is used; with several instances, or a name that matches more than one thing, select one explicitly.";

/// Help for `command` and `action` as the terminal shows it, `width` columns wide.
pub(super) fn text(command: &str, action: Option<&str>, width: usize) -> String {
    if let Some(action) = action
        && let Some(entry) = find(command, Some(action))
    {
        return details(entry, width);
    }
    let actions: Vec<&Entry> = actions(command).collect();
    match find(command, None) {
        Some(entry) if actions.is_empty() => details(entry, width),
        Some(entry) => format!(
            "{}\nActions:\n{}",
            details(entry, width),
            list(&actions, width)
        ),
        None if !actions.is_empty() => format!(
            "Usage: compi {command} ACTION\n\n{}\n{}\n",
            list(&actions, width),
            human::wrap(
                &format!("Run compi {command} ACTION --help for one action's details."),
                width,
                0
            )
        ),
        None => overview(width),
    }
}

/// The entries `text` describes, for `--json`.
pub(super) fn entries(command: &str, action: Option<&str>) -> Value {
    let selected: Vec<&Entry> = match action.and_then(|action| find(command, Some(action))) {
        Some(entry) => vec![entry],
        None if ENTRIES.iter().any(|entry| entry.command == command) => ENTRIES
            .iter()
            .filter(|entry| entry.command == command)
            .collect(),
        None => ENTRIES.iter().collect(),
    };
    json!(
        selected
            .iter()
            .map(|entry| json!({
                "command": entry.command,
                "action": entry.action,
                "group": group(entry.command),
                "usage": usage(entry),
                "summary": entry.summary,
                "description": entry.detail,
                "example": entry.example,
            }))
            .collect::<Vec<_>>()
    )
}

/// Whether `command` does nothing without an action, so alone it shows its actions.
pub(super) fn needs_action(command: &str) -> bool {
    find(command, None).is_none() && actions(command).next().is_some()
}

fn find(command: &str, action: Option<&str>) -> Option<&'static Entry> {
    ENTRIES
        .iter()
        .find(|entry| entry.command == command && entry.action == action)
}

fn actions(command: &str) -> impl Iterator<Item = &'static Entry> {
    ENTRIES
        .iter()
        .filter(move |entry| entry.command == command && entry.action.is_some())
}

fn group(command: &str) -> &'static str {
    GROUPS
        .iter()
        .find(|(_, commands)| commands.contains(&command))
        .map_or("", |(name, _)| name)
}

/// `compi USAGE`, each further form on its own line.
fn usage(entry: &Entry) -> String {
    entry
        .usage
        .lines()
        .map(|form| format!("compi {}", form.trim_start().trim_start_matches("compi ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What an entry is called in lists: `pane list`, `notes`.
fn name(entry: &Entry) -> String {
    match entry.action {
        Some(action) => format!("{} {action}", entry.command),
        None => entry.command.to_owned(),
    }
}

/// Entries as `name  summary` lines, summaries aligned and wrapped.
fn list(entries: &[&Entry], width: usize) -> String {
    let rows = rows(entries);
    rows_text(&rows, width, column(&rows))
}

fn rows(entries: &[&Entry]) -> Vec<(String, &'static str)> {
    entries
        .iter()
        .map(|entry| (name(entry), entry.summary))
        .collect()
}

/// Where summaries start: two spaces past the longest name.
fn column(rows: &[(String, &str)]) -> usize {
    rows.iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        + 4
}

fn rows_text(rows: &[(String, &str)], width: usize, column: usize) -> String {
    let mut text = String::new();
    for (name, summary) in rows {
        text.push_str(&hang(&format!("  {name}"), summary, width, column));
    }
    text
}

/// `body` wrapped into the columns from `indent` to `width`, its first line after
/// `prefix` (padded to `indent`), the others indented.
fn hang(prefix: &str, body: &str, width: usize, indent: usize) -> String {
    let wrapped = human::wrap(body, width.saturating_sub(indent), 0);
    let mut text = String::new();
    for (index, line) in wrapped.lines().enumerate() {
        let lead = if index == 0 { prefix } else { "" };
        text.push_str(lead);
        text.push_str(&" ".repeat(indent.saturating_sub(lead.chars().count())));
        text.push_str(line);
        text.push('\n');
    }
    text
}

fn details(entry: &Entry, width: usize) -> String {
    format!(
        "Usage: {}\n\n{}\n\nExample: {}\n\nGlobal options, output and exit codes: compi help\n",
        usage(entry),
        human::wrap(entry.detail, width, 0),
        entry.example,
    )
}

fn overview(width: usize) -> String {
    let mut text = String::from(
        "Usage: compi [GLOBAL OPTIONS] COMMAND [ACTION] [ARGS]\n       compi [DIR]   opens the Compi app\n",
    );
    // One column for every group, so summaries line up down the whole list.
    let all: Vec<&Entry> = ENTRIES.iter().collect();
    let summaries = column(&rows(&all));
    for (group, commands) in GROUPS {
        let entries: Vec<&Entry> = ENTRIES
            .iter()
            .filter(|entry| commands.contains(&entry.command))
            .collect();
        text.push_str(&format!(
            "\n{group}\n{}",
            rows_text(&rows(&entries), width, summaries)
        ));
    }
    let options: Vec<(String, &str)> = GLOBAL_OPTIONS
        .iter()
        .map(|(option, summary)| ((*option).to_owned(), *summary))
        .collect();
    text.push_str(&format!(
        "\nGlobal options\n{}",
        rows_text(&options, width, column(&options))
    ));
    for (title, body) in [
        ("Output", OUTPUT),
        ("Context", CONTEXT),
        ("Exit codes", EXIT_CODES),
    ] {
        text.push_str(&format!("\n{title}\n{}", hang("", body, width, 2)));
    }
    text.push('\n');
    for line in [
        "Run compi help COMMAND for its actions, compi COMMAND ACTION --help for details.",
        "compi --version prints the version; compi changes prints what changed in it.",
    ] {
        text.push_str(&human::wrap(line, width, 0));
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_help_and_a_group_and_the_overview_lists_every_entry() {
        for command in super::super::COMMANDS {
            assert!(
                ENTRIES.iter().any(|entry| entry.command == *command),
                "{command} has no help"
            );
            assert!(!group(command).is_empty(), "{command} is in no group");
        }
        let overview = text("", None, 200);
        for entry in ENTRIES {
            assert!(
                overview.contains(&format!("  {} ", name(entry))),
                "{} is missing from compi help",
                name(entry)
            );
        }
    }

    #[test]
    fn help_resolves_actions_commands_and_groups() {
        // An action's own details.
        assert!(text("pane", Some("list"), 80).starts_with("Usage: compi pane list\n"));
        // A command that needs an action lists them.
        let pane = text("pane", None, 80);
        assert!(pane.starts_with("Usage: compi pane ACTION\n"));
        assert!(pane.contains("  pane capture "));
        assert!(needs_action("pane") && !needs_action("monitor") && !needs_action("files"));
        // A command with a form of its own and actions shows both.
        let monitor = text("monitor", None, 80);
        assert!(monitor.starts_with("Usage: compi monitor [PAGE] [--source SOURCE]\n"));
        assert!(monitor.contains("Actions:\n") && monitor.contains("  monitor quit "));
        // Anything else is not an action: its command's form.
        assert!(text("monitor", Some("processes"), 80).starts_with("Usage: compi monitor [PAGE]"));
        assert!(text("nonsense", None, 80).starts_with("Usage: compi [GLOBAL OPTIONS]"));
        // --json lists the entries the text covers.
        assert_eq!(entries("pane", Some("send")).as_array().unwrap().len(), 1);
        assert_eq!(entries("monitor", None).as_array().unwrap().len(), 6);
        assert_eq!(entries("", None).as_array().unwrap().len(), ENTRIES.len());
    }

    #[test]
    fn help_fits_the_terminal_it_is_given() {
        for width in [60, 80, 120] {
            for (command, action) in [
                ("", None),
                ("pane", None),
                ("monitor", None),
                ("diff", None),
            ] {
                for line in text(command, action, width).lines() {
                    // Only a single word longer than the line may run past it.
                    assert!(
                        line.chars().count() <= width || !line.trim().contains(' '),
                        "{width}: {line}"
                    );
                }
            }
        }
    }
}

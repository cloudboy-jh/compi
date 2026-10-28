pub fn text(command: &str, action: Option<&str>) -> String {
    let (usage, detail, example) = match (command, action) {
        ("workspace", Some("list")) => (
            "workspace list",
            "List persistent workspaces and stable IDs.",
            "compi --instance dev --json workspace list",
        ),
        ("workspace", Some("show")) => (
            "workspace show",
            "Show the selected workspace, tabs and panes.",
            "compi --workspace work workspace show",
        ),
        ("workspace", Some("create")) => (
            "workspace create NAME",
            "Create an empty persistent workspace.",
            "compi workspace create work",
        ),
        ("workspace", Some("rename")) => (
            "workspace rename NAME",
            "Rename the selected workspace without changing its ID.",
            "compi --workspace work workspace rename project",
        ),
        ("workspace", Some("close")) => (
            "workspace close",
            "Close the workspace. Running processes and unsaved edits in open windows require interactive confirmation; scripts must explicitly terminate panes and save files first.",
            "compi --workspace work workspace close",
        ),
        ("tab", Some("list")) => (
            "tab list",
            "List tabs in the selected workspace; outside shell context an omitted workspace lists all tabs.",
            "compi --workspace work --json tab list",
        ),
        ("tab", Some("open")) => (
            "tab open [--name NAME] [--cwd PATH]",
            "Open a tab with the configured shell, in the selected workspace.",
            "compi --workspace work tab open --name build --cwd /home/me/project",
        ),
        ("tab", Some("rename")) => (
            "tab rename NAME",
            "Rename the selected tab, retaining its stable ID.",
            "compi --tab work/build tab rename tests",
        ),
        ("tab", Some("focus")) => (
            "tab focus",
            "Focus the existing tab in its native window; does not steal its terminal attachment.",
            "compi --tab work/tests tab focus",
        ),
        ("tab", Some("move")) => (
            "tab move --index N [--workspace DEST]",
            "Reorder the selected tab at zero-based index N, or move it to another workspace. --tab identifies the source independently of destination --workspace.",
            "compi --tab work/tests tab move --workspace archive --index 0",
        ),
        ("tab", Some("detach")) => (
            "tab detach",
            "Transfer the selected tab's existing view to a new native window.",
            "compi --tab work/tests tab detach",
        ),
        ("tab", Some("close")) => (
            "tab close",
            "Close the selected tab, confirming interactively if any process is running or an open window holds unsaved edits in its content panes.",
            "compi --tab work/tests tab close",
        ),
        ("tab", Some("merge")) => (
            "tab merge --with SOURCE... [--with SOURCE...] [--layout PRESET]",
            "Merge source tabs from the same workspace into the selected tab, preserving shells and pane IDs. An optional built-in or named preset arranges the combined panes.",
            "compi --tab work/build tab merge --with work/tests --layout grid",
        ),
        ("tab", Some("unmerge")) => (
            "tab unmerge",
            "Restore the surviving tabs recorded by a merge, retaining stable IDs.",
            "compi --tab work/build tab unmerge",
        ),
        ("pane", Some("list")) => (
            "pane list",
            "List each pane's ID and type: terminal panes with surface IDs, process state and fresh runtime metadata; content panes with the folder and files they show. Narrow with --workspace, --tab or --pane.",
            "compi --tab work/build --json pane list",
        ),
        ("pane", Some("show")) => (
            "pane show",
            "Show the selected pane and fresh runtime metadata without attaching or resizing.",
            "compi --pane pane-id --json pane show",
        ),
        ("pane", Some("focus")) => (
            "pane focus",
            "Focus the pane in its existing native window.",
            "compi --pane pane-id pane focus",
        ),
        ("pane", Some("resize")) => (
            "pane resize [--cols N] [--rows N]",
            "Resize the selected terminal pane's native presentation. At least one dimension is required; the omitted dimension retains its actual current value. Minimum 20 columns and 4 rows. Content panes are not sized in cells; resize a terminal beside one instead.",
            "compi --pane pane-id pane resize --cols 100",
        ),
        ("pane", Some("swap")) => (
            "pane swap --with OTHER",
            "Exchange two panes in the same tab without restarting processes.",
            "compi --pane pane-id pane swap --with other-pane-id",
        ),
        ("pane", Some("zoom")) => (
            "pane zoom",
            "Maximize the pane within its tab's native presentation.",
            "compi --pane pane-id pane zoom",
        ),
        ("pane", Some("unzoom")) => (
            "pane unzoom",
            "Restore the tab's normal pane presentation.",
            "compi --pane pane-id pane unzoom",
        ),
        ("pane", Some("float")) => (
            "pane float",
            "Float the existing pane in its tab.",
            "compi --pane pane-id pane float",
        ),
        ("pane", Some("dock")) => (
            "pane dock",
            "Return a floating pane to its existing docked location.",
            "compi --pane pane-id pane dock",
        ),
        ("pane", Some("close")) => (
            "pane close",
            "Remove the selected pane, confirming interactively before ending a running process or discarding a content pane's unsaved edits in an open window.",
            "compi --pane pane-id pane close",
        ),
        ("pane", Some("terminate")) => (
            "pane terminate",
            "Explicitly terminate this terminal pane's process lifetime; preserve pane and surface IDs.",
            "compi --pane pane-id pane terminate",
        ),
        ("pane", Some("send")) => (
            "pane send (--text TEXT | --key KEY)",
            "Send literal text with no implicit Enter, or one explicit key, to a terminal pane. Keys: enter, tab, escape, space, backspace, arrows, home/end, insert/delete, pageup/pagedown, f1..f12; ctrl-/alt-/shift- modifiers supported.",
            "compi --pane pane-id pane send --text 'echo hello'; compi --pane pane-id pane send --key enter",
        ),
        ("pane", Some("capture")) => (
            "pane capture [--scope screen|scrollback] [--format text|ansi]",
            "Capture a terminal pane observationally: screen is the default; scrollback includes retained history and the current screen. Text is the default; ANSI preserves text styling and hyperlinks. Soft wraps join, hard breaks and spaces remain. Never attaches or resizes.",
            "compi --pane pane-id pane capture --scope scrollback --format ansi",
        ),
        ("split", _) => (
            "split [--right | --down] [--cwd PATH]\n       compi split --panes TOTAL --layout PRESET [--cwd PATH]",
            "Split the selected terminal pane, or fill the selected tab to TOTAL panes and arrange it. Reuses all existing shells and IDs; a lower total is rejected before mutation. Built-ins and named configured presets are accepted.",
            "compi split --panes 4 --layout grid",
        ),
        ("files", _) => (
            "files [DIR]",
            "Browse DIR (default: the shell's current directory) in the selected terminal's own pane, in that terminal's environment (host or its WSL distribution). The Files view replaces what the pane shows; the shell keeps running underneath at its size, and Cmd/Ctrl+Shift+E or Esc returns to it. Relative DIR resolves against the shell's directory; . and .. are resolved without touching the filesystem. Files the view already shows stay open as tabs. From a content pane (--pane), that pane shows DIR.",
            "compi files ../docs",
        ),
        ("edit", _) => (
            "edit [FILE[:LINE[:COL]]]",
            "Open FILE in the selected terminal's Files view, in place of the shell, and put the cursor on LINE (default 1; COL is accepted and ignored). The view keeps the files it has open; its folder stays when it holds FILE, else becomes the shell's directory when that holds FILE, else FILE's folder. From a content pane (--pane), that pane opens FILE and keeps its folder. A missing or unreadable FILE is reported by the editor. An existing folder without :LINE, or no FILE, browses it like files. With no Compi window showing the tab the view still opens and a note says it was not shown.",
            "compi edit src/main.rs:42",
        ),
        ("notes", Some("search")) => (
            "notes search QUERY [FOLDER]",
            "Search Markdown contents, filenames and heading titles under FOLDER (default: the pane's current directory), without a GUI. Matches are case-insensitive and print FILE:LINE:TEXT. --json returns root, environment, query, hits, partial and warnings. Unreadable files and scan limits are visible warnings, not silently complete results.",
            "compi --json notes search 'project plan' ./notes",
        ),
        ("notes", Some("backlinks")) => (
            "notes backlinks FILE [FOLDER]",
            "List source lines containing wiki links to FILE in FOLDER (default: the pane's current directory), without a GUI. FILE resolves relative to the notebook root, with optional Markdown extension and unique basename/title fallback; ambiguous or missing targets fail. [[target#heading|label]] links count, but inline, fenced and indented code do not. --json returns root, environment, file, hits, partial and warnings.",
            "compi --json notes backlinks decisions ./notes",
        ),
        ("notes", _) => (
            "notes [FILE|FOLDER] [--edit]",
            "Open a Markdown FILE in the existing Files/editor view in Read mode (formatted); --edit opens it in Edit mode instead. Either becomes the file's remembered mode in that window. A FOLDER, or no argument, browses like compi files. Paths resolve in the selected pane's environment against its current directory, like edit. Notes remain ordinary .md/.markdown files, with no notebook database or persisted index. Notebook search/backlinks recursively read at most 4,096 notes, 2,048 directories, 32,768 entries, 64 directory levels and 32 MiB of text, with a 15-second scan budget; .git, target and node_modules are skipped and directory symlinks are not followed. Incomplete scans report warnings.",
            "compi notes ./notes/plan.md",
        ),
        ("diff", _) => (
            "diff [PATH[:LINE[:COL]]]\n       compi diff COMMIT [PATH[:LINE[:COL]]]",
            "Show PATH's uncommitted change as a diff tab in the selected terminal's Files view, in place of the shell, scrolled to LINE of the new side (COL is accepted and ignored). The unstaged change (work tree against the index) wins over the staged one (index against HEAD). PATH resolves like edit; the view's folder stays when it lies in the repository and holds PATH, else becomes the repository's top level. From a content pane (--pane) inside the repository, that pane shows the diff. Without PATH, the repository of the shell's directory lists its changes in the view's Changes sidebar. With COMMIT (a revision such as HEAD~1, a branch or an id), the diff is what that commit changed in PATH against its first parent, read-only; without PATH its changed files show in a section of their own at the top of the Changes sidebar. A lone argument is PATH when that file has uncommitted changes, else COMMIT when Git knows it. --json adds repo, commit (the full id), short and summary for a commit, with path and line, or changes (the count). Exits 1 with not_a_repository outside a Git work tree, unknown_commit when COMMIT names no commit, and no_changes when PATH has no uncommitted change (or names no commit) or COMMIT did not change PATH. Git is only read: files, the index and other Git state never change.",
            "compi diff HEAD~1 src/main.rs",
        ),
        ("settings", _) => (
            "settings [SECTION]",
            "Show Settings in the selected terminal's own pane, in place of the shell, which keeps running underneath at its size; Cmd/Ctrl+Shift+E or Esc returns to it. SECTION is appearance, interface, terminal, keyboard, monitor, updates or advanced; without it the pane keeps the section it shows, else Appearance. From a Settings pane (--pane), that pane changes section.",
            "compi settings terminal",
        ),
        ("monitor", Some("totals")) => (
            "monitor totals [--history 1m|5m|15m|1h] [--source SOURCE]",
            "Print the host's totals from the daemon, with no window: CPU, memory used and available, swap, GPU, disk read/write and network down/up of the newest sample. --history adds every sample of that span ending at the newest (one every 2 seconds, up to an hour). --source host|wsl:NAME reads that source instead of the daemon's host (see compi monitor sources); an unknown one exits 2 with unknown_source. --json returns {source, host, samples} with the protocol's source, host info and samples, oldest first; only the newest carries the per-core, volume and interface detail. Exits 1 with no_sample when the daemon has sampled nothing within 5 seconds.",
            "compi --json monitor totals --history 5m",
        ),
        ("monitor", Some("list")) => (
            "monitor list [--group] [--search TEXT] [--source SOURCE]",
            "Print the host's processes from the daemon, with no window, busiest first as Monitor's Processes table sorts them: PID, name, CPU (share of the whole machine), memory and user. Waits for the first sample with CPU values (about half a second). --search keeps processes whose name, command line, executable, user or PID contains every word, ignoring case. --group shows one row per app with its process count and sums; a search naming the app keeps all its processes. --source host|wsl:NAME lists that source's processes instead of the daemon's host (see compi monitor sources); an unknown one exits 2 with unknown_source. --json returns {source, sampled_at_ms, logical_cpus, apps_known, processes} (each process with its details and cpu_share) and, with --group, groups (id, name, pids and sums). Exits 1 with no_sample when no sample arrives within 5 seconds.",
            "compi monitor list --group --search safari",
        ),
        ("monitor", Some("sources")) => (
            "monitor sources",
            "List what Monitor can watch from the daemon, one per line as --source names it: host first, then, on a Windows daemon, wsl:NAME for each WSL 2 distribution. --json returns {sources} in the protocol's shape ({\"type\":\"host\"}, {\"type\":\"wsl\",\"distribution\":NAME}).",
            "compi monitor sources",
        ),
        ("monitor", Some("inspect")) => (
            "monitor inspect PID [--source SOURCE]",
            "Print one process from a sample of the daemon's host (or --source host|wsl:NAME), with what the sample leaves out: name, PID, start time and how long it has run, user, state, working directory, executable, every argument, parent, CPU (share of the whole machine), memory and, when it runs in a Compi terminal, that terminal's surface and pane. --json returns the process as monitor list gives it, plus source, sampled_at_ms, inspection {state, working_directory, arguments, arguments_cut}, surface and pane_id. Exits 1 with no_process when no process has that PID, process_gone when it exits before it is read, and 2 with unknown_source for an unknown source.",
            "compi monitor inspect 4120",
        ),
        ("monitor", Some("quit")) => (
            "monitor quit PID [--force] [--source SOURCE]",
            "Ask a process of the daemon's host (or --source host|wsl:NAME) to quit, as Monitor's Quit does: SIGTERM, or on Windows a close request to its windows. --force ends it at once (SIGKILL, or TerminateProcess), and unsaved work in it is lost. Asks first, naming the process and whether it runs in a Compi terminal; without a terminal to ask on it exits 4 with confirmation_required, and there is no bypass. The daemon signals only the process the PID named when it was looked up, never its group. Prints what was sent; --json returns {source, pid, start_time, name, force, outcome}. Exits 1 with no_process (no such PID), process_gone (it exited, or the PID was reused, first), permission_denied, no_window (Windows: nothing to close; use --force) or refused (the daemon will not signal it, with the reason); 2 with unknown_source for an unknown source; 4 with cancelled when the answer is not yes.",
            "compi monitor quit 4120",
        ),
        ("monitor", _) => (
            "monitor [PAGE] [--source SOURCE]\n       compi monitor totals [--history RANGE] [--source SOURCE]\n       compi monitor list [--group] [--search TEXT] [--source SOURCE]\n       compi monitor sources\n       compi monitor inspect PID [--source SOURCE]\n       compi monitor quit PID [--force] [--source SOURCE]",
            "Show Monitor in the selected terminal's own pane, in place of the shell, which keeps running underneath at its size; Cmd/Ctrl+Shift+E or Esc returns to it. PAGE is overview, processes, cpu, memory, gpu, disk or network; without it the pane keeps the page it shows, else Overview. --source host|wsl:NAME shows that source (see compi monitor sources); without it the pane keeps the source it shows, else the daemon's host. From a Monitor pane (--pane), that pane changes page or source. monitor totals, list and inspect print Monitor's data for scripts with no window, monitor sources lists the sources and monitor quit quits a process; see each one's --help.",
            "compi monitor processes",
        ),
        ("browse", _) => (
            "browse [URL] [--down]",
            "Open URL in a Browser pane beside the selected terminal (below it with --down); the shell stays as it is. URL is an http, https or file URL, or about:blank; without a scheme, http:// is assumed, so localhost:3000 opens http://localhost:3000. Text pasted around a link (quotes, <...>, a Markdown link, a Local: prefix, trailing punctuation) is dropped. Without URL the pane opens on its home page (local servers your terminals printed, recent sites) with the keyboard in its address field, where a link can be pasted without shell quoting. From a Browser pane (--pane), that pane goes to URL. The page is the system web view's (WebView2 on Windows, WKWebView on macOS), with one persistent profile per instance. --json returns {pane_id, url, shown, not_shown, receipt, workspace}.",
            "compi browse 'localhost:3000/search?q=x'",
        ),
        ("command", Some("run")) => (
            "command run ID",
            "Run the palette command ID (see command list) in the Compi window that shows the selected pane's tab (--window selects another), with that pane focused first, as if chosen from the palette there. A command that asks first, such as Close pane, opens its dialog in the window; there is no bypass. A command disabled there exits 1 with command_disabled and the palette's reason; an unknown ID exits 2. --json returns {command, name, pane_id, window_id}.",
            "compi command run open_settings",
        ),
        ("command", _) => (
            "command list\n       compi command run ID",
            "List every palette command: its ID, name and this platform's shortcut with configured keybindings applied, with no window needed. --json returns {platform, commands} with each command's id, name, category, shortcut (null when unbound), default_shortcut and description (null when it has none). Run one with command run ID.",
            "compi --json command list",
        ),
        ("layout", _) => (
            "layout list | PRESET | mirror | flip | restore",
            "Arrange the selected tab without restarting processes. Presets come from built-ins and configured layout_presets. Restore returns to the previous arrangement, or restores original tabs immediately after a merge.",
            "compi --tab work/build layout main-side",
        ),
        ("run", _) => (
            "run -- COMMAND [ARG...]",
            "Safely shell-quote argv and explicitly submit Enter to the selected pane. Supported POSIX shells only; alternate-screen applications and argv control characters that could become terminal keys are rejected. Does not wait for command completion.",
            "compi run -- printf '%s\\n' 'hello world'",
        ),
        ("attach", _) => (
            "attach",
            "Interactively attach an unattached pane to this console. An existing GUI or console attachment is never stolen. Press Ctrl-] to detach.",
            "compi --pane pane-id attach",
        ),
        ("detach", _) => (
            "detach",
            "Release a console attachment on the selected pane without ending its process. GUI attachments are not detached; use tab detach for native window transfer.",
            "compi --pane pane-id detach",
        ),
        ("window", Some("list")) => (
            "window list",
            "List native window slot IDs, visible tab IDs, and pane presentation state, with each pane's type (terminal or content), the unsaved_files of a content pane or a terminal's Files view, and a Browser pane's page (url, title, the loading address and the error of a failed load), for the selected connection.",
            "compi --instance dev --json window list",
        ),
        ("theme", Some("install")) => (
            "theme install FILE.json",
            "Validate and install a theme family in the local app library without launching a window. --connect is rejected. The WSL shell bridge resolves Linux files against the invoking shell's directory.",
            "compi theme install ./my-theme.json",
        ),
        ("update", _) => (
            "update [--check]",
            "Check for a newer Compi. --check only reports it. Otherwise download and verify it, list the shells a restart would end (marking this shell), and ask Restart now? [y/N]; only y restarts. Without an interactive terminal it stops after the download with exit 4; restart from a terminal or Settings → Updates. A running Compi window performs the restart, saving and reopening its windows; with none open, Compi opens first. --connect is rejected; there is no --yes/--force bypass.",
            "compi update --check",
        ),
        ("changes", _) => (
            "changes",
            "Print the running version and what changed in it, from the notes What's New shows: the headline, the other highlights, then each section under its title. Needs no running Compi. --connect is rejected.",
            "compi --json changes",
        ),
        _ => (
            "[GLOBAL OPTIONS] COMMAND [ARGS]",
            "Commands: workspace list/show/create/rename/close; tab list/open/rename/focus/move/detach/close/merge/unmerge; pane list/show/focus/resize/swap/zoom/unzoom/float/dock/close/terminate/send/capture; window list; split; files; edit; notes [FILE|FOLDER]/search/backlinks; diff; settings; monitor [PAGE]/totals/list/sources/inspect/quit; browse URL; command list/run; layout; run; attach; detach; theme install; update; changes. compi --version prints the version; compi changes prints what changed in it. Bare compi and existing GUI flags continue to open the native application. Use COMMAND ACTION --help for individual help.",
            "compi --instance dev --json workspace list",
        ),
    };
    format!(
        "Usage: compi {usage}\n\n{detail}\n\nTargets: --instance NAME | --connect [USER@]HOST[:PORT], --workspace ID|NAME, --tab ID|WORKSPACE/NAME, --pane ID|SURFACE_ID, --window ID. --surface ID sets shell context. --config PATH selects configuration. --json emits structured results/errors.\nInside a Compi shell, COMPI_INSTANCE and COMPI_SURFACE_ID supply context. Outside, only an existing daemon is used; multiple instances or name matches require an explicit target (--instance '' selects the default instance). Close has no --yes/--force bypass.\nExit codes: 0 success, 1 operation/transport failure, 2 invalid arguments, 3 missing/ambiguous target, 4 confirmation required/cancelled, 5 daemon/attachment unavailable, 6 concurrent revision/generation/lifetime conflict.\n\nExample: {example}\n"
    )
}

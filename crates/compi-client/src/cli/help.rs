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
            "Close the workspace. Running processes require interactive confirmation; scripts must explicitly terminate panes first.",
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
            "Close the selected tab, confirming interactively if any process is running.",
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
            "List pane IDs, surface IDs, process state and fresh runtime metadata; narrow with --workspace, --tab or --pane.",
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
            "Resize the selected pane's native presentation. At least one dimension is required; the omitted dimension retains its actual current value. Minimum 20 columns and 4 rows.",
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
            "Remove the selected pane, confirming interactively before ending a running process.",
            "compi --pane pane-id pane close",
        ),
        ("pane", Some("terminate")) => (
            "pane terminate",
            "Explicitly terminate this pane's process lifetime; preserve pane and surface IDs.",
            "compi --pane pane-id pane terminate",
        ),
        ("pane", Some("send")) => (
            "pane send (--text TEXT | --key KEY)",
            "Send literal text with no implicit Enter, or one explicit key. Keys: enter, tab, escape, space, backspace, arrows, home/end, insert/delete, pageup/pagedown, f1..f12; ctrl-/alt-/shift- modifiers supported.",
            "compi --pane pane-id pane send --text 'echo hello'; compi --pane pane-id pane send --key enter",
        ),
        ("pane", Some("capture")) => (
            "pane capture [--scope screen|scrollback] [--format text|ansi]",
            "Capture observationally: screen is the default; scrollback includes retained history and the current screen. Text is the default; ANSI preserves text styling and hyperlinks. Soft wraps join, hard breaks and spaces remain. Never attaches or resizes.",
            "compi --pane pane-id pane capture --scope scrollback --format ansi",
        ),
        ("split", _) => (
            "split [--right | --down] [--cwd PATH]\n       compi split --panes TOTAL --layout PRESET [--cwd PATH]",
            "Split the selected pane, or fill the selected tab to TOTAL panes and arrange it. Reuses all existing shells and IDs; a lower total is rejected before mutation. Built-ins and named configured presets are accepted.",
            "compi split --panes 4 --layout grid",
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
            "List native window slot IDs, visible tab IDs, and pane presentation state for the selected connection.",
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
            "Commands: workspace list/show/create/rename/close; tab list/open/rename/focus/move/detach/close/merge/unmerge; pane list/show/focus/resize/swap/zoom/unzoom/float/dock/close/terminate/send/capture; window list; split; layout; run; attach; detach; theme install; update; changes. compi --version prints the version; compi changes prints what changed in it. Bare compi and existing GUI flags continue to open the native application. Use COMMAND ACTION --help for individual help.",
            "compi --instance dev --json workspace list",
        ),
    };
    format!(
        "Usage: compi {usage}\n\n{detail}\n\nTargets: --instance NAME | --connect [USER@]HOST[:PORT], --workspace ID|NAME, --tab ID|WORKSPACE/NAME, --pane ID|SURFACE_ID, --window ID. --surface ID sets shell context. --config PATH selects configuration. --json emits structured results/errors.\nInside a Compi shell, COMPI_INSTANCE and COMPI_SURFACE_ID supply context. Outside, only an existing daemon is used; multiple instances or name matches require an explicit target (--instance '' selects the default instance). Close has no --yes/--force bypass.\nExit codes: 0 success, 1 operation/transport failure, 2 invalid arguments, 3 missing/ambiguous target, 4 confirmation required/cancelled, 5 daemon/attachment unavailable, 6 concurrent revision/generation/lifetime conflict.\n\nExample: {example}\n"
    )
}

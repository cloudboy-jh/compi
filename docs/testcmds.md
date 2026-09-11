# Compi Windows terminal test recipes

These recipes describe the existing Windows/WSL implementation and its previous acceptance process, plus the native Unix/Mac bring-up commands below. They are retained as regression inputs, not the complete cross-platform acceptance matrix or current milestone ordering. The authoritative requirements are in [Spec.md](Spec.md); [Next steps](NEXT_STEPS.md) records implementation evidence and remaining native qualification.

The procedures below use the current Windows commands and terminology. Use release builds for scripted and interactive checks and an isolated daemon instance for destructive lifecycle checks. Historical dimensions, limits, and artifact assumptions must be revisited when their implementation contracts change.

## Test environment record

Record Windows build, WSL distribution and version, display scale, monitor refresh rate, Compi commit, build profile, and whether a warm daemon already existed. Run display checks at 100%, 150%, and the machine's normal scale when available.

## Phase 1 dependency and compatibility checks

The three product crates can be tested together on macOS, Linux, or Windows:

```text
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client
cargo test --locked -p compi-daemon --test terminal_compatibility
```

On native Windows, verify the headless build separately from application/installer setup:

```powershell
python tools/check-dependencies.py --target x86_64-pc-windows-msvc
cargo build --locked --release -p compi-daemon --bin compi-daemon
wsl.exe --exec true
cargo test --locked -p compi-daemon --test daemon_integration
```

The daemon build does not require GPUI or `GPUI_FXC_PATH`. A failed WSL readiness check is missing runtime coverage, not a reason to count skipped daemon tests as passing. The commands in Tier 1 below require a working default WSL distribution for the full integration suite.

## Phase 2 native Unix and Mac checks

On macOS/Linux, build and exercise the real headless daemon without graphics dependencies:

```sh
cargo build --locked -p compi-daemon --bin compi-daemon
cargo build --locked -p compi-client --example compi-probe
./target/debug/compi-daemon --check-system
cargo test --locked -p compi-daemon --test unix_daemon_integration
```

On a Mac with Rust and Xcode/Metal tooling:

```sh
cargo build --locked -p compi-client -p compi-daemon --bins
./target/debug/compi --instance development
```

In the window, record `echo $$`, set a shell variable, run Vim or another fullscreen program, and resize. Close the native window, then rerun the same command: the same live session/PID/variable must remain. Check Cmd-T/W/C/V, Ctrl-C, Option/dead keys, IME composition, traffic lights, titlebar dragging, font fallback, and scaling physically. The initial Mac pass verified text/resize/close callbacks through AppKit debugger injection, not the full physical-input matrix.

An explicit `--working-directory /absolute/project/path` requests new work. Omit that option on ordinary reopen. Use an isolated instance for termination and crash scenarios; `compi-probe --instance development shutdown` intentionally ends all work in that instance.

CI is configured to run the Unix integration suite natively on Mac/Linux. Windows/WSL tests still require a qualified Windows host. The Windows immediate-descendant ownership regression is part of `cargo test --locked -p compi-daemon --lib`; compiling it on another host does not qualify the retained ConPTY backend.

## Phase 4 native workspace checks

Use matching v9 client/daemon binaries and an isolated instance. Do not stop an older daemon that owns valuable work merely to try the new client.

```powershell
cargo test --locked --workspace --all-targets --release
cargo build --locked --release --workspace --bins --examples
.\target\release\compi.exe --instance phase4
```

Set `GPUI_FXC_PATH` as described below. First launch creates one shell; later windows/relaunches do not repair hidden or intentionally empty work by creating replacement shells.

1. With the sidebar closed, create/rename/reorder terminal tabs. Open the sidebar explicitly, create/switch a workspace, and verify the top tabs remain primary. Hide and restore a tab without changing its process.
2. Build nested Split right/down layouts. Drag a divider repeatedly within 32 ms intervals, including while PTY resize metadata arrives. The final ratio must commit without a GPUI panic or self-generated revision conflict.
3. Shrink the window with the sidebar open. Every pane keeps at least a 20-column by 4-row canvas; workspace scrolling and directional focus reach clipped panes. Expanding restores saved ratios; visibility/width changes do not rewrite the tree.
4. Tear a three-pane tab into a new window, cancel another drag with Escape, then drop into an existing window. Verify the same shell PIDs/variables and split tree; the destination retains its own appearance. Moving the final tab leaves an empty source view.
5. Launch the same instance from another process. It should create another native window in the existing GUI host, not report a false closed-pipe error or launch another shell. Exercise attachment conflicts without takeover.
6. Open Quick Appearance and Settings with `Ctrl+,`. Drag the continuous opacity slider to 70%, set global Dark Glass, and exercise each clear/blurred effect; relaunch and verify TOML persistence. Switch to **This window**, choose Warm Carbon, and confirm global TOML is unchanged while private JSON contains only the explicit theme field. **Use global defaults** must remove the window appearance object.
7. Restart an idle isolated daemon without confirmation. With live work, verify the review names every affected surface, Escape preserves the same responsive shell, and **End all work and restart daemon** leaves truthful lost surfaces that can be explicitly restarted.
8. Select text in a static terminal workload, close, and reopen at the same geometry. Selection/scroll anchors restore only from the identical authoritative baseline. Changed output, lifetime, generation, or geometry invalidates uncertain anchors. The sidebar starts closed regardless of its previous visibility.
9. Flood one pane with `yes` while typing/navigating another. End a surface with an owned background child, inspect its final output, restart explicitly, and remove a pane/tab/workspace with the appropriate confirmation.
10. In an isolated state directory, exercise slot contention and failed atomic state writes. Failed window transfer retains the source; failed preference saving remains visibly unsaved without disabling the live presentation; a later successful save clears that warning.

Windows shortcuts: Ctrl-T/W, Ctrl-Tab / Ctrl-Shift-Tab, Ctrl-Shift-T restore, Ctrl-Plus/Minus font zoom, Ctrl-0 zoom reset, Ctrl-Comma Settings, Alt-Shift-Plus/Minus split right/down, Alt-Arrow pane focus, Alt-Shift-Arrow pane resize, Ctrl-Shift-W remove pane, Ctrl-Shift-B sidebar, Ctrl-Shift-P palette. Mac: Cmd-T/W, Cmd-D / Cmd-Shift-D, Cmd-B, Cmd-Shift-P, Cmd-Comma Settings. Physical input, IME, scaling/display pacing, and native Mac execution require separate attributed qualification; synthetic Win32 input is not that proof.

## Tier 1: automated regression

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release --bins
```

Release builds of GPUI require the Windows SDK shader compiler. Resolve the newest installed x64 compiler and pass its executable path through `GPUI_FXC_PATH`:

```powershell
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$fxc = Get-ChildItem -Path $sdkRoot -Filter fxc.exe -File -Recurse |
  Where-Object { $_.FullName -match '\\x64\\fxc\.exe$' } |
  Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
  Select-Object -First 1
if (-not $fxc) { throw "fxc.exe was not found below $sdkRoot" }
$env:GPUI_FXC_PATH = $fxc.FullName
```

`GPUI_FXC_PATH` must name `fxc.exe`, not its containing directory. Native Windows builds and the tag-triggered release workflow use this discovery rule.

Required result: every command exits zero. The release directory contains `compi.exe` and `compi-daemon.exe`; it does not contain `compi-probe.exe`.

## Tier 2: scripted terminal and performance checks

Start an isolated daemon:

```powershell
.\target\release\compi-daemon.exe --instance acceptance
```

In a second PowerShell window, launch the GUI against that instance:

```powershell
.\target\release\compi.exe --instance acceptance
```

Use `.\target\release\examples\compi-probe.exe --instance acceptance ...` only if the explicitly built development example is needed:

```powershell
cargo build --release --example compi-probe
.\target\release\examples\compi-probe.exe --instance acceptance create
```

Run these commands inside an attached 100x30 Compi terminal. Capture pass/fail, elapsed time, peak private bytes, peak working set, peak handles, and any visible corruption.

| Case | Command/action | Required result |
|---|---|---|
| Count and resize | `for i in $(seq 1 100); do echo "$i"; sleep 0.1; done` while resizing | Every integer appears once, in order; no gaps, overlap, or stale cells. |
| Sustained output | `timeout 10s yes '0123456789 abcdefghijklmnopqrstuvwxyz'` | Client stays responsive; prompt returns; scrollback remains coherent and bounded. |
| Large corpus | `seq 1 100000 > /tmp/compi-100k.txt; time cat /tmp/compi-100k.txt` | Final line is `100000`; no parser hang or unbounded memory/handle growth. Earlier lines may be evicted by the 1 MiB scrollback cap. |
| Rapid cancel | `yes 'COMPI-CANCEL'` then `Ctrl+C` after three seconds | Output stops promptly; one clean prompt appears; no partial escape sequence. |
| Wide wrap | `printf '=%.0s' {1..500}; echo` | Text wraps at terminal width without missing or duplicated cells. |
| Combining marks | `printf 'e\u0301 a\u0308 n\u0303\n'` | Decomposed graphemes remain attached to their base character. |
| Wide characters | `printf '你好世界  A\n'` | CJK cells occupy two columns and the `A` remains aligned. |
| Box drawing | `printf '\u250c\u2500\u2510\n\u2514\u2500\u2518\n'` | Corners and horizontal lines align without gaps. |
| 256 color | `for i in {0..255}; do printf '\033[38;5;%sm%03d ' "$i" "$i"; done; printf '\033[0m\n'` | All indices render and attributes reset at the prompt. |
| Truecolor | `printf '\033[38;2;255;100;50mtruecolor\033[0m normal\n'` | First word is RGB-colored; `normal` and the prompt use default foreground. |
| Clear display | `printf '\033[2J\033[Hclear-display\n'` | Visible screen clears and text begins at row 1, column 1. |
| Clear scrollback | Produce 100 lines, then `printf '\033[3J'` | Scrolling up cannot reveal the prior 100 lines. |
| Alternate screen | `printf '\033[?1049hALT\n'; sleep 2; printf '\033[?1049l'` | `ALT` disappears and the exact main screen returns. |
| Fullscreen TUI | Open Vim, edit, and exit | Alternate-screen content fills the terminal canvas, input is responsive, history indicators stay hidden, and the exact main screen returns without stale rows. |
| Terminal transparency | In Settings select 70%, then compare Clear and Blurred over a detailed background | Clear leaves background edges sharp; Blurred softens them; terminal text and explicit cell backgrounds remain opaque; reduced-transparency or unsupported systems use the opaque fallback. |
| Title | `printf '\033]0;Compi title test\007'` | Native window title and active tab update once, without repaint churn. |
| OSC 52 clipboard | `printf '\033]52;c;%s\a' "$(printf 'compi-osc52' | base64 -w0)"` | Clipboard contains `compi-osc52`; this is clipboard write, not bracketed paste. |
| Resize stream | `watch -n 0.2 date` while continuously resizing | TUI redraws cleanly; no garbage rows or crash. |
| Resize flood | Rapidly drag a corner for 15 seconds | Final grid matches final window size; client and daemon remain alive. |
| Escape flood | `timeout 10s sh -c 'while :; do printf "\033[31mX\033[0m"; done'` | Parser remains responsive; attributes do not bleed into prompt. |
| 10 KiB paste | Paste a deterministic 10 KiB ASCII block into `wc -c` input, then `Ctrl+D` | Reported byte count matches the source exactly. |
| Reattach under output | Start a build or count loop, close Compi, wait, reopen and attach | Process never stops; current screen and subsequent output are coherent. |

For ad hoc performance sampling, launch the release client with opt-in instrumentation:

```powershell
$env:COMPI_PERF_LOG = '1'
$env:COMPI_PERF_SAMPLE = 'manual-01'
.\target\release\compi.exe --instance acceptance
```

Instrumentation writes:

- `%LOCALAPPDATA%\Compi\client-startup.log`: daemon connection, first window, first terminal frame, and optional ready-probe timing;
- `%LOCALAPPDATA%\Compi\client-resource-<pid>.log`: six-second client private bytes, working set, handles, workload, and attached-tab count;
- `%LOCALAPPDATA%\Compi\daemon-resource-<pid>.log`: six-second daemon private bytes, working set, handles, and session count;
- `%LOCALAPPDATA%\Compi\client-perf.log`: frame-interval and terminal-paint distributions under active output.
- `%LOCALAPPDATA%\Compi\latency-<pid>.log`: correlated input IDs at GPUI receipt, client queue/send, daemon receipt, PTY output, terminal-state sequence, and the next presented frame.

Set `COMPI_PERF_EMPTY_WINDOW=1` with `COMPI_PERF_LOG=1` to measure a blank GPUI window without connecting to a daemon. Set `COMPI_PERF_READY_PROBE=1` only against an isolated measurement session; it sends a deterministic `printf` command and measures both launch-to-rendered-marker and input-to-rendered-marker time.

For deterministic terminal debugging, set `COMPI_TERMINAL_TRACE_DIR` on an isolated daemon and optionally set `COMPI_TERMINAL_TRACE_LABEL`. Each session then writes a bounded 16 MiB binary trace containing its initial grid, input bytes, PTY output bytes, resize events, and monotonic timing. Traces can contain commands, credentials, and terminal output; never enable capture for normal or valuable sessions. Regression tests replay captured PTY output through `TerminalState` without WSL or timing dependencies.

The release harness runs empty-window, warm-daemon, and cold-daemon launch samples; measures fresh one-, two-, and four-session client/daemon pairs; queries Windows GPU process-memory counters; and writes CSV plus environment JSON under `%LOCALAPPDATA%\Compi\measurements`:

```powershell
cargo build --release --bins --example compi-probe
.\tools\measure-release.ps1 -Samples 10 -ConfirmPhysicalDisplay
```

`-ConfirmPhysicalDisplay` is an operator assertion. Do not pass it through a virtual display or remote-only session. Without it, the harness intentionally labels the run diagnostic rather than qualified. For longer marginal-session analysis, keep instrumentation active, add one blank session at a time, wait at least six seconds per state, and compare consecutive client and daemon resource records by their `sessions` values.

Terminal-truth performance evidence is:

- correlated input-to-present instrumentation at GPUI key receipt, client queue/send, daemon input receipt, PTY output receipt, terminal-state update with screen sequence, and the next presented frame;
- p50, p95, and worst input-to-present latency from at least 100 interactive samples;
- terminal paint below the available frame budget;
- no monotonic private-byte, GPU-memory, handle, or queue growth during a 30-minute mixed TUI session.

The approximate 376 ms warm first-window, 544 ms warm ready-for-input, 153 ms input-to-render, and 89–96 MiB client-private-memory measurements are architecture baselines, not terminal-truth failures. The old 100 ms startup and 35 MiB client-memory aspirations are not acceptance gates for this sprint.

## Tier 3: interactive client acceptance

| Area | Procedure | Required result |
|---|---|---|
| Shell control | Verify command echo, `Ctrl+D`, `Ctrl+Z`, `bg`, and `fg`; run sustained output and press `Ctrl+C` with no selection. | Bash semantics match a native WSL terminal. With no selection, `Ctrl+C` sends `0x03`, stops output promptly, and returns one clean prompt. |
| Windows keyboard | Type several words and press `Ctrl+Backspace`; repeat copy/paste with `Ctrl+Insert` and `Shift+Insert`. | `Ctrl+Backspace` removes the preceding word, selection copy reaches the clipboard, and paste inserts the exact clipboard contents without an extra newline. |
| TUI applications | Exercise `htop` or `btop`, `vim` or `nvim`, `less`, `tmux`, and `fzf` with inline preview. | Alternate-screen transitions, cursor, mouse, keyboard, and redraw behavior remain correct. |
| Selection and clipboard | Select single-line, wrapped, multiline, CJK, and combining-mark text. Press `Ctrl+C` while a foreground process runs, then repeat with `Ctrl+Shift+C`; paste with both `Ctrl+V` and `Ctrl+Shift+V`. | `Ctrl+C` copies a non-empty selection without sending PTY input or interrupting the process. Both copy shortcuts preserve the exact logical text, both paste shortcuts insert the clipboard, and paste honors bracketed-paste mode. |
| Scrollback resize | Scroll several pages up, resize wider and narrower, then return to bottom. | Viewport stays anchored to the same logical content; no jump to bottom, overlap, or stale cells. |
| Tabs | Create tabs with `Ctrl+T`, cycle with `Ctrl+Tab` and `Ctrl+Shift+Tab`, hide one with `Ctrl+W`, restore it with `Ctrl+Shift+T`, close active and inactive tabs through their close controls, and overflow the available titlebar width. | The shortcuts select the expected tab without leaking input into its shell. The Compi terminal mark remains visible beside the sidebar toggle; fallback labels are concise; close always confirms before ending processes and removing the tab. |
| Panes | Split with `Alt+Shift+Plus` and `Alt+Shift+Minus`; move focus with `Alt+Arrow`; resize with `Alt+Shift+Arrow`; press `Ctrl+Shift+W` on a disposable pane. | Splits open right/down, focus and the divider move in the requested direction, and pane removal requires confirmation without affecting another pane. |
| Session palette | Open the command palette, switch to an open tab, attach a detached session, create a session, and inspect exited/failed sessions. End one attached and one detached session through confirmation. | Every state is represented accurately. End surface terminates the shell and descendants, shows `Ending…`, and retains the final readable grid. |
| Detach versus terminate | Hide an active tab with `Ctrl+W`, close the client with live sessions, reopen and restore the tab; separately use the tab close control and confirm removal. | Hide and client close preserve live sessions. Confirmed tab close terminates its process trees and removes the tab. |
| Window chrome | Drag from the mark, unused header space, and a tab; double-click unused header space; use minimize, maximize/restore, and close. | Native movement starts only after the drag threshold; controls never trigger dragging; maximize and restore match Windows behavior. |
| Persistence | Open two sessions, detach both, close the client, reopen, and attach in reverse order. | Shells keep running; state does not cross between sessions. |
| Failure handling | Abruptly terminate an isolated daemon, restart it, and inspect stale sessions; attempt a second daemon launch. | Stale sessions report dead and cannot attach; second daemon fails clearly; no live production session is affected. |
| Kitty graphics | Test raw RGBA, PNG, JPEG, chunking, compression, placement, clipping, resize, deletion, and reattach. | Image decode never stalls input; placement and deletion are correct; decoded memory is released after deletion. |
| Display behavior | Repeat window and text checks on normal, maximized, narrow, mixed-DPI, and multi-monitor layouts. | No clipped controls, unreadable text, stale scale, or broken hit targets. |
| Soak | For 30 minutes, alternate sustained output, typing, scrolling, tab switches, image display/deletion, detach, and reattach. | No crash, input loss, UI stall, cross-session state, or unbounded CPU, memory, GPU-memory, or handle growth. |

## Destructive cleanup

The following intentionally terminates every session owned by the isolated daemon:

```powershell
.\target\release\examples\compi-probe.exe --instance acceptance shutdown
```

Never run shutdown against the normal daemon instance while valuable sessions are active.

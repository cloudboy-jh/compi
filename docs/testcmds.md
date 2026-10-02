# Compi Windows terminal test recipes

These recipes describe the existing Windows/WSL implementation and its previous acceptance process, plus the native Unix/Mac bring-up commands below. They are retained as regression inputs, not the complete cross-platform acceptance matrix or current milestone ordering. The authoritative requirements are in [Spec.md](Spec.md); [Next steps](NEXT_STEPS.md) tracks remaining work and [Completed work](COMPLETED.md) records implementation evidence.

The procedures below use the current Windows commands and terminology. Use release builds for scripted and interactive checks and an isolated daemon instance for destructive lifecycle checks. Historical dimensions, limits, and artifact assumptions must be revisited when their implementation contracts change.

## Test environment record

Record Windows build, WSL distribution and version, display scale, monitor refresh rate, Compi commit, build profile, and whether a warm daemon already existed. Run display checks at 100%, 150%, and the machine's normal scale when available.

## Phase 1 dependency and compatibility checks

The four workspace crates can be tested together on macOS, Linux, or Windows. On Windows, first run `pwsh -File tools/prepare-conpty.ps1` and verify the default WSL2 guest with `wsl.exe --exec /bin/bash -lc true`; otherwise integration tests may be skipped or fail before exercising a shell:

```text
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo test --locked -p compi-daemon --test terminal_compatibility
```

On native Windows, verify the headless build separately from application/installer setup:

```powershell
python tools/check-dependencies.py --target x86_64-pc-windows-msvc
cargo build --locked --release -p compi-daemon --bin compi-daemon
wsl.exe --exec /bin/bash -lc true
cargo test --locked -p compi-daemon --test daemon_integration -- --test-threads=1
```

The daemon build does not require GPUI or `GPUI_FXC_PATH`. A failed WSL readiness check is missing runtime coverage, not a reason to count skipped daemon tests as passing. The Windows integration cases run one at a time on shared WSL2 runners: simultaneous guest startups have returned `0xffffffff` without diagnostic output, even though individual tests run. The commands in Tier 1 below require a working default WSL distribution for the full integration suite.
The 16 MiB output-backpressure case allows two minutes for shell output to complete on a debug CI runner; ordinary control-path waits remain 30 seconds.

### Installer and in-app update qualification

The bootstrapper is a separate Cargo workspace. Run its behavioral library tests with `cargo test --locked --manifest-path installer/bootstrapper/Cargo.toml --lib -- --test-threads=1`; testing its embedded-MSI binary additionally requires the generated `installer/bootstrapper/payload/Compi.msi`. The packaging script creates and removes that build input.

Use `tools/smoke-windows-distribution.ps1 -Install` only in a disposable Windows account/runner: it exercises real MSI registration, task management, repair and removal, and refuses an existing developer install. Without `-Install` it checks only the portable package. `tools/smoke-macos-bundle.sh` requires a native Apple Silicon Mac. Neither cross-compilation nor library tests qualify installation/uninstallation. The release workflow runs only the portable and copied-bundle smokes.

For a genuine compatible B→C cycle, build both versioned packages with the same disposable verification key and sign C's complete artifact metadata with B's exact daemon version qualified. Compile the development-only `compi-probe` and `compi-update-smoke` examples from B. Point the cycle script only at an extracted disposable B root carrying the smoke ownership receipt, never the live installation:

```text
python tools/smoke-update-cycle.py --root B_ROOT --artifact C_FULL_PACKAGE --metadata C_SIGNED_METADATA --probe B_PROBE --driver B_UPDATE_SMOKE --evidence EVIDENCE_DIRECTORY
```

The script launches the real GUI and shell, imports a managed custom theme, checks signature/hash rejection and cancellation, injects a missing restore handoff, requires a real old-build rollback attachment, then requires a real C attachment to the original B daemon. It verifies shell PID/cwd/environment/output continuity, workspace/surface/process-lifetime identity, logical launch selection, and retained config/theme/external-source bytes. It creates only isolated profiles and shuts down only its named instance.

Record artifact/metadata hashes, actual readiness receipts and native images. Also qualify multiple windows, incompatible-update consent, interrupted downloads, permissions/disk-full failures, actual MSI cancellation/rollback, default keep-data uninstall/reinstall, explicit managed-data removal, and macOS bundle/quarantine/Gatekeeper paths separately. Do not infer those results from the compatible single-window cycle.


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

## SSH and headless checks

Build the local development probe and install a matching `compi-daemon` on the SSH host (on its `PATH`). On macOS/Linux, exercise discovery and lifecycle without GPUI:

```sh
cargo build --locked -p compi-client --example compi-probe
./target/debug/examples/compi-probe --connect user@host:22 --instance qualification workspace
./target/debug/examples/compi-probe --connect user@host:22 --instance qualification start
./target/debug/examples/compi-probe --connect user@host:22 --instance qualification workspace
SURFACE_ID='paste-id-from-workspace'
./target/debug/examples/compi-probe --connect user@host:22 --instance qualification surface inspect "$SURFACE_ID"
```

Replace `user@host:22` with a reachable OpenSSH destination and assign `SURFACE_ID` from the second `workspace` listing after detaching the interactive `start` with Ctrl+]; do not terminate the shell. On Windows, use `.\target\debug\examples\compi-probe.exe` instead of `./target/debug/examples/compi-probe`. Closing or detaching the probe must leave the remote surface running. A later `workspace` or `surface attach` must report the same server, surface, and process-lifetime IDs. Use `surface end` with that ID for one process and `shutdown` only for a disposable instance. Unknown host keys, authentication failures, missing remote binaries, and dropped relays must fail visibly; Compi does not bypass OpenSSH policy or fall back to a local daemon.


## Current local portable build

From the repository root on Windows, prepare the runtime and build a matching release pair. Set `GPUI_FXC_PATH` to your Windows SDK's x64 `fxc.exe` if it is not already discoverable:

```powershell
pwsh -File tools/prepare-conpty.ps1
cargo build --locked --release --target-dir target/compi-latest -p compi-client -p compi-daemon --bins
New-Item -ItemType Directory -Force target/compi-latest/portable | Out-Null
Copy-Item target/compi-latest/release/compi.exe target/compi-latest/portable/compi-latest.exe
Copy-Item target/compi-latest/release/compi-daemon.exe,target/compi-latest/release/conpty.dll,target/compi-latest/release/OpenConsole.exe,target/compi-latest/release/ConPTY-LICENSE.txt,LICENSE target/compi-latest/portable/
```

Keep the runtime beside the daemon. Launch under a separate instance to avoid the installed default daemon:

```powershell
& '.\target\compi-latest\portable\compi-latest.exe' --instance compi-latest-local
```

## Phase 4 native workspace checks

Use matching protocol 14 client/daemon binaries and an isolated instance. Set `GPUI_FXC_PATH` as described in Tier 1 before building the Windows client. Do not stop an older daemon that owns valuable work merely to try the new client.

```powershell
cargo test --locked --workspace --all-targets --release -- --test-threads=1
cargo build --locked --release --workspace --bins --examples
.\target\release\compi.exe --instance phase4
```

First launch creates one shell; later windows/relaunches do not repair hidden or intentionally empty work by creating replacement shells.

1. With the sidebar closed, create/rename/reorder terminal tabs. Open the sidebar explicitly, create/switch a workspace, and verify the top tabs remain primary. Hide and restore a tab without changing its process.
2. Build nested Split right/down layouts. Drag a divider repeatedly within 32 ms intervals, including while PTY resize metadata arrives. The final ratio must commit without a GPUI panic or self-generated revision conflict.
3. Shrink the window with the sidebar open. Every pane keeps at least a 20-column by 4-row canvas; workspace scrolling and directional focus reach clipped panes. Expanding restores saved ratios; visibility/width changes do not rewrite the tree.
4. Tear a three-pane tab into a new window, cancel another drag with Escape, then drop into an existing window. Verify the same shell PIDs/variables and split tree; the destination retains its own appearance. Moving the final tab leaves an empty source view.
5. Launch the same instance from another process. It should create another native window in the existing GUI host, not report a false closed-pipe error or launch another shell. Exercise attachment conflicts without takeover.
6. Open Quick Appearance and Settings with `Ctrl+,`. Verify centered placement, responsive rail/wrapped navigation, centered button labels, shared text gutters, Tab/Shift-Tab focus, Escape dismissal, and nested catalog return. Select Compi Neutral with **Advanced → Terminal colors → Follow theme** and confirm application and terminal colors change together. Enable a different terminal override and confirm Appearance reports it; changing Theme must preserve the override. Turn **Transparent background** on, set opacity to 70%, and compare blur off/on over light, dark, and detailed backgrounds. Turn transparency off and back on; previous opacity and blur must return. Reduced transparency must use fully opaque effective fills without overwriting the requested preference. In **This window**, choose Warm Carbon and confirm global TOML is unchanged while version-5 private JSON holds scoped appearance overrides. Change global blur/transparency in another window and verify explicit window fields remain. **Use global defaults** removes the window appearance object.
   - Import an unmodified downloaded Zed `.json` family with dark/light variants. Both must appear as local entries without changing appearance, opacity, blur, or publication state. Also copy a standard `.json` directly into the theme directory and confirm discovery without applying it. Reject malformed required structures, invalid known colors/collection entries (including unused fields and a later variant), oversized files, conflicting variants, and whole-family capacity overflow without partial installation. Cancel both native pickers. Search/favorite, preview while the applied theme stays first with its inline Current selector, Cancel, apply scoped, relaunch, and export/reimport through More with author, supplied license/notices, RGBA, and unused syntax fields intact. Independently validate exported JSON against Zed's published schema. Verify the terminal-only catalog in Advanced and narrow feedback/footer layouts.
   - With the global config read-only, Apply must leave the truthful preview and catalog error visible without changing accepted settings. Remove an in-use variant and verify an explanatory block; switch away/cancel relevant previews, then remove one unused variant while its sibling remains active. Only that variant and its favorite may disappear; the sibling, original downloaded file, and external export must remain intact. Bundled themes remain undeletable. Seed a legacy managed import with saved selection/favorite/terminal override: startup migration must preserve identity, colors, and attribution, retain a verified `.migrated` original, and leave failed migrations recoverable. A missing imported ID on restart must preserve geometry/navigation and show an appearance fallback diagnostic.
   - With RGBA background/surface tokens, verify readable opaque catalog samples and controls. Disable transparency: even a zero-alpha main/terminal background must render its solid RGB. Enable transparency over an owned light/dark backdrop: theme alpha must compose with opacity. Output explicit and inverse RGB backgrounds equal to the palette background, plus indexed 16–255/truecolor output; matching explicit backgrounds must stay opaque over glass. Only Default and ANSI 0–15 follow palette changes, and appearance/material edits must not replace process lifetimes.
7. Open **Settings → Performance**. Confirm live client/daemon CPU, memory, OS-resource and surface values; display refresh; render timing; and bounded renderer-cache occupancy. Enable the FPS overlay, close Settings, and verify the window-level badge stays clear of chrome. Reopen Settings, rebuild the renderer, and verify caches repopulate while a shell marker and PID survive. Copy diagnostics and confirm the clipboard includes rendering, resources, surfaces, and cache sections. Run **Reconnect window** separately and confirm the marker and process survive.
8. Restart an idle isolated daemon without confirmation. With live work, verify the review names every affected surface, Escape preserves the same responsive shell, and **End work and restart daemon** leaves truthful lost surfaces that can be explicitly restarted.
9. Select text in a static terminal workload, close, and reopen at the same geometry. Selection/scroll anchors restore only from the identical authoritative baseline. Changed output, lifetime, generation, or geometry invalidates uncertain anchors. The sidebar starts closed regardless of its previous visibility.
10. Flood one pane with `yes` while typing/navigating another. End a surface with an owned background child, inspect its final output, restart explicitly, and remove a pane/tab/workspace with the appropriate confirmation.
11. In an isolated state directory, exercise slot contention and failed atomic state writes. Failed window transfer retains the source; failed preference saving remains visibly unsaved without disabling the live presentation; a later successful save clears that warning.
12. Floating panes. In a split tab, record `echo $$; stty size` in one pane and run **Float pane**. The same PID must remain, the siblings fill its space, and `stty size` follows the float frame. Drag the title and the right/bottom/corner handles in small steps: the frame must track the pointer without jumps. Switch tabs and workspaces; the float stays visible and typeable. Move typing with **Switch focus between floating and tiled panes**. While viewing another tab, right-click the source tab (the selection must not change), select the pane, and use **Float**/**Dock** in its bubble. Wait several seconds with the menu open; it must stay valid. **Dock** restores the original tile and size. Relaunch and confirm the float's placement and keyboard owner return. Removing the pane's tab through its confirmation must drop the float. Split and sidebar seams must be one-device-pixel lines that highlight on hover/drag, drag from a few pixels off the line, and never produce a workspace scrollbar with the sidebar open.

Windows shortcuts: Ctrl-T/W, Ctrl-Tab / Ctrl-Shift-Tab, Ctrl-Shift-T restore, Ctrl-Plus/Minus font zoom, Ctrl-0 zoom reset, Ctrl-Comma Settings, Alt-Shift-Plus/Minus split right/down, Alt-Arrow pane focus, Alt-Shift-Arrow pane resize, Ctrl-Shift-W remove pane, Ctrl-Shift-B sidebar, Ctrl-Shift-P palette. Mac: Cmd-T/W, Cmd-D / Cmd-Shift-D, Cmd-B, Cmd-Shift-P, Cmd-Comma Settings. Physical input, IME, scaling/display pacing, and native Mac execution require separate attributed qualification; synthetic Win32 input is not that proof.

## Tier 1: automated regression

On Windows, prepare the pinned ConPTY runtime with `pwsh -File tools/prepare-conpty.ps1`, verify the default WSL2 distribution with `wsl.exe --exec /bin/bash -lc true`, and set `GPUI_FXC_PATH` using the discovery snippet that follows before building the client. Hosted WSL2 guests should run daemon integration tests serially.

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

```powershell
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo build --locked --release --workspace --bins
```

Required result: every command exits zero. The release directory contains `compi.exe` and `compi-daemon.exe` (and the prepared ConPTY runtime on Windows); it does not contain `compi-probe.exe`, which is a development example under `release\examples` when built separately.

## Tier 2: scripted terminal and performance checks

Start an isolated daemon with opt-in instrumentation in one PowerShell window, if collecting the resource logs below:

```powershell
$env:COMPI_PERF_LOG = '1'
$env:COMPI_PERF_SAMPLE = 'manual-01'
.\target\release\compi-daemon.exe --instance acceptance
```

In a second PowerShell window, launch the GUI against that instance:

```powershell
.\target\release\compi.exe --instance acceptance
```

Use `.\target\release\examples\compi-probe.exe --instance acceptance ...` only if the explicitly built development example is needed:

```powershell
cargo build --locked --release -p compi-client --example compi-probe
.\target\release\examples\compi-probe.exe --instance acceptance workspace
```

Run these commands in WSL Bash inside an attached 100x30 Compi terminal; they use Bash syntax and common Ubuntu command-line tools. Capture pass/fail, elapsed time, peak private bytes, peak working set, peak handles, and any visible corruption.

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
| Terminal transparency | Enable Transparent background at 70%, compare blur off/on and transparency off over light and detailed content, and output an explicit RGB background equal to the palette default | Blur off leaves edges sharp; blur on softens them; transparency off removes backdrop content without forgetting opacity or blur. Text and explicit cell backgrounds including equal-RGB/inverse cases remain opaque; reduced transparency resolves fills to full opacity without changing the saved preference. |
| Title | `printf '\033]0;Compi title test\007'` | Native window title and active tab update once, without repaint churn. |
| OSC 52 clipboard | `printf '\033]52;c;%s\a' "$(printf 'compi-osc52' | base64 -w0)"` | Clipboard contains `compi-osc52`; this is clipboard write, not bracketed paste. |
| Resize stream | `watch -n 0.2 date` while continuously resizing | TUI redraws cleanly; no garbage rows or crash. |
| Resize flood | Rapidly drag a corner for 15 seconds | Final grid matches final window size; client and daemon remain alive. |
| Escape flood | `timeout 10s sh -c 'while :; do printf "\033[31mX\033[0m"; done'` | Parser remains responsive; attributes do not bleed into prompt. |
| 10 KiB paste | In WSL Bash, run `stty -icanon -echo min 1; dd bs=10240 count=1 iflag=fullblock status=none | wc -c; stty sane`. Paste exactly 10,240 printable ASCII bytes (no newline). If interrupted, run `stty sane`. | The count is `10240`; noncanonical input avoids the shell's canonical-line input limit. |
| Reattach under output | Start a build or count loop, close Compi, wait, reopen and attach | Process never stops; current screen and subsequent output are coherent. |

Settings provides the normal interactive monitor. The environment-gated logs below remain the attributed measurement path for release qualification and correlated latency work.

For ad hoc performance sampling, launch the release client with opt-in instrumentation in its PowerShell window (the daemon needs `COMPI_PERF_LOG=1` at startup for daemon resource records):

```powershell
$env:COMPI_PERF_LOG = '1'
$env:COMPI_PERF_SAMPLE = 'manual-01'
.\target\release\compi.exe --instance acceptance
```

Instrumentation writes:

- `%LOCALAPPDATA%\Compi\client-startup.log`: daemon connection, first window, first terminal frame, and optional ready-probe timing;
- `%LOCALAPPDATA%\Compi\client-resource-<pid>.log`: six-second client private bytes, working set, handles, workload, and workspace surface count (under the `sessions` field);
- `%LOCALAPPDATA%\Compi\daemon-resource-<pid>.log`: six-second daemon private bytes, working set, handles, and surface count (under the `sessions` field);
- `%LOCALAPPDATA%\Compi\client-perf.log`: frame-interval and terminal-paint distributions under active output.
- `%LOCALAPPDATA%\Compi\latency-<pid>.log`: correlated input IDs at GPUI receipt, client queue/send, daemon receipt, PTY output, terminal-state sequence, and the next presented frame.

Set `COMPI_PERF_EMPTY_WINDOW=1` with `COMPI_PERF_LOG=1` to measure a blank GPUI window without connecting to a daemon. Set `COMPI_PERF_READY_PROBE=1` only against an isolated measurement session; it sends a deterministic `printf` command and measures both launch-to-rendered-marker and input-to-rendered-marker time.

For deterministic terminal debugging, set `COMPI_TERMINAL_TRACE_DIR` on an isolated daemon and optionally set `COMPI_TERMINAL_TRACE_LABEL`. Each session then writes a bounded 16 MiB binary trace containing its initial grid, input bytes, PTY output bytes, resize events, and monotonic timing. Traces can contain commands, credentials, and terminal output; never enable capture for normal or valuable sessions. Regression tests replay captured PTY output through `TerminalState` without WSL or timing dependencies.

The release harness runs empty-window, warm-daemon, and cold-daemon launch samples; measures fresh one-, two-, and four-session client/daemon pairs; queries Windows GPU process-memory counters; and writes CSV plus environment JSON under `%LOCALAPPDATA%\Compi\measurements`:

```powershell
cargo build --locked --release --workspace --bins
cargo build --locked --release -p compi-client --example compi-probe
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
| Tabs | Create tabs with `Ctrl+T`, cycle with `Ctrl+Tab` and `Ctrl+Shift+Tab`, hide/restore with `Ctrl+W`/`Ctrl+Shift+T`, hover active/inactive close slots, reorder, tear off, and overflow a narrow titlebar. Repeat with Compi Neutral and a colored application theme. | Compact rounded rectangles keep the same geometry across themes; inactive tabs are transparent, active/hover fills are subtle, close buttons appear without label movement, and no bright active border/full-pill shape appears. Keyboard cycling reveals the selected overflowing tab without leaking input; drag/reorder/tear-off preserves shell identity. Close still confirms before ending processes/removing the tab. |
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

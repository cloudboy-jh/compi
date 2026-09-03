# Compi terminal acceptance matrix

Run the automated tier on every change. Run the scripted and interactive tiers in a release build before a milestone or installer release. Use an isolated daemon instance for destructive lifecycle checks.

## Test environment record

Record Windows build, WSL distribution and version, display scale, monitor refresh rate, Compi commit, build profile, and whether a warm daemon already existed. Run display checks at 100%, 150%, and the machine's normal scale when available.

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

`GPUI_FXC_PATH` must name `fxc.exe`, not its containing directory. The Windows CI workflow uses the same discovery rule.

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

Set `COMPI_PERF_EMPTY_WINDOW=1` with `COMPI_PERF_LOG=1` to measure a blank GPUI window without connecting to a daemon. Set `COMPI_PERF_READY_PROBE=1` only against an isolated measurement session; it sends a deterministic `printf` command and measures both launch-to-rendered-marker and input-to-rendered-marker time.

The release harness runs empty-window, warm-daemon, and cold-daemon launch samples; measures fresh one-, two-, and four-session client/daemon pairs; queries Windows GPU process-memory counters; and writes CSV plus environment JSON under `%LOCALAPPDATA%\Compi\measurements`:

```powershell
cargo build --release --bins --example compi-probe
.\tools\measure-release.ps1 -Samples 10 -ConfirmPhysicalDisplay
```

`-ConfirmPhysicalDisplay` is an operator assertion. Do not pass it through a virtual display or remote-only session. Without it, the harness intentionally labels the run diagnostic rather than qualified. For longer marginal-session analysis, keep instrumentation active, add one blank session at a time, wait at least six seconds per state, and compare consecutive client and daemon resource records by their `sessions` values.

The performance gate is:

- first visible frame p95 under 100 ms;
- warm existing-session ready-for-input p95 under 200 ms;
- cold first-terminal-frame p95 under 500 ms;
- active frame-interval p95 under 8,333 µs on a physical 120 Hz display;
- terminal-paint p95 below the frame interval;
- initially no more than 35 MiB client private bytes, with a stretch target of 25 MiB;
- no sustained private-byte, GPU-memory, or handle growth after tabs and Kitty images are closed.

## Tier 3: interactive client acceptance

| Area | Procedure | Required result |
|---|---|---|
| Shell control | Verify command echo, `Ctrl+C`, `Ctrl+D`, `Ctrl+Z`, `bg`, and `fg`. | Bash semantics match a native WSL terminal. `Ctrl+C` is never stolen by copy handling. |
| TUI applications | Exercise `htop` or `btop`, `vim` or `nvim`, `less`, `tmux`, and `fzf` with inline preview. | Alternate-screen transitions, cursor, mouse, keyboard, and redraw behavior remain correct. |
| Selection and clipboard | Select single-line, wrapped, multiline, CJK, and combining-mark text; copy and paste with both supported shortcuts. | Copied text preserves logical line breaks; selection does not alter terminal input; paste honors bracketed-paste mode. |
| Scrollback resize | Scroll several pages up, resize wider and narrower, then return to bottom. | Viewport stays anchored to the same logical content; no jump to bottom, overlap, or stale cells. |
| Tabs | Create multiple sessions, switch rapidly, close active and inactive tabs, and overflow the available titlebar width. | Active state is unambiguous; tabs remain reachable without permanent arrow controls; close appears only where intended. |
| Session palette | Open the command control, switch to an open tab, attach a detached session, create a session, and inspect exited/failed sessions. | Every state is represented accurately and the palette closes after a successful action. |
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

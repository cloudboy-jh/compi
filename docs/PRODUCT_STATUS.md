# Compi product status

Status date: 2026-09-04

## Executive status

Compi's persistent-session architecture and terminal-truth implementation are suitable for controlled internal dogfooding. The five representative workflows now pass through the real GUI, and the keyboard, terminal-state, resize/reflow, mouse/focus, OSC, Unicode, diagnostics, lifecycle, trace, and latency work is implemented.

The product is not ready for public release. Physical-keyboard confirmation, physical-display/DPI qualification, destructive recovery checks, clean-machine installer qualification, and signed artifact qualification remain open. Milestone 4 agent-session work remains paused until those release gates close.

## Product capability

The current build provides:

- Multiple independent WSL2 Bash sessions owned by a persistent per-user Windows daemon.
- Client detach and reattach without stopping live shells.
- A custom terminal state model with local scrollback, logical-line resize/reflow, sequenced-delta recovery, Kitty graphics transport, OSC 8 hyperlinks, write-only OSC 52 clipboard handling, grapheme clusters, and wide-cell repair.
- Tabs, wrapped and multiline selection, mouse/focus reporting, safe hyperlink opening, native titlebar controls, selection-aware copy, bracketed paste, modified keys, application cursor/keypad modes, and control-key forwarding. OS-synthesized input passes; physical-keyboard confirmation remains open.
- Session startup in the Linux home directory or an explicit absolute WSL/Windows project directory.
- Validated Windows-to-WSL path translation pinned to the selected default WSL2 distribution.
- OSC 7 current-directory tracking and active-directory inheritance for new tabs.
- Non-blocking warnings for projects under OneDrive and other known synchronized Windows directories.
- Protocol v7 with optional input-latency correlation and atomic persisted metadata v2 migration.
- Per-user setup, repair, removal, daemon supervision, portable packaging, embedded version metadata, and SHA-256 manifests.
- Windows 10 build 19041 minimum-version enforcement and actionable WSL prerequisite errors.

## Verification completed

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --all-targets --all-features -- -D warnings`: pass.
- `cargo test --all-targets`: 63 tests passed across six suites.
- Release setup and portable package builds: pass.
- Working-directory UI smoke: Bash rendered at `/mnt/c/Users/johns/OneDrive/Desktop/Proj/compi` when launched from the Windows repository directory.
- Thirty-minute resource soak with three sustained-output sessions and transient create/kill churn: pass.
  - Daemon handles remained at 152–153 under load and fell to 115 after cleanup.
  - Client private memory remained between 89.29 and 89.54 MiB.
  - Daemon private memory peaked at 37.31 MiB with bounded terminal state.
- A daemon connection-handle leak exposed by the initial soak was fixed by reaping completed connection handler threads and is covered by a transient-client regression test.
- Bash control/editing, `less`, Vim, `fzf` with inline preview, and OpenCode were exercised through the real GUI; captured streams drove deterministic terminal replays.
- Attached and detached session termination, descendant-process exit, `Ending…` lifecycle state, and detach-only tab/client close behavior passed isolated tests.
- A correlated 120-keystroke input-to-present run completed all 120 IDs in order: 29.347 ms p50, 36.861 ms p95, and 45.175 ms worst. Terminal-state-to-present dominated at 21.085 ms p50.
- The final release build completed a 30-minute mixed-TUI soak: 196 `less`/Vim/`fzf` preview/sustained-output/resize cycles and 62 client/daemon samples over 1,811 seconds.
  - During the final 15 minutes, client private bytes fluctuated between 110.77 and 159.27 MiB and ended at 132.75 MiB; dedicated GPU memory fluctuated between 45.63 and 113.70 MiB and ended at 71.14 MiB; handles remained between 597 and 599.
  - Daemon private bytes fluctuated between 3.00 and 4.99 MiB and ended at 4.20 MiB; daemon handles remained exactly 115. No measured resource grew monotonically.
  - The run completed without process exit or UI stall. Its trace recorded input, output, and resize events, emitted one truncation marker, parsed to its exact end, and remained below the 16 MiB cap.
- Mixed-TUI terminal paint remained inside the available frame budget; the highest recorded per-window paint p95 was 2.795 ms.

Historical release evidence is in [`ACCEPTANCE_RESULTS_2026-09-02.md`](ACCEPTANCE_RESULTS_2026-09-02.md); the current terminal-truth evidence is summarized above. The authoritative remaining work is in [`NEXT_STEPS.md`](NEXT_STEPS.md).

## Startup and memory assessment

Current GPUI startup and memory measurements are architecture baselines, not the primary terminal-correctness blocker. The original P0 targets are not achievable through ordinary Compi-level optimization with the current GPUI architecture.

| Metric | Original target | Current evidence | Assessment |
|---|---:|---:|---|
| Warm first-window p95 | 100 ms | 376 ms | GPUI initialization establishes a floor above the target. |
| Warm ready-for-input p95 | 200 ms | 544 ms | Cannot meet the target while first-window initialization exceeds 300 ms. |
| Launch-marker input-to-render p95 | 200 ms | 153 ms | Target met in diagnostic measurements. |
| Correlated input-to-present p95 | 200 ms | 36.861 ms across 120 OS-synthesized keystrokes | Target met; physical keypress-to-pixel qualification remains open. |
| Cold first-terminal p95 | 500 ms | Best measured 935 ms | Target unmet; fresh unsigned process launches also show host-side scanning variability. |
| Client private memory | 35 MiB | Approximately 89–96 MiB | Empty GPUI baseline already exceeds the target. |
| Client working set | Not separately budgeted | Approximately 56–61 MiB | Stable, but above a 35 MiB interpretation. |

Startup instrumentation attributes roughly 321–369 ms of typical launch time to `gpui::Application::new`. Disabling thin LTO did not improve it. Compi's absent-daemon detection wait was reduced from 250 ms to 25 ms, removing 225 ms from the controllable cold path. Instrumented daemon initialization itself completed in 36 ms; fresh unsigned daemon process creation sometimes incurred a repeatable 3.17-second host-side delay.

Recommended provisional P0 budgets for the current architecture:

- Warm first-window p95: 450 ms.
- Warm ready-for-input p95: 650 ms.
- Input-to-render p95: 200 ms.
- Signed clean-machine cold first-terminal p95: 1.5 seconds.
- Client private memory: 110 MiB.
- Client working set: 75 MiB.
- Daemon private memory during the documented three-session soak: 50 MiB with no monotonic memory or handle growth.

The original 100/200 ms startup and 35 MiB client-memory goals should remain longer-term architectural targets. Enforcing them for P0 would require substantial upstream GPUI work, a resident/prewarmed client architecture, or a rendering-framework replacement.

These provisional budgets remain separate from the terminal truth completion gates. The current sprint does not require the old startup or client-memory aspirations.

## Immediate blockers

1. Confirm sustained-output `Ctrl+C` and selection-aware `Ctrl+C`/`Ctrl+Shift+C` using a physical keyboard. OS-synthesized key events already pass through the GPUI input path.
2. Run physical-display frame pacing, normal/maximized/narrow window checks, mixed-DPI, and multi-monitor qualification.
3. Exercise daemon crash, forced WSL shutdown, Windows sign-out, and malformed/stale metadata recovery on isolated sessions.
4. Verify fresh-profile install, version-to-version upgrade, repair, and uninstall as a non-administrator on supported clean Windows 10 and Windows 11 WSL2 systems.
5. Produce, sign, and qualify the exact installer, maintenance executable, bootstrapper, and portable archive.

Milestone 4 resumes only after these release blockers close.

## Release artifacts

The current local artifacts are unsigned and intended only for internal testing:

- `target/distribution/Compi-0.1.0-Setup.exe`
  - SHA-256: `2a4ce9bbb910eb16c5c5cf77b1469734add3a248eea37bc00e719f79fe9447ce`
- `target/distribution/Compi-0.1.0-Windows-x64.zip`
  - SHA-256: `a23d25f71e9dc706429e59fe65028801ffbf91ffc70a3fb96201efa9ebe5f264`
- `target/distribution/SHA256SUMS.txt`

The tag-triggered Windows release workflow imports a certificate from repository secrets, signs the product executables, MSI, maintenance executable, and setup bootstrapper, verifies signatures and embedded versions, packages the release, and creates a draft GitHub release.

## Recommended path

Perform the two physical-keyboard terminal checks first, then execute destructive recovery and clean-machine release qualification. Keep GPUI's provisional budgets as architecture baselines, and defer broader agent-session features until the signed Windows release gates pass.

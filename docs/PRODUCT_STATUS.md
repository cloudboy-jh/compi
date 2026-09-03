# Compi product status

Status date: 2026-09-03

## Executive status

Compi is a P0 feature-complete release candidate suitable for controlled internal dogfooding. It is not ready for public production distribution because the current artifacts are unsigned and the physical compatibility and clean-machine qualification gates remain open.

Milestone 4 agent-session work has not started. It remains deferred until the P0 release gates close.

## Product capability

The current build provides:

- Multiple independent WSL2 Bash sessions owned by a persistent per-user Windows daemon.
- Client detach and reattach without stopping live shells.
- Authoritative terminal state, local scrollback, sequenced-delta recovery, and Kitty graphics transport.
- Tabs, selection, mouse input, native titlebar controls, terminal-safe copy and paste, and uninterrupted `Ctrl+C` handling.
- Session startup in the Linux home directory or an explicit absolute WSL/Windows project directory.
- Validated Windows-to-WSL path translation pinned to the selected default WSL2 distribution.
- OSC 7 current-directory tracking and active-directory inheritance for new tabs.
- Non-blocking warnings for projects under OneDrive and other known synchronized Windows directories.
- Protocol v6 and atomic persisted metadata v2 migration.
- Per-user setup, repair, removal, daemon supervision, portable packaging, embedded version metadata, and SHA-256 manifests.
- Windows 10 build 19041 minimum-version enforcement and actionable WSL prerequisite errors.

## Verification completed

- `cargo fmt --all`: pass.
- `cargo clippy --all-targets --all-features -- -D warnings`: pass.
- `cargo test --all-targets`: 52 tests passed across six suites.
- Release setup and portable package builds: pass.
- Working-directory UI smoke: Bash rendered at `/mnt/c/Users/johns/OneDrive/Desktop/Proj/compi` when launched from the Windows repository directory.
- Thirty-minute resource soak with three sustained-output sessions and transient create/kill churn: pass.
  - Daemon handles remained at 152–153 under load and fell to 115 after cleanup.
  - Client private memory remained between 89.29 and 89.54 MiB.
  - Daemon private memory peaked at 37.31 MiB with bounded terminal state.
- A daemon connection-handle leak exposed by the initial soak was fixed by reaping completed connection handler threads and is covered by a transient-client regression test.

Detailed test evidence is in [`ACCEPTANCE_RESULTS_2026-09-02.md`](ACCEPTANCE_RESULTS_2026-09-02.md). The authoritative remaining work is in [`NEXT_STEPS.md`](NEXT_STEPS.md).

## Startup and memory assessment

The original P0 targets are not achievable through ordinary Compi-level optimization with the current GPUI architecture.

| Metric | Original target | Current evidence | Assessment |
|---|---:|---:|---|
| Warm first-window p95 | 100 ms | 376 ms | GPUI initialization establishes a floor above the target. |
| Warm ready-for-input p95 | 200 ms | 544 ms | Cannot meet the target while first-window initialization exceeds 300 ms. |
| Input-to-render p95 | 200 ms | 153 ms | Target met in diagnostic measurements. |
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

## Release blockers

1. Run the complete physical keyboard/display terminal matrix for Bash, `top`, `less`, `vim`, `nano`, Git prompts, selection, mouse reporting, Unicode, and Kitty graphics.
2. Validate physical 100/120 Hz pacing, sustained-output `Ctrl+C`, mixed DPI, multiple monitors, and normal/minimized/maximized window states.
3. Configure a usable Windows code-signing certificate and produce signed release artifacts.
4. Qualify the exact signed artifacts on clean non-administrator Windows 10 and Windows 11 WSL2 machines.
5. Exercise fresh install, repair, real version-to-version upgrade, sign-in supervision, and complete uninstall on those clean machines.
6. Validate Git and two representative agent harnesses against projects under both `/home/...` and `/mnt/c/...`.
7. Approve realistic P0 startup and memory budgets or commit to the larger architectural work required by the original targets.

## Release artifacts

The current local artifacts are unsigned and intended only for internal testing:

- `target/distribution/Compi-0.1.0-Setup.exe`
  - SHA-256: `2a4ce9bbb910eb16c5c5cf77b1469734add3a248eea37bc00e719f79fe9447ce`
- `target/distribution/Compi-0.1.0-Windows-x64.zip`
  - SHA-256: `a23d25f71e9dc706429e59fe65028801ffbf91ffc70a3fb96201efa9ebe5f264`
- `target/distribution/SHA256SUMS.txt`

The tag-triggered Windows release workflow imports a certificate from repository secrets, signs the product executables, MSI, maintenance executable, and setup bootstrapper, verifies signatures and embedded versions, packages the release, and creates a draft GitHub release.

## Recommended path

Do not replace GPUI before validating actual user demand. Adopt the provisional P0 budgets, obtain signing credentials, run the physical and clean-machine qualification matrix, then publish a signed beta. Use beta telemetry and user reports—not the superseded aspirational limits—to decide whether startup or memory warrants future architectural work.

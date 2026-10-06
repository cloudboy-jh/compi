# Compi next steps

This file tracks only unfinished work. Completed implementation and verification history is in [Completed work](COMPLETED.md).

Work in the order below. Keep changes on the existing workspace, persistence, protocol, and navigation contracts unless a listed item requires a narrow extension.

## 1. Remaining UI/UX qualification

- Preserve the completed top-tab navigation, optional workspace sidebar, flat responsive overlays, typography, and compact control system. Change them only where native evidence exposes a defect.
- Qualify the transient loading frame, native tab tear-off drag, and real mixed-DPI transitions on supported Windows displays. Automated captures already cover normal, narrow, maximized, split, sidebar, palette, settings, theme catalog, overflow, empty, disconnected, exited, failed, lost, confirmation, and recovery states.
- Verify hover, focus, active, disabled, pending, and destructive states without relying on color alone. Keep New terminal, pane actions, and custom Windows controls visible, reachable, and non-overlapping.
- Exercise physical keyboard navigation, terminal input, native drag, maximize, close, platform shortcuts, clipboard, IME/dead keys, exact user fonts, and fallback glyphs on supported Windows display configurations.

### Appearance and theme follow-through

- Treat Compi Neutral, simplified Theme/Advanced terminal colors, independent transparency/blur controls, and the standard Zed JSON catalog cutover as completed. Preserve Current-first ordering, favorites, scoped preview/apply/cancel, protected variant removal, and program-supplied terminal colors; change them only for demonstrated defects.
- Wire `compi theme install <file.json>` in the separate CLI session to the existing headless `ThemeLibrary::import_file` installation path. UI import, CLI installation, and direct-directory discovery must use the same validator, stable identities, and application library. WSL installation must target the Windows application's library, not a separate Linux daemon directory; installing must not apply, publish, or restart shells.
- Exercise an unmodified downloaded Zed family through the finished Bash/WSL command, observe all variants in the native catalog, apply explicitly, and export. Exported JSON already passes the published schema; load it in a real Zed application to qualify interoperability beyond schema validation.
- Qualify Clear/Blurred and solid fallback on native macOS and with reduced-transparency settings. Compare Compi Neutral side by side with the Tern reference on light, dark, and busy desktops; low opacity cannot guarantee arbitrary-wallpaper contrast.
- Repeat catalog, settings, alternate-font, and tab-menu interactions across real display scales and mixed-DPI transitions. Cover keyboard/pointer selection, tooltip suppression, disabled lone-pane Detach, confirmed End, and workspace-revision invalidation without replacing shell processes.
- Online publishing is not implemented. If requested, define it separately with explicit consent, authorship, licensing, and validation; local import must never publish implicitly. Native editor, notes, web-preview, and diff features remain separate scope, not surfaces delivered by the appearance changes.

## 2. Performance and daily-use qualification

- Measure key receipt → PTY → replica → presentation latency, scrolling, resizing, and frame pacing under idle and sustained-output workloads. Use the current-FPS overlay for live feedback, not as a substitute for attributed timing.
- Record build and display context and compare identical workloads before and after any fix. Confirm opt-in continuous frame scheduling remains inactive when Performance and the FPS overlay are both hidden.
- Fix only demonstrated stalls, avoidable shaping or allocation, repaint scheduling, or queue problems. Keep caches and transport queues bounded; do not replace the terminal engine.
- Run the macOS measurement/soak runner on native release binaries. Repeat the Windows measurement and soak harnesses against the current source after the recent navigation and UI changes; older measurements remain dated baselines, not current-source performance qualification.
- Produce attributable Mac resource measurements and a current Mac dogfood artifact. A recent Windows release bundle passed isolated launch smoke, but its navigation and UI changes still need release-mode performance and daily-use qualification.

### Remaining protocol and persistence contracts

- Protocol 16 rejects mismatched versions but does not negotiate capabilities or server identity/generation in `Hello`. Implement the explicit handshake required by [the wire contract](Spec.md#wire-contract) before treating that acceptance gate as complete.
- Snapshots and deltas carry bounded scrollback, but there is no on-demand history paging or history epoch for reconnect and selection anchors. Implement and verify the stronger [replication contract](Spec.md#replication), including eviction/reflow behavior; current identity/sequence/geometry/content checks are not equivalent.
- Exercise durable workspace mutations and lifecycle recovery with fault injection at commit boundaries across supported hosts. Existing actor, receipt, migration, and quarantine coverage is not exhaustive.

## 3. Native Mac and shared-platform qualification

- Qualify the bundled application, not a bare `cargo run`, across the complete workspace flow: shell and fullscreen-editor input, resize, close/reopen, same-process persistence, workspace/tab/split operations, tear-off and transfer, Settings, appearance, and recovery states.
- Exercise physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, exact user font and fallback glyphs, native controls, fullscreen, scaling, mixed displays/DPI, and display pacing.
- On a native Mac, exercise the bundled interactive Zsh/Bash startup bridge, in-pane tree/search, `compi tree` and `compi z` changing the live shell, cwd inheritance, pane detach, and negotiated Ctrl+Enter in an installed terminal application. Windows/WSL native coverage is recorded in `COMPLETED.md`; cross-target builds and WSL PTY tests are not Mac visual proof.
- Repeat the UI state/size/scale matrix on Mac and verify image paste, file drop, preview, inspector, save, and reconnect behavior.
- Keep native Linux core CI passing and qualify the Linux graphical client separately; cross-compilation and headless tests are insufficient graphical evidence.
- Exercise imported multi-variant Zed themes, migrated local themes, scoped appearance inheritance, remembered opacity/blur, material accessibility fallbacks, and split-tab pane actions in the installed Mac bundle. Windows native evidence and schema validation do not qualify Mac rendering or interaction.
- Qualify floating panes and hairline seams on the native Mac bundle: float/dock from the palette and the tab menu, move/resize tracking, keyboard ownership with physical keys and IME, relaunch restoration, seam crispness at Retina and mixed scales, and a second window attempting to attach a floated surface. Windows/WSL evidence used synthetic window messages, not physical input.
- Qualify pane arrangements on the native Mac bundle: the Arrange panes and tabs picker with physical M/F/Delete keys, combining tabs and splitting them back out, swap/mirror/flip/restore, saving and editing `[layout_presets]`, and applying while a pane floats or the tab is zoomed. Windows/WSL evidence used synthetic input.
- Qualify `cargo dev` on a native Mac: the staged `Compi Dev.app` launch, readiness detection, SIGTERM swap (at most the last ~160 ms of window state may be lost), shell persistence across swaps, Ctrl+C and terminal close leaving the dev daemon running, and `cargo dev --stop`. Only Windows was exercised.
- Qualify shell prompt settings on a native Mac and one real SSH host: login Zsh startup and live reload through the ZDOTDIR wrappers, login-shell scope in `~/.zshrc`, BSD userland file writes (`mktemp`, `dd`, `wc`), and `dscl` login-shell detection. Also exercise Starship (presets, `STARSHIP_CONFIG`, its ble.sh integration) on any host; only Oh My Posh in WSL Bash/Zsh was exercised.
- Run focused regressions and final workspace/dependency checks after integration. Report unavailable physical-platform checks explicitly.

## 4. Distribution and process discovery

- Qualify one real SSH host end to end with host-key verification and authentication: install the matching daemon, launch/attach/detach, drop the relay, reconnect to the same server and process lifetime, upload an image, and exercise the remote headless commands. Deterministic relay tests and the actual OpenSSH failure path are complete, but no reachable SSH server was available for a successful real-host run.
- Qualify Windows signing with an Authenticode certificate and macOS Developer ID signing/notarization with Apple credentials when those credentials and a Mac are available. Unsigned Windows and ad-hoc macOS artifacts must remain explicit otherwise.
- Run a version-to-version Windows upgrade and current clean-profile package checks on a disposable host. The existing hosted install/repair/uninstall lifecycle is complete but did not have an older release to upgrade.
- Add optional non-secret agent discovery metadata only when a concrete agent consumer defines the required identifier and kind. Do not infer agents from command lines or process names, and keep credentials, memory, steering, and orchestration outside the process protocol.
- Run the tag-triggered workflow from the final committed source and publish only after both package jobs, merged checksums, licenses, native launch, and reconnect checks pass.

## Completion gate

The shared baseline is complete when the owner can build, run, develop, and dogfood the native client on Mac and Windows; representative workspace workflows and mixed-pane soak pass on both; latency/resource results are attributable; and any remaining distribution-only gaps are explicit.

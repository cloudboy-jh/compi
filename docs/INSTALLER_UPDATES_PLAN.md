# Installer and in-app updates plan

## Scope and completion contract

Implement only the **Installer** and **In-app updates** sections of `C:\Users\johns\OneDrive\Documents\jacksmainvault\Compi\feat and bugs.md`. Remaining features, later explorations, and unrelated qualification work stay unchanged. This is the approved plan; implementation and evidenced qualification limits are recorded in [Completed work](COMPLETED.md).

Preserve existing GPUI settings/commands, daemon-owned workspaces, client-owned presentation, TOML configuration, custom theme library, per-user Windows installation, and macOS app bundles. No terminal-engine replacement, general CLI expansion, remote deployment, or broader capability-negotiation project.

Target the currently distributed platforms: Windows x64 with WSL2 and Apple Silicon macOS 14+. Do not imply Windows ARM64, Intel Mac, or Linux desktop support. Native runtime evidence is required on both platforms.

## Implementation status

The scoped source implementation is present at 0.1.3. A genuine local Windows portable 0.1.3→0.1.4 cycle passed trust rejection, cancelled staging, real old-build rollback, journal recovery, and new-build attachment while preserving shell/daemon/workspace identity and user data. Native Setup/maintenance preflight and Updates checking were exercised separately.

Full native MSI migration/install/uninstall/rollback, native macOS, multi-window and incompatible-version activation, and production OS-trust qualification remain gates, not completed acceptance claims. No destructive checks were run against the developer's installed copy; the vault checklist remains unchanged.

## Pre-implementation 0.1.2 baseline

Source inspection found:

- Windows already ships embedded-MSI `Setup.exe`, per-user install, repair/removal maintenance, Start-menu/ARP registration, a scheduled daemon supervisor, portable ZIP, and checksums. macOS ships a drag-copy app in DMG/ZIP. Reuse these paths rather than introducing a second installer.
- The release workflow creates a **draft** only after both platform jobs pass. Windows signing is optional; macOS signing is ad hoc, not Developer ID/notarization. Checksums alone do not authenticate an update publisher.
- Installer UI has ready/working/complete/error states but no real percentage/stage events or in-flight cancellation. Its completion receiver handles only one attempt; source inspection indicates later retries cannot receive completion. Prerequisites are checked once, and application-launch errors are ignored.
- Maintenance removal unconditionally deletes the Compi application-data directory. Raw MSI removal does not, so the existing raw-MSI smoke misses this behavior.
- MSI removal calls `compi-daemon --shutdown`; daemon shutdown kills its PTYs. **[INFERENCE]** major-upgrade removal of the old MSI can traverse this destructive path; prove exact sequencing with actual old/new packages. Task rollback deletes the task rather than restoring the prior registration.
- The installer renders a legacy canvas mark. Executable/MSI icons and generated macOS icons already reference v4 assets.
- No in-app updater exists. Protocol 14 requires exact equality, without daemon product/build metadata. Connect-or-start currently treats incompatibility like absence. GUI launch handoff has its own version 2.
- Durable client state already contains layout/navigation/appearance, but ordinary relaunch claims the first free window slot. Launch with an initial working directory can create another terminal. Neither is a sufficient updater restore contract.

Evidence owners: `installer/bootstrapper/src/installer.rs` (131–231, 627–736), `installer/Compi.wxs` (11–16, 78–127), `crates/compi-daemon/src/surface.rs` (310–325), `crates/compi-daemon/src/supervisor.rs`, `crates/compi-protocol/src/client.rs` (195–217), `crates/compi-client/src/probe.rs` (444–573), `crates/compi-client/src/client_state.rs` (602–759), `crates/compi-client/src/gui/workspace.rs` (764–786), `.github/workflows/release.yml`, and both distribution smoke scripts.

## Design decisions

1. **One installation transaction.** Standalone setup and the in-app updater share artifact verification, staging, activation, rollback, and result reporting. Keep updater orchestration in one client service, not in every window. Use platform helpers for file activation; keep networking and update UI out of the daemon.
2. **Full packages, not binary deltas.** Download the complete matching platform payload, including Windows ConPTY/runtime licenses and the entire macOS bundle. Avoid independently updating client/daemon/runtime files.
3. **Compatible update is not daemon restart.** When the new client is qualified against the running daemon, detach/relaunch clients while keeping the same daemon and shells. Windows needs versioned payload directories plus a stable installed launch selection because active client/daemon/supervisor images may be locked. Retain old payloads until their processes no longer need them. The running supervisor must continue launching its selected compatible generation, not unexpectedly switch on crash.
4. **Incompatible update is deliberate.** Let users download and defer. Before stopping a daemon, show affected instances, attached clients, live shells, and the fact that those shells will end. Revalidate consent against daemon-owned generation/workspace revision immediately before shutdown. If live-work accounting is unavailable, block automatic activation and explain the manual recovery path.
5. **User data is separate from product files.** Upgrade, repair, rollback, and default uninstall preserve managed workspace/config/client-state/theme data. Removing Compi-managed user data is a separate unchecked uninstall option with exact paths. Never remove project directories, external configuration/theme sources, WSL distributions, or home directories.
6. **Local updates do not update remote hosts.** Show local client version separately from each connected daemon. Qualify supported mixed versions; explain incompatible SSH targets without deploying or restarting their daemon implicitly.
7. **Automatic checks only; installation stays explicit.** Offer persisted Never / On launch / Daily choices, default Daily, plus manual checks. No silent download, install, app exit, or daemon restart. Fetch once per app/target host and avoid competing checks across windows.

## Implementation sequence

### 1. Protect existing installs and establish the upgrade baseline

- Freeze a genuine older packaged release **A**, its source/version, and artifact hashes before changing packaging. Seed disposable profiles with workspaces, custom themes, settings, and active/detached shells.
- Fix unconditional user-data deletion. Make wrapper and raw-MSI retention behavior coherent; opt-in cleanup runs only after successful product removal. Report product removal and optional cleanup failures separately.
- Fix repeat-attempt event delivery, refresh prerequisite checks on retry, check launch results, and distinguish MSI restart-required codes from ordinary completion.
- Replace fixed staging/cache names with owned per-attempt paths and serialize installer/update/repair/removal operations. Restore the previous cached MSI and task registration on failure; do not discard rollback errors.
- Treat legacy A→new-installer migration separately: the new installer cannot rewrite old MSI removal actions. Until a live-preserving migration is proven, defer while work is active and require a clearly explained deliberate stop before legacy removal. No promise that shells survive this first transition.

**Files:** `installer/bootstrapper/src/installer.rs`, `installer/bootstrapper/src/bin/compi-maintenance.rs`, `installer/Compi.wxs`, narrowly scoped daemon/supervisor lifecycle hooks.

**Exit gate:** repeated failure→retry→completion works; default removal/reinstall preserves data; rollback restores prior install registration; legacy migration never silently ends work.

### 2. Establish trusted releases and recoverable platform activation

- Publish versioned update metadata alongside GitHub release assets: schema/product version, OS/architecture/minimum OS, artifact URL/size/SHA-256, release notes, exact supported daemon protocol and qualified mixed-version combinations, and any persistence-format requirements.
- Sign metadata with a release key; embed its public verification key in Compi. Verify signature, product/platform/version, and payload digest before executing/extracting. Reject malformed, wrong-platform, stale/downgrade, or untrusted input; no unsigned fallback. Keep drafts and prereleases out of stable checks.
- Add metadata signing to the existing release pipeline. Make publication deliberate only after both native package gates pass. Private signing material stays in CI secrets. Preserve Microsoft's signatures on ConPTY files.
- Windows publisher signing and macOS Developer ID signing/notarization/stapling remain optional for the official release, per the owner's 2026-10-01 decision to retain the previous OS-signing policy. Use them when credentials are supplied; otherwise label unsigned/ad-hoc artifacts truthfully and do not disable platform security. Authenticated update metadata and native package gates remain mandatory; unsigned/ad-hoc packages do not establish publisher or Gatekeeper trust.
- Implement versioned Windows payload selection and coordinated scheduled-task/supervisor selection; migrate existing installed paths and shortcut/ARP/repair/removal callers together. On macOS, stage the complete verified bundle beside its destination and use a helper to replace it after GUI exit, retaining the previous bundle for recovery. Do not try to replace a read-only mounted DMG; explain moving the app to Applications. Handle unwritable destinations explicitly.
- Handle portable Windows packages through whole-directory staged replacement after owned clients exit, preserving old running-daemon payloads. Never register a portable build as an installed MSI product or overwrite an unrelated installed copy.
- Persist a small operation journal: attempt/target/version, verified staging, prior selection, activation stage, and pending relaunch. Recover after crash/reboot without trusting partial downloads. Serialize commit, retain a runnable prior version, and clean only owned staging.
- Validate paths during archive extraction; do not follow archive entries outside staging. Keep logs/recovery instructions available after failure. Coordinate any data-format migration with rollback; never run old code against newly incompatible data.

**Files:** `.github/workflows/release.yml`, `tools/build-installer.ps1`, `tools/build-macos.sh`, MSI/bootstrapper, daemon supervisor/launch selection, and focused new update/activation modules under existing owning packages. Extract a shared crate only if both shipped consumers genuinely need the contract.

**Exit gate:** corrupted/untrusted packages cannot mutate the install; injected activation failure leaves A runnable; interruption resumes recovery; active compatible daemon/shell identities remain unchanged.

### 3. Finish installer UX and branding

- Windows preflight: supported OS/architecture, install registration/version, disk/access, missing WSL, no default distribution, WSL1 default, and failing guest startup. Reuse WSL checks and present specific fix instructions plus Recheck. Do not silently install/convert/select a distribution. Repair/removal should not be blocked merely because shell prerequisites are missing.
- Keep the macOS drag-to-Applications experience; make supported OS/architecture, location/access, quarantine/Gatekeeper, replacement, and next-launch guidance explicit. Use actual platform copy progress; do not add a redundant Mac wizard merely for parity.
- Use structured per-attempt events: checking prerequisites, extracting/downloading, verifying, staging, applying, rolling back, complete/error. Display bytes/percentage when measurable and an honest indeterminate stage otherwise. Embedded offline MSI has no network download to pretend to measure.
- Integrate real MSI progress and supported cancellation, not a killed `msiexec` process. Permit cancel before commit and during supported rollback-safe stages; explain any non-cancellable commit. Wait for rollback before saying cancellation is complete.
- Completion shows installed version, restart requirement, checked Open Compi action, and recovery/log action on failure. Retry must work after multiple failures in the same window.
- Replace the installer header/working legacy canvas mark with approved v4 artwork. Retain already-correct resource assets; visually verify setup, maintenance, executable/ARP/Start menu, Finder/Dock, and DMG surfaces.

**Files:** bootstrapper UI/preview and maintenance; existing WSL checker; macOS packaging/instructions; `README.md`.

**Exit gate:** actual setup/maintenance surfaces expose accurate progress, actionable errors, safe cancellation, completion/next action, and the correct mark.

### 4. Implement update compatibility and execution

- Add typed connection outcomes: absent, incompatible, unauthorized, and transport failure. Start a daemon only for genuine absence; never interpret mismatch as permission to launch a competing daemon.
- Add a narrow authenticated per-user lifecycle/status contract stable across updater generations: product/protocol identity, instance/generation, connected-client/live-work inventory, and conditional shutdown with acknowledged exit. This is update safety, not general protocol capability negotiation. Keep exact-protocol matching until a wider rule is actually maintained and qualified.
- Distinguish client restart, optional later daemon restart, and required daemon restart in the update decision. Reuse existing affected-surface/revision-based confirmation UI; enforce the final condition on the daemon, not merely a client snapshot. Quiesce reconnect loops and affected GUI hosts before activation.
- Run check→download→verify→stage→await consent→activate→relaunch through the shared transaction. Stream downloads to disk, support cancellation/retry, and revalidate recovered partial content before use. Preserve staged verified downloads when installation is deferred.
- Account for all instances and GUI processes using the target installation, including detached work and supervisor backoff. Coordinate host release and task selection; never force-close unrelated installations/instances.
- If an incompatible daemon is deliberately stopped, persisted live surfaces become Lost. Retain topology/settings but do not fabricate process resumption or automatically spawn replacement shells.
- After activation, confirm the new executable/build actually starts and attaches. Failed helper/app launch exposes Retry launch / restore prior selection / reinstall verified package, with useful logs. Do not declare success merely because spawning a process succeeded.

**Files:** focused updater owner/platform adapters; `crates/compi-protocol/src/client.rs` and lifecycle DTOs; daemon dispatch/supervisor; client `probe.rs`, `connection.rs`, `window_host.rs`.

**Exit gate:** compatible update keeps shell lifetimes; incompatible update can be deferred/cancelled without stopping work; stale consent is rejected; failed installation/relaunch has a usable recovery route.

### 5. Add the first-class Updates surface and attach-only restoration

- Extend existing Settings with Updates; add Check for Updates to the existing command registry. Show current/available client versions, connected daemon versions/protocols, last-check result, release notes, actual progress, errors/retry, and pending client/daemon/system restart distinctions.
- Keep update work off the UI thread. Persist automatic-check preferences through the existing comment/unknown-key-preserving TOML writer, serialized with other writes. Offline/service failures must leave normal terminal work usable.
- Actions: Check, Download, Cancel where safe, Install and restart client, Defer, and separately confirmed daemon restart when necessary. Background checks must not steal focus or interrupt typing.
- Before client exit, flush existing presentation and write a bounded one-time handoff identifying exact window slots, connection targets/instances, and selected configuration paths. Wait for old GUI host, state-slot, and terminal-attachment release.
- Add explicit updater restore launch mode: claim those exact slots, fetch authoritative workspace snapshots, reconcile saved views, and attach existing surface/lifetime IDs. Do not replay working-directory startup, initialize/create workspaces, or restart Lost surfaces.
- Consume the handoff once so launch retries cannot duplicate windows or shells. Respect exclusive terminal control; report unavailable/AlreadyAttached instead of stealing another client's attachment. Keep safe existing viewport fallback when output changed during relaunch.
- Preserve workspace topology/selection, hidden tabs, geometry, focus, fonts/appearance, theme overrides/custom themes, and configuration overrides without resetting normal presentation policies.

**Files:** `config.rs`, `commands.rs`, `gui.rs`, `gui/workspace/settings.rs`, `gui/workspace.rs`, `client_state.rs`, `window_host.rs`; reuse `theme_store.rs` without changing theme behavior.

**Exit gate:** one and multiple windows restore their intended views once, with no shell-creation mutation; manual/automatic checking, deferral, progress, notes, and restart/error states work in the actual UI.

### 6. Qualify real installed-version cycles and document the result

Produce update-capable release **B**, then genuinely newer release **C**. Test **A→B** legacy/manual migration separately from **B→C** in-app updating. Do not claim an old package with no updater can perform an in-app update. Include both protocol-compatible and deliberately incompatible update candidates.

Extend `tools/smoke-windows-distribution.ps1` and `tools/smoke-macos-bundle.sh`; retain existing same-process reconnect assertions. Exercise actual setup/maintenance, not only raw MSI; exercise copied installed app bundles, not only bare executables. Use disposable profiles/machines and attributable artifacts, never uninstall the developer's live installation.

| Scenario | Required proof on native Windows/WSL2 and macOS |
|---|---|
| Fresh install and launch | Standard-user install, prerequisites/actionable failures, correct branding, shell ready, correct payload/registration/location |
| Upgrade and repair/reinstall | A→B and B→C, same-version repair/reinstall, older-version rejection, settings/workspace/theme retention |
| Compatible active-work update | Active and detached shells retain PID/process-lifetime IDs, cwd/environment, daemon identity, workspace IDs, and shell count; output continues |
| Incompatible update | Warning covers all affected work; defer/cancel preserves it; changed live-work inventory invalidates consent; confirmed restart ends shells and marks prior surfaces Lost |
| Presentation restore | Multiple windows/targets/config overrides, focused pane/tab, hidden tabs, geometry/fonts/theme; repeat recovery does not duplicate windows/shells |
| Download/trust failures | Offline/HTTP failure, interrupted download, bad signature/hash, wrong platform/version, corrupted archive; no install mutation |
| Install/activation failures | Disk full/access denial, locked images, cancellation/rollback, crash/reboot at transaction boundaries, helper/task failure; prior usable version or explicit recovery |
| Relaunch failures | Old GUI-host/attachment lock, missing new executable, immediate exit, incompatible connection; success requires observed new build/attachment |
| Uninstall | Default keep-data→reinstall; explicit managed-data deletion; external projects/config/theme sources untouched; no residual owned task/process/install registration |

Windows-specific: default scheduled task and named instances, running supervisor/backoff, two-user task ownership, missing MSI source, task registration rollback, MSI restart-required outcomes, portable ZIP, logon/reboot selection. Test supported Windows 10 baseline and Windows 11 with WSL2.

macOS-specific: advertised minimum and current supported OS, DMG and ZIP, actual downloaded quarantine/Gatekeeper/signing, writable and denied Applications destinations, mounted-DMG launch guidance, live-bundle replacement, rollback and removal with a daemon still running.

Use existing behavioral regression suites for compatibility, lifecycle, persistence, and attachment. Add permanent tests only for real edge cases introduced here: retry terminal events, trust rejection, consent races, journal recovery, and one-time attach-only restoration. Tests and cross-compilation do not replace packaged native execution or visual proof.

After integration and smoke proof, update `README.md`, scoped sections of `docs/testcmds.md`, and `docs/COMPLETED.md` with exact versions/artifact hashes, host details, observed outcomes, and any unavailable platform/signing gate. Do not mark the vault checkboxes complete until their native acceptance evidence exists.

## Coverage of the requested checklist

| Requested item | Planned coverage |
|---|---|
| Installer: prerequisites/actionable failures | Phase 3 |
| Installer: percentage/progress/completion/retry/cancel | Phases 1 and 3 |
| Installer: verify artifacts/clean failed attempts safely | Phases 1–2 and 6 |
| Installer: preserve data/explicit uninstall deletion | Phases 1–2 and 6 |
| Installer: fresh/upgrade/launch/uninstall both platforms | Phase 6 |
| Installer: proper Compi mark | Phase 3 visual gate |
| Updates: first-class versions/notes/progress/failures/restarts | Phases 4–5 |
| Updates: manual/configurable automatic checks/defer | Phases 4–5 |
| Updates: compatibility/no silent shell termination | Phases 1–2, 4, and 6 |
| Updates: preserve workspace/appearance/no duplicate shells | Phase 5 and compatible-update proof |
| Updates: interrupted download/install/relaunch recovery | Phases 2, 4, and 6 |
| Updates: real old→new cycle with active work/failure recovery | A→B and B→C matrix in Phase 6 |

## Prerequisites and review evidence limits

Implementation can proceed from the existing source and package machinery. Native Mac access is required for macOS qualification; release-metadata signing key/CI configuration is required for trusted updates; publisher/Apple credentials are required for production OS-trust qualification. These are gates, not reasons to broaden the feature scope.

This planning pass inspected source and historical documentation. The existing `target/compi-latest/release/compi-daemon.exe --check-system` completed successfully on the local Windows host, exercising its prerequisite path only. No installer was run, no upgrade/uninstall was attempted, and no current macOS or update runtime qualification is claimed. Only this plan document was added; the original feature notes and remaining-feature schedule were not changed.

# Compi next steps

The documentation baseline is established. [Spec.md](Spec.md) is the authoritative product and technical contract. The implementation still provides the Windows/WSL terminal described in the [README](../README.md); the cross-platform migration has not started.

## Next session: Phase 0 — implementation contracts

Status: not started. Resolve these contracts before changing application code:

- [ ] Define process-lifetime identity and replica/history/selection invalidation when a surface restarts without a server restart.
- [ ] Define durable workspace mutation acknowledgements, revision publication, and recovery after process-launch or persistence failure.
- [ ] Define durable client identity, hidden-tab membership, remembered navigation, and behavior across relaunch and simultaneous windows.
- [ ] Define behavior when an existing split tree no longer fits the window. Preserve manual sidebar collapse and minimum pane dimensions; do not introduce automatic collapse or pane zoom.
- [ ] Map the specification's acceptance requirements to the implementation sequence, retaining useful Windows terminal regressions and identifying missing Unix/platform coverage.

Done when these decisions are explicit in the specification and the first extraction change has a bounded scope and observable acceptance criteria. This documentation baseline does not mark any of those decisions complete.

## After Phase 0

Follow the [specification's migration and build order](Spec.md#migration-and-build-order): neutral crates, a real persistent Mac terminal, workspace ownership and migration, the full workspace client with themes, shared daily-use qualification, then distribution.

The first implementation change should extract protocol/terminal boundaries while preserving existing terminal behavior and wire compatibility. Do not combine engine replacement, PTY migration, workspace migration, and UI redesign into one rewrite. Windows signing and installer qualification do not block the cross-platform foundation.

## Retained evidence

- [Windows terminal test recipes](testcmds.md): procedures for the existing implementation, to adapt during migration.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated observations, not current cross-platform qualification.

The superseded specification, status report, release targets, and detailed old roadmap remain available in Git history.

# AGENTS.md

## Project notes vault

- Before working on Compi, read the `jacksmainvault/Compi` notes, on Windows and macOS alike. Start with `feat and bugs.md` (scope, order, open items) and `References/` (UI references, scope rules). Then read `docs/dev/NEXT_STEPS.md` here.
- Windows: `C:\Users\johns\OneDrive\Documents\jacksmainvault\Compi`. macOS: the same `Documents/jacksmainvault/Compi` folder inside the synced OneDrive directory.
- The vault sets product scope and order; this repo's `docs/dev` holds the implementation detail. Borrow behaviour from references (Tern, Rex), never their layouts or look.

## README scope

- Keep `README.md` an evergreen introduction: what Compi is, how people and agents use it, a compact example, installation, and links to deeper documentation.
- Never add release or feature bloat: no version-by-version highlights, changelog entries, shipped-feature announcements, roadmaps, exhaustive feature lists, command tables, or implementation/qualification history. Shipping a feature does not require adding a README section or bullet.
- Put release details and version-specific upgrade warnings in `docs/user/release-notes.md`, installation detail in `docs/user/INSTALLATION.md`, and command syntax and behavior in `docs/user/USAGE.md`. Link instead of duplicating them.
- Keep necessary platform requirements and signing warnings beside downloads. Describe capabilities only when they help explain the core workflow; consolidate existing copy rather than appending to it.

## One dev instance per checkout

- Preview source changes only with `cargo dev`. Its instance is `compi-dev`, and it lives in `target/compi-dev`.
- Do not start other previews: no `compi --instance <name>` from `target/`, no `--target-dir` previews, and no portable copies.
- `cargo dev` and `cargo dev --stop` stop any Compi daemon or preview running from this checkout's target directory. Their shells end. The installed Compi and anything outside `target/` are never touched.
- Some qualification steps need another instance, for example the release and installer runs in `docs/dev/testcmds.md`. Stop that instance before you finish: `cargo run -p compi-client --example compi-probe -- --instance <name> shutdown`.
- Never stop the default instance or the installed Compi daemon.
- Leaving the `compi-dev` daemon running is fine. It is the one allowed instance.

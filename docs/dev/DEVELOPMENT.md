# Development

Requires Rust and the platform build tools. Windows also requires WSL2; macOS requires Xcode with the Metal compiler available through `xcrun`.

On Windows, prepare the pinned Microsoft runtime **before building or testing** (requires PowerShell 7 and HTTPS access to NuGet for the first download):

```powershell
pwsh -File tools/prepare-conpty.ps1
```

Preparation checks the pinned archive SHA256 and Microsoft Authenticode signatures, reuses only verified cached content, and stages `conpty.dll`, `OpenConsole.exe`, and `ConPTY-LICENSE.txt` in `target/conpty-runtime/x64`. The archive cache is `target/conpty-runtime/cache/`; the version, digest, package-signature provenance, and upstream license notice are recorded in `tools/prepare-conpty.ps1`. `-Architecture arm64` prepares the upstream ARM64 pair for a native ARM64 build; distributed Windows artifacts remain x64. Cargo copies the staged files beside the daemon and test executables, including custom target directories/profiles and explicit target triples. Compilation without preparation is allowed, but Windows terminal startup fails clearly if the bundled runtime is absent. The Windows installer build and both Windows CI workflows run preparation automatically.

### Develop the native client

```sh
cargo dev
```

`cargo dev` builds the client and daemon, opens an isolated preview window, and keeps watching. Save a file and the result appears in the preview:

- **Client** (`crates/compi-client`, `crates/compi-update`, bundled themes and fonts): incremental build of `compi`. If it compiles, the preview is closed and the new build opens against the same daemon, so shells, tabs, and the selected tab and pane survive; only the primary window reopens. If it fails, Cargo's diagnostics are printed and the running preview is left alone. If the new build compiles but exits before its window opens, the previous build is reopened and the broken one is not retried until you save again.
- **Appearance**: edit the dev `config.toml` (below) or use Settings in the preview. Theme, terminal theme, opacity, background effect, UI font, and terminal font family repaint within about half a second without a rebuild; user theme files are picked up within a few seconds. Bundled themes and fonts are compiled in and count as client changes.
- **Daemon** (`crates/compi-daemon`, `assets/compi-shell.sh`): the daemon is rebuilt and staged; the running dev daemon and its shells are kept. Press `r` Enter to restart it with the new build, which ends the dev shells.
- **Protocol** (`crates/compi-protocol`): both binaries are rebuilt, but the new client is held back because it may not understand the running daemon. Press `r` Enter to restart the dev daemon and open the new client.
- `Cargo.toml` and `Cargo.lock` rebuild both binaries. Changes to the runner itself (`tools/compi-dev`) need a `cargo dev` restart. `target/`, Markdown, editor swap files, and integration tests/examples are ignored. Bursts of writes are coalesced into one build.

Enter reopens a closed preview, `r` Enter restarts the dev daemon, and `q` Enter or Ctrl+C closes the preview and exits. The dev daemon and its shells keep running; the next `cargo dev` reattaches to them. `cargo dev --stop` closes the preview and stops the dev daemon, ending dev shells.

Everything runs as instance `compi-dev` from `target/compi-dev/`: runnable copies of the binaries in `bin/` (macOS: `Compi Dev.app`), state in `data/` (passed as `COMPI_DATA_DIR`), and a separate `config.toml` with update checks off. It never uses the installed Compi, its daemon, its configuration, or its state directory. Cargo still builds into the normal `target/debug` cache; because nothing runs from there, a running preview or dev daemon never blocks a build. `cargo clean` resets the dev environment. Run one `cargo dev` per user at a time.

On Windows, prepare ConPTY first (above). The dev daemon is started outside Cargo's process job, so closing the terminal ends the preview but not the dev shells. On macOS the preview runs from a minimal `Compi Dev.app` bundle, because the client's launch guard expects an app bundle.

The project allows one dev instance per checkout (see `AGENTS.md`). On start and on `--stop`, `cargo dev` stops every other Compi daemon or preview running from this checkout's target directory, such as hand-launched `--instance` previews, and prints what it stopped. Their shells end. The installed Compi and anything outside the target directory are never touched.

[Back to README](../../README.md)

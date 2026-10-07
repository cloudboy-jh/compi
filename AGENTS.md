# AGENTS.md

## One dev instance per checkout

- Preview source changes only with `cargo dev`. Its instance is `compi-dev`, and it lives in `target/compi-dev`.
- Do not start other previews: no `compi --instance <name>` from `target/`, no `--target-dir` previews, and no portable copies.
- `cargo dev` and `cargo dev --stop` stop any Compi daemon or preview running from this checkout's target directory. Their shells end. The installed Compi and anything outside `target/` are never touched.
- Some qualification steps need another instance, for example the release and installer runs in `docs/dev/testcmds.md`. Stop that instance before you finish: `cargo run -p compi-client --example compi-probe -- --instance <name> shutdown`.
- Never stop the default instance or the installed Compi daemon.
- Leaving the `compi-dev` daemon running is fine. It is the one allowed instance.

//! Maps repository paths to the process they can change.

/// What a source file feeds into. A file may feed more than one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Component {
    /// The GPUI client binary: `compi-client`, `compi-update`, bundled themes and fonts.
    Client,
    /// The daemon binary and the shell bridge it embeds.
    Daemon,
    /// The client/daemon wire contract. Both binaries depend on it.
    Protocol,
    /// This development runner.
    Runner,
}

/// Directories scanned beneath the workspace root. Everything else is irrelevant.
pub const WATCHED_ROOTS: [&str; 5] = [
    "crates",
    "assets",
    "tools/compi-dev",
    "Cargo.toml",
    "Cargo.lock",
];

/// Components a path (relative to the workspace, `/`-separated) can change.
/// Returns an empty slice for files that cannot affect a dev binary.
pub fn classify(path: &str) -> &'static [Component] {
    use Component::*;
    if is_ignored(path) {
        return &[];
    }
    match path {
        // Dependency changes rebuild both binaries; they do not hold the client the
        // way a wire-contract change does.
        "Cargo.toml" | "Cargo.lock" => return &[Client, Daemon],
        "assets/compi-shell.sh" => return &[Daemon],
        "assets/Compi-desktopappicon-v4.ico" => return &[Client, Daemon],
        _ => {}
    }
    if path.starts_with("tools/compi-dev/") {
        &[Runner]
    } else if path.starts_with("crates/compi-protocol/") {
        &[Protocol]
    } else if path.starts_with("crates/compi-daemon/") {
        &[Daemon]
    } else if path.starts_with("crates/compi-client/") || path.starts_with("crates/compi-update/") {
        &[Client]
    } else {
        &[]
    }
}

/// Whether a directory should not be descended into while scanning.
pub fn skip_directory(name: &str) -> bool {
    name == "target" || name.starts_with('.')
}

fn is_ignored(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| skip_directory(part)) {
        return true;
    }
    // A package's integration tests, examples and benches never feed the dev binaries:
    // `crates/<package>/tests/...`, `tools/<package>/examples/...`.
    if matches!(parts.first(), Some(&"crates" | &"tools"))
        && parts.len() > 3
        && matches!(parts[2], "tests" | "examples" | "benches")
    {
        return true;
    }
    let name = parts.last().copied().unwrap_or_default();
    name.starts_with('#')
        || name.ends_with('~')
        || name == "4913"
        || [".swp", ".swo", ".swx", ".tmp", ".bak", ".orig", ".md"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::{Component::*, classify};

    #[test]
    fn client_sources_and_bundled_assets_rebuild_the_client() {
        for path in [
            "crates/compi-client/src/gui/workspace.rs",
            "crates/compi-client/build.rs",
            "crates/compi-client/themes/one/one.json",
            "crates/compi-client/fonts/catalog.json",
            "crates/compi-update/src/lib.rs",
        ] {
            assert_eq!(classify(path), &[Client], "{path}");
        }
    }

    #[test]
    fn daemon_protocol_and_shared_inputs_are_distinguished() {
        assert_eq!(classify("crates/compi-daemon/src/daemon.rs"), &[Daemon]);
        assert_eq!(classify("assets/compi-shell.sh"), &[Daemon]);
        assert_eq!(classify("crates/compi-protocol/src/lib.rs"), &[Protocol]);
        assert_eq!(classify("Cargo.lock"), &[Client, Daemon]);
        assert_eq!(
            classify("assets/Compi-desktopappicon-v4.ico"),
            &[Client, Daemon]
        );
        assert_eq!(classify("tools/compi-dev/src/main.rs"), &[Runner]);
    }

    #[test]
    fn build_output_tests_docs_and_editor_debris_are_irrelevant() {
        for path in [
            "crates/compi-client/target/debug/compi.exe",
            "crates/compi-daemon/tests/daemon_integration.rs",
            "crates/compi-client/examples/probe.rs",
            "tools/compi-dev/tests/fixture.rs",
            "crates/compi-client/src/.workspace.rs.swp",
            "crates/compi-client/src/workspace.rs~",
            "crates/compi-client/src/4913",
            "crates/compi-client/README.md",
            "assets/compi-readme.png",
            "docs/dev/Spec.md",
            "tools/build-installer.ps1",
        ] {
            assert!(classify(path).is_empty(), "{path}");
        }
    }

    #[test]
    fn nested_test_modules_inside_src_still_count() {
        assert_eq!(
            classify("crates/compi-client/src/gui/tests/layout.rs"),
            &[Client]
        );
    }
}

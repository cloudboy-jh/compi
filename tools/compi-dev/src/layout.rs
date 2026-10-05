//! Where the isolated development runtime lives. Everything is under
//! `<target>/compi-dev`, so `cargo clean` resets it and nothing touches installed state.

use std::fs::File;
use std::path::{Path, PathBuf};

/// The only instance `cargo dev` uses. Never the default instance: on Windows that
/// would activate the installed daemon's scheduled task.
pub const INSTANCE: &str = "compi-dev";

const CONFIG_TEMPLATE: &str = "\
# Compi development preview (`cargo dev`). Appearance edits here apply live.
# This file is isolated from your installed Compi configuration.
version = 1

[updates]
automatic_checks = \"never\"
";

pub struct Layout {
    pub workspace: PathBuf,
    /// Cargo target directory. Any Compi process running from here, other than the dev
    /// runtime below, is a stray source preview.
    pub target: PathBuf,
    /// `<target>/compi-dev`
    pub root: PathBuf,
    /// `COMPI_DATA_DIR` for every dev process.
    pub data: PathBuf,
    pub config: PathBuf,
    /// Runnable binaries. Cargo never writes here, so running processes cannot block builds.
    pub bin: PathBuf,
    /// Daemon build waiting for an explicit restart.
    pub pending: PathBuf,
    /// Identity of the daemon installed in `bin`.
    pub stamp: PathBuf,
}

impl Layout {
    pub fn discover() -> Result<Self, String> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let workspace = simplify(
            workspace
                .canonicalize()
                .map_err(|error| format!("cannot locate workspace: {error}"))?,
        );
        let target = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(path) => workspace.join(path),
            None => workspace.join("target"),
        };
        Ok(Self::at(workspace, &target))
    }

    pub fn at(workspace: PathBuf, target: &Path) -> Self {
        let root = target.join("compi-dev");
        #[cfg(target_os = "macos")]
        let bin = root.join("Compi Dev.app/Contents/MacOS");
        #[cfg(not(target_os = "macos"))]
        let bin = root.join("bin");
        Self {
            workspace,
            target: target.to_owned(),
            data: root.join("data"),
            config: root.join("config.toml"),
            pending: root.join("pending"),
            stamp: root.join("daemon-stamp.json"),
            bin,
            root,
        }
    }

    pub fn client_exe(&self) -> PathBuf {
        self.bin.join(executable("compi"))
    }

    pub fn daemon_exe(&self) -> PathBuf {
        self.bin.join(executable("compi-daemon"))
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.data.join(format!("daemon-{INSTANCE}.log"))
    }

    /// Create the runtime directories and seed the dev config once.
    pub fn prepare(&self) -> Result<(), String> {
        let context = |path: &Path| {
            let path = path.display().to_string();
            move |error: std::io::Error| format!("{path}: {error}")
        };
        for directory in [&self.root, &self.bin, &self.pending] {
            std::fs::create_dir_all(directory).map_err(context(directory))?;
        }
        create_private_dir(&self.data).map_err(context(&self.data))?;
        if !self.config.exists() {
            std::fs::write(&self.config, CONFIG_TEMPLATE).map_err(context(&self.config))?;
        }
        #[cfg(target_os = "macos")]
        {
            let plist = self.root.join("Compi Dev.app/Contents/Info.plist");
            if !plist.exists() {
                std::fs::write(&plist, INFO_PLIST).map_err(context(&plist))?;
            }
        }
        Ok(())
    }

    /// One runner per checkout: a second one would fight over the same preview.
    pub fn lock_runner(&self) -> Result<File, String> {
        let path = self.root.join("runner.lock");
        let file = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err("another `cargo dev` is already running for this checkout".into())
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }
}

/// The `executable` name with the platform suffix.
pub fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Drop the Windows `\\?\` prefix so paths stay readable and accepted everywhere.
pub fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && !rest.starts_with("UNC\\")
    {
        return PathBuf::from(rest);
    }
    path
}

/// Canonical, prefix-free, and (on Windows) case-folded form for path comparison.
fn normalized(path: &Path) -> PathBuf {
    let path = simplify(path.canonicalize().unwrap_or_else(|_| path.to_owned()));
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path
    }
}

/// Whether two paths name the same file (case-insensitively on Windows).
pub fn same_file(left: &Path, right: &Path) -> bool {
    normalized(left) == normalized(right)
}

/// Whether `path` is inside `directory` (case-insensitively on Windows).
pub fn is_within(path: &Path, directory: &Path) -> bool {
    normalized(path).starts_with(normalized(directory))
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        // `data_dir()` rejects state directories readable by other users.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}

#[cfg(target_os = "macos")]
const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>Compi Dev</string>
    <key>CFBundleName</key><string>Compi Dev</string>
    <key>CFBundleExecutable</key><string>compi</string>
    <key>CFBundleIdentifier</key><string>com.compi.dev-preview</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.0.0</string>
    <key>CFBundleVersion</key><string>0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dev_path_stays_inside_the_dev_root() {
        let target = std::env::temp_dir().join("compi-dev-layout-target");
        let layout = Layout::at(std::env::temp_dir(), &target);
        let root = target.join("compi-dev");
        for path in [
            &layout.data,
            &layout.config,
            &layout.bin,
            &layout.pending,
            &layout.stamp,
            &layout.client_exe(),
            &layout.daemon_exe(),
            &layout.daemon_log(),
        ] {
            assert!(path.starts_with(&root), "{}", path.display());
        }
        // The client finds its daemon only as a sibling executable.
        assert_eq!(layout.client_exe().parent(), layout.daemon_exe().parent());
    }

    #[test]
    fn dev_instance_is_a_valid_non_default_instance() {
        let names = compi_protocol::identity::instance_names(Some(INSTANCE)).unwrap();
        let default = compi_protocol::identity::instance_names(None).unwrap();
        assert_ne!(names.pipe, default.pipe);
        assert_ne!(names.mutex, default.mutex);
    }

    #[test]
    fn seeded_config_disables_update_checks_and_is_kept_afterwards() {
        let target = std::env::temp_dir().join(format!("compi-dev-layout-{}", std::process::id()));
        let layout = Layout::at(std::env::temp_dir(), &target);
        layout.prepare().unwrap();
        let seeded = std::fs::read_to_string(&layout.config).unwrap();
        // The client refuses launches from an unversioned config.
        assert!(seeded.lines().any(|line| line == "version = 1"));
        assert!(seeded.contains("automatic_checks = \"never\""));

        std::fs::write(&layout.config, "[appearance]\ntheme = \"one-light\"\n").unwrap();
        layout.prepare().unwrap();
        assert!(
            std::fs::read_to_string(&layout.config)
                .unwrap()
                .contains("one-light")
        );
        let _ = std::fs::remove_dir_all(&target);
    }
}

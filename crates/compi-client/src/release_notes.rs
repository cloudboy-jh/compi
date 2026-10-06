//! The running version's embedded release notes and their per-version seen state.
//!
//! The seen state is global to the process: every window reads it at render time,
//! and a window that changes it must notify the others.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::LazyLock;

const SOURCE: &str = include_str!("../../../docs/release-notes.md");
const STATE_FILE: &str = "release-notes.json";
const MAX_STATE_BYTES: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseNotes {
    pub version: String,
    pub summary: Vec<String>,
    pub details: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub bullets: Vec<String>,
}

impl ReleaseNotes {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/cloudboy-jh/compi/releases/tag/v{}",
            self.version
        )
    }
}

static NOTES: LazyLock<Option<ReleaseNotes>> =
    LazyLock::new(|| parse(SOURCE, env!("CARGO_PKG_VERSION")));
static SEEN: LazyLock<Mutex<Seen>> = LazyLock::new(|| Mutex::new(load()));

/// Notes for the running version; `None` when the embedded notes are for another
/// version or malformed.
pub fn current() -> Option<&'static ReleaseNotes> {
    NOTES.as_ref()
}

/// Settles the seen state before this process writes any window-slot state, so a
/// fresh install is told apart from an upgrade.
pub fn initialize() {
    LazyLock::force(&SEEN);
}

/// `# <version>`, summary bullets, then `## <section>` headings with bullets.
/// Any other line makes the notes malformed.
pub fn parse(source: &str, running: &str) -> Option<ReleaseNotes> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let version = lines.next()?.strip_prefix("# ")?.trim();
    if version.is_empty() || version != running {
        return None;
    }
    let mut summary = Vec::new();
    let mut details: Vec<Section> = Vec::new();
    for line in lines {
        if let Some(title) = line.strip_prefix("## ") {
            let title = title.trim();
            if title.is_empty() || details.last().is_some_and(|last| last.bullets.is_empty()) {
                return None;
            }
            details.push(Section {
                title: title.to_owned(),
                bullets: Vec::new(),
            });
        } else {
            // Anything but a heading or a bullet is malformed.
            let bullet = line.strip_prefix("- ")?.replace("**", "").trim().to_owned();
            if bullet.is_empty() {
                return None;
            }
            match details.last_mut() {
                Some(section) => section.bullets.push(bullet),
                None => summary.push(bullet),
            }
        }
    }
    if summary.is_empty() || details.last().is_some_and(|last| last.bullets.is_empty()) {
        return None;
    }
    Some(ReleaseNotes {
        version: version.to_owned(),
        summary,
        details,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    #[serde(default)]
    pub dot_cleared: Option<String>,
    #[serde(default)]
    pub dismissed: Option<String>,
}

impl Seen {
    fn recorded(version: &str) -> Self {
        Self {
            dot_cleared: Some(version.to_owned()),
            dismissed: Some(version.to_owned()),
        }
    }

    pub fn shows_dot(&self, version: &str) -> bool {
        self.dot_cleared.as_deref() != Some(version)
    }

    pub fn shows_card(&self, version: &str) -> bool {
        self.dismissed.as_deref() != Some(version)
    }
}

pub fn seen() -> Seen {
    SEEN.lock().clone()
}

/// Returns whether the state changed; windows must be notified when it did.
pub fn clear_dot(version: &str) -> bool {
    update(|seen| seen.dot_cleared = Some(version.to_owned()))
}

/// Returns whether the state changed; windows must be notified when it did.
pub fn dismiss(version: &str) -> bool {
    update(|seen| seen.dismissed = Some(version.to_owned()))
}

fn update(change: impl FnOnce(&mut Seen)) -> bool {
    let mut seen = SEEN.lock();
    let before = seen.clone();
    change(&mut seen);
    if *seen == before {
        return false;
    }
    // A failed write only means the notes show again next launch; the choice
    // already applies to every window of this process.
    if let Ok(data) = compi_protocol::paths::data_dir() {
        let _ = persist(&data.join(STATE_FILE), &seen);
    }
    true
}

fn load() -> Seen {
    let Some(notes) = current() else {
        return Seen::default();
    };
    match compi_protocol::paths::data_dir() {
        Ok(data) => load_in(&data, &notes.version),
        Err(_) => Seen::default(),
    }
}

fn load_in(data: &Path, version: &str) -> Seen {
    let path = data.join(STATE_FILE);
    let stored = read(&path);
    // 0.1.3 never wrote this file, so only prior window state marks an upgrade.
    let prior_window_state = stored.is_none() && prior_window_state(data);
    let (seen, record) = resolve(stored, prior_window_state, version);
    if record {
        let _ = persist(&path, &seen);
    }
    seen
}

/// `stored` is `None` when the file is missing or unreadable. Returns the
/// effective state and whether it must be recorded.
fn resolve(stored: Option<Seen>, prior_window_state: bool, version: &str) -> (Seen, bool) {
    match stored {
        Some(seen) => (seen, false),
        None if prior_window_state => (Seen::default(), false),
        None => (Seen::recorded(version), true),
    }
}

fn read(path: &Path) -> Option<Seen> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Any saved window slot (`primary.json` or `aux-*.json`) under the client state root.
fn prior_window_state(data: &Path) -> bool {
    let mut pending = vec![data.join("client-state-v1")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(crate::client_state::valid_slot_id)
            {
                return true;
            }
        }
    }
    false
}

fn persist(path: &Path, seen: &Seen) -> crate::Result<()> {
    let bytes = serde_json::to_vec_pretty(seen)?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> crate::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        crate::client_state::replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOTES: &str = "# 0.1.4\n\n- Floating terminals\n- Arrange panes\n\n## New\n\n- **Floating terminals:** float any pane.\n- Plain bullet\n\n## Limitations\n\n- Bash and **Zsh** only.\n";

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "compi-release-notes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_heading_summary_and_sections_without_bold_markers() {
        let notes = parse(NOTES, "0.1.4").unwrap();
        assert_eq!(notes.version, "0.1.4");
        assert_eq!(notes.summary, ["Floating terminals", "Arrange panes"]);
        assert_eq!(
            notes.details,
            [
                Section {
                    title: "New".into(),
                    bullets: vec![
                        "Floating terminals: float any pane.".into(),
                        "Plain bullet".into()
                    ],
                },
                Section {
                    title: "Limitations".into(),
                    bullets: vec!["Bash and Zsh only.".into()],
                },
            ]
        );
        assert_eq!(
            notes.url(),
            "https://github.com/cloudboy-jh/compi/releases/tag/v0.1.4"
        );
    }

    #[test]
    fn notes_for_another_version_are_ignored() {
        assert_eq!(parse(NOTES, "0.1.5"), None);
    }

    /// The release workflow runs these tests, so a version bump without matching
    /// notes, or a notes line the app cannot show, fails before packaging.
    #[test]
    fn shipped_notes_parse_for_this_version() {
        let notes = parse(SOURCE, env!("CARGO_PKG_VERSION"))
            .expect("docs/release-notes.md must start with `# <workspace version>` and use only headings and bullets");
        assert!(!notes.summary.is_empty());
    }

    #[test]
    fn malformed_notes_are_ignored() {
        for source in [
            "",
            "- No heading\n",
            "## 0.1.4\n- Wrong heading level\n",
            "# 0.1.4\n",
            "# 0.1.4\n## New\n- Details without a summary\n",
            "# 0.1.4\n- Summary\nA stray paragraph.\n",
            "# 0.1.4\n- Summary\n## Empty\n## New\n- Detail\n",
            "# 0.1.4\n- Summary\n## Trailing empty section\n",
        ] {
            assert_eq!(parse(source, "0.1.4"), None, "{source:?}");
        }
    }

    #[test]
    fn fresh_install_records_silently() {
        let directory = Directory::new();
        let seen = load_in(&directory.0, "0.1.4");
        assert!(!seen.shows_dot("0.1.4"));
        assert!(!seen.shows_card("0.1.4"));
        // Window state saved later this launch does not turn it into an upgrade.
        write_slot(&directory.0);
        assert_eq!(load_in(&directory.0, "0.1.4"), seen);
    }

    #[test]
    fn upgrade_with_prior_window_state_shows_dot_and_card() {
        let directory = Directory::new();
        write_slot(&directory.0);
        let seen = load_in(&directory.0, "0.1.4");
        assert!(seen.shows_dot("0.1.4"));
        assert!(seen.shows_card("0.1.4"));
        // Nothing is recorded until the user acts, so it shows again next launch.
        assert!(load_in(&directory.0, "0.1.4").shows_card("0.1.4"));
    }

    #[test]
    fn unrelated_files_are_not_prior_window_state() {
        let directory = Directory::new();
        let root = directory.0.join("client-state-v1").join("primary-instance");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("registry.lock"), b"").unwrap();
        fs::write(root.join("primary.corrupt-0.json"), b"{").unwrap();
        assert!(!load_in(&directory.0, "0.1.4").shows_card("0.1.4"));
    }

    #[test]
    fn opening_the_sidebar_clears_only_the_dot() {
        let directory = Directory::new();
        write_slot(&directory.0);
        let path = directory.0.join(STATE_FILE);
        let mut seen = load_in(&directory.0, "0.1.4");
        seen.dot_cleared = Some("0.1.4".into());
        persist(&path, &seen).unwrap();
        let seen = load_in(&directory.0, "0.1.4");
        assert!(!seen.shows_dot("0.1.4"));
        assert!(seen.shows_card("0.1.4"));
    }

    #[test]
    fn dismissing_hides_the_card() {
        let directory = Directory::new();
        write_slot(&directory.0);
        let mut seen = load_in(&directory.0, "0.1.4");
        seen.dismissed = Some("0.1.4".into());
        persist(&directory.0.join(STATE_FILE), &seen).unwrap();
        assert!(!load_in(&directory.0, "0.1.4").shows_card("0.1.4"));
    }

    #[test]
    fn a_newer_version_shows_after_an_older_one_was_dismissed() {
        let directory = Directory::new();
        persist(&directory.0.join(STATE_FILE), &Seen::recorded("0.1.4")).unwrap();
        let seen = load_in(&directory.0, "0.1.5");
        assert!(seen.shows_dot("0.1.5"));
        assert!(seen.shows_card("0.1.5"));
    }

    fn write_slot(data: &Path) {
        let root = data
            .join("client-state-v1")
            .join("primary-instance")
            .join("server-6162");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("primary.json"), b"{}").unwrap();
    }
}

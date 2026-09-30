//! Standard Zed theme families and a bounded, headless local installation API.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    theme::{TerminalPalette, ThemeColors, ThemeDefinition, ThemeId, ThemeMetadata, ThemePreset},
    theme_file::{self, MAX_BYTES, ThemeAppearance, ThemeFamily},
    theme_migration,
};

const MAX_IMPORTED: usize = 256;
const INDEX_FILE: &str = ".index.json";
static MUTATIONS: Mutex<()> = Mutex::new(());
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
static BUNDLED: LazyLock<Arc<Vec<Arc<ThemeDefinition>>>> = LazyLock::new(|| {
    Arc::new(
        ThemePreset::ALL
            .into_iter()
            .map(ThemePreset::definition)
            .collect(),
    )
});

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    bytes: u64,
}

impl FileStamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        metadata.is_file().then_some(Self {
            modified: metadata.modified().ok()?,
            bytes: metadata.len(),
        })
    }
}

#[derive(Clone)]
struct CachedRecord {
    stamp: FileStamp,
    family: Arc<ThemeFamily>,
    definitions: Vec<Arc<ThemeDefinition>>,
}

// Only legacy identity overrides live here. Theme colors and required public
// metadata remain in ordinary Zed JSON, including files copied into the directory.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityIndex {
    version: u32,
    files: BTreeMap<String, FamilyIdentities>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyIdentities {
    name: String,
    author: String,
    variants: BTreeMap<String, ThemeId>,
}

/// Immutable snapshots retain both resolved variants and their original JSON.
/// File import and the future CLI use this same installation API. It never
/// changes accepted appearance, starts a GUI, or connects to a daemon.
#[derive(Clone)]
pub struct ThemeLibrary {
    root: Option<Arc<PathBuf>>,
    entries: Arc<Vec<Arc<ThemeDefinition>>>,
    records: Arc<HashMap<PathBuf, CachedRecord>>,
    index_stamp: Option<FileStamp>,
}

impl ThemeLibrary {
    pub fn load() -> (Self, Vec<String>) {
        match compi_protocol::paths::data_dir() {
            Ok(data) => {
                let (mut library, mut diagnostics) = Self::load_from(data.join("themes"));
                diagnostics.extend(library.migrate_directory(&data.join("themes-v1")));
                (library, diagnostics)
            }
            Err(error) => (
                Self {
                    root: None,
                    entries: BUNDLED.clone(),
                    records: Arc::default(),
                    index_stamp: None,
                },
                vec![format!("Cannot locate local themes: {error}")],
            ),
        }
    }

    pub fn load_from(root: PathBuf) -> (Self, Vec<String>) {
        let mut library = Self {
            root: Some(Arc::new(root.clone())),
            entries: BUNDLED.clone(),
            records: Arc::default(),
            index_stamp: None,
        };
        let mut diagnostics = library.migrate_directory(&root);
        diagnostics.extend(library.reload());
        (library, diagnostics)
    }

    pub fn directory(&self) -> Option<&Path> {
        self.root.as_deref().map(PathBuf::as_path)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Arc<ThemeDefinition>> {
        self.entries.iter()
    }

    pub fn resolve(&self, id: &ThemeId) -> Option<Arc<ThemeDefinition>> {
        self.entries
            .iter()
            .find(|entry| entry.theme_id() == id)
            .cloned()
    }

    pub fn fallback(&self) -> Arc<ThemeDefinition> {
        BUNDLED
            .iter()
            .find(|entry| entry.id() == ThemePreset::CompiNeutral.id())
            .expect("neutral bundled theme")
            .clone()
    }

    /// Install all variants in a Zed JSON file. Identical variants are a no-op;
    /// conflicting variants are rejected rather than replacing a user's theme.
    pub fn import_file(&mut self, path: &Path) -> Result<Vec<Arc<ThemeDefinition>>, String> {
        check_extension(path)?;
        let family = read_family(path, false)?;
        self.install_family(&family, None)
    }

    fn install_family(
        &mut self,
        incoming: &ThemeFamily,
        legacy_id: Option<&ThemeId>,
    ) -> Result<Vec<Arc<ThemeDefinition>>, String> {
        // Validate all variants, including any that would be merged, before I/O.
        theme_file::encode(incoming)?;
        let _process = MUTATIONS
            .lock()
            .map_err(|_| "Theme store lock is unavailable")?;
        let root = self
            .root
            .clone()
            .ok_or("No local theme directory is available")?;
        let _disk = lock_store(&root)?;
        self.reload_locked(&root)?;
        let mut index = read_index(&root)?;
        let matches: Vec<_> = self
            .records
            .iter()
            .filter(|(_, record)| {
                record.family.name == incoming.name && record.family.author == incoming.author
            })
            .collect();
        if matches.len() > 1 {
            return Err(
                "This theme family is split across several files; manage those files separately"
                    .into(),
            );
        }
        let (destination, mut family) = if let Some((path, record)) = matches.first() {
            let current = read_family(path, true)?;
            if current != *record.family {
                return Err("Theme family changed on disk; reload before installing".into());
            }
            if current.extra != incoming.extra {
                return Err("Theme family already exists with different metadata".into());
            }
            ((*path).clone(), current)
        } else {
            let destination = root.join(format!("family-{:016x}.json", family_hash(incoming)));
            if destination.exists() {
                return Err("Theme family filename collides with an existing file".into());
            }
            let mut family = incoming.clone();
            family.themes.clear();
            (destination, family)
        };
        let mut changed = false;
        for variant in &incoming.themes {
            if let Some(existing) = family
                .themes
                .iter()
                .find(|other| other.name == variant.name)
            {
                if existing != variant {
                    return Err(format!(
                        "Theme {:?} already exists with different data",
                        variant.name
                    ));
                }
            } else {
                family.themes.push(variant.clone());
                changed = true;
            }
        }
        let filename = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Theme filename is not valid UTF-8")?
            .to_owned();
        let mut index_changed = false;
        if let Some(id) = legacy_id {
            if incoming.themes.len() != 1 {
                return Err("Legacy migration requires one variant".into());
            }
            let identities =
                index
                    .files
                    .entry(filename.clone())
                    .or_insert_with(|| FamilyIdentities {
                        name: family.name.clone(),
                        author: family.author.clone(),
                        variants: BTreeMap::new(),
                    });
            if identities.name != family.name || identities.author != family.author {
                return Err("Stored theme identity metadata conflicts with migration".into());
            }
            index_changed = identities.variants.get(&incoming.themes[0].name) != Some(id);
            identities
                .variants
                .insert(incoming.themes[0].name.clone(), id.clone());
        }
        let definitions = resolve_family(&family, index.files.get(&filename), None)?;
        let existing_count = self
            .records
            .get(&destination)
            .map_or(0, |record| record.definitions.len());
        if self.entries.len() - ThemePreset::ALL.len() - existing_count + definitions.len()
            > MAX_IMPORTED
        {
            return Err(format!(
                "Local theme limit reached ({MAX_IMPORTED} variants)"
            ));
        }
        for definition in &definitions {
            if self.entries.iter().any(|entry| {
                entry.theme_id() == definition.theme_id()
                    && !self.records.get(&destination).is_some_and(|record| {
                        record
                            .definitions
                            .iter()
                            .any(|old| old.theme_id() == entry.theme_id())
                    })
            }) {
                return Err(format!(
                    "Theme identity collision for {:?}",
                    definition.label()
                ));
            }
        }
        let ids: Vec<_> = incoming
            .themes
            .iter()
            .map(|variant| {
                definitions
                    .iter()
                    .find(|definition| definition.label() == variant.name)
                    .expect("resolved incoming variant")
                    .theme_id()
                    .clone()
            })
            .collect();
        if changed || index_changed {
            let bytes = theme_file::encode(&family)?;
            let previous = if destination.exists() {
                Some(read_bytes(&destination, true)?)
            } else {
                None
            };
            let index_bytes = if index_changed {
                index.version = 1;
                Some(serde_json::to_vec_pretty(&index).map_err(|error| error.to_string())?)
            } else {
                None
            };
            atomic_write(&destination, &bytes)?;
            if let Some(index_bytes) = index_bytes
                && let Err(error) = atomic_write(&root.join(INDEX_FILE), &index_bytes)
            {
                let rollback = match previous {
                    Some(bytes) => atomic_write(&destination, &bytes),
                    None => fs::remove_file(&destination).map_err(|error| error.to_string()),
                };
                return Err(match rollback {
                    Ok(()) => error,
                    Err(rollback) => {
                        format!("{error}; restoring the theme also failed: {rollback}")
                    }
                });
            }
            self.reload_locked(&root)?;
        }
        ids.into_iter()
            .map(|id| {
                self.resolve(&id)
                    .ok_or_else(|| format!("Installed theme '{}' could not be loaded", id.id()))
            })
            .collect()
    }

    pub fn export_file(&self, id: &ThemeId, path: &Path) -> Result<(), String> {
        check_extension(path)?;
        let definition = self
            .resolve(id)
            .ok_or_else(|| format!("Theme '{}' is not installed", id.id()))?;
        let mut family = if let Some(record) = self.records.values().find(|record| {
            record
                .definitions
                .iter()
                .any(|theme| theme.theme_id() == id)
        }) {
            record.family.as_ref().clone()
        } else {
            let preset =
                ThemePreset::parse(id.id()).ok_or("Bundled theme source is unavailable")?;
            theme_file::parse(preset.theme_file().as_bytes())?
        };
        family
            .themes
            .retain(|variant| variant.name == definition.label());
        let bytes = theme_file::encode(&family)?;
        let _process = MUTATIONS
            .lock()
            .map_err(|_| "Theme store lock is unavailable")?;
        if let Some(root) = &self.root
            && let (Ok(root), Ok(parent)) =
                (root.canonicalize(), parent_directory(path).canonicalize())
            && root == parent
        {
            return Err("Export outside the theme directory, then import the file".into());
        }
        atomic_write(path, &bytes)
    }

    /// Remove only the selected variant; sibling variants and their references
    /// remain untouched. The catalog protects references to the selected ID.
    pub fn remove(&mut self, id: &ThemeId) -> Result<(), String> {
        let definition = self
            .resolve(id)
            .ok_or_else(|| format!("Theme '{}' is not installed", id.id()))?;
        if !definition.is_imported() {
            return Err("Bundled themes cannot be removed".into());
        }
        let (path, expected) = self
            .records
            .iter()
            .find(|(_, record)| {
                record
                    .definitions
                    .iter()
                    .any(|theme| theme.theme_id() == id)
            })
            .map(|(path, record)| (path.clone(), record.family.clone()))
            .ok_or("Installed theme source is unavailable")?;
        let _process = MUTATIONS
            .lock()
            .map_err(|_| "Theme store lock is unavailable")?;
        let root = self
            .root
            .clone()
            .ok_or("No local theme directory is available")?;
        let _disk = lock_store(&root)?;
        let mut family = read_family(&path, true)?;
        if family != *expected {
            return Err("Theme family changed on disk; reload before removing it".into());
        }
        family
            .themes
            .retain(|variant| variant.name != definition.label());
        if family.themes.is_empty() {
            fs::remove_file(&path)
                .map_err(|error| format!("Cannot remove {}: {error}", path.display()))?;
        } else {
            atomic_write(&path, &theme_file::encode(&family)?)?;
        }
        self.reload_locked(&root)?;
        Ok(())
    }

    pub fn reload(&mut self) -> Vec<String> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let result = (|| {
            let _process = MUTATIONS
                .lock()
                .map_err(|_| "Theme store lock is unavailable")?;
            let _disk = lock_store(&root)?;
            self.reload_locked(&root)
        })();
        result.unwrap_or_else(|error: String| {
            vec![format!("{error}; previous theme snapshot retained")]
        })
    }

    fn reload_locked(&mut self, root: &Path) -> Result<Vec<String>, String> {
        let index_stamp = FileStamp::read(&root.join(INDEX_FILE));
        let index = read_index(root)?;
        let mut paths = Vec::new();
        for entry in
            fs::read_dir(root).map_err(|error| format!("Cannot read theme directory: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("Cannot inspect theme directory: {error}"))?;
            if is_theme_path(&entry.path()) {
                paths.push(entry.path());
            }
        }
        paths.sort();
        if index_stamp == self.index_stamp
            && paths.len() == self.records.len()
            && paths.iter().all(|path| {
                self.records
                    .get(path)
                    .is_some_and(|record| FileStamp::read(path).as_ref() == Some(&record.stamp))
            })
        {
            return Ok(Vec::new());
        }
        let mut diagnostics = Vec::new();
        let mut entries = BUNDLED.as_ref().clone();
        let mut records = HashMap::new();
        let mut ids: HashSet<_> = entries
            .iter()
            .map(|entry| entry.theme_id().clone())
            .collect();
        for path in paths {
            let previous = self.records.get(&path);
            let stamp = FileStamp::read(&path);
            let record: Result<CachedRecord, String> = (|| {
                let stamp = stamp.ok_or("Theme is not a regular file")?;
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("Invalid theme filename")?;
                if index_stamp == self.index_stamp
                    && let Some(previous) = previous.filter(|previous| previous.stamp == stamp)
                {
                    return Ok(previous.clone());
                }
                let family = Arc::new(read_family(&path, true)?);
                let definitions = resolve_family(&family, index.files.get(filename), previous)?;
                Ok(CachedRecord {
                    stamp,
                    family,
                    definitions,
                })
            })();
            let record = match record {
                Ok(record) => record,
                Err(error) => {
                    diagnostics.push(format!("Cannot load {}: {error}", path.display()));
                    // A partial/invalid external edit must not replace a known-good family.
                    let Some(previous) = previous else {
                        continue;
                    };
                    previous.clone()
                }
            };
            if entries.len() - ThemePreset::ALL.len() + record.definitions.len() > MAX_IMPORTED {
                diagnostics.push(format!(
                    "Ignored {}: local limit is {MAX_IMPORTED} variants",
                    path.display()
                ));
                continue;
            }
            if record
                .definitions
                .iter()
                .any(|definition| ids.contains(definition.theme_id()))
            {
                diagnostics.push(format!(
                    "Ignored {}: duplicate theme identity",
                    path.display()
                ));
                continue;
            }
            for definition in &record.definitions {
                ids.insert(definition.theme_id().clone());
                entries.push(definition.clone());
            }
            records.insert(path, record);
        }
        entries[ThemePreset::ALL.len()..].sort_by(|a, b| a.id().cmp(b.id()));
        if self.entries.len() != entries.len()
            || self
                .entries
                .iter()
                .zip(&entries)
                .any(|(a, b)| !Arc::ptr_eq(a, b))
        {
            self.entries = Arc::new(entries);
        }
        self.records = Arc::new(records);
        self.index_stamp = index_stamp;
        Ok(diagnostics)
    }

    fn migrate_directory(&mut self, legacy_root: &Path) -> Vec<String> {
        let paths = match fs::read_dir(legacy_root) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(theme_migration::LEGACY_SUFFIX))
                })
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => return vec![format!("Cannot inspect legacy theme library: {error}")],
        };
        let mut diagnostics = Vec::new();
        for path in paths {
            // Standard JSON is valid under any .json filename, including an old suffix.
            if read_family(&path, true).is_ok() {
                continue;
            }
            let result: Result<(), String> = (|| {
                let migrated = {
                    let _disk = lock_store(legacy_root)?;
                    theme_migration::read(&path)?
                };
                let expected = migrated.family.resolve(0)?;
                let installed = self.install_family(&migrated.family, Some(&migrated.id))?;
                let definition = installed
                    .first()
                    .ok_or("Legacy variant was not installed")?;
                let actual = resolved_arrays(definition);
                if definition.theme_id() != &migrated.id
                    || actual.0 != expected.application
                    || actual.1 != expected.terminal
                {
                    return Err("Legacy theme verification failed; original retained".into());
                }
                let _disk = lock_store(legacy_root)?;
                if theme_migration::read(&path)?.original != migrated.original {
                    return Err("Legacy theme changed during migration; original retained".into());
                }
                let backup_root = legacy_root.join(".migrated");
                fs::create_dir_all(&backup_root).map_err(|error| error.to_string())?;
                let backup = backup_root.join(path.file_name().ok_or("Invalid legacy filename")?);
                if backup.exists() {
                    if read_bytes(&backup, true)? != migrated.original {
                        return Err("Legacy migration backup differs; original retained".into());
                    }
                    fs::remove_file(&path).map_err(|error| error.to_string())?;
                } else {
                    fs::rename(&path, &backup).map_err(|error| error.to_string())?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                diagnostics.push(format!("Cannot migrate {}: {error}", path.display()));
            }
        }
        diagnostics
    }
}

fn resolved_arrays(definition: &ThemeDefinition) -> ([u32; 9], [u32; 20]) {
    let a = definition.colors();
    let t = definition.terminal();
    let application = [
        a.background,
        a.surface,
        a.surface_hover,
        a.border,
        a.foreground,
        a.muted,
        a.accent,
        a.error,
        a.selection,
    ];
    let mut terminal = [0; 20];
    terminal[..4].copy_from_slice(&[t.background, t.foreground, t.selection, t.cursor]);
    terminal[4..].copy_from_slice(&t.ansi);
    (application, terminal)
}

fn resolve_family(
    family: &ThemeFamily,
    identities: Option<&FamilyIdentities>,
    previous: Option<&CachedRecord>,
) -> Result<Vec<Arc<ThemeDefinition>>, String> {
    if identities.is_some_and(|identities| {
        identities.name != family.name || identities.author != family.author
    }) {
        return Err("Theme identity metadata does not match its family".into());
    }
    family
        .themes
        .iter()
        .enumerate()
        .map(|(position, variant)| {
            let resolved = family.resolve(position)?;
            let id = identities
                .and_then(|identities| identities.variants.get(&variant.name))
                .cloned()
                .unwrap_or_else(|| variant_id(family, &variant.name));
            let [
                background,
                surface,
                surface_hover,
                border,
                foreground,
                muted,
                accent,
                error,
                selection,
            ] = resolved.application;
            let mut ansi = [0; 16];
            ansi.copy_from_slice(&resolved.terminal[4..]);
            let metadata = |key: &str, fallback: &str| {
                variant
                    .extra
                    .get(key)
                    .or_else(|| family.extra.get(key))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(fallback)
                    .to_owned()
                    .into()
            };
            let definition = ThemeDefinition {
                id,
                application: ThemeColors {
                    background,
                    surface,
                    surface_hover,
                    border,
                    foreground,
                    muted,
                    accent,
                    error,
                    selection,
                },
                terminal: TerminalPalette {
                    background: resolved.terminal[0],
                    foreground: resolved.terminal[1],
                    selection: resolved.terminal[2],
                    cursor: resolved.terminal[3],
                    ansi,
                },
                metadata: ThemeMetadata {
                    name: variant.name.clone().into(),
                    family: family.name.clone().into(),
                    description: metadata("description", "Imported Zed theme"),
                    dark: variant.appearance == ThemeAppearance::Dark,
                    author: if family.author.trim().is_empty() {
                        "Unspecified".into()
                    } else {
                        family.author.clone().into()
                    },
                    source: metadata("source", "Local Zed theme file"),
                    license: metadata("license", "Not supplied; check the source before sharing"),
                    notices: metadata("notices", ""),
                },
                imported: true,
            };
            if let Some(existing) = previous.and_then(|record| {
                record
                    .definitions
                    .iter()
                    .find(|existing| existing.as_ref() == &definition)
            }) {
                Ok(existing.clone())
            } else {
                Ok(Arc::new(definition))
            }
        })
        .collect()
}

fn stable_hash(parts: &[&str]) -> u64 {
    // Stable across builds and filenames. Full family/variant identity is checked
    // on collisions; this is an identifier, never an integrity/security digest.
    let mut hash = 0xcbf29ce484222325_u64;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn family_hash(family: &ThemeFamily) -> u64 {
    stable_hash(&[&family.author, &family.name])
}

fn variant_id(family: &ThemeFamily, name: &str) -> ThemeId {
    ThemeId::parse(&format!(
        "user-zed-{:016x}",
        stable_hash(&[&family.author, &family.name, name])
    ))
    .expect("generated valid theme identity")
}

fn read_index(root: &Path) -> Result<IdentityIndex, String> {
    let path = root.join(INDEX_FILE);
    if !path.exists() {
        return Ok(IdentityIndex {
            version: 1,
            files: BTreeMap::new(),
        });
    }
    let index: IdentityIndex = serde_json::from_slice(&read_bytes(&path, true)?)
        .map_err(|error| format!("Invalid private theme identity index: {error}"))?;
    if index.version != 1
        || index
            .files
            .values()
            .map(|family| family.variants.len())
            .sum::<usize>()
            > MAX_IMPORTED
    {
        return Err("Invalid private theme identity index version or size".into());
    }
    for (filename, identities) in &index.files {
        if !is_plain_filename(filename)
            || identities
                .variants
                .values()
                .any(|id| !id.id().starts_with("user-"))
        {
            return Err("Invalid private theme identity record".into());
        }
    }
    Ok(index)
}

fn is_plain_filename(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.starts_with('.')
        && Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn is_theme_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_plain_filename)
}

fn check_extension(path: &Path) -> Result<(), String> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return Err("Theme files must be Zed .json files".into());
    }
    Ok(())
}

fn read_family(path: &Path, managed: bool) -> Result<ThemeFamily, String> {
    theme_file::parse(&read_bytes(path, managed)?)
        .map_err(|error| format!("Cannot load {}: {error}", path.display()))
}

fn read_bytes(path: &Path, managed: bool) -> Result<Vec<u8>, String> {
    let metadata = if managed {
        fs::symlink_metadata(path)
    } else {
        fs::metadata(path)
    }
    .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("Theme must be a regular file no larger than 2 MiB".into());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("Theme must be a regular file no larger than 2 MiB".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Theme exceeds 2 MiB".into());
    }
    Ok(bytes)
}

fn lock_store(root: &Path) -> Result<File, String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("Cannot create {}: {error}", root.display()))?;
    let path = root.join("mutations.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err("Theme lock must be a regular file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect theme lock: {error}")),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(&path)
        .map_err(|error| format!("Cannot open theme store lock: {error}"))?;
    lock.lock()
        .map_err(|error| format!("Cannot lock theme store: {error}"))?;
    Ok(lock)
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("Cannot replace a non-regular theme destination".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect theme destination: {error}")),
    }
    let parent = parent_directory(destination);
    let (temporary, mut file) = loop {
        let serial = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".compi-theme-{}-{serial}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot create temporary theme file: {error}")),
        }
    };
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("Cannot save {}: {error}", destination.display()))
}

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)?;
    if let Ok(parent) = File::open(parent_directory(destination)) {
        let _ = parent.sync_all();
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let wide = |path: &Path| -> std::io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "NUL in theme path",
            ));
        }
        value.push(0);
        Ok(value)
    };
    let temporary = wide(temporary)?;
    let destination = wide(destination)?;
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "compi-zed-test-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn library(&self) -> ThemeLibrary {
            ThemeLibrary::load_from(self.0.join("store")).0
        }

        fn fixture(&self, name: &str, variants: &[(&str, &str)]) -> PathBuf {
            let path = self.0.join(format!("{name}.json"));
            fs::write(
                &path,
                serde_json::to_vec_pretty(&family_data(name, variants)).unwrap(),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn family_data(name: &str, variants: &[(&str, &str)]) -> Value {
        json!({
            "$schema": "https://zed.dev/schema/themes/v0.2.0.json",
            "name": name,
            "author": "A local author",
            "themes": variants.iter().map(|(name, appearance)| json!({
                "name": name,
                "appearance": appearance,
                "license": "MIT",
                "notices": "Original local notice\nSecond line",
                "style": {
                    "background": "#102030",
                    "surface.background": "#223344",
                    "text": "#f0f1f2",
                    "text.accent": "#abcdef",
                    "element.selected": "#11223340",
                    "terminal.background": "#456789",
                    "terminal.foreground": "#decafe",
                    "terminal.ansi.red": "#ff112288",
                    "players": [{"cursor": "#778899", "selection": "#44556680"}],
                    "syntax": {"comment": {"color": "#234567", "font_style": "italic", "font_weight": 400}}
                }
            })).collect::<Vec<_>>()
        })
    }

    fn legacy_data(id: &str) -> Value {
        let a = ThemePreset::CompiNeutral.colors();
        let t = ThemePreset::CompiNeutral.terminal();
        let hex = |value| format!("#{value:06x}");
        json!({
            "version": 1, "id": id, "name": "Migrated Neutral", "family": "Legacy family",
            "description": "My preserved palette", "mode": "dark", "author": "Legacy author",
            "source": "Original local source", "license": "MIT", "notices": "Preserved license notice",
            "application": {
                "background": hex(a.background), "surface": hex(a.surface), "surface_hover": hex(a.surface_hover),
                "border": hex(a.border), "foreground": hex(a.foreground), "muted": hex(a.muted),
                "accent": hex(a.accent), "error": hex(a.error), "selection": hex(a.selection)
            },
            "terminal": {
                "background": hex(t.background), "foreground": hex(t.foreground),
                "selection": hex(t.selection), "cursor": hex(t.cursor),
                "ansi": t.ansi.map(hex)
            }
        })
    }

    #[test]
    fn family_import_export_retains_alpha_unknown_styles_and_sibling_variants() {
        let directory = Directory::new();
        let source = directory.fixture(
            "Local family",
            &[("Local Dark", "dark"), ("Local Light", "light")],
        );
        let original = fs::read(&source).unwrap();
        let mut library = directory.library();
        let themes = library.import_file(&source).unwrap();
        let dark = themes
            .iter()
            .find(|theme| theme.label() == "Local Dark")
            .unwrap();
        let light = themes
            .iter()
            .find(|theme| theme.label() == "Local Light")
            .unwrap();
        assert_eq!(dark.colors().background, 0x102030);
        assert_eq!(dark.terminal().background, 0x456789);
        assert_eq!(dark.terminal().ansi[1], 0x77ff1122);
        assert_eq!(dark.terminal().selection, 0x7f445566);
        let clone = library.clone();
        let export = directory.0.join("share.json");
        library.export_file(dark.theme_id(), &export).unwrap();
        let exported: Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
        assert_eq!(
            exported["themes"][0]["style"]["syntax"]["comment"]["font_style"],
            "italic"
        );
        assert_eq!(exported["themes"][0]["license"], "MIT");
        assert_eq!(
            exported["themes"][0]["notices"],
            "Original local notice\nSecond line"
        );
        let mut other = ThemeLibrary::load_from(directory.0.join("other")).0;
        let imported = other.import_file(&export).unwrap();
        assert_eq!(imported[0].theme_id(), dark.theme_id());
        assert_eq!(imported[0].colors(), dark.colors());
        assert_eq!(imported[0].terminal(), dark.terminal());
        assert_eq!(
            library.import_file(&export).unwrap()[0].theme_id(),
            dark.theme_id()
        );
        library.remove(dark.theme_id()).unwrap();
        assert!(library.resolve(dark.theme_id()).is_none());
        assert_eq!(
            library.resolve(light.theme_id()).unwrap().terminal(),
            light.terminal()
        );
        assert!(clone.resolve(dark.theme_id()).is_some());
        let (reloaded, diagnostics) = ThemeLibrary::load_from(directory.0.join("store"));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(reloaded.resolve(dark.theme_id()).is_none());
        assert!(reloaded.resolve(light.theme_id()).is_some());
        assert_eq!(fs::read(source).unwrap(), original);
    }

    #[test]
    fn bundled_export_preserves_palette_and_complete_license_notices() {
        let directory = Directory::new();
        let mut library = directory.library();
        let path = directory.0.join("tokyo.json");
        library
            .export_file(&ThemePreset::TokyoNight.into(), &path)
            .unwrap();
        let imported = library.import_file(&path).unwrap();
        assert_eq!(imported[0].colors(), ThemePreset::TokyoNight.colors());
        assert_eq!(imported[0].terminal(), ThemePreset::TokyoNight.terminal());
        assert_eq!(
            imported[0].metadata.notices.lines().collect::<Vec<_>>(),
            crate::theme::THEME_ATTRIBUTION.lines().collect::<Vec<_>>(),
        );
        assert!(library.remove(&ThemePreset::TokyoNight.into()).is_err());
    }

    #[test]
    fn invalid_later_variant_and_conflicting_family_do_not_partially_install() {
        let directory = Directory::new();
        let source = directory.fixture("Collision", &[("Existing", "dark")]);
        let mut library = directory.library();
        let existing = library.import_file(&source).unwrap().remove(0);
        let managed = library.records.keys().next().unwrap().clone();
        let original = fs::read(&managed).unwrap();
        let mut data = family_data(
            "Collision",
            &[("New sibling", "light"), ("Existing", "dark")],
        );
        data["themes"][1]["style"]["text"] = json!("not a color");
        fs::write(&source, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(library.import_file(&source).is_err());
        data["themes"][1]["style"]["text"] = json!("#abcdef");
        fs::write(&source, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(library.import_file(&source).is_err());
        assert_eq!(fs::read(&managed).unwrap(), original);
        assert_eq!(
            library.resolve(existing.theme_id()).unwrap().colors(),
            existing.colors()
        );
        assert!(
            !library
                .entries()
                .any(|theme| theme.label() == "New sibling")
        );
    }

    #[test]
    fn identical_import_refreshes_stale_snapshot_without_replacing_files() {
        let directory = Directory::new();
        let source = directory.fixture("First", &[("First Dark", "dark")]);
        let mut first = directory.library();
        let mut stale = directory.library();
        let original = first.import_file(&source).unwrap().remove(0);
        let other = first
            .import_file(&directory.fixture("Second", &[("Second Dark", "dark")]))
            .unwrap()
            .remove(0);
        let noop = stale.import_file(&source).unwrap();
        assert_eq!(noop[0].theme_id(), original.theme_id());
        assert!(stale.resolve(other.theme_id()).is_some());
        assert_eq!(
            stale.resolve(original.theme_id()).unwrap().colors(),
            original.colors()
        );
    }

    #[test]
    fn direct_copy_rename_and_valid_or_invalid_edits_preserve_stable_selection() {
        let directory = Directory::new();
        let mut library = directory.library();
        let path = directory.0.join("store").join("any-filename.json");
        let mut data = family_data("Direct copy", &[("Editable Dark", "dark")]);
        fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(library.reload().is_empty());
        let retained = library
            .entries()
            .find(|theme| theme.label() == "Editable Dark")
            .unwrap()
            .clone();
        let renamed = path.with_file_name("renamed.json");
        fs::rename(&path, &renamed).unwrap();
        data["themes"][0]["style"]["text.accent"] = json!("#12345678");
        fs::write(&renamed, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(library.reload().is_empty());
        assert_eq!(
            library
                .resolve(retained.theme_id())
                .unwrap()
                .colors()
                .accent,
            0x87123456
        );
        assert_eq!(retained.colors().accent, 0xabcdef);
        fs::write(&renamed, b"{partial").unwrap();
        assert!(!library.reload().is_empty());
        assert_eq!(
            library
                .resolve(retained.theme_id())
                .unwrap()
                .colors()
                .accent,
            0x87123456
        );
    }

    #[test]
    fn migration_preserves_saved_identity_colors_and_attribution_then_retires_old_record() {
        let directory = Directory::new();
        let root = directory.0.join("store");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("user-kept.compi-theme.json");
        let bytes = serde_json::to_vec(&legacy_data("user-kept")).unwrap();
        fs::write(&source, &bytes).unwrap();
        let (library, diagnostics) = ThemeLibrary::load_from(root.clone());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let id = ThemeId::parse("user-kept").unwrap();
        let migrated = library.resolve(&id).unwrap();
        assert_eq!(migrated.colors(), ThemePreset::CompiNeutral.colors());
        assert_eq!(migrated.terminal(), ThemePreset::CompiNeutral.terminal());
        assert!(migrated.attribution().contains("Preserved license notice"));
        assert!(migrated.attribution().contains("Original local source"));
        assert!(!source.exists());
        assert_eq!(
            fs::read(root.join(".migrated").join("user-kept.compi-theme.json")).unwrap(),
            bytes
        );
        let (reloaded, diagnostics) = ThemeLibrary::load_from(root);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(reloaded.resolve(&id).unwrap().as_ref(), migrated.as_ref());
        let external_legacy = directory.0.join("legacy.json");
        fs::write(&external_legacy, bytes).unwrap();
        let mut library = library;
        assert!(library.import_file(&external_legacy).is_err());
    }

    #[test]
    fn whole_family_capacity_failure_leaves_existing_entries_intact() {
        let directory = Directory::new();
        let variants: Vec<_> = (0..255).map(|index| format!("Existing {index}")).collect();
        let references: Vec<_> = variants
            .iter()
            .map(|name| (name.as_str(), "dark"))
            .collect();
        let source = directory.fixture("Bulk", &references);
        let mut library = directory.library();
        let retained = library.import_file(&source).unwrap();
        let incoming =
            directory.fixture("Extra", &[("Extra Dark", "dark"), ("Extra Light", "light")]);
        assert!(library.import_file(&incoming).is_err());
        assert!(
            retained
                .iter()
                .all(|theme| library.resolve(theme.theme_id()).is_some())
        );
        assert!(
            !library
                .entries()
                .any(|theme| theme.label().starts_with("Extra "))
        );
    }

    #[test]
    fn failed_exports_preserve_existing_destination_bytes() {
        let directory = Directory::new();
        let library = directory.library();
        let destination = directory.0.join("existing.json");
        fs::write(&destination, b"previous contents").unwrap();
        assert!(
            library
                .export_file(&ThemeId::parse("user-missing").unwrap(), &destination)
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"previous contents");
        #[cfg(windows)]
        {
            let original = fs::metadata(&destination).unwrap().permissions();
            let mut readonly = original.clone();
            readonly.set_readonly(true);
            fs::set_permissions(&destination, readonly).unwrap();
            assert!(
                library
                    .export_file(&ThemePreset::CompiNeutral.into(), &destination)
                    .is_err()
            );
            assert_eq!(fs::read(&destination).unwrap(), b"previous contents");
            fs::set_permissions(&destination, original).unwrap();
        }
        let missing_parent = directory.0.join("missing").join("export.json");
        assert!(
            library
                .export_file(&ThemePreset::CompiNeutral.into(), &missing_parent)
                .is_err()
        );
        assert!(!missing_parent.exists());
    }

    #[cfg(windows)]
    #[test]
    fn migration_index_commit_failure_rolls_back_new_document_and_keeps_legacy_source() {
        let directory = Directory::new();
        let root = directory.0.join("store");
        fs::create_dir_all(&root).unwrap();
        let index = root.join(INDEX_FILE);
        fs::write(&index, br#"{"version":1,"files":{}}"#).unwrap();
        let permissions = fs::metadata(&index).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&index, readonly).unwrap();
        let legacy = root.join("user-rollback.compi-theme.json");
        let bytes = serde_json::to_vec(&legacy_data("user-rollback")).unwrap();
        fs::write(&legacy, &bytes).unwrap();
        let (library, diagnostics) = ThemeLibrary::load_from(root.clone());
        fs::set_permissions(index, permissions).unwrap();
        assert!(!diagnostics.is_empty());
        assert_eq!(fs::read(legacy).unwrap(), bytes);
        assert!(
            library
                .resolve(&ThemeId::parse("user-rollback").unwrap())
                .is_none()
        );
        assert!(
            !fs::read_dir(root)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().starts_with("family-"))
        );
    }

    #[test]
    fn differing_concurrent_family_imports_have_one_winner_and_no_mixed_palette() {
        use std::sync::Barrier;
        let directory = Directory::new();
        let first_source = directory.fixture("Race", &[("Race Dark", "dark")]);
        let mut changed = family_data("Race", &[("Race Dark", "dark")]);
        changed["themes"][0]["style"]["text.accent"] = json!("#123456");
        let second_source = directory.0.join("second.json");
        fs::write(&second_source, serde_json::to_vec(&changed).unwrap()).unwrap();
        let first = directory.library();
        let second = directory.library();
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [(first, first_source), (second, second_source)]
            .into_iter()
            .map(|(mut library, source)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    library.import_file(&source)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let winner = results.into_iter().find_map(Result::ok).unwrap();
        let reloaded = directory.library();
        assert_eq!(
            reloaded
                .resolve(winner[0].theme_id())
                .unwrap()
                .colors()
                .accent,
            winner[0].colors().accent
        );
        assert_eq!(
            reloaded.resolve(winner[0].theme_id()).unwrap().terminal(),
            winner[0].terminal()
        );
    }
}

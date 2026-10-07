//! Headless, authenticated full-package updates. All filesystem/network calls are blocking.
mod archive;
mod transaction;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub use transaction::*;

pub type Result<T, E = Error> = std::result::Result<T, E>;
#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}
pub(crate) fn err(message: impl Into<String>) -> Error {
    Error(message.into())
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(err("Update cancelled before activation"))
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdatePhase {
    Checking,
    Downloading,
    Verifying,
    Staging,
    Ready,
    Activating,
    Relaunching,
    RollingBack,
    Complete,
}
#[derive(Debug, Clone)]
pub struct UpdateEvent {
    pub phase: UpdatePhase,
    pub completed: u64,
    pub total: Option<u64>,
    pub message: String,
}
pub(crate) fn event(
    phase: UpdatePhase,
    completed: u64,
    total: Option<u64>,
    message: impl Into<String>,
) -> UpdateEvent {
    UpdateEvent {
        phase,
        completed,
        total,
        message: message.into(),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}
/// Signed Windows Setup for the same release. Published only in `compi-setup-<platform>.json`:
/// clients through 0.1.5 deny unknown manifest fields, so the update manifest never carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupArtifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: u32,
    pub product: String,
    pub version: String,
    pub platform: String,
    pub minimum_os: String,
    pub release_notes: String,
    pub daemon_protocol: u32,
    pub qualified_daemon_versions: Vec<String>,
    pub minimum_persistence: u32,
    pub artifact: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupArtifact>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub payload: String,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableRelease {
    pub manifest: ReleaseManifest,
    pub manifest_bytes: Vec<u8>,
    pub signature: String,
}
#[derive(Debug, Clone)]
pub struct ReleaseConfig {
    pub repository: String,
    pub public_key: [u8; 32],
    pub platform: String,
    pub current_version: String,
}
impl ReleaseConfig {
    pub fn compiled() -> Result<Self> {
        let encoded=option_env!("COMPI_UPDATE_PUBLIC_KEY").filter(|s|!s.is_empty()).ok_or_else(||err("Trusted updates are not configured: rebuild with COMPI_UPDATE_PUBLIC_KEY containing the publisher's base64 Ed25519 public key; unsigned updates are disabled"))?;
        let public_key = STANDARD
            .decode(encoded)
            .map_err(|_| err("Invalid COMPI_UPDATE_PUBLIC_KEY base64"))?
            .try_into()
            .map_err(|_| err("COMPI_UPDATE_PUBLIC_KEY must be 32 bytes"))?;
        Ok(Self {
            repository: option_env!("COMPI_UPDATE_REPOSITORY")
                .unwrap_or("cloudboy-jh/compi")
                .into(),
            public_key,
            platform: platform()?.into(),
            current_version: env!("CARGO_PKG_VERSION").into(),
        })
    }
}
pub fn platform() -> Result<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("macos-aarch64")
    } else {
        Err(err(
            "Updates support Windows x64 and Apple Silicon macOS only",
        ))
    }
}
const MAX_METADATA: u64 = 256 * 1024;
const MAX_ARTIFACT: u64 = 4 * 1024 * 1024 * 1024;
/// Update-qualified presentation format; changing it requires an explicit reversible migration.
pub const SUPPORTED_PERSISTENCE_VERSION: u32 = 5;
pub fn verify_manifest(bytes: &[u8], config: &ReleaseConfig) -> Result<AvailableRelease> {
    let release = verify_signed(bytes, config)?;
    let version = semver::Version::parse(&release.manifest.version)
        .map_err(|_| err("Invalid update version"))?;
    let current = semver::Version::parse(&config.current_version)
        .map_err(|_| err("Invalid current version"))?;
    if version <= current {
        return Err(err("Stable update is stale, a prerelease, or a downgrade"));
    }
    Ok(release)
}
/// Authenticity and shape only; repair may legitimately fetch the release already installed.
fn verify_signed(bytes: &[u8], config: &ReleaseConfig) -> Result<AvailableRelease> {
    if bytes.len() as u64 > MAX_METADATA {
        return Err(err("Update metadata exceeds size limit"));
    }
    let signed: SignedManifest = serde_json::from_slice(bytes)?;
    let payload = STANDARD
        .decode(&signed.payload)
        .map_err(|_| err("Invalid manifest encoding"))?;
    let signature = STANDARD
        .decode(&signed.signature)
        .map_err(|_| err("Invalid signature encoding"))?;
    let signature =
        Signature::from_slice(&signature).map_err(|_| err("Invalid Ed25519 signature"))?;
    VerifyingKey::from_bytes(&config.public_key)
        .map_err(|_| err("Invalid release public key"))?
        .verify_strict(&payload, &signature)
        .map_err(|_| err("Update publisher signature rejected"))?;
    let manifest: ReleaseManifest = serde_json::from_slice(&payload)?;
    if manifest.schema != 1 || manifest.product != "compi" || manifest.platform != config.platform {
        return Err(err("Unsupported metadata schema, product, or platform"));
    }
    let version =
        semver::Version::parse(&manifest.version).map_err(|_| err("Invalid update version"))?;
    if !version.pre.is_empty() {
        return Err(err("Stable update is stale, a prerelease, or a downgrade"));
    }
    validate_manifest(&manifest)?;
    Ok(AvailableRelease {
        manifest,
        manifest_bytes: payload,
        signature: signed.signature,
    })
}
pub(crate) fn validate_manifest(m: &ReleaseManifest) -> Result<()> {
    if m.artifact.size == 0
        || m.artifact.size > MAX_ARTIFACT
        || m.artifact.sha256.len() != 64
        || !m.artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(err("Invalid artifact size or SHA-256"));
    }
    trusted_asset_url(&m.artifact.url)?;
    if let Some(setup) = &m.setup {
        if setup.size == 0
            || setup.size > MAX_ARTIFACT
            || setup.sha256.len() != 64
            || !setup.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(err("Invalid Setup size or SHA-256"));
        }
        trusted_asset_url(&setup.url)?;
    }
    validate_minimum_os(m)?;
    if m.minimum_persistence != SUPPORTED_PERSISTENCE_VERSION {
        return Err(err(
            "This update has not qualified the current presentation persistence format for safe rollback",
        ));
    }
    if m.daemon_protocol == 0 {
        return Err(err("Missing daemon protocol qualification"));
    }
    Ok(())
}
fn trusted_asset_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| err("Invalid artifact URL"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(err("Update artifact must be an HTTPS GitHub release asset"));
    }
    Ok(())
}
fn validate_minimum_os(m: &ReleaseManifest) -> Result<()> {
    let required = m
        .minimum_os
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| err("Minimum OS must be a numeric version"))?;
    if required.is_empty() || required.len() > 3 {
        return Err(err("Invalid minimum OS version"));
    }
    let mut actual = vec![
        if m.platform == "windows-x86_64" {
            10
        } else {
            14
        },
        0,
        0,
    ];
    #[cfg(windows)]
    if m.platform == "windows-x86_64" {
        #[repr(C)]
        struct VersionInfo {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            service_pack: [u16; 128],
        }
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn RtlGetVersion(info: *mut VersionInfo) -> i32;
        }
        let mut info = VersionInfo {
            size: std::mem::size_of::<VersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        if unsafe { RtlGetVersion(&mut info) } != 0 {
            return Err(err("Could not determine Windows version safely"));
        }
        actual = vec![info.major, info.minor, info.build];
    }
    #[cfg(target_os = "macos")]
    if m.platform == "macos-aarch64" {
        let output = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()?;
        if !output.status.success() {
            return Err(err("Could not determine macOS version"));
        }
        actual = String::from_utf8_lossy(&output.stdout)
            .trim()
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| err("Unrecognized macOS version"))?;
    }
    let mut required = required;
    required.resize(3, 0);
    actual.resize(3, 0);
    if actual < required {
        return Err(err(format!(
            "This update requires {} {}; current OS does not qualify",
            m.platform, m.minimum_os
        )));
    }
    Ok(())
}
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}
#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}
pub struct UpdateManager {
    config: ReleaseConfig,
    client: reqwest::blocking::Client,
}
impl UpdateManager {
    pub fn new() -> Result<Self> {
        Self::with_config(ReleaseConfig::compiled()?)
    }
    pub fn with_config(config: ReleaseConfig) -> Result<Self> {
        VerifyingKey::from_bytes(&config.public_key)
            .map_err(|_| err("Invalid release public key"))?;
        if config.repository.split('/').count() != 2
            || config
                .repository
                .bytes()
                .any(|c| !(c.is_ascii_alphanumeric() || b"/-_.".contains(&c)))
        {
            return Err(err("Invalid GitHub release repository"));
        }
        Ok(Self {
            config,
            client: http_client()?,
        })
    }
    pub fn check(
        &mut self,
        cancel: &Cancellation,
        mut progress: impl FnMut(UpdateEvent),
    ) -> Result<Option<AvailableRelease>> {
        cancel.check()?;
        progress(event(
            UpdatePhase::Checking,
            0,
            None,
            "Checking latest published stable release",
        ));
        let Some((release, tagged)) = self.latest_published(cancel)? else {
            return Ok(None);
        };
        let current = semver::Version::parse(&self.config.current_version)
            .map_err(|_| err("Current version is invalid"))?;
        if tagged <= current {
            return Ok(None);
        }
        let name = format!("compi-update-{}.json", self.config.platform);
        let bytes = self.signed_asset(&release, &name, cancel)?.ok_or_else(|| {
            err("Published release has no signed update metadata for this platform")
        })?;
        let available = verify_manifest(&bytes, &self.config)?;
        self.check_published_identity(&available, &tagged)?;
        Ok(Some(available))
    }
    /// The latest published stable release with its signed Windows Setup, even when it is the
    /// version already running: repair must always be able to obtain current maintenance code.
    pub fn latest_setup_release(&mut self, cancel: &Cancellation) -> Result<AvailableRelease> {
        cancel.check()?;
        let (release, tagged) = self
            .latest_published(cancel)?
            .ok_or_else(|| err("No Compi release is published yet"))?;
        let name = format!("compi-setup-{}.json", self.config.platform);
        let bytes = self
            .signed_asset(&release, &name, cancel)?
            .ok_or_else(|| err("The latest release has no repair tool for this platform"))?;
        let available = verify_signed(&bytes, &self.config)?;
        self.check_published_identity(&available, &tagged)?;
        if available.manifest.setup.is_none() {
            return Err(err(
                "The latest release has no repair tool for this platform",
            ));
        }
        Ok(available)
    }
    fn latest_published(
        &self,
        cancel: &Cancellation,
    ) -> Result<Option<(GithubRelease, semver::Version)>> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.config.repository
        );
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|e| err(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let release: GithubRelease =
            serde_json::from_slice(&bounded_response(response, MAX_METADATA, cancel)?)?;
        if release.draft || release.prerelease {
            return Err(err("GitHub returned an unpublished or prerelease update"));
        }
        let tagged = semver::Version::parse(release.tag_name.trim_start_matches('v'))
            .map_err(|_| err("Release tag is not a semantic version"))?;
        Ok(Some((release, tagged)))
    }
    fn signed_asset(
        &self,
        release: &GithubRelease,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<Option<Vec<u8>>> {
        let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else {
            return Ok(None);
        };
        trusted_asset_url(&asset.browser_download_url)?;
        bounded_response(
            self.client
                .get(&asset.browser_download_url)
                .send()
                .map_err(|e| err(e.to_string()))?,
            MAX_METADATA,
            cancel,
        )
        .map(Some)
    }
    fn check_published_identity(
        &self,
        available: &AvailableRelease,
        tagged: &semver::Version,
    ) -> Result<()> {
        if semver::Version::parse(&available.manifest.version)
            .ok()
            .as_ref()
            != Some(tagged)
        {
            return Err(err("Signed version does not match published release tag"));
        }
        check_release_owner(&available.manifest, &self.config.repository)
    }
    pub fn prepare(
        &mut self,
        release: &AvailableRelease,
        target: &InstallTarget,
        cancel: &Cancellation,
        mut progress: impl FnMut(UpdateEvent),
    ) -> Result<PreparedUpdate> {
        // Verify again: callers cannot substitute a deserialized/tampered AvailableRelease.
        let envelope = SignedManifest {
            payload: STANDARD.encode(&release.manifest_bytes),
            signature: release.signature.clone(),
        };
        let release = verify_manifest(&serde_json::to_vec(&envelope)?, &self.config)?;
        let _lock = OperationLock::acquire(&target.root)?;
        recover_locked(target)?;
        ensure_newer_than_selected(target, &release.manifest.version)?;
        let attempt = unique_id();
        let state = state_dir(&target.root);
        fs::create_dir_all(&state)?;
        let owned = state.join(format!("stage-{attempt}"));
        fs::create_dir(&owned)?;
        let result = (|| {
            let download = owned.join("package.zip");
            let artifact = &release.manifest.artifact;
            let completed = download_verified(
                &self.client,
                Download {
                    url: &artifact.url,
                    size: artifact.size,
                    sha256: &artifact.sha256,
                    path: &download,
                    message: "Downloading verified full package",
                },
                cancel,
                &mut progress,
            )?;
            let payload = owned.join("payload");
            archive::extract(&download, &payload, cancel, &mut progress)?;
            validate_payload(target, &payload)?;
            cancel.check()?;
            let journal = Journal::prepared(
                target.clone(),
                attempt.clone(),
                release.manifest.clone(),
                payload.clone(),
                envelope.clone(),
            )?;
            let journal_path = state.join("journal.json");
            atomic_json(&journal_path, &journal)?;
            progress(event(
                UpdatePhase::Ready,
                completed,
                Some(completed),
                "Verified package ready; installation requires explicit consent",
            ));
            Ok(PreparedUpdate {
                journal_path,
                target: target.clone(),
                version: release.manifest.version.clone(),
                stage: payload,
            })
        })();
        if result.is_err() {
            fs::remove_dir_all(&owned).map_err(|cleanup| {
                err(format!(
                    "Staging failed; owned staging cleanup failed: {cleanup}"
                ))
            })?;
        }
        result
    }
    pub fn launch_worker(
        &self,
        prepared: &PreparedUpdate,
        request: &RelaunchRequest,
    ) -> Result<std::process::Child> {
        launch_worker(prepared, request)
    }
}
fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Compi/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("Non-HTTPS update redirect")
            } else if attempt.previous().len() > 10 {
                attempt.error("Too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| err(e.to_string()))
}
fn check_release_owner(manifest: &ReleaseManifest, repository: &str) -> Result<()> {
    let owned = format!("https://github.com/{repository}/releases/download/");
    if !manifest.artifact.url.starts_with(&owned)
        || manifest
            .setup
            .as_ref()
            .is_some_and(|setup| !setup.url.starts_with(&owned))
    {
        return Err(err(
            "Signed artifact is not owned by configured release repository",
        ));
    }
    Ok(())
}
struct Download<'a> {
    url: &'a str,
    size: u64,
    sha256: &'a str,
    path: &'a Path,
    message: &'static str,
}
/// Streams a signed asset to a new file and returns its verified length.
fn download_verified(
    client: &reqwest::blocking::Client,
    download: Download<'_>,
    cancel: &Cancellation,
    progress: &mut impl FnMut(UpdateEvent),
) -> Result<u64> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(download.path)?;
    let mut response = client
        .get(download.url)
        .send()
        .map_err(|e| err(e.to_string()))?
        .error_for_status()
        .map_err(|e| err(e.to_string()))?;
    if response
        .content_length()
        .is_some_and(|n| n != download.size)
    {
        return Err(err("Artifact content length differs from signed metadata"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut completed = 0;
    loop {
        cancel.check()?;
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        completed += n as u64;
        if completed > download.size {
            return Err(err("Artifact exceeds signed size"));
        }
        file.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        progress(event(
            UpdatePhase::Downloading,
            completed,
            Some(download.size),
            download.message,
        ));
    }
    file.sync_all()?;
    drop(file);
    cancel.check()?;
    progress(event(
        UpdatePhase::Verifying,
        completed,
        Some(completed),
        "Verifying SHA-256",
    ));
    if completed != download.size
        || format!("{:x}", hash.finalize()) != download.sha256.to_ascii_lowercase()
    {
        return Err(err(
            "Downloaded file does not match its signed size or SHA-256",
        ));
    }
    Ok(completed)
}
/// Downloads the Setup named in a signed release into `destination` (a directory) and
/// returns its path. The release is re-verified against the compiled publisher key, so a
/// deserialized or edited `AvailableRelease` cannot substitute another executable.
pub fn download_verified_setup(
    release: &AvailableRelease,
    destination: &Path,
    cancel: &Cancellation,
    mut progress: impl FnMut(UpdateEvent),
) -> Result<PathBuf> {
    let config = ReleaseConfig::compiled()?;
    let envelope = SignedManifest {
        payload: STANDARD.encode(&release.manifest_bytes),
        signature: release.signature.clone(),
    };
    let release = verify_signed(&serde_json::to_vec(&envelope)?, &config)?;
    check_release_owner(&release.manifest, &config.repository)?;
    let setup = release
        .manifest
        .setup
        .as_ref()
        .ok_or_else(|| err("This release has no repair tool for this platform"))?;
    fs::create_dir_all(destination)?;
    let path = destination.join(format!("Compi-{}-Setup.exe", release.manifest.version));
    let partial = path.with_extension("partial");
    if partial.exists() {
        fs::remove_file(&partial)?;
    }
    let result = download_verified(
        &http_client()?,
        Download {
            url: &setup.url,
            size: setup.size,
            sha256: &setup.sha256,
            path: &partial,
            message: "Downloading repair tool",
        },
        cancel,
        &mut progress,
    );
    if let Err(error) = result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}
fn bounded_response(
    response: reqwest::blocking::Response,
    max: u64,
    cancel: &Cancellation,
) -> Result<Vec<u8>> {
    let mut response = response
        .error_for_status()
        .map_err(|e| err(e.to_string()))?;
    if response.content_length().is_some_and(|n| n > max) {
        return Err(err("Network response exceeds size limit"));
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        cancel.check()?;
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > max {
            return Err(err("Network response exceeds size limit"));
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::RngCore;
    fn signed() -> (ReleaseConfig, ReleaseManifest, SigningKey) {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        let config = ReleaseConfig {
            repository: "cloudboy-jh/compi".into(),
            public_key: *key.verifying_key().as_bytes(),
            platform: "windows-x86_64".into(),
            current_version: "1.0.0".into(),
        };
        let manifest = ReleaseManifest {
            schema: 1,
            product: "compi".into(),
            version: "1.1.0".into(),
            platform: config.platform.clone(),
            minimum_os: "10".into(),
            release_notes: "New release".into(),
            daemon_protocol: 14,
            qualified_daemon_versions: vec!["1.0.0".into()],
            minimum_persistence: SUPPORTED_PERSISTENCE_VERSION,
            artifact: Artifact {
                url: "https://github.com/cloudboy-jh/compi/releases/download/v1.1.0/package.zip"
                    .into(),
                size: 1,
                sha256: "00".repeat(32),
            },
            setup: None,
        };
        (config, manifest, key)
    }
    fn envelope(manifest: &ReleaseManifest, key: &SigningKey) -> Vec<u8> {
        let bytes = serde_json::to_vec(manifest).unwrap();
        serde_json::to_vec(&SignedManifest {
            payload: STANDARD.encode(&bytes),
            signature: STANDARD.encode(key.sign(&bytes).to_bytes()),
        })
        .unwrap()
    }
    #[test]
    fn rejects_tampering_wrong_platform_and_downgrade() {
        let (config, mut manifest, key) = signed();
        assert_eq!(
            verify_manifest(&envelope(&manifest, &key), &config)
                .unwrap()
                .manifest
                .version,
            "1.1.0"
        );
        let mut bytes: SignedManifest = serde_json::from_slice(&envelope(&manifest, &key)).unwrap();
        manifest.version = "9.0.0".into();
        bytes.payload = STANDARD.encode(serde_json::to_vec(&manifest).unwrap());
        assert!(verify_manifest(&serde_json::to_vec(&bytes).unwrap(), &config).is_err());
        manifest.platform = "macos-aarch64".into();
        assert!(verify_manifest(&envelope(&manifest, &key), &config).is_err());
        manifest.platform = config.platform.clone();
        manifest.version = "0.9.9".into();
        assert!(verify_manifest(&envelope(&manifest, &key), &config).is_err());
        manifest.version = "1.1.0-beta.1".into();
        assert!(verify_manifest(&envelope(&manifest, &key), &config).is_err());
    }
    #[test]
    fn setup_is_optional_and_never_emitted_into_update_manifests() {
        let (config, mut manifest, key) = signed();
        // Update metadata stays byte-compatible with clients that deny unknown fields.
        let legacy = serde_json::to_value(&manifest).unwrap();
        assert!(legacy.get("setup").is_none());
        let parsed: ReleaseManifest = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.setup, None);
        manifest.setup = Some(SetupArtifact {
            url: "https://github.com/cloudboy-jh/compi/releases/download/v1.1.0/Compi-Setup.exe"
                .into(),
            size: 42,
            sha256: "ab".repeat(32),
        });
        let release = verify_manifest(&envelope(&manifest, &key), &config).unwrap();
        assert_eq!(release.manifest.setup, manifest.setup);
        check_release_owner(&release.manifest, &config.repository).unwrap();
    }
    #[test]
    fn setup_must_be_well_formed_and_owned_by_the_release_repository() {
        let (config, mut manifest, key) = signed();
        manifest.setup = Some(SetupArtifact {
            url: "https://github.com/cloudboy-jh/compi/releases/download/v1.1.0/Compi-Setup.exe"
                .into(),
            size: 42,
            sha256: "not-a-digest".into(),
        });
        assert!(verify_manifest(&envelope(&manifest, &key), &config).is_err());
        manifest.setup = Some(SetupArtifact {
            url: "https://github.com/someone-else/compi/releases/download/v1.1.0/Setup.exe".into(),
            size: 42,
            sha256: "ab".repeat(32),
        });
        let release = verify_manifest(&envelope(&manifest, &key), &config).unwrap();
        assert!(check_release_owner(&release.manifest, &config.repository).is_err());
    }
    #[test]
    fn repair_accepts_the_running_release_but_updates_require_a_newer_one() {
        let (mut config, manifest, key) = signed();
        config.current_version = manifest.version.clone();
        let bytes = envelope(&manifest, &key);
        assert!(verify_manifest(&bytes, &config).is_err());
        assert_eq!(
            verify_signed(&bytes, &config).unwrap().manifest.version,
            "1.1.0"
        );
    }
    #[test]
    fn cancellation_preserves_selected_payload() {
        let (config, manifest, key) = signed();
        let release = verify_manifest(&envelope(&manifest, &key), &config).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        fs::create_dir(&target.root).unwrap();
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "1.0.0".into(),
        };
        restore_selection(&target.root, Some(&previous)).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel();
        let error = prepare_local(
            &release,
            &target,
            Path::new("not-a-package.zip"),
            &config,
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(read_selection(&target.root).unwrap(), Some(previous));
        assert!(!state_dir(&target.root).join("journal.json").exists());
    }
}

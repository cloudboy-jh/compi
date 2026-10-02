use base64::{Engine as _, engine::general_purpose::STANDARD};
use compi_update::*;
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};
use zeroize::Zeroize;
fn value(args: &[String], name: &str) -> Result<String> {
    args.windows(2)
        .find(|p| p[0] == name)
        .map(|p| p[1].clone())
        .ok_or_else(|| Error(format!("Missing {name}")))
}
fn optional(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|p| p[0] == name).map(|p| p[1].clone())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Release metadata signing failed: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|s| s == "keygen") {
        let path = PathBuf::from(value(&args, "--private-key")?);
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        #[cfg(windows)]
        {
            let account = std::env::var("USERNAME")
                .map_err(|_| Error("Cannot secure private key without Windows account".into()))?;
            let status = std::process::Command::new("icacls")
                .arg(&path)
                .args(["/inheritance:r", "/grant:r"])
                .arg(format!("{account}:F"))
                .stdout(std::process::Stdio::null())
                .status()?;
            if !status.success() {
                drop(file);
                fs::remove_file(path)?;
                return Err(Error("Failed restricting private key permissions".into()));
            }
        }
        let mut encoded = STANDARD.encode(seed);
        file.write_all(encoded.as_bytes())?;
        encoded.zeroize();
        file.sync_all()?;
        seed.zeroize();
        println!("{}", STANDARD.encode(signing.verifying_key().as_bytes()));
        return Ok(());
    }
    if !args.first().is_some_and(|s| s == "sign") {
        return Err(Error("Usage: compi-release-metadata sign --version V --platform PLATFORM --artifact ZIP --url HTTPS_URL --output DIR --daemon-protocol N [--notes FILE] [--minimum-os N] [--qualified-daemon V ...] [--qualification FILE] [--key-file FILE]; keygen --private-key FILE".into()));
    }
    let mut encoded = if let Some(path) = optional(&args, "--key-file") {
        let metadata = fs::metadata(&path)?;
        if metadata.len() > 4096 {
            return Err(Error("Signing key file exceeds limit".into()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error(
                    "Signing key file permissions must be owner-only (0600)".into(),
                ));
            }
        }
        fs::read_to_string(path)?
    } else {
        std::env::var("COMPI_UPDATE_SIGNING_KEY").map_err(|_|Error("Set secret COMPI_UPDATE_SIGNING_KEY or owner-only --key-file; never commit private keys".into()))?
    };
    let mut decoded = STANDARD
        .decode(encoded.trim())
        .map_err(|_| Error("Signing key is not base64".into()))?;
    encoded.zeroize();
    let mut seed: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| Error("Signing key must be a 32-byte Ed25519 seed".into()))?;
    decoded.zeroize();
    let signing = SigningKey::from_bytes(&seed);
    seed.zeroize();
    let configured=std::env::var("COMPI_UPDATE_PUBLIC_KEY").ok().or_else(||option_env!("COMPI_UPDATE_PUBLIC_KEY").map(str::to_owned)).ok_or_else(||Error("COMPI_UPDATE_PUBLIC_KEY is required; refuse signing metadata for an unconfigured build".into()))?;
    let public = STANDARD
        .decode(configured.trim())
        .map_err(|_| Error("Invalid public key encoding".into()))?;
    if public.as_slice() != signing.verifying_key().as_bytes() {
        return Err(Error(
            "Release signing secret does not match COMPI_UPDATE_PUBLIC_KEY embedded in product"
                .into(),
        ));
    }
    let version = value(&args, "--version")?;
    let parsed =
        semver::Version::parse(&version).map_err(|_| Error("Version must be semantic".into()))?;
    if !parsed.pre.is_empty() {
        return Err(Error("Stable metadata cannot publish a prerelease".into()));
    }
    let platform = value(&args, "--platform")?;
    if !matches!(platform.as_str(), "windows-x86_64" | "macos-aarch64") {
        return Err(Error("Unsupported release platform".into()));
    }
    let artifact = PathBuf::from(value(&args, "--artifact")?);
    let mut file = File::open(&artifact)?;
    let size = file.metadata()?.len();
    if size == 0 || size > 4 * 1024 * 1024 * 1024 {
        return Err(Error("Artifact size outside update bounds".into()));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let sha256 = format!("{:x}", hash.finalize());
    let protocol = value(&args, "--daemon-protocol")?
        .parse::<u32>()
        .map_err(|_| Error("Invalid daemon protocol".into()))?;
    let qualified = args
        .windows(2)
        .filter(|p| p[0] == "--qualified-daemon")
        .map(|p| p[1].clone())
        .collect::<Vec<_>>();
    for daemon in &qualified {
        semver::Version::parse(daemon)
            .map_err(|_| Error("Invalid qualified daemon version".into()))?;
    }
    if let Some(path) = optional(&args, "--qualification") {
        #[derive(serde::Deserialize)]
        struct Qualification {
            platform: String,
            version: String,
            daemon_protocol: u32,
            qualified_daemons: Vec<String>,
            artifact_sha256: String,
        }
        let qualification: Qualification = serde_json::from_slice(&fs::read(path)?)?;
        if qualification.platform != platform
            || qualification.version != version
            || qualification.daemon_protocol != protocol
            || qualification.artifact_sha256 != sha256
            || qualified
                .iter()
                .any(|v| !qualification.qualified_daemons.contains(v))
        {
            return Err(Error(
                "Release metadata exceeds observed native qualification or artifact identity"
                    .into(),
            ));
        }
    }
    let notes = if let Some(path) = optional(&args, "--notes") {
        let metadata = fs::metadata(&path)?;
        if metadata.len() > 128 * 1024 {
            return Err(Error("Release notes exceed metadata bounds".into()));
        }
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let minimum_os = optional(&args, "--minimum-os").unwrap_or_else(|| {
        if platform == "windows-x86_64" {
            "10".into()
        } else {
            "14".into()
        }
    });
    let url = value(&args, "--url")?;
    let parsed_url = reqwest::Url::parse(&url).map_err(|_| Error("Invalid artifact URL".into()))?;
    if parsed_url.scheme() != "https" || parsed_url.host_str() != Some("github.com") {
        return Err(Error(
            "Artifact URL must be a GitHub HTTPS release asset".into(),
        ));
    }
    let manifest = ReleaseManifest {
        schema: 1,
        product: "compi".into(),
        version,
        platform: platform.clone(),
        minimum_os,
        release_notes: notes,
        daemon_protocol: protocol,
        qualified_daemon_versions: qualified,
        minimum_persistence: SUPPORTED_PERSISTENCE_VERSION,
        artifact: Artifact { url, size, sha256 },
    };
    let payload = serde_json::to_vec(&manifest)?;
    let envelope = SignedManifest {
        signature: STANDARD.encode(signing.sign(&payload).to_bytes()),
        payload: STANDARD.encode(payload),
    };
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    if bytes.len() > 256 * 1024 {
        return Err(Error("Signed metadata exceeds consumer bound".into()));
    }
    let output = PathBuf::from(value(&args, "--output")?);
    fs::create_dir_all(&output)?;
    let path = output.join(format!("compi-update-{platform}.json"));
    let mut file = File::create(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!("{}", path.display());
    Ok(())
}

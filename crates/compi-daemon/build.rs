fn main() {
    println!("cargo:rerun-if-changed=../../assets/Compi-desktopappicon-v4.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        copy_conpty_runtime();
    }

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/Compi-desktopappicon-v4.ico");
        resource.set("ProductName", "Compi");
        resource.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        resource.set("FileVersion", env!("CARGO_PKG_VERSION"));
        resource
            .compile()
            .expect("failed to embed Compi daemon resources");
    }
}

fn copy_conpty_runtime() {
    let architecture = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        _ => {
            println!("cargo:warning=bundled ConPTY supports Windows x64 and arm64 only");
            return;
        }
    };
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory is missing"),
    );
    let staged = manifest
        .join("../../target/conpty-runtime")
        .join(architecture);
    println!("cargo:rerun-if-changed={}", staged.display());
    let files = ["conpty.dll", "OpenConsole.exe", "ConPTY-LICENSE.txt"];
    for file in files {
        println!("cargo:rerun-if-changed={}", staged.join(file).display());
    }
    if files.iter().any(|file| !staged.join(file).is_file()) {
        println!(
            "cargo:warning=bundled ConPTY runtime is not staged; run tools/prepare-conpty.ps1 -Architecture {architecture}, then rebuild before launching the daemon"
        );
        return;
    }

    // OUT_DIR is <target>/<optional triple>/<profile>/build/<package>/out.
    // Do not reconstruct it from PROFILE: custom profiles and target dirs work too.
    let out = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo output directory is missing"),
    );
    let profile = out.ancestors().nth(3).expect("unexpected Cargo OUT_DIR");
    // Cargo runs test executables from deps; they use the same sibling-only loader.
    for destination in [profile.to_path_buf(), profile.join("deps")] {
        std::fs::create_dir_all(&destination).expect("failed to create runtime output directory");
        for file in files {
            std::fs::copy(staged.join(file), destination.join(file))
                .unwrap_or_else(|error| panic!("failed to stage bundled {file}: {error}"));
        }
    }
}

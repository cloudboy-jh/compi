fn main() {
    println!("cargo:rerun-if-changed=../../assets/Compi-desktopappicon-v4.ico");
    // Setup embeds the package that `tools/build-installer.ps1` builds into `payload/`.
    // Without it (lint, tests, a build of the other binaries), an empty one stands in;
    // the installer script checks the Setup it ships embeds the real one.
    println!("cargo:rerun-if-changed=payload/Compi.msi");
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut payload = manifest.join("payload").join("Compi.msi");
    if !payload.is_file() {
        payload = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("Compi.msi");
        std::fs::write(&payload, []).expect("failed to write the stand-in Setup payload");
    }
    println!("cargo:rustc-env=COMPI_SETUP_PAYLOAD={}", payload.display());

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/Compi-desktopappicon-v4.ico");
        resource.set("ProductName", "Compi");
        resource.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        resource.set("FileVersion", env!("CARGO_PKG_VERSION"));
        resource
            .compile()
            .expect("failed to embed Compi Setup icon");
    }
}

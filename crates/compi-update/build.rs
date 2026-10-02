fn main() {
    println!("cargo:rerun-if-changed=../../assets/Compi-desktopappicon-v4.ico");
    println!("cargo:rerun-if-env-changed=COMPI_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-env-changed=COMPI_UPDATE_REPOSITORY");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/Compi-desktopappicon-v4.ico");
        resource.set("ProductName", "Compi");
        resource.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        resource.set("FileVersion", env!("CARGO_PKG_VERSION"));
        resource
            .compile()
            .expect("failed to embed Compi launcher and update worker resources");
    }
}

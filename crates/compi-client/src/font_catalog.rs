include!(concat!(env!("OUT_DIR"), "/font_catalog.rs"));

/// License notices for every font embedded in native client binaries.
pub const FONT_ATTRIBUTION: &str = include_str!("../fonts/ATTRIBUTION.txt");

#[cfg(any(windows, target_os = "macos"))]
pub fn register_bundled(text_system: &gpui::TextSystem) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let symbols = register_symbol_fallbacks();
    text_system
        .add_fonts(bundled_font_data())
        .map_err(|error| format!("Could not register bundled fonts: {error}"))?;

    let available = text_system.all_font_names();
    let missing_ui = UiFontPreset::ALL
        .into_iter()
        .filter(|preset| *preset != UiFontPreset::System)
        .find(|preset| !contains_family(&available, preset.family()))
        .map(UiFontPreset::family);
    let missing_terminal = TerminalFontPreset::ALL
        .into_iter()
        .filter(|preset| *preset != TerminalFontPreset::SystemMonospace)
        .find(|preset| !contains_family(&available, preset.family()))
        .map(TerminalFontPreset::family);
    if let Some(family) = missing_ui.or(missing_terminal) {
        return Err(format!(
            "Bundled font {family:?} did not register; using the platform default"
        ));
    }
    #[cfg(target_os = "macos")]
    symbols?;
    Ok(())
}

/// GPUI builds macOS fallback cascades from family-name descriptors, which
/// CoreText resolves only against registered fonts, never GPUI's in-memory ones.
#[cfg(target_os = "macos")]
fn register_symbol_fallbacks() -> Result<(), String> {
    use std::ffi::c_void;

    type ReleaseData = unsafe extern "C" fn(*mut c_void, *const c_void, usize);
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFErrorGetCode(error: *const c_void) -> isize;
        fn CFRelease(object: *const c_void);
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release: Option<ReleaseData>,
        ) -> *const c_void;
        fn CGFontCreateWithDataProvider(provider: *const c_void) -> *const c_void;
    }
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerRegisterGraphicsFont(
            font: *const c_void,
            error: *mut *const c_void,
        ) -> bool;
    }
    // Name clashes mean a user-installed copy already serves the cascade.
    const ALREADY_REGISTERED: isize = 105;
    const DUPLICATED_NAME: isize = 305;

    for (family, data) in BUNDLED_SYMBOL_FONTS {
        // SAFETY: the provider borrows 'static bytes and needs no release callback;
        // every Create-rule object is released once, and a null result is checked.
        let registered = unsafe {
            let provider = CGDataProviderCreateWithData(
                std::ptr::null_mut(),
                data.as_ptr().cast(),
                data.len(),
                None,
            );
            let font = if provider.is_null() {
                std::ptr::null()
            } else {
                let font = CGFontCreateWithDataProvider(provider);
                CFRelease(provider);
                font
            };
            if font.is_null() {
                false
            } else {
                let mut error = std::ptr::null();
                let registered = CTFontManagerRegisterGraphicsFont(font, &mut error);
                CFRelease(font);
                if error.is_null() {
                    registered
                } else {
                    let code = CFErrorGetCode(error);
                    CFRelease(error);
                    matches!(code, ALREADY_REGISTERED | DUPLICATED_NAME)
                }
            }
        };
        if !registered {
            return Err(format!(
                "Bundled symbol font {family:?} did not register; prompt icons may be missing"
            ));
        }
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
pub fn resolve_ui_font(requested: UiFontPreset, text_system: &gpui::TextSystem) -> UiFontPreset {
    if requested == UiFontPreset::System
        || contains_family(&text_system.all_font_names(), requested.family())
    {
        requested
    } else {
        UiFontPreset::System
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn contains_family(available: &[String], requested: &str) -> bool {
    available
        .iter()
        .any(|family| family.eq_ignore_ascii_case(requested))
}

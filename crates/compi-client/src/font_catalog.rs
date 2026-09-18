include!(concat!(env!("OUT_DIR"), "/font_catalog.rs"));

/// License notices for every font embedded in native client binaries.
pub const FONT_ATTRIBUTION: &str = include_str!("../fonts/ATTRIBUTION.txt");

#[cfg(any(windows, target_os = "macos"))]
pub fn register_bundled(text_system: &gpui::TextSystem) -> Result<(), String> {
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

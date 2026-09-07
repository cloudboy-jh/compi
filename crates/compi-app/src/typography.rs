use std::sync::Arc;

use gpui::{Font, FontFallbacks, FontFeatures, FontId, TextRun, TextSystem, Window, font, px};

use crate::config::FontSettings;

#[cfg(target_os = "macos")]
const NATIVE_MONOSPACE: &[&str] = &["Menlo", "Monaco", "Andale Mono", "PT Mono"];
#[cfg(windows)]
const NATIVE_MONOSPACE: &[&str] = &["Cascadia Mono", "Consolas", "Courier New"];

const SYMBOL_MONOSPACE: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "MesloLGS Nerd Font Mono",
    "Symbols Nerd Font Mono",
];

#[cfg(target_os = "macos")]
const PLATFORM_FALLBACKS: &[&str] = &["Apple Symbols", "Arial Unicode MS", "Apple Color Emoji"];
#[cfg(windows)]
const PLATFORM_FALLBACKS: &[&str] = &["Segoe UI Symbol", "Segoe UI Emoji"];

/// Resolved once per font, zoom, or display-scale change, never per terminal cell.
#[derive(Clone, Debug)]
pub struct TerminalTypography {
    pub font: Font,
    pub font_size: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    /// Device-snapped distance from the cell top to the primary font's baseline.
    ///
    /// `ShapedLine::paint` adds `(height - line.ascent - line.descent) / 2 +
    /// line.ascent` to its origin. Subtract that offset from `cell_top + baseline`
    /// when using it, or pass `cell_top + baseline` to `Window::paint_glyph` /
    /// `paint_emoji` directly. Fallback metrics must not move the terminal grid.
    pub baseline: f32,
    pub diagnostics: Vec<String>,
}

impl TerminalTypography {
    pub fn resolve(settings: &FontSettings, zoom: f32, window: &Window) -> Self {
        let text_system = window.text_system();
        let available = text_system.all_font_names();
        let mut diagnostics = Vec::new();
        let zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            diagnostics.push("Invalid font zoom; using 100%.".into());
            1.0
        };
        let font_size = settings.size * zoom;
        let features = FontFeatures(Arc::new(vec![
            ("calt".into(), 0),
            ("liga".into(), 0),
            ("clig".into(), 0),
            ("kern".into(), 0),
        ]));

        let requested = resolve_primary(
            &settings.family,
            &available,
            &features,
            font_size,
            text_system,
        );
        let (mut resolved_font, advance, ascent, descent) = match requested {
            Ok(resolved) => resolved,
            Err(reason) => {
                diagnostics.push(format!("Font {:?}: {reason}.", settings.family));
                let native = NATIVE_MONOSPACE.iter().find_map(|family| {
                    resolve_primary(family, &available, &features, font_size, text_system).ok()
                });
                let native = native.expect("no usable native monospace font is installed");
                diagnostics.push(format!(
                    "Using native monospace font {:?}.",
                    native.0.family
                ));
                native
            }
        };

        let mut fallbacks = Vec::new();
        for requested in &settings.fallbacks {
            match installed_family(&available, requested) {
                Some(family) => append_fallback(&mut fallbacks, family, &resolved_font),
                None => diagnostics.push(format!(
                    "Fallback font {requested:?} is not installed; skipping it."
                )),
            }
        }
        // Menlo has no Nerd Font private-use icons. Add an actually installed
        // patched font after user choices, rather than relying on OS PUA fallback.
        if let Some(family) = SYMBOL_MONOSPACE
            .iter()
            .find_map(|family| installed_family(&available, family))
        {
            append_fallback(&mut fallbacks, family, &resolved_font);
        }
        for family in PLATFORM_FALLBACKS {
            if let Some(family) = installed_family(&available, family) {
                append_fallback(&mut fallbacks, family, &resolved_font);
            }
        }
        resolved_font.fallbacks = Some(FontFallbacks::from_fonts(fallbacks));

        // GPUI exposes glyph availability through advance(), not glyph_for_char.
        // Probe the shaped cascade: standalone resolution rejects symbol/emoji
        // fonts without an 'm' on macOS, although CoreText can use them as fallbacks.
        for (sample, description) in [
            ('\u{e0b0}', "Powerline separator U+E0B0"),
            ('\u{f07b}', "Nerd Font folder U+F07B"),
            ('\u{f418}', "Nerd Font branch U+F418"),
            ('\u{1f600}', "emoji U+1F600"),
        ] {
            let text = sample.to_string();
            let run = TextRun {
                len: text.len(),
                font: resolved_font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = text_system.shape_line(text.into(), px(font_size), &[run], None);
            if !shaped.runs.iter().any(|run| {
                !run.glyphs.is_empty()
                    && text_system
                        .advance(run.font_id, px(font_size), sample)
                        .is_ok()
            }) {
                diagnostics.push(format!(
                    "No usable glyph for {description} in {:?} and its fallback cascade.",
                    resolved_font.family
                ));
            }
        }

        let scale = window.scale_factor();
        let cell_width = (advance * scale).round().max(1.0) / scale;
        let cell_height = ((font_size * settings.line_height).max(ascent + descent) * scale)
            .ceil()
            .max(1.0)
            / scale;
        let baseline = (((cell_height - ascent - descent) / 2.0 + ascent) * scale).round() / scale;
        Self {
            font: resolved_font,
            font_size,
            cell_width,
            cell_height,
            baseline,
            diagnostics,
        }
    }
}

fn installed_family<'a>(available: &'a [String], requested: &str) -> Option<&'a str> {
    available
        .iter()
        .find(|family| family.eq_ignore_ascii_case(requested))
        .map(String::as_str)
}

fn append_fallback(fallbacks: &mut Vec<String>, family: &str, primary: &Font) {
    if !primary.family.eq_ignore_ascii_case(family)
        && !fallbacks
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(family))
    {
        fallbacks.push(family.to_owned());
    }
}

fn is_requested_font(text_system: &TextSystem, id: FontId, requested: &Font) -> bool {
    text_system.get_font_for_id(id).is_some_and(|resolved| {
        resolved.family.eq_ignore_ascii_case(&requested.family)
            && resolved.weight == requested.weight
            && resolved.style == requested.style
    })
}

fn resolve_primary(
    requested: &str,
    available: &[String],
    features: &FontFeatures,
    font_size: f32,
    text_system: &TextSystem,
) -> Result<(Font, f32, f32, f32), String> {
    let family = installed_family(available, requested).ok_or("not installed")?;
    let mut candidate = font(family.to_owned());
    candidate.features = features.clone();
    let id = text_system.resolve_font(&candidate);
    // all_font_names also includes GPUI's built-in fallback names, whether
    // installed or not. Therefore enumeration alone is not proof of resolution.
    if !is_requested_font(text_system, id, &candidate) {
        return Err("could not load the requested face (GPUI substituted another font)".into());
    }
    let advance = text_system
        .ch_advance(id, px(font_size))
        .map_err(|error| format!("cannot measure the cell advance: {error}"))?;
    let advance = f32::from(advance);
    if !advance.is_finite() || advance <= 0.0 {
        return Err("has an invalid cell advance".into());
    }
    // Test actual ASCII advances, not family names or ink bounding boxes.
    // Emoji and wide Unicode glyphs belong to their own logical-cell spans.
    let tolerance = (font_size / 4096.0).max(0.001);
    for byte in b' '..=b'~' {
        let ch = char::from(byte);
        let width = text_system
            .advance(id, px(font_size), ch)
            .map_err(|_| format!("is missing the ASCII glyph {ch:?}"))?
            .width;
        let width = f32::from(width);
        if !width.is_finite() || (width - advance).abs() > tolerance {
            return Err(format!(
                "is not fixed-cell: {ch:?} and '0' have different advances"
            ));
        }
    }
    let ascent = f32::from(text_system.ascent(id, px(font_size)));
    // font-kit's macOS descent is negative; GPUI's shaped-line descent is
    // positive. Normalize here instead of using TextSystem::baseline_offset.
    let descent = f32::from(text_system.descent(id, px(font_size))).abs();
    if !ascent.is_finite() || ascent <= 0.0 || !descent.is_finite() {
        return Err("has invalid vertical metrics".into());
    }
    Ok((candidate, advance, ascent, descent))
}

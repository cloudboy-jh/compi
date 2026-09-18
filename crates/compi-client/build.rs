use std::{
    collections::HashSet,
    fmt::Write,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    default: String,
    themes: Vec<Theme>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Theme {
    variant: String,
    id: String,
    name: String,
    family: String,
    description: String,
    mode: Mode,
    source: String,
    colors: Colors,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontCatalog {
    default: String,
    fonts: Vec<FontPreset>,
    terminal_default: String,
    terminal_fonts: Vec<FontPreset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontPreset {
    variant: String,
    id: String,
    name: String,
    family_windows: String,
    family_macos: String,
    description: String,
    files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Dark,
    Light,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Colors {
    background: String,
    surface: String,
    surface_hover: String,
    border: String,
    foreground: String,
    muted: String,
    accent: String,
    error: String,
    selection: String,
    cursor: String,
    ansi: [String; 16],
}

fn rgb(value: &str, theme: &str, field: &str) -> u32 {
    assert!(
        value.len() == 7
            && value.starts_with('#')
            && value[1..].bytes().all(|b| b.is_ascii_hexdigit()),
        "theme {theme}: {field} must be #RRGGBB, got {value:?}"
    );
    u32::from_str_radix(&value[1..], 16).expect("validated RGB")
}

fn generate_catalog() {
    const SOURCE: &str = "themes/catalog.json";
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=themes/ATTRIBUTION.txt");
    let catalog: Catalog =
        serde_json::from_str(&fs::read_to_string(SOURCE).expect("read bundled themes"))
            .expect("bundled theme schema must be complete and valid");
    assert!(!catalog.themes.is_empty(), "theme catalog cannot be empty");
    let mut ids = HashSet::new();
    let mut variants = HashSet::new();
    for theme in &catalog.themes {
        assert!(
            !theme.id.is_empty()
                && theme.id.split('-').all(|part| !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
                && ids.insert(&theme.id),
            "invalid or duplicate theme ID: {:?}",
            theme.id
        );
        assert!(
            theme.variant.starts_with(|c: char| c.is_ascii_uppercase())
                && theme.variant.bytes().all(|b| b.is_ascii_alphanumeric())
                && theme.variant != "Self"
                && variants.insert(&theme.variant),
            "invalid or duplicate theme variant: {:?}",
            theme.variant
        );
        for value in [
            &theme.name,
            &theme.family,
            &theme.description,
            &theme.source,
        ] {
            assert!(
                !value.trim().is_empty() && !value.chars().any(char::is_control),
                "theme {} has empty or invalid metadata",
                theme.id
            );
        }
    }
    assert!(
        ids.contains(&catalog.default),
        "default theme must exist in catalog"
    );

    let mut output = String::from(
        "// Generated from themes/catalog.json; do not edit.\n#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]\npub enum ThemePreset {\n",
    );
    for theme in &catalog.themes {
        if theme.id == catalog.default {
            output.push_str("    #[default]\n");
        }
        writeln!(
            output,
            "    #[serde(rename = {:?})]\n    {},",
            theme.id, theme.variant
        )
        .unwrap();
    }
    output.push_str("}\nimpl ThemePreset {\n");
    write!(
        output,
        "    pub const ALL: [Self; {}] = [",
        catalog.themes.len()
    )
    .unwrap();
    for theme in &catalog.themes {
        write!(output, "Self::{},", theme.variant).unwrap();
    }
    output.push_str("];\n");
    for method in ["id", "label", "family", "description"] {
        writeln!(
            output,
            "    pub const fn {method}(self) -> &'static str {{ match self {{"
        )
        .unwrap();
        for theme in &catalog.themes {
            let value = match method {
                "id" => &theme.id,
                "label" => &theme.name,
                "family" => &theme.family,
                "description" => &theme.description,
                _ => unreachable!(),
            };
            writeln!(output, "        Self::{} => {:?},", theme.variant, value).unwrap();
        }
        output.push_str("    }}\n");
    }
    output.push_str("    pub const fn is_dark(self) -> bool { match self {\n");
    for theme in &catalog.themes {
        writeln!(
            output,
            "        Self::{} => {},",
            theme.variant,
            matches!(theme.mode, Mode::Dark)
        )
        .unwrap();
    }
    output
        .push_str("    }}\n    pub const fn colors(self) -> &'static ThemeColors { match self {\n");
    for theme in &catalog.themes {
        writeln!(output, "        Self::{} => &ThemeColors {{", theme.variant).unwrap();
        let colors = &theme.colors;
        for (field, value) in [
            ("background", &colors.background),
            ("surface", &colors.surface),
            ("surface_hover", &colors.surface_hover),
            ("border", &colors.border),
            ("foreground", &colors.foreground),
            ("muted", &colors.muted),
            ("accent", &colors.accent),
            ("error", &colors.error),
            ("selection", &colors.selection),
            ("cursor", &colors.cursor),
        ] {
            writeln!(
                output,
                "            {field}: 0x{:06x},",
                rgb(value, &theme.id, field)
            )
            .unwrap();
        }
        output.push_str("            ansi: [");
        for value in &colors.ansi {
            write!(output, "0x{:06x},", rgb(value, &theme.id, "ansi")).unwrap();
        }
        output.push_str("],\n        },\n");
    }
    output.push_str("    }}\n    pub fn parse(value: &str) -> Option<Self> { match value {\n");
    for theme in &catalog.themes {
        writeln!(
            output,
            "        {:?} => Some(Self::{}),",
            theme.id, theme.variant
        )
        .unwrap();
    }
    output.push_str("        _ => None,\n    }}\n}\n");
    let destination = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    fs::write(destination.join("theme_catalog.rs"), output).expect("write generated theme catalog");
}

fn validate_font_presets(
    kind: &str,
    default: &str,
    fonts: &[FontPreset],
    files: &mut HashSet<String>,
) {
    assert!(!fonts.is_empty(), "{kind} font catalog cannot be empty");
    let mut ids = HashSet::new();
    let mut variants = HashSet::new();
    for font in fonts {
        assert!(
            !font.id.is_empty()
                && font.id.split('-').all(|part| !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()))
                && ids.insert(font.id.clone()),
            "invalid or duplicate {kind} font ID: {:?}",
            font.id
        );
        assert!(
            font.variant
                .starts_with(|character: char| character.is_ascii_uppercase())
                && font
                    .variant
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
                && font.variant != "Self"
                && variants.insert(font.variant.clone()),
            "invalid or duplicate {kind} font variant: {:?}",
            font.variant
        );
        for value in [
            &font.name,
            &font.family_windows,
            &font.family_macos,
            &font.description,
        ] {
            assert!(
                !value.trim().is_empty() && !value.chars().any(char::is_control),
                "{kind} font {} has empty or invalid metadata",
                font.id
            );
        }
        for file in &font.files {
            let path = Path::new(file);
            assert!(
                file.starts_with("fonts/")
                    && path.is_relative()
                    && !path.components().any(|component| matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    ))
                    && files.insert(file.clone())
                    && fs::metadata(path).is_ok_and(|metadata| metadata.is_file()),
                "{kind} font {} has an invalid, duplicate, or missing file: {file:?}",
                font.id
            );
            println!("cargo:rerun-if-changed={file}");
        }
    }
    assert!(
        ids.contains(default),
        "default {kind} font must exist in catalog"
    );
}

fn generate_font_enum(output: &mut String, enum_name: &str, default: &str, fonts: &[FontPreset]) {
    writeln!(
        output,
        "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\npub enum {enum_name} {{"
    )
    .unwrap();
    for font in fonts {
        if font.id == default {
            output.push_str("    #[default]\n");
        }
        writeln!(
            output,
            "    #[serde(rename = {:?})]\n    {},",
            font.id, font.variant
        )
        .unwrap();
    }
    writeln!(output, "}}\nimpl {enum_name} {{").unwrap();
    write!(output, "    pub const ALL: [Self; {}] = [", fonts.len()).unwrap();
    for font in fonts {
        write!(output, "Self::{},", font.variant).unwrap();
    }
    output.push_str("];\n");
    for (method, field) in [
        ("id", "id"),
        ("label", "name"),
        ("description", "description"),
    ] {
        writeln!(
            output,
            "    pub const fn {method}(self) -> &'static str {{ match self {{"
        )
        .unwrap();
        for font in fonts {
            let value = match field {
                "id" => &font.id,
                "name" => &font.name,
                "description" => &font.description,
                _ => unreachable!(),
            };
            writeln!(output, "        Self::{} => {:?},", font.variant, value).unwrap();
        }
        output.push_str("    }}\n");
    }
    output.push_str(
        "    pub const fn family(self) -> &'static str {\n        #[cfg(windows)]\n        { match self {\n",
    );
    for font in fonts {
        writeln!(
            output,
            "            Self::{} => {:?},",
            font.variant, font.family_windows
        )
        .unwrap();
    }
    output.push_str("        }}\n        #[cfg(target_os = \"macos\")]\n        { match self {\n");
    for font in fonts {
        writeln!(
            output,
            "            Self::{} => {:?},",
            font.variant, font.family_macos
        )
        .unwrap();
    }
    output.push_str(
        "        }}\n        #[cfg(not(any(windows, target_os = \"macos\")))]\n        { match self {\n",
    );
    for font in fonts {
        writeln!(
            output,
            "            Self::{} => {:?},",
            font.variant, font.family_windows
        )
        .unwrap();
    }
    output.push_str(
        "        }}\n    }\n    pub fn parse(value: &str) -> Option<Self> { match value {\n",
    );
    for font in fonts {
        writeln!(
            output,
            "        {:?} => Some(Self::{}),",
            font.id, font.variant
        )
        .unwrap();
    }
    output.push_str(
        "        _ => None,\n    }}\n    pub fn from_family(value: &str) -> Option<Self> { Self::ALL.into_iter().find(|preset| preset.family().eq_ignore_ascii_case(value)) }\n}\n",
    );
}

fn generate_font_catalog() {
    const SOURCE: &str = "fonts/catalog.json";
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=fonts/ATTRIBUTION.txt");
    let catalog: FontCatalog =
        serde_json::from_str(&fs::read_to_string(SOURCE).expect("read bundled fonts"))
            .expect("bundled font schema must be complete and valid");
    let mut files = HashSet::new();
    validate_font_presets("UI", &catalog.default, &catalog.fonts, &mut files);
    validate_font_presets(
        "terminal",
        &catalog.terminal_default,
        &catalog.terminal_fonts,
        &mut files,
    );

    let mut output = String::from("// Generated from fonts/catalog.json; do not edit.\n");
    generate_font_enum(
        &mut output,
        "UiFontPreset",
        &catalog.default,
        &catalog.fonts,
    );
    generate_font_enum(
        &mut output,
        "TerminalFontPreset",
        &catalog.terminal_default,
        &catalog.terminal_fonts,
    );
    output.push_str(
        "#[cfg(any(windows, target_os = \"macos\"))]\npub fn bundled_font_data() -> Vec<std::borrow::Cow<'static, [u8]>> {\n    vec![\n",
    );
    for file in catalog
        .fonts
        .iter()
        .chain(&catalog.terminal_fonts)
        .flat_map(|font| &font.files)
    {
        writeln!(
            output,
            "        std::borrow::Cow::Borrowed(include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {file:?})).as_slice()),"
        )
        .unwrap();
    }
    output.push_str("    ]\n}\n");

    let destination = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    fs::write(destination.join("font_catalog.rs"), output).expect("write generated font catalog");
}

fn main() {
    generate_catalog();
    generate_font_catalog();
    println!("cargo:rerun-if-changed=../../assets/Compi-desktopappicon-v4.ico");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/Compi-desktopappicon-v4.ico");
        resource.set("ProductName", "Compi");
        resource.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        resource.set("FileVersion", env!("CARGO_PKG_VERSION"));
        resource
            .compile()
            .expect("failed to embed Compi application resources");
    }
}

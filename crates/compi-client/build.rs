use std::{collections::HashSet, fmt::Write, fs, path::PathBuf};

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

fn main() {
    generate_catalog();
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

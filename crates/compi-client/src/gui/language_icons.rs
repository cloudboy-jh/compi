//! Language icons: Catppuccin's file icons for Zed (github.com/catppuccin/zed-icons at
//! f695ec0, MIT; its licence is `assets/languages/LICENSE`), unchanged. They keep their
//! own colours instead of taking the theme's tint: the Mocha flavour on dark themes,
//! Latte on light ones.
use super::ui_rems;
use gpui::{
    App, Asset, DevicePixels, ImageCacheError, Img, RenderImage, Styled as _, SvgSize, Window, img,
    size,
};
use std::sync::Arc;

/// The side of an icon in logical pixels: the grid the icons are drawn on.
const ICON_SIZE: f32 = 16.0;

/// Where the icons come from and their licence, for the licences Settings shows.
pub(super) const ATTRIBUTION: &str = concat!(
    "CATPPUCCIN ICONS FOR ZED (language icons, Mocha and Latte, unchanged)\n",
    "Source: https://github.com/catppuccin/zed-icons/tree/",
    "f695ec0d81d53d6776471c3d3cad702cf11670e3/icons\n\n",
    include_str!("../../assets/languages/LICENSE"),
);

/// Declares the icons: each one's file name in both flavours' folders and the name a
/// code block shows beside it.
macro_rules! language_icons {
    ($($icon:ident = $file:literal, $name:literal;)*) => {
        /// A language Compi bundles an icon for.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(super) enum LanguageIcon {
            $($icon,)*
        }

        impl LanguageIcon {
            /// The language as a code block names it.
            pub(super) fn name(self) -> &'static str {
                match self {
                    $(Self::$icon => $name,)*
                }
            }

            /// The icon's path in the application's assets, for a dark or a light theme.
            fn path(self, dark: bool) -> &'static str {
                match (self, dark) {
                    $(
                        (Self::$icon, true) => concat!("languages/mocha/", $file, ".svg"),
                        (Self::$icon, false) => concat!("languages/latte/", $file, ".svg"),
                    )*
                }
            }
        }

        /// Every icon file by its path in the application's assets.
        const FILES: &[(&str, &[u8])] = &[
            $(
                (
                    concat!("languages/mocha/", $file, ".svg"),
                    include_bytes!(concat!("../../assets/languages/mocha/", $file, ".svg"))
                        .as_slice(),
                ),
                (
                    concat!("languages/latte/", $file, ".svg"),
                    include_bytes!(concat!("../../assets/languages/latte/", $file, ".svg"))
                        .as_slice(),
                ),
            )*
        ];
    };
}

language_icons! {
    Bash = "bash", "Shell";
    C = "c", "C";
    Cpp = "cpp", "C++";
    Css = "css", "CSS";
    Go = "go", "Go";
    Html = "html", "HTML";
    JavaScript = "javascript", "JavaScript";
    Jsx = "javascript-react", "JSX";
    Json = "json", "JSON";
    Markdown = "markdown", "Markdown";
    PowerShell = "powershell", "PowerShell";
    Python = "python", "Python";
    Rust = "rust", "Rust";
    Toml = "toml", "TOML";
    TypeScript = "typescript", "TypeScript";
    Tsx = "typescript-react", "TSX";
    Yaml = "yaml", "YAML";
}

/// The icon file at `path`, for the application's assets.
pub(super) fn file(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(known, _)| *known == path)
        .map(|(_, bytes)| *bytes)
}

impl LanguageIcon {
    /// The icon of `language`, in any case: a highlighter language (`rust`, `tsx`),
    /// one Compi has only the icon for (`jsx`, `powershell`), or another name code
    /// blocks give one of them (`c++`, `shell`, `pwsh`).
    pub(super) fn of(language: &str) -> Option<Self> {
        const LANGUAGES: &[(&str, LanguageIcon)] = &[
            ("bash", LanguageIcon::Bash),
            ("shell", LanguageIcon::Bash),
            ("c", LanguageIcon::C),
            ("cpp", LanguageIcon::Cpp),
            ("c++", LanguageIcon::Cpp),
            ("css", LanguageIcon::Css),
            ("go", LanguageIcon::Go),
            ("html", LanguageIcon::Html),
            ("javascript", LanguageIcon::JavaScript),
            ("jsx", LanguageIcon::Jsx),
            ("json", LanguageIcon::Json),
            ("markdown", LanguageIcon::Markdown),
            ("powershell", LanguageIcon::PowerShell),
            ("pwsh", LanguageIcon::PowerShell),
            ("python", LanguageIcon::Python),
            ("rust", LanguageIcon::Rust),
            ("toml", LanguageIcon::Toml),
            ("typescript", LanguageIcon::TypeScript),
            ("tsx", LanguageIcon::Tsx),
            ("yaml", LanguageIcon::Yaml),
        ];
        LANGUAGES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(language))
            .map(|(_, icon)| *icon)
    }

    /// The icon at 16 px in its own colours, for a dark or a light theme.
    pub(super) fn image(self, dark: bool) -> Img {
        let path = self.path(dark);
        img(move |window: &mut Window, cx: &mut App| {
            let side = ui_rems(ICON_SIZE)
                .to_pixels(window.rem_size())
                .scale(window.scale_factor());
            let side = side.0.round().max(1.0) as u32;
            window.use_asset::<IconImage>(&(path, side), cx)
        })
        .flex_none()
        .size(ui_rems(ICON_SIZE))
    }
}

/// An icon file drawn for one size in device pixels. `img` given the file's path would
/// draw it once at 32 px and stretch that, which blurs above 200% scaling; drawn for
/// the pixels it covers, the 1 px outlines stay sharp at any scale.
enum IconImage {}

impl Asset for IconImage {
    /// The file's path in the application's assets and its side in device pixels.
    type Source = (&'static str, u32);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        (path, side): Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        let assets = cx.asset_source().clone();
        async move {
            let bytes = assets.load(path).ok().flatten().ok_or_else(|| {
                ImageCacheError::Asset(format!("Embedded resource not found: {path}").into())
            })?;
            let svg = renderer.parse_svg(&bytes)?;
            let side = DevicePixels(side as i32);
            Ok(renderer.render_parsed(&svg, SvgSize::Size(size(side, side)))?)
        }
    }
}

//! Offline diagram and equation rendering. No browser, shell, filesystem or package access.
use gpui::{Image, ImageFormat};
use markdown::mdast::Node;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::gui) enum Kind {
    Mermaid,
    InlineMath,
    DisplayMath,
}

#[derive(Clone)]
pub(in crate::gui) struct Asset {
    pub image: Arc<Image>,
    pub width: f32,
    pub height: f32,
}

pub(super) type Assets = HashMap<(Kind, String), Result<Asset, String>>;

/// The constructs Kit's Markdown view parses with: GFM and math, no frontmatter. Its
/// top-level blocks are this parse's root children, one for one.
fn parse_options() -> markdown::ParseOptions {
    markdown::ParseOptions {
        constructs: markdown::Constructs {
            math_text: true,
            math_flow: true,
            ..markdown::Constructs::gfm()
        },
        ..markdown::ParseOptions::gfm()
    }
}

/// The 1-based source line each top-level block of `source` starts on, as Kit's view
/// lists them; empty when the source does not parse.
pub(super) fn block_lines(source: &str) -> Vec<u32> {
    let Ok(Node::Root(root)) = markdown::to_mdast(source, &parse_options()) else {
        return Vec::new();
    };
    root.children
        .iter()
        .map(|child| child.position().map_or(1, |at| at.start.line as u32))
        .collect()
}

pub(super) fn prepare(source: &str, dark: bool, foreground: u32, known: &Assets) -> Assets {
    let Ok(root) = markdown::to_mdast(source, &parse_options()) else {
        return Assets::new();
    };
    let mut assets = Assets::new();
    collect(&root, dark, foreground, known, &mut assets);
    assets
}

fn collect(node: &Node, dark: bool, foreground: u32, known: &Assets, assets: &mut Assets) {
    let resource = match node {
        Node::Code(code) if code.lang.as_deref() == Some("mermaid") => {
            Some((Kind::Mermaid, &code.value))
        }
        Node::InlineMath(math) => Some((Kind::InlineMath, &math.value)),
        Node::Math(math) => Some((Kind::DisplayMath, &math.value)),
        _ => None,
    };
    if let Some((kind, value)) = resource {
        let key = (kind, value.clone());
        if !assets.contains_key(&key) {
            let result = if let Some(asset) = known.get(&key) {
                asset.clone()
            } else if assets.len() >= 128 {
                Err("Only the first 128 distinct diagrams and equations are rendered".into())
            } else {
                render(kind, value, dark, foreground)
            };
            assets.insert(key, result);
        }
    }
    if let Some(children) = node.children() {
        for child in children {
            collect(child, dark, foreground, known, assets);
        }
    }
}

fn render(kind: Kind, value: &str, dark: bool, foreground: u32) -> Result<Asset, String> {
    let limit = if kind == Kind::Mermaid {
        64 * 1024
    } else {
        8 * 1024
    };
    if value.len() > limit {
        return Err(format!(
            "Source exceeds the {} KiB rendering limit",
            limit / 1024
        ));
    }
    let svg = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if kind == Kind::Mermaid {
            // The library accepts headerless text as a graph. A fenced Mermaid block
            // must identify its diagram, rather than quietly inventing a flowchart.
            let header = value
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("%%"))
                .and_then(|line| {
                    line.split(|ch: char| ch.is_whitespace() || ch == ';')
                        .next()
                })
                .unwrap_or_default();
            if !matches!(
                header,
                "flowchart"
                    | "graph"
                    | "sequenceDiagram"
                    | "classDiagram"
                    | "stateDiagram"
                    | "stateDiagram-v2"
                    | "erDiagram"
                    | "pie"
                    | "xychart"
                    | "xychart-beta"
                    | "quadrantChart"
                    | "gantt"
                    | "timeline"
                    | "journey"
                    | "mindmap"
                    | "gitGraph"
            ) {
                return Err(format!(
                    "Missing or unsupported Mermaid diagram type: {header}"
                ));
            }
            let options = mermaid_rs_renderer::RenderOptions {
                theme: if dark {
                    mermaid_rs_renderer::Theme::dark()
                } else {
                    mermaid_rs_renderer::Theme::neutral()
                },
                ..Default::default()
            };
            mermaid_rs_renderer::render_with_options(value, options).map_err(|e| e.to_string())
        } else {
            math_svg(value, kind == Kind::DisplayMath, foreground)
        }
    }))
    .map_err(|_| "The renderer could not process this source".to_owned())??;
    if svg.len() > 2 * 1024 * 1024 {
        return Err("Rendered SVG exceeds 2 MiB".into());
    }
    let document = roxmltree::Document::parse(&svg).map_err(|e| e.to_string())?;
    let root = document.root_element();
    let dimension = |name| {
        let raw = root.attribute(name)?;
        if let Some(pt) = raw.strip_suffix("pt") {
            pt.parse::<f32>().ok().map(|v| v * 4.0 / 3.0)
        } else {
            raw.trim_end_matches("px").parse::<f32>().ok()
        }
    };
    let (width, height) = dimension("width")
        .zip(dimension("height"))
        .or_else(|| {
            let mut values = root.attribute("viewBox")?.split_whitespace();
            values.next()?;
            values.next()?;
            Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
        })
        .ok_or("Rendered SVG has no dimensions")?;
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || width > 4096.0
        || height > 4096.0
    {
        return Err("Rendered dimensions must be between 1 and 4096 pixels".into());
    }
    Ok(Asset {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width,
        height,
    })
}

struct Fonts {
    book: typst::utils::LazyHash<typst::text::FontBook>,
    fonts: Vec<typst::text::Font>,
}

static FONTS: LazyLock<Fonts> = LazyLock::new(|| {
    let mut book = typst::text::FontBook::new();
    let mut fonts = Vec::new();
    for data in typst_assets::fonts() {
        for font in typst::text::Font::iter(typst::foundations::Bytes::new(data)) {
            book.push(font.info().clone());
            fonts.push(font);
        }
    }
    Fonts {
        book: typst::utils::LazyHash::new(book),
        fonts,
    }
});

struct MathWorld {
    source: typst::syntax::Source,
    library: typst::utils::LazyHash<typst::Library>,
}

impl typst::World for MathWorld {
    fn library(&self) -> &typst::utils::LazyHash<typst::Library> {
        &self.library
    }
    fn book(&self) -> &typst::utils::LazyHash<typst::text::FontBook> {
        &FONTS.book
    }
    fn main(&self) -> typst::syntax::FileId {
        self.source.id()
    }
    fn source(&self, id: typst::syntax::FileId) -> typst::diag::FileResult<typst::syntax::Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(typst::diag::FileError::AccessDenied)
        }
    }
    fn file(&self, _: typst::syntax::FileId) -> typst::diag::FileResult<typst::foundations::Bytes> {
        Err(typst::diag::FileError::AccessDenied)
    }
    fn font(&self, index: usize) -> Option<typst::text::Font> {
        FONTS.fonts.get(index).cloned()
    }
    fn today(&self, _: Option<i64>) -> Option<typst::foundations::Datetime> {
        None
    }
}

fn math_svg(latex: &str, display: bool, foreground: u32) -> Result<String, String> {
    use typst::LibraryExt;
    let math = mitex::convert_math(latex, None)?;
    // These are expression-valued aliases emitted by MiTeX, not document commands.
    let preamble = r#"
#let mitexsqrt(..args) = { let a = args.pos(); if a.len() == 2 { math.root(a.at(0), a.at(1)) } else { math.sqrt(a.at(0)) } }
#let mitexmathbf(it) = math.bold(math.upright(it))
#let mitexupright(it) = math.upright(it)
#let mitexitalic(it) = math.italic(it)
#let mitexsans(it) = math.sans(it)
#let mitexfrak(it) = math.frak(it)
#let mitexmono(it) = math.mono(it)
#let mitexcal(it) = math.cal(it)
#let textmath(it) = it
#let textbf(it) = math.bold(it)
#let textit(it) = math.italic(it)
#let textrm(it) = math.upright(it)
#let textsf(it) = math.sans(it)
#let texttt(it) = math.mono(it)
#let mitexdisplay(it) = math.display(it)
#let mitexinline(it) = math.inline(it)
#let mitexoverbrace(..args) = math.overbrace(..args)
#let mitexunderbrace(..args) = math.underbrace(..args)
#let operatornamewithlimits(it) = math.op(it, limits: true)
"#;
    let equation = if display {
        format!("$ {math} $")
    } else {
        format!("${math}$")
    };
    let source = format!(
        "{preamble}\n#set page(width:auto,height:auto,margin:2pt,fill:none)\n#set text(size:12pt,fill:rgb(\"#{:06x}\"))\n{equation}",
        foreground & 0x00ff_ffff
    );
    let world = MathWorld {
        source: typst::syntax::Source::detached(source),
        library: typst::utils::LazyHash::new(typst::Library::default()),
    };
    let document = typst::compile::<typst::layout::PagedDocument>(&world)
        .output
        .map_err(|errors| {
            errors
                .iter()
                .map(|e| e.message.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
    let page = document
        .pages
        .first()
        .ok_or("The equation produced no page")?;
    Ok(typst_svg::svg(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mermaid_block_never_invents_a_diagram_type() {
        assert!(render(Kind::Mermaid, "not-a-diagram", true, 0xeeeeee).is_err());
        assert!(
            render(
                Kind::Mermaid,
                "architecture-beta\nservice x",
                true,
                0xeeeeee
            )
            .is_err()
        );
    }
}

//! Image files in a file tab: read from the daemon in pieces, decoded off the UI thread
//! within the image limits (64 MiB encoded, 8192 pixels a side, 64 MiB decoded) and drawn
//! fitted, with zoom and pan. An SVG small enough to edit shows this way in View mode.

use super::*;
use crate::gui::workspace::media::{ZoomTarget, ZoomView, zoom_canvas};
use crate::image_input::{self, MAX_ENCODED_BYTES};
use base64::Engine as _;
use compi_protocol::{ImageFormat, MAX_CONTENT_RANGE_BYTES};
use gpui::{DevicePixels, RenderImage, Size, SvgRenderer, SvgSize};

static NEXT_IMAGE_LOAD: AtomicU64 = AtomicU64::new(1);

/// The longest side an SVG is drawn at. Twice its own size keeps it sharp to 200%.
const MAX_SVG_RASTER: f32 = 4096.0;
/// The width an SVG without a size of its own is drawn at.
const UNSIZED_SVG_RASTER: f32 = 1024.0;

/// A decoded image and its size at 100%, in logical pixels.
pub(in crate::gui) struct LoadedImage {
    image: Arc<RenderImage>,
    natural: Size<f32>,
}

/// An image file's tab, or an SVG's View.
pub(super) struct ImageFile {
    format: ImageFormat,
    len: u64,
    image: Option<Arc<RenderImage>>,
    error: Option<String>,
    pub(super) view: ZoomView,
    /// The load this waits for; older ones are dropped.
    request: u64,
}

impl ImageFile {
    /// What a load of this file needs: its format and the request it waits for.
    pub(super) fn load_key(&self) -> (ImageFormat, u64) {
        (self.format, self.request)
    }

    fn loading(format: ImageFormat, len: u64) -> Self {
        Self {
            format,
            len,
            image: None,
            error: None,
            view: ZoomView::default(),
            request: NEXT_IMAGE_LOAD.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn accept(&mut self, request: u64, result: Result<LoadedImage, String>) {
        if request != self.request {
            return;
        }
        match result {
            Ok(loaded) => {
                self.view.set_natural(loaded.natural);
                self.image = Some(loaded.image);
                self.error = None;
            }
            Err(error) => {
                self.image = None;
                self.error = Some(error);
            }
        }
    }
}

/// An SVG text file's View: the image drawn from its editor's text.
pub(super) struct SvgView {
    source: Rope,
    file: ImageFile,
}

impl SvgView {
    pub(super) fn file(&self) -> &ImageFile {
        &self.file
    }
}

pub(in crate::gui) fn is_svg(path: &str) -> bool {
    ImageFormat::of_path(path) == Some(ImageFormat::Svg)
}

/// The tab of an image file the editor cannot open: one that is not text, or too large,
/// within the encoded limit.
pub(super) fn image_tab(kind: FileKind, len: u64) -> Option<ImageFile> {
    match kind {
        FileKind::Image { format } if len <= MAX_ENCODED_BYTES as u64 => {
            Some(ImageFile::loading(format, len))
        }
        _ => None,
    }
}

/// An SVG's own size in pixels: its `width` and `height` (px or pt), else its viewBox's.
pub(in crate::gui) fn svg_size(svg: &str) -> Option<(f32, f32)> {
    let document = roxmltree::Document::parse(svg).ok()?;
    let root = document.root_element();
    let dimension = |name| {
        let raw = root.attribute(name)?;
        if let Some(points) = raw.strip_suffix("pt") {
            points.parse::<f32>().ok().map(|value| value * 4.0 / 3.0)
        } else {
            raw.trim_end_matches("px").parse::<f32>().ok()
        }
    };
    let (width, height) = dimension("width").zip(dimension("height")).or_else(|| {
        let mut values = root.attribute("viewBox")?.split_whitespace();
        values.next()?;
        values.next()?;
        Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
    })?;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some((width, height))
}

/// Draws an SVG at twice its own size, its longest side at most `MAX_SVG_RASTER`.
fn rasterize_svg(renderer: &SvgRenderer, bytes: &[u8]) -> Result<LoadedImage, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "The SVG is not UTF-8 text".to_owned())?;
    let natural = svg_size(text);
    let width = natural.map_or(UNSIZED_SVG_RASTER, |(width, height)| {
        width * 2.0_f32.min(MAX_SVG_RASTER / width.max(height))
    });
    let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let parsed = renderer.parse_svg(bytes)?;
        renderer.render_parsed(
            &parsed,
            SvgSize::Size(size(
                DevicePixels(width.round().max(1.0) as i32),
                DevicePixels(0),
            )),
        )
    }))
    .map_err(|_| "The SVG could not be drawn".to_owned())?
    .map_err(|error| format!("The SVG could not be drawn: {error}"))?;
    let pixels = image.size(0);
    let natural = natural.map_or_else(
        || size(pixels.width.0 as f32 / 2.0, pixels.height.0 as f32 / 2.0),
        |(width, height)| size(width, height),
    );
    Ok(LoadedImage { image, natural })
}

/// Decodes an image file's bytes: SVG through `svg`, others within the image limits.
fn decode(format: ImageFormat, bytes: &[u8], svg: &SvgRenderer) -> Result<LoadedImage, String> {
    if format == ImageFormat::Svg {
        return rasterize_svg(svg, bytes);
    }
    let image = image_input::decode_render(bytes)?;
    let pixels = image.size(0);
    Ok(LoadedImage {
        natural: size(pixels.width.0 as f32, pixels.height.0 as f32),
        image,
    })
}

/// The whole file, in pieces; it must not change, nor outgrow the encoded limit, meanwhile.
fn read_whole(
    target: &ConnectionTarget,
    environment: &ContentEnvironment,
    path: &str,
) -> Result<Vec<u8>, String> {
    let mut client = target.connect().map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let mut stamp = None;
    loop {
        let range = client
            .read_content_range(
                environment.clone(),
                path.to_owned(),
                bytes.len() as u64,
                MAX_CONTENT_RANGE_BYTES as u64,
            )
            .map_err(|error| error.to_string())?;
        if range.len > MAX_ENCODED_BYTES as u64 {
            return Err(format!(
                "Larger than the {} MiB image limit",
                MAX_ENCODED_BYTES / (1024 * 1024)
            ));
        }
        if stamp
            .replace((range.len, range.modified))
            .is_some_and(|stamp| stamp != (range.len, range.modified))
        {
            return Err("The file changed while it was read; open it again".into());
        }
        let piece = base64::engine::general_purpose::STANDARD
            .decode(&range.base64)
            .map_err(|error| error.to_string())?;
        if piece.is_empty() {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&piece);
        if bytes.len() as u64 >= range.len {
            return Ok(bytes);
        }
    }
}

impl CompiApp {
    /// Reads and decodes image file `path` for its tab in `pane_id`, off the UI thread.
    pub(super) fn request_image_load(
        &self,
        pane_id: PaneId,
        environment: ContentEnvironment,
        path: String,
        (format, request): (ImageFormat, u64),
        cx: &App,
    ) {
        let (target, sender, svg) = (
            self.target.clone(),
            self.event_tx.clone(),
            cx.svg_renderer(),
        );
        thread::spawn(move || {
            let result = read_whole(&target, &environment, &path)
                .and_then(|bytes| decode(format, &bytes, &svg));
            sender.send(UiEvent::ContentImageLoaded {
                pane_id,
                path,
                request,
                result,
            });
        });
    }

    /// An image file's, or an SVG View's, decoded image or why there is none.
    pub(in crate::gui) fn content_image_loaded(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        request: u64,
        result: Result<LoadedImage, String>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        if let Some(OpenFile {
            body: FileBody::Image(file),
            ..
        }) = ui.files.get_mut(path)
        {
            file.accept(request, result);
        } else if let Some(view) = ui.svg_views.get_mut(path) {
            view.file.accept(request, result);
        }
    }

    /// The zoom view of image `path` in `pane`: an image file's or an SVG View's.
    pub(in crate::gui) fn file_zoom_view(
        &mut self,
        pane: &PaneId,
        path: &str,
    ) -> Option<&mut ZoomView> {
        let ui = self.content.panes.get_mut(pane)?;
        match ui.files.get_mut(path) {
            Some(OpenFile {
                body: FileBody::Image(file),
                ..
            }) => Some(&mut file.view),
            _ => ui.svg_views.get_mut(path).map(|view| &mut view.file.view),
        }
    }
}

impl ContentUi {
    /// Every decoded image of image files and SVG Views, to keep their textures.
    pub(in crate::gui) fn images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        self.panes.values().flat_map(|ui| {
            let files = ui.files.values().filter_map(|file| match &file.body {
                FileBody::Image(image) => image.image.as_ref(),
                _ => None,
            });
            files.chain(
                ui.svg_views
                    .values()
                    .filter_map(|view| view.file.image.as_ref()),
            )
        })
    }
}

impl CompiApp {
    /// Before drawing: each shown SVG in View mode draws its editor's text when it
    /// changed; Views of files no longer open, or no longer in View, are dropped.
    pub(super) fn prepare_svg_views(&mut self, cx: &mut Context<Self>) {
        let shown: Vec<_> = self
            .shown_content_panes()
            .filter_map(|pane| {
                let content = find_content(self.workspace.as_ref()?, pane)?;
                Some((pane.clone(), content.active_file()?.to_owned()))
            })
            .collect();
        for ui in self.content.panes.values_mut() {
            let PaneUi {
                svg_views,
                files,
                notes,
                ..
            } = ui;
            svg_views.retain(|path, _| files.contains_key(path) && notes.is_rendered(path));
        }
        for (pane, path) in shown {
            let Some(ui) = self.content.panes.get_mut(&pane) else {
                continue;
            };
            if !is_svg(&path) || !ui.notes.is_rendered(&path) {
                continue;
            }
            let Some(editor) = ui.editor(&path) else {
                continue;
            };
            let source = editor.state.read(cx).text().clone();
            if ui
                .svg_views
                .get(&path)
                .is_some_and(|view| view.source == source)
            {
                continue;
            }
            let text = source.to_string();
            let mut file = ImageFile::loading(ImageFormat::Svg, text.len() as u64);
            if let Some(old) = ui.svg_views.remove(&path) {
                // Keep showing the last drawing, and its zoom, until the new one lands.
                file.image = old.file.image;
                file.view = old.file.view;
            }
            let (request, sender, svg) = (file.request, self.event_tx.clone(), cx.svg_renderer());
            ui.svg_views.insert(path.clone(), SvgView { source, file });
            let pane = pane.clone();
            thread::spawn(move || {
                let result = rasterize_svg(&svg, text.as_bytes());
                sender.send(UiEvent::ContentImageLoaded {
                    pane_id: pane,
                    path,
                    request,
                    result,
                });
            });
        }
    }

    /// An image file's body, or an SVG's View: a bar with its size and zoom controls over
    /// the image.
    pub(super) fn render_image_body(
        &self,
        pane_id: &PaneId,
        path: &str,
        file: Option<&ImageFile>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let info = match file {
            Some(ImageFile {
                error: Some(error), ..
            }) => error.clone(),
            Some(
                file @ ImageFile {
                    image: Some(image), ..
                },
            ) => {
                let pixels = image.size(0);
                // An SVG's raster is not its size.
                let natural = if file.format == ImageFormat::Svg {
                    String::new()
                } else {
                    format!("{} × {} · ", pixels.width.0, pixels.height.0)
                };
                let zoom = file
                    .view
                    .scale()
                    .map(|scale| format!(" · {:.0}%", scale * 100.0))
                    .unwrap_or_default();
                format!(
                    "{natural}{} image · {}{zoom}",
                    image_format_name(file.format),
                    human_size(file.len)
                )
            }
            _ => "Loading image…".to_owned(),
        };
        let button = |id: &'static str,
                      label: &'static str,
                      tooltip: &'static str,
                      act: fn(&mut ZoomView)| {
            let (pane, path) = (pane_id.clone(), path.to_owned());
            text_button(id, label)
                .tooltip(tooltip)
                .header()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(view) = this.file_zoom_view(&pane, &path) {
                        act(view);
                    }
                    cx.notify();
                }))
        };
        let controls = file.and_then(|file| file.image.as_ref()).map(|_| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(button("image-file-zoom-out", "−", "Zoom out", |view| {
                    view.zoom(Some(0.8), None)
                }))
                .child(button("image-file-fit", "Fit", "Fit the pane", |view| {
                    view.zoom(None, None)
                }))
                .child(button(
                    "image-file-actual",
                    "100%",
                    "Actual size",
                    ZoomView::actual_size,
                ))
                .child(button("image-file-zoom-in", "+", "Zoom in", |view| {
                    view.zoom(Some(1.25), None)
                }))
        });
        let canvas = file.and_then(|file| {
            let image = file.image.clone()?;
            let target = ZoomTarget::File {
                pane: pane_id.clone(),
                path: path.to_owned(),
            };
            Some(zoom_canvas(image, &file.view, target, cx.entity()))
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(info),
                    )
                    .children(controls),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .children(canvas),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_size_reads_pixels_points_or_the_view_box() {
        let svg = |attributes: &str| svg_size(&format!("<svg {attributes}/>"));
        assert_eq!(svg(r#"width="240" height="120""#), Some((240.0, 120.0)));
        assert_eq!(svg(r#"width="240px" height="120px""#), Some((240.0, 120.0)));
        assert_eq!(svg(r#"width="12pt" height="6pt""#), Some((16.0, 8.0)));
        // Without both sizes, the view box's.
        assert_eq!(svg(r#"width="50%" viewBox="0 0 64 32""#), Some((64.0, 32.0)));
        assert_eq!(svg(r#"width="0" height="10""#), None);
        assert_eq!(svg(""), None);
        assert_eq!(svg_size("not markup"), None);
    }
}

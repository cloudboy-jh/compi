//! App icons for Monitor's Processes rows and Memory by app tiles.
//!
//! The window keeps one cache of icons by app group id, decoded once into a
//! `RenderImage` (as Kitty images and image previews are), and one fetcher thread on a
//! connection of its own (`watch::watch_icons`) fed by a bounded queue. Only the icons of
//! rows and tiles actually drawn are asked for, each once. An app without an icon, or
//! whose icon could not be read, is remembered as having none until the fetcher
//! reconnects; it is drawn as a rounded square with its initial, tinted from the theme.
use super::*;
use std::cell::RefCell;

/// Icons waiting to be fetched, at most; a full queue is asked again next frame.
pub(super) const ICON_QUEUE: usize = 64;
/// Larger icons are refused rather than decoded.
const MAX_ICON_SIDE: u32 = 1024;

/// What the icon fetcher reports.
pub(in crate::gui) enum IconsEvent {
    /// A (new) connection: icons it could not read before may be asked again.
    Connected,
    Fetched {
        id: String,
        image: Option<Arc<RenderImage>>,
    },
}

/// The window's app icons.
#[derive(Default)]
pub(super) struct IconCache {
    pub(super) slot: WatchSlot,
    /// Each app asked for: its icon, or `None` when it has none.
    known: HashMap<String, Option<Arc<RenderImage>>>,
    requests: Option<mpsc::SyncSender<String>>,
    /// Apps asked for and not answered yet.
    asked: RefCell<HashSet<String>>,
}

impl IconCache {
    /// A fetcher started, taking requests from `requests`.
    pub(super) fn start(&mut self, requests: mpsc::SyncSender<String>) {
        self.requests = Some(requests);
        self.asked.get_mut().clear();
    }

    /// The fetcher stopped: the icons go with it.
    pub(super) fn stop(&mut self) {
        self.requests = None;
        self.known.clear();
        self.asked.get_mut().clear();
    }

    pub(super) fn event(&mut self, event: IconsEvent) {
        match event {
            // Retry what failed or had no icon on the last connection.
            IconsEvent::Connected => {
                self.known.retain(|_, icon| icon.is_some());
                self.asked.get_mut().clear();
            }
            IconsEvent::Fetched { id, image } => {
                self.asked.get_mut().remove(&id);
                self.known.insert(id, image);
            }
        }
    }

    /// The icon of app `id`, asked for once while unknown.
    pub(super) fn get(&self, id: &str) -> Option<&Arc<RenderImage>> {
        if let Some(known) = self.known.get(id) {
            return known.as_ref();
        }
        if let Some(requests) = &self.requests {
            let mut asked = self.asked.borrow_mut();
            if !asked.contains(id) && requests.try_send(id.to_owned()).is_ok() {
                asked.insert(id.to_owned());
            }
        }
        None
    }

    /// Every icon held, so the window keeps them in its sprite atlas while they are.
    pub(in crate::gui) fn images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        self.known.values().flatten()
    }
}

/// An icon's PNG as an image to draw, or `None` when it is not a readable PNG.
pub(super) fn decode_icon(png: &[u8]) -> Option<Arc<RenderImage>> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ICON_SIDE);
    limits.max_image_height = Some(MAX_ICON_SIDE);
    limits.max_alloc = Some(u64::from(MAX_ICON_SIDE) * u64::from(MAX_ICON_SIDE) * 4);
    reader.limits(limits);
    let mut rgba = reader.decode().ok()?.into_rgba8();
    // GPUI draws BGRA.
    let pixels: &mut [u8] = rgba.as_mut();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(smallvec![ImageFrame::new(rgba)])))
}

/// A stable small number for `name`, to pick its fallback tint.
fn name_hash(name: &str) -> u32 {
    name.chars().fold(0_u32, |hash, character| {
        hash.wrapping_mul(31).wrapping_add(character as u32)
    })
}

impl CompiApp {
    /// An app's icon, `size` pixels square: the app group `id`'s image when it has one,
    /// else a rounded square with `name`'s initial in a theme colour picked by the name.
    pub(super) fn app_icon(&self, id: Option<&str>, name: &str, size: f32) -> AnyElement {
        if let Some(image) = id.and_then(|id| self.monitor.icons.get(id)) {
            return gpui::img(image.clone())
                .flex_none()
                .size(px(size))
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element();
        }
        let colors = self.colors();
        // Red, green, yellow, blue, magenta, cyan.
        let index = 1 + (name_hash(name) % 6) as usize;
        let tint = color(modal_text_color(self.theme.terminal().ansi[index], colors));
        let initial: String = name
            .chars()
            .find(|character| character.is_alphanumeric())
            .map_or_else(
                || "?".to_owned(),
                |initial| initial.to_uppercase().collect(),
            );
        div()
            .flex_none()
            .size(px(size))
            .rounded(px(size * 0.22))
            .bg(tint.opacity(0.18))
            .flex()
            .items_center()
            .justify_center()
            .text_color(tint)
            .text_size(px((size * 0.6).max(8.0)))
            .line_height(px(size))
            .font_weight(FontWeight::SEMIBOLD)
            .child(initial)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode_from_png_and_garbage_is_no_icon() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let icon = decode_icon(&png).expect("a PNG decodes");
        assert_eq!(icon.size(0).width.0, 2);
        assert!(decode_icon(b"not a png").is_none());
        assert_eq!(name_hash("Safari"), name_hash("Safari"));
    }
}

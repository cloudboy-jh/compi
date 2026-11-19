//! Project icons: a project's own image (a favicon or logo the daemon finds in its
//! folder), else a tile holding the first letter of its name in a colour of the theme's
//! terminal palette, and for anything without a project the muted folder glyph. Each
//! window keeps the images it was given ([`ProjectIcons`]), asking the daemon off the UI
//! thread once per project every [`ICON_TTL`].

use super::*;
use compi_protocol::{Project, ProjectIconImage};
use std::cell::RefCell;

/// How long a project's image, or its having none, stands before the daemon is asked
/// again.
const ICON_TTL: Duration = Duration::from_secs(10 * 60);
/// The most projects a window keeps an answer for; the longest unused go first.
const MAX_ICONS: usize = 64;
/// The terminal palette's colours a letter tile takes: red, green, yellow, blue, magenta
/// and cyan, then their bright forms.
const TILE_COLORS: [usize; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];

/// The letter a project called `name` shows: the first letter or digit of its name,
/// uppercase; `None` when it has none.
fn tile_letter(name: &str) -> Option<String> {
    let first = name.chars().find(|c| c.is_alphanumeric())?;
    Some(first.to_uppercase().collect())
}

/// The ANSI colour index a project called `name` takes, the same for the same name.
fn tile_color(name: &str) -> usize {
    let hash = name.to_lowercase().chars().fold(0usize, |hash, c| {
        hash.wrapping_mul(31).wrapping_add(c as usize)
    });
    TILE_COLORS[hash % TILE_COLORS.len()]
}

/// The images projects were found to have, with when each was asked for.
#[derive(Default)]
pub(super) struct ProjectIcons {
    entries: RefCell<HashMap<Project, IconEntry>>,
}

struct IconEntry {
    image: Option<Arc<gpui::Image>>,
    /// When the daemon last answered; `None` before its first answer.
    answered: Option<Instant>,
    /// An ask is out.
    asking: bool,
}

impl ProjectIcons {
    /// `project`'s image as last found. Calls `ask` when the daemon has not been asked
    /// yet, or last answered over [`ICON_TTL`] ago, and no ask is out.
    fn image(&self, project: &Project, ask: impl FnOnce()) -> Option<Arc<gpui::Image>> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.entry(project.clone()).or_insert_with(|| IconEntry {
            image: None,
            answered: None,
            asking: false,
        });
        if !entry.asking
            && entry
                .answered
                .is_none_or(|answered| answered.elapsed() >= ICON_TTL)
        {
            entry.asking = true;
            ask();
        }
        entry.image.clone()
    }

    /// The daemon's answer for `project`: its image replaces the one before, whose
    /// decoded frames are freed, and past [`MAX_ICONS`] the longest-answered projects
    /// are dropped with theirs.
    pub(super) fn found(&mut self, project: Project, icon: Option<ProjectIconImage>, cx: &mut App) {
        let entries = self.entries.get_mut();
        let image = icon.map(|icon| {
            Arc::new(gpui::Image::from_bytes(
                image_format(icon.format),
                icon.bytes,
            ))
        });
        let entry = entries.entry(project).or_insert_with(|| IconEntry {
            image: None,
            answered: None,
            asking: false,
        });
        let same = match (&entry.image, &image) {
            (Some(old), Some(new)) => old.bytes == new.bytes && old.format == new.format,
            (None, None) => true,
            _ => false,
        };
        if !same {
            if let Some(old) = entry.image.take() {
                gpui::ImageSource::Image(old).remove_asset(cx);
            }
            entry.image = image;
        }
        entry.answered = Some(Instant::now());
        entry.asking = false;
        while entries.len() > MAX_ICONS {
            let Some(oldest) = entries
                .iter()
                .filter(|(_, entry)| !entry.asking)
                .min_by_key(|(_, entry)| entry.answered)
                .map(|(project, _)| project.clone())
            else {
                break;
            };
            if let Some(image) = entries.remove(&oldest).and_then(|entry| entry.image) {
                gpui::ImageSource::Image(image).remove_asset(cx);
            }
        }
    }
}

fn image_format(format: compi_protocol::ImageFormat) -> gpui::ImageFormat {
    use compi_protocol::ImageFormat as Format;
    match format {
        Format::Png => gpui::ImageFormat::Png,
        Format::Jpeg => gpui::ImageFormat::Jpeg,
        Format::Gif => gpui::ImageFormat::Gif,
        Format::Webp => gpui::ImageFormat::Webp,
        Format::Bmp => gpui::ImageFormat::Bmp,
        Format::Ico => gpui::ImageFormat::Ico,
        Format::Svg => gpui::ImageFormat::Svg,
    }
}

/// A `size` px tile holding `letter` in `text`, on a tint of it, with the corners of
/// Compi's chips.
fn tile(letter: SharedString, text: u32, size: f32) -> gpui::AnyElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(4.0))
        .bg(gpui::rgba((text << 8) | 0x2e))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .text_color(rgb(text))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(size * 0.62))
        .line_height(px(size))
        .child(letter)
        .into_any_element()
}

impl CompiApp {
    /// `project`'s icon, `size` px square: its image once the daemon found one and it
    /// decodes, else its letter tile; for no project, or a name without a letter, the
    /// folder glyph.
    pub(super) fn project_icon(&self, project: Option<&Project>, size: f32) -> gpui::AnyElement {
        let colors = self.theme.colors();
        let folder = || {
            gpui::svg()
                .flex_none()
                .size(px(size))
                .path(controls::IconName::Folder.path())
                .text_color(rgb(modal_text_color(colors.muted, colors)))
                .into_any_element()
        };
        let Some(project) = project else {
            return folder();
        };
        let name = project.name();
        let Some(letter) = tile_letter(name) else {
            return folder();
        };
        let letter: SharedString = letter.into();
        let text = modal_text_color(self.theme.terminal().ansi[tile_color(name)], colors);
        let image = self.project_icons.image(project, || {
            let (target, sender, project) =
                (self.target.clone(), self.event_tx.clone(), project.clone());
            thread::spawn(move || {
                let icon =
                    (|| -> crate::Result<_> { target.connect()?.project_icon(project.clone()) })()
                        .ok()
                        .flatten();
                sender.send(UiEvent::ProjectIconFound { project, icon });
            });
        });
        match image {
            Some(image) => {
                let fallback = move || tile(letter.clone(), text, size);
                gpui::img(gpui::ImageSource::Image(image))
                    .flex_none()
                    .size(px(size))
                    .rounded(px(4.0))
                    .object_fit(gpui::ObjectFit::Contain)
                    .with_loading(fallback.clone())
                    .with_fallback(fallback)
                    .into_any_element()
            }
            None => tile(letter, text, size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_shows_its_first_letter_in_a_steady_palette_colour() {
        assert_eq!(tile_letter("compi").as_deref(), Some("C"));
        assert_eq!(tile_letter("  -my-app").as_deref(), Some("M"));
        assert_eq!(tile_letter("3productdemo").as_deref(), Some("3"));
        assert_eq!(tile_letter(" -- "), None);
        assert_eq!(tile_color("compi"), tile_color("Compi"));
        assert!(TILE_COLORS.contains(&tile_color("glib-code")));
    }
}

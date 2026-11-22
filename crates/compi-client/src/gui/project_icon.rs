//! Project icons: the icon you chose for a project in Settings → Projects (a Lucide
//! icon, an emoji, a monogram or an image file), else its own image (a favicon or logo
//! the daemon finds in its folder), else a tile holding the first letter of its name in
//! a colour of the theme's terminal palette, and for anything without a project the
//! muted folder glyph. Each window keeps the images it was given ([`ProjectIcons`]),
//! asking the daemon off the UI thread once per project every [`ICON_TTL`].

use super::*;
use crate::config::ProjectIcon;
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
pub(super) fn tile_letter(name: &str) -> Option<String> {
    let first = name.chars().find(|c| c.is_alphanumeric())?;
    Some(first.to_uppercase().collect())
}

/// The ANSI colour index a project called `name` takes, the same for the same name.
pub(super) fn tile_color(name: &str) -> usize {
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

/// A `size` px tile holding `text` (a letter, or a monogram's one or two) in `color`, on
/// a tint of it, with the corners of Compi's chips.
fn tile(text: SharedString, color: u32, size: f32) -> gpui::AnyElement {
    let scale = if text.chars().count() > 1 { 0.46 } else { 0.62 };
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(4.0))
        .bg(gpui::rgba((color << 8) | 0x2e))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .text_color(rgb(color))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(size * scale))
        .line_height(px(size))
        .child(text)
        .into_any_element()
}

impl CompiApp {
    /// The terminal palette's colour `index` (0–15), made legible on the theme's
    /// surfaces; `None` past the palette.
    pub(super) fn icon_color(&self, index: usize) -> Option<u32> {
        let ansi = self.theme.terminal().ansi.get(index).copied()?;
        Some(modal_text_color(ansi, self.theme.colors()))
    }

    /// `icon` drawn `size` px square; `None` for a Lucide icon Compi does not have or
    /// a colour past the palette. An image file that fails to load shows `fallback`.
    pub(super) fn chosen_icon(
        &self,
        icon: &ProjectIcon,
        size: f32,
        fallback: impl Fn() -> gpui::AnyElement + Clone + 'static,
    ) -> Option<gpui::AnyElement> {
        Some(match icon {
            ProjectIcon::Lucide { name, color } => gpui::svg()
                .flex_none()
                .size(px(size))
                .path(lucide_icons::path(name)?)
                .text_color(rgb(self.icon_color(usize::from(*color))?))
                .into_any_element(),
            ProjectIcon::Emoji { emoji } => div()
                .flex_none()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .text_size(px(size * 0.8))
                .line_height(px(size))
                .child(SharedString::from(emoji.clone()))
                .into_any_element(),
            ProjectIcon::Monogram { text, color } => tile(
                text.clone().into(),
                self.icon_color(usize::from(*color))?,
                size,
            ),
            ProjectIcon::File { path } => gpui::img(std::path::PathBuf::from(path))
                .flex_none()
                .size(px(size))
                .rounded(px(4.0))
                .object_fit(gpui::ObjectFit::Contain)
                .with_loading(fallback.clone())
                .with_fallback(fallback)
                .into_any_element(),
        })
    }

    /// `project`'s icon, `size` px square: the one you chose for it, else a house for
    /// Home, else its automatic one (`automatic_project_icon`); for no project, the
    /// folder glyph.
    pub(super) fn project_icon(&self, project: Option<&Project>, size: f32) -> gpui::AnyElement {
        let Some(project) = project else {
            return self.folder_glyph(size);
        };
        if let Some(icon) = self
            .project_settings(project)
            .and_then(|settings| settings.icon.as_ref())
            && let Some(icon) = self.chosen_icon(icon, size, self.letter_icon(project, size))
        {
            return icon;
        }
        if self.is_home_project(project)
            && let Some(house) = lucide_icons::path("house")
        {
            let tint = modal_text_color(self.theme.colors().muted, self.theme.colors());
            return gpui::svg()
                .flex_none()
                .size(px(size))
                .path(house)
                .text_color(rgb(tint))
                .into_any_element();
        }
        self.automatic_project_icon(project, size)
    }

    /// `project`'s icon when none is chosen, `size` px square: its image once the
    /// daemon found one and it decodes, else its letter tile, else (a name without a
    /// letter) the folder glyph.
    pub(super) fn automatic_project_icon(&self, project: &Project, size: f32) -> gpui::AnyElement {
        let automatic = self.letter_icon(project, size);
        if tile_letter(&self.project_name(project)).is_none() {
            return automatic();
        }
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
            Some(image) => gpui::img(gpui::ImageSource::Image(image))
                .flex_none()
                .size(px(size))
                .rounded(px(4.0))
                .object_fit(gpui::ObjectFit::Contain)
                .with_loading(automatic.clone())
                .with_fallback(automatic)
                .into_any_element(),
            None => automatic(),
        }
    }

    /// What draws `project`'s letter tile, `size` px square; the folder glyph for a
    /// name without a letter.
    fn letter_icon(
        &self,
        project: &Project,
        size: f32,
    ) -> impl Fn() -> gpui::AnyElement + Clone + use<> {
        let colors = self.theme.colors();
        let name = self.project_name(project);
        let letter = tile_letter(&name).map(|letter| {
            let letter: SharedString = letter.into();
            let text = modal_text_color(self.theme.terminal().ansi[tile_color(&name)], colors);
            (letter, text)
        });
        let folder = self.folder_glyph_painter(size);
        move || match &letter {
            Some((letter, text)) => tile(letter.clone(), *text, size),
            None => folder(),
        }
    }

    /// The muted folder glyph, `size` px square: what stands for no project.
    fn folder_glyph(&self, size: f32) -> gpui::AnyElement {
        self.folder_glyph_painter(size)()
    }

    fn folder_glyph_painter(&self, size: f32) -> impl Fn() -> gpui::AnyElement + Clone + use<> {
        let colors = self.theme.colors();
        let folder_color = modal_text_color(colors.muted, colors);
        move || {
            gpui::svg()
                .flex_none()
                .size(px(size))
                .path(controls::IconName::Folder.path())
                .text_color(rgb(folder_color))
                .into_any_element()
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

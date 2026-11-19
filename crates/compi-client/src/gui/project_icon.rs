//! Project icons: a project's own image (a favicon or logo the daemon finds in its
//! folder), else a coloured two-letter monogram of its name, and for anything without a
//! project a gray dashed speech bubble (Lucide `message-square-dashed`).
//!
//! The monogram is T3 Code's (`projectIdentity.ts`), so a project looks the same in both:
//! compi is a purple "CI". Each window keeps the images it was given ([`ProjectIcons`]),
//! asking the daemon off the UI thread once per project every [`ICON_TTL`].

use super::*;
use compi_protocol::{Project, ProjectIconImage};
use std::cell::RefCell;
use unicode_normalization::UnicodeNormalization as _;

/// How long a project's image, or its having none, stands before the daemon is asked
/// again.
const ICON_TTL: Duration = Duration::from_secs(10 * 60);
/// The most projects a window keeps an answer for; the longest unused go first.
const MAX_ICONS: usize = 64;

/// Tailwind v4's colours, in T3 Code's order: name, `<c>-600` (light themes) and
/// `<c>-400` (dark themes).
const COLORS: [(&str, u32, u32); 18] = [
    ("gray", 0x4a5565, 0x99a1af),
    ("red", 0xe7000b, 0xff6467),
    ("orange", 0xf54900, 0xff8904),
    ("amber", 0xe17100, 0xffb900),
    ("yellow", 0xd08700, 0xfdc700),
    ("lime", 0x5ea500, 0x9ae600),
    ("green", 0x00a63e, 0x05df72),
    ("emerald", 0x009966, 0x00d492),
    ("teal", 0x009689, 0x00d5be),
    ("cyan", 0x0092b8, 0x00d3f2),
    ("sky", 0x0084d1, 0x00bcff),
    ("blue", 0x155dfc, 0x51a2ff),
    ("indigo", 0x4f39f6, 0x7c86ff),
    ("violet", 0x7f22fe, 0xa684ff),
    ("purple", 0x9810fa, 0xc27aff),
    ("fuchsia", 0xc800de, 0xed6aff),
    ("pink", 0xe60076, 0xfb64b6),
    ("rose", 0xec003f, 0xff637e),
];

/// The monogram's letters for a project called `name`: the first letter or digit of its
/// first word, then the first digit after it in that word, else the first of its last
/// word, else the last of its first word; uppercase. "PR" for an empty name.
pub(super) fn monogram_letters(name: &str) -> String {
    let name: String = name.nfkc().collect();
    let words: Vec<Vec<char>> = name
        .trim()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.chars().collect())
        .collect();
    let Some(first) = words.first() else {
        return "PR".to_owned();
    };
    let second = first[1..]
        .iter()
        .find(|c| c.is_numeric())
        .or_else(|| (words.len() > 1).then(|| &words[words.len() - 1][0]))
        .or_else(|| first[1..].last());
    [Some(&first[0]), second]
        .into_iter()
        .flatten()
        .flat_map(|c| c.to_uppercase())
        .take(2)
        .collect()
}

/// The index into [`COLORS`] of a project called `name`.
fn monogram_color(name: &str) -> usize {
    name.to_lowercase()
        .chars()
        .fold(0, |index, c| (index * 31 + c as usize) % COLORS.len())
}

/// The monogram's text colour for a project called `name`, as `0xRRGGBB`.
fn monogram_rgb(name: &str, dark: bool) -> u32 {
    let (_, light, dark_shade) = COLORS[monogram_color(name)];
    if dark { dark_shade } else { light }
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

/// A `size` px tile holding `letters` in bold monospace, in `text` on a 14 % tint of
/// it, with corners a quarter of its side.
fn monogram(letters: SharedString, text: u32, size: f32, font: SharedString) -> gpui::AnyElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 0.25))
        .bg(gpui::rgba((text << 8) | 0x24))
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .text_color(rgb(text))
        .font_family(font)
        .font_weight(FontWeight::BOLD)
        .text_size(px(size * 8.25 / 16.0))
        .line_height(px(size))
        .child(letters)
        .into_any_element()
}

impl CompiApp {
    /// `project`'s icon, `size` px square: its image once the daemon found one and it
    /// decodes, else its monogram; for no project, the dashed speech bubble.
    pub(super) fn project_icon(&self, project: Option<&Project>, size: f32) -> gpui::AnyElement {
        let dark = self.theme.is_dark();
        let Some(project) = project else {
            let (_, light, dark_shade) = COLORS[0];
            return gpui::svg()
                .flex_none()
                .size(px(size))
                .path(controls::IconName::MessageSquareDashed.path())
                .text_color(rgb(if dark { dark_shade } else { light }))
                .into_any_element();
        };
        let name = project.name();
        let letters: SharedString = monogram_letters(name).into();
        let text = monogram_rgb(name, dark);
        let font = self.typography.font.family.clone();
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
                let tile = move || monogram(letters.clone(), text, size, font.clone());
                gpui::img(gpui::ImageSource::Image(image))
                    .flex_none()
                    .size(px(size))
                    .rounded(px(size * 0.25))
                    .object_fit(gpui::ObjectFit::Contain)
                    .with_loading(tile.clone())
                    .with_fallback(tile)
                    .into_any_element()
            }
            None => monogram(letters, text, size, font),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms_take_t3_codes_letters_and_colours() {
        for (name, letters, colour) in [
            ("compi", "CI", "purple"),
            ("Nebula", "NA", "red"),
            ("t3code", "T3", "teal"),
        ] {
            assert_eq!(monogram_letters(name), letters, "{name}");
            assert_eq!(COLORS[monogram_color(name)].0, colour, "{name}");
        }
        assert_eq!(monogram_letters("Silver Orchard"), "SO");
        assert_eq!(monogram_letters("M7 Forge"), "M7");
        assert_eq!(monogram_letters("  my-app_v2 "), "MV");
        assert_eq!(monogram_letters("x"), "X");
        assert_eq!(monogram_letters(""), "PR");
        assert_eq!(monogram_letters(" -- "), "PR");
        // NFKC: a fullwidth name reads as its plain letters.
        assert_eq!(monogram_letters("ｃｏｍｐｉ"), "CI");
        assert_eq!(monogram_rgb("compi", false), 0x9810fa);
        assert_eq!(monogram_rgb("compi", true), 0xc27aff);
    }
}

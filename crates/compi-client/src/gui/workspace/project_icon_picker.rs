//! Change icon, which Settings → Projects opens for a project, in five tabs: Automatic
//! (the project's favicon or logo, else its first letter), a Lucide icon (any of them,
//! searched by name), an emoji, a monogram of one or two letters or numbers (icon and
//! monogram in a colour of the terminal palette), or an image file, which Choose file…
//! copies into Compi's data folder (`project-icons/`). Save icon saves through
//! `set_project_settings`, so every window shows the new icon.

use super::*;
use crate::config::ProjectIcon;
use crate::gui::controls::IconName;
use crate::gui::lucide_icons;
use crate::gui::project_icon::{tile_color, tile_letter};
use compi_protocol::Project;
use gpui::{ElementId, ScrollStrategy, UniformListScrollHandle, uniform_list};
use sha2::{Digest, Sha256};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use unicode_segmentation::UnicodeSegmentation;

/// Icons in a row of a grid.
const COLUMNS: usize = 10;
/// A grid row's height, and a little more than a cell's side.
const CELL: f32 = 44.0;
/// Rows of icons the Icons grid shows before it scrolls.
const VISIBLE_ROWS: f32 = 5.0;
const DEFAULT_ICON: &str = "folder-code";
/// The emoji offered, with what each shows.
const EMOJIS: [(&str, &str); 30] = [
    ("💻", "Computer"),
    ("🛠️", "Tools"),
    ("🚀", "Rocket"),
    ("🤖", "Robot"),
    ("✨", "Sparkles"),
    ("⚡", "Lightning"),
    ("🌐", "Web"),
    ("📱", "Mobile"),
    ("🖥️", "Desktop"),
    ("⌨️", "Keyboard"),
    ("⚙️", "Gear"),
    ("🗄️", "Database"),
    ("☁️", "Cloud"),
    ("📦", "Package"),
    ("📚", "Books"),
    ("🧪", "Test tube"),
    ("🔒", "Lock"),
    ("🎮", "Game"),
    ("🎵", "Music"),
    ("🎬", "Movie"),
    ("🖼️", "Picture"),
    ("🛍️", "Shopping"),
    ("🔥", "Fire"),
    ("💡", "Idea"),
    ("🧩", "Puzzle"),
    ("📊", "Chart"),
    ("🧠", "Brain"),
    ("🦄", "Unicorn"),
    ("🐙", "Octopus"),
    ("🌱", "Seedling"),
];
/// The terminal palette's colours by index.
const COLOR_NAMES: [&str; 16] = [
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
    "Bright black",
    "Bright red",
    "Bright green",
    "Bright yellow",
    "Bright blue",
    "Bright magenta",
    "Bright cyan",
    "Bright white",
];
/// The image files Choose file takes, by extension.
const FILE_KINDS: [&str; 3] = ["png", "svg", "ico"];

/// What kind of icon the dialog makes, one tab each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum IconKind {
    /// None chosen: the project's own image, else its letter.
    Automatic,
    Icons,
    Emoji,
    Monogram,
    /// An image file copied into Compi's data folder.
    File,
}

impl IconKind {
    const ALL: [Self; 5] = [
        Self::Automatic,
        Self::Icons,
        Self::Emoji,
        Self::Monogram,
        Self::File,
    ];

    fn index(self) -> usize {
        match self {
            Self::Automatic => 0,
            Self::Icons => 1,
            Self::Emoji => 2,
            Self::Monogram => 3,
            Self::File => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Icons => "Icons",
            Self::Emoji => "Emoji",
            Self::Monogram => "Monogram",
            Self::File => "Image file",
        }
    }

    /// Whether the kind has a grid the arrows move through.
    fn grid(self) -> bool {
        matches!(self, Self::Icons | Self::Emoji)
    }
}

/// Change icon's draft: what each kind would save. The shown kind's text field (the
/// icon search, a pasted emoji, the monogram's letters) is the overlay's text.
#[derive(Clone)]
pub(in crate::gui) struct IconPicker {
    project: Project,
    kind: IconKind,
    /// The Lucide icon picked, by name.
    icon: String,
    /// The terminal palette colour (0–15) of the icon or monogram.
    color: u8,
    emoji: String,
    /// The image file chosen, as copied into Compi's data folder.
    file: Option<String>,
    /// The text fields of the kinds not shown, by [`IconKind::index`].
    fields: [String; 5],
    grid: UniformListScrollHandle,
}

/// `text` as a monogram, trimmed and uppercase, when that is one or two letters or
/// numbers.
fn monogram(text: &str) -> Option<String> {
    let text = text.trim().to_uppercase();
    let count = text.graphemes(true).count();
    let letters = text
        .graphemes(true)
        .all(|grapheme| grapheme.chars().next().is_some_and(char::is_alphanumeric));
    ((1..=2).contains(&count) && letters).then_some(text)
}

/// The emoji `text` starts with, after any spaces, with its modifiers and joined parts.
fn first_emoji(text: &str) -> Option<&str> {
    let first = text.trim_start().graphemes(true).next()?;
    let lead = first.chars().next()?;
    (first.contains('\u{20E3}') || pictograph(lead)).then_some(first)
}

/// Whether `c` comes from the blocks emoji are drawn from: pictographs, symbols,
/// dingbats, arrows, shapes and the flags' regional indicators.
fn pictograph(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF
            | 0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x25A0..=0x27BF
            | 0x2934..=0x2935
            | 0x2B00..=0x2BFF
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
    )
}

/// Whether `bytes` hold an image of the kind `extension` names.
fn image_of_kind(bytes: &[u8], extension: &str) -> bool {
    match extension {
        "png" => image::guess_format(bytes).ok() == Some(image::ImageFormat::Png),
        "ico" => image::guess_format(bytes).ok() == Some(image::ImageFormat::Ico),
        _ => std::str::from_utf8(bytes).is_ok_and(|text| text.contains("<svg")),
    }
}

/// Copies the image at `source` into `folder`, named by its content so choosing the
/// same image again reuses the copy; the copy's path.
fn copy_icon_file(source: &Path, folder: &Path) -> Result<PathBuf, String> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| FILE_KINDS.contains(&extension.as_str()))
        .ok_or("Choose a PNG, SVG or ICO image.")?;
    let bytes = fs::read(source)
        .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
    if !image_of_kind(&bytes, &extension) {
        return Err(format!(
            "{} is not a {} image.",
            source.display(),
            extension.to_ascii_uppercase()
        ));
    }
    let digest = Sha256::digest(&bytes);
    let name: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let destination = folder.join(format!("{name}.{extension}"));
    if !destination.exists() {
        let failed = |error: std::io::Error| {
            format!("Could not copy the icon into {}: {error}", folder.display())
        };
        fs::create_dir_all(folder).map_err(failed)?;
        // Written whole before it takes its name, so a copy cut short is never reused.
        let partial = folder.join(format!("{name}.{extension}.partial"));
        fs::write(&partial, &bytes).map_err(failed)?;
        fs::rename(&partial, &destination).map_err(failed)?;
    }
    Ok(destination)
}

/// One choice in a grid: `face` in a square that shows `selected`, naming itself
/// `label` on hover. A click picks it; a double-click picks it and saves.
fn picker_cell(
    colors: ThemeColors,
    id: impl Into<ElementId>,
    selected: bool,
    label: impl Into<SharedString>,
    face: impl IntoElement,
    pick: impl Fn(&mut IconPicker) + 'static,
    cx: &Context<CompiApp>,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .size(px(CELL - 4.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(if selected {
            color(colors.accent)
        } else {
            gpui::transparent_black()
        })
        .when(selected, |cell| cell.bg(color(colors.surface_hover)))
        .hover(move |style| style.bg(color(colors.surface_hover)))
        .cursor_pointer()
        .tooltip(controls::tooltip(label, None, None))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            if let Some(picker) = this.icon_picker_mut() {
                pick(picker);
            }
            if event.click_count() > 1 {
                this.save_icon_choice(window, cx);
            }
            cx.notify();
        }))
        .child(face)
        .into_any_element()
}

impl CompiApp {
    /// Opens Change icon for `project`, on the tab of the icon you chose for it.
    pub(in crate::gui) fn open_project_icon_picker(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.project_name(&project);
        let current = self
            .project_settings(&project)
            .and_then(|settings| settings.icon.clone());
        let mut picker = IconPicker {
            project,
            kind: IconKind::Automatic,
            icon: DEFAULT_ICON.to_owned(),
            color: tile_color(&name) as u8,
            emoji: EMOJIS[0].0.to_owned(),
            file: None,
            fields: Default::default(),
            grid: UniformListScrollHandle::new(),
        };
        picker.fields[IconKind::Monogram.index()] = tile_letter(&name).unwrap_or_default();
        match current {
            Some(ProjectIcon::Lucide { name, color }) => {
                picker.kind = IconKind::Icons;
                if lucide_icons::path(&name).is_some() {
                    picker.icon = name;
                }
                picker.color = color;
            }
            Some(ProjectIcon::Emoji { emoji }) => {
                picker.kind = IconKind::Emoji;
                picker.emoji = emoji;
            }
            Some(ProjectIcon::Monogram { text, color }) => {
                picker.kind = IconKind::Monogram;
                picker.fields[IconKind::Monogram.index()] = text;
                picker.color = color;
            }
            Some(ProjectIcon::File { path }) => {
                picker.kind = IconKind::File;
                picker.file = Some(path);
            }
            None => {}
        }
        if self.icon_color(usize::from(picker.color)).is_none() {
            picker.color = tile_color(&name) as u8;
        }
        let text = std::mem::take(&mut picker.fields[picker.kind.index()]);
        self.open_overlay(Overlay::ProjectIcon(Box::new(picker)), &text);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Image file's Choose file…: asks for a PNG, SVG or ICO image and copies it into
    /// Compi's data folder; the dialog, still open for the same project, then shows it,
    /// for Save icon to save.
    fn choose_icon_file(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.icon_picker().map(|picker| picker.project.clone()) else {
            return;
        };
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose icon".into()),
        });
        cx.spawn(async move |this, cx| {
            let source = match prompt.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.global_error = Some(format!("Could not open Choose file: {error}"));
                        cx.notify();
                    });
                    return;
                }
                _ => None,
            };
            let Some(source) = source else {
                return;
            };
            let copied = cx
                .background_executor()
                .spawn(async move {
                    let folder = compi_protocol::paths::data_dir()
                        .map_err(|error| format!("Cannot locate Compi's data folder: {error}"))?
                        .join("project-icons");
                    copy_icon_file(&source, &folder)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match copied {
                    Ok(path) => {
                        if let Some(picker) = this
                            .icon_picker_mut()
                            .filter(|picker| picker.project == project)
                        {
                            picker.file = Some(path.to_string_lossy().into_owned());
                        }
                    }
                    Err(error) => this.global_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn icon_picker(&self) -> Option<&IconPicker> {
        match &self.overlay {
            Some(Overlay::ProjectIcon(picker)) => Some(picker),
            _ => None,
        }
    }

    fn icon_picker_mut(&mut self) -> Option<&mut IconPicker> {
        match &mut self.overlay {
            Some(Overlay::ProjectIcon(picker)) => Some(picker),
            _ => None,
        }
    }

    /// The icon the dialog would save (`None`: Automatic), or why it cannot save yet: a
    /// monogram that is not one or two letters or numbers, no image file chosen.
    fn icon_choice(&self, picker: &IconPicker) -> Result<Option<ProjectIcon>, &'static str> {
        Ok(Some(match picker.kind {
            IconKind::Automatic => return Ok(None),
            IconKind::Icons => ProjectIcon::Lucide {
                name: picker.icon.clone(),
                color: picker.color,
            },
            IconKind::Emoji => ProjectIcon::Emoji {
                emoji: picker.emoji.clone(),
            },
            IconKind::Monogram => ProjectIcon::Monogram {
                text: monogram(&self.ime_text).ok_or("Enter one or two letters or numbers.")?,
                color: picker.color,
            },
            IconKind::File => ProjectIcon::File {
                path: picker.file.clone().ok_or("Choose an image file first.")?,
            },
        }))
    }

    /// Saves the dialog's icon and closes it; nothing while it cannot save yet.
    fn save_icon_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.icon_picker() else {
            return;
        };
        let Ok(icon) = self.icon_choice(picker) else {
            return;
        };
        let project = picker.project.clone();
        self.dismiss_overlay();
        let mut settings = self.config.project_settings_or_new(&project);
        settings.icon = icon;
        self.set_project_settings(&project, settings, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Shows `kind`'s choices, keeping the text of the field it leaves.
    fn set_icon_kind(&mut self, kind: IconKind) {
        let Some(Overlay::ProjectIcon(picker)) = &mut self.overlay else {
            return;
        };
        if picker.kind == kind {
            return;
        }
        picker.fields[picker.kind.index()] = std::mem::take(&mut self.ime_text);
        picker.kind = kind;
        self.ime_text = std::mem::take(&mut picker.fields[kind.index()]);
        let end = self.ime_text.encode_utf16().count();
        self.ime_selected_range = end..end;
        self.ime_marked_range = None;
    }

    /// Follows an edit of the dialog's text field: a new search starts at the top, a
    /// pasted emoji is picked.
    pub(super) fn icon_picker_text_changed(&mut self) {
        let Some(Overlay::ProjectIcon(picker)) = &mut self.overlay else {
            return;
        };
        match picker.kind {
            IconKind::Icons => picker.grid.scroll_to_item(0, ScrollStrategy::Top),
            IconKind::Emoji => {
                if let Some(emoji) = first_emoji(&self.ime_text) {
                    picker.emoji = emoji.to_owned();
                }
            }
            IconKind::Automatic | IconKind::Monogram | IconKind::File => {}
        }
    }

    /// Moves the picked icon or emoji `step` places through its grid.
    fn move_icon_choice(&mut self, step: isize) {
        let found = lucide_icons::search(&self.ime_text);
        let Some(picker) = self.icon_picker_mut() else {
            return;
        };
        let moved = |at: Option<usize>, count: usize| {
            at.map_or(0, |at| at.saturating_add_signed(step).min(count - 1))
        };
        match picker.kind {
            IconKind::Icons if !found.is_empty() => {
                let at = found.iter().position(|(name, _)| *name == picker.icon);
                let next = moved(at, found.len());
                picker.icon = found[next].0.to_owned();
                picker
                    .grid
                    .scroll_to_item(next / COLUMNS, ScrollStrategy::Nearest);
            }
            IconKind::Emoji => {
                let at = EMOJIS.iter().position(|(emoji, _)| *emoji == picker.emoji);
                picker.emoji = EMOJIS[moved(at, EMOJIS.len())].0.to_owned();
            }
            _ => {}
        }
    }

    /// The dialog's keys: Tab and Shift+Tab switch kinds, arrows move through the grid,
    /// Enter saves, Escape closes; typing goes to the text field.
    pub(super) fn handle_icon_picker_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(kind) = self.icon_picker().map(|picker| picker.kind) else {
            return false;
        };
        let columns = COLUMNS as isize;
        let kinds = IconKind::ALL.len();
        match key.key.as_str() {
            "escape" => self.dismiss_overlay(),
            "enter" => self.save_icon_choice(window, cx),
            "tab" => {
                let step = if key.modifiers.shift { kinds - 1 } else { 1 };
                self.set_icon_kind(IconKind::ALL[(kind.index() + step) % kinds]);
            }
            "left" if kind.grid() => self.move_icon_choice(-1),
            "right" if kind.grid() => self.move_icon_choice(1),
            "up" if kind.grid() => self.move_icon_choice(-columns),
            "down" if kind.grid() => self.move_icon_choice(columns),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The dialog's text field, holding the overlay's text.
    fn render_icon_picker_field(
        &self,
        placeholder: &'static str,
        search: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .h(px(36.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(color(colors.accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .when(search, |field| {
                field.child(controls::glyph(
                    IconName::Search,
                    color(modal_text_color(colors.muted, &colors)),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(20.0))
                    .child(self.render_query_text(placeholder, true, cx)),
            )
            .into_any_element()
    }

    /// A muted caption over a part of the dialog.
    fn icon_picker_caption(&self, text: impl Into<SharedString>) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(text.into())
            .into_any_element()
    }

    /// The terminal palette's colours to pick from, `selected` ringed.
    fn render_icon_colors(&self, selected: u8, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children((0..16u8).filter_map(|index| {
                let swatch = self.icon_color(usize::from(index))?;
                Some(
                    div()
                        .id(("project-icon-color", usize::from(index)))
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(if index == selected {
                            color(modal_text_color(colors.foreground, &colors))
                        } else {
                            gpui::transparent_black()
                        })
                        .cursor_pointer()
                        .hover(move |style| style.bg(color(colors.surface_hover)))
                        .tooltip(controls::tooltip(
                            COLOR_NAMES[usize::from(index)],
                            None,
                            None,
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(picker) = this.icon_picker_mut() {
                                picker.color = index;
                            }
                            cx.notify();
                        }))
                        .child(div().size(px(16.0)).rounded_full().bg(rgb(swatch))),
                )
            }))
            .into_any_element()
    }

    /// Every Lucide icon the search finds, in rows that draw only while in view, in the
    /// picked colour.
    fn render_icon_grid(&self, picker: &IconPicker, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let found: Rc<[(&'static str, &'static str)]> = lucide_icons::search(&self.ime_text).into();
        if found.is_empty() {
            return div()
                .h(px(CELL * VISIBLE_ROWS))
                .flex()
                .items_center()
                .justify_center()
                .child(self.icon_picker_caption("No icons found."))
                .into_any_element();
        }
        let tint = self
            .icon_color(usize::from(picker.color))
            .unwrap_or(colors.foreground);
        let selected = picker.icon.clone();
        let rows = found.len().div_ceil(COLUMNS);
        uniform_list(
            "project-icon-grid",
            rows,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let icons = &found[row * COLUMNS..((row + 1) * COLUMNS).min(found.len())];
                        div()
                            .h(px(CELL))
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(icons.iter().map(|&(name, path)| {
                                picker_cell(
                                    colors,
                                    name,
                                    name == selected,
                                    name,
                                    gpui::svg()
                                        .flex_none()
                                        .size(px(20.0))
                                        .path(path)
                                        .text_color(rgb(tint)),
                                    move |picker| picker.icon = name.to_owned(),
                                    cx,
                                )
                            }))
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .w_full()
        .h(px(CELL * VISIBLE_ROWS))
        .track_scroll(&picker.grid)
        .into_any_element()
    }

    /// The emoji offered, the picked one ringed.
    fn render_emoji_grid(&self, picker: &IconPicker, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex()
            .flex_col()
            .children(EMOJIS.chunks(COLUMNS).enumerate().map(|(row, emojis)| {
                div().h(px(CELL)).flex().items_center().gap_1().children(
                    emojis.iter().enumerate().map(|(column, &(emoji, label))| {
                        picker_cell(
                            colors,
                            ("project-icon-emoji", row * COLUMNS + column),
                            picker.emoji == emoji,
                            label,
                            div().text_size(px(22.0)).line_height(px(28.0)).child(emoji),
                            move |picker| picker.emoji = emoji.to_owned(),
                            cx,
                        )
                    }),
                )
            }))
            .into_any_element()
    }

    /// Change icon: the icon it would save beside the project's name, the kinds, then
    /// the chosen kind's choices, then Cancel and Save icon.
    pub(super) fn render_icon_picker(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let Some(picker) = self.icon_picker() else {
            unreachable!("Change icon requires its overlay");
        };
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (viewport_width, _) = overlay_viewport_size(window);
        let choice = self.icon_choice(picker);
        let preview = match &choice {
            Ok(Some(icon)) => self.chosen_icon(icon, 40.0, || div().into_any_element()),
            Ok(None) => Some(self.automatic_project_icon(&picker.project, 40.0)),
            Err(_) => None,
        }
        .unwrap_or_else(|| self.project_icon(Some(&picker.project), 40.0));
        let kinds = IconKind::ALL.map(|kind| {
            controls::text_button(("project-icon-kind", kind.index()), kind.label())
                .selected(picker.kind == kind)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_icon_kind(kind);
                    window.focus(&this.focus_handle, cx);
                    cx.notify();
                }))
        });
        let choices = match picker.kind {
            IconKind::Automatic => div()
                .flex()
                .items_center()
                .gap_3()
                .child(self.automatic_project_icon(&picker.project, 24.0))
                .child(self.icon_picker_caption(
                    "The project's favicon or logo, found in its folder; else its first \
                     letter, in a colour of the theme.",
                )),
            IconKind::Icons => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.render_icon_picker_field("Search all Lucide icons", true, cx))
                .child(self.render_icon_grid(picker, cx)),
            IconKind::Emoji => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.render_emoji_grid(picker, cx))
                .child(self.icon_picker_caption("Or paste any emoji"))
                .child(self.render_icon_picker_field("Paste an emoji", false, cx)),
            IconKind::Monogram => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.icon_picker_caption("Letters"))
                .child(self.render_icon_picker_field("One or two letters or numbers", false, cx))
                .child(
                    div()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(modal_text_color(
                            if choice.is_ok() {
                                colors.muted
                            } else {
                                colors.error
                            },
                            &colors,
                        )))
                        .child("One or two letters or numbers."),
                ),
            IconKind::File => div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    controls::text_button("project-icon-file", "Choose file…")
                        .header()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choose_icon_file(cx);
                            cx.notify();
                        })),
                )
                .child(match &picker.file {
                    Some(path) => self.icon_picker_caption(format!(
                        "{}, kept in Compi's data folder.",
                        content::file_name(path)
                    )),
                    None => self.icon_picker_caption("A PNG, SVG or ICO image."),
                }),
        };
        self.render_centered_scrim(
            div()
                .w(px((viewport_width - 32.0).clamp(1.0, 520.0)))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.render_dialog_header("Change icon", cx))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div().flex().items_center().gap_3().child(preview).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(self.project_name(&picker.project)),
                                    )
                                    .child(self.icon_picker_caption(
                                        "Automatic, an icon, an emoji, a monogram or an image.",
                                    )),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .child(controls::segmented("project-icon-kinds", kinds)),
                        )
                        .when(
                            matches!(picker.kind, IconKind::Icons | IconKind::Monogram),
                            |body| {
                                body.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(self.icon_picker_caption("Colour"))
                                        .child(self.render_icon_colors(picker.color, cx)),
                                )
                            },
                        )
                        .child(choices),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .child("Tab switches kind · arrows pick · Enter saves"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .gap_2()
                                .child(
                                    controls::text_button("project-icon-cancel", "Cancel")
                                        .header()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.dismiss_overlay();
                                            window.focus(&this.focus_handle, cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    controls::text_button("project-icon-save", "Save icon")
                                        .header()
                                        .primary()
                                        .key("enter")
                                        .disabled_because(choice.err())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.save_icon_choice(window, cx);
                                        })),
                                ),
                        ),
                ),
            window,
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_monogram_is_one_or_two_letters_or_numbers() {
        assert_eq!(monogram(" ab ").as_deref(), Some("AB"));
        assert_eq!(monogram("7").as_deref(), Some("7"));
        assert_eq!(monogram("ée").as_deref(), Some("ÉE"));
        assert_eq!(monogram("abc"), None);
        assert_eq!(monogram("a-"), None);
        assert_eq!(monogram("  "), None);
    }

    #[test]
    fn a_pasted_emoji_is_its_first_whole_emoji() {
        assert_eq!(first_emoji(" 🚀 launch"), Some("🚀"));
        assert_eq!(first_emoji("👩‍💻!"), Some("👩‍💻"));
        assert_eq!(first_emoji("👍🏽"), Some("👍🏽"));
        assert_eq!(first_emoji("🇫🇷🇩🇪"), Some("🇫🇷"));
        assert_eq!(first_emoji("1️⃣"), Some("1️⃣"));
        assert_eq!(first_emoji("⚙️"), Some("⚙️"));
        assert_eq!(first_emoji("abc"), None);
        assert_eq!(first_emoji("字"), None);
        assert_eq!(first_emoji(""), None);
        assert!(
            EMOJIS
                .iter()
                .all(|(emoji, _)| first_emoji(emoji) == Some(*emoji))
        );
    }

    #[test]
    fn a_chosen_file_is_copied_once_by_its_content() {
        let root = std::env::temp_dir().join(format!("compi-project-icons-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let folder = root.join("project-icons");
        let svg = root.join("Logo.SVG");
        fs::write(&svg, "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
        let copy = copy_icon_file(&svg, &folder).unwrap();
        assert_eq!(copy.parent(), Some(folder.as_path()));
        assert_eq!(copy.extension().and_then(|e| e.to_str()), Some("svg"));
        assert_eq!(copy_icon_file(&svg, &folder).unwrap(), copy);
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);

        let fake = root.join("fake.png");
        fs::write(&fake, "not an image").unwrap();
        assert!(
            copy_icon_file(&fake, &folder)
                .unwrap_err()
                .contains("not a PNG")
        );
        let text = root.join("notes.txt");
        fs::write(&text, "x").unwrap();
        assert_eq!(
            copy_icon_file(&text, &folder).unwrap_err(),
            "Choose a PNG, SVG or ICO image."
        );
        fs::remove_dir_all(&root).unwrap();
    }
}

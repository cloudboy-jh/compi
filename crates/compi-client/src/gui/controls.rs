//! The editor surface's controls: one vocabulary over GPUI Kit.
//!
//! Every button, segmented switch, checkbox and chip in a content pane (tab strip, folder
//! sidebar, Changes list, commit box, diff header and diff body) is built here, so they
//! share two sizes, four variants and the same states.
//!
//! - **Sizes.** [`ControlSize::Row`] (20 px, Kit `XSmall`) inside list rows, diff lines,
//!   the find bar and the tab strip; [`ControlSize::Header`] (24 px, Kit `Small`) in
//!   header bars, banners and the commit box.
//! - **Variants.** Ghost for icon actions ([`icon_button`]), outline for secondary text
//!   actions ([`text_button`]), [`Control::primary`] for the one primary action (Commit)
//!   and [`Control::danger`] for destructive ones (Discard).
//! - **States.** Kit's hover, pressed and focus ring (Compi's accent); disabled with its
//!   reason in the tooltip ([`Control::disabled_because`]); loading while the action's
//!   write runs ([`Control::loading`]); selected for switches ([`Control::selected`]).
//! - **Tooltips.** Every icon-only control has one, and an action with a key shows it
//!   through Kit's `Kbd` ([`Control::key`]). Kit's own tooltips need Kit's `Root`, which
//!   Compi does not use, so controls show Kit's `Tooltip` view through GPUI's tooltip
//!   from a wrapper around the control: it also covers a disabled control, whose reason
//!   must still show. Other elements of the surface use [`tooltip`] for the same look.
//! - **Activation.** Actions fire on click (mouse up, or Enter/Space on a focused
//!   control) and stop there: the row or pane behind a control never sees its click. A
//!   pointer press does not take the keyboard focus.
//!
//! [`sync_theme`] completes the Kit theme from Compi's colors before a window draws.
use super::{
    UI_MICRO_TEXT_SIZE, UI_SMALL_TEXT_SIZE, blend_rgb, color, composite_rgb, ui_text_color,
    ui_text_color_for,
};
use crate::theme::ThemeColors;
use gpui::{
    AnyElement, AnyView, App, AssetSource, ClickEvent, ElementId, FocusHandle, FontWeight, Global,
    IntoElement, Keystroke, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::kbd::Kbd;
use gpui_component::spinner::Spinner;
use gpui_component::tag::Tag;
use gpui_component::theme::Theme;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Icon, Selectable as _, Sizable as _, Size};
pub(super) use gpui_kit_assets::IconName;
use std::borrow::Cow;
use std::rc::Rc;

/// Corner radius of controls, fields and tooltips: GPUI's `rounded_sm`, which the rest
/// of Compi's chrome uses.
pub(super) const CONTROL_RADIUS: f32 = 4.0;

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        X,
        RotateCcw,
        ChevronsDownUp,
        List,
        ListTree,
        SquarePen,
        FileDiff,
        GitBranch,
        Columns2,
        Rows2,
    ]
);

/// The application's assets: GPUI Kit's default icons and the extra Lucide icons Compi
/// uses (`ExtraIcons`).
pub(super) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The colors controls derive from a Compi theme, as packed `0xTTRRGGBB` like
/// [`ThemeColors`]. Text colors meet WCAG AA (4.5:1) on every background they sit on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlPalette {
    /// Outline buttons: rest, hover and pressed backgrounds, and their text.
    outline: [u32; 3],
    outline_text: u32,
    /// Ghost buttons: hover and pressed backgrounds, the selected one, and their text.
    ghost_hover: u32,
    ghost_pressed: u32,
    selected: u32,
    ghost_text: u32,
    /// An unselected option of a segmented switch.
    quiet_text: u32,
    primary: [u32; 3],
    primary_text: u32,
    danger: [u32; 3],
    danger_text: u32,
}

impl ControlPalette {
    /// Shades the way Compi's settings buttons do: a blend toward the foreground for
    /// neutral surfaces, toward the accent or error for tinted ones.
    fn derive(colors: &ThemeColors) -> Self {
        let background = colors.background;
        let outline =
            [0.04, 0.09, 0.14].map(|amount| blend_rgb(background, colors.foreground, amount));
        let ghost_pressed = blend_rgb(colors.surface_hover, colors.foreground, 0.08);
        let opaque = background & 0x00ff_ffff;
        let ghost_backdrops = [
            opaque,
            composite_rgb(colors.surface_hover, opaque),
            composite_rgb(ghost_pressed, opaque),
            composite_rgb(colors.selection, opaque),
        ];
        // Hover and pressed move the fill away from its text, so neither loses contrast.
        let text_on_accent = ui_text_color(opaque, colors.accent);
        let away = if super::relative_luminance(text_on_accent & 0x00ff_ffff) < 0.18 {
            0xffffff
        } else {
            0x000000
        };
        let primary = [
            colors.accent,
            blend_rgb(colors.accent, away, 0.12),
            blend_rgb(colors.accent, away, 0.22),
        ];
        let danger = [0.12, 0.2, 0.28].map(|amount| blend_rgb(background, colors.error, amount));
        Self {
            outline,
            outline_text: ui_text_color_for(colors.foreground, &outline),
            ghost_hover: colors.surface_hover,
            ghost_pressed,
            selected: colors.selection,
            ghost_text: ui_text_color_for(colors.foreground, &ghost_backdrops),
            quiet_text: ui_text_color_for(colors.muted, &ghost_backdrops),
            primary,
            primary_text: ui_text_color_for(text_on_accent, &primary),
            danger,
            danger_text: ui_text_color_for(colors.error, &danger),
        }
    }
}

/// The window's Compi colors as controls last installed them; see [`sync_theme`].
struct ControlTheme {
    colors: ThemeColors,
    palette: ControlPalette,
}

impl Global for ControlTheme {}

fn control_theme(cx: &App) -> (ThemeColors, ControlPalette) {
    match cx.try_global::<ControlTheme>() {
        Some(theme) => (theme.colors, theme.palette),
        None => {
            let colors = *crate::theme::ThemePreset::CompiNeutral
                .definition()
                .colors();
            (colors, ControlPalette::derive(&colors))
        }
    }
}

/// Completes GPUI Kit's theme with what its buttons, checkboxes and tooltips read, from
/// `colors`: corner radius, focus ring, and the outline, primary and danger shades with
/// their text. The caller resolves Kit's tokens afterwards.
pub(super) fn sync_theme(theme: &mut Theme, colors: &ThemeColors) {
    let palette = ControlPalette::derive(colors);
    theme.radius = px(CONTROL_RADIUS);
    theme.focus_ring = true;
    theme.ring = color(colors.accent);
    theme.input = color(colors.border);
    theme.popover = color(colors.surface);
    theme.popover_foreground = color(super::modal_text_color(colors.foreground, colors));
    theme.button = color(palette.outline[0]);
    theme.button_hover = color(palette.outline[1]);
    theme.button_active = color(palette.outline[2]);
    theme.button_foreground = color(palette.outline_text);
    theme.primary = color(palette.primary[0]);
    theme.primary_hover = color(palette.primary[1]);
    theme.primary_active = color(palette.primary[2]);
    theme.primary_foreground = color(palette.primary_text);
    theme.button_primary = theme.primary;
    theme.button_primary_hover = theme.primary_hover;
    theme.button_primary_active = theme.primary_active;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.danger = color(colors.error);
    theme.danger_hover = color(palette.danger[1]);
    theme.danger_active = color(palette.danger[2]);
    theme.danger_foreground = color(ui_text_color(colors.background, colors.error));
    theme.button_danger = color(palette.danger[0]);
    theme.button_danger_hover = color(palette.danger[1]);
    theme.button_danger_active = color(palette.danger[2]);
    theme.button_danger_foreground = color(palette.danger_text);
}

/// Records the colors chips and tooltips read; call after [`sync_theme`].
pub(super) fn install_colors(colors: ThemeColors, cx: &mut App) {
    cx.set_global(ControlTheme {
        palette: ControlPalette::derive(&colors),
        colors,
    });
}

/// How tall a control is, and where it goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ControlSize {
    /// 20 px, inside list rows, diff lines, the find bar and the tab strip.
    #[default]
    Row,
    /// 24 px, in header bars, banners and the commit box.
    Header,
}

impl ControlSize {
    pub(super) const fn height(self) -> f32 {
        match self {
            Self::Row => 20.0,
            Self::Header => 24.0,
        }
    }

    fn kit(self) -> Size {
        match self {
            Self::Row => Size::XSmall,
            Self::Header => Size::Small,
        }
    }

    fn text_size(self) -> f32 {
        match self {
            Self::Row => UI_MICRO_TEXT_SIZE,
            Self::Header => UI_SMALL_TEXT_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Ghost,
    Outline,
    Primary,
    Danger,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A button of the editor surface. Build one with [`icon_button`] or [`text_button`].
#[derive(IntoElement)]
pub(super) struct Control {
    id: ElementId,
    icon: Option<Icon>,
    label: Option<SharedString>,
    variant: Variant,
    size: ControlSize,
    tooltip: Option<SharedString>,
    detail: Option<SharedString>,
    key: Option<&'static str>,
    disabled: Option<SharedString>,
    loading: bool,
    selected: bool,
    /// An unselected option of a segmented switch: muted text.
    quiet: bool,
    tab_stop: bool,
    on_click: Option<ClickHandler>,
}

/// An icon-only ghost button with `tooltip` as its title and accessible name.
pub(super) fn icon_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    tooltip: impl Into<SharedString>,
) -> Control {
    Control::new(id.into(), Variant::Ghost)
        .icon(icon)
        .tooltip(tooltip)
}

/// An outline button labeled `label`.
pub(super) fn text_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Control {
    let mut control = Control::new(id.into(), Variant::Outline);
    control.label = Some(label.into());
    control
}

impl Control {
    fn new(id: ElementId, variant: Variant) -> Self {
        Self {
            id,
            icon: None,
            label: None,
            variant,
            size: ControlSize::Row,
            tooltip: None,
            detail: None,
            key: None,
            disabled: None,
            loading: false,
            selected: false,
            quiet: false,
            tab_stop: true,
            on_click: None,
        }
    }

    /// An icon before the label, or the whole face of an icon button.
    pub(super) fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// 24 px, for header bars; controls are 20 px otherwise.
    pub(super) fn header(mut self) -> Self {
        self.size = ControlSize::Header;
        self
    }

    /// Transparent until hovered: for actions that sit among content.
    pub(super) fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }

    /// Filled with the accent: the one primary action of its surface.
    pub(super) fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }

    /// Tinted with the theme's error color: an action that throws work away.
    pub(super) fn danger(mut self) -> Self {
        self.variant = Variant::Danger;
        self
    }

    /// The tooltip's title. Icon buttons take it as their accessible name.
    pub(super) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// A muted line under the tooltip's title: what the action does or affects.
    pub(super) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The key that runs the action too, shown in the tooltip; GPUI keystroke syntax
    /// (`"s"`, `"secondary-enter"`).
    pub(super) fn key(mut self, key: &'static str) -> Self {
        self.key = Some(key);
        self
    }

    /// Disabled while `reason` is set; the reason shows in the tooltip.
    pub(super) fn disabled_because(mut self, reason: Option<impl Into<SharedString>>) -> Self {
        self.disabled = reason.map(Into::into);
        self
    }

    /// A spinner while the action's work runs; the control ignores the pointer meanwhile.
    pub(super) fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// The current choice of a switch, or a toggle that is on.
    pub(super) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Whether Tab reaches the control (default). Row actions turn it off: a list's own
    /// keyboard selection reaches them.
    pub(super) fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    pub(super) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Control {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (_, palette) = control_theme(cx);
        let size = self.size;
        let mut button = Button::new("control")
            .with_size(size.kit())
            .tab_stop(self.tab_stop)
            .selected(self.selected)
            .disabled(self.disabled.is_some())
            .loading(self.loading);
        if self.selected || self.quiet {
            button = button.toggled(self.selected);
        }
        button = match self.variant {
            Variant::Ghost => button.custom(
                ButtonCustomVariant::new(cx)
                    .foreground(color(if self.quiet {
                        palette.quiet_text
                    } else {
                        palette.ghost_text
                    }))
                    .hover(color(palette.ghost_hover))
                    .active(color(if self.selected {
                        palette.selected
                    } else {
                        palette.ghost_pressed
                    })),
            ),
            Variant::Outline => button,
            Variant::Primary => button.primary(),
            Variant::Danger => button.danger(),
        };
        match (self.icon, self.loading) {
            (Some(icon), _) => button = button.icon(icon),
            (None, true) => button = button.icon(Spinner::new()),
            (None, false) => {}
        }
        if let Some(label) = &self.label {
            button = button
                .accessibility_label(label.clone())
                .when(size == ControlSize::Row, |button| button.px_2())
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(size.text_size()))
                        .when(self.variant == Variant::Primary, |label| {
                            label.font_weight(FontWeight::MEDIUM)
                        })
                        .child(label.clone()),
                );
        } else if let Some(tooltip) = &self.tooltip {
            button = button.accessibility_label(tooltip.clone());
        }
        if let Some(on_click) = self.on_click {
            button = button.on_click(move |event, window, cx| {
                cx.stop_propagation();
                on_click(event, window, cx);
            });
        }
        // The title is the tooltip; a label that already says it needs one only to
        // explain, to name a key or to give a reason.
        let title = self.tooltip.or_else(|| {
            (self.detail.is_some() || self.key.is_some() || self.disabled.is_some())
                .then(|| self.label.clone())
                .flatten()
        });
        let detail = self.disabled.or(self.detail);
        let key = self.key;
        div()
            .id(self.id)
            .flex_none()
            .flex()
            .items_center()
            .child(button)
            .when_some(title, |wrapper, title| {
                wrapper.tooltip(tooltip(title, detail, key))
            })
    }
}

/// A switch between options in one bordered strip: `options` are controls (from
/// [`text_button`] or [`icon_button`]), the current one [`Control::selected`]. 24 px
/// tall, the header size, around 20 px options.
pub(super) fn segmented(
    id: impl Into<ElementId>,
    options: impl IntoIterator<Item = Control>,
) -> Segmented {
    Segmented {
        id: id.into(),
        options: options.into_iter().collect(),
    }
}

#[derive(IntoElement)]
pub(super) struct Segmented {
    id: ElementId,
    options: Vec<Control>,
}

impl RenderOnce for Segmented {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, _) = control_theme(cx);
        div()
            .id(self.id)
            .flex_none()
            .flex()
            .items_center()
            .h(px(ControlSize::Header.height()))
            .p(px(1.0))
            .gap(px(1.0))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(color(colors.border))
            .children(self.options.into_iter().map(|mut option| {
                option.variant = Variant::Ghost;
                option.size = ControlSize::Row;
                option.quiet = !option.selected;
                option
            }))
    }
}

/// A labeled checkbox. Build with [`checkbox`].
#[derive(IntoElement)]
pub(super) struct CheckControl {
    id: ElementId,
    label: SharedString,
    checked: bool,
    size: ControlSize,
    tooltip: Option<SharedString>,
    detail: Option<SharedString>,
    disabled: Option<SharedString>,
    on_toggle: Option<ToggleHandler>,
}

/// A checkbox labeled `label`, checked when `checked`.
pub(super) fn checkbox(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
) -> CheckControl {
    CheckControl {
        id: id.into(),
        label: label.into(),
        checked,
        size: ControlSize::Row,
        tooltip: None,
        detail: None,
        disabled: None,
        on_toggle: None,
    }
}

impl CheckControl {
    pub(super) fn header(mut self) -> Self {
        self.size = ControlSize::Header;
        self
    }

    pub(super) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub(super) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(super) fn disabled_because(mut self, reason: Option<impl Into<SharedString>>) -> Self {
        self.disabled = reason.map(Into::into);
        self
    }

    /// Runs with the new state when the box is clicked or toggled from the keyboard.
    pub(super) fn on_toggle(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CheckControl {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut check = Checkbox::new("check")
            .label(self.label)
            .checked(self.checked)
            .with_size(Size::XSmall)
            .disabled(self.disabled.is_some())
            .text_size(px(self.size.text_size()));
        if let Some(on_toggle) = self.on_toggle {
            check = check.on_click(move |checked, window, cx| {
                cx.stop_propagation();
                on_toggle(*checked, window, cx);
            });
        }
        let detail = self.disabled.or(self.detail);
        div()
            .id(self.id)
            .flex_none()
            .h(px(self.size.height()))
            .px_1()
            .flex()
            .items_center()
            .child(check)
            .when_some(self.tooltip, |wrapper, title| {
                wrapper.tooltip(tooltip(title, detail, None))
            })
    }
}

/// What a chip's text says about its subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChipTone {
    /// A plain fact: Read-only, staged.
    Neutral,
    /// Work that goes away: a deletion.
    Danger,
    /// Text in a theme color (packed like [`ThemeColors`]), made legible on the chip.
    Tint(u32),
}

/// A small non-interactive label: a change's kind, Read-only, staged.
pub(super) fn chip(label: impl Into<SharedString>, tone: ChipTone) -> Chip {
    Chip {
        label: label.into(),
        tone,
    }
}

#[derive(IntoElement)]
pub(super) struct Chip {
    label: SharedString,
    tone: ChipTone,
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, _) = control_theme(cx);
        let background = colors.surface_hover;
        let text = match self.tone {
            ChipTone::Neutral => colors.muted,
            ChipTone::Danger => colors.error,
            ChipTone::Tint(tint) => tint,
        };
        Tag::custom(
            color(background),
            color(ui_text_color(text, background)),
            color(background),
        )
        .with_size(Size::XSmall)
        .rounded(px(CONTROL_RADIUS))
        .flex_none()
        .text_size(px(UI_MICRO_TEXT_SIZE))
        .child(self.label)
    }
}

/// A non-interactive icon in `tint`, 12 px: a row's fold chevron, a tab's kind mark.
pub(super) fn glyph(icon: IconName, tint: gpui::Hsla) -> Icon {
    Icon::new(icon).with_size(Size::XSmall).text_color(tint)
}

/// A tooltip with `title`, an optional muted `detail` line and an optional `key`
/// (GPUI keystroke syntax) as Kit's `Kbd`, for GPUI's `tooltip`.
pub(super) fn tooltip(
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    key: Option<&'static str>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let title = title.into();
    move |window, cx| {
        let (title, detail) = (title.clone(), detail.clone());
        Tooltip::element(move |_, cx| tooltip_text(title.clone(), detail.clone(), key, cx))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .rounded(px(CONTROL_RADIUS))
            .build(window, cx)
    }
}

/// The tooltip's lines. The key is drawn here rather than by Kit's tooltip, which paints
/// it in the theme's dimmed line-number color; here it keeps AA contrast.
fn tooltip_text(
    title: SharedString,
    detail: Option<SharedString>,
    key: Option<&'static str>,
    cx: &App,
) -> AnyElement {
    let (colors, _) = control_theme(cx);
    let muted = color(super::modal_text_color(colors.muted, &colors));
    div()
        .py_0p5()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .font_weight(FontWeight::MEDIUM)
                .child(title)
                .children(key.and_then(kbd).map(|key| {
                    key.outline()
                        .text_color(muted)
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .font_weight(FontWeight::NORMAL)
                })),
        )
        .when_some(detail, |text, detail| {
            text.child(div().text_color(muted).child(detail))
        })
        .into_any_element()
}

/// Kit's `Kbd` for a key in GPUI keystroke syntax.
pub(super) fn kbd(key: &str) -> Option<Kbd> {
    Keystroke::parse(key).ok().map(Kbd::new)
}

/// Moves the keyboard focus to the next (or previous) control inside `within`, wrapping
/// around inside it; controls elsewhere in the window are skipped. Returns whether a
/// control took the focus.
pub(super) fn cycle_focus(
    within: &FocusHandle,
    forward: bool,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let start = window.focused(cx);
    // Bounded: the window's tab stops are finite, and the walk stops once it is back.
    for _ in 0..512 {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        let Some(focused) = window.focused(cx) else {
            return false;
        };
        if Some(&focused) == start.as_ref() {
            return false;
        }
        if focused != *within && within.contains_focused(window, cx) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemePreset;

    #[test]
    fn control_text_meets_aa_on_every_bundled_theme() {
        let mut failures = Vec::new();
        for preset in ThemePreset::ALL {
            let definition = preset.definition();
            let colors = definition.colors();
            let palette = ControlPalette::derive(colors);
            let opaque = colors.background & 0x00ff_ffff;
            let checks = palette
                .outline
                .iter()
                .map(|&background| (palette.outline_text, background, "outline"))
                .chain(
                    palette
                        .primary
                        .iter()
                        .map(|&background| (palette.primary_text, background, "primary")),
                )
                .chain(
                    palette
                        .danger
                        .iter()
                        .map(|&background| (palette.danger_text, background, "danger")),
                )
                .chain(
                    [
                        opaque,
                        palette.ghost_hover,
                        palette.ghost_pressed,
                        palette.selected,
                    ]
                    .into_iter()
                    .flat_map(|background| {
                        let background = composite_rgb(background, opaque);
                        [
                            (palette.ghost_text, background, "ghost"),
                            (palette.quiet_text, background, "segment"),
                        ]
                    }),
                );
            for (text, background, role) in checks {
                let ratio = super::super::contrast_ratio(text, background);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{} {role}: {text:08x} on {background:08x} is {ratio:.2}:1",
                        definition.id(),
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn extra_icons_load_beside_the_default_ones() {
        for icon in [
            IconName::X,
            IconName::RotateCcw,
            IconName::ChevronsDownUp,
            IconName::List,
            IconName::ListTree,
            IconName::SquarePen,
            IconName::FileDiff,
            IconName::GitBranch,
            IconName::Columns2,
            IconName::Rows2,
            IconName::Plus,
            IconName::Minus,
            IconName::ChevronDown,
            IconName::Loader,
        ] {
            let path = icon.path();
            assert!(
                Assets.load(&path).ok().flatten().is_some(),
                "{path} is not bundled"
            );
        }
    }
}

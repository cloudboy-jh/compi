//! The editor surface's controls: one vocabulary over GPUI Kit.
//!
//! Every button, segmented switch, on/off switch, checkbox and chip in a content pane
//! (tab strip, folder sidebar, Changes list, commit box, diff header and diff body,
//! Settings) and in dialogs is built here, so they share two sizes, four variants and
//! the same states.
//!
//! - **Sizes.** [`ControlSize::Row`] (20 px, Kit `XSmall`) inside list rows, diff lines,
//!   the find bar and the tab strip; [`ControlSize::Header`] (24 px, Kit `Small`) in
//!   header bars, banners, the commit box, Settings and dialogs.
//! - **Variants.** Ghost for icon actions ([`icon_button`]), outline for secondary text
//!   actions ([`text_button`]), [`Control::primary`] for the one primary action (Commit)
//!   and [`Control::danger`] for destructive ones (Discard).
//! - **States.** Kit's hover, pressed and focus ring (Compi's accent); disabled with its
//!   reason in the tooltip ([`Control::disabled_because`]); loading while the action's
//!   write runs ([`Control::loading`]); selected for switches ([`Control::selected`]).
//!   Surfaces that keep their own keyboard highlight (Settings, dialogs) ring the
//!   highlighted control with [`highlight`].
//! - **Tooltips.** Every icon-only control has one, and an action with a key shows it
//!   through Kit's `Kbd` ([`Control::key`]). Kit's own tooltips need Kit's `Root`, which
//!   Compi does not use, so controls show Kit's `Tooltip` view through GPUI's tooltip
//!   from a wrapper around the control: it also covers a disabled control, whose reason
//!   must still show. Other elements of the surface use [`tooltip`] for the same look.
//! - **Activation.** Actions fire on click (mouse up, or Enter/Space on a focused
//!   control) and stop there: the row or pane behind a control never sees its click. A
//!   pointer press does not take the keyboard focus.
//! - **Popups.** [`select`] picks one of a few options: the closed control is as wide as
//!   its value, the list opens under it as wide as its longest option (up to 320 px),
//!   scrolls past 320 px, checks the current option and takes arrows, typed letters,
//!   Enter and Escape. [`menu`] is Kit's popup menu of a few actions under a button,
//!   [`context_menu`] the same menu at a point (a right-click, a pane's grip). Menus
//!   take [`MenuEntry`] rows: actions with a shortcut hint or a disabled reason,
//!   separators, group labels and submenus, and are as wide as their longest row (up
//!   to 320 px). [`popover`] anchors any content to its button (Quick Appearance, a
//!   confirm). The caller owns each one's open state, so a command or a key can open
//!   it, and Kit reports clicks on the trigger, clicks outside and Escape. While open,
//!   a popup holds the keyboard: no key reaches the window behind it, and Escape
//!   closes only the popup. They need no Kit `Root`, unlike Kit's own tooltips. A menu
//!   Kit's popup menu cannot host draws its rows with [`menu_surface`], [`menu_row`],
//!   [`menu_separator_row`] and [`menu_label_row`], which match Kit's.
//!
//! [`sync_theme`] completes the Kit theme from Compi's colors before a window draws;
//! popups take the surface color, its text and the theme's border.
use super::{
    UI_MICRO_TEXT_SIZE, UI_SMALL_TEXT_SIZE, blend_rgb, color, composite_rgb, ui_text_color,
    ui_text_color_for,
};
use crate::theme::ThemeColors;
use gpui::{
    Anchor, AnyElement, AnyView, App, AssetSource, ClickEvent, Context, DismissEvent, Div,
    ElementId, Entity, FocusHandle, Focusable as _, FontWeight, Global, IntoElement, KeyDownEvent,
    Keystroke, ParentElement, Pixels, Point, RenderOnce, ScrollHandle, SharedString, Stateful,
    StyleRefinement, Styled, Subscription, Window, anchored, deferred, div, prelude::*, px,
};
use gpui_base::actions::Confirm;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::kbd::Kbd;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::spinner::Spinner;
use gpui_component::switch::Switch;
use gpui_component::tag::Tag;
use gpui_component::theme::Theme;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable, Sizable as _, Size, ThemeStyled as _,
};
pub(super) use gpui_kit_assets::IconName;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        Circle,
        LayoutPanelLeft,
        Keyboard,
        Gauge,
        CircleArrowDown,
        SlidersHorizontal,
        Command,
        Gpu,
        Server,
        Clock,
        Bell,
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
    /// Text of a select's list on the popup surface, plain and muted, at rest and
    /// highlighted.
    popup_text: u32,
    popup_muted: u32,
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
        let surface = composite_rgb(colors.surface, opaque);
        let popup_backdrops = [surface, composite_rgb(colors.surface_hover, surface)];
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
            popup_text: ui_text_color_for(colors.foreground, &popup_backdrops),
            popup_muted: ui_text_color_for(colors.muted, &popup_backdrops),
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

/// Completes GPUI Kit's theme with what its buttons, checkboxes, tooltips and popups
/// read, from `colors`: corner radius, focus ring, the outline, primary and danger
/// shades with their text, and the popup surface. The caller resolves Kit's tokens
/// afterwards.
pub(super) fn sync_theme(theme: &mut Theme, colors: &ThemeColors) {
    let palette = ControlPalette::derive(colors);
    theme.radius = px(CONTROL_RADIUS);
    theme.focus_ring = true;
    theme.ring = color(colors.accent);
    theme.input = color(colors.border);
    // Popups float over translucent panes and dialogs: an opaque surface keeps their
    // text at the contrast `ControlPalette` checks.
    theme.popover = color(composite_rgb(
        colors.surface,
        colors.background & 0x00ff_ffff,
    ));
    theme.popover_foreground = color(palette.popup_text);
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
    // A switch's thumb is the text color on the accent, on its track when off too.
    theme.switch = color(blend_rgb(colors.background, colors.foreground, 0.3));
    theme.switch_thumb = color(palette.primary_text);
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
type SwitchHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

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
    /// The menu this button opened is showing: its tooltip would cover the menu.
    menu_open: bool,
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
            menu_open: false,
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

    /// Hides the tooltip while the menu this button opened is showing.
    pub(super) fn menu_open(mut self, open: bool) -> Self {
        self.menu_open = open;
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
        let title = title.filter(|_| !self.menu_open);
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

/// An on/off switch. Build with [`switch`].
#[derive(IntoElement)]
pub(super) struct SwitchControl {
    id: ElementId,
    checked: bool,
    label: Option<SharedString>,
    on_toggle: Option<SwitchHandler>,
}

/// Kit's switch, on when `checked`: 28 × 16 px, the accent when on. It has no text of
/// its own; the row beside it says what it turns on.
pub(super) fn switch(id: impl Into<ElementId>, checked: bool) -> SwitchControl {
    SwitchControl {
        id: id.into(),
        checked,
        label: None,
        on_toggle: None,
    }
}

impl SwitchControl {
    /// The name a screen reader announces: the setting's title.
    pub(super) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Runs with the requested state when the switch is clicked or toggled from the
    /// keyboard; a `cx.listener` fits.
    pub(super) fn on_toggle(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SwitchControl {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut control = Switch::new("switch")
            .checked(self.checked)
            .with_size(Size::Small);
        if let Some(label) = self.label {
            control = control.accessibility_label(label);
        }
        if let Some(on_toggle) = self.on_toggle {
            control = control.on_click(move |checked, window, cx| {
                cx.stop_propagation();
                on_toggle(checked, window, cx);
            });
        }
        div()
            .id(self.id)
            .flex_none()
            .h(px(ControlSize::Header.height()))
            .flex()
            .items_center()
            .child(control)
    }
}

/// `child` with an accent ring around it while `highlighted`: the keyboard highlight of
/// a surface that keeps its own (Settings, dialogs) rather than GPUI focus.
pub(super) fn highlight(highlighted: bool, child: impl IntoElement, cx: &App) -> Div {
    let (colors, _) = control_theme(cx);
    div()
        .relative()
        .flex_none()
        .child(child)
        .when(highlighted, |wrapper| {
            wrapper.child(
                div()
                    .absolute()
                    .top(px(-2.0))
                    .left(px(-2.0))
                    .right(px(-2.0))
                    .bottom(px(-2.0))
                    .rounded(px(CONTROL_RADIUS + 2.0))
                    .border_2()
                    .border_color(color(colors.accent)),
            )
        })
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
        Tooltip::element(move |window, cx| {
            tooltip_text(title.clone(), detail.clone(), key, window, cx)
        })
        .text_size(px(UI_SMALL_TEXT_SIZE))
        .rounded(px(CONTROL_RADIUS))
        .build(window, cx)
    }
}

/// How far Kit's tooltip popup (gpui-component 0.7.1) reaches past its content: its
/// `px_2`/`py_0p5` padding and 1 px border.
const TOOLTIP_INSET: (f32, f32) = (9.0, 3.0);

/// The tooltip's lines, cut out of any web page they are drawn over. The key is drawn
/// here rather than by Kit's tooltip, which paints it in the theme's dimmed line-number
/// color; here it keeps AA contrast.
fn tooltip_text(
    title: SharedString,
    detail: Option<SharedString>,
    key: Option<&'static str>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let (colors, _) = control_theme(cx);
    let muted = color(super::modal_text_color(colors.muted, &colors));
    div()
        .relative()
        .py_0p5()
        .flex()
        .flex_col()
        .gap_0p5()
        .children(super::workspace::browser::above_pages(
            TOOLTIP_INSET,
            window,
            cx,
        ))
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

/// A key in GPUI keystroke syntax as the platform writes it (Ctrl+Shift+W), for a
/// title that carries its shortcut in text.
pub(super) fn key_label(key: &str) -> String {
    Keystroke::parse(key).map_or_else(|_| key.to_owned(), |key| Kbd::format(&key))
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

/// Widest a select's list or a menu grows; longer items truncate.
const POPUP_MAX_WIDTH: f32 = 320.0;
/// Tallest a select's list grows before it scrolls.
const POPUP_MAX_HEIGHT: f32 = 320.0;
/// How long typed letters keep extending a select's type-to-jump prefix.
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(800);

type OpenHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;
type KeyHandler = Rc<dyn Fn(&Keystroke, &mut Window, &mut App)>;
type PickHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

/// What a popup opens from. Kit's popover takes a [`Selectable`] trigger; the caller
/// already draws the open state.
#[derive(IntoElement)]
struct Trigger {
    element: AnyElement,
    open: bool,
}

impl Selectable for Trigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.element
    }
}

/// A child id of `id`, for the state a popup keeps beside Kit's.
fn child_id(id: &ElementId, name: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), name.into())
}

/// The focus a popup's content takes while it is open; it lives as long as the
/// control is drawn, so Kit's popover can move focus to it as it opens.
fn popup_focus(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window
        .use_keyed_state(child_id(id, "focus"), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone()
}

/// Kit's popover under `trigger`, controlled by `open`: a click on the trigger, a
/// click outside and Escape report the new state through `on_open_change`.
fn kit_popover(
    id: ElementId,
    trigger: AnyElement,
    open: bool,
    anchor: Anchor,
    on_open_change: Option<OpenHandler>,
) -> Popover {
    Popover::new(id)
        .anchor(anchor)
        .open(open)
        .trigger(Trigger {
            element: trigger,
            open,
        })
        .when_some(on_open_change, |popover, handler| {
            popover.on_open_change(move |open, window, cx| handler(open, window, cx))
        })
}

/// The content of a popup holding the keyboard: `on_key` sees every key except
/// Escape, which closes the popup, and no key reaches the window behind it. Kit's
/// popover turns Enter and Space into its Confirm action; it arrives as `enter`.
fn popup_keys(focus: &FocusHandle, on_key: Option<KeyHandler>) -> Div {
    let confirm = on_key.clone();
    div()
        .track_focus(focus)
        .on_action(move |_: &Confirm, window, cx| {
            if let (Some(on_key), Ok(enter)) = (&confirm, Keystroke::parse("enter")) {
                on_key(&enter, window, cx);
            }
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            cx.stop_propagation();
            if let Some(on_key) = &on_key {
                on_key(&event.keystroke, window, cx);
            }
        })
}

/// Content anchored to a trigger. Build with [`popover`].
#[derive(IntoElement)]
pub(super) struct PopoverControl {
    id: ElementId,
    trigger: AnyElement,
    open: bool,
    anchor: Anchor,
    on_open_change: Option<OpenHandler>,
    on_key: Option<KeyHandler>,
    content: Option<AnyElement>,
    full_width: bool,
}

/// A popover attached to `trigger`, open while `open`. The caller owns the state:
/// commands and keys open it by setting `open`, and [`PopoverControl::on_open_change`]
/// reports a click on the trigger, a click outside and Escape.
pub(super) fn popover(
    id: impl Into<ElementId>,
    trigger: impl IntoElement,
    open: bool,
) -> PopoverControl {
    PopoverControl {
        id: id.into(),
        trigger: trigger.into_any_element(),
        open,
        anchor: Anchor::TopLeft,
        on_open_change: None,
        on_key: None,
        content: None,
        full_width: false,
    }
}

impl PopoverControl {
    /// The popover's corner that meets the trigger; the default, top left, opens it
    /// below the trigger.
    pub(super) fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub(super) fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }

    /// Keys while the popover is open; see [`popup_keys`].
    pub(super) fn on_key(
        mut self,
        handler: impl Fn(&Keystroke, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_key = Some(Rc::new(handler));
        self
    }

    /// Lets the trigger fill its slot, as a list row does.
    pub(super) fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// What the popover shows; built only while it is open.
    pub(super) fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }
}

impl RenderOnce for PopoverControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = popup_focus(&self.id, window, cx);
        let content = self.content.filter(|_| self.open);
        kit_popover(
            self.id,
            self.trigger,
            self.open,
            self.anchor,
            self.on_open_change,
        )
        .track_focus(&focus)
        .when(self.full_width, |popover| {
            popover.trigger_style(StyleRefinement::default().w_full())
        })
        .when_some(content, |popover, content| {
            popover.child(popup_keys(&focus, self.on_key).child(content))
        })
    }
}

/// One choice of a [`select`].
#[derive(Clone)]
pub(super) struct SelectOption {
    label: SharedString,
    detail: Option<SharedString>,
    font: Option<SharedString>,
}

pub(super) fn select_option(label: impl Into<SharedString>) -> SelectOption {
    SelectOption {
        label: label.into(),
        detail: None,
        font: None,
    }
}

impl SelectOption {
    /// A small muted line under the label.
    pub(super) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Draws the label and detail in `family`: a font list shows each font.
    pub(super) fn font(mut self, family: impl Into<SharedString>) -> Self {
        self.font = Some(family.into());
        self
    }
}

/// A choice among options. Build with [`select`].
#[derive(IntoElement)]
pub(super) struct Select {
    id: ElementId,
    placeholder: SharedString,
    options: Rc<[SelectOption]>,
    selected: Option<usize>,
    open: bool,
    focused: bool,
    anchor: Anchor,
    on_open_change: Option<OpenHandler>,
    on_pick: Option<PickHandler>,
}

/// A select showing `options[selected]`, its list open while `open`. The closed
/// control is as wide as its value; the list opens under it, as wide as its longest
/// option up to a cap, with the current one checked. Arrows, Home and End move, typed
/// letters jump to a matching option, Enter picks, Escape closes.
pub(super) fn select(
    id: impl Into<ElementId>,
    options: impl IntoIterator<Item = SelectOption>,
    selected: Option<usize>,
    open: bool,
) -> Select {
    Select {
        id: id.into(),
        placeholder: SharedString::default(),
        options: options.into_iter().collect(),
        selected,
        open,
        focused: false,
        anchor: Anchor::TopLeft,
        on_open_change: None,
        on_pick: None,
    }
}

impl Select {
    /// What the closed control shows while no option is selected.
    pub(super) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The accent ring of a surface that tracks its own keyboard focus (Settings).
    pub(super) fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Which corner of the list meets the trigger; [`Anchor::TopRight`] for a select at
    /// the right end of a row, so its list opens leftward instead of past the edge.
    pub(super) fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub(super) fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }

    /// Runs with the picked option's index; the list closes after it.
    pub(super) fn on_pick(
        mut self,
        handler: impl Fn(&usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }
}

/// An open select list's keyboard position.
struct SelectList {
    highlight: usize,
    typed: String,
    typed_at: Option<Instant>,
    scroll: ScrollHandle,
}

/// Where typing `typed` jumps from `from`: the next option starting with it, or for a
/// repeated letter the next option starting with that letter.
fn jump_target(labels: &[SharedString], from: usize, typed: &str) -> Option<usize> {
    let mut letters = typed.chars();
    let first = letters.next()?;
    let (prefix, start) = if letters.all(|letter| letter == first) {
        (first.to_string(), from + 1)
    } else {
        (typed.to_owned(), from)
    };
    (0..labels.len())
        .map(|offset| (start + offset) % labels.len())
        .find(|&index| labels[index].to_lowercase().starts_with(&prefix))
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, palette) = control_theme(cx);
        let focus = popup_focus(&self.id, window, cx);
        let current = self
            .selected
            .and_then(|index| self.options.get(index))
            .cloned();
        let ring = self.open || self.focused;
        let trigger = div()
            .flex_none()
            .h(px(ControlSize::Header.height()))
            .max_w(px(POPUP_MAX_WIDTH))
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(color(if ring { colors.accent } else { colors.border }))
            .bg(color(palette.outline[0]))
            .hover(move |style| style.bg(color(palette.outline[1])))
            .cursor_pointer()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(palette.outline_text))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .when_some(
                        current.as_ref().and_then(|option| option.font.clone()),
                        |label, font| label.font_family(font),
                    )
                    .child(current.map_or(self.placeholder.clone(), |option| option.label)),
            )
            .child(glyph(IconName::ChevronDown, color(palette.outline_text)))
            .into_any_element();
        let content = self
            .open
            .then(|| self.render_list(&focus, palette, window, cx));
        kit_popover(
            self.id,
            trigger,
            self.open,
            self.anchor,
            self.on_open_change,
        )
        .track_focus(&focus)
        .p_1()
        .when_some(content, |popover, content| popover.child(content))
    }
}

impl Select {
    fn render_list(
        &self,
        focus: &FocusHandle,
        palette: ControlPalette,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let selected = self.selected;
        let list = window.use_keyed_state(child_id(&self.id, "list"), cx, |_, _| {
            let highlight = selected.unwrap_or(0);
            let scroll = ScrollHandle::new();
            scroll.scroll_to_item(highlight);
            SelectList {
                highlight,
                typed: String::new(),
                typed_at: None,
                scroll,
            }
        });
        let (highlight, scroll) = {
            let list = list.read(cx);
            (list.highlight, list.scroll.clone())
        };
        let pick: PickHandler = {
            let on_pick = self.on_pick.clone();
            let on_open_change = self.on_open_change.clone();
            Rc::new(move |index, window, cx| {
                if let Some(on_pick) = &on_pick {
                    on_pick(index, window, cx);
                }
                if let Some(on_open_change) = &on_open_change {
                    on_open_change(&false, window, cx);
                }
            })
        };
        let labels: Rc<[SharedString]> = self
            .options
            .iter()
            .map(|option| option.label.clone())
            .collect();
        let on_key: KeyHandler = {
            let list = list.clone();
            let pick = pick.clone();
            Rc::new(move |key, window, cx| {
                let count = labels.len();
                if count == 0 {
                    return;
                }
                let current = list.read(cx).highlight;
                let next = match key.key.as_str() {
                    "up" => (current + count - 1) % count,
                    "down" => (current + 1) % count,
                    "home" | "pageup" => 0,
                    "end" | "pagedown" => count - 1,
                    "enter" => return pick(&current, window, cx),
                    _ => {
                        let modified =
                            key.modifiers.control || key.modifiers.alt || key.modifiers.platform;
                        let Some(letter) = key.key_char.as_deref().filter(|_| !modified) else {
                            return;
                        };
                        let Some(next) = list.update(cx, |list, _| {
                            let now = Instant::now();
                            if list
                                .typed_at
                                .is_none_or(|at| now.duration_since(at) > TYPE_AHEAD_WINDOW)
                            {
                                list.typed.clear();
                            }
                            list.typed.push_str(&letter.to_lowercase());
                            list.typed_at = Some(now);
                            jump_target(&labels, current, &list.typed)
                        }) else {
                            return;
                        };
                        next
                    }
                };
                list.update(cx, |list, cx| {
                    list.highlight = next;
                    list.scroll.scroll_to_item(next);
                    cx.notify();
                });
            })
        };
        let rows = self.options.iter().enumerate().map(|(index, option)| {
            let pick = pick.clone();
            let list = list.clone();
            div()
                .id(index)
                .min_h(px(ControlSize::Header.height()))
                .px_2()
                .py_0p5()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(CONTROL_RADIUS))
                .when(index == highlight, |row| row.bg(color(palette.ghost_hover)))
                .cursor_pointer()
                .on_mouse_move(move |_, _, cx| {
                    list.update(cx, |list, cx| {
                        if list.highlight != index {
                            list.highlight = index;
                            cx.notify();
                        }
                    });
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    pick(&index, window, cx);
                })
                .child(
                    div()
                        .w(px(12.0))
                        .flex_none()
                        .when(selected == Some(index), |mark| {
                            mark.child(glyph(IconName::Check, color(palette.popup_text)))
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .when_some(option.font.clone(), |text, font| text.font_family(font))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(palette.popup_text))
                                .child(option.label.clone()),
                        )
                        .when_some(option.detail.clone(), |text, detail| {
                            text.child(
                                div()
                                    .truncate()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(palette.popup_muted))
                                    .child(detail),
                            )
                        }),
                )
        });
        popup_keys(focus, Some(on_key)).child(
            div()
                .id(child_id(&self.id, "options"))
                .min_w(px(160.0))
                .max_w(px(POPUP_MAX_WIDTH))
                .max_h(px(POPUP_MAX_HEIGHT))
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .flex()
                .flex_col()
                .gap_px()
                .children(rows),
        )
    }
}

/// One action of a menu.
#[derive(Clone)]
pub(super) struct MenuItem {
    label: SharedString,
    /// A shortcut hint, in GPUI keystroke syntax.
    key: Option<SharedString>,
    /// Why the action is unavailable; shown under its label.
    reason: Option<SharedString>,
    disabled: bool,
    /// A check mark before the label: the option is on.
    checked: bool,
    on_select: ClickHandler,
}

/// A menu action labeled `label`.
pub(super) fn menu_item(
    label: impl Into<SharedString>,
    on_select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> MenuItem {
    MenuItem {
        label: label.into(),
        key: None,
        reason: None,
        disabled: false,
        checked: false,
        on_select: Rc::new(on_select),
    }
}

impl MenuItem {
    pub(super) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Marks the item as the option that is on.
    pub(super) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Disables the action when `reason` is given, and shows the reason under it.
    pub(super) fn disabled_because(mut self, reason: Option<impl Into<SharedString>>) -> Self {
        if let Some(reason) = reason {
            self.disabled = true;
            self.reason = Some(reason.into());
        }
        self
    }

    /// The action's shortcut, in GPUI keystroke syntax, as a hint at the row's end.
    pub(super) fn key(mut self, key: Option<&str>) -> Self {
        self.key = key.map(|key| SharedString::from(key.to_owned()));
        self
    }
}

/// One row of a menu.
#[derive(Clone)]
pub(super) enum MenuEntry {
    Item(MenuItem),
    Separator,
    /// A group's heading.
    Label(SharedString),
    /// A row opening more entries beside the menu.
    Submenu(SharedString, Rc<[MenuEntry]>),
}

impl From<MenuItem> for MenuEntry {
    fn from(item: MenuItem) -> Self {
        Self::Item(item)
    }
}

/// A row opening `entries` beside the menu.
pub(super) fn submenu(
    label: impl Into<SharedString>,
    entries: impl IntoIterator<Item = MenuEntry>,
) -> MenuEntry {
    MenuEntry::Submenu(label.into(), entries.into_iter().collect())
}

/// Kit's menu row for `item`: a plain label, or the label with its shortcut and
/// disabled reason.
fn popup_menu_item(item: &MenuItem) -> PopupMenuItem {
    let row = if item.key.is_none() && item.reason.is_none() {
        PopupMenuItem::new(item.label.clone())
    } else {
        let (label, key, reason) = (item.label.clone(), item.key.clone(), item.reason.clone());
        PopupMenuItem::element(move |_, cx| {
            menu_item_body(label.clone(), key.as_deref(), reason.clone(), cx)
        })
    };
    let on_select = item.on_select.clone();
    row.disabled(item.disabled)
        .checked(item.checked)
        .on_click(move |event, window, cx| on_select(event, window, cx))
}

/// A menu row's label with its shortcut at the end and its disabled reason, muted,
/// under it; Kit's rows and [`menu_row`]s share it.
pub(super) fn menu_item_body(
    label: SharedString,
    key: Option<&str>,
    reason: Option<SharedString>,
    cx: &App,
) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(label)
                .children(key.and_then(kbd).map(|key| {
                    key.p_0()
                        .flex_nowrap()
                        .border_0()
                        .bg(gpui::transparent_white())
                })),
        )
        .children(reason.map(|reason| {
            div()
                .pb_0p5()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .text_color(cx.theme().muted_foreground)
                .child(reason)
        }))
}

/// Fills Kit's `menu` with `entries`.
fn fill_menu(
    mut menu: PopupMenu,
    entries: &[MenuEntry],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = menu.max_w(px(POPUP_MAX_WIDTH));
    for entry in entries {
        menu = match entry {
            MenuEntry::Item(item) => menu.item(popup_menu_item(item)),
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Label(label) => menu.label(label.clone()),
            MenuEntry::Submenu(label, entries) => {
                let entries = entries.clone();
                menu.submenu(label.clone(), window, cx, move |menu, window, cx| {
                    fill_menu(menu, &entries, window, cx)
                })
            }
        };
    }
    menu
}

/// A short list of actions under a button. Build with [`menu`].
#[derive(IntoElement)]
pub(super) struct Menu {
    id: ElementId,
    trigger: AnyElement,
    open: bool,
    anchor: Anchor,
    entries: Rc<[MenuEntry]>,
    on_open_change: Option<OpenHandler>,
}

/// Kit's popup menu of `entries` under `trigger`, open while `open`; like
/// [`popover`], the caller owns the state. Arrows move, Enter runs the item and
/// closes the menu, Escape closes it.
pub(super) fn menu(
    id: impl Into<ElementId>,
    trigger: impl IntoElement,
    open: bool,
    entries: impl IntoIterator<Item = impl Into<MenuEntry>>,
) -> Menu {
    Menu {
        id: id.into(),
        trigger: trigger.into_any_element(),
        open,
        anchor: Anchor::TopLeft,
        entries: entries.into_iter().map(Into::into).collect(),
        on_open_change: None,
    }
}

impl Menu {
    /// See [`PopoverControl::anchor`].
    pub(super) fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub(super) fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }
}

/// The menu an open [`Menu`] shows; built when it opens, dropped when it closes.
#[derive(Default)]
struct MenuState {
    menu: Option<Entity<PopupMenu>>,
}

impl RenderOnce for Menu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state =
            window.use_keyed_state(child_id(&self.id, "menu"), cx, |_, _| MenuState::default());
        if !self.open {
            state.update(cx, |state, _| state.menu = None);
        }
        let entries = self.entries;
        kit_popover(
            self.id,
            self.trigger,
            self.open,
            self.anchor,
            self.on_open_change,
        )
        // The menu draws its own surface and closes itself on a click outside.
        .appearance(false)
        .overlay_closable(false)
        .content(move |_, window, cx| {
            let menu = match state.read(cx).menu.clone() {
                Some(menu) => menu,
                None => {
                    let entries = entries.clone();
                    let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
                        fill_menu(menu, &entries, window, cx)
                    });
                    state.update(cx, |state, _| state.menu = Some(menu.clone()));
                    menu.focus_handle(cx).focus(window, cx);
                    // The menu closes itself (a pick, Escape, a click outside); the
                    // popover follows and reports it.
                    let popover = cx.entity().downgrade();
                    window
                        .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                            if let Some(popover) = popover.upgrade() {
                                popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                            }
                        })
                        .detach();
                    menu
                }
            };
            div()
                .on_key_down(|_, _, cx| cx.stop_propagation())
                .child(menu)
        })
    }
}

/// A menu at a point: what a right-click or a pane's grip opens. Its owner keeps it
/// while it is open and draws it with [`ContextMenu::render`]; build one with
/// [`context_menu`].
pub(super) struct ContextMenu {
    position: Point<Pixels>,
    anchor: Anchor,
    menu: Entity<PopupMenu>,
    _dismissed: Subscription,
}

/// Kit's popup menu of `entries` with its `anchor` corner at `position` (window
/// coordinates), kept inside the window. It takes the keyboard as it opens: arrows
/// move, → opens a submenu and ← leaves it, Enter runs an item, Escape closes. It
/// closes itself after an action, on Escape and on a click outside, then calls
/// `on_dismiss` with its menu so the owner can drop it (see [`ContextMenu::is`]) and
/// returns the keyboard to where it was, unless the action moved it.
pub(super) fn context_menu<V: 'static>(
    position: Point<Pixels>,
    anchor: Anchor,
    entries: impl IntoIterator<Item = MenuEntry>,
    window: &mut Window,
    cx: &mut Context<V>,
    on_dismiss: impl Fn(&mut V, &Entity<PopupMenu>, &mut Window, &mut Context<V>) + 'static,
) -> ContextMenu {
    let entries: Vec<MenuEntry> = entries.into_iter().collect();
    let previous = window.focused(cx);
    let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
        let menu = fill_menu(menu, &entries, window, cx);
        match previous {
            Some(previous) => menu.action_context(previous),
            None => menu,
        }
    });
    menu.focus_handle(cx).focus(window, cx);
    let dismissed = cx.subscribe_in(
        &menu,
        window,
        move |owner, menu, _: &DismissEvent, window, cx| on_dismiss(owner, menu, window, cx),
    );
    ContextMenu {
        position,
        anchor,
        menu,
        _dismissed: dismissed,
    }
}

impl ContextMenu {
    /// Whether this is `menu`: a dismissal reported late, after another menu
    /// opened, leaves that one open.
    pub(super) fn is(&self, menu: &Entity<PopupMenu>) -> bool {
        &self.menu == menu
    }

    /// The open menu, over everything else in the window.
    pub(super) fn render(&self) -> AnyElement {
        deferred(
            anchored()
                .position(self.position)
                .anchor(self.anchor)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    div()
                        .on_key_down(|_, _, cx| cx.stop_propagation())
                        .child(self.menu.clone()),
                ),
        )
        .with_priority(gpui_base::POPUP_PRIORITY)
        .into_any_element()
    }
}

/// The surface of a menu drawn with Compi rows, as Kit draws its popup menu: the
/// popover's color, edge, shadow and radius, as wide as its longest row (from 128 px
/// up to 320 px).
pub(super) fn menu_surface(cx: &App) -> Div {
    div()
        .popover_style(cx)
        .p_1()
        .flex()
        .flex_col()
        .gap_y_0p5()
        .min_w(px(128.0))
        .max_w(px(POPUP_MAX_WIDTH))
        .text_size(px(UI_SMALL_TEXT_SIZE))
}

/// A menu row as Kit draws one: 26 px, lit while `highlighted` or hovered, muted and
/// inert while `disabled`.
pub(super) fn menu_row(
    id: impl Into<ElementId>,
    highlighted: bool,
    disabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (accent, accent_text) = (theme.tokens.accent, theme.accent_foreground);
    div()
        .id(id)
        .min_h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .gap_x_1()
        .rounded(theme.radius.min(px(8.0)))
        .text_color(if disabled {
            theme.muted_foreground
        } else {
            theme.popover_foreground
        })
        .when(!disabled, |row| {
            row.hover(move |style| style.bg(accent).text_color(accent_text))
                .when(highlighted, |row| row.bg(accent).text_color(accent_text))
        })
}

/// A line between groups of menu rows, as Kit draws it.
pub(super) fn menu_separator_row(cx: &App) -> Div {
    div()
        .my_0p5()
        .mx_neg_1()
        .border_b(px(2.0))
        .border_color(cx.theme().border)
}

/// A group's heading in a menu, as Kit draws a label row.
pub(super) fn menu_label_row(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .min_h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .text_color(cx.theme().muted_foreground)
        .child(label.into())
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
                )
                .chain({
                    let surface = composite_rgb(colors.surface, opaque);
                    [surface, composite_rgb(colors.surface_hover, surface)]
                        .into_iter()
                        .flat_map(|background| {
                            [
                                (palette.popup_text, background, "popup"),
                                (palette.popup_muted, background, "popup detail"),
                            ]
                        })
                });
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
    fn typing_jumps_by_prefix_and_a_repeated_letter_cycles() {
        let labels: Vec<SharedString> =
            ["System", "JetBrains Mono", "IBM Plex", "Inter", "Iosevka"]
                .map(SharedString::from)
                .to_vec();
        assert_eq!(jump_target(&labels, 0, "i"), Some(2));
        assert_eq!(jump_target(&labels, 2, "i"), Some(3));
        assert_eq!(jump_target(&labels, 2, "ii"), Some(3));
        assert_eq!(jump_target(&labels, 4, "i"), Some(2));
        assert_eq!(jump_target(&labels, 2, "io"), Some(4));
        assert_eq!(jump_target(&labels, 2, "ib"), Some(2));
        assert_eq!(jump_target(&labels, 0, "x"), None);
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
            IconName::Check,
            IconName::Loader,
            IconName::Circle,
            IconName::LoaderCircle,
            IconName::CircleCheck,
            IconName::CircleAlert,
            IconName::CircleX,
            IconName::Palette,
            IconName::LayoutPanelLeft,
            IconName::SquareTerminal,
            IconName::Keyboard,
            IconName::Gauge,
            IconName::CircleArrowDown,
            IconName::SlidersHorizontal,
            IconName::Command,
            IconName::Gpu,
            IconName::Server,
            IconName::Clock,
            IconName::Bell,
            IconName::Search,
            IconName::Folder,
            IconName::File,
            IconName::Star,
            IconName::StarFill,
            IconName::ChevronLeft,
            IconName::ChevronRight,
            IconName::Ellipsis,
        ] {
            let path = icon.path();
            assert!(
                Assets.load(&path).ok().flatten().is_some(),
                "{path} is not bundled"
            );
        }
    }
}

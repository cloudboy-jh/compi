//! What's new for the running version: the title-bar dot, the sidebar card, and the
//! notes view shared with Settings → Updates. Seen state is process-global.
use super::*;
use crate::release_notes::{self, ReleaseNotes};

/// Keeps a long expanded card from pushing the sidebar footer off-screen.
const CARD_MAX_HEIGHT: f32 = 320.0;

impl CompiApp {
    pub(super) fn whats_new_dot(&self) -> bool {
        release_notes::current()
            .is_some_and(|notes| release_notes::seen().shows_dot(&notes.version))
    }

    pub(super) fn clear_whats_new_dot(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(notes) = release_notes::current()
            && release_notes::clear_dot(&notes.version)
        {
            self.broadcast_whats_new(window, cx);
        }
    }

    fn dismiss_whats_new(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(notes) = release_notes::current()
            && release_notes::dismiss(&notes.version)
        {
            self.broadcast_whats_new(window, cx);
        }
    }

    /// Every window renders from the shared seen state; repaint them all.
    fn broadcast_whats_new(&self, window: &Window, cx: &mut Context<Self>) {
        let current = Window::window_handle(window).window_id();
        for handle in cx.windows() {
            if handle.window_id() == current {
                continue;
            }
            if let Some(target) = handle.downcast::<CompiApp>() {
                let _ = target.update(cx, |_, _, target_cx| target_cx.notify());
            }
        }
        cx.notify();
    }

    pub(super) fn open_release_notes(&mut self, notes: &ReleaseNotes) {
        if let Err(error) = open_web_url(&notes.url()) {
            self.global_error = Some(error);
        }
    }

    pub(super) fn render_whats_new_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let notes = release_notes::current()?;
        if !release_notes::seen().shows_card(&notes.version) {
            return None;
        }
        let colors = *self.colors();
        let expanded = self.whats_new_expanded;
        let more = self.whats_new_button(
            "whats-new-more",
            if expanded { "Less" } else { "More" },
            cx.listener(|this, _, _, cx| {
                this.whats_new_expanded = !this.whats_new_expanded;
                cx.stop_propagation();
                cx.notify();
            }),
        );
        let link = self.whats_new_button(
            "whats-new-release-notes",
            "Release notes",
            cx.listener(move |this, _, _, cx| {
                this.open_release_notes(notes);
                cx.stop_propagation();
                cx.notify();
            }),
        );
        let close = div()
            .id("whats-new-dismiss")
            .size(px(24.0))
            .flex_none()
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .text_color(color(colors.muted))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| {
                this.dismiss_whats_new(window, cx);
                cx.stop_propagation();
            }))
            .child("×");
        Some(
            div()
                .mx_2()
                .mb_2()
                .max_h(px(CARD_MAX_HEIGHT))
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(blend_rgb(colors.surface, colors.foreground, 0.04)))
                .child(
                    div()
                        .flex_none()
                        .pl_3()
                        .pr_1()
                        .pt_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(format!("What's new · {}", notes.version)),
                        )
                        .child(close),
                )
                .child(
                    div()
                        .id("whats-new-body")
                        .flex_shrink()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_2()
                        .child(self.render_release_notes_view(notes, expanded, more, link)),
                )
                .into_any_element(),
        )
    }

    fn whats_new_button(
        &self,
        id: &'static str,
        label: &'static str,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(colors.foreground))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .child(label)
            .into_any_element()
    }

    /// Summary bullets, the More/Less toggle and release link, then the detail
    /// sections when expanded. The caller supplies the two controls.
    pub(super) fn render_release_notes_view(
        &self,
        notes: &ReleaseNotes,
        expanded: bool,
        more: AnyElement,
        link: AnyElement,
    ) -> AnyElement {
        let colors = *self.colors();
        let bullet = |text: &str| {
            div()
                .min_w_0()
                .flex()
                .gap_2()
                .child(div().flex_none().child("•"))
                .child(div().flex_1().min_w_0().child(text.to_owned()))
        };
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .children(notes.summary.iter().map(|line| bullet(line)))
            .child(
                div()
                    .pt_1()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(more)
                    .child(link),
            )
            .when(expanded, |view| {
                view.children(notes.details.iter().map(|section| {
                    div()
                        .pt_2()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child(section.title.to_uppercase()),
                        )
                        .children(section.bullets.iter().map(|line| bullet(line)))
                }))
            })
            .into_any_element()
    }
}

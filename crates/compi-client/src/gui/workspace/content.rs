//! Content panes: layout leaves that show files instead of a terminal. They tile,
//! float, zoom and move like terminal panes but have no surface, so they never
//! attach, resize a PTY, or receive terminal input. Phase 1 shows a placeholder
//! naming the folder and the environment it belongs to.
use super::*;

impl CompiApp {
    /// Keyboard focus follows the focused pane: a content pane owns `content_focus`,
    /// while terminals, overlays and the file tree use `focus_handle`, which carries
    /// their text input. Focus held by anything else is left alone.
    pub(super) fn reconcile_keyboard_focus(&mut self, window: &mut Window, cx: &mut App) {
        if !self.focus_handle.is_focused(window) && !self.content_focus.is_focused(window) {
            return;
        }
        let wanted = if self.overlay.is_none()
            && self.file_tree.is_none()
            && self.focused_content().is_some()
        {
            &self.content_focus
        } else {
            &self.focus_handle
        };
        if !wanted.is_focused(window) {
            window.focus(wanted, cx);
        }
    }

    /// One content pane at `rect`, sharing `render_pane`'s placement so tiled and
    /// floating presentations look alike.
    pub(super) fn render_content_pane(
        &self,
        id: impl Into<gpui::ElementId>,
        geometry: &layout::PaneLayout,
        rect: layout::Rect,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = *self.colors();
        let pane_id = geometry.pane_id.clone();
        let content = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find_map(|tab| match leaf_node(&tab.layout, &pane_id)? {
                    LayoutNode::Content { content, .. } => Some(content),
                    _ => None,
                })
        });
        // Only the focused content pane tracks the handle, so focus has one owner.
        let keyboard = self.focused_pane() == Some(&pane_id)
            && self.overlay.is_none()
            && self.file_tree.is_none();
        let menu_id = pane_id.clone();
        let focus_id = pane_id;
        div()
            .id(id)
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.width))
            .h(px(rect.height))
            .bg(material_color(
                colors.background,
                self.effective_background_opacity(),
            ))
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.focus_pane(menu_id.clone());
                    this.open_overlay(Overlay::Palette, "");
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .when(keyboard, |body| body.track_focus(&self.content_focus))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        // Compi shortcuts still run; every other key stops here, so
                        // nothing typed in a content pane reaches a shell.
                        this.handle_app_key(event, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.focus_pane(focus_id.clone());
                            window.focus(&this.content_focus, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .p_3()
                    .text_center()
                    .when_some(content, |body, content| {
                        body.child(
                            div()
                                .max_w_full()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(UI_BODY_TEXT_SIZE + 2.0))
                                .child(content.folder_name().to_owned()),
                        )
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(colors.muted))
                                .child(content.environment.to_string()),
                        )
                        .child(
                            div()
                                .max_w_full()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(color(colors.muted))
                                .child(content.root.clone()),
                        )
                    }),
            )
    }
}

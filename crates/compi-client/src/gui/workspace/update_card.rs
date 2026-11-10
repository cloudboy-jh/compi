//! The sidebar's update card: an available, downloading, ready or failed update in the
//! What's new card's place and look. Its actions go through Settings → Updates' path;
//! Later hides the card and the title-bar dot for that version.
use super::settings::unix_now;
use super::*;
use crate::release_notes;
use crate::updates::{CardAction, UpdateButton};

impl CompiApp {
    pub(super) fn render_update_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let card = self.update_card.as_ref()?;
        let colors = *self.colors();
        let actions = card.actions.iter().enumerate().map(|(index, &action)| {
            let label = match action {
                CardAction::Update(button) => button.label(card.ending),
                CardAction::Details => "Details".into(),
            };
            let control = controls::text_button(("update-card", index), label);
            let control = match action {
                CardAction::Update(UpdateButton::RestartEndShells) => control.danger(),
                CardAction::Update(UpdateButton::Cancel) | CardAction::Details => control,
                CardAction::Update(_) => control.primary(),
            };
            control
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.press_card_action(action, window, cx);
                }))
                .into_any_element()
        });
        let later = card.later.then(|| {
            let version = card.version.clone();
            controls::text_button("update-card-later", "Later")
                .ghost()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_later(&version, window, cx);
                }))
                .into_any_element()
        });
        let line = card.line.as_ref().map(|line| {
            div()
                .min_w_0()
                .text_color(color(modal_text_color(
                    if card.failed {
                        colors.error
                    } else {
                        colors.foreground
                    },
                    &colors,
                )))
                .child(self.notes_text(line))
        });
        let progress = card.progress.map(|fraction| {
            div()
                .h(px(4.0))
                .rounded_full()
                .bg(color(colors.border))
                .child(
                    div()
                        .h(px(4.0))
                        .rounded_full()
                        .bg(color(colors.accent))
                        .w(gpui::relative(fraction)),
                )
        });
        let body = div()
            .min_w_0()
            .pt_1()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .children(line)
            .children(progress)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(actions)
                    .children(later),
            )
            .into_any_element();
        Some(self.sidebar_card("update-card-body", card.title.clone(), None, body))
    }

    fn press_card_action(
        &mut self,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The state moved on since the card was drawn; the next frame shows it.
        let later = release_notes::seen().update_later;
        let offered = self
            .updates
            .snapshot()
            .card(unix_now(), later.as_deref(), |_| None)
            .is_some_and(|card| card.actions.contains(&action));
        if offered {
            match action {
                CardAction::Update(button) => self.press_update_button(button, window, cx),
                CardAction::Details => {
                    self.open_settings(Some(SettingsSection::Updates), None, window, cx)
                }
            }
        }
        cx.notify();
    }

    fn update_later(&mut self, version: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.update_card = None;
        self.update_dot = self.updates.shows_dot(Some(version));
        if release_notes::update_later(version) {
            self.broadcast_seen(window, cx);
        }
        cx.notify();
    }
}

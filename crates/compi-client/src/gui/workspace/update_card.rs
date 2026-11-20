//! The sidebar's update button: while a release is on offer, downloading, ready or
//! failed, a round accent button at the end of the sidebar's bottom row. Its tooltip
//! names the release and its state; a click does what the update card's main action
//! did, through Settings → Updates' path, and a right-click lists every action with
//! Later, which hides the button and the title-bar dot for that version.
use super::settings::unix_now;
use super::*;
use crate::gui::controls::{IconName, MenuEntry, menu_item};
use crate::release_notes;
use crate::updates::{CardAction, UpdateButton};

impl CompiApp {
    pub(super) fn render_update_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let card = self.update_card.as_ref()?;
        let colors = *self.colors();
        // Cancel would stop a download on a stray click; the button opens Settings →
        // Updates then, which shows the download and its Cancel.
        let action = card
            .actions
            .first()
            .copied()
            .filter(|action| *action != CardAction::Update(UpdateButton::Cancel))
            .unwrap_or(CardAction::Details);
        let detail = card
            .line
            .iter()
            .cloned()
            .chain([action_label(action, card.ending)])
            .collect::<Vec<_>>()
            .join("\n");
        let tint = if card.failed {
            colors.error
        } else {
            colors.accent
        };
        Some(
            div()
                .id("update-button")
                .flex_none()
                .size(px(24.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color(tint))
                .cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.press_card_action(action, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        this.open_update_menu(event.position, window, cx);
                        cx.stop_propagation();
                    }),
                )
                .tooltip(controls::tooltip(
                    card.title.clone(),
                    Some(detail.into()),
                    None,
                ))
                .child(controls::glyph(
                    IconName::CircleArrowDown,
                    color(colors.background),
                ))
                .into_any_element(),
        )
    }

    /// The update button's menu: every action the update offers, then Later.
    fn open_update_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = self.update_card.clone() else {
            return;
        };
        let mut entries: Vec<MenuEntry> = card
            .actions
            .iter()
            .map(|&action| {
                menu_item(
                    action_label(action, card.ending),
                    cx.listener(move |this, _, window, cx| {
                        this.press_card_action(action, window, cx);
                    }),
                )
                .into()
            })
            .collect();
        if card.later {
            let version = card.version.clone();
            entries.push(MenuEntry::Separator);
            entries.push(
                menu_item(
                    "Later",
                    cx.listener(move |this, _, window, cx| {
                        this.update_later(&version, window, cx);
                    }),
                )
                .into(),
            );
        }
        self.open_menu(position, Anchor::BottomLeft, entries, window, cx);
    }

    fn press_card_action(
        &mut self,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The state moved on since the button was drawn; the next frame shows it.
        let later = release_notes::seen().update_later;
        let offered = self
            .updates
            .snapshot()
            .card(unix_now(), later.as_deref(), |_| None)
            .is_some_and(|card| {
                card.actions.contains(&action)
                    || (action == CardAction::Details
                        && card
                            .actions
                            .contains(&CardAction::Update(UpdateButton::Cancel)))
            });
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

/// What `action` is called, `ending` the shells a restart ends.
fn action_label(action: CardAction, ending: usize) -> String {
    match action {
        CardAction::Update(button) => button.label(ending),
        CardAction::Details => "Details".into(),
    }
}

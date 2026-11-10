//! The transcript's scroll: GPUI's virtual `list`, following the newest entry while the
//! reader is at the bottom and staying put while they read further up, with a Latest
//! button back down. Kit's `MessageScroller` keeps its list to itself, so it cannot page;
//! this keeps what Compi used of it and pages.
use super::*;
use gpui::{Axis, FollowMode, ListAlignment, ListState, Role, list};
use gpui_component::scroll::{ScrollableElement as _, ScrollableMask};

/// Rows laid out past the viewport's edges, so scrolling does not pop them in.
const OVERDRAW: Pixels = px(400.0);

/// The transcript's rows as the list knows them, one per entry. Clones share the list.
#[derive(Clone)]
pub(super) struct TranscriptScroll {
    list: ListState,
}

impl TranscriptScroll {
    /// An empty transcript following its tail; a scroll redraws `app`, for Latest.
    pub(super) fn new(app: WeakEntity<CompiApp>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, OVERDRAW);
        list.set_follow_mode(FollowMode::Tail);
        // Deferred: the list is borrowed while it reports a scroll.
        list.set_scroll_handler(move |_, _, cx| {
            let app = app.clone();
            cx.defer(move |cx| {
                app.update(cx, |_, cx| cx.notify()).ok();
            });
        });
        Self { list }
    }

    /// Starts over with `count` rows, following the tail.
    pub(super) fn reset(&self, count: usize) {
        self.list.reset(count);
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// `count` rows added after the last.
    pub(super) fn append(&self, count: usize) {
        let end = self.list.item_count();
        self.list.splice(end..end, count);
    }

    /// Every row laid out again (the text size changed), keeping the place in proportion.
    pub(super) fn remeasure(&self) {
        self.list.remeasure();
    }

    /// Row `index` laid out again (it grew or folded), keeping the rows above in place.
    pub(super) fn remeasure_row(&self, index: usize) {
        if index < self.list.item_count() {
            self.list.remeasure_items(index..index + 1);
        }
    }

    /// Page Up (`down` false) or Page Down: a viewport less `overlap`, which stays in view.
    pub(super) fn page(&self, down: bool, overlap: Pixels) {
        let step = (self.list.viewport_bounds().size.height - overlap).max(overlap);
        self.list.scroll_by(if down { step } else { -step });
    }

    /// Back to the newest entry, following it again.
    pub(super) fn scroll_to_end(&self) {
        self.list.set_follow_mode(FollowMode::Tail);
        self.list.scroll_to_end();
    }

    /// The reader scrolled away from the newest entry.
    fn scrolled_up(&self) -> bool {
        self.list.max_offset_for_scrollbar().y > px(0.0)
            && !self.list.is_following_tail()
            && !self.list.is_scrolled_to_end().unwrap_or(false)
    }

    /// The list with its scrollbar, `row` drawing each row, and Latest while scrolled up.
    pub(super) fn render(
        &self,
        mut row: impl FnMut(usize) -> AnyElement + 'static,
        cx: &Context<CompiApp>,
    ) -> AnyElement {
        let rows = list(self.list.clone(), move |index, _, _| {
            div()
                .w_full()
                .min_w_0()
                .px_3()
                .child(row(index))
                .into_any_element()
        })
        .size_full()
        .min_h_0()
        .py_2();
        let scrolled_up = self.scrolled_up();
        let latest = self.clone();
        div()
            .id("chat-transcript")
            .relative()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .id("chat-transcript-viewport")
                    .role(Role::Log)
                    .size_full()
                    .min_h_0()
                    .min_w_0()
                    .child(rows)
                    .vertical_scrollbar(&self.list),
            )
            // Wheel scrolls the transcript, not a scroller around it, until it ends.
            .child(ScrollableMask::new(Axis::Vertical, &self.list).id("chat-transcript"))
            .when(scrolled_up, |transcript| {
                transcript.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(ui_rems(16.0))
                        .flex()
                        .justify_center()
                        .child(
                            text_button("chat-latest", "Latest")
                                .icon(IconName::ArrowDown)
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    latest.scroll_to_end();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

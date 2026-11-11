//! Dev servers in terminals. When a terminal this window draws printed a local
//! server's address (a dev server's `Local:   http://localhost:5173/`, for one) and
//! something answers there, a card at the bottom right of that pane offers it: the
//! address and **Open**, which shows the page in place of the shell as the terminal's
//! Browser view (see `browser.rs`). Ctrl/Cmd-click on a link to this machine opens the
//! same way. **Settings → Terminal → When a dev server starts** (`DevServer`) picks the
//! card, the page showing by itself, or neither.
//!
//! Addresses come from the window's own copy of each drawn terminal's screen and its
//! last `HISTORY_ROWS` history rows, read again once its output has changed. Whether
//! they answer is checked on a background thread, at once for new addresses and every
//! `RECHECK` while a pane has any; nothing runs for hidden tabs, for a terminal showing
//! a view, or during paint.
//!
//! A pane offers the newest address it printed that answers. One it opened or
//! dismissed is settled: it is not offered, nor shown by itself, again until its server
//! stops answering (and so has to start again) or a different address answers.

use super::home::{display_address, listening, local_link, local_links, snapshot_lines};
use super::*;
use crate::client_state::address_site;
use crate::config::DevServer;
use std::sync::mpsc;

/// History rows read with a pane's screen: a server's start-up lines, not its past.
const HISTORY_ROWS: usize = 200;
/// The most addresses of one pane that are checked, newest first.
const MAX_ADDRESSES: usize = 4;
/// How long after reading a pane's addresses changed output is read again.
const READ_INTERVAL: Duration = Duration::from_millis(500);
/// How often a pane's addresses are checked again.
const RECHECK: Duration = Duration::from_secs(3);
/// The card's width, as the pasted-image card's, and the least it shrinks to in a
/// narrow pane before it is left out.
const CARD_WIDTH: f32 = 240.0;
const MIN_CARD_WIDTH: f32 = 140.0;
/// The card's distance from its pane's right and bottom edges, and from the bottom
/// while the pane's new-lines button sits under it.
const CARD_INSET: f32 = 12.0;
const CARD_LIFTED_INSET: f32 = 36.0;

/// What a check of one pane's addresses found.
#[derive(Debug, PartialEq, Eq)]
struct Checked {
    /// The newest printed address that answers.
    answering: Option<String>,
    /// The settled address, when it no longer answers.
    stopped: Option<String>,
}

/// Checks a pane's `printed` addresses, newest first, and its `settled` one, asking
/// `answers` about each until one does.
fn check(printed: &[String], settled: Option<&str>, answers: impl Fn(&str) -> bool) -> Checked {
    let answering = printed.iter().find(|url| answers(url.as_str())).cloned();
    let stopped = settled
        .filter(|settled| answering.as_deref() != Some(*settled) && !answers(settled))
        .map(str::to_owned);
    Checked { answering, stopped }
}

/// What this window knows of one terminal pane's servers.
#[derive(Default)]
struct PaneServers {
    /// The local addresses on the pane's screen and in its recent history, newest
    /// first.
    printed: Vec<String>,
    /// The screen they were read from, and when.
    sequence: Option<u64>,
    read: Option<Instant>,
    /// When they were last checked; `None` checks them at once.
    checked: Option<Instant>,
    /// The newest of them that answered then.
    answering: Option<String>,
    /// The address opened from here or dismissed.
    settled: Option<String>,
}

impl PaneServers {
    /// Takes in a check. The settled address is forgotten once it stopped answering or
    /// a different address answers.
    fn take_in(&mut self, checked: Checked) {
        let another = checked.answering.is_some() && checked.answering != self.settled;
        if another || (checked.stopped.is_some() && checked.stopped == self.settled) {
            self.settled = None;
        }
        self.answering = checked.answering;
    }

    /// The address the pane offers: the one answering, unless it is settled.
    fn offer(&self) -> Option<&str> {
        self.answering
            .as_deref()
            .filter(|address| Some(*address) != self.settled.as_deref())
    }

    /// The offered address was opened or dismissed.
    fn settle(&mut self) {
        if let Some(address) = self.offer().map(str::to_owned) {
            self.settled = Some(address);
        }
    }

    /// Whether the pane offers the site `page` is at.
    fn offers_site(&self, page: &str) -> bool {
        self.offer()
            .is_some_and(|address| address_site(address).eq_ignore_ascii_case(address_site(page)))
    }

    /// The pane's shell is not drawn: nothing is offered behind a view or in a hidden
    /// tab, and its addresses are checked afresh once it shows again.
    fn hide(&mut self) {
        self.answering = None;
        self.checked = None;
    }

    /// Whether the pane's addresses should be checked now.
    fn due(&self, now: Instant) -> bool {
        (!self.printed.is_empty() || self.settled.is_some() || self.answering.is_some())
            && self
                .checked
                .is_none_or(|at| now.duration_since(at) >= RECHECK)
    }
}

/// A terminal pane this window draws.
struct Drawn {
    pane_id: PaneId,
    /// Its shell shows, not a view.
    shell: bool,
    /// The address of the page it shows in place of its shell.
    page: Option<String>,
    /// A page may show in its place by itself: it carries no view, or a page already.
    free: bool,
}

/// Client-only state of this window's dev-server cards.
#[derive(Default)]
pub(super) struct ServersUi {
    panes: HashMap<PaneId, PaneServers>,
    /// A check on its way from a background thread.
    pending: Option<mpsc::Receiver<Vec<(PaneId, Checked)>>>,
}

impl CompiApp {
    /// Takes in a finished check, reads the addresses of drawn terminals whose output
    /// changed and starts the next check that is due; with **Open automatically**,
    /// shows an offered address's page. Runs on the window's timer.
    pub(in crate::gui) fn poll_dev_servers(&mut self, cx: &mut Context<Self>) {
        let mode = self.config.dev_server;
        if mode == DevServer::Nothing {
            let servers = &mut self.browser.servers;
            servers.pending = None;
            if !servers.panes.is_empty() {
                servers.panes.clear();
                cx.notify();
            }
            return;
        }
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        let drawn: Vec<Drawn> = self
            .visible_layout()
            .into_iter()
            .flat_map(|layout| &layout.panes)
            .chain(self.float_layouts.iter().map(|float| &float.pane))
            .filter_map(|pane| {
                let leaf = content::find_leaf(workspace, &pane.pane_id)?;
                leaf.surface_id()?;
                let shown = leaf.shown_content();
                Some(Drawn {
                    pane_id: pane.pane_id.clone(),
                    shell: shown.is_none(),
                    page: shown
                        .and_then(PaneContent::browser)
                        .map(|browser| browser.url.clone()),
                    free: leaf
                        .view()
                        .is_none_or(|view| view.content.browser().is_some()),
                })
            })
            .collect();
        // What a pane settled stays with it while it is a terminal, drawn or not.
        self.browser.servers.panes.retain(|pane_id, _| {
            content::find_leaf(workspace, pane_id).is_some_and(|leaf| leaf.surface_id().is_some())
        });
        let mut changed = false;
        if let Some(receiver) = &self.browser.servers.pending {
            match receiver.try_recv() {
                Ok(found) => {
                    self.browser.servers.pending = None;
                    for (pane_id, checked) in found {
                        if let Some(pane) = self.browser.servers.panes.get_mut(&pane_id) {
                            let offered = pane.offer().map(str::to_owned);
                            pane.take_in(checked);
                            changed |= offered.as_deref() != pane.offer();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.browser.servers.pending = None,
            }
        }
        let now = Instant::now();
        for terminal in &drawn {
            if !terminal.shell {
                // Its page shows, opened from the card or by a click on the address:
                // the offer is taken.
                if let Some(page) = &terminal.page
                    && let Some(pane) = self.browser.servers.panes.get_mut(&terminal.pane_id)
                    && pane.offers_site(page)
                {
                    pane.settle();
                }
                continue;
            }
            let Some(snapshot) = self
                .surface_views
                .iter()
                .find(|view| view.pane_id == terminal.pane_id)
                .and_then(|view| view.mirror.snapshot())
            else {
                continue;
            };
            let pane = self
                .browser
                .servers
                .panes
                .entry(terminal.pane_id.clone())
                .or_default();
            if pane.sequence != Some(snapshot.sequence)
                && pane
                    .read
                    .is_none_or(|at| now.duration_since(at) >= READ_INTERVAL)
            {
                pane.sequence = Some(snapshot.sequence);
                pane.read = Some(now);
                let mut printed = local_links(&snapshot_lines(snapshot, HISTORY_ROWS));
                printed.truncate(MAX_ADDRESSES);
                if pane.printed != printed {
                    pane.printed = printed;
                    pane.checked = None;
                }
            }
        }
        for (pane_id, pane) in &mut self.browser.servers.panes {
            if !drawn
                .iter()
                .any(|terminal| terminal.shell && &terminal.pane_id == pane_id)
            {
                pane.hide();
            }
        }
        if self.browser.servers.pending.is_none() {
            let due: Vec<(PaneId, Vec<String>, Option<String>)> = drawn
                .iter()
                .filter(|terminal| terminal.shell)
                .filter_map(|terminal| {
                    let pane = self.browser.servers.panes.get_mut(&terminal.pane_id)?;
                    if !pane.due(now) {
                        return None;
                    }
                    pane.checked = Some(now);
                    Some((
                        terminal.pane_id.clone(),
                        pane.printed.clone(),
                        pane.settled.clone(),
                    ))
                })
                .collect();
            if !due.is_empty() {
                let (sender, receiver) = mpsc::channel();
                self.browser.servers.pending = Some(receiver);
                thread::spawn(move || {
                    let found: Vec<(PaneId, Checked)> = due
                        .into_iter()
                        .map(|(pane_id, printed, settled)| {
                            (pane_id, check(&printed, settled.as_deref(), listening))
                        })
                        .collect();
                    let _ = sender.send(found);
                });
            }
        }
        if mode == DevServer::Open && !self.mutation_pending {
            let opening = drawn
                .iter()
                .filter(|terminal| terminal.shell && terminal.free)
                .find_map(|terminal| {
                    let address = self.browser.servers.panes.get(&terminal.pane_id)?.offer()?;
                    Some((terminal.pane_id.clone(), address.to_owned()))
                });
            if let Some((pane_id, url)) = opening
                && self
                    .mutate_in(
                        WorkspaceMutation::ShowView {
                            pane_id: pane_id.clone(),
                            content: PaneContent::Browser(BrowserContent { url }),
                        },
                        false,
                        None,
                    )
                    .is_some()
                && let Some(pane) = self.browser.servers.panes.get_mut(&pane_id)
            {
                // Once for this start of the server: back at the shell, the page does
                // not show again by itself.
                pane.settle();
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Shows the page at `url` for terminal pane `pane_id`: in place of its shell, as
    /// its Browser view, or in a pane beside it when it carries a chat, which a view
    /// in its place would end. `false` when nothing was sent or asked.
    fn show_page_for(
        &mut self,
        pane_id: &PaneId,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let page = PaneContent::Browser(BrowserContent { url });
        let chat = self
            .workspace
            .as_ref()
            .and_then(|workspace| content::find_pane_content(workspace, pane_id))
            .is_some_and(|content| content.chat().is_some());
        if chat {
            self.open_content_beside(pane_id, page)
        } else {
            self.show_view(pane_id, page, window, cx)
        }
    }

    /// Ctrl/Cmd-click on web link `uri` in terminal pane `pane_id`: an address of this
    /// machine shows as a page for the pane (`show_page_for`). `false` for any other
    /// address, which is the system browser's.
    pub(in crate::gui) fn open_local_link(
        &mut self,
        pane_id: &PaneId,
        uri: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(url) = local_link(uri) else {
            return false;
        };
        self.show_page_for(pane_id, url, window, cx);
        true
    }

    /// The address terminal pane `pane_id`'s card offers.
    fn served_address(&self, pane_id: &PaneId) -> Option<String> {
        Some(self.browser.servers.panes.get(pane_id)?.offer()?.to_owned())
    }

    /// Pane `pane_id`'s card was opened or dismissed: it goes.
    fn settle_server_card(&mut self, pane_id: &PaneId) {
        if let Some(pane) = self.browser.servers.panes.get_mut(pane_id) {
            pane.settle();
        }
    }

    /// Open on pane `pane_id`'s card: the page shows for the pane (`show_page_for`),
    /// which takes focus, so Toggle view goes back to its shell.
    fn open_served_page(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = self.served_address(pane_id) else {
            return;
        };
        // Replacing a Files view may ask about unsaved edits first; the card then
        // stays until the page shows.
        let asks = self
            .workspace
            .as_ref()
            .and_then(|workspace| content::find_pane_content(workspace, pane_id))
            .is_some_and(|content| content.folder().is_some());
        self.focus_pane(pane_id.clone());
        if self.show_page_for(pane_id, url, window, cx) && !asks {
            self.settle_server_card(pane_id);
        }
        cx.notify();
    }

    /// Open in pane beside: the page opens in a Browser pane split beside the terminal.
    fn open_served_page_beside(&mut self, pane_id: &PaneId) {
        let Some(url) = self.served_address(pane_id) else {
            return;
        };
        if self.open_content_beside(pane_id, PaneContent::Browser(BrowserContent { url })) {
            self.settle_server_card(pane_id);
        }
    }

    /// Right-click on a pane's card: open the page in place or in a pane beside the
    /// terminal, copy the address, or dismiss the card.
    fn open_server_card_menu(
        &mut self,
        pane_id: &PaneId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(address) = self.served_address(pane_id) else {
            return;
        };
        let (open, beside, dismiss) = (pane_id.clone(), pane_id.clone(), pane_id.clone());
        let entries = vec![
            controls::menu_item(
                "Open",
                cx.listener(move |this, _, window, cx| this.open_served_page(&open, window, cx)),
            )
            .into(),
            controls::menu_item(
                "Open in pane beside",
                cx.listener(move |this, _, _, cx| {
                    this.open_served_page_beside(&beside);
                    cx.notify();
                }),
            )
            .into(),
            controls::MenuEntry::Separator,
            controls::menu_item("Copy address", move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(address.clone()));
            })
            .into(),
            controls::MenuEntry::Separator,
            controls::menu_item(
                "Dismiss",
                cx.listener(move |this, _, _, cx| {
                    this.settle_server_card(&dismiss);
                    cx.notify();
                }),
            )
            .into(),
        ];
        self.open_menu(position, gpui::Anchor::TopLeft, entries, window, cx);
    }

    /// Terminal pane `pane_id`'s card, when it offers an address: the address without
    /// its scheme, **Open** and a dismiss ×, looking like the pasted-image card, at the
    /// bottom right of the `pane_width` wide pane; `lifted` above its new-lines button.
    pub(in crate::gui) fn render_server_card(
        &self,
        pane_id: &PaneId,
        pane_width: f32,
        lifted: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let address = self.browser.servers.panes.get(pane_id)?.offer()?;
        let width = CARD_WIDTH.min(pane_width - 2.0 * CARD_INSET);
        if self.config.dev_server == DevServer::Nothing || width < MIN_CARD_WIDTH {
            return None;
        }
        let colors = *self.colors();
        let id = |name: &str| SharedString::from(format!("server-card-{name}-{pane_id}"));
        let (open, open_button, menu, dismiss) = (
            pane_id.clone(),
            pane_id.clone(),
            pane_id.clone(),
            pane_id.clone(),
        );
        Some(
            div()
                .id(id("card"))
                .absolute()
                .right(px(CARD_INSET))
                .bottom(px(if lifted {
                    CARD_LIFTED_INSET
                } else {
                    CARD_INSET
                }))
                .w(px(width))
                .p_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.open_served_page(&open, window, cx)
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_server_card_menu(&menu, event.position, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    gpui_component::Icon::new(IconName::Server)
                        .size(px(14.0))
                        .text_color(color(colors.muted)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(display_address(address).to_owned()),
                )
                .child(
                    controls::text_button(id("open"), "Open").on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.open_served_page(&open_button, window, cx)
                        },
                    )),
                )
                .child(
                    div()
                        .id(id("dismiss"))
                        .px_1()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settle_server_card(&dismiss);
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .child(chrome_icon(ChromeIcon::Close, color(colors.muted))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VITE: &str = "http://localhost:5173";
    const API: &str = "http://localhost:3000";

    /// One check of `pane` with `printed` on its screen, newest first, while the
    /// addresses in `up` answer.
    fn checked(pane: &mut PaneServers, printed: &[&str], up: &[&str]) {
        let printed: Vec<String> = printed.iter().map(|url| (*url).to_owned()).collect();
        let found = check(&printed, pane.settled.as_deref(), |url| up.contains(&url));
        pane.take_in(found);
    }

    #[test]
    fn a_pane_offers_the_newest_address_it_printed_that_answers() {
        let lines: Vec<String> = "  ➜  Local:   http://localhost:5173/\n\
             \x20 ➜  Network: http://192.168.1.20:5173/\n\
             API listening on http://0.0.0.0:3000\n\
             docs at https://vitejs.dev/guide and http://127.0.0.1:8000/guide"
            .lines()
            .map(str::to_owned)
            .collect();
        let printed = local_links(&lines);
        let answering = |up: &[&str]| check(&printed, None, |url| up.contains(&url)).answering;
        // The docs server printed last is not running; the API server is the newest
        // that is.
        assert_eq!(answering(&[VITE, API]).as_deref(), Some(API));
        assert_eq!(answering(&[VITE]).as_deref(), Some(VITE));
        assert_eq!(
            answering(&[VITE, API, "http://127.0.0.1:8000/guide"]).as_deref(),
            Some("http://127.0.0.1:8000/guide")
        );
        // Nothing answers, or only addresses of other machines do: no card.
        assert_eq!(answering(&[]), None);
        assert_eq!(
            answering(&["http://192.168.1.20:5173", "https://vitejs.dev/guide"]),
            None
        );
    }

    #[test]
    fn a_dismissed_address_stays_dismissed_until_another_answers_or_its_server_restarts() {
        let mut pane = PaneServers::default();
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(pane.offer(), Some(VITE));
        pane.settle();
        // Still running: printed again, scrolled out of the rows read, or under a
        // newer address nothing answers at yet.
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(pane.offer(), None);
        checked(&mut pane, &[], &[VITE]);
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(pane.offer(), None);
        checked(&mut pane, &[API, VITE], &[VITE]);
        assert_eq!(pane.offer(), None);
        // The newer address answers: the card is for it.
        checked(&mut pane, &[API, VITE], &[VITE, API]);
        assert_eq!(pane.offer(), Some(API));
        pane.settle();
        checked(&mut pane, &[API, VITE], &[VITE, API]);
        assert_eq!(pane.offer(), None);

        // A server that stopped and started again is offered again.
        let mut pane = PaneServers::default();
        checked(&mut pane, &[VITE], &[VITE]);
        pane.settle();
        checked(&mut pane, &[VITE], &[]);
        assert_eq!(pane.offer(), None);
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(pane.offer(), Some(VITE));
    }

    #[test]
    fn a_server_opens_by_itself_once_per_start() {
        // What Open automatically does after each check: show the offered page.
        fn opened(pane: &mut PaneServers) -> Option<String> {
            let address = pane.offer().map(str::to_owned);
            pane.settle();
            address
        }
        let mut pane = PaneServers::default();
        // Printed before anything listens there: nothing to show yet.
        checked(&mut pane, &[VITE], &[]);
        assert_eq!(opened(&mut pane), None);
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(opened(&mut pane).as_deref(), Some(VITE));
        // Back at the shell with the server still running: not again.
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(opened(&mut pane), None);
        checked(&mut pane, &[], &[VITE]);
        assert_eq!(opened(&mut pane), None);
        // Stopped, then started again: once more.
        checked(&mut pane, &[VITE], &[]);
        assert_eq!(opened(&mut pane), None);
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(opened(&mut pane).as_deref(), Some(VITE));
        checked(&mut pane, &[VITE], &[VITE]);
        assert_eq!(opened(&mut pane), None);
    }
}

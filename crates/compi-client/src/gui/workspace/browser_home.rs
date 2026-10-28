//! A blank Browser pane's home page, drawn by Compi in place of the web view's empty
//! page: Compi's eye mark in the theme's accent, the local servers this workspace's
//! terminals printed addresses for (**Running here**, the pane's own tab first) and the
//! window's recent addresses.
//! Its page hides while it shows; a link, or Enter in the address field, loads the page,
//! which appears once the load settles, and Back from there returns here.
//!
//! Servers are found away from paint: a background thread reads each running
//! terminal's screen and history from the daemon, keeps the loopback `http` and `https`
//! addresses it printed, newest first, and drops those nothing answers at. It runs when
//! a home page appears and again every few seconds while one has the keyboard.

use super::*;
use crate::client_state::{MAX_RECENT_ADDRESSES, address_site};
use compi_protocol::{ScreenSnapshot, SurfaceInfo};
use gpui::relative;
use std::borrow::Cow;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;

/// Whether `node` holds pane `pane_id`.
fn holds(node: &LayoutNode, pane_id: &PaneId) -> bool {
    match node {
        LayoutNode::Split { first, second, .. } => holds(first, pane_id) || holds(second, pane_id),
        leaf => leaf.leaf_pane() == Some(pane_id),
    }
}

/// The eye mark's size, drawn in the theme's accent like the header's.
const MARK_SIZE: f32 = 64.0;
/// How often a home page with the keyboard looks for servers again.
const RESCAN: Duration = Duration::from_secs(5);
/// How long a server has to accept a connection to count as running.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
/// The most addresses checked, and listed, under Running here.
const MAX_PROBED: usize = 24;
const MAX_LINKS: usize = 6;

/// A local server's address and the name of the tab whose terminal printed it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HomeLink {
    url: String,
    tab: String,
}

/// This window's home pages: what Running here lists and when it was last looked for.
#[derive(Default)]
pub(super) struct HomeUi {
    links: Vec<HomeLink>,
    pending: Option<mpsc::Receiver<Vec<HomeLink>>>,
    scanned: Option<Instant>,
    /// The tab whose servers come first, while a home page is drawn: the focused home
    /// page's, else the first one's.
    here: Option<TabId>,
    /// A home page has the keyboard.
    focused: bool,
}

impl HomeUi {
    /// Notes which home pages were drawn this frame. One appearing looks for servers at
    /// once; none left forgets them.
    pub(super) fn follow(&mut self, here: Option<TabId>, focused: bool) {
        if here.is_none() {
            self.links.clear();
            self.scanned = None;
        } else if self.here.is_none() {
            self.scanned = None;
        }
        self.here = here;
        self.focused = focused;
    }
}

/// Each logical line of `snapshot`, history first: soft-wrapped rows joined.
fn snapshot_lines(snapshot: &ScreenSnapshot) -> Vec<String> {
    let history = snapshot
        .scrollback
        .iter()
        .map(|row| (Cow::Borrowed(row.text.as_str()), row.wrapped));
    let screen = snapshot.cells.iter().map(|row| {
        let text: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
        (Cow::Owned(text), row.wrapped)
    });
    join_wrapped(history.chain(screen))
}

/// Rows, each with whether it wraps onto the next, as logical lines.
fn join_wrapped<'a>(rows: impl Iterator<Item = (Cow<'a, str>, bool)>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for (text, wrapped) in rows {
        line.push_str(&text);
        if !wrapped {
            lines.push(std::mem::take(&mut line));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The loopback addresses `lines` mention, newest (last) first, each once.
fn local_links(lines: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in lines.iter().rev() {
        for (at, _) in line.match_indices("://") {
            let start = if line
                .get(at.saturating_sub(5)..at)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
            {
                at - 5
            } else if line
                .get(at.saturating_sub(4)..at)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http"))
            {
                at - 4
            } else {
                continue;
            };
            let token = line[start..]
                .split(char::is_whitespace)
                .next()
                .unwrap_or_default();
            if let Some(url) = local_link(token)
                && !found.contains(&url)
            {
                found.push(url);
            }
        }
    }
    found
}

/// `token` as a link a Browser pane can open, when it is an `http` or `https` address
/// of this machine: `localhost` or a name under it, `127.x.x.x` or `[::1]`. A server
/// listening on every interface (`0.0.0.0`, `[::]`) is opened at `localhost`; a bare
/// `/` path is dropped, so one server reads the same however it was printed.
fn local_link(token: &str) -> Option<String> {
    let url = BrowserContent::from_typed(token).ok()?.url;
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let authority = authority.rsplit('@').next()?;
    let (host, port) = split_port(authority);
    let host = host.to_ascii_lowercase();
    let host = match host.as_str() {
        "0.0.0.0" | "[::]" => "localhost",
        "localhost" | "[::1]" => host.as_str(),
        name if name.ends_with(".localhost") => name,
        address if address.starts_with("127.") && address.parse::<std::net::Ipv4Addr>().is_ok() => {
            address
        }
        _ => return None,
    };
    let path = if path == "/" { "" } else { path };
    let port = port.map(|port| format!(":{port}")).unwrap_or_default();
    Some(format!("{scheme}://{host}{port}{path}"))
}

/// An authority's host and port: `localhost:5173`, `[::1]:8080`, `example.com`.
fn split_port(authority: &str) -> (&str, Option<&str>) {
    let colon = if authority.starts_with('[') {
        authority
            .find(']')
            .and_then(|close| authority[close..].find(':').map(|colon| close + colon))
    } else {
        authority.rfind(':')
    };
    match colon {
        Some(colon) => (&authority[..colon], Some(&authority[colon + 1..])),
        None => (authority, None),
    }
}

/// Whether something accepts connections where `url` points, as the page would reach it.
fn listening(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = split_port(authority);
    let port = match port {
        Some(port) => port.parse::<u16>().ok(),
        None if scheme == "https" => Some(443),
        None => Some(80),
    };
    let Some(port) = port else {
        return false;
    };
    // A name under `localhost` is this machine to the web view, not to the resolver.
    let host = if host.ends_with(".localhost") {
        "localhost"
    } else {
        host.trim_start_matches('[').trim_end_matches(']')
    };
    (host, port).to_socket_addrs().is_ok_and(|mut addresses| {
        addresses.any(|address| TcpStream::connect_timeout(&address, PROBE_TIMEOUT).is_ok())
    })
}

/// An address as a row shows it: without its scheme, or the trailing `/` of a site.
fn display_address(url: &str) -> &str {
    let shown = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    match shown.strip_suffix('/') {
        Some(site) if !site.contains('/') => site,
        _ => shown,
    }
}

impl CompiApp {
    /// Takes in servers found, then looks again when a home page has just appeared or,
    /// while one has the keyboard, every few seconds. Runs on the window's timer.
    pub(in crate::gui) fn poll_browser_home(&mut self, cx: &mut Context<Self>) {
        let home = &mut self.browser.home;
        if let Some(receiver) = &home.pending {
            match receiver.try_recv() {
                Ok(links) => {
                    home.pending = None;
                    if home.here.is_some() && home.links != links {
                        home.links = links;
                        cx.notify();
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => home.pending = None,
            }
        }
        let Some(here) = home.here.clone() else {
            return;
        };
        let due = home
            .scanned
            .is_none_or(|at| home.focused && at.elapsed() >= RESCAN);
        if !due || self.connection_error.is_some() {
            return;
        }
        home.scanned = Some(Instant::now());
        let surfaces = self.home_surfaces(&here);
        if surfaces.is_empty() {
            if !self.browser.home.links.is_empty() {
                self.browser.home.links.clear();
                cx.notify();
            }
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.browser.home.pending = Some(receiver);
        let target = self.target.clone();
        thread::spawn(move || {
            let mut links: Vec<HomeLink> = Vec::new();
            if let Ok(mut client) = target.connect_existing() {
                for (surface, tab) in surfaces {
                    let Ok(snapshot) = client.inspect_surface(&surface, true) else {
                        continue;
                    };
                    for url in local_links(&snapshot_lines(&snapshot)) {
                        if !links.iter().any(|link| link.url == url) {
                            links.push(HomeLink {
                                url,
                                tab: tab.clone(),
                            });
                        }
                    }
                }
            }
            links.truncate(MAX_PROBED);
            links.retain(|link| listening(&link.url));
            links.truncate(MAX_LINKS);
            let _ = sender.send(links);
        });
    }

    /// The tab holding pane `pane_id`.
    pub(super) fn browser_tab(&self, pane_id: &PaneId) -> Option<TabId> {
        self.workspace
            .as_ref()?
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| holds(&tab.layout, pane_id))
            .map(|tab| tab.id.clone())
    }

    /// The running terminals of `here`'s workspace with where they are, `here`'s first
    /// ("This tab"), then the other tabs in order, hidden ones included, by name.
    fn home_surfaces(&self, here: &TabId) -> Vec<(SurfaceInfo, String)> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let Some(session) = workspace
            .sessions
            .iter()
            .find(|session| session.tabs.iter().any(|tab| &tab.id == here))
        else {
            return Vec::new();
        };
        let tabs = session
            .tabs
            .iter()
            .filter(|tab| &tab.id == here)
            .chain(session.tabs.iter().filter(|tab| &tab.id != here));
        let mut surfaces = Vec::new();
        for tab in tabs {
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            let label = if &tab.id == here {
                "This tab".to_owned()
            } else if !tab.label.is_empty() {
                tab.label.clone()
            } else {
                self.metadata_tab_caption(tab, &self.tab_panes(tab)).0
            };
            for surface_id in leaves.iter().filter_map(|(_, surface)| surface.as_ref()) {
                if let Some(surface) = workspace.surface(surface_id)
                    && surface.status == SurfaceStatus::Running
                {
                    surfaces.push((surface.clone(), label.clone()));
                }
            }
        }
        surfaces
    }

    /// Browser pane `pane_id`'s home page.
    pub(super) fn render_browser_home(&self, pane_id: &PaneId, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let running = &self.browser.home.links;
        let recent: Vec<&String> = self
            .state
            .recent_addresses
            .iter()
            .filter(|url| {
                !running
                    .iter()
                    .any(|link| address_site(&link.url).eq_ignore_ascii_case(address_site(url)))
            })
            .take(MAX_RECENT_ADDRESSES)
            .collect();
        let section = |title: &'static str| {
            div()
                .w_full()
                .px_2()
                .pb_1()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(title)
        };
        let row = |index: usize, icon: IconName, url: &str, detail: Option<&str>| {
            let (pane, target) = (pane_id.clone(), url.to_owned());
            div()
                .id(SharedString::from(format!(
                    "browser-home-{index}-{pane_id}"
                )))
                .w_full()
                .h(px(30.0))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(controls::CONTROL_RADIUS))
                .cursor_pointer()
                .hover(|style| style.bg(color(colors.surface_hover)))
                .child(
                    gpui_component::Icon::new(icon)
                        .size(px(14.0))
                        .text_color(color(colors.muted)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .map(|address| match detail {
                            // The address keeps most of the row; the tab gets the rest.
                            Some(_) => address.flex_shrink_0().max_w(relative(0.7)),
                            None => address.flex_1(),
                        })
                        .child(display_address(url).to_owned()),
                )
                .when_some(detail, |row, detail| {
                    row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_right()
                            .truncate()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(detail.to_owned()),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.focus_pane(pane.clone());
                    this.browser_go(&pane, target.clone(), window, cx);
                }))
        };
        let mut lists = div().w_full().flex().flex_col().gap_4();
        if !running.is_empty() {
            lists = lists.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(section("Running here"))
                    .children(running.iter().enumerate().map(|(index, link)| {
                        row(index, IconName::Server, &link.url, Some(&link.tab))
                    })),
            );
        }
        if !recent.is_empty() {
            lists = lists.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(section("Recent"))
                    .children(recent.iter().enumerate().map(|(index, url)| {
                        row(running.len() + index, IconName::Clock, url, None)
                    })),
            );
        }
        let empty = running.is_empty() && recent.is_empty();
        div()
            .id(SharedString::from(format!("browser-home-{pane_id}")))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .min_h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(480.0))
                            .px_6()
                            .py_8()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_4()
                            .child(
                                canvas(
                                    |_, _, _| (),
                                    move |bounds, _, window, _| {
                                        brand::paint(bounds, color(colors.accent), window)
                                    },
                                )
                                .size(px(MARK_SIZE)),
                            )
                            .child(div().text_size(px(20.0)).child("Where to?"))
                            .when(empty, |column| {
                                column.child(
                                    div()
                                        .text_center()
                                        .text_size(px(UI_SMALL_TEXT_SIZE))
                                        .text_color(color(colors.muted))
                                        .child(
                                            "Type an address above. Servers your terminals \
                                             start show up here.",
                                        ),
                                )
                            })
                            .when(!empty, |column| column.child(lists)),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn local_links_are_loopback_addresses_newest_first_each_once() {
        let output = lines(
            "  VITE v5.4.0  ready in 312 ms\n\
             \n\
             \x20 ➜  Local:   http://localhost:5173/\n\
             \x20 ➜  Network: http://192.168.1.20:5173/\n\
             docs at https://vitejs.dev/guide and http://127.0.0.1:8000/api?x=1.\n\
             Listening on http://0.0.0.0:3000 (and HTTP://[::]:3001)\n\
             - Local:        http://localhost:5173\n\
             auth via http://user@app.localhost:4000/callback, then <http://[::1]:9229/json>",
        );
        assert_eq!(
            local_links(&output),
            [
                "http://app.localhost:4000/callback",
                "http://[::1]:9229/json",
                "http://localhost:5173",
                "http://localhost:3000",
                "http://localhost:3001",
                "http://127.0.0.1:8000/api?x=1",
            ]
        );
    }

    #[test]
    fn local_links_ignore_other_hosts_schemes_and_partial_words() {
        let output = lines(
            "https://example.com:443/ ftp://localhost/ file:///tmp/a.html\n\
             http://127.0.0.1.example.com/ http://localhost.example.com/ http://\n\
             é://localhost ahttps://x",
        );
        assert!(local_links(&output).is_empty());
    }

    #[test]
    fn wrapped_rows_join_into_one_line_before_links_are_read() {
        let rows = [
            ("Local: http://local", true),
            ("host:5173/", false),
            ("next", false),
            ("tail", true),
        ];
        let lines = join_wrapped(
            rows.into_iter()
                .map(|(text, wrapped)| (Cow::Borrowed(text), wrapped)),
        );
        assert_eq!(lines, ["Local: http://localhost:5173/", "next", "tail"]);
        assert_eq!(local_links(&lines), ["http://localhost:5173"]);
    }

    #[test]
    fn a_listening_port_counts_and_a_closed_one_does_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listening(&format!("http://127.0.0.1:{port}/app")));
        drop(listener);
        assert!(!listening(&format!("http://127.0.0.1:{port}")));
    }

    #[test]
    fn rows_show_addresses_without_scheme_or_a_bare_trailing_slash() {
        assert_eq!(display_address("http://localhost:5173/"), "localhost:5173");
        assert_eq!(
            display_address("https://example.com/docs/"),
            "example.com/docs/"
        );
        assert_eq!(display_address("file:///tmp/a.html"), "file:///tmp/a.html");
    }
}

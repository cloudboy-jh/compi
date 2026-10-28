//! Browser panes. Each Browser content pane shown in a window gets a page: the system
//! webview (WebView2 on Windows, WKWebView on macOS) as a child of the window, drawn
//! above GPUI. GPUI draws the pane's toolbar (back, forward, reload or stop, the
//! address field) and leaves the rest of the body to the page, which follows the body's
//! bounds every frame it is painted, through splits, floats and landing animations.
//!
//! Since the page covers whatever GPUI draws there, it hides while the palette, a
//! menu, a dialog, the image inspector or a pane drag is open, while its pane is not
//! drawn and while a failed load shows its notice. Float frames, the window's error
//! banner and Compi's tooltips above a page are cut out of it (a window region on
//! Windows); elsewhere the page hides while a float or the banner overlaps it, and
//! tooltips stay under it.
//!
//! The URL lives in the daemon (`PaneContent::Browser`) and follows the page's finished
//! loads; history, cookies and scroll position are the webview's, with one persistent
//! web profile per instance under its data directory. The address field shows where the
//! page is going while it loads and where it is otherwise; Enter goes to what was typed
//! (`BrowserContent::from_typed`), Escape puts the address back. A blank page shows
//! Compi's home page instead (`browser_home.rs`), and focus arriving at it puts the
//! keyboard in its address. On Windows, clicking
//! into a page focuses its pane, Ctrl+L and F6 select the address, Compi's own chords
//! (Ctrl+Shift or Alt with a key, except Alt+Left/Right, the page's back and forward,
//! and Ctrl+Tab/T/W/Comma) are forwarded to the window while the page has the keyboard,
//! the keyboard returns to GPUI when another pane takes focus, and a failed load shows
//! why with Retry. On macOS the window's first responder says whether a page has the
//! keyboard: a click into a page focuses its pane, and the keyboard returns to GPUI's
//! view when another pane, the address field or an overlay takes focus, and before a
//! page holding it hides or closes (AppKit would leave it with nothing, and every key
//! would beep).
use super::*;
use crate::gui::controls::IconName;
use compi_protocol::{BrowserContent, PaneContent};
#[cfg(windows)]
use gpui::Keystroke;
use gpui::{Bounds, Entity, FocusHandle, Subscription, Task, canvas};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebView, WebViewBuilder};

#[path = "browser_media.rs"]
mod media;
pub(in crate::gui) use media::MediaSource;
#[path = "browser_home.rs"]
mod home;

/// The toolbar above the page.
const TOOLBAR_HEIGHT: f32 = 34.0;
/// The bar along the toolbar's foot while a page loads.
const LOADING_BAR_HEIGHT: f32 = 2.0;
/// Where to get the WebView2 Runtime when it is missing.
const WEBVIEW2_RUNTIME_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Window-coordinate frames of the floats drawn this frame, by stacking index (later
/// floats are above earlier ones).
type FloatFrames = Rc<RefCell<Vec<(usize, Bounds<Pixels>)>>>;

/// Each window's float frames and pages, for what GPUI draws above them from outside
/// the workspace view (tooltips). Weak, so a closed window keeps no page alive.
#[derive(Default)]
struct PageLayers(HashMap<gpui::WindowId, PageLayer>);

impl gpui::Global for PageLayers {}

struct PageLayer {
    floats: FloatFrames,
    pages: Vec<Weak<PageView>>,
}

/// A frame drawn above every page: records itself where it is painted, as float
/// `index`, then re-clips `pages`, which are painted before it.
fn cut_out(floats: FloatFrames, pages: Vec<Rc<PageView>>, index: usize) -> gpui::Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            floats.borrow_mut().push((index, bounds));
            let floats = floats.borrow();
            for page in &pages {
                if !page.hidden.get() {
                    page.clip(&floats, window.scale_factor(), false);
                }
            }
        },
    )
}

/// Cuts a tooltip's frame out of its window's pages: Kit's popup reaches `inset` past
/// the element this is placed in (absolutely, in a relative parent). Elsewhere than
/// Windows a cut-out hides the whole page, so tooltips stay under it there.
pub(in crate::gui) fn above_pages(
    inset: (f32, f32),
    window: &Window,
    cx: &App,
) -> Option<AnyElement> {
    if !cfg!(windows) {
        return None;
    }
    let layer = cx
        .try_global::<PageLayers>()?
        .0
        .get(&window.window_handle().window_id())?;
    let pages: Vec<Rc<PageView>> = layer.pages.iter().filter_map(Weak::upgrade).collect();
    if pages.is_empty() {
        return None;
    }
    let (x, y) = inset;
    Some(
        cut_out(layer.floats.clone(), pages, usize::MAX)
            .absolute()
            .left(px(-x))
            .right(px(-x))
            .top(px(-y))
            .bottom(px(-y))
            .into_any_element(),
    )
}

/// A pane's tab-menu title when its page has none: the page's host (with its port), a
/// `file:` URL's file name, or "Browser" for a blank page.
pub(super) fn pane_title(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "Browser".into();
    };
    let location = rest.split(['?', '#']).next().unwrap_or_default();
    let title = if scheme.eq_ignore_ascii_case("file") {
        location.rsplit('/').find(|part| !part.is_empty())
    } else {
        location.split('/').next().filter(|host| !host.is_empty())
    };
    title.map_or_else(|| "Browser".into(), str::to_owned)
}

/// Whether the page may go to `url`: what a Browser pane may hold.
fn navigable(url: &str) -> bool {
    BrowserContent { url: url.into() }.validate().is_ok()
}

/// What the address field shows for `address`: nothing for a blank page.
fn field_text(address: &str) -> &str {
    if address.eq_ignore_ascii_case(BrowserContent::BLANK) {
        ""
    } else {
        address
    }
}

/// What a page's callbacks report, shared with the window.
#[derive(Default)]
struct Signals {
    /// Where the page is, from its finished loads.
    url: RefCell<String>,
    /// Where a load in progress is going.
    loading: RefCell<Option<String>>,
    /// The page's document title.
    title: RefCell<String>,
    /// Why the last load failed; cleared when another starts.
    failure: RefCell<Option<String>>,
    /// The page has the keyboard.
    native_focus: Cell<bool>,
    /// Compi stopped the load in progress: its ending is not a failure.
    stopping: Cell<bool>,
}

/// What the paint pass needs of a page.
struct PageView {
    webview: WebView,
    /// wry's container window, which a clip region applies to.
    #[cfg(windows)]
    container: windows::Win32::Foundation::HWND,
    /// Logical window bounds last given to the webview.
    placed: Cell<Option<Bounds<Pixels>>>,
    shown: Cell<bool>,
    /// Hidden this frame: the pane is not drawn, an overlay is open or a load failed.
    hidden: Cell<bool>,
    /// The page's float stacking index, when its pane floats.
    float_index: Cell<Option<usize>>,
    /// Float frames last cut out of the page.
    clipped: RefCell<Vec<Bounds<Pixels>>>,
    signals: Rc<Signals>,
    /// Whether the page has somewhere to go back or forward to, as of its last event.
    can_back: Cell<bool>,
    can_forward: Cell<bool>,
}

impl PageView {
    fn place(&self, bounds: Bounds<Pixels>, floats: &[(usize, Bounds<Pixels>)], scale: f32) {
        if self.hidden.get() {
            return;
        }
        if self.placed.get() != Some(bounds) {
            let rect = wry::Rect {
                position: LogicalPosition::new(
                    f64::from(f32::from(bounds.origin.x)),
                    f64::from(f32::from(bounds.origin.y)),
                )
                .into(),
                size: LogicalSize::new(
                    f64::from(f32::from(bounds.size.width).max(1.0)),
                    f64::from(f32::from(bounds.size.height).max(1.0)),
                )
                .into(),
            };
            // A failed move leaves the page where it was until the next frame.
            let _ = self.webview.set_bounds(rect);
            self.placed.set(Some(bounds));
            self.clipped.borrow_mut().clear();
            self.clip(floats, scale, true);
        } else {
            self.clip(floats, scale, false);
        }
    }

    /// The frames of floats above this page that overlap it.
    fn floats_above(&self, floats: &[(usize, Bounds<Pixels>)]) -> Vec<Bounds<Pixels>> {
        let Some(bounds) = self.placed.get() else {
            return Vec::new();
        };
        let own = self.float_index.get();
        floats
            .iter()
            .filter(|(index, _)| own.is_none_or(|own| *index > own))
            .map(|(_, frame)| *frame)
            .filter(|frame| frame.intersects(&bounds))
            .collect()
    }

    /// Cuts the frames of floats above the page out of it, then shows it.
    #[cfg(windows)]
    fn clip(&self, floats: &[(usize, Bounds<Pixels>)], scale: f32, force: bool) {
        use windows::Win32::Graphics::Gdi::{
            CombineRgn, CreateRectRgn, DeleteObject, RGN_DIFF, SetWindowRgn,
        };
        let Some(bounds) = self.placed.get() else {
            return;
        };
        let above = self.floats_above(floats);
        if force || *self.clipped.borrow() != above {
            let physical = |value: Pixels| (f32::from(value) * scale).round() as i32;
            unsafe {
                let region = CreateRectRgn(
                    0,
                    0,
                    physical(bounds.size.width),
                    physical(bounds.size.height),
                );
                for frame in &above {
                    let x = physical(frame.origin.x - bounds.origin.x);
                    let y = physical(frame.origin.y - bounds.origin.y);
                    let hole = CreateRectRgn(
                        x,
                        y,
                        x + physical(frame.size.width),
                        y + physical(frame.size.height),
                    );
                    CombineRgn(Some(region), Some(region), Some(hole), RGN_DIFF);
                    let _ = DeleteObject(hole.into());
                }
                // The window owns the region from here on.
                SetWindowRgn(self.container, Some(region), true);
            }
            *self.clipped.borrow_mut() = above;
        }
        self.show(true);
    }

    /// Without window regions, the page hides while a float above overlaps it.
    #[cfg(not(windows))]
    fn clip(&self, floats: &[(usize, Bounds<Pixels>)], _: f32, _: bool) {
        let above = self.floats_above(floats);
        self.show(above.is_empty());
        *self.clipped.borrow_mut() = above;
    }

    fn show(&self, visible: bool) {
        if !visible {
            // A hidden view keeps no keyboard; AppKit would leave it with nothing.
            self.release_keyboard();
        }
        if self.shown.get() != visible {
            let _ = self.webview.set_visible(visible);
            self.shown.set(visible);
        }
    }

    /// Hands the keyboard back to GPUI's view when the page has it.
    fn release_keyboard(&self) {
        #[cfg(target_os = "macos")]
        let held = macos_page::holds_keyboard(&self.webview);
        #[cfg(not(target_os = "macos"))]
        let held = self.signals.native_focus.get();
        if held {
            let _ = self.webview.focus_parent();
        }
        self.signals.native_focus.set(false);
    }

    /// Follows who has the keyboard; true when the page just took it. On macOS a click
    /// into the page takes it unseen by GPUI, and the window's first responder is the
    /// only record of it; on Windows WebView2's focus hooks keep `native_focus` and
    /// report a click (`PageEvent::Focused`).
    fn sync_keyboard(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            let holds = macos_page::holds_keyboard(&self.webview);
            let took = holds && !self.signals.native_focus.get();
            self.signals.native_focus.set(holds);
            took
        }
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Where the address field shows the page: where a load is going, else where it is.
    fn address(&self) -> String {
        let loading = self.signals.loading.borrow();
        loading
            .clone()
            .unwrap_or_else(|| self.signals.url.borrow().clone())
    }

    /// The page is blank and loaded nothing that failed: its pane shows the home page.
    /// A load from there keeps it until the load settles.
    fn at_home(&self) -> bool {
        self.signals
            .url
            .borrow()
            .eq_ignore_ascii_case(BrowserContent::BLANK)
            && self.signals.failure.borrow().is_none()
    }

    /// The page's title, unless it has none, shows the home page, or a failure notice
    /// covers WebView2's error page, whose title it would be.
    fn title(&self) -> Option<String> {
        if self.signals.failure.borrow().is_some() || self.at_home() {
            return None;
        }
        let title = self.signals.title.borrow().trim().to_owned();
        (!title.is_empty()).then_some(title)
    }

    fn refresh_history(&self) {
        self.can_back
            .set(self.webview.can_go_back().unwrap_or(false));
        self.can_forward
            .set(self.webview.can_go_forward().unwrap_or(false));
    }

    /// Stops the load in progress.
    fn stop(&self) {
        self.signals.stopping.set(true);
        #[cfg(windows)]
        {
            use wry::WebViewExtWindows;
            let _ = unsafe { self.webview.webview().Stop() };
        }
        #[cfg(not(windows))]
        let _ = self.webview.evaluate_script("window.stop()");
    }
}

impl Drop for PageView {
    fn drop(&mut self) {
        // The page leaves the window; it must not take the keyboard with it.
        self.release_keyboard();
    }
}

/// A page and how its URL stands with the daemon's.
struct Page {
    view: Rc<PageView>,
    /// What the daemon holds for the pane.
    persisted: String,
    /// A URL sent to the daemon and not yet seen back.
    sent: Option<String>,
    /// The address last remembered as recent from this page.
    recorded: String,
    /// The pane showed its home page at the last draw.
    home: bool,
}

/// A Browser pane's GPUI side in this window: its keyboard focus and address field.
struct BrowserPane {
    focus: FocusHandle,
    address: Entity<InputState>,
    _address_events: Subscription,
    /// The address last put in the field, which follows the page while not edited.
    shown: String,
    /// Why the typed address was not opened.
    address_error: Option<String>,
}

/// Why a pane has no page.
struct PageFailure {
    message: String,
    /// The WebView2 Runtime is not installed.
    runtime_missing: bool,
}

/// What a page reports back. Keyboard and focus come from WebView2's own hooks, which
/// only Windows has so far.
enum PageEvent {
    /// The page took the keyboard.
    #[cfg(windows)]
    Focused(PaneId),
    /// A Compi chord pressed while the page had the keyboard.
    #[cfg(windows)]
    Key(Keystroke),
    /// Ctrl+L or F6 pressed while the page had the keyboard.
    #[cfg(windows)]
    FocusAddress(PaneId),
    /// The page asked to open `url` in a new window; it opens in the page instead.
    Open(PaneId, String),
    /// A load started, finished or failed, or the title changed.
    Changed(PaneId),
    /// A mouse button went down in the window: it may have put the keyboard in a page.
    #[cfg(target_os = "macos")]
    Clicked,
}

/// Toolbar actions, from its buttons and the palette.
#[derive(Clone, Copy)]
pub(in crate::gui) enum Navigation {
    Back,
    Forward,
    /// Reload, or stop the load in progress.
    Reload,
}

/// Client-only state of this window's Browser panes.
#[derive(Default)]
pub(in crate::gui) struct BrowserUi {
    pages: HashMap<PaneId, Page>,
    panes: HashMap<PaneId, BrowserPane>,
    /// Pages being created, and why a pane's page could not be.
    creating: HashSet<PaneId>,
    failed: HashMap<PaneId, PageFailure>,
    /// The instance's web profile, shared by its pages.
    context: Option<Rc<RefCell<WebContext>>>,
    events: Option<async_channel::Sender<PageEvent>>,
    _listener: Option<Task<()>>,
    floats: FloatFrames,
    /// The pane focused at the last draw, to give a page the keyboard when focus
    /// arrives at its pane.
    focused: Option<PaneId>,
    /// Reports clicks, which may put the keyboard in a page without GPUI seeing it.
    #[cfg(target_os = "macos")]
    click_monitor: Option<macos_page::ClickMonitor>,
    /// Pages of PDF, audio and video files open in file tabs.
    media: media::MediaPages,
    /// Blank panes' home pages.
    home: home::HomeUi,
}

/// Every Browser pane of `tab`, with the URL the daemon holds.
fn browser_leaves(node: &LayoutNode, output: &mut Vec<(PaneId, String)>) {
    match node {
        LayoutNode::Content {
            pane_id,
            content: PaneContent::Browser(browser),
        } => output.push((pane_id.clone(), browser.url.clone())),
        LayoutNode::Split { first, second, .. } => {
            browser_leaves(first, output);
            browser_leaves(second, output);
        }
        LayoutNode::Content { .. } | LayoutNode::Pane { .. } => {}
    }
}

/// The window a page is created in, by its raw handle: a page is created outside any
/// update of the window, where GPUI's `Window` cannot be borrowed.
struct ParentWindow(RawWindowHandle);

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // The task creating pages belongs to the window, which outlives it.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

/// Whether the WebView2 Runtime is missing, which is why a page could not start.
#[cfg(windows)]
fn runtime_missing() -> bool {
    use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows::core::{PCWSTR, PWSTR};
    let mut version = PWSTR::null();
    let found =
        unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) };
    found.is_err() || version.is_null()
}

#[cfg(not(windows))]
fn runtime_missing() -> bool {
    false
}

/// A page for `pane_id` at `url` in `parent`, hidden until its pane is painted.
fn build_page(
    parent: RawWindowHandle,
    context: &RefCell<WebContext>,
    pane_id: &PaneId,
    url: &str,
    events: &async_channel::Sender<PageEvent>,
) -> Result<PageView, PageFailure> {
    let signals = Rc::new(Signals {
        url: RefCell::new(url.to_owned()),
        ..Signals::default()
    });
    let (loads, titles, opens) = (events.clone(), events.clone(), events.clone());
    let (load_signals, title_signals) = (signals.clone(), signals.clone());
    let (load_pane, title_pane, open_pane) = (pane_id.clone(), pane_id.clone(), pane_id.clone());
    let mut context = context.borrow_mut();
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(url)
        .with_visible(false)
        .with_focused(false)
        .with_bounds(wry::Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(1.0, 1.0).into(),
        })
        .with_navigation_handler(|url| navigable(&url))
        .with_new_window_req_handler(move |url, _| {
            if navigable(&url) {
                let _ = opens.try_send(PageEvent::Open(open_pane.clone(), url));
            }
            NewWindowResponse::Deny
        })
        .with_on_page_load_handler(move |event, url| {
            match event {
                PageLoadEvent::Started => {
                    *load_signals.loading.borrow_mut() = Some(url);
                    // Content arrived: an earlier failure no longer stands.
                    *load_signals.failure.borrow_mut() = None;
                }
                PageLoadEvent::Finished => {
                    *load_signals.url.borrow_mut() = url;
                    *load_signals.loading.borrow_mut() = None;
                }
            }
            let _ = loads.try_send(PageEvent::Changed(load_pane.clone()));
        })
        .with_document_title_changed_handler(move |title| {
            *title_signals.title.borrow_mut() = title;
            let _ = titles.try_send(PageEvent::Changed(title_pane.clone()));
        })
        .build_as_child(&ParentWindow(parent))
        .map_err(|error| PageFailure {
            runtime_missing: runtime_missing(),
            message: format!("The system web view could not start: {error}"),
        })?;
    #[cfg(windows)]
    let container = windows_page::watch(&webview, pane_id, &signals, events.clone());
    Ok(PageView {
        webview,
        #[cfg(windows)]
        container,
        placed: Cell::new(None),
        shown: Cell::new(false),
        hidden: Cell::new(false),
        float_index: Cell::new(None),
        clipped: RefCell::new(Vec::new()),
        signals,
        can_back: Cell::new(false),
        can_forward: Cell::new(false),
    })
}

impl CompiApp {
    /// Opens a Browser pane at `url` beside the focused pane, or in a tab of its own
    /// when there is none. A blank one opens with the keyboard in its address.
    pub(super) fn open_browser(&mut self, url: String) {
        let content = PaneContent::Browser(BrowserContent { url });
        let beside = self.focused_pane().cloned().or_else(|| {
            let mut leaves = Vec::new();
            collect_leaves(&self.selected_tab()?.layout, &mut leaves);
            leaves.into_iter().next().map(|(pane, _)| pane)
        });
        if let Some(beside) = beside {
            self.open_content_beside(&beside, content);
            return;
        }
        let Some(session_id) = self
            .state
            .selected_session
            .clone()
            .or_else(|| Some(self.workspace.as_ref()?.sessions.first()?.id.clone()))
        else {
            self.global_error = Some("Open a workspace first".into());
            return;
        };
        self.mutate(
            WorkspaceMutation::CreateContentTab {
                session_id,
                label: String::new(),
                content,
            },
            true,
        );
    }

    /// The focused Browser pane's page goes back, forward, reloads or stops.
    pub(super) fn browser_navigate(&mut self, navigation: Navigation) {
        let Some(page) = self
            .focused_pane()
            .and_then(|pane_id| self.browser.pages.get(pane_id))
        else {
            return;
        };
        let view = &page.view;
        let result = match navigation {
            Navigation::Back => view.webview.go_back(),
            Navigation::Forward => view.webview.go_forward(),
            Navigation::Reload if view.signals.loading.borrow().is_some() => {
                view.stop();
                Ok(())
            }
            Navigation::Reload if view.signals.failure.borrow().is_some() => {
                let url = view.signals.url.borrow().clone();
                view.webview.load_url(&url)
            }
            Navigation::Reload => view.webview.reload(),
        };
        if let Err(error) = result {
            self.global_error = Some(format!("The page could not do that: {error}"));
        }
    }

    /// Gives the keyboard to pane `pane_id`'s address field, its text selected.
    pub(super) fn focus_address(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        let Some(pane) = self.browser.panes.get(pane_id) else {
            return;
        };
        if let Some(page) = self.browser.pages.get(pane_id) {
            page.view.release_keyboard();
        }
        let handle = pane.address.focus_handle(cx);
        window.focus(&handle, cx);
        pane.address
            .update(cx, |address, cx| address.select_all(window, cx));
    }

    /// Goes where the address field says, or says why not.
    fn submit_address(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.browser.panes.get_mut(pane_id) else {
            return;
        };
        let typed = pane.address.read(cx).value().to_string();
        match BrowserContent::from_typed(&typed) {
            Ok(content) => self.browser_go(pane_id, content.url, window, cx),
            Err(error) => {
                pane.address_error = Some(error);
                cx.notify();
            }
        }
    }

    /// Browser pane `pane_id` goes to `url`, a URL it may hold, and the keyboard goes to
    /// its page; from the home page, once the page appears.
    pub(super) fn browser_go(
        &mut self,
        pane_id: &PaneId,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.browser.panes.get_mut(pane_id) else {
            return;
        };
        pane.address_error = None;
        let focus = pane.focus.clone();
        match self.browser.pages.get(pane_id) {
            Some(page) => {
                if let Err(error) = page.view.webview.load_url(&url) {
                    pane.address_error = Some(error.to_string());
                    cx.notify();
                    return;
                }
                if !page.view.at_home() {
                    let _ = page.view.webview.focus();
                }
            }
            // Not created yet: the daemon's URL is where it starts.
            None => self.mutate(
                WorkspaceMutation::UpdateContent {
                    pane_id: pane_id.clone(),
                    content: PaneContent::Browser(BrowserContent { url }),
                },
                false,
            ),
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Escape in the address field: the page's address comes back and the page takes
    /// the keyboard.
    fn revert_address(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.browser.panes.get_mut(pane_id) else {
            return;
        };
        let address = self
            .browser
            .pages
            .get(pane_id)
            .map(|page| page.view.address())
            .unwrap_or_else(|| pane.shown.clone());
        pane.address.update(cx, |field, cx| {
            field.set_value(field_text(&address).to_owned(), window, cx)
        });
        pane.shown = address;
        pane.address_error = None;
        window.focus(&pane.focus, cx);
        if let Some(page) = self.browser.pages.get(pane_id)
            && !page.view.at_home()
        {
            let _ = page.view.webview.focus();
        }
        cx.notify();
    }

    /// The tab menu's and a float's title for a Browser pane at `url`.
    pub(super) fn browser_title(&self, pane_id: &PaneId, url: &str) -> String {
        self.browser
            .pages
            .get(pane_id)
            .and_then(|page| page.view.title())
            .unwrap_or_else(|| pane_title(url))
    }

    /// What Browser pane `pane_id`'s page is doing, for `window list`; `url` is the
    /// daemon's. `None` while the page is being created.
    pub(super) fn browser_page_info(
        &self,
        pane_id: &PaneId,
        url: &str,
    ) -> Option<crate::cli_window::PageInfo> {
        use crate::cli_window::PageInfo;
        let Some(page) = self.browser.pages.get(pane_id) else {
            return self.browser.failed.get(pane_id).map(|failure| PageInfo {
                url: url.to_owned(),
                error: Some(failure.message.clone()),
                ..PageInfo::default()
            });
        };
        let signals = &page.view.signals;
        Some(PageInfo {
            url: signals.url.borrow().clone(),
            title: page.view.title(),
            loading: signals.loading.borrow().clone(),
            error: signals.failure.borrow().clone(),
        })
    }

    /// Whether `focused` belongs to a Browser pane: the pane or its address field.
    pub(super) fn browser_owns(&self, focused: &FocusHandle, cx: &App) -> bool {
        self.browser
            .panes
            .values()
            .any(|pane| pane.focus == *focused || pane.address.focus_handle(cx) == *focused)
    }

    /// Browser pane `pane_id`'s focus, which holds the address field.
    pub(super) fn browser_pane_focus(&self, pane_id: &PaneId) -> Option<&FocusHandle> {
        self.browser.panes.get(pane_id).map(|pane| &pane.focus)
    }

    /// What should have the keyboard in Browser pane `pane_id`: its pane focus, or
    /// `None` when the pane or its address field already has it. A field focused in the
    /// frame that created it is not yet inside the pane's focus.
    pub(super) fn browser_keyboard(
        &self,
        pane_id: &PaneId,
        window: &Window,
        cx: &App,
    ) -> Option<FocusHandle> {
        let pane = self.browser.panes.get(pane_id)?;
        let held = pane.focus.contains_focused(window, cx)
            || pane.address.focus_handle(cx).is_focused(window);
        (!held).then(|| pane.focus.clone())
    }

    /// Before drawing: drop pages whose pane left this window, create pages for new
    /// panes once this frame is done, follow URL changes both ways, keep each address
    /// field on its page's address while it is not edited, hide pages that are not
    /// drawn, are under an overlay, failed to load or show the home page, remember
    /// where pages settle as recent, and move the keyboard between GPUI and pages.
    pub(super) fn prepare_browsers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        macos_page::reclaim_lost_keyboard(window);
        self.browser.floats.borrow_mut().clear();
        let overlay = self.overlay.is_some()
            || self.popup.is_some()
            || self.context_menu.is_some()
            || self.tab_menu.is_some()
            || self.pane_drag.is_some()
            || self.image_inspector.is_some();
        self.prepare_media(overlay, window, cx);
        let mut wanted = Vec::new();
        if let Some(workspace) = &self.workspace {
            for session in &workspace.sessions {
                for tab in self.state.visible_tabs(session) {
                    browser_leaves(&tab.layout, &mut wanted);
                }
            }
        }
        let kept = |pane: &PaneId| wanted.iter().any(|(wanted, _)| wanted == pane);
        self.browser.pages.retain(|pane, _| kept(pane));
        self.browser.panes.retain(|pane, _| kept(pane));
        self.browser.failed.retain(|pane, _| kept(pane));
        let layers = &mut cx.default_global::<PageLayers>().0;
        layers.retain(|_, layer| layer.pages.iter().any(|page| page.strong_count() > 0));
        let id = Window::window_handle(window).window_id();
        if self.browser.pages.is_empty() && self.browser.media.views().next().is_none() {
            layers.remove(&id);
        } else {
            let pages = self.browser.pages.values().map(|page| &page.view);
            layers.insert(
                id,
                PageLayer {
                    floats: self.browser.floats.clone(),
                    pages: pages
                        .chain(self.browser.media.views())
                        .map(Rc::downgrade)
                        .collect(),
                },
            );
        }
        if wanted.is_empty() {
            self.browser.focused = None;
            self.browser.home.follow(None, false);
            return;
        }
        for (pane_id, url) in &wanted {
            if !self.browser.panes.contains_key(pane_id) {
                let pane = self.new_browser_pane(pane_id, url, window, cx);
                self.browser.panes.insert(pane_id.clone(), pane);
            }
        }
        let focused = self.focused_pane().cloned();
        let arrived = focused != self.browser.focused;
        self.browser.focused = focused.clone();
        // Focus arriving at a blank page goes to its address, to type or paste one.
        if arrived
            && !overlay
            && let Some(pane_id) = &focused
            && let Some((_, daemon)) = wanted.iter().find(|(wanted, _)| wanted == pane_id)
        {
            let address = self
                .browser
                .pages
                .get(pane_id)
                .map_or_else(|| daemon.clone(), |page| page.view.address());
            if address.eq_ignore_ascii_case(BrowserContent::BLANK) {
                self.focus_address(pane_id, window, cx);
            }
        }
        let mutation_pending = self.mutation_pending;
        let mut update = None;
        let mut missing = Vec::new();
        let mut load_failed = None;
        let mut clicked = None;
        let mut remembered = Vec::new();
        let mut entered_home = None;
        // The home page whose tab's servers come first: the focused one, else the first.
        let mut home_here: Option<&PaneId> = None;
        let mut home_focused = false;
        for (pane_id, daemon) in &wanted {
            let own = focused.as_ref() == Some(pane_id);
            let float_index = self
                .float_layouts
                .iter()
                .position(|float| &float.pane.pane_id == pane_id);
            let drawn = float_index.is_some()
                || self
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout.pane(pane_id).is_some());
            let Some(page) = self.browser.pages.get_mut(pane_id) else {
                if !self.browser.creating.contains(pane_id)
                    && !self.browser.failed.contains_key(pane_id)
                {
                    missing.push((pane_id.clone(), daemon.clone()));
                }
                if drawn
                    && daemon.eq_ignore_ascii_case(BrowserContent::BLANK)
                    && !self.browser.failed.contains_key(pane_id)
                    && (home_here.is_none() || own)
                {
                    home_here = Some(pane_id);
                    home_focused |= own;
                }
                continue;
            };
            let current = page.view.signals.url.borrow().clone();
            if page.sent.as_ref() == Some(daemon) {
                page.sent = None;
                page.persisted = daemon.clone();
            } else if (page.sent.is_some() && !mutation_pending)
                || (page.sent.is_none() && *daemon != page.persisted)
            {
                // The change did not apply, or another client or the CLI moved the
                // pane: go where the daemon says.
                page.sent = None;
                page.persisted = daemon.clone();
                if current != *daemon
                    && let Err(error) = page.view.webview.load_url(daemon)
                {
                    load_failed = Some(format!("The page could not load {daemon}: {error}"));
                }
            } else if current != page.persisted
                && page.sent.is_none()
                && !mutation_pending
                && update.is_none()
                && navigable(&current)
                // Mid-load, the page is still at its old address; the new one counts
                // once the load settles.
                && page.view.signals.loading.borrow().is_none()
            {
                page.sent = Some(current.clone());
                update = Some((pane_id.clone(), current.clone()));
            }
            // Where a page settles is the window's most recent address.
            if current != page.recorded
                && page.view.signals.loading.borrow().is_none()
                && page.view.signals.failure.borrow().is_none()
                && !current.eq_ignore_ascii_case(BrowserContent::BLANK)
                && navigable(&current)
            {
                page.recorded = current.clone();
                remembered.push(current);
            }
            // The field follows the page unless it is being edited.
            let editing = self
                .browser
                .panes
                .get(pane_id)
                .is_some_and(|pane| pane.address.focus_handle(cx).is_focused(window));
            if let Some(pane) = self.browser.panes.get_mut(pane_id) {
                let address = page.view.address();
                if !editing && pane.shown != address {
                    pane.address.update(cx, |field, cx| {
                        field.set_value(field_text(&address).to_owned(), window, cx)
                    });
                    pane.shown = address;
                    pane.address_error = None;
                }
            }
            page.view.float_index.set(float_index);
            let failed = page.view.signals.failure.borrow().is_some();
            let home = page.view.at_home();
            let left_home = page.home && !home;
            if home && !page.home && own && !overlay && !editing {
                // Back to the home page: the keyboard goes to the address.
                entered_home = Some(pane_id.clone());
            }
            page.home = home;
            if home && drawn && (home_here.is_none() || own) {
                home_here = Some(pane_id);
                home_focused |= own;
            }
            let hidden = !drawn || overlay || failed || home;
            let took = page.view.sync_keyboard();
            page.view.hidden.set(hidden);
            if hidden {
                page.view.show(false);
            }
            if took && !hidden && !arrived && (!own || editing) {
                // A click put the keyboard in the page: its pane takes focus.
                clicked = Some(pane_id.clone());
            } else if page.view.signals.native_focus.get() && (!own || editing) {
                page.view.release_keyboard();
            } else if own
                && (arrived || left_home)
                && !hidden
                && !editing
                && !page.view.signals.native_focus.get()
            {
                let _ = page.view.webview.focus();
            }
        }
        let here = home_here.and_then(|pane_id| self.browser_tab(pane_id));
        self.browser.home.follow(here, home_focused);
        if !remembered.is_empty() {
            for url in &remembered {
                self.state.remember_address(url);
            }
            self.save_state();
        }
        if let Some(pane_id) = entered_home {
            self.focus_address(&pane_id, window, cx);
        }
        if let Some(pane_id) = clicked {
            self.focus_pane(pane_id.clone());
            self.browser.focused = Some(pane_id.clone());
            if let Some(pane) = self.browser.panes.get(&pane_id) {
                window.focus(&pane.focus, cx);
            }
        }
        if let Some(error) = load_failed {
            self.global_error = Some(error);
        }
        if let Some((pane_id, url)) = update {
            self.mutate(
                WorkspaceMutation::UpdateContent {
                    pane_id,
                    content: PaneContent::Browser(BrowserContent { url }),
                },
                false,
            );
        }
        if !missing.is_empty() {
            self.create_pages(missing, window, cx);
        }
    }

    fn new_browser_pane(
        &self,
        pane_id: &PaneId,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserPane {
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type an address")
                .default_value(if url == BrowserContent::BLANK {
                    ""
                } else {
                    url
                })
        });
        let pane = pane_id.clone();
        let events = cx.subscribe_in(
            &address,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_address(&pane, window, cx),
                InputEvent::Change => {
                    if let Some(pane) = this.browser.panes.get_mut(&pane)
                        && pane.address_error.take().is_some()
                    {
                        cx.notify();
                    }
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        BrowserPane {
            focus: cx.focus_handle(),
            address,
            _address_events: events,
            shown: url.to_owned(),
            address_error: None,
        }
    }

    fn page_events(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> async_channel::Sender<PageEvent> {
        if let Some(events) = &self.browser.events {
            return events.clone();
        }
        let (sender, receiver) = async_channel::unbounded::<PageEvent>();
        #[cfg(target_os = "macos")]
        {
            self.browser.click_monitor = macos_page::ClickMonitor::new(window, sender.clone());
        }
        self.browser._listener = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let alive = match event {
                    // A chord reaches CompiApp's own handlers, so it is dispatched
                    // outside any update of it.
                    #[cfg(windows)]
                    PageEvent::Key(keystroke) => cx
                        .update(|window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        })
                        .is_ok(),
                    #[cfg(windows)]
                    PageEvent::Focused(pane_id) => this
                        .update(cx, |this, cx| {
                            if this.focused_pane() != Some(&pane_id) {
                                this.focus_pane(pane_id.clone());
                                this.browser.focused = Some(pane_id);
                            }
                            cx.notify();
                        })
                        .is_ok(),
                    #[cfg(windows)]
                    PageEvent::FocusAddress(pane_id) => this
                        .update_in(cx, |this, window, cx| {
                            this.focus_pane(pane_id.clone());
                            this.browser.focused = Some(pane_id.clone());
                            this.focus_address(&pane_id, window, cx);
                            cx.notify();
                        })
                        .is_ok(),
                    PageEvent::Open(pane_id, url) => this
                        .update(cx, |this, cx| {
                            if let Some(page) = this.browser.pages.get(&pane_id)
                                && let Err(error) = page.view.webview.load_url(&url)
                            {
                                this.global_error =
                                    Some(format!("The page could not load {url}: {error}"));
                            }
                            cx.notify();
                        })
                        .is_ok(),
                    PageEvent::Changed(pane_id) => this
                        .update(cx, |this, cx| {
                            if let Some(page) = this.browser.pages.get(&pane_id) {
                                page.view.refresh_history();
                            }
                            cx.notify();
                        })
                        .is_ok(),
                    // The next frame looks at who has the keyboard.
                    #[cfg(target_os = "macos")]
                    PageEvent::Clicked => this.update(cx, |_, cx| cx.notify()).is_ok(),
                };
                if !alive {
                    break;
                }
            }
        }));
        self.browser.events = Some(sender.clone());
        sender
    }

    /// Creates pages for `missing` panes. Creating a webview runs the platform's message
    /// loop until it is ready, and GPUI runs its other work from that loop, so it happens
    /// in a task of its own, outside any update of the window, against the window's raw
    /// handle.
    fn create_pages(
        &mut self,
        missing: Vec<(PaneId, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(parent) = HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        else {
            return;
        };
        self.browser
            .creating
            .extend(missing.iter().map(|(pane, _)| pane.clone()));
        let events = self.page_events(window, cx);
        let context = self.web_context();
        cx.spawn_in(window, async move |this, cx| {
            for (pane_id, url) in missing {
                // The window, and so `parent`, may have closed since.
                if this.update(cx, |_, _| ()).is_err() {
                    break;
                }
                let built = build_page(parent, &context, &pane_id, &url, &events);
                let stored = this.update(cx, |this, cx| {
                    this.browser.creating.remove(&pane_id);
                    match built {
                        Ok(view) => {
                            let home = view.at_home();
                            this.browser.pages.insert(
                                pane_id,
                                Page {
                                    view: Rc::new(view),
                                    persisted: url,
                                    sent: None,
                                    recorded: String::new(),
                                    home,
                                },
                            );
                        }
                        Err(failure) => {
                            this.browser.failed.insert(pane_id, failure);
                        }
                    }
                    cx.notify();
                });
                if stored.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The instance's web profile, made on first use.
    fn web_context(&mut self) -> Rc<RefCell<WebContext>> {
        self.browser
            .context
            .get_or_insert_with(|| {
                Rc::new(RefCell::new(WebContext::new(
                    compi_protocol::paths::data_dir()
                        .ok()
                        .map(|directory| directory.join("webview")),
                )))
            })
            .clone()
    }

    /// A Browser pane's body: the toolbar, then the page, the home page for a blank one,
    /// or why there is none. `url` is the daemon's.
    pub(super) fn render_browser_body(
        &self,
        pane_id: &PaneId,
        url: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.browser.panes.get(pane_id) else {
            return div().flex_1().into_any_element();
        };
        let colors = *self.colors();
        let page = self.browser.pages.get(pane_id).map(|page| &page.view);
        let body = match (page, self.browser.failed.get(pane_id)) {
            (Some(view), _)
                if view.signals.failure.borrow().is_some()
                    && view.signals.loading.borrow().is_some() =>
            {
                // Over the failed page, until the new one has content.
                let opening = view.address();
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .text_color(color(colors.muted))
                    .child(format!("Opening {opening}…"))
                    .into_any_element()
            }
            (Some(view), _) if view.signals.failure.borrow().is_some() => {
                let reason = view.signals.failure.borrow().clone().unwrap_or_default();
                let url = view.signals.url.borrow().clone();
                let retry = pane_id.clone();
                self.render_browser_notice(
                    format!("Couldn't open {url}"),
                    reason,
                    controls::text_button(
                        SharedString::from(format!("browser-retry-{pane_id}")),
                        "Retry",
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.focus_pane(retry.clone());
                        this.browser_navigate(Navigation::Reload);
                        if let Some(pane) = this.browser.panes.get(&retry) {
                            window.focus(&pane.focus, cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                )
            }
            (Some(view), _) if view.at_home() => self.render_browser_home(pane_id, cx),
            (Some(view), _) => {
                let view = view.clone();
                let floats = self.browser.floats.clone();
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        view.place(bounds, &floats.borrow(), window.scale_factor())
                    },
                )
                .flex_1()
                .w_full()
                .into_any_element()
            }
            (None, Some(failure)) => self.render_browser_notice(
                if failure.runtime_missing {
                    "The WebView2 Runtime is not installed".into()
                } else {
                    "The page could not start".into()
                },
                failure.message.clone(),
                if failure.runtime_missing {
                    controls::text_button(
                        SharedString::from(format!("browser-runtime-{pane_id}")),
                        "Get WebView2",
                    )
                    .on_click(|_, _, cx| cx.open_url(WEBVIEW2_RUNTIME_URL))
                    .into_any_element()
                } else {
                    div().into_any_element()
                },
            ),
            (None, None) if url.eq_ignore_ascii_case(BrowserContent::BLANK) => {
                self.render_browser_home(pane_id, cx)
            }
            (None, None) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color(colors.muted))
                .child("Starting the page…")
                .into_any_element(),
        };
        let key_pane = pane_id.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&pane.focus)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let modifiers = keystroke.modifiers;
                let plain =
                    !modifiers.control && !modifiers.alt && !modifiers.shift && !modifiers.platform;
                let address_key = (keystroke.key == "l"
                    && modifiers.secondary()
                    && !modifiers.alt
                    && !modifiers.shift)
                    || (keystroke.key == "f6" && plain);
                if address_key {
                    this.focus_address(&key_pane, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                } else if keystroke.key == "escape"
                    && plain
                    && this
                        .browser
                        .panes
                        .get(&key_pane)
                        .is_some_and(|pane| pane.address.focus_handle(cx).is_focused(window))
                {
                    this.revert_address(&key_pane, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.render_browser_toolbar(pane_id, pane, page, cx))
            .when_some(pane.address_error.clone(), |body, error| {
                body.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(colors.error))
                        .child(error),
                )
            })
            .child(body)
            .into_any_element()
    }

    fn render_browser_toolbar(
        &self,
        pane_id: &PaneId,
        pane: &BrowserPane,
        page: Option<&Rc<PageView>>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let loading = page.is_some_and(|view| view.signals.loading.borrow().is_some());
        let can_back = page.is_some_and(|view| view.can_back.get());
        let can_forward = page.is_some_and(|view| view.can_forward.get());
        let id = |name: &str| SharedString::from(format!("browser-{name}-{pane_id}"));
        let button = |name: &str, icon: IconName, tooltip: &'static str, navigation: Navigation| {
            let pane = pane_id.clone();
            controls::icon_button(id(name), icon, tooltip).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.focus_pane(pane.clone());
                    this.browser_navigate(navigation);
                    cx.notify();
                },
            ))
        };
        div()
            .relative()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(color(colors.border))
            .child(
                button("back", IconName::ArrowLeft, "Back", Navigation::Back)
                    .disabled_because((!can_back).then_some("Nothing to go back to")),
            )
            .child(
                button(
                    "forward",
                    IconName::ArrowRight,
                    "Forward",
                    Navigation::Forward,
                )
                .disabled_because((!can_forward).then_some("Nothing to go forward to")),
            )
            .child(if loading {
                button("stop", IconName::X, "Stop", Navigation::Reload)
            } else {
                button("reload", IconName::RotateCw, "Reload", Navigation::Reload)
                    .disabled_because(page.is_none().then_some("The page has not started"))
            })
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&pane.address)
                        .appearance(true)
                        .with_size(gpui_component::Size::Small),
                ),
            )
            .when(loading, |toolbar| {
                toolbar.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(LOADING_BAR_HEIGHT))
                        .bg(color(colors.accent)),
                )
            })
    }

    /// Why a pane shows no page, with what can be done about it.
    fn render_browser_notice(
        &self,
        title: String,
        detail: String,
        action: AnyElement,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .text_center()
            .child(
                gpui_component::Icon::new(IconName::TriangleAlert)
                    .size(px(24.0))
                    .text_color(color(colors.error)),
            )
            .child(div().text_size(px(UI_BODY_TEXT_SIZE)).child(title))
            .child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child(detail),
            )
            .child(action)
            .into_any_element()
    }

    /// Records float `index`'s frame where it is painted, then re-clips every page: pages
    /// are painted before the floats above them.
    pub(super) fn record_browser_float(&self, index: usize) -> Option<AnyElement> {
        if self.browser.pages.is_empty() && self.browser.media.views().next().is_none() {
            return None;
        }
        let pages = self.browser.pages.values().map(|page| &page.view);
        let pages = pages.chain(self.browser.media.views()).cloned().collect();
        Some(
            cut_out(self.browser.floats.clone(), pages, index)
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .size_full()
                .into_any_element(),
        )
    }
}

/// AppKit's keyboard: the view that has it is the window's first responder. GPUI's view
/// takes it only when told: it does not accept it from a click.
#[cfg(target_os = "macos")]
mod macos_page {
    use super::PageEvent;
    use block2::RcBlock;
    use gpui::Window;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use wry::{WebView, WebViewExtMacOS};

    /// Whether `webview`'s page, or a view inside it, has the keyboard.
    pub(super) fn holds_keyboard(webview: &WebView) -> bool {
        let page = webview.webview();
        let page: *const AnyObject = Retained::as_ptr(&page).cast();
        unsafe {
            let window: *mut AnyObject = msg_send![page, window];
            if window.is_null() {
                return false;
            }
            let responder: *mut AnyObject = msg_send![window, firstResponder];
            if responder.is_null() {
                return false;
            }
            let is_view: bool = msg_send![responder, isKindOfClass: class!(NSView)];
            is_view && msg_send![responder, isDescendantOf: page]
        }
    }

    /// GPUI's view of `window` and its NSWindow.
    fn views(window: &Window) -> Option<(*mut AnyObject, *mut AnyObject)> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        let view = handle.ns_view.as_ptr().cast::<AnyObject>();
        let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
        (!ns_window.is_null()).then_some((view, ns_window))
    }

    /// Gives the keyboard back to GPUI's view when nothing has it: AppKit hands it to the
    /// window itself when the view holding it is removed or hidden, and every key then
    /// only beeps.
    pub(super) fn reclaim_lost_keyboard(window: &Window) {
        let Some((view, ns_window)) = views(window) else {
            return;
        };
        unsafe {
            let responder: *mut AnyObject = msg_send![ns_window, firstResponder];
            if responder.is_null() || responder == ns_window {
                let _: bool = msg_send![ns_window, makeFirstResponder: view];
            }
        }
    }

    /// Reports mouse-downs in a window, which may put the keyboard in a page without
    /// GPUI seeing them. Removed when dropped.
    pub(super) struct ClickMonitor(Retained<AnyObject>);

    impl ClickMonitor {
        pub(super) fn new(
            window: &Window,
            events: async_channel::Sender<PageEvent>,
        ) -> Option<Self> {
            // Left, right and other mouse down.
            const MOUSE_DOWNS: u64 = 1 << 1 | 1 << 3 | 1 << 25;
            let (_, ns_window) = views(window)?;
            let ours = ns_window as usize;
            let handler = RcBlock::new(move |event: *mut AnyObject| -> *mut AnyObject {
                let window: *mut AnyObject = unsafe { msg_send![event, window] };
                if window as usize == ours {
                    let _ = events.try_send(PageEvent::Clicked);
                }
                event
            });
            let monitor: Option<Retained<AnyObject>> = unsafe {
                msg_send![
                    class!(NSEvent),
                    addLocalMonitorForEventsMatchingMask: MOUSE_DOWNS,
                    handler: &*handler
                ]
            };
            monitor.map(Self)
        }
    }

    impl Drop for ClickMonitor {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![class!(NSEvent), removeMonitor: &*self.0];
            }
        }
    }
}

/// WebView2's focus, accelerator and navigation events.
#[cfg(windows)]
mod windows_page {
    use super::{PageEvent, PaneId, Signals};
    use gpui::Keystroke;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
        COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_WEB_ERROR_STATUS,
        COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT,
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED,
        COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS,
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
        COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
        COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
        COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE, COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT,
        COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN,
    };
    use webview2_com::{
        AcceleratorKeyPressedEventHandler, FocusChangedEventHandler,
        NavigationCompletedEventHandler, NavigationStartingEventHandler, take_pwstr,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use wry::{WebView, WebViewExtWindows};

    /// What a key pressed in a page does when Compi rather than the page has it.
    pub(super) enum Forward {
        /// Compi's keystroke.
        Chord(String),
        /// Ctrl+L or F6: the pane's address field.
        Address,
    }

    /// The accelerator `virtual_key` with the modifiers held now, when Compi has it:
    /// Ctrl+L and F6 select the address; Ctrl+Shift or Alt with a key (except
    /// Alt+Left/Right, the page's back and forward) and Ctrl+Tab/T/W/Comma are Compi's.
    pub(super) fn forwarded(virtual_key: u32) -> Option<Forward> {
        let down = |key: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(key.0)) } < 0;
        let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
        let key = match virtual_key {
            0x41..=0x5A => char::from_u32(virtual_key + 0x20)?.to_string(),
            0x30..=0x39 => char::from_u32(virtual_key)?.to_string(),
            0x09 => "tab".into(),
            0x25 => "left".into(),
            0x26 => "up".into(),
            0x27 => "right".into(),
            0x28 => "down".into(),
            0xBC => ",".into(),
            0xBB => "+".into(),
            0xBD => "-".into(),
            0x70..=0x7B => format!("f{}", virtual_key - 0x6F),
            _ => return None,
        };
        if (ctrl && !shift && !alt && key == "l") || (!ctrl && !shift && !alt && key == "f6") {
            return Some(Forward::Address);
        }
        let history = alt && !ctrl && !shift && matches!(key.as_str(), "left" | "right");
        let compi = !history
            && ((ctrl && shift)
                || alt
                || (ctrl && matches!(key.as_str(), "tab" | "t" | "w" | ",")));
        if !compi {
            return None;
        }
        let mut chord = String::new();
        for (held, name) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-")] {
            if held {
                chord.push_str(name);
            }
        }
        chord.push_str(&key);
        Some(Forward::Chord(chord))
    }

    /// Why a navigation failed, in words.
    fn failure(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> String {
        match status {
            COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT
            | COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE => {
                "Nothing answered there. Is the server running?".into()
            }
            COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => {
                "That host name could not be found.".into()
            }
            COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => "The server took too long to answer.".into(),
            COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
            | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET
            | COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => {
                "The connection was closed before the page arrived.".into()
            }
            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
            | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS => {
                "The site's security certificate is not valid.".into()
            }
            // A refused connection to a local port reports no particular status.
            COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN => {
                "The page did not load. Is something listening at that address?".into()
            }
            other => format!("The page did not load (WebView2 error {}).", other.0),
        }
    }

    /// Forwards the page's focus, Compi's keys and failed loads to `events`; returns
    /// the window a clip region applies to.
    pub(super) fn watch(
        webview: &WebView,
        pane_id: &PaneId,
        signals: &Rc<Signals>,
        events: async_channel::Sender<PageEvent>,
    ) -> HWND {
        let controller = webview.controller();
        let mut container = HWND::default();
        let mut token = 0i64;
        unsafe {
            let _ = controller.ParentWindow(&mut container);
            let (keys, key_pane) = (events.clone(), pane_id.clone());
            let _ = controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    {
                        return Ok(());
                    }
                    let mut virtual_key = 0u32;
                    args.VirtualKey(&mut virtual_key)?;
                    let event = match forwarded(virtual_key) {
                        Some(Forward::Address) => PageEvent::FocusAddress(key_pane.clone()),
                        Some(Forward::Chord(chord)) => match Keystroke::parse(&chord) {
                            Ok(keystroke) => PageEvent::Key(keystroke),
                            Err(_) => return Ok(()),
                        },
                        None => return Ok(()),
                    };
                    args.SetHandled(true)?;
                    let _ = keys.try_send(event);
                    Ok(())
                })),
                &mut token,
            );
            let (focus, got, pane) = (signals.clone(), events.clone(), pane_id.clone());
            let _ = controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    focus.native_focus.set(true);
                    let _ = got.try_send(PageEvent::Focused(pane.clone()));
                    Ok(())
                })),
                &mut token,
            );
            let focus = signals.clone();
            let _ = controller.add_LostFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    focus.native_focus.set(false);
                    Ok(())
                })),
                &mut token,
            );
            // wry reports a start only once content arrives; a slow server is loading
            // from the moment the navigation starts.
            let (starting, starts, start_pane) = (signals.clone(), events.clone(), pane_id.clone());
            let _ = webview.webview().add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut uri = windows::core::PWSTR::null();
                    args.Uri(&mut uri)?;
                    *starting.loading.borrow_mut() = Some(take_pwstr(uri));
                    let _ = starts.try_send(PageEvent::Changed(start_pane.clone()));
                    Ok(())
                })),
                &mut token,
            );
            let (failed, loads, load_pane) = (signals.clone(), events, pane_id.clone());
            let _ = webview.webview().add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut success = windows::core::BOOL::default();
                    args.IsSuccess(&mut success)?;
                    // A load Compi stopped ends however WebView2 reports it.
                    let stopped = failed.stopping.replace(false);
                    if success.as_bool() || stopped {
                        return Ok(());
                    }
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                    args.WebErrorStatus(&mut status)?;
                    // Stopped, or replaced by another navigation: nothing went wrong.
                    if status == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
                        return Ok(());
                    }
                    *failed.failure.borrow_mut() = Some(failure(status));
                    let _ = loads.try_send(PageEvent::Changed(load_pane.clone()));
                    Ok(())
                })),
                &mut token,
            );
        }
        container
    }
}

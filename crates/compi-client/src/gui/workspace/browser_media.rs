//! PDF, audio and video files in a file tab: a system web view page, hosted like a
//! Browser pane's, showing the file through the `compimedia` protocol, which serves its
//! bytes from the daemon in ranges to the web view's PDF viewer or players. A page lives
//! while its file is open in the pane; out of view it is hidden and paused, and it is
//! destroyed when the file closes.

use super::*;
use base64::Engine as _;
use compi_protocol::{ContentEnvironment, FileKind, MAX_CONTENT_RANGE_BYTES};
use std::borrow::Cow;
use wry::http::{Request, Response, StatusCode, header};

/// The protocol media pages load from: `compimedia://localhost/` for an audio or video
/// file's page, `…/file` for the file. WebView2 sees it as `http://compimedia.localhost/`.
const SCHEME: &str = "compimedia";
/// A request without a range gets the whole file up to this size, else its first piece.
const WHOLE_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Pauses every player in a page.
const PAUSE: &str = "document.querySelectorAll('video,audio').forEach(m => m.pause())";

/// The file a media page shows.
#[derive(Clone)]
pub(in crate::gui) struct MediaSource {
    pub(in crate::gui) environment: ContentEnvironment,
    pub(in crate::gui) path: String,
    pub(in crate::gui) kind: FileKind,
}

/// A media file open in a pane.
type MediaKey = (PaneId, String);

/// This window's media pages.
#[derive(Default)]
pub(super) struct MediaPages {
    pages: HashMap<MediaKey, Rc<PageView>>,
    creating: HashSet<MediaKey>,
    failed: HashMap<MediaKey, PageFailure>,
    /// Files whose tab was active at the last draw, to pause those that leave.
    active: HashSet<MediaKey>,
}

impl MediaPages {
    pub(super) fn views(&self) -> impl Iterator<Item = &Rc<PageView>> {
        self.pages.values()
    }
}

/// The MIME type a media file is served as, by its extension.
fn mime(path: &str) -> &'static str {
    let extension = path
        .rsplit(['/', '\\'])
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("pdf") => "application/pdf",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("ogv") => "video/ogg",
        Some("avi") => "video/x-msvideo",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        _ => "application/octet-stream",
    }
}

/// A requested byte range: from `start` to `end` inclusive, or the last `suffix` bytes.
#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    From { start: u64, end: Option<u64> },
    Last(u64),
}

/// The single range of a `Range: bytes=…` header.
fn parse_range(value: &str) -> Option<ByteRange> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        return end.parse().ok().map(ByteRange::Last);
    }
    let start = start.parse().ok()?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok().filter(|end| *end >= start)?)
    };
    Some(ByteRange::From { start, end })
}

/// The page an audio or video file plays in, in the window's colours; it says so when
/// the web view cannot play the file.
fn player_page(kind: FileKind, background: u32, foreground: u32) -> String {
    let element = if kind == FileKind::Audio {
        "<audio controls preload=metadata src=file></audio>"
    } else {
        "<video controls preload=metadata src=file></video>"
    };
    format!(
        "<!doctype html><meta charset=utf-8><style>html,body{{margin:0;height:100%;\
         background:#{background:06x};color:#{foreground:06x};font:13px system-ui}}\
         body{{display:flex;align-items:center;justify-content:center}}\
         video{{max-width:100%;max-height:100%}}audio{{width:min(640px,90%)}}</style>\
         {element}<script>document.querySelector('video,audio').addEventListener('error',\
         ()=>document.body.textContent='This file does not play in the system web view.')\
         </script>"
    )
}

/// Answers a media page's request: its player page, or bytes of its file in the range
/// asked for (at most `MAX_CONTENT_RANGE_BYTES`, or the whole file up to
/// `WHOLE_FILE_BYTES` without a range).
fn serve(
    target: &ConnectionTarget,
    source: &MediaSource,
    page: &str,
    request: &Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let plain = |status: StatusCode, body: String| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Cow::Owned(body.into_bytes()))
            .unwrap_or_default()
    };
    match request.uri().path() {
        "/" => {
            return Response::builder()
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Owned(page.as_bytes().to_vec()))
                .unwrap_or_default();
        }
        "/file" => {}
        _ => return plain(StatusCode::NOT_FOUND, "Not found".into()),
    }
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_range);
    let result = (|| -> Result<Response<Cow<'static, [u8]>>, String> {
        let mut client = target.connect().map_err(|error| error.to_string())?;
        let mut read = |offset: u64, len: u64| {
            let range = client
                .read_content_range(source.environment.clone(), source.path.clone(), offset, len)
                .map_err(|error| error.to_string())?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&range.base64)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((range.len, bytes))
        };
        let piece = MAX_CONTENT_RANGE_BYTES as u64;
        let (len, _) = read(0, 0)?;
        let (start, end, partial) = match range {
            Some(ByteRange::From { start, end }) => {
                let last = end.unwrap_or(u64::MAX).min(len.saturating_sub(1));
                (start, last.min(start + piece - 1), true)
            }
            Some(ByteRange::Last(suffix)) => {
                let start = len.saturating_sub(suffix.min(piece));
                (start, len.saturating_sub(1), true)
            }
            None if len <= WHOLE_FILE_BYTES => (0, len.saturating_sub(1), false),
            None => (0, piece - 1, true),
        };
        if len == 0 || start >= len {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Cow::Borrowed(&[][..]))
                .unwrap_or_default());
        }
        let mut body = Vec::with_capacity((end - start + 1) as usize);
        while (body.len() as u64) < end - start + 1 {
            let at = start + body.len() as u64;
            let (_, bytes) = read(at, (end + 1 - at).min(piece))?;
            if bytes.is_empty() {
                break;
            }
            body.extend_from_slice(&bytes);
        }
        let end = start + body.len() as u64 - 1;
        let mut response = Response::builder()
            .header(header::CONTENT_TYPE, mime(&source.path))
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "no-store");
        response = if partial {
            response
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        } else {
            response.status(StatusCode::OK)
        };
        Ok(response.body(Cow::Owned(body)).unwrap_or_default())
    })();
    result.unwrap_or_else(|error| plain(StatusCode::INTERNAL_SERVER_ERROR, error))
}

/// Whether a media page may go to `url`: only to its own protocol.
fn media_url(url: &str) -> bool {
    url.starts_with(&format!("{SCHEME}://"))
        || url.starts_with(&format!("http://{SCHEME}."))
        || url.starts_with(&format!("https://{SCHEME}."))
}

/// A page for media file `key` in `parent`, hidden until its tab is painted.
fn build_media_page(
    parent: RawWindowHandle,
    context: &RefCell<WebContext>,
    key: &MediaKey,
    source: MediaSource,
    target: ConnectionTarget,
    colors: (u32, u32),
    events: &async_channel::Sender<PageEvent>,
) -> Result<PageView, PageFailure> {
    let url = if source.kind == FileKind::Pdf {
        format!("{SCHEME}://localhost/file")
    } else {
        format!("{SCHEME}://localhost/")
    };
    let signals = Rc::new(Signals {
        url: RefCell::new(url.clone()),
        ..Signals::default()
    });
    let page: Arc<str> = player_page(source.kind, colors.0, colors.1).into();
    let background = colors.0.to_be_bytes();
    let mut context = context.borrow_mut();
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_asynchronous_custom_protocol(SCHEME.into(), move |_, request, responder| {
            let (target, source, page) = (target.clone(), source.clone(), page.clone());
            thread::spawn(move || responder.respond(serve(&target, &source, &page, &request)));
        })
        .with_url(&url)
        .with_background_color((background[1], background[2], background[3], 255))
        .with_visible(false)
        .with_focused(false)
        .with_bounds(wry::Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(1.0, 1.0).into(),
        })
        .with_navigation_handler(|url| media_url(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .build_as_child(&ParentWindow(parent))
        .map_err(|error| PageFailure {
            runtime_missing: runtime_missing(),
            message: format!("The system web view could not start: {error}"),
        })?;
    #[cfg(windows)]
    let container = windows_page::watch(&webview, &key.0, &signals, events.clone());
    #[cfg(not(windows))]
    let _ = (key, events);
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
    /// Before drawing: drops the pages of media files no longer open, creates pages for
    /// files whose tab is active in a shown pane, hides pages out of view or under
    /// `covered` GPUI, pauses those whose tab stopped being active, and hands the
    /// keyboard back to GPUI when a page's pane loses focus.
    pub(super) fn prepare_media(
        &mut self,
        covered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.media_files();
        let media = &mut self.browser.media;
        let kept = |key: &MediaKey| open.iter().any(|(open, _, _)| open == key);
        media.pages.retain(|key, _| kept(key));
        media.failed.retain(|key, _| kept(key));
        media.active.retain(|key| kept(key));
        let focused = self.focused_pane().cloned();
        let mut missing = Vec::new();
        let mut clicked = None;
        for (key, source, active) in open {
            let Some(view) = self.browser.media.pages.get(&key) else {
                if active
                    && !self.browser.media.creating.contains(&key)
                    && !self.browser.media.failed.contains_key(&key)
                {
                    missing.push((key, source));
                }
                continue;
            };
            let float_index = self
                .float_layouts
                .iter()
                .position(|float| float.pane.pane_id == key.0);
            view.float_index.set(float_index);
            let drawn = active
                && (float_index.is_some()
                    || self
                        .layout
                        .as_ref()
                        .is_some_and(|layout| layout.pane(&key.0).is_some()));
            let hidden = !drawn || covered;
            let took = view.sync_keyboard();
            view.hidden.set(hidden);
            if hidden {
                view.show(false);
            }
            if !active && self.browser.media.active.remove(&key) {
                let _ = view.webview.evaluate_script(PAUSE);
            } else if active {
                self.browser.media.active.insert(key.clone());
            }
            let own = focused.as_ref() == Some(&key.0);
            if took && !hidden && !own {
                // A click put the keyboard in the page: its pane takes focus.
                clicked = Some(key.0.clone());
            } else if view.signals.native_focus.get() && !own {
                view.release_keyboard();
            }
        }
        if let Some(pane_id) = clicked {
            self.focus_pane(pane_id);
        }
        if !missing.is_empty() {
            self.create_media_pages(missing, window, cx);
        }
    }

    fn create_media_pages(
        &mut self,
        missing: Vec<(MediaKey, MediaSource)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(parent) = HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        else {
            return;
        };
        self.browser
            .media
            .creating
            .extend(missing.iter().map(|(key, _)| key.clone()));
        let events = self.page_events(window, cx);
        let context = self.web_context();
        let colors = self.colors();
        let colors = (colors.background, colors.foreground);
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            for (key, source) in missing {
                if this.update(cx, |_, _| ()).is_err() {
                    break;
                }
                let built = build_media_page(
                    parent,
                    &context,
                    &key,
                    source,
                    target.clone(),
                    colors,
                    &events,
                );
                let stored = this.update(cx, |this, cx| {
                    let media = &mut this.browser.media;
                    media.creating.remove(&key);
                    match built {
                        Ok(view) => {
                            media.pages.insert(key, Rc::new(view));
                        }
                        Err(failure) => {
                            media.failed.insert(key, failure);
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

    /// A media file's body: its page, placed where this is painted, or why it has none.
    pub(in crate::gui) fn render_media_page(&self, pane_id: &PaneId, path: &str) -> AnyElement {
        let colors = *self.colors();
        let key = (pane_id.clone(), path.to_owned());
        if let Some(view) = self.browser.media.pages.get(&key) {
            let view = view.clone();
            let floats = self.browser.floats.clone();
            return canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    view.place(bounds, &floats.borrow(), window.scale_factor())
                },
            )
            .flex_1()
            .size_full()
            .into_any_element();
        }
        let message = match self.browser.media.failed.get(&key) {
            Some(failure) if failure.runtime_missing => {
                "The WebView2 Runtime is not installed, which shows this file".to_owned()
            }
            Some(failure) => failure.message.clone(),
            None => "Opening…".to_owned(),
        };
        div()
            .flex_1()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .text_color(color(colors.muted))
            .child(message)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_as_web_views_send_them() {
        let from = |start, end| Some(ByteRange::From { start, end });
        assert_eq!(parse_range("bytes=0-"), from(0, None));
        assert_eq!(parse_range("bytes=100-199"), from(100, Some(199)));
        assert_eq!(parse_range("bytes=-500"), Some(ByteRange::Last(500)));
        // Several ranges, reversed or malformed ones are answered as no range.
        assert_eq!(parse_range("bytes=0-1,5-6"), None);
        assert_eq!(parse_range("bytes=9-3"), None);
        assert_eq!(parse_range("items=0-1"), None);
        assert_eq!(parse_range("bytes=x-"), None);
    }

    #[test]
    fn media_pages_stay_on_their_own_protocol() {
        assert!(media_url("compimedia://localhost/file"));
        assert!(media_url("http://compimedia.localhost/"));
        assert!(!media_url("https://example.com/"));
        assert!(!media_url("http://compimedia-evil.example/"));
        assert!(!media_url("file:///etc/passwd"));
    }
}

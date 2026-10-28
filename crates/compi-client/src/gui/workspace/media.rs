use super::*;
use crate::image_input::{self, ImageInput, ImageTarget, PreparedImage};
use gpui::{DispatchPhase, Entity, ScrollDelta, Size};
use std::cell::Cell as ViewportCell;
use std::path::PathBuf;
use std::rc::Rc;

const MAX_PREVIEWS: usize = 8;
const MAX_PENDING_INPUTS: usize = 4;
static NEXT_IMAGE_REQUEST: AtomicU64 = AtomicU64::new(1);

pub(in crate::gui) struct InputOrigin {
    pub(in crate::gui) view_id: u64,
    surface_id: SurfaceId,
    lifetime: compi_protocol::ProcessLifetimeId,
    server_id: compi_protocol::ServerId,
    generation: compi_protocol::ServerGeneration,
}

pub(in crate::gui) struct ImagePreview {
    id: u64,
    surface_id: SurfaceId,
    lifetime: compi_protocol::ProcessLifetimeId,
    pub(super) image: Arc<PreparedImage>,
}

pub(in crate::gui) struct InspectorState {
    request: u64,
    path: PathBuf,
    name: String,
    pub(super) image: Option<Arc<RenderImage>>,
    error: Option<String>,
    view: ZoomView,
}

/// How an image sits in its box: fitted (no larger than its natural size) or zoomed, and
/// panned; with the box it was last drawn in. The inspector's and an image file's.
#[derive(Default)]
pub(in crate::gui) struct ZoomView {
    /// The image's size at 100%, in logical pixels.
    natural: Option<Size<f32>>,
    /// `None` fits the image.
    zoom: Option<f32>,
    pan: Point<Pixels>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    viewport: Rc<ViewportCell<Option<Bounds<Pixels>>>>,
}

/// Where a `ZoomView` lives, for the events that reach it after its frame.
#[derive(Clone)]
pub(in crate::gui) enum ZoomTarget {
    Inspector,
    File { pane: PaneId, path: String },
}

/// The scale an image of `natural` size shows at in `bounds`: `zoom`, or fitted.
fn view_scale(bounds: Bounds<Pixels>, natural: Size<f32>, zoom: Option<f32>) -> f32 {
    zoom.unwrap_or_else(|| {
        (f32::from(bounds.size.width) / natural.width.max(1.0))
            .min(f32::from(bounds.size.height) / natural.height.max(1.0))
            .min(1.0)
    })
}

impl ZoomView {
    /// Shows an image of `natural` size; a different size starts fitted.
    pub(in crate::gui) fn set_natural(&mut self, natural: Size<f32>) {
        if self.natural != Some(natural) {
            self.natural = Some(natural);
            self.zoom = None;
            self.pan = point(px(0.0), px(0.0));
        }
    }

    /// The scale the image showed at when last drawn.
    pub(in crate::gui) fn scale(&self) -> Option<f32> {
        Some(view_scale(self.viewport.get()?, self.natural?, self.zoom))
    }

    fn clamp_pan(&mut self, bounds: Bounds<Pixels>) {
        let Some(natural) = self.natural else { return };
        let scale = view_scale(bounds, natural, self.zoom);
        let x = ((natural.width * scale - f32::from(bounds.size.width)) / 2.0).max(0.0);
        let y = ((natural.height * scale - f32::from(bounds.size.height)) / 2.0).max(0.0);
        self.pan.x = px(f32::from(self.pan.x).clamp(-x, x));
        self.pan.y = px(f32::from(self.pan.y).clamp(-y, y));
    }

    /// Zooms by `factor` (5% to 1600%), about `anchor` or the centre; `None` fits.
    pub(in crate::gui) fn zoom(&mut self, factor: Option<f32>, anchor: Option<Point<Pixels>>) {
        let (Some(bounds), Some(natural)) = (self.viewport.get(), self.natural) else {
            return;
        };
        let old = view_scale(bounds, natural, self.zoom);
        self.zoom = factor.map(|factor| (old * factor).clamp(0.05, 16.0));
        if let Some(anchor) = anchor {
            let ratio = view_scale(bounds, natural, self.zoom) / old.max(0.0001);
            let dx = anchor.x - bounds.center().x;
            let dy = anchor.y - bounds.center().y;
            self.pan = point(
                dx - (dx - self.pan.x) * ratio,
                dy - (dy - self.pan.y) * ratio,
            );
        } else {
            self.pan = point(px(0.0), px(0.0));
        }
        self.clamp_pan(bounds);
    }

    /// 100%, centred.
    pub(in crate::gui) fn actual_size(&mut self) {
        self.zoom = Some(1.0);
        self.pan = point(px(0.0), px(0.0));
    }
}

/// `image` as `view` places it, filling the canvas's box: the wheel zooms about the
/// pointer and a drag pans. Events find the view again through `target`.
pub(in crate::gui) fn zoom_canvas(
    image: Arc<RenderImage>,
    view: &ZoomView,
    target: ZoomTarget,
    entity: Entity<CompiApp>,
) -> impl IntoElement {
    let viewport = view.viewport.clone();
    let (zoom, pan) = (view.zoom, view.pan);
    let natural = view.natural.unwrap_or_else(|| {
        let pixels = image.size(0);
        size(pixels.width.0 as f32, pixels.height.0 as f32)
    });
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let scale = view_scale(bounds, natural, zoom);
            viewport.set(Some(bounds));
            let dimensions = size(px(natural.width * scale), px(natural.height * scale));
            let origin = point(
                bounds.center().x - dimensions.width / 2.0 + pan.x,
                bounds.center().y - dimensions.height / 2.0 + pan.y,
            );
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let _ = window.paint_image(
                    bounds,
                    Bounds::new(origin, dimensions),
                    Corners::default(),
                    image.clone(),
                    0,
                    false,
                );
            });
            window.on_mouse_event({
                let (entity, target) = (entity.clone(), target.clone());
                move |event: &MouseDownEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.button == MouseButton::Left
                        && bounds.contains(&event.position)
                    {
                        entity.update(cx, |this, _| {
                            if let Some(view) = this.zoom_view(&target) {
                                view.drag = Some((event.position, view.pan));
                            }
                        });
                    }
                }
            });
            window.on_mouse_event({
                let (entity, target) = (entity.clone(), target.clone());
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    entity.update(cx, |this, cx| {
                        if let Some(view) = this.zoom_view(&target)
                            && let Some((origin, pan)) = view.drag
                        {
                            if event.dragging() {
                                view.pan = point(
                                    pan.x + event.position.x - origin.x,
                                    pan.y + event.position.y - origin.y,
                                );
                                view.clamp_pan(bounds);
                                cx.stop_propagation();
                                cx.notify();
                            } else {
                                view.drag = None;
                            }
                        }
                    });
                }
            });
            window.on_mouse_event({
                let (entity, target) = (entity.clone(), target.clone());
                move |_: &MouseUpEvent, _, _, cx| {
                    entity.update(cx, |this, _| {
                        if let Some(view) = this.zoom_view(&target) {
                            view.drag = None;
                        }
                    })
                }
            });
            window.on_mouse_event({
                let (entity, target) = (entity.clone(), target.clone());
                move |event: &ScrollWheelEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && bounds.contains(&event.position) {
                        entity.update(cx, |this, cx| {
                            // A wheel notch arrives as the system's scroll lines (often 3
                            // to 5), a trackpad as pixels: about 1.2× to 1.4× a notch.
                            let factor = match event.delta {
                                ScrollDelta::Lines(lines) => (lines.y * 0.065).exp(),
                                ScrollDelta::Pixels(pixels) => (f32::from(pixels.y) * 0.005).exp(),
                            };
                            if let Some(view) = this.zoom_view(&target) {
                                view.zoom(Some(factor), Some(event.position));
                            }
                            cx.stop_propagation();
                            cx.notify();
                        });
                    }
                }
            });
        },
    )
    .size_full()
}

enum MediaJob {
    Prepare {
        request: u64,
        origin: InputOrigin,
        input: ImageInput,
        target: ImageTarget,
        connection_target: ConnectionTarget,
        sender: UiEventSender,
    },
    Inspect {
        request: u64,
        path: PathBuf,
        sender: UiEventSender,
    },
    Copy {
        path: PathBuf,
        sender: UiEventSender,
    },
    Save {
        source: PathBuf,
        destination: PathBuf,
        sender: UiEventSender,
    },
}

static MEDIA_WORKERS: LazyLock<mpsc::SyncSender<MediaJob>> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::sync_channel::<MediaJob>(4);
    let receiver = Arc::new(Mutex::new(receiver));
    for _ in 0..2 {
        let receiver = receiver.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let Ok(receiver) = receiver.lock() else {
                        return;
                    };
                    receiver.recv()
                };
                let Ok(job) = job else { return };
                match job {
                    MediaJob::Prepare {
                        request,
                        origin,
                        input,
                        target,
                        connection_target,
                        sender,
                    } => {
                        let result = image_input::prepare(input, target).and_then(|mut image| {
                            if image.quoted_path.is_none() {
                                let bytes = image_input::read_upload(&image.path)?;
                                let mut client = connection_target.connect().map_err(|error| {
                                    format!("Cannot connect for image upload: {error}")
                                })?;
                                let remote_path = client
                                    .upload_image(&image.name, &bytes)
                                    .map_err(|error| format!("Cannot upload image: {error}"))?;
                                image.quoted_path =
                                    Some(image_input::quote_remote_path(&remote_path)?);
                                image.shell_path = Some(remote_path);
                            }
                            Ok(image)
                        });
                        sender.send(UiEvent::ImagePrepared {
                            request,
                            origin,
                            result,
                        });
                    }
                    MediaJob::Inspect {
                        request,
                        path,
                        sender,
                    } => {
                        let result = image_input::load_full_image(&path);
                        sender.send(UiEvent::ImageInspectorLoaded { request, result });
                    }
                    MediaJob::Copy { path, sender } => {
                        sender.send(UiEvent::ImageClipboardReady(image_input::clipboard_image(
                            &path,
                        )));
                    }
                    MediaJob::Save {
                        source,
                        destination,
                        sender,
                    } => {
                        let result = image_input::clipboard_image(&source).and_then(|image| {
                            let mut file = OpenOptions::new()
                                .write(true)
                                .create(true)
                                .truncate(true)
                                .open(&destination)
                                .map_err(|error| format!("Cannot save image: {error}"))?;
                            file.write_all(&image.bytes)
                                .and_then(|()| file.sync_all())
                                .map_err(|error| format!("Cannot save image: {error}"))?;
                            Ok(format!("Saved {}", destination.display()))
                        });
                        sender.send(UiEvent::ImageOperationFinished(result));
                    }
                }
            }
        });
    }
    sender
});

impl CompiApp {
    pub(in crate::gui) fn prepare_image_input(
        &mut self,
        input: ImageInput,
        view_id: u64,
        cx: &mut Context<Self>,
    ) {
        if self.pending_image_inputs.len() >= MAX_PENDING_INPUTS {
            self.global_error = Some(
                "Four images are already being prepared. Wait for them before pasting more.".into(),
            );
            cx.notify();
            return;
        }
        let Some(view) = self.surface_views.iter().find(|view| view.id == view_id) else {
            return;
        };
        if view.state != ConnectionState::Attached {
            self.global_error =
                Some("Reconnect a running terminal before pasting an image.".into());
            cx.notify();
            return;
        }
        let Some(workspace) = &self.workspace else {
            return;
        };
        let origin = InputOrigin {
            view_id,
            surface_id: view.surface_id.clone(),
            lifetime: view.lifetime.clone(),
            server_id: workspace.server_id.clone(),
            generation: workspace.server_generation.clone(),
        };
        let target = if self.target.is_remote() {
            ImageTarget::Remote
        } else {
            #[cfg(windows)]
            {
                let distribution = workspace
                    .surface(&view.surface_id)
                    .and_then(|surface| surface.wsl_distribution())
                    .map(str::to_owned);
                ImageTarget::Wsl { distribution }
            }
            #[cfg(target_os = "macos")]
            {
                ImageTarget::Native
            }
        };
        let request = NEXT_IMAGE_REQUEST.fetch_add(1, Ordering::Relaxed);
        let job = MediaJob::Prepare {
            request,
            origin,
            input,
            target,
            connection_target: self.target.clone(),
            sender: self.event_tx.clone(),
        };
        if MEDIA_WORKERS.try_send(job).is_err() {
            self.global_error = Some(
                "Image workers are busy. Try pasting again after the current images finish.".into(),
            );
        } else {
            self.pending_image_inputs.insert(request);
            self.image_notice = None;
        }
        cx.notify();
    }

    pub(super) fn drop_image_files(
        &mut self,
        paths: &gpui::ExternalPaths,
        view_id: u64,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            self.global_error =
                Some("Close the overlay before dropping images into a terminal.".into());
            cx.notify();
            return;
        }
        if paths.paths().len() > MAX_PENDING_INPUTS {
            self.global_error = Some("Drop up to four images at a time.".into());
            cx.notify();
            return;
        }
        for path in paths.paths() {
            self.prepare_image_input(ImageInput::File(path.clone()), view_id, cx);
        }
    }

    pub(super) fn accept_prepared_image(
        &mut self,
        request: u64,
        origin: InputOrigin,
        result: Result<PreparedImage, String>,
    ) {
        self.pending_image_inputs.remove(&request);
        let image = match result {
            Ok(image) => Arc::new(image),
            Err(error) => {
                self.global_error = Some(error);
                return;
            }
        };
        let valid_generation = self.workspace.as_ref().is_some_and(|workspace| {
            workspace.server_id == origin.server_id
                && workspace.server_generation == origin.generation
        });
        let view = self.surface_views.iter_mut().find(|view| {
            valid_generation
                && view.id == origin.view_id
                && view.surface_id == origin.surface_id
                && view.lifetime == origin.lifetime
                && view.state == ConnectionState::Attached
                && !view.stop.load(Ordering::Acquire)
        });
        let Some(view) = view else {
            self.global_error = Some(format!(
                "The image is ready, but its terminal changed. File retained at {}",
                image.path.display()
            ));
            return;
        };
        let bracketed = view
            .mirror
            .snapshot()
            .is_some_and(|snapshot| snapshot.modes.bracketed_paste);
        let Some(quoted_path) = image.quoted_path.as_deref() else {
            self.global_error = Some("Remote image upload did not return a shell path".into());
            return;
        };
        let input = format!("{quoted_path} ");
        view.send(ClientMessage::Input {
            data: crate::input::encode_paste(&input, bracketed),
            latency_id: None,
            alternate_scroll: false,
        });
        if self.image_previews.len() == MAX_PREVIEWS {
            self.image_previews.pop_front();
        }
        self.image_previews.push_back(ImagePreview {
            id: request,
            surface_id: origin.surface_id,
            lifetime: origin.lifetime,
            image,
        });
    }

    fn open_image_inspector(&mut self, image: Arc<PreparedImage>, cx: &mut Context<Self>) {
        let request = NEXT_IMAGE_REQUEST.fetch_add(1, Ordering::Relaxed);
        let job = MediaJob::Inspect {
            request,
            path: image.path.clone(),
            sender: self.event_tx.clone(),
        };
        if MEDIA_WORKERS.try_send(job).is_err() {
            self.global_error =
                Some("Image workers are busy. Try opening the preview again shortly.".into());
            cx.notify();
            return;
        }
        self.open_overlay(Overlay::ImageInspector, "");
        self.image_inspector = Some(InspectorState {
            request,
            path: image.path.clone(),
            name: image.name.clone(),
            image: None,
            error: None,
            view: ZoomView::default(),
        });
        self.image_notice = None;
        cx.notify();
    }

    pub(super) fn accept_inspector_image(
        &mut self,
        request: u64,
        result: Result<Arc<RenderImage>, String>,
    ) {
        if let Some(inspector) = &mut self.image_inspector {
            if inspector.request != request {
                return;
            }
            match result {
                Ok(image) => {
                    let pixels = image.size(0);
                    inspector
                        .view
                        .set_natural(size(pixels.width.0 as f32, pixels.height.0 as f32));
                    inspector.image = Some(image);
                }
                Err(error) => inspector.error = Some(error),
            }
        }
    }

    fn copy_inspector_image(&mut self, cx: &mut Context<Self>) {
        let Some(inspector) = &self.image_inspector else {
            return;
        };
        let job = MediaJob::Copy {
            path: inspector.path.clone(),
            sender: self.event_tx.clone(),
        };
        if MEDIA_WORKERS.try_send(job).is_err() {
            self.image_notice = Some("Image workers are busy. Try Copy again shortly.".into());
        } else {
            self.image_notice = Some("Copying image…".into());
        }
        cx.notify();
    }

    fn save_inspector_image(&mut self, cx: &mut Context<Self>) {
        let Some(inspector) = &self.image_inspector else {
            return;
        };
        let source = inspector.path.clone();
        let parent = source.parent().unwrap_or_else(|| std::path::Path::new("."));
        let picker =
            cx.prompt_for_new_path(parent, source.file_name().and_then(|name| name.to_str()));
        let sender = self.event_tx.clone();
        cx.spawn(async move |weak, cx| match picker.await {
            Ok(Ok(Some(destination))) => {
                let queued = MEDIA_WORKERS
                    .try_send(MediaJob::Save {
                        source,
                        destination,
                        sender,
                    })
                    .is_ok();
                let _ = weak.update(cx, |this, cx| {
                    this.image_notice = Some(
                        if queued {
                            "Saving image…"
                        } else {
                            "Image workers are busy. Try Save again shortly."
                        }
                        .into(),
                    );
                    cx.notify();
                });
            }
            Ok(Err(error)) => {
                let _ = weak.update(cx, |this, cx| {
                    this.image_notice = Some(format!("Could not open Save dialog: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    fn zoom_inspector(&mut self, factor: Option<f32>, anchor: Option<Point<Pixels>>) {
        if let Some(inspector) = &mut self.image_inspector {
            inspector.view.zoom(factor, anchor);
        }
    }

    /// The zoom view `target` names, if it is still there.
    pub(in crate::gui) fn zoom_view(&mut self, target: &ZoomTarget) -> Option<&mut ZoomView> {
        match target {
            ZoomTarget::Inspector => self
                .image_inspector
                .as_mut()
                .map(|inspector| &mut inspector.view),
            ZoomTarget::File { pane, path } => self.file_zoom_view(pane, path),
        }
    }

    pub(super) fn inspector_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.overlay, Some(Overlay::ImageInspector)) {
            return false;
        }
        match key.key.as_str() {
            "escape" => {
                self.dismiss_overlay();
                window.focus(&self.focus_handle, cx);
            }
            "+" | "plus" | "=" | "equal" => self.zoom_inspector(Some(1.25), None),
            "-" | "minus" => self.zoom_inspector(Some(0.8), None),
            "0" => self.zoom_inspector(None, None),
            "1" => {
                if let Some(inspector) = &mut self.image_inspector {
                    inspector.view.actual_size();
                }
            }
            "c" if key.modifiers.control || key.modifiers.platform => self.copy_inspector_image(cx),
            "s" if key.modifiers.control || key.modifiers.platform => self.save_inspector_image(cx),
            _ => {}
        }
        cx.notify();
        true
    }

    /// Right-click on a pasted image's card: open it, open it in a pane beside its
    /// terminal, copy its path as the shell sees it, or dismiss the card.
    fn open_image_preview_menu(
        &mut self,
        id: u64,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.image_previews.iter().find(|preview| preview.id == id) else {
            return;
        };
        let image = preview.image.clone();
        let shell_path = preview.image.shell_path.clone();
        let mut entries = vec![
            controls::menu_item(
                "Open",
                cx.listener(move |this, _, _, cx| this.open_image_inspector(image.clone(), cx)),
            )
            .into(),
            controls::menu_item(
                "Open in pane beside",
                cx.listener(move |this, _, _, cx| {
                    this.open_image_beside(id);
                    cx.notify();
                }),
            )
            .into(),
        ];
        if let Some(path) = shell_path {
            entries.push(controls::MenuEntry::Separator);
            entries.push(
                controls::menu_item("Copy path", move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                })
                .into(),
            );
        }
        entries.push(controls::MenuEntry::Separator);
        entries.push(
            controls::menu_item(
                "Dismiss",
                cx.listener(move |this, _, _, cx| {
                    this.image_previews.retain(|preview| preview.id != id);
                    cx.notify();
                }),
            )
            .into(),
        );
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// Opens a pasted image in a Files pane split beside its terminal, read through
    /// the terminal's environment like any file the shell names.
    fn open_image_beside(&mut self, id: u64) {
        let Some(preview) = self.image_previews.iter().find(|preview| preview.id == id) else {
            return;
        };
        let Some(path) = preview.image.shell_path.clone() else {
            self.global_error = Some("The image has no path its terminal can read".into());
            return;
        };
        let target = self.selected_tab().and_then(|tab| {
            let workspace = self.workspace.as_ref()?;
            let surface = workspace
                .surface(&preview.surface_id)
                .filter(|surface| surface.process_lifetime_id == preview.lifetime)?;
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            let (pane_id, _) = leaves
                .into_iter()
                .find(|(_, leaf)| leaf.as_ref() == Some(&preview.surface_id))?;
            Some((
                pane_id,
                compi_protocol::ContentEnvironment::of_surface(surface),
            ))
        });
        let Some((pane_id, environment)) = target else {
            self.global_error = Some("The image's terminal is no longer in this tab".into());
            return;
        };
        let tab = compi_protocol::ContentTab::edit(path.clone());
        self.open_content_beside(
            &pane_id,
            compi_protocol::PaneContent::Files(compi_protocol::FolderContent {
                environment,
                root: super::content::parent_folder(&path),
                tabs: vec![tab.clone()],
                active: Some(tab),
            }),
        );
    }

    /// Pasted image cards of the focused terminal, at most `max_height` tall; placed at
    /// the bottom right by `render_corner`.
    pub(super) fn render_image_previews(
        &self,
        max_height: Pixels,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let focused = self.focused_view();
        let previews = self
            .image_previews
            .iter()
            .filter(|preview| {
                focused.is_some_and(|view| {
                    view.surface_id == preview.surface_id && view.lifetime == preview.lifetime
                })
            })
            .map(|preview| {
                let image = preview.image.clone();
                let id = preview.id;
                div()
                    .id(("image-preview", id as usize))
                    .w(px(240.0))
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_image_inspector(image.clone(), cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_image_preview_menu(id, event.position, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        gpui::img(preview.image.thumbnail.clone())
                            .w(px(56.0))
                            .h(px(44.0))
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(preview.image.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(format!(
                                        "{} × {} · {:.1} KiB",
                                        preview.image.width,
                                        preview.image.height,
                                        preview.image.byte_len as f64 / 1024.0
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id(("dismiss-image-preview", id as usize))
                            .px_1()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.image_previews.retain(|preview| preview.id != id);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            .child(chrome_icon(ChromeIcon::Close, color(colors.muted))),
                    )
            });
        div()
            .id("image-preview-shelf")
            .max_h(max_height)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .children(previews)
            .when(!self.pending_image_inputs.is_empty(), |shelf| {
                shelf.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .bg(color(colors.surface))
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .child(format!(
                            "Preparing {} image(s)…",
                            self.pending_image_inputs.len()
                        )),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_image_inspector(&self, cx: &Context<Self>) -> AnyElement {
        let Some(inspector) = &self.image_inspector else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let image_canvas = match inspector.image.clone() {
            Some(image) => zoom_canvas(image, &inspector.view, ZoomTarget::Inspector, cx.entity())
                .into_any_element(),
            None => div().into_any_element(),
        };
        let error = inspector.error.clone();
        let status = self.image_notice.clone().unwrap_or_else(|| {
            if let Some(error) = error {
                error
            } else if let Some(image) = &inspector.image {
                format!(
                    "{} × {} · wheel to zoom · drag to pan · 0 to fit",
                    image.size(0).width.0,
                    image.size(0).height.0
                )
            } else {
                "Loading image…".into()
            }
        });
        div()
            .absolute()
            .top(px(CHROME_HEIGHT))
            .bottom_0()
            .left_0()
            .right_0()
            .p_3()
            .bg(color(colors.background).opacity(0.8))
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(color(colors.border))
                    .rounded_md()
                    .overflow_hidden()
                    .bg(color(colors.background))
                    .child(
                        div()
                            .flex_none()
                            .px_3()
                            .py_2()
                            .bg(color(colors.surface))
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(inspector.name.clone()),
                            )
                            .child(self.inspector_button(
                                "image-zoom-out",
                                "−",
                                cx,
                                |this, cx| {
                                    this.zoom_inspector(Some(0.8), None);
                                    cx.notify();
                                },
                            ))
                            .child(self.inspector_button("image-fit", "Fit", cx, |this, cx| {
                                this.zoom_inspector(None, None);
                                cx.notify();
                            }))
                            .child(self.inspector_button(
                                "image-actual-size",
                                "100%",
                                cx,
                                |this, cx| {
                                    if let Some(inspector) = &mut this.image_inspector {
                                        inspector.view.actual_size();
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(self.inspector_button("image-zoom-in", "+", cx, |this, cx| {
                                this.zoom_inspector(Some(1.25), None);
                                cx.notify();
                            }))
                            .child(self.inspector_button(
                                "image-copy",
                                "Copy",
                                cx,
                                Self::copy_inspector_image,
                            ))
                            .child(self.inspector_button(
                                "image-save",
                                "Save",
                                cx,
                                Self::save_inspector_image,
                            ))
                            .child(
                                div()
                                    .id("close-image-inspector")
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .child("Close")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_overlay();
                                        window.focus(&this.focus_handle, cx);
                                        cx.stop_propagation();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .overflow_hidden()
                            .child(image_canvas),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_3()
                            .py_2()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(status),
                    ),
            )
            .into_any_element()
    }

    fn inspector_button(
        &self,
        id: &'static str,
        label: &'static str,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_h(px(44.0))
            .min_w(px(44.0))
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(color(colors.surface_hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                action(this, cx);
                cx.stop_propagation();
            }))
            .child(label)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_never_enlarges_and_zoom_and_pan_stay_bounded() {
        let mut view = ZoomView::default();
        view.viewport.set(Some(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(400.0), px(300.0)),
        )));
        // Smaller than the box: fitted at its own size.
        view.set_natural(size(100.0, 50.0));
        assert_eq!(view.scale(), Some(1.0));
        // Larger: fitted to the tighter side.
        view.set_natural(size(800.0, 1200.0));
        assert_eq!(view.scale(), Some(0.25));
        view.zoom(Some(1000.0), None);
        assert_eq!(view.scale(), Some(16.0));
        view.zoom(Some(0.0), None);
        assert_eq!(view.scale(), Some(0.05));
        view.actual_size();
        // At 100% the image overhangs 200 px each side across, 450 down: a pan stops there.
        view.pan = point(px(5000.0), px(-5000.0));
        view.clamp_pan(view.viewport.get().unwrap());
        assert_eq!(view.pan, point(px(200.0), px(-450.0)));
        // A new image starts fitted and centred.
        view.set_natural(size(10.0, 10.0));
        assert_eq!(
            (view.scale(), view.pan),
            (Some(1.0), point(px(0.0), px(0.0)))
        );
    }
}

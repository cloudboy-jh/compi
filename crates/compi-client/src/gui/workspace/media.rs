use super::*;
use crate::image_input::{self, ImageInput, ImageTarget, PreparedImage};
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
    zoom: Option<f32>,
    pan: Point<Pixels>,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    viewport: Rc<ViewportCell<Option<Bounds<Pixels>>>>,
}

impl InspectorState {
    fn scale(&self, bounds: Bounds<Pixels>) -> f32 {
        self.zoom.unwrap_or_else(|| {
            let Some(image) = &self.image else { return 1.0 };
            (f32::from(bounds.size.width) / image.size(0).width.0.max(1) as f32)
                .min(f32::from(bounds.size.height) / image.size(0).height.0.max(1) as f32)
                .min(1.0)
        })
    }

    fn clamp_pan(&mut self, bounds: Bounds<Pixels>) {
        let Some(image) = &self.image else { return };
        let scale = self.scale(bounds);
        let x =
            ((image.size(0).width.0 as f32 * scale - f32::from(bounds.size.width)) / 2.0).max(0.0);
        let y = ((image.size(0).height.0 as f32 * scale - f32::from(bounds.size.height)) / 2.0)
            .max(0.0);
        self.pan.x = px(f32::from(self.pan.x).clamp(-x, x));
        self.pan.y = px(f32::from(self.pan.y).clamp(-y, y));
    }
}

enum MediaJob {
    Prepare {
        request: u64,
        origin: InputOrigin,
        input: ImageInput,
        target: ImageTarget,
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
                        sender,
                    } => {
                        let result = image_input::prepare(input, target);
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
        #[cfg(windows)]
        let target = {
            let surface = workspace.surface(&view.surface_id);
            let distribution = surface
                .and_then(|surface| surface.working_directory.as_ref())
                .map(|cwd| cwd.distribution.clone())
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    surface
                        .and_then(|surface| surface.launch.profile.as_ref())
                        .and_then(|profile| profile.distribution.clone())
                });
            ImageTarget::Wsl { distribution }
        };
        #[cfg(target_os = "macos")]
        let target = ImageTarget::Native;
        let request = NEXT_IMAGE_REQUEST.fetch_add(1, Ordering::Relaxed);
        let job = MediaJob::Prepare {
            request,
            origin,
            input,
            target,
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
        let input = format!("{} ", image.quoted_path);
        view.send(ClientMessage::Input {
            data: crate::input::encode_paste(&input, bracketed),
            latency_id: None,
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
            zoom: None,
            pan: point(px(0.0), px(0.0)),
            drag: None,
            viewport: Rc::new(ViewportCell::new(None)),
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
                Ok(image) => inspector.image = Some(image),
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
        let Some(inspector) = &mut self.image_inspector else {
            return;
        };
        let Some(bounds) = inspector.viewport.get() else {
            return;
        };
        let old = inspector.scale(bounds);
        inspector.zoom = factor.map(|factor| (old * factor).clamp(0.05, 16.0));
        if let Some(anchor) = anchor {
            let ratio = inspector.scale(bounds) / old.max(0.0001);
            let dx = anchor.x - bounds.center().x;
            let dy = anchor.y - bounds.center().y;
            inspector.pan = point(
                dx - (dx - inspector.pan.x) * ratio,
                dy - (dy - inspector.pan.y) * ratio,
            );
        } else {
            inspector.pan = point(px(0.0), px(0.0));
        }
        inspector.clamp_pan(bounds);
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
                window.focus(&self.focus_handle);
            }
            "+" | "plus" | "=" | "equal" => self.zoom_inspector(Some(1.25), None),
            "-" | "minus" => self.zoom_inspector(Some(0.8), None),
            "0" => self.zoom_inspector(None, None),
            "1" => {
                if let Some(inspector) = &mut self.image_inspector {
                    inspector.zoom = Some(1.0);
                    inspector.pan = point(px(0.0), px(0.0));
                }
            }
            "c" if key.modifiers.control || key.modifiers.platform => self.copy_inspector_image(cx),
            "s" if key.modifiers.control || key.modifiers.platform => self.save_inspector_image(cx),
            _ => {}
        }
        cx.notify();
        true
    }

    pub(super) fn render_image_previews(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
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
                                    .text_color(color(modal_text_color(colors.muted, colors)))
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
            .absolute()
            .right(px(12.0))
            .bottom(px(12.0))
            .max_w(gpui::relative(0.9))
            .max_h(gpui::relative(0.32))
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
        let colors = self.colors();
        let viewport = inspector.viewport.clone();
        let entity = cx.entity();
        let image = inspector.image.clone();
        let zoom = inspector.zoom;
        let pan = inspector.pan;
        let error = inspector.error.clone();
        let image_canvas = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let scale = image.as_ref().map_or(1.0, |image| {
                    zoom.unwrap_or_else(|| {
                        (f32::from(bounds.size.width) / image.size(0).width.0.max(1) as f32)
                            .min(
                                f32::from(bounds.size.height)
                                    / image.size(0).height.0.max(1) as f32,
                            )
                            .min(1.0)
                    })
                });
                viewport.set(Some(bounds));
                if let Some(image) = &image {
                    let dimensions = size(
                        px(image.size(0).width.0 as f32 * scale),
                        px(image.size(0).height.0 as f32 * scale),
                    );
                    let origin = point(
                        bounds.center().x - dimensions.width / 2.0 + pan.x,
                        bounds.center().y - dimensions.height / 2.0 + pan.y,
                    );
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        let _ = window.paint_image(
                            Bounds::new(origin, dimensions),
                            Corners::default(),
                            image.clone(),
                            0,
                            false,
                        );
                    });
                }
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |event: &MouseDownEvent, _, _, cx| {
                        if event.button == MouseButton::Left && bounds.contains(&event.position) {
                            entity.update(cx, |this, cx| {
                                if let Some(inspector) = &mut this.image_inspector {
                                    inspector.drag = Some((event.position, inspector.pan));
                                }
                                cx.stop_propagation();
                            });
                        }
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |event: &MouseMoveEvent, _, _, cx| {
                        entity.update(cx, |this, cx| {
                            if let Some(inspector) = &mut this.image_inspector
                                && let Some((origin, pan)) = inspector.drag
                            {
                                if event.dragging() {
                                    inspector.pan = point(
                                        pan.x + event.position.x - origin.x,
                                        pan.y + event.position.y - origin.y,
                                    );
                                    inspector.clamp_pan(bounds);
                                    cx.stop_propagation();
                                    cx.notify();
                                } else {
                                    inspector.drag = None;
                                }
                            }
                        });
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |_: &MouseUpEvent, _, _, cx| {
                        entity.update(cx, |this, _| {
                            if let Some(inspector) = &mut this.image_inspector {
                                inspector.drag = None;
                            }
                        })
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |event: &ScrollWheelEvent, _, _, cx| {
                        if bounds.contains(&event.position) {
                            entity.update(cx, |this, cx| {
                                let delta = f32::from(event.delta.pixel_delta(px(32.0)).y);
                                this.zoom_inspector(
                                    Some((delta * 0.005).exp()),
                                    Some(event.position),
                                );
                                cx.stop_propagation();
                                cx.notify();
                            });
                        }
                    }
                });
            },
        )
        .size_full();
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
            .text_color(color(modal_text_color(colors.foreground, colors)))
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
                                        inspector.zoom = Some(1.0);
                                        inspector.pan = point(px(0.0), px(0.0));
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
                                "Save…",
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
                                        window.focus(&this.focus_handle);
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
                            .text_color(color(modal_text_color(colors.muted, colors)))
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
        let colors = self.colors();
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

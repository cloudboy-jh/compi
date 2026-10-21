//! Markdown is another presentation of the same editor buffer, not another document.
use super::*;
use crate::notes::{self as model, Notebook};
use gpui::WeakEntity;
use gpui_base::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin,
};
use gpui_component::text::{TextView, TextViewState};
use markdown::mdast::Node;
use std::fmt::Write as _;

#[path = "note_render.rs"]
mod note_render;
use note_render::{Assets, Kind};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(in crate::gui) struct NotesUi {
    rendered: HashSet<String>,
    previews: HashMap<String, Preview>,
    root: String,
    notebook: Option<Notebook>,
    request: Option<u64>,
    error: Option<String>,
    panel: bool,
    focus_search: bool,
    searching: bool,
    search: Option<Entity<InputState>>,
    search_events: Option<Subscription>,
    scroll: gpui::UniformListScrollHandle,
    indexed: HashMap<String, Rope>,
    revision: u64,
    results_key: Option<(Rope, String, u64)>,
    results: Arc<Vec<model::SearchHit>>,
}

impl NotesUi {
    pub(in crate::gui) fn is_rendered(&self, path: &str) -> bool {
        self.rendered.contains(path)
    }
}

fn update_results(ui: &mut PaneUi, path: &str, cx: &App) {
    let PaneUi { files, notes, .. } = ui;
    let Some(book) = notes.notebook.as_mut() else {
        return;
    };
    for (path, file) in files {
        let FileBody::Ready(editor) = &file.body else {
            continue;
        };
        if !model::is_markdown(path)
            || crate::content_path::repo_relative(&book.root, path).is_none()
        {
            continue;
        }
        let text = editor.state.read(cx).text();
        if notes
            .indexed
            .get(path)
            .is_some_and(|indexed| indexed == text)
        {
            continue;
        }
        let source = text.to_string();
        let title = model::title(&source, path);
        match book.notes.iter_mut().find(|note| note.path == *path) {
            Some(note) => {
                note.text = source;
                note.title = title;
            }
            None => book.notes.push(model::Note {
                path: path.clone(),
                text: source,
                title,
            }),
        }
        notes.indexed.insert(path.clone(), text.clone());
        notes.revision += 1;
    }
    if !notes.panel {
        return;
    }
    let query = notes
        .search
        .as_ref()
        .map(|input| input.read(cx).text().clone())
        .unwrap_or_default();
    if notes
        .results_key
        .as_ref()
        .is_some_and(|(previous, active, revision)| {
            *previous == query && active == path && *revision == notes.revision
        })
    {
        return;
    }
    let query_text = query.to_string();
    notes.searching = !query_text.trim().is_empty();
    notes.results = Arc::new(if notes.searching {
        book.search(&query_text)
    } else {
        book.backlinks(path)
    });
    notes.results_key = Some((query, path.to_owned(), notes.revision));
    notes.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
}

struct Preview {
    original: Rope,
    state: Entity<TextViewState>,
    assets: Arc<Assets>,
    request: u64,
    dark: bool,
    foreground: u32,
}

pub(in crate::gui) enum NoteEvent {
    Loaded {
        pane: PaneId,
        request: u64,
        result: Result<Notebook, String>,
    },
    Prepared {
        pane: PaneId,
        path: String,
        request: u64,
        assets: Assets,
    },
}

/// Links encode their original task offset, so preview rewrites never change the edit target.
fn preview_source(source: &str) -> String {
    let mut edits: Vec<(std::ops::Range<usize>, String)> = model::wiki_links(source)
        .into_iter()
        .map(|link| {
            (
                link.range,
                format!(
                    "[{}](compi-note:{})",
                    escape_label(&link.label),
                    encode(&link.target)
                ),
            )
        })
        .collect();
    edits.extend(model::tasks(source).into_iter().map(|task| {
        (
            task.marker - 1..task.marker + 2,
            format!(
                "[{}](compi-task:{}:{})",
                if task.checked { "☑" } else { "☐" },
                task.marker,
                u8::from(task.checked)
            ),
        )
    }));
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(source.len());
    let mut at = 0;
    for (range, replacement) in edits {
        if range.start < at {
            continue;
        }
        out.push_str(&source[at..range.start]);
        out.push_str(&replacement);
        at = range.end;
    }
    out.push_str(&source[at..]);
    out
}

fn escape_label(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '[' | ']' | '*' | '_' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn decode(text: &str) -> Option<String> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

impl CompiApp {
    pub(in crate::gui) fn set_note_mode(
        &mut self,
        pane: &PaneId,
        path: &str,
        rendered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !model::is_markdown(path) {
            return;
        }
        if let Some(ui) = self.content.panes.get_mut(pane) {
            if rendered {
                ui.notes.rendered.insert(path.to_owned());
            } else {
                ui.notes.rendered.remove(path);
            }
            if rendered {
                self.content.released = Some(pane.clone());
                window.focus(&ui.focus, cx);
            } else {
                self.content.released = None;
                if let Some(editor) = ui.editor(path) {
                    window.focus(&editor.state.focus_handle(cx), cx);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn prepare_note_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown: Vec<_> = self
            .shown_content_panes()
            .filter_map(|pane| {
                let content = find_content(self.workspace.as_ref()?, pane)?;
                Some((
                    pane.clone(),
                    content.root.clone(),
                    content.active_file()?.to_owned(),
                ))
            })
            .collect();
        let dark = self.theme.is_dark();
        let foreground = self.colors().foreground;
        for (pane, root, path) in shown {
            let Some(ui) = self.content.panes.get_mut(&pane) else {
                continue;
            };
            if ui.notes.root != root {
                ui.notes.root = root;
                ui.notes.notebook = None;
                ui.notes.request = None;
                ui.notes.error = None;
                ui.notes.indexed.clear();
                ui.notes.results = Arc::default();
                ui.notes.results_key = None;
            }
            ui.notes
                .previews
                .retain(|path, _| ui.files.contains_key(path));
            if !model::is_markdown(&path) {
                continue;
            }
            if ui.notes.panel && ui.notes.search.is_none() {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search notes…"));
                let events = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
                ui.notes.search = Some(input);
                ui.notes.search_events = Some(events);
            }
            if ui.notes.focus_search
                && let Some(input) = &ui.notes.search
            {
                window.focus(&input.focus_handle(cx), cx);
                ui.notes.focus_search = false;
            }
            let rendered = ui.notes.is_rendered(&path);
            if (rendered || ui.notes.panel)
                && ui.notes.notebook.is_none()
                && ui.notes.request.is_none()
                && ui.notes.error.is_none()
            {
                self.load_notes(&pane);
            }
            let Some(ui) = self.content.panes.get_mut(&pane) else {
                continue;
            };
            update_results(ui, &path, cx);
            if !rendered {
                continue;
            }
            let Some(editor) = ui.editor(&path) else {
                continue;
            };
            let original = editor.state.read(cx).text().clone();
            if ui.notes.previews.get(&path).is_some_and(|preview| {
                preview.original == original
                    && preview.dark == dark
                    && preview.foreground == foreground
            }) {
                continue;
            }
            let source = preview_source(&original.to_string());
            let state = match ui.notes.previews.get(&path) {
                Some(preview) => {
                    preview
                        .state
                        .update(cx, |state, cx| state.set_text(&source, cx));
                    preview.state.clone()
                }
                None => cx.new(|cx| TextViewState::markdown(&source, cx)),
            };
            let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
            let known = ui
                .notes
                .previews
                .get(&path)
                .filter(|preview| preview.dark == dark && preview.foreground == foreground)
                .map_or_else(|| Arc::new(Assets::new()), |preview| preview.assets.clone());
            ui.notes.previews.insert(
                path.clone(),
                Preview {
                    original,
                    state,
                    assets: known.clone(),
                    request,
                    dark,
                    foreground,
                },
            );
            let sender = self.event_tx.clone();
            let pane = pane.clone();
            cx.background_executor()
                .spawn(async move {
                    let assets = note_render::prepare(&source, dark, foreground, &known);
                    sender.send(UiEvent::Notes(NoteEvent::Prepared {
                        pane,
                        path,
                        request,
                        assets,
                    }));
                })
                .detach();
        }
    }

    fn load_notes(&mut self, pane: &PaneId) {
        let Some(ui) = self.content.panes.get_mut(pane) else {
            return;
        };
        if ui.notes.request.is_some() {
            return;
        }
        let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        ui.notes.request = Some(request);
        ui.notes.error = None;
        let root = ui.notes.root.clone();
        let environment = ui.environment.clone();
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        let pane = pane.clone();
        thread::spawn(move || {
            let result = target
                .connect()
                .and_then(|mut client| model::load(&mut client, environment, &root))
                .map_err(|e| e.to_string());
            sender.send(UiEvent::Notes(NoteEvent::Loaded {
                pane,
                request,
                result,
            }));
        });
    }

    pub(in crate::gui) fn note_event(&mut self, event: NoteEvent, cx: &mut Context<Self>) {
        match event {
            NoteEvent::Loaded {
                pane,
                request,
                result,
            } => {
                let Some(ui) = self
                    .content
                    .panes
                    .get_mut(&pane)
                    .filter(|ui| ui.notes.request == Some(request))
                else {
                    return;
                };
                ui.notes.request = None;
                match result {
                    Ok(notebook) => {
                        ui.notes.notebook = Some(notebook);
                        ui.notes.indexed.clear();
                        ui.notes.revision += 1;
                    }
                    Err(error) => ui.notes.error = Some(error),
                }
            }
            NoteEvent::Prepared {
                pane,
                path,
                request,
                assets,
            } => {
                let Some(preview) = self
                    .content
                    .panes
                    .get_mut(&pane)
                    .and_then(|ui| ui.notes.previews.get_mut(&path))
                    .filter(|preview| preview.request == request)
                else {
                    return;
                };
                preview.assets = Arc::new(assets);
                preview
                    .state
                    .update(cx, |state, cx| state.invalidate_inline_layout(cx));
            }
        }
        cx.notify();
    }

    fn toggle_note_task(
        &mut self,
        pane: &PaneId,
        path: &str,
        original: &Rope,
        marker: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get(pane) else {
            return;
        };
        let Some(editor) = ui.editor(path) else {
            return;
        };
        if !editor.writable || editor.saving.is_some() || editor.state.read(cx).text() != original {
            return;
        }
        let source = original.to_string();
        let Ok(toggled) = model::toggle_task(&source, marker) else {
            return;
        };
        let state = editor.state.clone();
        state.update(cx, |state, cx| {
            let selection = state.cursor_position();
            let scroll = state.scroll_offset();
            state.set_selected_range(marker..marker + 1, cx);
            state.replace(toggled[marker..marker + 1].to_owned(), window, cx);
            let at = clamped_offset(state.text(), selection);
            state.set_selected_range(at..at, cx);
            state.set_scroll_offset(scroll, cx);
        });
        // Programmatic Replace is silent; share dirty/backup/gutter bookkeeping with typing.
        self.editor_changed(pane, &state, cx);
        let Some(editor) = self.content.panes.get(pane).and_then(|ui| ui.editor(path)) else {
            return;
        };
        let expected = save_expected(&editor.disk, &editor.version);
        self.write_content(pane, path, path.to_owned(), Expect::Exactly(expected), cx);
        cx.notify();
    }

    fn open_note_target(
        &mut self,
        pane: &PaneId,
        from: &str,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get(pane) else {
            return;
        };
        let result = ui
            .notes
            .notebook
            .as_ref()
            .ok_or_else(|| "Notes are still loading; try the link again".to_owned())
            .and_then(|book| book.resolve(from, target));
        match result {
            Ok(path) => self.open_note_hit(pane, &path, 1, true, window, cx),
            Err(error) => {
                if let Some(ui) = self.content.panes.get_mut(pane) {
                    ui.notice = Some((from.to_owned(), error));
                }
            }
        }
    }

    fn open_note_hit(
        &mut self,
        pane: &PaneId,
        path: &str,
        line: u32,
        rendered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane))
            .cloned()
        else {
            return;
        };
        self.open_content_file(
            pane,
            content.environment,
            Some(content.root),
            Some(path.to_owned()),
            Some(line),
            false,
            window,
            cx,
        );
        self.set_note_mode(pane, path, rendered, window, cx);
    }

    fn notes_panel(&mut self, pane: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ui) = self.content.panes.get_mut(pane) {
            ui.notes.panel = !ui.notes.panel;
            ui.notes.focus_search = ui.notes.panel;
            if !ui.notes.panel {
                window.focus(&ui.focus, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn note_key(
        &mut self,
        pane: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key.key == "escape"
            && !key.modifiers.control
            && !key.modifiers.platform
            && !key.modifiers.alt
            && let Some(ui) = self.content.panes.get_mut(pane).filter(|ui| ui.notes.panel)
        {
            ui.notes.panel = false;
            window.focus(&ui.focus, cx);
            return true;
        }
        let rendered = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane))
            .and_then(|content| content.active_file())
            .is_some_and(|path| {
                self.content
                    .panes
                    .get(pane)
                    .is_some_and(|ui| ui.notes.is_rendered(path))
            });
        if rendered && is_save_key(key) {
            self.save_focused_file(cx);
            return true;
        }
        if rendered
            && key.key.eq_ignore_ascii_case("f")
            && (key.modifiers.control || key.modifiers.platform)
            && !key.modifiers.alt
            && !key.modifiers.shift
        {
            if let Some(ui) = self.content.panes.get_mut(pane) {
                ui.notes.panel = true;
                ui.notes.focus_search = true;
            }
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn render_note_body(
        &self,
        pane: &PaneId,
        path: &str,
        ui: &PaneUi,
        editor_body: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let (toggle_pane, refresh_pane) = (pane.clone(), pane.clone());
        let rendered = ui.notes.is_rendered(path);
        let body = if rendered {
            match ui.notes.previews.get(path) {
                Some(preview) => {
                    let plugin = NotePlugin {
                        app: cx.entity().downgrade(),
                        pane: pane.clone(),
                        path: path.to_owned(),
                        original: preview.original.clone(),
                        assets: preview.assets.clone(),
                        colors,
                        writable: ui
                            .editor(path)
                            .is_some_and(|editor| editor.writable && editor.saving.is_none()),
                    };
                    let link_plugin = plugin.clone();
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .px_3()
                        .py_2()
                        .child(
                            TextView::new(&preview.state)
                                .scrollable(true)
                                .selectable(true)
                                .markdown_extensions(extensions(plugin))
                                .on_link_click(move |url, _, window, cx| {
                                    link_plugin.open_link(url, window, cx)
                                })
                                .text_size(px(UI_BODY_TEXT_SIZE))
                                .text_color(color(colors.foreground))
                                .size_full(),
                        )
                        .into_any_element()
                }
                None => div()
                    .flex_1()
                    .p_3()
                    .child("Preparing Markdown…")
                    .into_any_element(),
            }
        } else {
            editor_body
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        text_button("notes-panel", "Notes")
                            .selected(ui.notes.panel)
                            .header()
                            .detail("Search this folder's Markdown files and show backlinks")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.notes_panel(&toggle_pane, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .overflow_hidden()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(ui.notes.root.clone()),
                    )
                    .when(ui.notes.panel, |row| {
                        row.child(
                            icon_button("notes-refresh", IconName::RefreshCw, "Refresh notes")
                                .disabled_because(ui.notes.request.map(|_| "Notes are loading"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.load_notes(&refresh_pane);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(body)
            .when(ui.notes.panel, |column| {
                column.child(self.render_note_panel(pane, path, ui, cx))
            })
            .into_any_element()
    }

    fn render_note_panel(
        &self,
        pane: &PaneId,
        _path: &str,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let hits = ui.notes.results.clone();
        let searching = ui.notes.searching;
        let title = if searching {
            "Search results"
        } else {
            "Backlinks"
        };
        let mut panel = div()
            .flex_none()
            .h(px(210.0))
            .min_w_0()
            .border_t_1()
            .border_color(color(colors.border))
            .flex()
            .flex_col()
            .px_3()
            .py_2()
            .gap_1();
        if let Some(input) = &ui.notes.search {
            panel = panel.child(Input::new(input).w_full());
        }
        panel = panel.child(
            div()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(format!("{title} · {}", hits.len())),
        );
        if let Some(error) = &ui.notes.error {
            panel = panel.child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(colors.error))
                    .child(error.clone()),
            );
        }
        if let Some(book) = &ui.notes.notebook {
            for warning in &book.warnings {
                panel = panel.child(
                    div()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(colors.muted))
                        .child(warning.clone()),
                );
            }
        }
        if ui.notes.request.is_some() {
            panel = panel.child("Reading notes…");
        } else if hits.is_empty() && ui.notes.error.is_none() {
            panel = panel.child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child(if searching {
                        "No matching notes."
                    } else {
                        "No notes link here yet. Use [[note name]] in another Markdown file."
                    }),
            );
        }
        let pane = pane.clone();
        panel
            .child(
                gpui::uniform_list(
                    "notes-results",
                    hits.len(),
                    cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let hit = &hits[index];
                                let (pane, path, line) = (pane.clone(), hit.path.clone(), hit.line);
                                div().h(px(26.0)).w_full().overflow_hidden().child(
                                    text_button(
                                        ("note-result", index),
                                        format!("{}:{}  {}", file_name(&hit.path), line, hit.text),
                                    )
                                    .ghost()
                                    .detail(hit.path.clone())
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.open_note_hit(
                                                &pane, &path, line, false, window, cx,
                                            )
                                        },
                                    )),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .min_h_0()
                .track_scroll(&ui.notes.scroll),
            )
            .into_any_element()
    }
}

#[derive(Clone)]
struct NotePlugin {
    app: WeakEntity<CompiApp>,
    pane: PaneId,
    path: String,
    original: Rope,
    assets: Arc<Assets>,
    colors: crate::theme::ThemeColors,
    writable: bool,
}

fn extensions(plugin: NotePlugin) -> MarkdownExtensions {
    MarkdownExtensions::default()
        .plugin(BlockPlugin(plugin.clone()))
        .plugin(plugin)
        .parser_revision(1)
}

impl NotePlugin {
    fn open_link(&self, url: &str, window: &mut Window, cx: &mut App) {
        if let Some(target) = url.strip_prefix("compi-note:").and_then(decode) {
            let _ = self.app.update(cx, |app, cx| {
                app.open_note_target(&self.pane, &self.path, &target, window, cx)
            });
        } else if url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("mailto:")
        {
            cx.open_url(url);
        } else if !url.contains(':') && !url.starts_with('#') {
            let _ = self.app.update(cx, |app, cx| {
                app.open_note_target(&self.pane, &self.path, url, window, cx)
            });
        }
    }

    fn resource(&self, key: &(Kind, String), inline: bool) -> AnyElement {
        let source = &key.1;
        match self.assets.get(key) {
            Some(Ok(asset)) => {
                let scale = if inline {
                    1.0
                } else {
                    (880.0 / asset.width).min(1.0)
                };
                gpui::img(asset.image.clone())
                    .w(px(asset.width * scale))
                    .h(px(asset.height * scale))
                    .into_any_element()
            }
            result => div()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(self.colors.muted))
                .child(match result {
                    Some(Err(error)) => format!("{source}\nCannot render: {error}"),
                    _ => format!("{source}\nRendering…"),
                })
                .into_any_element(),
        }
    }
}

impl MarkdownPlugin for NotePlugin {
    fn name(&self) -> &str {
        "compi-inline"
    }
    fn parse(&self, node: &Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        match node {
            Node::InlineMath(math) => Some(
                MarkdownNode::new(self.name(), (Kind::InlineMath, math.value.clone()))
                    .text(math.value.clone()),
            ),
            Node::Link(link) if link.url.starts_with("compi-task:") => {
                let (marker, checked) = link.url.strip_prefix("compi-task:")?.split_once(':')?;
                Some(
                    MarkdownNode::new(self.name(), (marker.parse::<usize>().ok()?, checked == "1"))
                        .text(if checked == "1" { "☑" } else { "☐" }),
                )
            }
            _ => None,
        }
    }
    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        if let Some((marker, checked)) = node.data::<(usize, bool)>() {
            let (app, pane, path, original, marker) = (
                self.app.clone(),
                self.pane.clone(),
                self.path.clone(),
                self.original.clone(),
                *marker,
            );
            Some(
                InlineElement::new(
                    controls::checkbox(("note-task", marker), "", *checked)
                        .tooltip("Write this task's state to the Markdown file")
                        .disabled_because(
                            (!self.writable)
                                .then_some("This note is read-only or a save is in progress"),
                        )
                        .on_toggle(move |_, window, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.toggle_note_task(&pane, &path, &original, marker, window, cx)
                            });
                        }),
                )
                .with_baseline(px(15.0)),
            )
        } else {
            let key = node.data::<(Kind, String)>()?;
            let asset = self.assets.get(key);
            let scale = f32::from(context.font_size()) / 16.0;
            if let Some(Ok(asset)) = asset {
                Some(
                    InlineElement::new(
                        gpui::img(asset.image.clone())
                            .w(px(asset.width * scale))
                            .h(px(asset.height * scale)),
                    )
                    .with_baseline(px(asset.height * scale * 0.78)),
                )
            } else {
                Some(InlineElement::new(self.resource(key, true)))
            }
        }
    }
}

#[derive(Clone)]
struct BlockPlugin(NotePlugin);
impl MarkdownPlugin for BlockPlugin {
    fn name(&self) -> &str {
        "compi-block"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let data = match node {
            Node::Code(code) if code.lang.as_deref() == Some("mermaid") => {
                (Kind::Mermaid, code.value.clone())
            }
            Node::Math(math) => (Kind::DisplayMath, math.value.clone()),
            Node::Blockquote(_) => {
                let source = cx.node_source(node)?;
                let mut lines = source.lines().map(|line| {
                    line.trim_start()
                        .strip_prefix('>')
                        .unwrap_or(line)
                        .strip_prefix(' ')
                        .unwrap_or_else(|| line.trim_start().strip_prefix('>').unwrap_or(line))
                });
                let first = lines.next()?;
                let rest = first.strip_prefix("[!")?;
                let end = rest.find(']')?;
                let kind = &rest[..end];
                if kind.is_empty()
                    || !kind
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                {
                    return None;
                }
                let custom_title = rest[end + 1..].trim().trim_start_matches(['+', '-']).trim();
                let title = if custom_title.is_empty() {
                    kind.to_owned()
                } else {
                    custom_title.to_owned()
                };
                let body = lines.collect::<Vec<_>>().join("\n");
                return Some(MarkdownNode::new(self.name(), (title, body)).text(source.to_owned()));
            }
            _ => return None,
        };
        Some(MarkdownNode::new(self.name(), data.clone()).text(data.1))
    }
    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        if let Some(key) = node.data::<(Kind, String)>() {
            div()
                .id("note-resource")
                .w_full()
                .overflow_x_scroll()
                .py_2()
                .child(self.0.resource(key, false))
                .into_any_element()
        } else if let Some((title, body)) = node.data::<(String, String)>() {
            let plugin = self.0.clone();
            let links = plugin.clone();
            div()
                .w_full()
                .p_3()
                .border_1()
                .border_color(color(plugin.colors.border))
                .rounded_sm()
                .bg(color(plugin.colors.surface))
                .child(
                    div()
                        .mb_2()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.clone()),
                )
                .child(
                    TextView::markdown("callout-content", body.clone())
                        .markdown_extensions(extensions(plugin))
                        .on_link_click(move |url, _, window, cx| links.open_link(url, window, cx)),
                )
                .into_any_element()
        } else {
            div().child(node.as_text().to_owned()).into_any_element()
        }
    }
}

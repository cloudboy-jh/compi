use super::*;
use gpui::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TREE_REQUEST: AtomicU64 = AtomicU64::new(1);

impl CompiApp {
    pub(super) fn open_file_tree(&mut self) {
        self.open_file_tree_with_shell(false);
    }

    pub(super) fn open_file_tree_with_shell(&mut self, pending_shell: bool) {
        let Some(view) = self.focused_view() else {
            self.global_error = Some("Select a terminal pane to browse files".into());
            return;
        };
        let pane_id = view.pane_id.clone();
        let surface_id = view.surface_id.clone();
        let reported = view
            .mirror
            .snapshot()
            .and_then(|snapshot| snapshot.current_directory.clone());
        let surface = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(&surface_id));
        let root = reported.or_else(|| {
            surface.and_then(|surface| {
                surface
                    .working_directory
                    .as_ref()
                    .map(|cwd| cwd.resolved_wsl_path.clone())
                    .or_else(|| surface.launch.working_directory.clone())
                    .or_else(|| {
                        surface
                            .launch
                            .profile
                            .as_ref()
                            .and_then(|profile| profile.working_directory.clone())
                    })
            })
        });
        let Some(root) = root.filter(|root| root.starts_with('/')) else {
            self.global_error = Some(
                "The shell has not reported its current directory. Enable OSC 7 shell integration to browse from this pane.".into(),
            );
            if pending_shell {
                self.reply_to_shell(&pane_id, None);
            }
            return;
        };
        if self.file_tree.is_some() {
            self.close_file_tree();
        }
        let mut tree = tree::FileTree::new(pane_id.clone(), root.clone());
        tree.pending_shell = pending_shell;
        tree.request = NEXT_TREE_REQUEST.fetch_add(1, Ordering::Relaxed);
        let request = tree.request;
        self.report_focus(false);
        self.file_tree = Some(tree);
        self.tree_scroll.set_offset(point(px(0.0), px(0.0)));
        self.request_tree_directory(pane_id, surface_id, root, request);
    }

    pub(super) fn reply_to_shell(&mut self, pane_id: &PaneId, path: Option<&str>) {
        let data = path
            .map(|path| {
                let mut encoded = base64::engine::general_purpose::STANDARD
                    .encode(path.as_bytes())
                    .into_bytes();
                encoded.push(b'\n');
                encoded
            })
            .unwrap_or_else(|| vec![b'\n']);
        if let Some(view) = self
            .surface_views
            .iter_mut()
            .find(|view| &view.pane_id == pane_id)
        {
            view.send(ClientMessage::Input {
                data,
                latency_id: None,
            });
        }
    }

    pub(super) fn close_file_tree(&mut self) {
        if let Some(tree) = self.file_tree.take() {
            if tree.pending_shell {
                self.reply_to_shell(&tree.pane_id, None);
            }
            self.report_focus(true);
        }
    }

    fn enter_tree_directory(&mut self) {
        let Some(tree) = self.file_tree.as_ref() else {
            return;
        };
        if !tree.pending_shell {
            let command = if tree.project_mode {
                "compi z"
            } else {
                "compi tree"
            };
            self.global_error = Some(format!(
                "To change this shell's directory, run {command} from its prompt."
            ));
            return;
        }
        let Some(path) = tree
            .selected()
            .filter(|row| row.directory)
            .map(|row| row.path.clone())
        else {
            return;
        };
        let pane_id = tree.pane_id.clone();
        self.file_tree = None;
        self.reply_to_shell(&pane_id, Some(&path));
        self.report_focus(true);
    }

    pub(super) fn open_project_jump(&mut self) {
        self.open_project_jump_with_shell(false);
    }

    pub(super) fn open_project_jump_with_shell(&mut self, pending_shell: bool) {
        let Some(view) = self.focused_view() else {
            self.global_error = Some("Select a terminal pane to jump to a project".into());
            return;
        };
        let pane_id = view.pane_id.clone();
        let current = view
            .mirror
            .snapshot()
            .and_then(|snapshot| snapshot.current_directory.clone());
        if let Some(path) = current.as_deref()
            && self.state.project_history.record(path)
        {
            self.save_state();
        }
        if self.file_tree.is_some() {
            self.close_file_tree();
        }
        let mut picker = tree::FileTree::new(pane_id, current.unwrap_or_else(|| "/".into()));
        picker.project_mode = true;
        picker.pending_shell = pending_shell;
        self.report_focus(false);
        self.file_tree = Some(picker);
        self.tree_scroll.set_offset(point(px(0.0), px(0.0)));
        self.refresh_project_matches();
    }

    fn refresh_project_matches(&mut self) {
        let Some(picker) = self.file_tree.as_mut().filter(|picker| picker.project_mode) else {
            return;
        };
        let paths = self
            .state
            .project_history
            .search(&picker.query)
            .into_iter()
            .map(|path| (path.to_owned(), true))
            .collect();
        picker.replace_search_results(paths);
        self.tree_scroll.scroll_to_item(picker.selected);
    }

    pub(in crate::gui) fn append_tree_text(&mut self, text: &str) {
        if text.is_empty() || text.chars().any(char::is_control) {
            return;
        }
        let Some(tree) = self.file_tree.as_mut() else {
            return;
        };
        if tree.project_mode {
            tree.query.push_str(text);
            self.refresh_project_matches();
        } else if tree.searching {
            tree.query.push_str(text);
            self.request_tree_search();
        }
    }

    fn request_tree_directory(
        &self,
        pane_id: PaneId,
        surface_id: SurfaceId,
        path: String,
        request: u64,
    ) {
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target.connect()?.list_directory(surface_id, path.clone())
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::TreeListed {
                pane_id,
                request,
                parent: path,
                result,
            });
        });
    }

    fn request_tree_search(&mut self) {
        let Some(tree) = self.file_tree.as_mut() else {
            return;
        };
        let request = NEXT_TREE_REQUEST.fetch_add(1, Ordering::Relaxed);
        tree.search_request = request;
        tree.search_results.clear();
        tree.selected = 0;
        if tree.query.is_empty() {
            return;
        }
        let pane_id = tree.pane_id.clone();
        let Some(surface_id) = self
            .surface_views
            .iter()
            .find(|view| view.pane_id == pane_id)
            .map(|view| view.surface_id.clone())
        else {
            return;
        };
        let root = tree.rows[0].path.clone();
        let query = tree.query.clone();
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target.connect()?.search_directory(surface_id, root, query)
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::TreeSearched {
                pane_id,
                request,
                result,
            });
        });
    }

    pub(super) fn tree_listed(
        &mut self,
        pane_id: PaneId,
        request: u64,
        parent: String,
        result: Result<Vec<compi_protocol::DirectoryEntry>, String>,
    ) {
        let Some(tree) = self
            .file_tree
            .as_mut()
            .filter(|tree| tree.pane_id == pane_id && tree.request == request)
        else {
            return;
        };
        match result {
            Ok(entries) => {
                tree.error = None;
                tree.replace_children(
                    &parent,
                    entries
                        .into_iter()
                        .map(|entry| tree::Entry {
                            name: entry.name,
                            directory: entry.is_directory,
                        })
                        .collect(),
                );
            }
            Err(error) => {
                if let Some(row) = tree.rows.iter_mut().find(|row| row.path == parent) {
                    row.loading = false;
                    row.expanded = false;
                }
                tree.error = Some(error);
            }
        }
    }

    pub(super) fn tree_searched(
        &mut self,
        pane_id: PaneId,
        request: u64,
        result: Result<Vec<compi_protocol::SearchEntry>, String>,
    ) {
        let Some(tree) = self.file_tree.as_mut().filter(|tree| {
            tree.pane_id == pane_id && tree.search_request == request && !tree.query.is_empty()
        }) else {
            return;
        };
        match result {
            Ok(entries) => {
                tree.error = None;
                tree.replace_search_results(
                    entries
                        .into_iter()
                        .map(|entry| (entry.path, entry.is_directory))
                        .collect(),
                );
            }
            Err(error) => {
                tree.error = Some(error);
            }
        }
    }

    fn scroll_to_selected_tree_row(&self) {
        if let Some(tree) = &self.file_tree {
            self.tree_scroll.scroll_to_item(tree.selected);
        }
    }

    fn toggle_tree_row(&mut self, index: usize) {
        let Some(tree) = self.file_tree.as_mut() else {
            return;
        };
        if tree.project_mode {
            tree.selected = index;
            self.enter_tree_directory();
            return;
        }
        if tree.searching && !tree.query.is_empty() {
            let Some(row) = tree.search_results.get(index).filter(|row| row.directory) else {
                return;
            };
            let path = row.path.clone();
            let pane_id = tree.pane_id.clone();
            let request = NEXT_TREE_REQUEST.fetch_add(1, Ordering::Relaxed);
            let pending_shell = tree.pending_shell;
            *tree = tree::FileTree::new(pane_id.clone(), path.clone());
            tree.pending_shell = pending_shell;
            tree.request = request;
            if let Some(surface_id) = self
                .surface_views
                .iter()
                .find(|view| view.pane_id == pane_id)
                .map(|view| view.surface_id.clone())
            {
                self.request_tree_directory(pane_id, surface_id, path, request);
            }
            return;
        }
        let Some(path) = tree.toggle(index) else {
            return;
        };
        let pane_id = tree.pane_id.clone();
        let request = tree.request;
        if let Some(surface_id) = self
            .surface_views
            .iter()
            .find(|view| view.pane_id == pane_id)
            .map(|view| view.surface_id.clone())
        {
            self.request_tree_directory(pane_id, surface_id, path, request);
        }
    }

    fn copy_tree_path(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .file_tree
            .as_ref()
            .and_then(|tree| tree.selected())
            .map(|row| row.path.clone())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    fn handle_project_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) {
        match key.key.as_str() {
            "escape" => self.close_file_tree(),
            "up" => self.file_tree.as_mut().unwrap().move_selection(-1),
            "down" => self.file_tree.as_mut().unwrap().move_selection(1),
            "enter" => {
                if self.file_tree.as_ref().unwrap().pending_shell {
                    self.enter_tree_directory();
                } else {
                    self.copy_tree_path(cx);
                }
            }
            "c" if key.modifiers.control || key.modifiers.platform => self.copy_tree_path(cx),
            "backspace" => {
                self.file_tree.as_mut().unwrap().query.pop();
                self.refresh_project_matches();
            }
            _ if !key.modifiers.control && !key.modifiers.platform && !key.modifiers.alt => {
                if let Some(text) = key.key_char.as_deref() {
                    self.append_tree_text(text);
                }
            }
            _ => {}
        }
        self.scroll_to_selected_tree_row();
        cx.notify();
    }

    pub(super) fn handle_tree_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(tree) = self.file_tree.as_ref() else {
            return false;
        };
        #[cfg(target_os = "macos")]
        if key.key_char.is_some()
            && !key.modifiers.control
            && !key.modifiers.platform
            && (tree.project_mode || tree.searching)
        {
            // AppKit supplies composed text and IME through EntityInputHandler.
            return false;
        }
        if tree.project_mode {
            self.handle_project_key(key, cx);
            return true;
        }
        let searching = tree.searching;
        match key.key.as_str() {
            "escape" => self.close_file_tree(),
            "up" => self.file_tree.as_mut().unwrap().move_selection(-1),
            "down" => self.file_tree.as_mut().unwrap().move_selection(1),
            "enter" if key.modifiers.control || key.modifiers.platform => {
                self.enter_tree_directory();
            }
            "right" | "enter" => {
                let tree = self.file_tree.as_ref().unwrap();
                let index = tree.selected;
                if tree.selected().is_some_and(|row| row.directory) {
                    self.toggle_tree_row(index);
                } else if key.key == "enter" {
                    self.copy_tree_path(cx);
                }
            }
            "left" if !searching => {
                let tree = self.file_tree.as_mut().unwrap();
                let selected = tree.selected;
                if tree.rows[selected].expanded {
                    tree.toggle(selected);
                } else if tree.rows[selected].depth > 0 {
                    tree.selected = (0..selected)
                        .rev()
                        .find(|index| tree.rows[*index].depth < tree.rows[selected].depth)
                        .unwrap_or(0);
                }
            }
            "backspace" if searching => {
                self.file_tree.as_mut().unwrap().query.pop();
                self.request_tree_search();
            }
            "c" if !searching || key.modifiers.control || key.modifiers.platform => {
                self.copy_tree_path(cx);
            }
            "/" if !searching => {
                let tree = self.file_tree.as_mut().unwrap();
                tree.searching = true;
                tree.query.clear();
                tree.selected = 0;
            }
            "f" if key.modifiers.control || key.modifiers.platform => {
                let tree = self.file_tree.as_mut().unwrap();
                tree.searching = true;
                tree.query.clear();
                tree.selected = 0;
            }
            _ if searching && !key.modifiers.control && !key.modifiers.platform => {
                if let Some(text) = key.key_char.as_deref() {
                    self.append_tree_text(text);
                }
            }
            _ => {}
        }
        self.scroll_to_selected_tree_row();
        cx.notify();
        true
    }

    pub(super) fn render_file_tree(&self, cx: &Context<Self>) -> AnyElement {
        let Some(tree) = self.file_tree.as_ref() else {
            return div().into_any_element();
        };
        let colors = self.colors();
        let can_enter = tree.pending_shell && tree.selected().is_some_and(|row| row.directory);
        let input = cx.entity();
        let focus = self.focus_handle.clone();
        let line_height = self.typography.cell_height.max(18.0);
        let mut continuing = [false; 32];
        let rows = tree.display_rows().iter().enumerate().map(|(index, row)| {
            let selected = index == tree.selected;
            let folder = row.directory;
            let depth = row.depth.min(31);
            let branch =
                (depth > 0 && !tree.project_mode && (!tree.searching || tree.query.is_empty()))
                    .then(|| {
                        let mut prefix = String::with_capacity(depth * 3);
                        for &has_next in continuing.iter().take(depth).skip(1) {
                            prefix.push_str(if has_next { "│  " } else { "   " });
                        }
                        prefix.push_str(if row.has_next_sibling {
                            "├─"
                        } else {
                            "└─"
                        });
                        prefix
                    });
            continuing[depth] = row.has_next_sibling;
            let label = if tree.project_mode {
                row.path.clone()
            } else if tree.searching && !tree.query.is_empty() {
                row.path
                    .strip_prefix(tree.rows[0].path.trim_end_matches('/'))
                    .unwrap_or(&row.path)
                    .trim_start_matches('/')
                    .to_owned()
            } else {
                row.name.clone()
            };
            div()
                .id(("tree-row", index))
                .h(px(line_height))
                .w_full()
                .px_2()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .bg(color(colors.background))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        if let Some(tree) = this.file_tree.as_mut() {
                            tree.selected = index;
                        }
                        this.scroll_to_selected_tree_row();
                        if folder && event.click_count >= 2 {
                            this.toggle_tree_row(index);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .when_some(branch, |entry, prefix| {
                    entry.child(div().text_color(color(colors.muted)).child(prefix))
                })
                .child(
                    div()
                        .h(px(line_height - 2.0))
                        .px_1()
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .when(selected, |entry| entry.bg(color(colors.selection)))
                        .when(!selected, |entry| {
                            entry.hover(move |style| style.bg(color(colors.surface_hover)))
                        })
                        .child(
                            div()
                                .w(px(20.0))
                                .flex_none()
                                .text_color(color(if selected {
                                    colors.foreground
                                } else {
                                    colors.muted
                                }))
                                .child(if folder {
                                    if row.expanded { "▾" } else { "▸" }
                                } else {
                                    ""
                                })
                                .when(folder, |arrow| {
                                    arrow.on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            if let Some(tree) = this.file_tree.as_mut() {
                                                tree.selected = index;
                                            }
                                            this.toggle_tree_row(index);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    )
                                }),
                        )
                        .child(tree_entry_icon(
                            folder,
                            row.expanded,
                            color(if selected {
                                colors.foreground
                            } else if folder {
                                colors.accent
                            } else {
                                colors.muted
                            }),
                        ))
                        .child(div().ml_1().child(label)),
                )
        });
        div()
            .id("file-tree")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(color(colors.background))
            .text_color(color(colors.foreground))
            .font_family(self.font_settings.family.clone())
            .text_size(px(self.typography.font_size))
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, input.clone()), cx);
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size(px(1.0)),
            )
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(color(colors.muted)).child(if tree.project_mode { "Jump to project".to_owned() } else { tree.rows[0].path.clone() }))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("tree-copy-path")
                                    .cursor_pointer()
                                    .text_color(color(colors.accent))
                                    .child("Copy path")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_tree_path(cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .when(can_enter, |actions| {
                                actions.child(
                                    div()
                                        .id("tree-enter-folder")
                                        .cursor_pointer()
                                        .text_color(color(colors.accent))
                                        .child("Enter folder")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.enter_tree_directory();
                                            cx.stop_propagation();
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(div().id("file-tree-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.tree_scroll).children(rows))
            .when(tree.project_mode && tree.search_results.is_empty(), |view| {
                view.child(div().px_2().py_1().text_color(color(colors.muted)).child("No visited directories match. Browse a project first."))
            })
            .when_some(tree.error.as_deref(), |view, error| view.child(div().px_2().text_color(color(colors.error)).child(error.to_owned())))
            .child(div().flex_none().px_2().py_1().text_color(color(colors.muted)).child(if tree.project_mode {
                format!("Jump: {}  ·  ↑↓ choose · Enter {} · Esc close", tree.query, if tree.pending_shell { "cd" } else { "copy path" })
            } else if tree.searching {
                format!("Search: {}  ·  ↑↓ choose · Enter expand/copy · Esc close", tree.query)
            } else if tree.pending_shell {
                "↑↓ select · Enter expand/copy · Ctrl+Enter enter folder · C copy path · / search · Esc close".to_owned()
            } else {
                "↑↓ select · Enter expand/copy · C copy path · / search · Esc close".to_owned()
            }))
            .into_any_element()
    }
}

fn tree_entry_icon(folder: bool, expanded: bool, tint: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.25));
            if folder {
                path.move_to(point(x(1.5), y(13.0)));
                path.line_to(point(x(1.5), y(4.0)));
                path.line_to(point(x(5.5), y(4.0)));
                path.line_to(point(x(7.0), y(6.0)));
                path.line_to(point(x(14.5), y(6.0)));
                if expanded {
                    path.line_to(point(x(14.5), y(8.0)));
                    path.move_to(point(x(3.0), y(8.0)));
                    path.line_to(point(x(14.5), y(8.0)));
                    path.line_to(point(x(12.5), y(13.0)));
                } else {
                    path.line_to(point(x(14.5), y(13.0)));
                }
                path.line_to(point(x(1.5), y(13.0)));
            } else {
                path.move_to(point(x(3.0), y(2.0)));
                path.line_to(point(x(10.0), y(2.0)));
                path.line_to(point(x(13.0), y(5.0)));
                path.line_to(point(x(13.0), y(14.0)));
                path.line_to(point(x(3.0), y(14.0)));
                path.line_to(point(x(3.0), y(2.0)));
                path.move_to(point(x(10.0), y(2.0)));
                path.line_to(point(x(10.0), y(5.0)));
                path.line_to(point(x(13.0), y(5.0)));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, tint);
            }
        },
    )
    .size(px(16.0))
}

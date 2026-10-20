//! The pickers drawn over the pane they serve: `compi z`'s Jump to project, which
//! answers the shell that asked, and Switch branch / Create branch (see `branches`).
//! Browsing folders is the Files view's (see `views`).
use super::*;
use gpui::prelude::*;

impl CompiApp {
    /// Answers the shell of `pane_id` waiting in `compi tree` or `compi z`: with
    /// `path`, a folder to change to; without, nothing, which cancels.
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
                alternate_scroll: false,
            });
        }
    }

    /// Closes the picker; Jump to project answers its shell with nothing.
    pub(super) fn close_file_tree(&mut self) {
        if let Some(tree) = self.file_tree.take() {
            if tree.project_mode {
                self.reply_to_shell(&tree.pane_id, None);
            }
            self.report_focus(true);
        }
    }

    /// Jump to project: the shell changes to the selected folder.
    fn enter_tree_directory(&mut self) {
        let Some(tree) = self.file_tree.as_ref().filter(|tree| tree.project_mode) else {
            return;
        };
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

    /// `compi z`: the shell's visited directories, searched as you type; Enter replies
    /// to the shell with the chosen one to cd into.
    pub(super) fn open_shell_project_jump(&mut self) {
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
        tree.query.push_str(text);
        if tree.branches.is_some() {
            self.refresh_branch_matches();
        } else {
            self.refresh_project_matches();
        }
    }

    fn scroll_to_selected_tree_row(&self) {
        if let Some(tree) = &self.file_tree {
            self.tree_scroll.scroll_to_item(tree.selected);
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
            "enter" => self.enter_tree_directory(),
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

    /// Keys while a picker is open: it takes them all.
    pub(super) fn handle_tree_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tree) = self.file_tree.as_ref() else {
            return false;
        };
        #[cfg(target_os = "macos")]
        if key.key_char.is_some() && !key.modifiers.control && !key.modifiers.platform {
            // AppKit supplies composed text and IME through EntityInputHandler.
            return false;
        }
        if tree.branches.is_some() {
            self.handle_branch_key(key, window, cx);
        } else {
            self.handle_project_key(key, cx);
        }
        true
    }

    /// The open picker, over its pane.
    pub(super) fn render_file_tree(&self, cx: &Context<Self>) -> AnyElement {
        let Some(tree) = self.file_tree.as_ref() else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let can_enter = tree.project_mode && tree.selected().is_some_and(|row| row.directory);
        let input = cx.entity();
        let focus = self.focus_handle.clone();
        let line_height = self.typography.cell_height.max(18.0);
        let rows = tree.display_rows().iter().enumerate().map(|(index, row)| {
            let selected = index == tree.selected;
            let folder = row.directory;
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
                        if event.click_count >= 2 && folder {
                            this.enter_tree_directory();
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
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
                        .child(tree_entry_icon(
                            folder,
                            false,
                            color(if selected {
                                colors.foreground
                            } else if folder {
                                colors.accent
                            } else {
                                colors.muted
                            }),
                        ))
                        .child(div().ml_1().child(row.path.clone())),
                )
        });
        let branch_rows = tree
            .branches
            .as_ref()
            .map(|picker| self.render_branch_rows(tree, picker, line_height, cx));
        let branch_note = tree.branches.as_ref().and_then(|picker| {
            if picker.listing.is_none() && tree.error.is_none() {
                Some("Reading the branches…".to_owned())
            } else if picker.listing.is_some() && picker.matches.is_empty() {
                Some(format!("No branch matches “{}”", tree.query.trim()))
            } else {
                None
            }
        });
        let title = match &tree.branches {
            Some(picker) if picker.create => format!("Create branch in {}", picker.repo),
            Some(picker) => format!("Switch branch in {}", picker.repo),
            None => "Jump to project".to_owned(),
        };
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
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, input.clone()),
                            cx,
                        );
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
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(color(colors.muted))
                            .child(title),
                    )
                    .when_some(tree.branches.as_ref(), |header, picker| {
                        header.child(self.render_branch_action(tree, picker, cx))
                    })
                    .when(tree.branches.is_none(), |header| {
                        header.child(
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
                        )
                    }),
            )
            .child(
                div()
                    .id("file-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.tree_scroll)
                    .map(|list| match branch_rows {
                        Some(branch_rows) => list.children(branch_rows),
                        None => list.children(rows),
                    }),
            )
            .when_some(branch_note, |view, note| {
                view.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(color(colors.muted))
                        .child(note),
                )
            })
            .when(
                tree.project_mode && tree.search_results.is_empty(),
                |view| {
                    view.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_color(color(colors.muted))
                            .child("No visited directories match. Browse a project first."),
                    )
                },
            )
            .when_some(tree.error.as_deref(), |view, error| {
                view.child(
                    div()
                        .px_2()
                        .text_color(color(colors.error))
                        .child(error.to_owned()),
                )
            })
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .text_color(color(colors.muted))
                    .child(
                        if let Some(write) = self
                            .branch_write
                            .as_ref()
                            .filter(|_| tree.branches.is_some())
                        {
                            write.label.clone()
                        } else if let Some(picker) = &tree.branches {
                            format!(
                                "Branch: {}  ·  type to filter · ↑↓ choose · Enter {} · Esc close",
                                tree.query,
                                if picker.create { "create" } else { "switch" }
                            )
                        } else {
                            format!("Jump: {}  ·  ↑↓ choose · Enter cd · Esc close", tree.query)
                        },
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn tree_entry_icon(folder: bool, expanded: bool, tint: Hsla) -> impl IntoElement {
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

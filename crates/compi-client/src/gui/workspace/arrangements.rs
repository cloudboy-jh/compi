//! Pane arrangement picker and commands. A tab's own panes are rearranged by one
//! `ArrangeTab` mutation; other tabs are combined into it by one `MergeTabs`
//! mutation. Both only move existing panes, and previews never resize a PTY.
use super::*;
use crate::arrangement::{self, Builtin, Preset, Shape, Transform};

const PREVIEW_WIDTH: f32 = 248.0;
const PREVIEW_HEIGHT: f32 = 156.0;

#[derive(Clone, PartialEq)]
pub(super) enum ArrangementChoice {
    /// The current tree (tabs side by side when merging), so mirror/flip can be
    /// previewed on it.
    Current,
    Builtin(Builtin),
    Named(String),
}

impl CompiApp {
    pub(super) fn open_arrangements(&mut self) {
        let Some(tab_id) = self.selected_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        self.refresh_layout_presets();
        let tab_rows = self.mergeable_tabs(&tab_id).len();
        self.open_overlay(
            Overlay::Arrangements {
                tab_id,
                transform: Transform::default(),
                confirm_delete: None,
                merge: Vec::new(),
            },
            "",
        );
        // Start on the first built-in rather than a tab row or the no-op current tree.
        self.overlay_index = tab_rows + 1;
    }

    /// Pick up presets saved by other windows or edited by hand.
    fn refresh_layout_presets(&mut self) {
        if self.config.path.is_file() {
            self.config.layout_presets = crate::config::load(
                Some(&self.config.path),
                crate::config::FontOverrides::default(),
            )
            .layout_presets;
        }
    }

    /// Other tabs this window shows in the same workspace, in tab order. Tabs
    /// hidden here (including tabs owned by another window) are never offered.
    fn mergeable_tabs(&self, tab_id: &TabId) -> Vec<&WorkspaceTab> {
        self.workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .find(|session| session.tabs.iter().any(|tab| &tab.id == tab_id))
            })
            .map(|session| {
                self.state
                    .visible_tabs(session)
                    .filter(|tab| &tab.id != tab_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The receiving tab, then each still-mergeable chosen tab, in tab order.
    fn arrangement_tabs(&self, tab_id: &TabId, merge: &[TabId]) -> Vec<&WorkspaceTab> {
        let Some(tab) = self.tab_by_id(tab_id) else {
            return Vec::new();
        };
        std::iter::once(tab)
            .chain(
                self.mergeable_tabs(tab_id)
                    .into_iter()
                    .filter(|tab| merge.contains(&tab.id)),
            )
            .collect()
    }

    pub(super) fn arrangement_choices(
        &self,
        tab_id: &TabId,
        merge: &[TabId],
        confirm_delete: Option<&str>,
    ) -> Vec<Choice> {
        let tabs = self.arrangement_tabs(tab_id, merge);
        let panes: usize = tabs.iter().map(|tab| leaf_count(&tab.layout)).sum();
        let mut choices = Vec::new();
        for tab in self.mergeable_tabs(tab_id) {
            let count = leaf_count(&tab.layout);
            let noun = if count == 1 { "pane" } else { "panes" };
            let included = merge.contains(&tab.id);
            choices.push(Choice {
                title: if included {
                    format!("✓ {}", self.tab_label(tab))
                } else {
                    self.tab_label(tab)
                },
                detail: if included {
                    format!("Combined into this tab · {count} {noun} · click to leave it out")
                } else {
                    format!("{count} {noun} · click to combine it into this tab")
                },
                group: Some("Tabs to combine"),
                reason: None,
                action: ChoiceAction::ArrangementTab(tab.id.clone()),
            });
        }
        let merging = tabs.len() > 1;
        choices.push(Choice {
            title: if merging {
                "Tabs side by side".into()
            } else {
                "Current arrangement".into()
            },
            detail: if merging {
                "Each tab keeps its splits; Mirror or Flip reorders them".into()
            } else {
                "Use Mirror or Flip to change it".into()
            },
            group: Some("Current"),
            reason: None,
            action: ChoiceAction::Arrangement(ArrangementChoice::Current),
        });
        for builtin in Builtin::ALL {
            let detail = match builtin {
                Builtin::Columns => "Side by side, equal widths",
                Builtin::Rows => "Stacked, equal heights",
                Builtin::Grid => "Rows of equal cells",
                Builtin::MainSide => "Focused pane large, the rest stacked beside it",
                Builtin::MainTop => "Focused pane large, the rest in a row below it",
                Builtin::Equalize if merging => "Tabs side by side, every split equal",
                Builtin::Equalize => "Keep the current splits; equal sizes",
            };
            choices.push(Choice {
                title: builtin.label().into(),
                detail: detail.into(),
                group: Some("Built-in"),
                reason: None,
                action: ChoiceAction::Arrangement(ArrangementChoice::Builtin(builtin)),
            });
        }
        for (name, shape) in &self.config.layout_presets {
            let slots = shape.slots();
            let fit = match slots.cmp(&panes) {
                std::cmp::Ordering::Equal => format!("{slots} panes"),
                std::cmp::Ordering::Greater => {
                    format!("{slots} panes; unused slots are dropped")
                }
                std::cmp::Ordering::Less => {
                    format!("{slots} panes; extra panes share the last slot")
                }
            };
            choices.push(Choice {
                title: name.clone(),
                detail: if confirm_delete == Some(name.as_str()) {
                    "Press Delete again to remove this preset".into()
                } else {
                    format!("{fit} · Delete removes")
                },
                group: Some("Saved presets"),
                reason: None,
                action: ChoiceAction::Arrangement(ArrangementChoice::Named(name.clone())),
            });
        }
        choices
    }

    fn arrangement_candidate(
        &self,
        tab_id: &TabId,
        merge: &[TabId],
        choice: &ArrangementChoice,
        transform: Transform,
    ) -> Option<LayoutNode> {
        let tabs = self.arrangement_tabs(tab_id, merge);
        let layouts: Vec<_> = tabs.iter().map(|tab| &tab.layout).collect();
        let current = arrangement::combine(&layouts)?;
        let main = self
            .focused_view()
            .map(|view| &view.pane_id)
            .filter(|pane| layout_contains_pane(&current, pane));
        Some(match choice {
            ArrangementChoice::Current => arrangement::apply_transform(current, transform),
            ArrangementChoice::Builtin(builtin) => {
                arrangement::arrange(&current, Preset::Builtin(*builtin), main, transform)
            }
            ArrangementChoice::Named(name) => arrangement::arrange(
                &current,
                Preset::Named(self.config.layout_presets.get(name)?),
                main,
                transform,
            ),
        })
    }

    fn selected_arrangement(&self, cx: &App) -> Option<ArrangementChoice> {
        match self
            .overlay_choices(cx)
            .get(self.overlay_index)?
            .action
            .clone()
        {
            ChoiceAction::Arrangement(choice) => Some(choice),
            _ => None,
        }
    }

    pub(super) fn toggle_arrangement_tab(&mut self, source: &TabId) {
        let Some(Overlay::Arrangements { tab_id, .. }) = &self.overlay else {
            return;
        };
        let order: Vec<TabId> = self
            .mergeable_tabs(tab_id)
            .into_iter()
            .map(|tab| tab.id.clone())
            .collect();
        if let Some(Overlay::Arrangements {
            merge,
            confirm_delete,
            ..
        }) = &mut self.overlay
        {
            if let Some(index) = merge.iter().position(|id| id == source) {
                merge.remove(index);
            } else {
                merge.push(source.clone());
            }
            merge.sort_by_key(|id| order.iter().position(|candidate| candidate == id));
            *confirm_delete = None;
        }
    }

    /// Clear zoom first so the committed result is visible.
    fn reveal_tab_layout(&mut self, tab_id: &TabId, window: &Window) {
        if self.pane_zoom.clear(tab_id) {
            self.zoom_layout = None;
            self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
            self.rebuild_layout(window, true);
        }
    }

    /// Commit a rearranged tree for one tab.
    pub(super) fn apply_arrangement(
        &mut self,
        tab_id: &TabId,
        layout: LayoutNode,
        window: &Window,
    ) {
        let Some(tab) = self.tab_by_id(tab_id) else {
            self.global_error = Some("The terminal tab no longer exists".into());
            return;
        };
        if tab.layout == layout {
            return;
        }
        self.reveal_tab_layout(tab_id, window);
        self.mutate(
            WorkspaceMutation::ArrangeTab {
                tab_id: tab_id.clone(),
                layout,
            },
            false,
        );
    }

    pub(super) fn apply_arrangement_choice(
        &mut self,
        tab_id: &TabId,
        merge: &[TabId],
        choice: &ArrangementChoice,
        transform: Transform,
        window: &Window,
    ) {
        let sources: Vec<TabId> = self
            .arrangement_tabs(tab_id, merge)
            .into_iter()
            .skip(1)
            .map(|tab| tab.id.clone())
            .collect();
        let Some(layout) = self.arrangement_candidate(tab_id, &sources, choice, transform) else {
            self.global_error = Some("That preset is no longer available".into());
            return;
        };
        if sources.is_empty() {
            self.apply_arrangement(tab_id, layout, window);
            return;
        }
        self.reveal_tab_layout(tab_id, window);
        self.mutate(
            WorkspaceMutation::MergeTabs {
                tab_id: tab_id.clone(),
                sources,
                layout,
            },
            false,
        );
    }

    pub(super) fn run_arrangement_command(&mut self, command: Command, window: &Window) {
        let Some(tab) = self.selected_tab() else {
            return;
        };
        let tab_id = tab.id.clone();
        // Restore undoes the last change: an arrangement, or else a merge.
        let split_merge = command == Command::SplitMergedTabs
            || (command == Command::RestoreArrangement && tab.previous_layout.is_none());
        if split_merge {
            if tab.merge.is_none() {
                self.global_error = Some("No tabs were merged into this tab".into());
                return;
            }
            self.reveal_tab_layout(&tab_id, window);
            self.mutate(WorkspaceMutation::SplitMergedTabs { tab_id }, false);
            return;
        }
        let layout = match command {
            Command::MirrorArrangement | Command::FlipArrangement => {
                Some(arrangement::apply_transform(
                    tab.layout.clone(),
                    Transform {
                        mirror: command == Command::MirrorArrangement,
                        flip: command == Command::FlipArrangement,
                    },
                ))
            }
            Command::RestoreArrangement => tab
                .previous_layout
                .as_deref()
                .and_then(|previous| arrangement::restore(previous, &tab.layout)),
            Command::SwapPaneLeft
            | Command::SwapPaneRight
            | Command::SwapPaneUp
            | Command::SwapPaneDown => {
                let direction = match command {
                    Command::SwapPaneLeft => layout::Direction::Left,
                    Command::SwapPaneRight => layout::Direction::Right,
                    Command::SwapPaneUp => layout::Direction::Up,
                    _ => layout::Direction::Down,
                };
                let pane = self.focused_view().map(|view| view.pane_id.clone());
                pane.as_ref()
                    .and_then(|pane| self.layout.as_ref()?.focus_neighbor(pane, direction))
                    .and_then(|neighbor| arrangement::swap(&tab.layout, pane.as_ref()?, neighbor))
            }
            _ => unreachable!("not an arrangement command"),
        };
        match layout {
            Some(layout) => self.apply_arrangement(&tab_id, layout, window),
            None => {
                self.global_error = Some("This tab's panes cannot be rearranged that way".into())
            }
        }
    }

    pub(super) fn open_save_arrangement(&mut self) {
        if let Some(tab_id) = self.selected_tab().map(|tab| tab.id.clone()) {
            self.open_overlay(
                Overlay::Text {
                    purpose: TextPurpose::SaveArrangement(tab_id),
                    title: "Save pane arrangement as preset",
                },
                "",
            );
        }
    }

    /// Saves the tab's split shape; an existing preset with the same name is replaced.
    pub(super) fn save_arrangement(&mut self, tab_id: &TabId, name: &str) {
        let Some(shape) = self.tab_by_id(tab_id).map(|tab| Shape::of(&tab.layout)) else {
            self.global_error = Some("The terminal tab no longer exists".into());
            return;
        };
        self.refresh_layout_presets();
        if let Err(error) = self.config.save_layout_preset(name, &shape) {
            self.global_error = Some(error);
        }
    }

    /// Picker keys: M mirrors, F flips, Delete twice removes a saved preset.
    pub(super) fn handle_arrangement_key(
        &mut self,
        key: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Overlay::Arrangements {
            tab_id,
            merge,
            confirm_delete,
            ..
        }) = self.overlay.clone()
        else {
            return false;
        };
        let modified = key.modifiers.control || key.modifiers.platform || key.modifiers.alt;
        match key.key.as_str() {
            "m" if !modified => self.set_arrangement_transform(|t| t.mirror = !t.mirror),
            "f" if !modified => self.set_arrangement_transform(|t| t.flip = !t.flip),
            "delete" | "backspace" => {
                let Some(ArrangementChoice::Named(name)) = self.selected_arrangement(cx) else {
                    return true;
                };
                let confirmed = confirm_delete.as_deref() == Some(name.as_str());
                if confirmed {
                    match self.config.remove_layout_preset(&name) {
                        Ok(()) => {
                            let count = self.arrangement_choices(&tab_id, &merge, None).len();
                            self.overlay_index = self.overlay_index.min(count.saturating_sub(1));
                        }
                        Err(error) => self.global_error = Some(error),
                    }
                }
                if let Some(Overlay::Arrangements { confirm_delete, .. }) = &mut self.overlay {
                    *confirm_delete = (!confirmed).then_some(name);
                }
            }
            // Swallow other printable keys: the picker has no text field.
            other => return other.chars().count() == 1 && !modified,
        }
        cx.notify();
        true
    }

    fn set_arrangement_transform(&mut self, update: impl FnOnce(&mut Transform)) {
        if let Some(Overlay::Arrangements { transform, .. }) = &mut self.overlay {
            update(transform);
        }
    }

    pub(super) fn render_arrangement_overlay(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(Overlay::Arrangements {
            tab_id,
            transform,
            merge,
            ..
        }) = &self.overlay
        else {
            unreachable!("arrangement picker requires its overlay");
        };
        let colors = *self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let panel_width = (viewport_width - 64.0).clamp(1.0, 720.0);
        // Header, the 380px choice list, and the Cancel/Apply footer.
        let panel_height = (viewport_height - 48.0).clamp(1.0, 474.0);
        let tabs = self.arrangement_tabs(tab_id, merge);
        // A selected tab row previews the arrangement it would join, but only a
        // selected layout can be applied.
        let selected = self.selected_arrangement(cx);
        let candidate = self.arrangement_candidate(
            tab_id,
            merge,
            selected.as_ref().unwrap_or(&ArrangementChoice::Current),
            *transform,
        );
        let overflow = candidate.as_ref().is_some_and(|candidate| {
            layout::without_panes(candidate, &|pane| self.state.is_floating(pane)).is_some_and(
                |tiled| {
                    layout::compute_layout(&tiled, self.float_area, self.metrics()).has_overflow()
                },
            )
        });
        let merging = tabs.len() > 1;
        let unchanged = !merging
            && tabs
                .first()
                .zip(candidate.as_ref())
                .is_some_and(|(tab, candidate)| &tab.layout == candidate);
        let note = if overflow {
            "Some panes would be smaller than 20 columns by 4 rows; the tab will scroll."
        } else if unchanged {
            "No change from the current arrangement."
        } else if merging {
            "The chosen tabs become this one tab; terminals keep running. Restore previous pane arrangement splits them back."
        } else {
            "Terminals keep running; panes only move and resize."
        };
        let mirror = transform.mirror;
        let flip = transform.flip;
        let side = div()
            .w(px(PREVIEW_WIDTH + 24.0))
            .flex_none()
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .border_l_1()
            .border_color(color(colors.border))
            .child(self.render_arrangement_preview(&tabs, candidate.as_ref()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.settings_segment_button(
                        "arrangement-mirror",
                        "Mirror (M)",
                        mirror,
                        false,
                        cx.listener(|this, _, _, cx| {
                            this.set_arrangement_transform(|t| t.mirror = !t.mirror);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ))
                    .child(self.settings_segment_button(
                        "arrangement-flip",
                        "Flip (F)",
                        flip,
                        false,
                        cx.listener(|this, _, _, cx| {
                            this.set_arrangement_transform(|t| t.flip = !t.flip);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(modal_text_color(
                        if overflow { colors.error } else { colors.muted },
                        &colors,
                    )))
                    .child(note),
            );
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(viewport_width))
            .h(px(viewport_height.max(1.0)))
            .px_4()
            .pb_4()
            .pt(px(CHROME_HEIGHT + 12.0))
            .flex()
            .justify_center()
            .items_start()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .min_h_0()
                    .mt_2()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(color(colors.border))
                    .bg(color(colors.surface))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_dialog_header("Arrange panes and tabs", cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(self.render_choice_rows(cx)),
                            )
                            .child(side),
                    )
                    .child(
                        div()
                            .min_h(px(48.0))
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .border_t_1()
                            .border_color(color(colors.border))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(
                                        "Click to preview · double-click or Enter applies · M mirrors · F flips",
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .gap_2()
                                    .child(self.settings_action_button(
                                        "arrangement-cancel",
                                        "Cancel",
                                        false,
                                        false,
                                        cx.listener(|this, _, _, cx| {
                                            this.dismiss_overlay();
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    ))
                                    // Hidden, like the name dialog's Apply, when there is nothing to apply.
                                    .when(selected.is_some() && candidate.is_some() && !unchanged, |actions| {
                                        actions.child(self.settings_primary_button(
                                            "arrangement-apply",
                                            "Apply",
                                            false,
                                            cx.listener(|this, _, window, cx| {
                                                this.activate_overlay(window, cx);
                                                cx.stop_propagation();
                                            }),
                                        ))
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// A wireframe of the candidate: each pane at its new place, numbered in
    /// tab order across every combined tab. The focused pane is outlined.
    fn render_arrangement_preview(
        &self,
        tabs: &[&WorkspaceTab],
        candidate: Option<&LayoutNode>,
    ) -> AnyElement {
        let colors = *self.colors();
        let panes: Vec<_> = tabs.iter().flat_map(|tab| self.tab_panes(tab)).collect();
        let focused = self.focused_view().map(|view| view.pane_id.clone());
        let cells = candidate
            .map(arrangement::pane_fractions)
            .unwrap_or_default()
            .into_iter()
            .map(|(pane_id, rect)| {
                let (number, title, floating) = panes
                    .iter()
                    .enumerate()
                    .find(|(_, pane)| pane.pane_id == pane_id)
                    .map(|(index, pane)| (index + 1, pane.title.clone(), pane.floating))
                    .unwrap_or((0, String::new(), false));
                let label = if floating {
                    format!("{number}. {title} · Floating")
                } else {
                    format!("{number}. {title}")
                };
                let main = focused.as_ref() == Some(&pane_id);
                div()
                    .absolute()
                    .left(px(rect.x * PREVIEW_WIDTH))
                    .top(px(rect.y * PREVIEW_HEIGHT))
                    .w(px(rect.width * PREVIEW_WIDTH))
                    .h(px(rect.height * PREVIEW_HEIGHT))
                    .p(px(2.0))
                    .child(
                        div()
                            .size_full()
                            .p_1()
                            .overflow_hidden()
                            .rounded_sm()
                            .border_1()
                            .border_color(color(if main { colors.accent } else { colors.border }))
                            .bg(color(blend_rgb(colors.surface, colors.foreground, 0.05)))
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.foreground, &colors)))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(label),
                    )
                    .into_any_element()
            });
        div()
            .relative()
            .w(px(PREVIEW_WIDTH))
            .h(px(PREVIEW_HEIGHT))
            .flex_none()
            .rounded_md()
            .border_1()
            .border_color(color(colors.border))
            .children(cells)
            .into_any_element()
    }
}

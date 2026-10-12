use super::*;
use crate::cli_window::{
    Action, Applied, PaneInfo, PaneType, Request, Response, TargetIdentity, WindowInfo,
};
use compi_protocol::ContentTab;

struct ResizeStep {
    tab_id: TabId,
    path: Vec<bool>,
    ratio: f32,
    before: LayoutNode,
    after: LayoutNode,
}

struct PresentationChange {
    destination: Option<String>,
    resize: VecDeque<ResizeStep>,
}

type PendingResize = (gpui::WindowHandle<CompiApp>, Applied, VecDeque<ResizeStep>);

enum Pending {
    Resize(PendingResize),
    /// `compi edit`, `compi files` and `compi diff` ask for the reveal or focus right after
    /// the workspace change that adds the pane or tab, so the window may not have it yet.
    Polled(gpui::WindowHandle<CompiApp>, Applied),
}

fn inventory(target: &TargetIdentity, cx: &mut App) -> Vec<WindowInfo> {
    cx.windows()
        .into_iter()
        .filter_map(|handle| {
            let typed = handle.downcast::<CompiApp>()?;
            typed
                .update(cx, |app, _, _| {
                    (TargetIdentity::from_target(&app.target) == *target)
                        .then(|| app.presentation_info())
                })
                .ok()
                .flatten()
        })
        .collect()
}

pub(in crate::gui) fn dispatch(
    request: Request,
    completion: crate::window_host::ControlCompletion,
    cx: &mut App,
) {
    let result = (|| -> crate::Result<Option<Pending>> {
        request.validate()?;
        if Instant::now() >= completion.deadline {
            return Err("GUI control expired before application; no operation was applied".into());
        }
        let windows = inventory(&request.target, cx);
        let candidates: Vec<_> = windows
            .iter()
            .filter(|info| {
                request
                    .window
                    .as_ref()
                    .is_none_or(|id| info.window_id == *id)
            })
            .collect();
        if matches!(request.action, Action::List) {
            if request.window.is_some() && candidates.is_empty() {
                return Err("Window ID was not found for this connection; use windows list".into());
            }
            let windows = candidates.into_iter().cloned().collect();
            let _ = completion.sender.send(Ok(Response {
                windows,
                applied: None,
            }));
            return Ok(None);
        }
        let selected = match candidates.as_slice() {
            [] => {
                return Err(
                    "No matching Compi window; use windows list and an explicit --window ID".into(),
                );
            }
            [selected] => selected.window_id.clone(),
            _ => return Err(
                "Several Compi windows match this connection; select exactly one with --window ID"
                    .into(),
            ),
        };
        let handle = cx
            .windows()
            .into_iter()
            .find_map(|handle| {
                let typed = handle.downcast::<CompiApp>()?;
                typed
                    .update(cx, |app, _, _| app.slot_id == selected)
                    .ok()
                    .filter(|yes| *yes)
                    .map(|_| typed)
            })
            .ok_or("Selected window closed before application")?;
        if matches!(
            request.action,
            Action::RevealFile { .. }
                | Action::RevealDiff { .. }
                | Action::ShowChanges { .. }
                | Action::FocusPane { .. }
        ) {
            let applied = Applied {
                window_id: selected,
                action: request.action.clone(),
                destination_window_id: None,
            };
            return Ok(Some(Pending::Polled(handle, applied)));
        }
        let change = handle.update(cx, |app, window, cx| {
            app.apply_presentation(&request.action, window, cx)
        })??;
        let applied = Applied {
            window_id: selected,
            action: request.action.clone(),
            destination_window_id: change.destination,
        };
        if matches!(request.action, Action::ResizePane { .. }) {
            Ok(Some(Pending::Resize((handle, applied, change.resize))))
        } else {
            let windows = inventory(&request.target, cx);
            let _ = completion.sender.send(Ok(Response {
                windows,
                applied: Some(applied),
            }));
            Ok(None)
        }
    })();
    match result {
        Err(error) => {
            let _ = completion.sender.send(Err(error.to_string()));
        }
        Ok(None) => {}
        Ok(Some(Pending::Resize((handle, applied, mut steps)))) => {
            cx.spawn(async move |cx| {
                let mut active: Option<ResizeStep> = None;
                loop {
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                    let checked = cx.update(|cx| {
                        handle.update(cx, |app, window, cx| -> crate::Result<bool> {
                            if Instant::now() >= completion.deadline {
                                return Err("Pane resize deadline elapsed; inspect current pane dimensions before retrying".into());
                            }
                            if app.mutation_pending { return Ok(false); }
                            if let Some(step) = &active {
                                if app.tab_by_id(&step.tab_id).is_none_or(|tab| tab.layout != step.after) {
                                    return Err(app.global_error.clone().unwrap_or_else(|| "Split change was rejected or the layout changed during pane resize".into()).into());
                                }
                                active = None;
                            }
                            if let Some(step) = steps.pop_front() {
                                if app.tab_by_id(&step.tab_id).is_none_or(|tab| tab.layout != step.before) {
                                    return Err("Split layout changed before pane resize could commit; no further divider was changed".into());
                                }
                                app.mutate(WorkspaceMutation::SetSplitRatio {
                                    tab_id: step.tab_id.clone(), path: step.path.clone(), ratio: step.ratio,
                                }, false);
                                if !app.mutation_pending { return Err("Split resize could not start".into()); }
                                active = Some(step);
                                return Ok(false);
                            }
                            app.rebuild_layout(window, true);
                            cx.notify();
                            let Action::ResizePane { ref pane_id, cols, rows } = request.action else { return Ok(false); };
                            Ok(app.surface_views.iter().find(|view| &view.pane_id == pane_id).is_some_and(|view| {
                                (view.cols, view.rows) == (cols, rows)
                                    && view.mirror.snapshot().is_some_and(|screen| (screen.cols, screen.rows) == (cols as u16, rows as u16))
                            }))
                        })
                    });
                    match checked {
                        Ok(Ok(true)) => {
                            let response = cx.update(|cx| Response {
                                windows: inventory(&request.target, cx), applied: Some(applied),
                            });
                            let _ = completion.sender.send(Ok(response));
                            return;
                        }
                        Ok(Ok(false)) if Instant::now() < completion.deadline => {}
                        Ok(Ok(false)) => {
                            let _ = completion.sender.send(Err("Exact pane resize could not be acknowledged: the split geometry changed or its terminal attachment is unavailable. Inspect the pane before retrying".into()));
                            return;
                        }
                        Ok(Err(error)) => {
                            let _ = completion.sender.send(Err(error.to_string()));
                            return;
                        }
                        other => {
                            let _ = completion.sender.send(Err(format!("Window closed during pane resize: {other:?}")));
                            return;
                        }
                    }
                }
            }).detach();
        }
        Ok(Some(Pending::Polled(handle, applied))) => {
            cx.spawn(async move |cx| {
                loop {
                    let checked = cx.update(|cx| {
                        handle.update(cx, |app, window, cx| {
                            app.polled_presentation(&request.action, window, cx)
                        })
                    });
                    match checked {
                        Ok(Ok(true)) => {
                            let response = cx.update(|cx| Response {
                                windows: inventory(&request.target, cx),
                                applied: Some(applied),
                            });
                            let _ = completion.sender.send(Ok(response));
                            return;
                        }
                        Ok(Ok(false)) if Instant::now() < completion.deadline => {
                            cx.background_executor()
                                .timer(Duration::from_millis(20))
                                .await;
                        }
                        Ok(Ok(false)) => {
                            let _ = completion.sender.send(Err(match request.action {
                                Action::RevealFile { .. } => "The pane did not show the file before the request expired; open it in the pane first",
                                Action::RevealDiff { .. } => "The pane did not show the diff before the request expired; open it in the pane first",
                                Action::ShowChanges { .. } => "The pane was not shown before the request expired; show its tab first",
                                _ => "Pane ID does not exist in this window's daemon workspace",
                            }
                            .into()));
                            return;
                        }
                        Ok(Err(error)) => {
                            let _ = completion.sender.send(Err(error.to_string()));
                            return;
                        }
                        Err(error) => {
                            let _ = completion.sender.send(Err(format!(
                                "Window closed before the request was applied: {error}"
                            )));
                            return;
                        }
                    }
                }
            })
            .detach();
        }
    }
}

impl CompiApp {
    fn presentation_info(&self) -> WindowInfo {
        let visible_tabs = self
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .flat_map(|session| self.state.visible_tabs(session))
                    .map(|tab| tab.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let shown_content: Vec<&PaneId> = self.shown_content_panes().collect();
        WindowInfo {
            window_id: self.slot_id.clone(),
            target: TargetIdentity::from_target(&self.target),
            server_id: self
                .workspace
                .as_ref()
                .map(|workspace| workspace.server_id.to_string()),
            selected_tab: self.selected_tab().map(|tab| tab.id.clone()),
            visible_tabs,
            panes: self
                .surface_views
                .iter()
                .filter(|view| !view.stop.load(Ordering::Acquire))
                .map(|view| PaneInfo {
                    pane_id: view.pane_id.clone(),
                    pane_type: PaneType::Terminal,
                    surface_id: Some(view.surface_id.clone()),
                    cols: Some(view.cols),
                    rows: Some(view.rows),
                    focused: self.focused_view == Some(view.id),
                    floating: self.state.is_floating(&view.pane_id),
                    zoomed: self.zoomed_pane() == Some(&view.pane_id),
                    unsaved_files: Vec::new(),
                })
                .chain(
                    // Content panes have no view; list the ones this window shows.
                    shown_content.iter().map(|pane_id| PaneInfo {
                        pane_id: (*pane_id).clone(),
                        pane_type: PaneType::Content,
                        surface_id: None,
                        cols: None,
                        rows: None,
                        focused: self.focused_pane() == Some(*pane_id),
                        floating: self.state.is_floating(pane_id),
                        zoomed: self.zoomed_pane() == Some(*pane_id),
                        unsaved_files: self.unsaved_files(pane_id),
                    }),
                )
                .chain(
                    // Hidden tabs keep their editors; closing them must still see edits.
                    self.content_panes()
                        .filter(|pane_id| !shown_content.contains(pane_id))
                        .filter_map(|pane_id| {
                            let unsaved_files = self.unsaved_files(pane_id);
                            (!unsaved_files.is_empty()).then(|| PaneInfo {
                                pane_id: pane_id.clone(),
                                pane_type: PaneType::Content,
                                surface_id: None,
                                cols: None,
                                rows: None,
                                focused: false,
                                floating: self.state.is_floating(pane_id),
                                zoomed: false,
                                unsaved_files,
                            })
                        }),
                )
                .collect(),
            busy: self.mutation_pending
                || self.daemon_restarting
                || self.update_quiesced.load(Ordering::Acquire),
        }
    }

    fn apply_presentation(
        &mut self,
        action: &Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::Result<PresentationChange> {
        if self.mutation_pending
            || self.daemon_restarting
            || self.update_quiesced.load(Ordering::Acquire)
        {
            return Err("Window is busy with a workspace change, restart, or update; retry after it completes".into());
        }
        if self.overlay.is_some() {
            return Err("Close the active GUI menu or dialog before controlling this window; CLI requests cannot bypass confirmation".into());
        }
        let workspace = self
            .workspace
            .as_ref()
            .ok_or("Window has no authoritative workspace")?;
        let tab_id = match action {
            Action::List => return Err("Listing is not a window mutation".into()),
            Action::RevealFile { .. } | Action::RevealDiff { .. } | Action::ShowChanges { .. } => {
                unreachable!("dispatch polls reveals")
            }
            Action::FocusTab { tab_id } | Action::DetachTab { tab_id } => tab_id.clone(),
            Action::FocusPane { pane_id }
            | Action::ResizePane { pane_id, .. }
            | Action::ZoomPane { pane_id }
            | Action::UnzoomPane { pane_id }
            | Action::FloatPane { pane_id }
            | Action::DockPane { pane_id } => workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| layout_contains_pane(&tab.layout, pane_id))
                .map(|tab| tab.id.clone())
                .ok_or("Pane ID does not exist in this window's daemon workspace")?,
        };
        if !workspace
            .sessions
            .iter()
            .flat_map(|session| self.state.visible_tabs(session))
            .any(|tab| tab.id == tab_id)
        {
            return Err(
                "Tab is not visible in the selected window; choose the window that owns it".into(),
            );
        }
        let prior_focus = if matches!(action, Action::FloatPane { .. } | Action::DockPane { .. }) {
            self.state.focused_pane(workspace).cloned()
        } else {
            None
        };
        let mut resize = VecDeque::new();
        match action {
            Action::FocusTab { tab_id } => {
                self.select_terminal(tab_id.clone(), false);
                if self.selected_tab().is_none_or(|tab| tab.id != *tab_id) {
                    return Err("Tab could not receive GUI focus".into());
                }
            }
            Action::DetachTab { tab_id } => {
                if self.selected_tab().is_none_or(|tab| tab.id != *tab_id) {
                    return Err("Focus the tab in this window before detaching it".into());
                }
                let before: HashSet<_> = cx
                    .windows()
                    .into_iter()
                    .map(|handle| handle.window_id())
                    .collect();
                let previous_error = self.global_error.take();
                self.transfer_tab_mode(tab_id.clone(), None, false, window, cx);
                if !self.state.hidden_tabs.contains(tab_id) {
                    return Err(self
                        .global_error
                        .take()
                        .unwrap_or_else(|| {
                            "Tab transfer did not complete; its source attachment was retained"
                                .into()
                        })
                        .into());
                }
                self.global_error = previous_error;
                let destination = cx
                    .windows()
                    .into_iter()
                    .filter(|handle| !before.contains(&handle.window_id()))
                    .find_map(|handle| handle.downcast::<CompiApp>())
                    .ok_or("Transferred tab destination window closed")?;
                let id = destination.update(cx, |app, _, _| app.slot_id.clone())?;
                cx.notify();
                return Ok(PresentationChange {
                    destination: Some(id),
                    resize,
                });
            }
            Action::FocusPane { pane_id } => {
                if !self.state.is_floating(pane_id) {
                    self.select_terminal(tab_id, false);
                }
                self.focus_pane(pane_id.clone());
                if self.focused_pane() != Some(pane_id) {
                    return Err("Pane could not receive GUI focus".into());
                }
            }
            Action::FloatPane { pane_id } => {
                let workspace = self
                    .workspace
                    .as_ref()
                    .ok_or("Workspace became unavailable")?;
                if !self.state.float_pane(workspace, pane_id) {
                    return Err("Pane could not float".into());
                }
                if let Some(prior) = &prior_focus {
                    self.state.focus_pane(workspace, prior);
                } else {
                    self.state.floating_focused = false;
                }
                self.sync_visible_views();
            }
            Action::DockPane { pane_id } => {
                let workspace = self
                    .workspace
                    .as_ref()
                    .ok_or("Workspace became unavailable")?;
                if !self.state.is_floating(pane_id) {
                    return Err("Pane is already docked".into());
                }
                if !self.state.dock_pane(workspace, pane_id) {
                    return Err("Pane could not dock".into());
                }
                if let Some(prior) = &prior_focus {
                    self.state.focus_pane(workspace, prior);
                }
                self.sync_visible_views();
            }
            Action::ZoomPane { pane_id } => {
                if self.state.is_floating(pane_id) {
                    return Err("Floating panes cannot zoom; dock the pane first".into());
                }
                if self.selected_tab().is_none_or(|tab| tab.id != tab_id) {
                    return Err("Focus the target tab first; zoom will not switch tabs or take keyboard focus".into());
                }
                if self.pane_zoom.pane(&tab_id) != Some(pane_id) {
                    self.pane_zoom.clear(&tab_id);
                    self.pane_zoom.toggle(tab_id, pane_id.clone());
                }
                self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
            }
            Action::UnzoomPane { pane_id } => {
                if self.selected_tab().is_none_or(|tab| tab.id != tab_id) {
                    return Err("Focus the target tab first; unzoom will not switch tabs".into());
                }
                if self.pane_zoom.pane(&tab_id) != Some(pane_id) {
                    return Err("Specified pane is not zoomed in this window".into());
                }
                self.pane_zoom.clear(&tab_id);
                self.zoom_layout = None;
            }
            Action::ResizePane {
                pane_id,
                cols,
                rows,
            } => {
                if self
                    .tab_by_id(&tab_id)
                    .and_then(|tab| leaf_node(&tab.layout, pane_id))
                    .is_some_and(|leaf| matches!(leaf, LayoutNode::Content { .. }))
                {
                    return Err("Content panes are not sized in terminal columns and rows; resize a terminal beside it instead".into());
                }
                self.rebuild_layout(window, true);
                resize = self.resize_presentation(pane_id, *cols, *rows)?;
            }
            Action::List
            | Action::RevealFile { .. }
            | Action::RevealDiff { .. }
            | Action::ShowChanges { .. } => unreachable!(),
        }
        self.rebuild_layout(window, true);
        if let Action::ZoomPane { pane_id } = action
            && self.zoomed_pane() != Some(pane_id)
        {
            return Err("Pane zoom was invalidated by the current layout".into());
        }
        self.save_state();
        if matches!(action, Action::FocusTab { .. } | Action::FocusPane { .. }) {
            window.focus(&self.focus_handle, cx);
            cx.activate(true);
            self.report_focus(window.is_window_active());
        }
        cx.notify();
        Ok(PresentationChange {
            destination: None,
            resize,
        })
    }

    /// Applies an action `dispatch` polls: `Ok(false)` while this window has not yet received
    /// the workspace change that added the pane (or, for a reveal, made the file active).
    fn polled_presentation(
        &mut self,
        action: &Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::Result<bool> {
        let pane_id = match action {
            Action::FocusPane { pane_id } => pane_id,
            Action::RevealFile { .. } => return self.reveal_presentation(action, window, cx),
            _ => return self.changes_presentation(action, window, cx),
        };
        let known = self.workspace.as_ref().is_some_and(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .any(|tab| layout_contains_pane(&tab.layout, pane_id))
        });
        if !known
            || self.mutation_pending
            || self.daemon_restarting
            || self.update_quiesced.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        self.apply_presentation(action, window, cx).map(|_| true)
    }

    /// Applies `Action::RevealFile`: `Ok(false)` until the pane shows the file as active, so
    /// the reveal follows the CLI's own change instead of submitting one from a stale
    /// snapshot (which the daemon would refuse as a revision conflict).
    fn reveal_presentation(
        &mut self,
        action: &Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::Result<bool> {
        let Action::RevealFile {
            pane_id,
            file,
            line,
        } = action
        else {
            unreachable!("polled_presentation passes only file reveals here");
        };
        if self.overlay.is_some() {
            return Err("Close the active GUI menu or dialog before controlling this window; CLI requests cannot bypass confirmation".into());
        }
        if self.mutation_pending
            || self.daemon_restarting
            || self.update_quiesced.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        let workspace = self
            .workspace
            .as_ref()
            .ok_or("Window has no authoritative workspace")?;
        let Some(tab) = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| layout_contains_pane(&tab.layout, pane_id))
        else {
            return Ok(false);
        };
        if !workspace
            .sessions
            .iter()
            .flat_map(|session| self.state.visible_tabs(session))
            .any(|visible| visible.id == tab.id)
        {
            return Err(
                "Tab is not visible in the selected window; choose the window that owns it".into(),
            );
        }
        let Some(LayoutNode::Content { content, .. }) = leaf_node(&tab.layout, pane_id) else {
            return Err("Pane shows a terminal, not files".into());
        };
        if content.active_file() != Some(file.as_str()) {
            return Ok(false);
        }
        let environment = content.environment.clone();
        let tab_id = tab.id.clone();
        if !self.state.is_floating(pane_id) {
            self.select_terminal(tab_id, false);
        }
        self.focus_pane(pane_id.clone());
        if self.focused_pane() != Some(pane_id) {
            return Err("Pane could not receive GUI focus".into());
        }
        self.open_content_file(
            pane_id,
            environment,
            None,
            Some(file.clone()),
            Some(*line),
            false,
            window,
            cx,
        );
        self.rebuild_layout(window, true);
        self.save_state();
        window.focus(&self.focus_handle, cx);
        cx.activate(true);
        self.report_focus(window.is_window_active());
        cx.notify();
        Ok(true)
    }

    /// Applies `Action::RevealDiff` and `Action::ShowChanges`: `Ok(false)` until this window
    /// has the pane, a diff tab as the active tab (the CLI's own change makes it so), and
    /// for the Changes list the pane drawn.
    fn changes_presentation(
        &mut self,
        action: &Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::Result<bool> {
        let (pane_id, diff) = match action {
            Action::RevealDiff {
                pane_id,
                path,
                scope,
                line,
            } => (pane_id, Some((path, *scope, *line))),
            Action::ShowChanges { pane_id } => (pane_id, None),
            _ => unreachable!("polled_presentation passes only diff reveals and Changes here"),
        };
        if self.overlay.is_some() {
            return Err("Close the active GUI menu or dialog before controlling this window; CLI requests cannot bypass confirmation".into());
        }
        if self.mutation_pending
            || self.daemon_restarting
            || self.update_quiesced.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        let workspace = self
            .workspace
            .as_ref()
            .ok_or("Window has no authoritative workspace")?;
        let Some(tab) = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| layout_contains_pane(&tab.layout, pane_id))
        else {
            return Ok(false);
        };
        if !workspace
            .sessions
            .iter()
            .flat_map(|session| self.state.visible_tabs(session))
            .any(|visible| visible.id == tab.id)
        {
            return Err(
                "Tab is not visible in the selected window; choose the window that owns it".into(),
            );
        }
        let Some(LayoutNode::Content { content, .. }) = leaf_node(&tab.layout, pane_id) else {
            return Err("Pane shows a terminal, not files".into());
        };
        if let Some((path, scope, _)) = diff
            && !matches!(
                &content.active,
                Some(ContentTab::Diff { path: active, scope: shown, .. })
                    if active == path && *shown == scope
            )
        {
            return Ok(false);
        }
        let tab_id = tab.id.clone();
        if !self.state.is_floating(pane_id) {
            self.select_terminal(tab_id, false);
        }
        self.focus_pane(pane_id.clone());
        if self.focused_pane() != Some(pane_id) {
            return Err("Pane could not receive GUI focus".into());
        }
        match diff {
            // `open_diff` waits for a pane not drawn yet, or for its first Git read.
            Some((path, scope, line)) => {
                self.open_diff(pane_id, path.clone(), Some(scope), line, window, cx)
            }
            None if !self.content.panes.contains_key(pane_id) => {
                self.rebuild_layout(window, true);
                cx.notify();
                return Ok(false);
            }
            None => self.show_changes_list(pane_id, window, cx),
        }
        self.rebuild_layout(window, true);
        self.save_state();
        cx.activate(true);
        self.report_focus(window.is_window_active());
        cx.notify();
        Ok(true)
    }

    fn resize_presentation(
        &mut self,
        pane_id: &PaneId,
        cols: i16,
        rows: i16,
    ) -> crate::Result<VecDeque<ResizeStep>> {
        let metrics = self.metrics();
        if self.state.is_floating(pane_id) {
            let float = self
                .float_layouts
                .iter()
                .find(|float| &float.pane.pane_id == pane_id)
                .ok_or("Floating pane has no displayed geometry; focus the target first")?;
            let mut frame = float.frame;
            frame.width = (f32::from(cols) + 0.5) * metrics.cell_width + 2.0 * metrics.padding_x;
            frame.height = (f32::from(rows) + 0.5) * metrics.line_height
                + 2.0 * metrics.padding_y
                + FLOAT_TITLE_HEIGHT;
            if frame.width > self.float_area.width || frame.height > self.float_area.height {
                return Err("Requested floating pane size exceeds this window; enlarge the window explicitly or dock the pane first".into());
            }
            let fraction = layout::float_fraction(frame, self.float_area);
            let saved = self
                .state
                .floating
                .iter_mut()
                .find(|float| &float.pane_id == pane_id)
                .ok_or("Floating placement disappeared")?;
            saved.rect = FloatRect {
                x: fraction.x,
                y: fraction.y,
                width: fraction.width,
                height: fraction.height,
            };
            return Ok(VecDeque::new());
        }
        let tab = self.selected_tab().filter(|tab| layout_contains_pane(&tab.layout, pane_id))
            .ok_or("Focus the target tab first; pane resize will not switch tabs or resize the native window")?;
        let current = self
            .visible_layout()
            .and_then(|layout| layout.pane(pane_id))
            .ok_or("Focus the target pane first; it has no displayed resize geometry")?
            .grid_size(metrics);
        if self.pane_zoomed() {
            return if current == (cols, rows) {
                Ok(VecDeque::new())
            } else {
                Err("Zoomed pane dimensions are fixed by the native window; unzoom before resizing its splits".into())
            };
        }
        let tab_id = tab.id.clone();
        let mut saved_tree = tab.layout.clone();
        let mut tree = layout::without_panes(&saved_tree, &|pane| self.state.is_floating(pane))
            .ok_or("Tab has no tiled panes")?;
        let mut steps = VecDeque::new();
        let compute = |tree: &LayoutNode| {
            let mut available = self.float_area;
            let mut computed = layout::compute_layout(tree, available, metrics);
            if computed.has_overflow() {
                available.width = (available.width - 8.0).max(1.0);
                available.height = (available.height - 8.0).max(1.0);
                computed = layout::compute_layout(tree, available, metrics);
            }
            computed
        };
        for (axis, wanted) in [(SplitAxis::Horizontal, cols), (SplitAxis::Vertical, rows)] {
            let computed = compute(&tree);
            let grid = computed
                .pane(pane_id)
                .ok_or("Pane is absent from the displayed layout")?
                .grid_size(metrics);
            let dimension = |grid: (i16, i16)| {
                if axis == SplitAxis::Horizontal {
                    grid.0
                } else {
                    grid.1
                }
            };
            if dimension(grid) == wanted {
                continue;
            }
            let divider = computed.dividers.iter().filter(|divider| {
                divider.axis == axis && layout::node_at_path(&tree, &divider.path)
                    .is_some_and(|node| layout_contains_pane(node, pane_id))
            }).max_by_key(|divider| divider.path.len())
                .ok_or("Requested dimension has no applicable split divider; resize the native window explicitly or change the split layout")?;
            if divider.disabled_reason().is_some() {
                return Err("Both sides of the target divider are at minimum size; requested pane dimensions do not fit".into());
            }
            let first = match layout::node_at_path(&tree, &divider.path) {
                Some(LayoutNode::Split { first, .. }) => layout_contains_pane(first, pane_id),
                _ => return Err("Target divider disappeared".into()),
            };
            let path = divider.path.clone();
            let saved_path =
                layout::saved_path(&saved_tree, &path, &|pane| self.state.is_floating(pane));
            let (mut low, mut high) = (divider.min_ratio, divider.max_ratio);
            let mut at = |ratio: f32| -> crate::Result<i16> {
                set_ratio(&mut tree, &path, ratio);
                Ok(dimension(
                    compute(&tree)
                        .pane(pane_id)
                        .ok_or("Pane disappeared from divider geometry")?
                        .grid_size(metrics),
                ))
            };
            let a = at(low)?;
            let b = at(high)?;
            if wanted < a.min(b) || wanted > a.max(b) {
                return Err("Requested pane dimensions do not fit this divider while preserving sibling minimum sizes".into());
            }
            for _ in 0..26 {
                let middle = (low + high) * 0.5;
                let value = at(middle)?;
                if if first {
                    value < wanted
                } else {
                    value > wanted
                } {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            let edge = high;
            let mut end_low = edge;
            let mut end_high = divider.max_ratio;
            for _ in 0..26 {
                let middle = (end_low + end_high) * 0.5;
                let value = at(middle)?;
                if if first {
                    value <= wanted
                } else {
                    value >= wanted
                } {
                    end_low = middle;
                } else {
                    end_high = middle;
                }
            }
            let ratio = (edge + end_low) * 0.5;
            if at(ratio)? != wanted {
                return Err("Requested exact pane dimensions are not representable at the current display scale and split geometry".into());
            }
            let before = saved_tree.clone();
            set_ratio(&mut saved_tree, &saved_path, ratio);
            steps.push_back(ResizeStep {
                tab_id: tab_id.clone(),
                path: saved_path,
                ratio,
                before,
                after: saved_tree.clone(),
            });
        }
        Ok(steps)
    }
}

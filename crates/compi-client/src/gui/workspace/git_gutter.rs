//! The Git gutter: marks beside an open file's lines where its text, unsaved edits
//! included, differs from the Git index, and the baseline (`vs index`, `untracked`,
//! `conflicted`) in the tab strip. It only reads: nothing is staged or discarded.
//!
//! The index text is read once per index version of the file (the stamp of its unstaged
//! change, or with no unstaged change the staged stamp and `HEAD`), from the repository
//! status its pane keeps fresh. Edits are compared with it on a background thread,
//! `DIFF_DELAY` after the typing stops; a generation counter drops stale results.
//!
//! GPUI Kit's editor has no gutter marks, so a canvas over the editor paints them in the
//! editor's left padding, after the editor itself in the same frame. Rows come from the
//! editor's own layout of that frame (`range_to_bounds` of each visible line start), so
//! marks follow scrolling, typing, resizing, font size and folds exactly.
use super::content::{CHECK_INTERVAL, HIGHLIGHT_LIMIT, find_content, reveal_in_editor};
use super::*;
use crate::content_path::repo_relative;
use crate::diff::{FileDiff, Hunk, Whitespace};
use crate::gui::controls::{self, ChipTone, text_button};
use compi_protocol::{GitChangeKind, GitChanges, GitFileContent, GitRevision, GitScope};
use gpui::{
    ContentMask, CursorStyle, DispatchPhase, HitboxBehavior, MouseDownEvent, MouseMoveEvent,
    PathBuilder, Task,
};
use gpui_component::input::RopeExt;
use std::sync::Arc;

/// Edits are compared with the index once typing has paused this long.
const DIFF_DELAY: Duration = Duration::from_millis(150);
/// The GPUI Kit code editor's left padding (`min(input px, 6)`), which the marks sit in
/// and which answers their clicks.
const STRIP_WIDTH: f32 = 6.0;
const BAR_LEFT: f32 = 1.0;
const BAR_WIDTH: f32 = 3.0;
/// Half the height of a deletion's wedge, which is one pixel wider than it is half tall.
const WEDGE: f32 = 4.0;

/// What an open file is compared with, as its header names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Baseline {
    Index,
    /// Not in the index (untracked, ignored, or only intended to be added): no marks.
    Untracked,
    /// Unmerged: there is no single index version to compare with.
    Conflicted,
}

/// A file's state in its repository's status.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    /// Compared with the index, read again when `key` changes.
    Index {
        key: String,
    },
    Untracked,
    Conflicted,
}

/// The index version of a file as read.
enum Base {
    Text(Arc<str>),
    /// No index version: the file is not tracked.
    Missing,
    /// Binary, an image, or larger than the editor opens.
    Unreadable,
}

/// An editor's Git gutter. Kept per editor; brought up to date while it is shown.
#[derive(Default)]
pub(super) struct GitGutter {
    /// What the file is compared with; `None` outside a repository, before its status is
    /// known, or for a file too large to compare.
    baseline: Option<Baseline>,
    /// The file's repository-relative path once its repository is known.
    rel_path: Option<String>,
    /// The index version and the status key it was read at.
    base: Option<(String, Base)>,
    /// The key of the index read under way.
    reading: Option<String>,
    /// The key of the last failed read, and when; it is tried again after `CHECK_INTERVAL`.
    failed: Option<(String, Instant)>,
    hunks: Arc<[Hunk]>,
    /// Bumped by every change of the text or the base; a diff of an older one is dropped.
    generation: u64,
    /// The generation `hunks` were computed for.
    diffed: Option<u64>,
    /// The diff under way; replacing it cancels it.
    diffing: Option<Task<()>>,
}

impl GitGutter {
    /// The editor's text changed without an edit event (a reload).
    pub(super) fn text_replaced(&mut self) {
        self.generation += 1;
    }

    fn base_text(&self) -> Option<&Arc<str>> {
        match &self.base {
            Some((_, Base::Text(text))) if self.baseline == Some(Baseline::Index) => Some(text),
            _ => None,
        }
    }

    /// The hunks against the index, when the file is compared with it.
    fn compared(&self) -> Option<&[Hunk]> {
        self.base_text().map(|_| &*self.hunks)
    }

    fn clear(&mut self, baseline: Option<Baseline>, rel_path: Option<String>) {
        *self = Self {
            baseline,
            rel_path,
            generation: self.generation,
            ..Self::default()
        };
    }
}

/// Where the file `rel` stands in `changes`, and so what its index version is keyed by.
fn file_status(changes: &GitChanges, rel: &str) -> Status {
    let mut staged = None;
    for change in changes.changes.iter().filter(|change| change.path == rel) {
        match (change.scope, change.kind) {
            (_, GitChangeKind::Conflicted) => return Status::Conflicted,
            (GitScope::Unstaged, GitChangeKind::Untracked | GitChangeKind::Added) => {
                return Status::Untracked;
            }
            (GitScope::Unstaged, _) => {
                // An unstaged stamp starts with the index entry's mode and blob id.
                let index = change.stamp.splitn(3, ':').take(2).collect::<Vec<_>>();
                return Status::Index {
                    key: format!("index {}", index.join(":")),
                };
            }
            (GitScope::Staged, _) => staged = Some(&change.stamp),
        }
    }
    // The index matches the work tree: its version changes with the staged change, or
    // with no staged change, with `HEAD`.
    Status::Index {
        key: match staged {
            Some(stamp) => format!("staged {stamp}"),
            None => format!("head {}", changes.head.as_deref().unwrap_or("")),
        },
    }
}

/// Which kind of change a mark shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MarkKind {
    Added,
    /// Lines replacing as many old lines (the i-th removed line pairs with the i-th added).
    Modified,
    /// Lines removed between two lines: a wedge at their boundary.
    Deleted,
}

/// A row the editor shows: buffer line `line` with its top at `top`. Lines folded under
/// it (up to the next row's line) are drawn as part of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Row {
    pub line: u32,
    pub top: f32,
}

/// One mark: a bar from `top` to `bottom`, or for a deletion a wedge at `top` (equal to
/// `bottom`). `line` is the 1-based buffer line its diff reveals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Mark {
    pub kind: MarkKind,
    pub top: f32,
    pub bottom: f32,
    pub line: u32,
}

/// The rows of visible lines from each laid-out line's top, in line order. The editor
/// lays a folded line out at the start of the line ending its fold, so a line sharing
/// its top with the line after it is hidden.
pub(super) fn visible_rows(tops: &[(u32, f32)]) -> Vec<Row> {
    tops.iter()
        .enumerate()
        .filter(|(index, (_, top))| tops.get(index + 1).is_none_or(|(_, next)| next != top))
        .map(|(_, &(line, top))| Row { line, top })
        .collect()
}

/// The marks of `hunks` (new lines are buffer lines) on `rows`, rows `line_height` tall.
/// Changes inside a fold mark its row; changes outside the rows are left out.
pub(super) fn layout_marks(
    hunks: &[Hunk],
    rows: &[Row],
    line_height: f32,
    line_count: u32,
) -> Vec<Mark> {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return Vec::new();
    };
    // The row drawing `line`; `None` above the first row or below the last.
    let row_of = |line: u32| -> Option<&Row> {
        if line < first.line || line > last.line {
            return None;
        }
        rows.get(rows.partition_point(|row| row.line <= line) - 1)
    };
    let reveal = |line: u32| (line + 1).min(line_count.max(1));
    let mut marks = Vec::new();
    let start = hunks.partition_point(|hunk| hunk.new.end.max(hunk.new.start + 1) <= first.line);
    for hunk in &hunks[start..] {
        if hunk.new.start > last.line + 1 {
            break;
        }
        if hunk.new.is_empty() {
            let at = hunk.new.start;
            let top = match row_of(at) {
                Some(row) if row.line == at => Some(row.top),
                // Inside a fold, or just past the last row: the bottom of the row above.
                _ => at
                    .checked_sub(1)
                    .and_then(row_of)
                    .map(|row| row.top + line_height),
            };
            if let Some(top) = top {
                marks.push(Mark {
                    kind: MarkKind::Deleted,
                    top,
                    bottom: top,
                    line: reveal(at),
                });
            }
            continue;
        }
        let paired = hunk.new.start + hunk.old.len().min(hunk.new.len()) as u32;
        for (kind, lines) in [
            (MarkKind::Modified, hunk.new.start..paired),
            (MarkKind::Added, paired..hunk.new.end),
        ] {
            if lines.is_empty() || lines.end <= first.line || lines.start > last.line {
                continue;
            }
            let (Some(top), Some(bottom)) = (
                row_of(lines.start.max(first.line)),
                row_of((lines.end - 1).min(last.line)),
            ) else {
                continue;
            };
            marks.push(Mark {
                kind,
                top: top.top,
                bottom: bottom.top + line_height,
                line: reveal(hunk.new.start),
            });
        }
    }
    marks
}

/// The 0-based line of the next (or previous) hunk's start from the cursor on `cursor`;
/// `None` past the last (or before the first): stepping stops at the ends.
pub(super) fn step_hunk(
    hunks: &[Hunk],
    cursor: u32,
    forward: bool,
    line_count: u32,
) -> Option<u32> {
    let mut starts = hunks.iter().map(|hunk| hunk.new.start);
    let start = if forward {
        starts.find(|&start| start > cursor)
    } else {
        starts.rev().find(|&start| start < cursor)
    }?;
    Some(start.min(line_count.saturating_sub(1)))
}

impl CompiApp {
    /// Brings the gutters of the editors on screen up to date with their repositories'
    /// status: reads index versions that moved and compares texts that changed.
    pub(super) fn refresh_gutters(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let shown: Vec<_> = self
            .shown_content_panes()
            .filter_map(|pane_id| {
                let path = find_content(workspace, pane_id)?.active_file()?.to_owned();
                let status = self.repo_changes(pane_id).and_then(|changes| {
                    let rel = repo_relative(&changes.repo, &path)?;
                    let status = file_status(changes, &rel);
                    Some((changes.repo.clone(), rel, status))
                });
                Some((pane_id.clone(), path, status))
            })
            .collect();
        for (pane_id, path, status) in shown {
            self.update_gutter(&pane_id, &path, status, cx);
        }
    }

    fn update_gutter(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        status: Option<(String, String, Status)>,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(editor) = ui.editor_mut(path) else {
            return;
        };
        let too_large = editor.state.read(cx).text().len() > HIGHLIGHT_LIMIT;
        let gutter = &mut editor.gutter;
        let (repo, rel, key) = match status {
            Some((repo, rel, Status::Index { key })) if !too_large => (repo, rel, key),
            Some((_, rel, Status::Untracked)) => {
                if gutter.baseline != Some(Baseline::Untracked) {
                    gutter.clear(Some(Baseline::Untracked), Some(rel));
                    cx.notify();
                }
                return;
            }
            Some((_, rel, Status::Conflicted)) => {
                if gutter.baseline != Some(Baseline::Conflicted) {
                    gutter.clear(Some(Baseline::Conflicted), Some(rel));
                    cx.notify();
                }
                return;
            }
            _ => {
                if gutter.baseline.is_some() {
                    gutter.clear(None, None);
                    cx.notify();
                }
                return;
            }
        };
        if gutter.rel_path.as_deref() != Some(rel.as_str()) {
            gutter.clear(None, Some(rel.clone()));
        }
        let baseline = match &gutter.base {
            Some((_, Base::Missing)) => Some(Baseline::Untracked),
            Some((_, Base::Unreadable)) => None,
            _ => Some(Baseline::Index),
        };
        if gutter.baseline != baseline {
            gutter.baseline = baseline;
            cx.notify();
        }
        let current = gutter.base.as_ref().is_some_and(|(at, _)| *at == key);
        let failed_recently = gutter
            .failed
            .as_ref()
            .is_some_and(|(at, when)| *at == key && when.elapsed() < CHECK_INTERVAL);
        if !current && gutter.reading.as_ref() != Some(&key) && !failed_recently {
            gutter.reading = Some(key.clone());
            let read = cx.background_executor().spawn(async move {
                (|| -> crate::Result<_> {
                    target
                        .connect()?
                        .read_git_file(environment, repo, rel, GitRevision::Index)
                })()
                .map_err(|error| error.to_string())
            });
            let (pane, file) = (pane_id.clone(), path.to_owned());
            cx.spawn(async move |this, cx| {
                let result = read.await;
                this.update(cx, |this, cx| {
                    this.gutter_base_read(&pane, &file, key, result, cx)
                })
                .ok();
            })
            .detach();
        }
        if gutter.base_text().is_some()
            && gutter.diffed != Some(gutter.generation)
            && gutter.diffing.is_none()
        {
            self.schedule_gutter_diff(pane_id, path, Duration::ZERO, cx);
        }
    }

    fn gutter_base_read(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        key: String,
        result: Result<GitFileContent, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(gutter) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
            .map(|editor| &mut editor.gutter)
        else {
            return;
        };
        if gutter.reading.as_ref() != Some(&key) {
            return;
        }
        gutter.reading = None;
        let base = match result {
            Ok(GitFileContent::Text { text }) => Base::Text(text.into()),
            Ok(GitFileContent::Missing) => Base::Missing,
            Ok(_) => Base::Unreadable,
            Err(_) => {
                gutter.failed = Some((key, Instant::now()));
                return;
            }
        };
        gutter.failed = None;
        gutter.baseline = match base {
            Base::Text(_) => Some(Baseline::Index),
            Base::Missing => Some(Baseline::Untracked),
            Base::Unreadable => None,
        };
        let compare = matches!(base, Base::Text(_));
        gutter.base = Some((key, base));
        gutter.generation += 1;
        if compare {
            self.schedule_gutter_diff(pane_id, path, Duration::ZERO, cx);
        } else {
            gutter.hunks = Arc::from([]);
            gutter.diffing = None;
        }
        cx.notify();
    }

    /// An edit in the editor of `path`: compares it again once typing pauses.
    pub(super) fn gutter_edited(&mut self, pane_id: &PaneId, path: &str, cx: &mut Context<Self>) {
        let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        else {
            return;
        };
        editor.gutter.generation += 1;
        if editor.gutter.base_text().is_some() {
            self.schedule_gutter_diff(pane_id, path, DIFF_DELAY, cx);
        }
    }

    /// Compares the editor's text with the index after `delay`, on a background thread,
    /// replacing (and so cancelling) any comparison under way.
    fn schedule_gutter_diff(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let (pane, file) = (pane_id.clone(), path.to_owned());
        let task = cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok(Some((generation, base, text))) = this.update(cx, |this, cx| {
                let editor = this.content.panes.get(&pane)?.editor(&file)?;
                let text = editor.state.read(cx).text().clone();
                let base = editor.gutter.base_text()?.clone();
                (text.len() <= HIGHLIGHT_LIMIT).then_some((editor.gutter.generation, base, text))
            }) else {
                return;
            };
            let hunks: Arc<[Hunk]> = cx
                .background_executor()
                .spawn(async move {
                    FileDiff::new(&base, &text.to_string(), Whitespace::Exact)
                        .hunks()
                        .into()
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(gutter) = this
                    .content
                    .panes
                    .get_mut(&pane)
                    .and_then(|ui| ui.editor_mut(&file))
                    .map(|editor| &mut editor.gutter)
                else {
                    return;
                };
                gutter.diffing = None;
                if gutter.generation == generation {
                    gutter.hunks = hunks;
                    gutter.diffed = Some(generation);
                    cx.notify();
                }
            })
            .ok();
        });
        if let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        {
            editor.gutter.diffing = Some(task);
        }
    }

    /// The hunks of the editor of `path` against the index: old lines are index lines,
    /// new lines buffer lines. `None` when the file is not compared with the index.
    pub(super) fn editor_hunks(&self, pane_id: &PaneId, path: &str) -> Option<&[Hunk]> {
        self.content
            .panes
            .get(pane_id)?
            .editor(path)?
            .gutter
            .compared()
    }

    /// **Next change** / **Previous change**: puts the cursor on the next (or previous)
    /// hunk of the focused editor and centers it. Stops at the last (or first) one.
    pub(super) fn step_editor_change(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, path)) = self.focused_pane().and_then(|pane| {
            Some((
                pane.clone(),
                self.focused_content()?.active_file()?.to_owned(),
            ))
        }) else {
            return;
        };
        let (Some(hunks), Some(editor)) = (
            self.editor_hunks(&pane_id, &path),
            self.content
                .panes
                .get(&pane_id)
                .and_then(|ui| ui.editor(&path)),
        ) else {
            return;
        };
        let state = editor.state.read(cx);
        let line_count = state.text().lines_len() as u32;
        let Some(line) = step_hunk(hunks, state.cursor_position().line, forward, line_count) else {
            return;
        };
        editor.state.clone().update(cx, |state, cx| {
            reveal_in_editor(state, line + 1, window, cx)
        });
        cx.notify();
    }

    /// The editor's baseline for the tab strip: `vs index` opens the file's diff at the
    /// cursor's line; `untracked` and `conflicted` only say why there are no marks.
    pub(super) fn render_baseline(
        &self,
        pane_id: &PaneId,
        path: &str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.content.panes.get(pane_id)?.editor(path)?;
        let gutter = &editor.gutter;
        Some(match gutter.baseline? {
            Baseline::Index => {
                let (pane, rel, state) = (
                    pane_id.clone(),
                    gutter.rel_path.clone()?,
                    editor.state.clone(),
                );
                text_button("gutter-baseline", "vs index")
                    .ghost()
                    .tooltip("Compared with the Git index")
                    .detail("Opens the file's diff")
                    .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                        let line = state.read(cx).cursor_position().line + 1;
                        this.open_diff(
                            &pane,
                            rel.clone(),
                            Some(GitScope::Unstaged),
                            Some(line),
                            window,
                            cx,
                        );
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Baseline::Untracked => {
                controls::chip("untracked", ChipTone::Neutral).into_any_element()
            }
            Baseline::Conflicted => {
                controls::chip("conflicted", ChipTone::Danger).into_any_element()
            }
        })
    }

    /// The marks over the editor of `path`, painted after it in the same frame.
    pub(super) fn render_gutter(
        &self,
        pane_id: &PaneId,
        path: &str,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let editor = self.content.panes.get(pane_id)?.editor(path)?;
        let hunks = editor.gutter.hunks.clone();
        editor.gutter.compared().filter(|hunks| !hunks.is_empty())?;
        let rel = editor.gutter.rel_path.clone()?;
        let colors = *self.colors();
        let tint = move |kind: MarkKind| {
            color(match kind {
                MarkKind::Added => colors.added,
                MarkKind::Modified => colors.modified,
                MarkKind::Deleted => colors.removed,
            })
        };
        let (state, app, pane) = (
            editor.state.clone(),
            cx.entity().downgrade(),
            pane_id.clone(),
        );
        Some(
            canvas(
                |bounds, window, _| {
                    window.insert_hitbox(
                        Bounds::new(bounds.origin, size(px(STRIP_WIDTH), bounds.size.height)),
                        HitboxBehavior::Normal,
                    )
                },
                move |bounds, hitbox, window, cx| {
                    let marks = {
                        let state = state.read(cx);
                        let (Some(line_height), Some(range)) =
                            (state.line_height(), state.visible_row_range())
                        else {
                            return;
                        };
                        let text = state.text();
                        let tops: Vec<(u32, f32)> = range
                            .filter_map(|line| {
                                let offset = text.line_start_offset(line);
                                let row = state.range_to_bounds(&(offset..offset))?;
                                Some((line as u32, f32::from(row.origin.y)))
                            })
                            .collect();
                        layout_marks(
                            &hunks,
                            &visible_rows(&tops),
                            f32::from(line_height),
                            text.lines_len() as u32,
                        )
                    };
                    let left = bounds.left();
                    // Where each mark answers the pointer: the strip's width, and a wedge's
                    // height around a deletion.
                    let targets: Vec<(Bounds<Pixels>, u32)> = marks
                        .iter()
                        .map(|mark| {
                            let (top, bottom) = if mark.kind == MarkKind::Deleted {
                                (mark.top - WEDGE, mark.top + WEDGE)
                            } else {
                                (mark.top, mark.bottom)
                            };
                            (
                                Bounds::new(
                                    point(left, px(top)),
                                    size(px(STRIP_WIDTH), px(bottom - top)),
                                ),
                                mark.line,
                            )
                        })
                        .collect();
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        for mark in &marks {
                            let x = left + px(BAR_LEFT);
                            if mark.kind == MarkKind::Deleted {
                                let mut wedge = PathBuilder::fill();
                                wedge.move_to(point(x, px(mark.top - WEDGE)));
                                wedge.line_to(point(x + px(WEDGE + 1.0), px(mark.top)));
                                wedge.line_to(point(x, px(mark.top + WEDGE)));
                                wedge.close();
                                if let Ok(path) = wedge.build() {
                                    window.paint_path(path, tint(mark.kind));
                                }
                            } else {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(x, px(mark.top)),
                                        size(px(BAR_WIDTH), px(mark.bottom - mark.top)),
                                    ),
                                    tint(mark.kind),
                                ));
                            }
                        }
                    });
                    let target_at = {
                        let targets = targets.clone();
                        move |position: Point<Pixels>| {
                            targets
                                .iter()
                                .find(|(target, _)| target.contains(&position))
                                .map(|&(_, line)| line)
                        }
                    };
                    let hovered =
                        hitbox.is_hovered(window) && target_at(window.mouse_position()).is_some();
                    if hovered {
                        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                    }
                    window.on_mouse_event({
                        let (hitbox, target_at) = (hitbox.clone(), target_at.clone());
                        move |event: &MouseMoveEvent, phase, window, _| {
                            if phase == DispatchPhase::Bubble
                                && (hitbox.is_hovered(window)
                                    && target_at(event.position).is_some())
                                    != hovered
                            {
                                window.refresh();
                            }
                        }
                    });
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble
                            || event.button != MouseButton::Left
                            || !hitbox.is_hovered(window)
                        {
                            return;
                        }
                        let Some(line) = target_at(event.position) else {
                            return;
                        };
                        cx.stop_propagation();
                        app.update(cx, |this, cx| {
                            this.open_diff(
                                &pane,
                                rel.clone(),
                                Some(GitScope::Unstaged),
                                Some(line),
                                window,
                                cx,
                            );
                            cx.notify();
                        })
                        .ok();
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::GitChange;

    fn hunk(old: std::ops::Range<u32>, new: std::ops::Range<u32>) -> Hunk {
        Hunk {
            old,
            new,
            formatting_only: false,
        }
    }

    /// One row per line, 10 px tall, for lines `lines`.
    fn rows(lines: std::ops::Range<u32>) -> Vec<Row> {
        lines
            .map(|line| Row {
                line,
                top: line as f32 * 10.0,
            })
            .collect()
    }

    fn mark(kind: MarkKind, top: f32, bottom: f32, line: u32) -> Mark {
        Mark {
            kind,
            top,
            bottom,
            line,
        }
    }

    #[test]
    fn paired_lines_are_modified_and_the_rest_added() {
        // Two old lines replaced by four: two modified, two added.
        let marks = layout_marks(&[hunk(2..4, 2..6)], &rows(0..10), 10.0, 10);
        assert_eq!(
            marks,
            [
                mark(MarkKind::Modified, 20.0, 40.0, 3),
                mark(MarkKind::Added, 40.0, 60.0, 3),
            ]
        );
        // More old lines than new: only modified.
        let marks = layout_marks(&[hunk(2..6, 2..3)], &rows(0..10), 10.0, 10);
        assert_eq!(marks, [mark(MarkKind::Modified, 20.0, 30.0, 3)]);
    }

    #[test]
    fn deletions_sit_on_the_boundary_at_the_start_and_end_of_the_file() {
        // Lines removed before the first line: the top of the file.
        let marks = layout_marks(&[hunk(0..2, 0..0)], &rows(0..5), 10.0, 5);
        assert_eq!(marks, [mark(MarkKind::Deleted, 0.0, 0.0, 1)]);
        // Removed after the last line (no newline at the end): the bottom of the last row.
        let marks = layout_marks(&[hunk(5..7, 5..5)], &rows(0..5), 10.0, 5);
        assert_eq!(marks, [mark(MarkKind::Deleted, 50.0, 50.0, 5)]);
        // Between two lines: the top of the line after.
        let marks = layout_marks(&[hunk(3..4, 3..3)], &rows(0..5), 10.0, 5);
        assert_eq!(marks, [mark(MarkKind::Deleted, 30.0, 30.0, 4)]);
    }

    #[test]
    fn changes_outside_the_rows_are_clipped_or_left_out() {
        let shown = rows(10..20);
        // Above, a deletion past the last row's bottom boundary, and below.
        let hunks = [hunk(0..0, 2..4), hunk(21..22, 21..21), hunk(30..31, 30..31)];
        assert!(layout_marks(&hunks, &shown, 10.0, 40).is_empty());
        // Straddling the top: drawn from the first row.
        let marks = layout_marks(&[hunk(0..0, 8..12)], &shown, 10.0, 40);
        assert_eq!(marks, [mark(MarkKind::Added, 100.0, 120.0, 9)]);
        // Straddling the bottom: drawn to the last row.
        let marks = layout_marks(&[hunk(0..0, 18..25)], &shown, 10.0, 40);
        assert_eq!(marks, [mark(MarkKind::Added, 180.0, 200.0, 19)]);
    }

    #[test]
    fn folded_lines_mark_their_fold_row() {
        // Lines 2–4 fold under line 1; the editor lays them out at line 5's top.
        let tops = [
            (0, 0.0),
            (1, 10.0),
            (2, 20.0),
            (3, 20.0),
            (4, 20.0),
            (5, 20.0),
            (6, 30.0),
        ];
        let shown = visible_rows(&tops);
        assert_eq!(
            shown.iter().map(|row| row.line).collect::<Vec<_>>(),
            [0, 1, 5, 6]
        );
        // An added line inside the fold marks the fold's row.
        let marks = layout_marks(&[hunk(0..0, 3..4)], &shown, 10.0, 7);
        assert_eq!(marks, [mark(MarkKind::Added, 10.0, 20.0, 4)]);
        // A change spanning the fold's end runs from its row through line 5's.
        let marks = layout_marks(&[hunk(0..0, 4..6)], &shown, 10.0, 7);
        assert_eq!(marks, [mark(MarkKind::Added, 10.0, 30.0, 5)]);
        // A deletion inside the fold sits at the bottom of the fold's row.
        let marks = layout_marks(&[hunk(3..4, 3..3)], &shown, 10.0, 7);
        assert_eq!(marks, [mark(MarkKind::Deleted, 20.0, 20.0, 4)]);
    }

    #[test]
    fn stepping_between_changes_stops_at_the_ends() {
        let hunks = [hunk(0..0, 2..3), hunk(5..6, 6..6), hunk(9..9, 10..12)];
        assert_eq!(step_hunk(&hunks, 0, true, 20), Some(2));
        assert_eq!(step_hunk(&hunks, 2, true, 20), Some(6));
        assert_eq!(step_hunk(&hunks, 6, true, 20), Some(10));
        assert_eq!(step_hunk(&hunks, 10, true, 20), None);
        assert_eq!(step_hunk(&hunks, 11, false, 20), Some(10));
        assert_eq!(step_hunk(&hunks, 10, false, 20), Some(6));
        assert_eq!(step_hunk(&hunks, 2, false, 20), None);
        // A deletion after the last line steps to the last line.
        assert_eq!(step_hunk(&[hunk(4..6, 4..4)], 0, true, 4), Some(3));
    }

    #[test]
    fn the_index_version_is_keyed_by_the_status() {
        let change = |scope, kind, path: &str, stamp: &str| GitChange {
            scope,
            kind,
            path: path.into(),
            old_path: None,
            added: None,
            removed: None,
            stamp: stamp.into(),
        };
        let mut changes = GitChanges {
            repo: "/repo".into(),
            branch: Some("main".into()),
            head: Some("abc1234".into()),
            changes: vec![
                change(
                    GitScope::Unstaged,
                    GitChangeKind::Modified,
                    "m.rs",
                    "100644:aaaa:100644:12:34",
                ),
                change(GitScope::Unstaged, GitChangeKind::Untracked, "new.rs", "?"),
                change(GitScope::Unstaged, GitChangeKind::Added, "intent.rs", "x"),
                change(GitScope::Staged, GitChangeKind::Conflicted, "c.rs", "x"),
                change(
                    GitScope::Staged,
                    GitChangeKind::Modified,
                    "s.rs",
                    "100644:bbbb:100644:cccc",
                ),
            ],
            truncated: false,
            ..Default::default()
        };
        let index = |key: &str| Status::Index { key: key.into() };
        // A second save of a modified file keeps its index key.
        assert_eq!(file_status(&changes, "m.rs"), index("index 100644:aaaa"));
        changes.changes[0].stamp = "100644:aaaa:100644:13:99".into();
        assert_eq!(file_status(&changes, "m.rs"), index("index 100644:aaaa"));
        assert_eq!(file_status(&changes, "new.rs"), Status::Untracked);
        assert_eq!(file_status(&changes, "intent.rs"), Status::Untracked);
        assert_eq!(file_status(&changes, "c.rs"), Status::Conflicted);
        assert_eq!(
            file_status(&changes, "s.rs"),
            index("staged 100644:bbbb:100644:cccc")
        );
        assert_eq!(file_status(&changes, "clean.rs"), index("head abc1234"));
    }
}

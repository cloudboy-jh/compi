//! Switch branch… and Create branch…: a repository's branches in Compi's picker overlay,
//! the one Open file… and Jump to project use.
//!
//! The picker lists the current branch, local branches, then remote-tracking ones, filtered
//! as you type. Enter switches (`GitAction::SwitchBranch`; a remote branch becomes a local
//! branch tracking it), and a `Create branch “name”` row makes a branch at `HEAD` and
//! switches to it (`GitAction::CreateBranch`). Nothing is stashed or forced: when Git
//! refuses, because uncommitted work would be overwritten, its message shows in the picker.
//! Once a switch or create is done, every pane listing the repository reads it again, so
//! the Changes list, open diffs and gutters follow.
use super::changes::truncate_middle;
use super::content::same_folder;
use super::*;
use crate::gui::controls::{self, ChipTone, IconName, text_button};
use crate::project_history::fuzzy_score;
use compi_protocol::{ContentEnvironment, GitAction, GitBranches, GitChanges};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, SharedString, relative};
use std::collections::HashSet;

/// Branch names longer than this are elided in the Changes header.
const BRANCH_LABEL_CHARS: usize = 40;

/// The branch picker's part of the picker overlay (`FileTree::branches`).
pub(in crate::gui) struct BranchPicker {
    pub environment: ContentEnvironment,
    /// The repository's top level.
    pub repo: String,
    /// `HEAD` abbreviated, which a new branch starts from.
    pub head: Option<String>,
    /// The repository's branches; `None` until they are read.
    pub listing: Option<GitBranches>,
    /// Opened as Create branch…: the create row comes first.
    pub create: bool,
    /// What the picker shows for its query, in order.
    pub matches: Vec<BranchRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum BranchRow {
    /// The branch at this index of the listing.
    Branch(usize),
    /// A new branch of this name.
    Create(String),
}

/// A switch or create the daemon has not answered yet.
pub(in crate::gui) struct BranchWrite {
    /// The content pane whose Changes list the picker was opened from.
    pub pane_id: PaneId,
    pub repo: String,
    /// What it does, for the picker and the branch button: `Switching to main…`.
    pub label: String,
}

/// The rows for `query`: the branches matching it (every branch while it is empty), best
/// match first, then a row creating `query` when it names no local branch and Git could
/// take it as a branch name. With `create`, or when nothing matches, that row comes first.
/// A remote branch whose local branch exists is left out: it is reached through that one.
pub(super) fn branch_rows(listing: &GitBranches, query: &str, create: bool) -> Vec<BranchRow> {
    let query = query.trim();
    let locals: HashSet<&str> = listing
        .branches
        .iter()
        .filter(|branch| !branch.remote)
        .map(|branch| branch.name.as_str())
        .collect();
    let mut scored: Vec<(i32, usize)> = listing
        .branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| {
            !branch.remote
                || !branch
                    .name
                    .split_once('/')
                    .is_some_and(|(_, local)| locals.contains(local))
        })
        .filter_map(|(index, branch)| {
            if query.is_empty() {
                Some((0, index))
            } else {
                fuzzy_score(branch.name.as_bytes(), query.as_bytes()).map(|score| (score, index))
            }
        })
        .collect();
    // The listing's order (current, local, remote, newest first) breaks ties.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut rows: Vec<BranchRow> = scored
        .into_iter()
        .map(|(_, index)| BranchRow::Branch(index))
        .collect();
    if branch_name_allowed(query) && !locals.contains(query) {
        let row = BranchRow::Create(query.to_owned());
        if create || rows.is_empty() {
            rows.insert(0, row);
        } else {
            rows.push(row);
        }
    }
    rows
}

/// Whether Git takes `name` as a new branch name, as `git check-ref-format --branch` does
/// for names Compi offers to create; the daemon checks again before creating it.
pub(super) fn branch_name_allowed(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/'])
        && !name.ends_with(['/', '.'])
        && name != "@"
        && name != "HEAD"
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && !name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
        && name
            .split('/')
            .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
}

/// The Changes header's name for `HEAD`: its branch, or the commit it is detached at.
fn branch_label(listing: &GitChanges) -> String {
    match (&listing.branch, &listing.head) {
        (Some(branch), _) => truncate_middle(branch, BRANCH_LABEL_CHARS).into_owned(),
        (None, Some(head)) => format!("detached at {head}"),
        (None, None) => "No commits yet".to_owned(),
    }
}

/// `↑2 ↓1`: commits to push and to pull; nothing when there are neither.
fn ahead_behind(listing: &GitChanges) -> Option<String> {
    match (listing.ahead, listing.behind) {
        (0, 0) => None,
        (ahead, 0) => Some(format!("↑{ahead}")),
        (0, behind) => Some(format!("↓{behind}")),
        (ahead, behind) => Some(format!("↑{ahead} ↓{behind}")),
    }
}

/// How `HEAD` stands against its upstream, for the branch button's tooltip.
fn upstream_detail(listing: &GitChanges) -> String {
    let commits = |count: u32| {
        if count == 1 {
            "1 commit".to_owned()
        } else {
            format!("{count} commits")
        }
    };
    match (&listing.branch, &listing.head, &listing.upstream) {
        (None, ..) => "HEAD is detached: switch to a branch or create one here".to_owned(),
        (Some(_), None, _) => "No commits yet".to_owned(),
        (Some(branch), Some(_), None) => {
            format!("No upstream: Push runs git push -u origin {branch}")
        }
        (Some(_), Some(_), Some(upstream)) => match (listing.ahead, listing.behind) {
            (0, 0) => format!("Up to date with {upstream}"),
            (ahead, 0) => format!("{} to push to {upstream}", commits(ahead)),
            (0, behind) => format!("{} to pull from {upstream}", commits(behind)),
            (ahead, behind) => {
                format!("{} to push to {upstream}, {behind} to pull", commits(ahead))
            }
        },
    }
}

/// Git's own text for a refused write: the daemon's message without its error code.
fn refusal_text(error: &(dyn std::error::Error + 'static)) -> String {
    error
        .downcast_ref::<compi_protocol::DaemonError>()
        .map_or_else(|| error.to_string(), |error| error.message.clone())
}

impl CompiApp {
    /// Opens the branch picker over the content pane `pane_id`, for the repository its
    /// Changes list shows. `create` opens it as Create branch….
    pub(super) fn open_branch_picker(
        &mut self,
        pane_id: &PaneId,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((environment, listing)) = self.content.panes.get(pane_id).and_then(|ui| {
            ui.repo
                .listing()
                .map(|listing| (ui.environment.clone(), listing.clone()))
        }) else {
            self.global_error = Some("Open a folder in a Git repository first".into());
            return;
        };
        if self.file_tree.is_some() {
            self.close_file_tree();
        }
        let mut picker = tree::FileTree::new(pane_id.clone(), listing.repo.clone());
        picker.branches = Some(BranchPicker {
            environment: environment.clone(),
            repo: listing.repo.clone(),
            head: listing.head.clone(),
            listing: None,
            create,
            matches: Vec::new(),
        });
        self.report_focus(false);
        self.file_tree = Some(picker);
        self.tree_scroll.set_offset(point(px(0.0), px(0.0)));
        // Typed text reaches the picker through the window's input handler.
        window.focus(&self.focus_handle, cx);
        self.read_branches(environment, listing.repo, window, cx);
        cx.notify();
    }

    fn read_branches(
        &mut self,
        environment: ContentEnvironment,
        repo: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let (environment, repo) = (environment.clone(), repo.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.read_git_branches(environment, repo)
                        })()
                        .map_err(|error| refusal_text(error.as_ref()))
                    })
                    .await
            };
            this.update(cx, |this, cx| {
                let Some(tree) = this.file_tree.as_mut() else {
                    return;
                };
                let Some(picker) = tree
                    .branches
                    .as_mut()
                    .filter(|picker| picker.environment == environment && picker.repo == repo)
                else {
                    return;
                };
                match result {
                    Ok(listing) => {
                        picker.listing = Some(listing);
                        this.refresh_branch_matches();
                    }
                    Err(error) => tree.error = Some(format!("Cannot list the branches: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Filters the picker's branches by its query again, selecting the first row.
    pub(super) fn refresh_branch_matches(&mut self) {
        let Some(tree) = self.file_tree.as_mut() else {
            return;
        };
        let Some(picker) = tree.branches.as_mut() else {
            return;
        };
        picker.matches = picker
            .listing
            .as_ref()
            .map(|listing| branch_rows(listing, &tree.query, picker.create))
            .unwrap_or_default();
        tree.selected = 0;
        self.tree_scroll.scroll_to_item(0);
    }

    /// The picker's keys: type to filter, ↑/↓ to choose, Enter to switch or create, Escape
    /// to close.
    pub(super) fn handle_branch_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key.key.as_str() {
            "escape" => self.close_file_tree(),
            "up" => self.file_tree.as_mut().unwrap().move_selection(-1),
            "down" => self.file_tree.as_mut().unwrap().move_selection(1),
            "enter" => self.pick_branch(window, cx),
            "backspace" => {
                self.file_tree.as_mut().unwrap().query.pop();
                self.refresh_branch_matches();
            }
            _ if !key.modifiers.control && !key.modifiers.platform && !key.modifiers.alt => {
                if let Some(text) = key.key_char.as_deref() {
                    self.append_tree_text(text);
                }
            }
            _ => {}
        }
        if let Some(tree) = &self.file_tree {
            self.tree_scroll.scroll_to_item(tree.selected);
        }
        cx.notify();
    }

    /// Switches to the selected branch or creates the new one. The current branch only
    /// closes the picker; nothing starts while another switch runs.
    pub(super) fn pick_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_write.is_some() {
            return;
        }
        let Some(tree) = self.file_tree.as_mut() else {
            return;
        };
        let Some(picker) = tree.branches.as_ref() else {
            return;
        };
        let (action, label) = match picker.matches.get(tree.selected) {
            Some(BranchRow::Branch(index)) => {
                let Some(branch) = picker
                    .listing
                    .as_ref()
                    .and_then(|listing| listing.branches.get(*index))
                else {
                    return;
                };
                if branch.current {
                    self.close_file_tree();
                    return;
                }
                (
                    GitAction::SwitchBranch {
                        name: branch.name.clone(),
                    },
                    format!("Switching to {}…", branch.name),
                )
            }
            Some(BranchRow::Create(name)) => (
                GitAction::CreateBranch { name: name.clone() },
                format!("Creating {name}…"),
            ),
            None => return,
        };
        let (pane_id, environment, repo) = (
            tree.pane_id.clone(),
            picker.environment.clone(),
            picker.repo.clone(),
        );
        tree.error = None;
        self.branch_write = Some(BranchWrite {
            pane_id: pane_id.clone(),
            repo: repo.clone(),
            label,
        });
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let (environment, repo) = (environment.clone(), repo.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.git_write(environment, repo, action)
                        })()
                        .map(|_| ())
                        .map_err(|error| refusal_text(error.as_ref()))
                    })
                    .await
            };
            this.update_in(cx, |this, window, cx| {
                this.branch_written(&environment, &repo, result, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// A switch or create was answered: done closes the picker and reads the repository
    /// again; Git's refusal shows in the picker, or as the window's notice once it closed.
    fn branch_written(
        &mut self,
        environment: &ContentEnvironment,
        repo: &str,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.branch_write = None;
        let picker_open = self.file_tree.as_ref().is_some_and(|tree| {
            tree.branches
                .as_ref()
                .is_some_and(|picker| picker.environment == *environment && picker.repo == repo)
        });
        match result {
            Ok(()) => {
                if picker_open {
                    self.close_file_tree();
                }
                self.refresh_repo_listers(environment, repo, window, cx);
            }
            Err(error) if picker_open => {
                if let Some(tree) = self.file_tree.as_mut() {
                    tree.error = Some(error);
                }
            }
            Err(error) => self.global_error = Some(error),
        }
        cx.notify();
    }

    /// The Changes header's branch: a button that opens the branch picker, then how far
    /// the branch is from its upstream (`↑2 ↓1`), or a `no upstream` chip. The button
    /// spins while a switch runs.
    pub(super) fn render_branch_status(
        &self,
        pane_id: &PaneId,
        listing: &GitChanges,
        cx: &Context<Self>,
    ) -> AnyElement {
        let switching = self
            .branch_write
            .as_ref()
            .filter(|write| write.pane_id == *pane_id && same_folder(&write.repo, &listing.repo));
        let pane = pane_id.clone();
        let button = text_button("changes-branch", branch_label(listing))
            .icon(IconName::GitBranch)
            .header()
            .ghost()
            .tooltip(switching.map_or_else(
                || SharedString::new_static("Switch branch…"),
                |write| write.label.clone().into(),
            ))
            .detail(upstream_detail(listing))
            .loading(switching.is_some())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_branch_picker(&pane, false, window, cx);
            }));
        let no_upstream =
            listing.branch.is_some() && listing.head.is_some() && listing.upstream.is_none();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(button)
            .when_some(ahead_behind(listing), |status, counts| {
                status.child(
                    div()
                        .flex_none()
                        .font_family(self.typography.font.family.clone())
                        .child(counts),
                )
            })
            .when(no_upstream, |status| {
                status.child(controls::chip("no upstream", ChipTone::Neutral))
            })
            .into_any_element()
    }

    /// The picker's rows for its branch mode, as tall as file rows.
    pub(super) fn render_branch_rows(
        &self,
        tree: &tree::FileTree,
        picker: &BranchPicker,
        line_height: f32,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = *self.colors();
        let branches = picker
            .listing
            .as_ref()
            .map_or(&[][..], |listing| &listing.branches[..]);
        picker
            .matches
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let selected = index == tree.selected;
                let tint = color(if selected {
                    colors.foreground
                } else {
                    colors.muted
                });
                let (icon, label, note) = match row {
                    BranchRow::Branch(at) => {
                        let branch = &branches[*at];
                        let note = if branch.current {
                            Some("current".to_owned())
                        } else if branch.remote {
                            Some("remote".to_owned())
                        } else {
                            branch.upstream.clone()
                        };
                        (IconName::GitBranch, branch.name.clone(), note)
                    }
                    BranchRow::Create(name) => (
                        IconName::Plus,
                        format!("Create branch “{name}”"),
                        Some(match &picker.head {
                            Some(head) => format!("from {head}"),
                            None => "before the first commit".to_owned(),
                        }),
                    ),
                };
                div()
                    .id(("branch-row", index))
                    .h(px(line_height))
                    .w_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if let Some(tree) = this.file_tree.as_mut() {
                                tree.selected = index;
                            }
                            if event.click_count >= 2 {
                                this.pick_branch(window, cx);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .h(px(line_height - 2.0))
                            .w_full()
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_sm()
                            .when(selected, |entry| entry.bg(color(colors.selection)))
                            .when(!selected, |entry| {
                                entry.hover(move |style| style.bg(color(colors.surface_hover)))
                            })
                            .child(controls::glyph(icon, tint))
                            // The name keeps most of the row; the note gives way first.
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .max_w(relative(0.8))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(label),
                            )
                            .when_some(note, |entry, note| {
                                entry.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_color(color(colors.muted))
                                        .child(note),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The control at the right of the picker's header: Switch or Create branch for the
    /// selected row, with a spinner while a switch runs.
    pub(super) fn render_branch_action(
        &self,
        tree: &tree::FileTree,
        picker: &BranchPicker,
        cx: &Context<Self>,
    ) -> AnyElement {
        let creating = matches!(
            picker.matches.get(tree.selected),
            Some(BranchRow::Create(_))
        );
        let current = match picker.matches.get(tree.selected) {
            Some(BranchRow::Branch(index)) => picker
                .listing
                .as_ref()
                .and_then(|listing| listing.branches.get(*index))
                .is_some_and(|branch| branch.current),
            _ => false,
        };
        let running = self.branch_write.as_ref().map(|write| write.label.clone());
        let reason = if picker.listing.is_none() {
            Some("Reading the branches…")
        } else if picker.matches.get(tree.selected).is_none() {
            Some("No branch matches")
        } else if current {
            Some("This branch is checked out")
        } else {
            None
        };
        text_button(
            "branch-pick",
            if creating { "Create branch" } else { "Switch" },
        )
        .header()
        .tooltip(running.unwrap_or_else(|| {
            if creating {
                "Create the branch at HEAD and switch to it".to_owned()
            } else {
                "Switch to the selected branch".to_owned()
            }
        }))
        .detail("Uncommitted changes come along; Git refuses if they would be overwritten")
        .key("enter")
        .loading(self.branch_write.is_some())
        .disabled_because(reason)
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.pick_branch(window, cx);
        }))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::GitBranch;

    fn listing(names: &[(&str, bool, bool)]) -> GitBranches {
        GitBranches {
            branches: names
                .iter()
                .map(|&(name, current, remote)| GitBranch {
                    name: name.to_owned(),
                    current,
                    upstream: None,
                    remote,
                })
                .collect(),
            truncated: false,
        }
    }

    fn names(listing: &GitBranches, rows: &[BranchRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                BranchRow::Branch(index) => listing.branches[*index].name.clone(),
                BranchRow::Create(name) => format!("+{name}"),
            })
            .collect()
    }

    #[test]
    fn an_empty_query_lists_every_branch_but_remotes_of_local_ones() {
        let listing = listing(&[
            ("main", true, false),
            ("topic", false, false),
            ("origin/main", false, true),
            ("origin/feature", false, true),
        ]);
        assert_eq!(
            names(&listing, &branch_rows(&listing, "", false)),
            ["main", "topic", "origin/feature"]
        );
    }

    #[test]
    fn a_query_ranks_matches_and_offers_a_new_name() {
        let listing = listing(&[
            ("main", true, false),
            ("feature/login", false, false),
            ("origin/fix-login", false, true),
        ]);
        assert_eq!(
            names(&listing, &branch_rows(&listing, "login", false)),
            ["feature/login", "origin/fix-login", "+login"]
        );
        // Create branch… puts the new name first; an existing local name is not offered.
        assert_eq!(
            names(&listing, &branch_rows(&listing, "login", true)),
            ["+login", "feature/login", "origin/fix-login"]
        );
        assert_eq!(
            names(&listing, &branch_rows(&listing, "main", true)),
            ["main"]
        );
        // Nothing matches: the new name is the only row, selected.
        assert_eq!(
            names(&listing, &branch_rows(&listing, " zzz ", false)),
            ["+zzz"]
        );
    }

    #[test]
    fn names_git_refuses_are_not_offered() {
        for name in [
            "",
            "-f",
            "/a",
            "a/",
            "a.",
            "a..b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "@",
            "HEAD",
            "a@{1}",
            "a//b",
            ".a",
            "a/.b",
            "a.lock",
            "a/b.lock/c",
        ] {
            assert!(!branch_name_allowed(name), "{name:?}");
        }
        for name in ["main", "feature/login", "fix-1.2", "a@b", "x.y/z"] {
            assert!(branch_name_allowed(name), "{name:?}");
        }
    }
}

//! Fetch, Pull and Push: typed into a terminal beside the editor, so credential, SSH-key
//! and 2FA prompts work and the output stays visible.
//!
//! A terminal of the editor's tab that sits idle at a prompt in the repository folder gets
//! the command. Otherwise a new terminal opens beside the editor pane, in the repository
//! folder and its environment: the WSL distribution of a WSL folder, and the daemon's
//! default shell for a host folder (on Windows, WSL's view of the drive). The command is
//! then typed into it with Enter. Push never forces; a branch without an upstream pushes
//! with `-u origin <branch>`. The ahead and behind counts follow with the next read of the
//! repository.
use super::content::same_folder;
use super::*;
use crate::gui::controls::{Control, IconName, icon_button};
use compi_protocol::metadata::MetadataState;
use compi_protocol::{
    ContentEnvironment, GitChanges, MutationReceipt, SplitAxis, SplitGeometry, SurfaceStatus,
};
use gpui::{AnyElement, ClickEvent};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum RemoteCommand {
    Fetch,
    Pull,
    Push,
}

impl RemoteCommand {
    const ALL: [Self; 3] = [Self::Fetch, Self::Pull, Self::Push];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch",
            Self::Pull => "Pull",
            Self::Push => "Push",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Fetch => IconName::RefreshCw,
            Self::Pull => IconName::ArrowDown,
            Self::Push => IconName::ArrowUp,
        }
    }

    /// The command line typed for the repository of `listing`.
    pub(super) fn line(self, listing: &GitChanges) -> String {
        match (self, &listing.branch, &listing.upstream) {
            (Self::Fetch, ..) => "git fetch".to_owned(),
            (Self::Pull, ..) => "git pull".to_owned(),
            (Self::Push, Some(branch), None) => {
                format!("git push -u origin {}", shell_word(branch))
            }
            (Self::Push, ..) => "git push".to_owned(),
        }
    }

    /// Why it cannot run for the repository of `listing`.
    pub(super) fn refusal(self, listing: &GitChanges) -> Option<&'static str> {
        match self {
            Self::Fetch => None,
            Self::Pull if listing.branch.is_none() => Some("Switch to a branch to pull"),
            Self::Pull if listing.upstream.is_none() => {
                Some("This branch has no upstream to pull from")
            }
            Self::Push if listing.branch.is_none() => Some("Switch to a branch to push"),
            Self::Push if listing.head.is_none() => Some("Nothing to push before the first commit"),
            Self::Pull | Self::Push => None,
        }
    }
}

/// `word` as one shell word: as it is when nothing in it is special to a shell, else in
/// single quotes. Branch names may hold `$`, `;`, quotes and the like.
fn shell_word(word: &str) -> Cow<'_, str> {
    if !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./+@%=,".contains(&byte))
    {
        Cow::Borrowed(word)
    } else {
        Cow::Owned(format!("'{}'", word.replace('\'', r"'\''")))
    }
}

/// A Windows path as WSL mounts it by default: `C:\a\b` is `/mnt/c/a/b`.
fn mounted_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let rest = path[2..].replace('\\', "/");
    Some(format!(
        "/mnt/{}{}",
        char::from(bytes[0]).to_ascii_lowercase(),
        rest.trim_end_matches('/')
    ))
}

/// Whether a shell whose current directory is `cwd` (in `distribution`, or on the host) is
/// in the repository `repo` of `environment`.
fn in_repo(
    cwd: &str,
    distribution: Option<&str>,
    environment: &ContentEnvironment,
    repo: &str,
) -> bool {
    match environment {
        ContentEnvironment::Wsl { distribution: own } => {
            distribution == Some(own.as_str()) && same_folder(cwd, repo)
        }
        ContentEnvironment::Host => match mounted_path(repo) {
            // A Windows folder: the shells beside it run in WSL.
            Some(mounted) => {
                distribution.is_some() && cwd.trim_end_matches('/').eq_ignore_ascii_case(&mounted)
            }
            None => distribution.is_none() && same_folder(cwd, repo),
        },
    }
}

/// Whether the process in the foreground of a terminal is its shell: it waits at a prompt.
fn at_prompt(shell: &str, foreground: &str) -> bool {
    let shell = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
    shell.trim_start_matches('-') == foreground.trim_start_matches('-')
}

/// The terminal panes of `node`, with their surfaces.
fn terminal_leaves<'a>(node: &'a LayoutNode, leaves: &mut Vec<(&'a PaneId, &'a SurfaceId)>) {
    match node {
        LayoutNode::Pane {
            pane_id,
            surface_id,
        } => leaves.push((pane_id, surface_id)),
        LayoutNode::Content { .. } => {}
        LayoutNode::Split { first, second, .. } => {
            terminal_leaves(first, leaves);
            terminal_leaves(second, leaves);
        }
    }
}

/// Fetch, Pull or Push typed into a terminal the split for it opens, until it attaches.
struct RemoteInput {
    /// The split asked for; its receipt names the new terminal.
    mutation: MutationId,
    surface: Option<SurfaceId>,
    /// The command line and Enter.
    data: Vec<u8>,
}

/// A window's Fetch, Pull and Push.
#[derive(Default)]
pub(in crate::gui) struct RemoteState {
    /// The command waiting for the terminal opened for it.
    pending: Option<RemoteInput>,
    /// The terminal a command was last typed into, and when. It counts as busy until
    /// metadata asked for after that says its shell is back at the prompt, so a second
    /// click never types into the first command's password prompt.
    typed: Option<(SurfaceId, Instant)>,
}

impl CompiApp {
    /// The focused content pane and the repository it lists, or why Git commands cannot
    /// run from the focused pane.
    pub(super) fn git_pane(&self) -> Result<(PaneId, &GitChanges), &'static str> {
        let pane_id = self.focused_pane().ok_or("Select a pane first")?;
        self.focused_content()
            .ok_or("Select a file pane of a Git repository first")?;
        let listing = self
            .repo_changes(pane_id)
            .ok_or("This folder is not in a Git repository")?;
        Ok((pane_id.clone(), listing))
    }

    /// `command` for the repository the content pane `pane_id` lists, in a terminal
    /// beside it.
    pub(super) fn run_remote(
        &mut self,
        pane_id: &PaneId,
        command: RemoteCommand,
        window: &mut Window,
    ) {
        let Some((environment, listing)) = self.content.panes.get(pane_id).and_then(|ui| {
            ui.repo
                .listing()
                .map(|listing| (ui.environment.clone(), listing.clone()))
        }) else {
            self.global_error = Some("Open a folder in a Git repository first".into());
            return;
        };
        if let Some(reason) = command.refusal(&listing) {
            self.global_error = Some(reason.into());
            return;
        }
        let mut data = command.line(&listing).into_bytes();
        data.push(b'\r');
        if let Some(terminal) = self.idle_repo_terminal(pane_id, &environment, &listing.repo) {
            if let Some(view) = self
                .surface_views
                .iter_mut()
                .find(|view| view.pane_id == terminal)
            {
                view.scroll_offset = 0;
                view.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
                self.remote.typed = Some((view.surface_id.clone(), Instant::now()));
            }
            self.focus_pane(terminal);
            return;
        }
        self.open_remote_terminal(pane_id, environment, listing.repo, data, window);
    }

    /// A terminal of `pane_id`'s tab that is attached, idle at a prompt, and in the
    /// repository `repo`. Unknown idleness counts as busy: nothing is typed into a
    /// program that may be running.
    fn idle_repo_terminal(
        &self,
        pane_id: &PaneId,
        environment: &ContentEnvironment,
        repo: &str,
    ) -> Option<PaneId> {
        let workspace = self.workspace.as_ref()?;
        let tab = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| layout_contains_pane(&tab.layout, pane_id))?;
        let mut leaves = Vec::new();
        terminal_leaves(&tab.layout, &mut leaves);
        leaves.into_iter().find_map(|(terminal, surface_id)| {
            let surface = workspace.surface(surface_id)?;
            let view = self
                .surface_views
                .iter()
                .find(|view| &view.surface_id == surface_id)?;
            let snapshot = view.mirror.snapshot();
            let metadata = match &self.remote.typed {
                Some((typed, at)) if typed == surface_id => {
                    self.pane_metadata_since(surface_id, *at)?
                }
                _ => self.pane_metadata(surface_id)?,
            };
            let shell = metadata.shell_executable.as_deref()?;
            let foreground = metadata
                .process
                .value
                .as_ref()
                .filter(|_| metadata.process.state == MetadataState::Available)?;
            let cwd = metadata
                .directory
                .value
                .clone()
                .or_else(|| snapshot.and_then(|snapshot| snapshot.current_directory.clone()))?;
            (surface.status == SurfaceStatus::Running
                && view.transport.is_some()
                && !snapshot.is_some_and(|snapshot| snapshot.modes.alternate_screen)
                && at_prompt(shell, &foreground.name)
                && in_repo(&cwd, surface.wsl_distribution(), environment, repo))
            .then(|| terminal.clone())
        })
    }

    /// Splits a terminal off `pane_id`, to its right when it fits, else below, starting in
    /// `repo` of `environment`; `data` is typed once it attaches.
    fn open_remote_terminal(
        &mut self,
        pane_id: &PaneId,
        environment: ContentEnvironment,
        repo: String,
        data: Vec<u8>,
        window: &mut Window,
    ) {
        if self.state.is_floating(pane_id) {
            self.global_error = Some("Dock this pane before opening a terminal beside it".into());
            return;
        }
        let tab_id = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| layout_contains_pane(&tab.layout, pane_id))
                .map(|tab| tab.id.clone())
        });
        if tab_id.is_some_and(|tab_id| self.pane_zoom.clear(&tab_id)) {
            self.zoom_layout = None;
            self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
            self.rebuild_layout(window, true);
        }
        let Some(layout) = &self.layout else {
            return;
        };
        let new_pane = layout::PaneKind::Terminal;
        let mut refusal = "The editor pane no longer exists";
        let mut planned = None;
        for axis in [SplitAxis::Horizontal, SplitAxis::Vertical] {
            match layout.split_feasibility(pane_id, axis, new_pane) {
                Ok(rect) => {
                    planned = layout
                        .split_minimum(pane_id, new_pane)
                        .map(|minimum| (axis, rect, minimum));
                    break;
                }
                Err(reason) if axis == SplitAxis::Horizontal => refusal = reason,
                Err(_) => {}
            }
        }
        let Some((axis, rect, minimum)) = planned else {
            self.global_error = Some(refusal.into());
            return;
        };
        let distribution = match environment {
            ContentEnvironment::Wsl { distribution } => Some(distribution),
            ContentEnvironment::Host => None,
        };
        let operation = WorkspaceMutation::SplitPane {
            pane_id: pane_id.clone(),
            axis,
            cols: self.terminal_cols,
            rows: self.terminal_rows,
            working_directory: Some(repo),
            geometry: SplitGeometry {
                width: rect.width,
                height: rect.height,
                min_width: minimum.width,
                min_height: minimum.height,
                divider: self.metrics().divider_thickness,
            },
        };
        if let Some(mutation) = self.mutate_in(operation, true, distribution) {
            self.remote.pending = Some(RemoteInput {
                mutation,
                surface: None,
                data,
            });
        }
    }

    /// A split finished (`None`: it failed): the remote command waits for the terminal
    /// the receipt names, or goes with a failed split.
    pub(super) fn remote_split_finished(&mut self, receipt: Option<&MutationReceipt>) {
        let Some(input) = self
            .remote
            .pending
            .as_mut()
            .filter(|input| input.surface.is_none())
        else {
            return;
        };
        match receipt {
            Some(receipt) if receipt.mutation_id == input.mutation => {
                input.surface = receipt.affected_surfaces.first().cloned();
                if input.surface.is_none() {
                    self.remote.pending = None;
                }
            }
            Some(_) => {}
            None => self.remote.pending = None,
        }
    }

    /// The remote command to type into the terminal of view `view_id`, which just
    /// attached, if it was opened for one.
    pub(super) fn take_remote_input(&mut self, view_id: u64) -> Option<Vec<u8>> {
        let surface = &self
            .surface_views
            .iter()
            .find(|view| view.id == view_id)?
            .surface_id;
        if self.remote.pending.as_ref()?.surface.as_ref() != Some(surface) {
            return None;
        }
        let surface = surface.clone();
        let input = self.remote.pending.take()?;
        self.remote.typed = Some((surface, Instant::now()));
        Some(input.data)
    }

    /// Fetch, Pull and Push for the Changes header of `pane_id`: for the repository of
    /// `listing`, or disabled with the reason there is none.
    pub(super) fn remote_controls(
        &self,
        pane_id: &PaneId,
        listing: Result<&GitChanges, &'static str>,
        cx: &Context<Self>,
    ) -> [Control; 3] {
        RemoteCommand::ALL.map(|command| {
            let pane = pane_id.clone();
            let (reason, detail) = match listing {
                Ok(listing) => {
                    let line = command.line(listing);
                    let detail = if command == RemoteCommand::Push && listing.upstream.is_none()
                    {
                        format!("Runs {line} in a terminal beside the editor: the branch has no upstream yet")
                    } else {
                        format!("Runs {line} in a terminal beside the editor")
                    };
                    (command.refusal(listing), detail)
                }
                Err(reason) => (Some(reason), String::new()),
            };
            let control = icon_button(
                ("changes-remote", command as usize),
                command.icon(),
                command.label(),
            )
            .header()
            .disabled_because(reason)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.run_remote(&pane, command, window);
                cx.notify();
            }));
            if detail.is_empty() {
                control
            } else {
                control.detail(detail)
            }
        })
    }

    /// The Changes list's `message` while there is no repository listing, under a header
    /// whose Fetch, Pull and Push are disabled with `reason`.
    pub(super) fn without_listing(
        &self,
        pane_id: &PaneId,
        reason: &'static str,
        message: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .pl_3()
                    .pr_1()
                    .py(px(2.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(2.0))
                    .children(self.remote_controls(pane_id, Err(reason), cx)),
            )
            .child(message)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(branch: Option<&str>, head: Option<&str>, upstream: Option<&str>) -> GitChanges {
        GitChanges {
            repo: "/r".into(),
            branch: branch.map(Into::into),
            head: head.map(Into::into),
            upstream: upstream.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn push_sets_an_upstream_only_when_there_is_none() {
        let tracked = listing(Some("main"), Some("abc1234"), Some("origin/main"));
        assert_eq!(RemoteCommand::Push.line(&tracked), "git push");
        assert_eq!(RemoteCommand::Pull.line(&tracked), "git pull");
        assert_eq!(RemoteCommand::Fetch.line(&tracked), "git fetch");
        let new = listing(Some("feature/a-1"), Some("abc1234"), None);
        assert_eq!(
            RemoteCommand::Push.line(&new),
            "git push -u origin feature/a-1"
        );
        let odd = listing(Some("it's$x"), Some("abc1234"), None);
        assert_eq!(
            RemoteCommand::Push.line(&odd),
            r"git push -u origin 'it'\''s$x'"
        );
    }

    #[test]
    fn pull_and_push_need_a_branch() {
        let detached = listing(None, Some("abc1234"), None);
        assert!(RemoteCommand::Fetch.refusal(&detached).is_none());
        assert!(RemoteCommand::Pull.refusal(&detached).is_some());
        assert!(RemoteCommand::Push.refusal(&detached).is_some());
        let unborn = listing(Some("main"), None, None);
        assert!(RemoteCommand::Push.refusal(&unborn).is_some());
        let untracked = listing(Some("main"), Some("abc1234"), None);
        assert!(RemoteCommand::Pull.refusal(&untracked).is_some());
        assert!(RemoteCommand::Push.refusal(&untracked).is_none());
    }

    #[test]
    fn shells_in_the_repository_folder_and_environment_match() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert!(in_repo("/home/u/r/", Some("Ubuntu"), &wsl, "/home/u/r"));
        assert!(!in_repo("/home/u/r", Some("Debian"), &wsl, "/home/u/r"));
        assert!(!in_repo("/home/u/r/src", Some("Ubuntu"), &wsl, "/home/u/r"));
        let host = ContentEnvironment::Host;
        assert!(in_repo(
            "/mnt/c/Users/u/R",
            Some("Ubuntu"),
            &host,
            r"C:\Users\u\r"
        ));
        assert!(in_repo(
            "/mnt/c/users/u/r",
            Some("Ubuntu"),
            &host,
            "C:/Users/u/r/"
        ));
        assert!(!in_repo(
            "/mnt/d/Users/u/r",
            Some("Ubuntu"),
            &host,
            r"C:\Users\u\r"
        ));
        assert!(in_repo("/Users/u/r", None, &host, "/Users/u/r"));
        assert!(!in_repo("/Users/u/r", Some("Ubuntu"), &host, "/Users/u/r"));
    }

    #[test]
    fn a_shell_in_the_foreground_waits_at_a_prompt() {
        assert!(at_prompt("/bin/bash", "bash"));
        assert!(at_prompt("/usr/bin/zsh", "-zsh"));
        assert!(!at_prompt("/bin/bash", "git"));
    }
}

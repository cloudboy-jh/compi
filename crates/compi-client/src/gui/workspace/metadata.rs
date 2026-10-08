//! Refresh away from paint and PTY I/O. One background batch per window at a time.
use super::*;
use compi_protocol::metadata::{MetadataField, MetadataState, PaneMetadata};
use std::sync::mpsc;

type MetadataResult = (
    SurfaceId,
    compi_protocol::ProcessLifetimeId,
    Result<PaneMetadata, String>,
);

pub(in crate::gui) struct MetadataUi {
    entries: HashMap<SurfaceId, (Instant, PaneMetadata)>,
    pending: Option<mpsc::Receiver<Vec<MetadataResult>>>,
    attempted: HashMap<SurfaceId, Instant>,
    next_refresh: Instant,
}

impl Default for MetadataUi {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            attempted: HashMap::new(),
            pending: None,
            next_refresh: Instant::now(),
        }
    }
}

fn retain_caption_field<T>(current: &mut MetadataField<T>, previous: &mut MetadataField<T>) {
    if current.state != MetadataState::Available && previous.value.is_some() {
        current.value = previous.value.take();
        current.state = MetadataState::Stale;
    }
}

impl CompiApp {
    pub(super) fn poll_metadata(&mut self, cx: &mut Context<Self>) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        for (at, value) in self.metadata.entries.values_mut() {
            if (at.elapsed() > Duration::from_secs(12) || self.connection_error.is_some())
                && [value.directory.state, value.process.state, value.git.state]
                    .contains(&compi_protocol::metadata::MetadataState::Available)
            {
                value.mark_stale("metadata is no longer current");
                cx.notify();
            }
        }
        if let Some(receiver) = &self.metadata.pending {
            match receiver.try_recv() {
                Ok(results) => {
                    self.metadata.pending = None;
                    for (id, lifetime, result) in results {
                        let valid = self
                            .workspace
                            .as_ref()
                            .and_then(|workspace| workspace.surface(&id))
                            .is_some_and(|surface| surface.process_lifetime_id == lifetime);
                        if !valid {
                            self.metadata.entries.remove(&id);
                            continue;
                        }
                        match result {
                            Ok(mut value) => {
                                let live = self.connection_error.is_none()
                                    && self
                                        .workspace
                                        .as_ref()
                                        .and_then(|workspace| workspace.surface(&id))
                                        .is_some_and(|surface| {
                                            surface.status == SurfaceStatus::Running
                                        });
                                if !live {
                                    value.mark_stale("shell metadata is no longer current");
                                }
                                if let Some((_, previous)) = self.metadata.entries.get_mut(&id)
                                    && previous.process_lifetime_id == value.process_lifetime_id
                                {
                                    retain_caption_field(
                                        &mut value.directory,
                                        &mut previous.directory,
                                    );
                                    retain_caption_field(&mut value.process, &mut previous.process);
                                    retain_caption_field(&mut value.git, &mut previous.git);
                                    if value.state != MetadataState::Available {
                                        value.dimensions = previous.dimensions.clone();
                                    }
                                }
                                self.metadata.entries.insert(id, (Instant::now(), value));
                            }
                            Err(reason) => {
                                if let Some((_, value)) = self.metadata.entries.get_mut(&id) {
                                    value.mark_stale(&reason);
                                }
                            }
                        }
                    }
                    cx.notify();
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.metadata.pending = None;
                }
            }
        }
        if self.connection_error.is_some() {
            return;
        }
        if !self.config.metadata.directory
            && !self.config.metadata.process
            && !self.config.metadata.git
            && !self.config.metadata.dimensions
        {
            return;
        }
        if Instant::now() < self.metadata.next_refresh {
            return;
        }
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.metadata.entries.retain(|id, (_, value)| {
            workspace
                .surface(id)
                .is_some_and(|surface| surface.process_lifetime_id == value.process_lifetime_id)
        });
        self.metadata
            .attempted
            .retain(|id, _| workspace.surface(id).is_some());
        let mut surfaces: Vec<_> = workspace
            .surfaces
            .iter()
            .filter(|surface| {
                if surface.status != SurfaceStatus::Running {
                    return false;
                }
                let attempted = self.metadata.attempted.get(&surface.id);
                let directory_changed = self
                    .surface_views
                    .iter()
                    .find(|view| view.surface_id == surface.id)
                    .and_then(|view| view.mirror.snapshot())
                    .is_some_and(|snapshot| {
                        self.metadata
                            .entries
                            .get(&surface.id)
                            .is_some_and(|(_, metadata)| {
                                metadata.directory.value != snapshot.current_directory
                            })
                    });
                attempted.is_none_or(|at| {
                    at.elapsed() >= Duration::from_secs(5)
                        || (directory_changed && at.elapsed() >= Duration::from_secs(1))
                })
            })
            .collect();
        surfaces.sort_by_key(|surface| {
            let attempted = self.metadata.attempted.get(&surface.id);
            let visible = self
                .surface_views
                .iter()
                .any(|view| view.surface_id == surface.id);
            let overdue = attempted.is_none_or(|at| at.elapsed() >= Duration::from_secs(15));
            (!overdue && !visible, attempted.copied())
        });
        let surfaces: Vec<_> = surfaces.into_iter().take(4).cloned().collect();
        for surface in &surfaces {
            self.metadata
                .attempted
                .insert(surface.id.clone(), Instant::now());
        }
        self.metadata.next_refresh = Instant::now() + Duration::from_secs(1);
        if surfaces.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.metadata.pending = Some(receiver);
        let target = self.target.clone();
        thread::spawn(move || {
            let result = target.connect_existing();
            let mut results = Vec::new();
            match result {
                Ok(mut client) => {
                    for surface in surfaces {
                        let value = client
                            .surface_metadata(&surface)
                            .map_err(|error| error.to_string());
                        results.push((surface.id, surface.process_lifetime_id, value));
                    }
                }
                Err(error) => {
                    for surface in surfaces {
                        results.push((
                            surface.id,
                            surface.process_lifetime_id,
                            Err(error.to_string()),
                        ));
                    }
                }
            }
            let _ = sender.send(results);
        });
    }

    pub(super) fn pane_metadata(&self, id: &SurfaceId) -> Option<&PaneMetadata> {
        let surface = self.workspace.as_ref()?.surface(id)?;
        let (_, value) = self.metadata.entries.get(id)?;
        if value.process_lifetime_id != surface.process_lifetime_id {
            return None;
        }
        Some(value)
    }

    pub(super) fn metadata_tab_caption(
        &self,
        tab: &WorkspaceTab,
        panes: &[TabPane],
    ) -> (String, Option<String>) {
        let focused = self
            .focused_view
            .and_then(|id| self.surface_views.iter().find(|view| view.id == id))
            .map(|view| &view.pane_id);
        let pane = focused
            .and_then(|focused| panes.iter().find(|pane| &pane.pane_id == focused))
            .or_else(|| panes.first());
        let fallback = pane.map_or("Terminal", |pane| pane.title.as_str());
        let metadata = pane
            .and_then(|pane| self.tab_pane_target(&tab.id, &pane.pane_id))
            .and_then(|(_, id)| self.pane_metadata(&id));
        let fields = metadata
            .map(|metadata| crate::metadata::caption_fields(metadata, &self.config.metadata))
            .unwrap_or_default();
        let status = metadata
            .filter(|_| self.connection_error.is_none())
            .and_then(|metadata| crate::metadata::caption_status(metadata, &self.config.metadata));
        crate::metadata::aggregate_caption(&tab.label, fallback, &fields, panes.len(), status)
    }
}

#[cfg(test)]
mod tests {
    use super::{MetadataField, MetadataState, retain_caption_field};

    #[test]
    fn metadata_gaps_preserve_last_meaningful_caption_field() {
        let mut previous = MetadataField::available("project".to_owned());
        let mut unavailable = MetadataField::unavailable("shell disconnected");
        retain_caption_field(&mut unavailable, &mut previous);
        assert_eq!(unavailable.value.as_deref(), Some("project"));
        assert_eq!(unavailable.state, MetadataState::Stale);
        assert_eq!(unavailable.reason.as_deref(), Some("shell disconnected"));

        let mut stale = MetadataField::available("old-project".to_owned());
        stale.mark_stale("collection expired");
        retain_caption_field(&mut stale, &mut unavailable);
        assert_eq!(stale.value.as_deref(), Some("project"));
        assert_eq!(stale.state, MetadataState::Stale);
    }

    #[test]
    fn fresh_observations_replace_names_and_clear_absent_git() {
        let mut previous = MetadataField::available("main".to_owned());
        let mut current = MetadataField::available("release".to_owned());
        retain_caption_field(&mut current, &mut previous);
        assert_eq!(current.value.as_deref(), Some("release"));
        assert_eq!(current.state, MetadataState::Available);

        let mut not_a_worktree = MetadataField {
            state: MetadataState::Available,
            value: None,
            reason: None,
        };
        retain_caption_field(&mut not_a_worktree, &mut current);
        assert_eq!(not_a_worktree.value, None);
        assert_eq!(not_a_worktree.state, MetadataState::Available);
    }
}

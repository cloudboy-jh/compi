//! Program status (OSC 7501) of every terminal. One watching connection per window
//! follows all of them, attached or not, so background tabs show what needs the user.
use super::*;
use crate::gui::controls::IconName;
use crate::program_status::{self as status, Headline};
use compi_protocol::program_status::{ProgramState, ProgramStatusRecord, SurfaceProgramStatus};
use std::sync::mpsc;

/// How often the watching connection looks for status and acknowledgements.
const WATCH_POLL: Duration = Duration::from_millis(50);
/// Wait before reconnecting after the daemon connection failed or was released.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

type Acknowledgement = (SurfaceId, compi_protocol::ProcessLifetimeId);

pub(in crate::gui) struct ProgramStatusUi {
    statuses: HashMap<SurfaceId, SurfaceProgramStatus>,
    acknowledgements: mpsc::Sender<Acknowledgement>,
}

impl ProgramStatusUi {
    /// Starts the window's watching connection. It stops when the window closes, and
    /// stays released while an update holds `quiesced`.
    pub(in crate::gui) fn start(
        target: ConnectionTarget,
        sender: UiEventSender,
        quiesced: Arc<AtomicBool>,
        connection_guard: Arc<Mutex<()>>,
    ) -> Self {
        let (acknowledgements, requests) = mpsc::channel();
        thread::spawn(move || watch(target, sender, requests, quiesced, connection_guard));
        Self {
            statuses: HashMap::new(),
            acknowledgements,
        }
    }

    pub(in crate::gui) fn replace_all(&mut self, statuses: Vec<SurfaceProgramStatus>) {
        self.statuses = statuses
            .into_iter()
            .map(|status| (status.surface_id.clone(), status))
            .collect();
    }

    pub(in crate::gui) fn update(&mut self, status: SurfaceProgramStatus) {
        if status.records.is_empty() {
            self.statuses.remove(&status.surface_id);
        } else {
            self.statuses.insert(status.surface_id.clone(), status);
        }
    }

    /// What `surface_id` last reported, of whichever process.
    pub(in crate::gui) fn status(&self, surface_id: &SurfaceId) -> Option<&SurfaceProgramStatus> {
        self.statuses.get(surface_id)
    }
}

#[derive(PartialEq, Eq)]
enum WatchEnd {
    WindowClosed,
    Reconnect,
}

fn watch(
    target: ConnectionTarget,
    sender: UiEventSender,
    requests: mpsc::Receiver<Acknowledgement>,
    quiesced: Arc<AtomicBool>,
    connection_guard: Arc<Mutex<()>>,
) {
    let mut showing = false;
    loop {
        let client = {
            let Ok(_guard) = connection_guard.lock() else {
                return;
            };
            if quiesced.load(Ordering::Acquire) {
                None
            } else {
                target.connect().ok()
            }
        };
        if let Some(mut client) = client
            && follow(&mut client, &sender, &requests, &quiesced, &mut showing)
                == WatchEnd::WindowClosed
        {
            return;
        }
        // What a lost connection reported is no longer known.
        if std::mem::take(&mut showing) && !sender.send(UiEvent::ProgramStatusWatched(Vec::new())) {
            return;
        }
        thread::sleep(RECONNECT_DELAY);
        if matches!(requests.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
            return;
        }
    }
}

fn follow(
    client: &mut DaemonClient,
    sender: &UiEventSender,
    requests: &mpsc::Receiver<Acknowledgement>,
    quiesced: &AtomicBool,
    showing: &mut bool,
) -> WatchEnd {
    if client.watch_program_status().is_err() {
        return WatchEnd::Reconnect;
    }
    let current = client.program_status().values().cloned().collect();
    if !sender.send(UiEvent::ProgramStatusWatched(current)) {
        return WatchEnd::WindowClosed;
    }
    *showing = true;
    loop {
        if quiesced.load(Ordering::Acquire) {
            return WatchEnd::Reconnect;
        }
        loop {
            match requests.try_recv() {
                Ok((surface_id, expected_lifetime)) => {
                    let request = ClientMessage::AcknowledgeProgramStatus {
                        surface_id,
                        expected_lifetime,
                    };
                    if client.send(request).is_err() {
                        return WatchEnd::Reconnect;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return WatchEnd::WindowClosed,
            }
        }
        match client.poll_event() {
            Ok(Some(ServerEvent::Control {
                message: ServerMessage::ProgramStatusChanged { status },
                ..
            })) => {
                if !sender.send(UiEvent::ProgramStatusChanged(status)) {
                    return WatchEnd::WindowClosed;
                }
            }
            // Acknowledgement replies and workspace revisions.
            Ok(Some(_)) => {}
            Ok(None) => thread::sleep(WATCH_POLL),
            Err(_) => return WatchEnd::Reconnect,
        }
    }
}

impl CompiApp {
    /// The surface's records, when they belong to its current process.
    pub(super) fn program_records(&self, surface_id: &SurfaceId) -> &[ProgramStatusRecord] {
        let Some(reported) = self.program_status.statuses.get(surface_id) else {
            return &[];
        };
        let current = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(surface_id))
            .is_some_and(|surface| surface.process_lifetime_id == reported.process_lifetime_id);
        if current { &reported.records } else { &[] }
    }

    /// The state the tab's panes most need the user for.
    pub(super) fn tab_program_headline(&self, tab: &WorkspaceTab) -> Option<Headline> {
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        status::combine(
            leaves
                .iter()
                .filter_map(|(_, surface)| surface.as_ref())
                .filter_map(|surface| status::headline(self.program_records(surface))),
        )
    }

    /// Hover text for the tab's program status. A tab of one pane has its name as the
    /// tooltip title, so only its records; otherwise every reporting pane by name, then
    /// its records, so a program cannot pose as one in another terminal.
    pub(super) fn tab_program_detail(&self, tab: &WorkspaceTab) -> String {
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        if let [(_, Some(surface))] = leaves.as_slice() {
            return self.pane_program_detail(surface);
        }
        let titles = self.tab_panes(tab);
        let mut lines = Vec::new();
        for (pane_id, surface) in &leaves {
            let Some(surface) = surface else { continue };
            let records = self.program_records(surface);
            if records.is_empty() {
                continue;
            }
            let title = titles
                .iter()
                .find(|pane| &pane.pane_id == pane_id)
                .map_or("Terminal", |pane| pane.title.as_str());
            lines.push(status::display_text(title));
            lines.extend(
                status::record_lines(records)
                    .into_iter()
                    .map(|line| format!("  {line}")),
            );
        }
        lines.join("\n")
    }

    /// Hover text for one pane: its name is the tooltip title, so only its records.
    fn pane_program_detail(&self, surface_id: &SurfaceId) -> String {
        status::record_lines(self.program_records(surface_id)).join("\n")
    }

    fn program_state_color(&self, state: ProgramState) -> u32 {
        let colors = self.colors();
        match state {
            ProgramState::Idle => colors.muted,
            ProgramState::Working => colors.accent,
            ProgramState::Done => colors.added,
            ProgramState::Blocked => colors.modified,
            ProgramState::Error => colors.error,
        }
    }

    /// The glyph for `state`, in its color.
    fn program_state_glyph(&self, state: ProgramState) -> gpui_component::Icon {
        let icon = match state {
            ProgramState::Idle => IconName::Circle,
            ProgramState::Working => IconName::LoaderCircle,
            ProgramState::Done => IconName::CircleCheck,
            ProgramState::Blocked => IconName::CircleAlert,
            ProgramState::Error => IconName::CircleX,
        };
        controls::glyph(icon, color(self.program_state_color(state)))
    }

    /// The surface's status glyph, when its program reports one: a pane row in the
    /// tab menu.
    pub(super) fn program_glyph(&self, surface_id: &SurfaceId) -> Option<gpui_component::Icon> {
        let headline = status::headline(self.program_records(surface_id))?;
        Some(self.program_state_glyph(headline.state))
    }

    /// A static glyph for the state, with its progress when `show_progress` (narrow tab
    /// chips leave it to the tooltip). Static, so status never makes the window repaint
    /// on its own.
    pub(super) fn program_status_glyph(&self, headline: Headline, show_progress: bool) -> Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(self.program_state_glyph(headline.state))
            .when_some(
                headline.progress.filter(|_| show_progress),
                |status, progress| {
                    status.child(
                        div()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .child(format!("{progress}%")),
                    )
                },
            )
    }

    /// `program_status_glyph` with the tooltip `title` and `detail`; the records in
    /// `detail` say the state, so the title does not repeat it.
    pub(super) fn render_program_status(
        &self,
        id: impl Into<gpui::ElementId>,
        headline: Headline,
        show_progress: bool,
        title: String,
        detail: String,
    ) -> AnyElement {
        self.program_status_glyph(headline, show_progress)
            .id(id)
            .tooltip(controls::tooltip(
                title,
                (!detail.is_empty()).then(|| detail.into()),
                None,
            ))
            .into_any_element()
    }

    /// Hover for a tab chip: its full `title`, and with `show_status` the records of
    /// its panes (the chip's glyph shows the state they most need the user for).
    pub(super) fn tab_tooltip(
        &self,
        tab: &WorkspaceTab,
        title: &str,
        show_status: bool,
    ) -> (String, Option<SharedString>) {
        let title = status::display_text(title);
        if !show_status {
            return (title, None);
        }
        let detail = self.tab_program_detail(tab);
        (title, (!detail.is_empty()).then(|| detail.into()))
    }

    /// The tiled pane's status: a dot in the top padding band, which never covers a
    /// cell, with the pane's records on hover.
    pub(super) fn render_pane_program_status(
        &self,
        index: usize,
        pane_id: &PaneId,
        surface_id: &SurfaceId,
    ) -> Option<AnyElement> {
        let headline = status::headline(self.program_records(surface_id))?;
        let (title, _) = self.floating_caption(pane_id);
        let tint = color(self.program_state_color(headline.state));
        Some(
            div()
                .id(("pane-program-status", index))
                .absolute()
                .top_0()
                .right(px(2.0 * PANE_RADIUS))
                .w(px(3.0 * TERMINAL_PADDING))
                .h(px(TERMINAL_PADDING))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(6.0)).rounded_full().bg(tint))
                .tooltip(controls::tooltip(
                    status::display_text(&title),
                    Some(self.pane_program_detail(surface_id).into()),
                    None,
                ))
                .into_any_element(),
        )
    }

    /// The floating pane's status for its title strip.
    pub(super) fn render_floating_program_status(
        &self,
        index: usize,
        surface_id: &SurfaceId,
        title: &str,
    ) -> Option<AnyElement> {
        let headline = status::headline(self.program_records(surface_id))?;
        Some(self.render_program_status(
            ("floating-program-status", index),
            headline,
            true,
            status::display_text(title),
            self.pane_program_detail(surface_id),
        ))
    }

    /// A keypress in a pane means the user has seen its finished results. They are
    /// removed here at once, so typing sends one acknowledgement, not one per key.
    pub(in crate::gui) fn acknowledge_program_results(
        &mut self,
        surface_id: &SurfaceId,
        lifetime: &compi_protocol::ProcessLifetimeId,
    ) {
        if !status::has_results(self.program_records(surface_id)) {
            return;
        }
        if let Some(reported) = self.program_status.statuses.get_mut(surface_id) {
            reported
                .records
                .retain(|record| !matches!(record.state, ProgramState::Done | ProgramState::Error));
        }
        let _ = self
            .program_status
            .acknowledgements
            .send((surface_id.clone(), lifetime.clone()));
    }
}

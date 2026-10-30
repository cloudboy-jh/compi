//! Notifications: a terminal's program reporting (OSC 7501) that it needs the user
//! (`blocked`: a permission, a question or a sign-in) or that its work is done tells
//! the user, in Compi (a toast at the bottom right of the window showing the pane) or
//! from the system, and a click goes to its pane.
//!
//! Only transitions count: a record entering `blocked` or `done` in a status change.
//! The snapshot a window's watching connection starts with (and restarts with after a
//! reconnect) sets the baseline and is never announced. Records of another process of
//! the same terminal are not the baseline of a new one. Every window watches every
//! terminal, so one window per connection target, the oldest open, announces: the
//! others stay silent. Nothing here acknowledges status; a toast's × only hides it.
//!
//! The pane the user is using stays silent: the focused pane of the active Compi
//! window, until `focused_pane_after` seconds pass without a key press, click or
//! scroll in that window (0: it stays silent while focused).
//!
//! A system notification carries its own sound; a toast shown only in Compi plays it
//! through the same system quiet checks.

use super::settings::{RowCx, SettingsAction};
use super::*;
use crate::config::{NotificationDelivery, NotificationSound};
use crate::gui::controls::IconName;
use crate::gui::os_notifications::{self, OsNotice, OsStatus, OsSubmit, SoundPlayed};
use crate::program_status::{display_text, inherited_app};
use compi_protocol::ProcessLifetimeId;
use compi_protocol::program_status::{
    BlockedKind, ProgramState, ProgramStatusRecord, SurfaceProgramStatus,
};
use gpui::WindowId;
use std::cell::RefCell;

/// How long a toast stays; what still needs the user stays on its tab and pane glyphs.
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
/// The most toasts a window shows at once; older ones go first.
const MAX_TOASTS: usize = 3;
/// Longest reported message a notification repeats.
const MAX_DETAIL_CHARS: usize = 140;
/// How long the system's answer about notifications is reused.
const OS_STATUS_TTL: Duration = Duration::from_secs(3);
/// The waits Settings offers for Focused pane, in seconds; 0 is Never.
const FOCUSED_PANE_CHOICES: [u32; 6] = [0, 15, 30, 60, 120, 300];
/// What Settings and test results call the system that shows notifications.
const SYSTEM_NAME: &str = if cfg!(windows) {
    "Windows"
} else if cfg!(target_os = "macos") {
    "macOS"
} else {
    "The system"
};
/// The system whose sounds a shared configuration may name that this one can't play.
const OTHER_SYSTEM_NAME: &str = if cfg!(target_os = "macos") {
    "Windows"
} else {
    "macOS"
};

/// What a notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum NoticeKind {
    Attention,
    Finished,
    Test,
}

/// One notification: what it says and the terminal it opens.
#[derive(Clone, Debug)]
pub(in crate::gui) struct Notice {
    kind: NoticeKind,
    /// The terminal and its process; none for a test.
    target: Option<(SurfaceId, ProcessLifetimeId)>,
    title: String,
    /// The program's message, bounded; may be empty.
    message: String,
    /// The tab and pane it comes from.
    place: String,
}

struct Toast {
    id: u64,
    notice: Notice,
    shown: Instant,
}

/// This window's toasts and Settings' notification state.
pub(in crate::gui) struct NotificationsUi {
    window_id: WindowId,
    /// The window is the active one: it has the system's keyboard focus. Followed from
    /// activation changes; GPUI's `active_window` stays set while another app is active.
    pub(super) window_active: bool,
    toasts: Vec<Toast>,
    next_id: u64,
    /// What the last test did, for Settings.
    test_result: Option<String>,
    /// What the last Play did, for Settings.
    sound_result: Option<String>,
    os_status: RefCell<Option<(Instant, OsStatus)>>,
    /// The last key press, click or scroll in this window.
    last_input: Instant,
}

/// Every Compi window by the target it connects to, oldest first: the first open one
/// of a target announces for it.
#[derive(Default)]
struct NotificationWindows(Vec<(WindowId, ConnectionTarget)>);

impl gpui::Global for NotificationWindows {}

/// The terminal each system notification opens, by tag.
#[derive(Default)]
struct NotificationRoutes(HashMap<String, (ConnectionTarget, SurfaceId)>);

impl gpui::Global for NotificationRoutes {}

impl NotificationsUi {
    pub(in crate::gui) fn new(
        window_id: WindowId,
        window_active: bool,
        target: &ConnectionTarget,
        cx: &mut App,
    ) -> Self {
        let live: Vec<WindowId> = cx
            .windows()
            .iter()
            .map(|window| window.window_id())
            .collect();
        let windows = &mut cx.default_global::<NotificationWindows>().0;
        windows.retain(|(id, _)| live.contains(id));
        windows.push((window_id, target.clone()));
        Self {
            window_id,
            window_active,
            toasts: Vec::new(),
            next_id: 0,
            test_result: None,
            sound_result: None,
            os_status: RefCell::new(None),
            last_input: Instant::now(),
        }
    }

    /// The user pressed a key, clicked or scrolled in this window.
    pub(in crate::gui) fn input_seen(&mut self) {
        self.last_input = Instant::now();
    }

    pub(in crate::gui) fn test_result(&self) -> Option<&str> {
        self.test_result.as_deref()
    }

    /// Whether the system shows Compi's notifications, asked at most every few seconds.
    pub(in crate::gui) fn os_status(&self) -> OsStatus {
        let mut cached = self.os_status.borrow_mut();
        if let Some((at, status)) = cached.as_ref()
            && at.elapsed() < OS_STATUS_TTL
        {
            return status.clone();
        }
        let status = os_notifications::status();
        *cached = Some((Instant::now(), status.clone()));
        status
    }
}

/// Once per process: system notification clicks open their terminal.
pub(in crate::gui) fn init(target: &ConnectionTarget, cx: &mut App) {
    let instance = target.state_instance();
    os_notifications::init(cx, instance.as_deref(), open_route);
}

/// Where each delivery goes: in Compi, from the system.
pub(in crate::gui) fn channels(delivery: NotificationDelivery, app_active: bool) -> (bool, bool) {
    match delivery {
        NotificationDelivery::Automatic => (app_active, !app_active),
        NotificationDelivery::InApp => (true, false),
        NotificationDelivery::Os => (false, true),
        NotificationDelivery::Both => (true, true),
    }
}

/// Whether Compi plays the notification sound itself: the event asks for one and shows
/// in Compi, and no system notification carries it (so Both plays it once).
fn sound_in_compi(wants_sound: bool, in_app: bool, system_sound: bool) -> bool {
    wants_sound && in_app && !system_sound
}

/// What a notification's sound or Play did, for Settings.
fn sound_line(sound: NotificationSound, played: Result<SoundPlayed, String>) -> String {
    let name = sound.on_this_system().label().to_lowercase();
    match played {
        Ok(SoundPlayed::Played) => format!("Played the {name} sound"),
        Ok(SoundPlayed::Handed) => format!(
            "Sent the {name} sound to {SYSTEM_NAME}; Focus or its notification sound setting can keep it quiet"
        ),
        Err(why) => format!("No sound: {why}"),
    }
}

/// Whether the focused pane of the active window is in use: Never (0) keeps it in use
/// while focused; otherwise input within the last `after` seconds does.
fn focused_pane_in_use(since_input: Duration, after: u32) -> bool {
    after == 0 || since_input < Duration::from_secs(u64::from(after))
}

/// What Focused pane's dropdown calls a wait.
fn focused_pane_label(seconds: u32) -> String {
    match seconds {
        0 => "Never".into(),
        seconds => format!("After {}", wait_text(seconds)),
    }
}

/// A wait of `seconds`, in words: whole minutes as minutes.
fn wait_text(seconds: u32) -> String {
    match seconds {
        1 => "1 second".into(),
        60 => "1 minute".into(),
        seconds if seconds % 60 == 0 => format!("{} minutes", seconds / 60),
        seconds => format!("{seconds} seconds"),
    }
}

/// What a delivery mode does, in a sentence.
fn delivery_detail(delivery: NotificationDelivery) -> String {
    match delivery {
        NotificationDelivery::Automatic => format!(
            "In Compi while it is the active app; from {SYSTEM_NAME} while it is in the background."
        ),
        NotificationDelivery::InApp => {
            "Only in Compi, at the bottom right of the window showing the terminal.".into()
        }
        NotificationDelivery::Os => format!("Only from {SYSTEM_NAME}, even while Compi is active."),
        NotificationDelivery::Both => format!("In Compi and from {SYSTEM_NAME}, each time."),
    }
}

/// The record a status change announces, if any: one entering `blocked` (when
/// `attention`) before one entering `done` (when `finished`). `before` is what the
/// terminal reported last; records of another process are no baseline for this one.
fn announced<'a>(
    before: Option<&SurfaceProgramStatus>,
    after: &'a SurfaceProgramStatus,
    attention: bool,
    finished: bool,
) -> Option<(NoticeKind, &'a ProgramStatusRecord)> {
    let previous = before
        .filter(|before| before.process_lifetime_id == after.process_lifetime_id)
        .map_or(&[][..], |before| before.records.as_slice());
    let entered = |state: ProgramState| {
        after.records.iter().find(|record| {
            record.state == state
                && previous
                    .iter()
                    .find(|old| old.id == record.id)
                    .is_none_or(|old| old.state != state)
        })
    };
    if attention && let Some(record) = entered(ProgramState::Blocked) {
        return Some((NoticeKind::Attention, record));
    }
    if finished && let Some(record) = entered(ProgramState::Done) {
        return Some((NoticeKind::Finished, record));
    }
    None
}

/// Untrusted program text made safe to show: invisible and direction-changing
/// characters removed, whitespace runs one space, at most `MAX_DETAIL_CHARS`.
fn bounded(text: &str) -> String {
    let text = display_text(text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let text = words.join(" ");
    if text.chars().count() > MAX_DETAIL_CHARS {
        let mut cut: String = text.chars().take(MAX_DETAIL_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}

/// The notification for `record`: who reports it and what it needs, its message, and
/// the place (tab and pane) it is in.
fn notice_text(
    kind: NoticeKind,
    record: &ProgramStatusRecord,
    records: &[ProgramStatusRecord],
    pane_title: &str,
) -> (String, String) {
    let who = inherited_app(record, records)
        .map(bounded)
        .filter(|app| !app.is_empty())
        .or_else(|| {
            record
                .title
                .as_deref()
                .map(bounded)
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| {
            let title = bounded(pane_title);
            if title.is_empty() {
                "A program".into()
            } else {
                title
            }
        });
    let title = match (kind, record.kind) {
        (NoticeKind::Attention, Some(BlockedKind::Permission)) => format!("{who} needs permission"),
        (NoticeKind::Attention, Some(BlockedKind::Question)) => format!("{who} has a question"),
        (NoticeKind::Attention, Some(BlockedKind::Auth)) => format!("{who} needs you to sign in"),
        (NoticeKind::Attention, None) => format!("{who} needs you"),
        (NoticeKind::Finished, _) => format!("{who} finished"),
        (NoticeKind::Test, _) => "Compi notification test".into(),
    };
    let message = record.message.as_deref().map(bounded).unwrap_or_default();
    (title, message)
}

/// A short tag for the system: one per terminal, so a newer notification replaces it.
fn tag(surface_id: &SurfaceId) -> String {
    let digest = Sha256::digest(surface_id.as_str().as_bytes());
    let hex: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("compi-{hex}")
}

/// A system notification was clicked: the window showing its terminal (else any window
/// of its target that has it) opens it. A terminal that is gone opens nothing.
fn open_route(tag: &str, cx: &mut App) {
    let Some((target, surface_id)) = cx
        .try_global::<NotificationRoutes>()
        .and_then(|routes| routes.0.get(tag).cloned())
    else {
        return;
    };
    let mut fallback = None;
    for handle in cx.windows() {
        let Some(typed) = handle.downcast::<CompiApp>() else {
            continue;
        };
        let Ok((same, has, shows)) = typed.read(cx).map(|app| {
            (
                app.target == target,
                app.surface_place(&surface_id).is_some(),
                app.shows_surface(&surface_id),
            )
        }) else {
            continue;
        };
        if !same || !has {
            continue;
        }
        if shows {
            fallback = Some(typed);
            break;
        }
        fallback.get_or_insert(typed);
    }
    if let Some(typed) = fallback {
        let _ = typed.update(cx, |app, window, cx| {
            app.open_notice_target(&surface_id, window, cx);
        });
    }
}

impl CompiApp {
    /// Whether this window announces for its target: the oldest open window does.
    fn announces(&self, cx: &App) -> bool {
        let live: Vec<WindowId> = cx
            .windows()
            .iter()
            .map(|window| window.window_id())
            .collect();
        cx.try_global::<NotificationWindows>()
            .and_then(|windows| {
                windows
                    .0
                    .iter()
                    .find(|(id, target)| *target == self.target && live.contains(id))
            })
            .is_none_or(|(id, _)| *id == self.notifications.window_id)
    }

    /// The tab and pane of terminal `surface_id`, when this window's workspace has it.
    fn surface_place(&self, surface_id: &SurfaceId) -> Option<(TabId, PaneId)> {
        let workspace = self.workspace.as_ref()?;
        monitor::surface_pane(workspace, surface_id).map(|(_, tab, pane)| (tab, pane))
    }

    /// Whether this window shows the tab of terminal `surface_id`.
    fn shows_surface(&self, surface_id: &SurfaceId) -> bool {
        let (Some(workspace), Some((tab_id, _))) =
            (self.workspace.as_ref(), self.surface_place(surface_id))
        else {
            return false;
        };
        workspace
            .sessions
            .iter()
            .flat_map(|session| self.state.visible_tabs(session))
            .any(|tab| tab.id == tab_id)
    }

    /// Before `status` replaces what its terminal reported: announces what it newly
    /// needs the user for, unless another window announces or the user is using it.
    pub(super) fn notice_program_status(
        &mut self,
        status: &SurfaceProgramStatus,
        cx: &mut Context<Self>,
    ) {
        let settings = self.config.notifications;
        if !settings.attention && !settings.finished || !self.announces(cx) {
            return;
        }
        // Only the terminal's current process speaks.
        let current = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(&status.surface_id))
            .is_some_and(|surface| surface.process_lifetime_id == status.process_lifetime_id);
        if !current {
            return;
        }
        let before = self.program_status.status(&status.surface_id);
        let Some((kind, record)) = announced(before, status, settings.attention, settings.finished)
        else {
            return;
        };
        let Some((tab_id, pane_id)) = self.surface_place(&status.surface_id) else {
            return;
        };
        if self.in_use(&pane_id, cx) {
            return;
        }
        let tab = self.tab_by_id(&tab_id);
        let pane_title = tab
            .map(|tab| self.tab_panes(tab))
            .and_then(|panes| panes.into_iter().find(|pane| pane.pane_id == pane_id))
            .map(|pane| pane.title)
            .unwrap_or_default();
        let place = tab
            .map(|tab| bounded(&self.tab_label(tab)))
            .unwrap_or_default();
        let (title, message) = notice_text(kind, record, &status.records, &pane_title);
        let notice = Notice {
            kind,
            target: Some((
                status.surface_id.clone(),
                status.process_lifetime_id.clone(),
            )),
            title,
            message,
            place,
        };
        self.deliver(notice, cx);
    }

    /// The Compi window that is active, as `(window, focused pane, time since its last
    /// input)`; none while another app is active.
    fn active_compi(&self, cx: &App) -> Option<(WindowId, Option<PaneId>, Duration)> {
        if self.notifications.window_active {
            return Some((
                self.notifications.window_id,
                self.focused_pane().cloned(),
                self.notifications.last_input.elapsed(),
            ));
        }
        cx.windows().into_iter().find_map(|handle| {
            if handle.window_id() == self.notifications.window_id {
                return None;
            }
            let app = handle.downcast::<CompiApp>()?.read(cx).ok()?;
            app.notifications.window_active.then(|| {
                (
                    handle.window_id(),
                    app.focused_pane().cloned(),
                    app.notifications.last_input.elapsed(),
                )
            })
        })
    }

    /// Whether the user is using `pane_id`: it has the keyboard in Compi's active window
    /// and Focused pane's wait has not passed since the last input there.
    fn in_use(&self, pane_id: &PaneId, cx: &App) -> bool {
        let after = self.config.notifications.focused_pane_after;
        self.active_compi(cx)
            .is_some_and(|(_, focused, since_input)| {
                focused.as_ref() == Some(pane_id) && focused_pane_in_use(since_input, after)
            })
    }

    /// Sends `notice` where Notifications says, and says what happened.
    fn deliver(&mut self, notice: Notice, cx: &mut Context<Self>) -> String {
        let settings = self.config.notifications;
        let wants_sound = settings.sound != NotificationSound::Off;
        let active = self.active_compi(cx).is_some();
        let (in_app, os) = channels(settings.delivery, active);
        let mut outcome = Vec::new();
        let mut system_sound = false;
        if os {
            let tag = match &notice.target {
                Some((surface_id, _)) => {
                    let tag = tag(surface_id);
                    cx.default_global::<NotificationRoutes>()
                        .0
                        .insert(tag.clone(), (self.target.clone(), surface_id.clone()));
                    tag
                }
                None => "compi-test".to_owned(),
            };
            let body = [notice.message.as_str(), notice.place.as_str()]
                .into_iter()
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let submitted = os_notifications::show(OsNotice {
                tag,
                title: notice.title.clone(),
                body,
                sound: settings.sound,
            });
            outcome.push(match submitted {
                OsSubmit::Submitted => {
                    system_sound = wants_sound;
                    format!("Sent to {}; it decides whether to show it", SYSTEM_NAME)
                }
                OsSubmit::Unavailable(why) => format!("Not sent to the system: {why}"),
            });
        }
        if in_app {
            match notice
                .target
                .as_ref()
                .and_then(|(surface_id, _)| self.window_showing(surface_id, cx))
            {
                Some(handle) => {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |app, _, cx| app.push_toast(notice, cx));
                    });
                }
                None => self.push_toast(notice, cx),
            }
            outcome.push("Shown in Compi".to_owned());
        }
        if sound_in_compi(wants_sound, in_app, system_sound) {
            outcome.push(sound_line(
                settings.sound,
                os_notifications::play_sound(settings.sound),
            ));
        }
        outcome.join(". ")
    }

    /// Another window that shows the tab of `surface_id`; `None` when it is this one,
    /// or none does (this window shows the toast).
    fn window_showing(
        &self,
        surface_id: &SurfaceId,
        cx: &App,
    ) -> Option<gpui::WindowHandle<CompiApp>> {
        if self.shows_surface(surface_id) {
            return None;
        }
        cx.windows().into_iter().find_map(|handle| {
            if handle.window_id() == self.notifications.window_id {
                return None;
            }
            let typed = handle.downcast::<CompiApp>()?;
            typed
                .read(cx)
                .ok()
                .filter(|app| app.target == self.target && app.shows_surface(surface_id))
                .map(|_| typed)
        })
    }

    /// Shows `notice` as a toast, replacing one from the same terminal.
    fn push_toast(&mut self, notice: Notice, cx: &mut Context<Self>) {
        let toasts = &mut self.notifications.toasts;
        toasts.retain(|toast| {
            toast.notice.target.as_ref().map(|(surface, _)| surface)
                != notice.target.as_ref().map(|(surface, _)| surface)
        });
        toasts.push(Toast {
            id: self.notifications.next_id,
            notice,
            shown: Instant::now(),
        });
        self.notifications.next_id += 1;
        let excess = toasts.len().saturating_sub(MAX_TOASTS);
        toasts.drain(..excess);
        cx.notify();
    }

    /// Takes nothing on screen: records every click and scroll in the window, wherever
    /// it lands and whatever handles it, for Focused pane. Keys are recorded by the
    /// window's root.
    pub(super) fn render_input_watch(&self, cx: &Context<Self>) -> AnyElement {
        let app = cx.entity().downgrade();
        gpui::canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let clicks = app.clone();
                window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _ = clicks.update(cx, |app, _| app.notifications.input_seen());
                    }
                });
                let scrolls = app.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _ = scrolls.update(cx, |app, _| app.notifications.input_seen());
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// The bottom-right corner of the window: toasts, then the pasted image cards (not
    /// while a menu or dialog is open).
    pub(super) fn render_corner(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let shelf_height = window.viewport_size().height * 0.32;
        div()
            .absolute()
            .right(px(12.0))
            .bottom(px(12.0))
            .max_w(gpui::relative(0.9))
            .flex()
            .flex_col()
            .items_end()
            .gap_2()
            .children(self.render_toasts(cx))
            .when(self.overlay.is_none(), |corner| {
                corner.child(self.render_image_previews(shelf_height, cx))
            })
            .into_any_element()
    }

    /// Saves Notifications settings and gives them to every window, since whichever
    /// window announces reads its own copy.
    pub(super) fn set_notification_settings(
        &mut self,
        settings: crate::config::NotificationSettings,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.config.save_notification_settings(settings) {
            self.global_error = Some(error);
            return;
        }
        let own = self.notifications.window_id;
        cx.defer(move |cx| {
            for handle in cx.windows() {
                if handle.window_id() == own {
                    continue;
                }
                if let Some(typed) = handle.downcast::<CompiApp>() {
                    let _ = typed.update(cx, |app, _, cx| {
                        app.config.notifications = settings;
                        cx.notify();
                    });
                }
            }
        });
    }

    /// Delivery: a dropdown of Automatic, In-app only, System only and Both, each with
    /// what it does.
    pub(super) fn render_notification_delivery_row(
        &self,
        row: &RowCx,
        cx: &Context<Self>,
    ) -> AnyElement {
        let popup = Popup::NotificationDelivery;
        let action = SettingsAction::Select(popup);
        let current = self.config.notifications.delivery;
        let focused = self.settings_focused(row, action);
        let options = NotificationDelivery::ALL.into_iter().map(|delivery| {
            controls::select_option(delivery.label()).detail(match delivery {
                NotificationDelivery::Automatic => {
                    format!("In Compi while it's active, else from {SYSTEM_NAME}")
                }
                NotificationDelivery::InApp => "Only in Compi".into(),
                NotificationDelivery::Os => format!("Only from {SYSTEM_NAME}"),
                NotificationDelivery::Both => format!("In Compi and from {SYSTEM_NAME}"),
            })
        });
        let open_row = row.clone();
        let select = controls::select(
            "notification-delivery-select",
            options,
            NotificationDelivery::ALL
                .iter()
                .position(|delivery| *delivery == current),
            self.popup == Some(popup),
        )
        .placeholder(current.label())
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(|this, index: &usize, _, cx| {
            if let Some(delivery) = NotificationDelivery::ALL.get(*index) {
                let mut settings = this.config.notifications;
                settings.delivery = *delivery;
                this.set_notification_settings(settings, cx);
            }
            cx.notify();
        }));
        let control = div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element();
        self.settings_row(row.title, delivery_detail(current), control, row.compact)
    }

    /// Focused pane: a dropdown of how long the focused pane of the active window goes
    /// without input before it notifies too. A value set in the file outside the choices
    /// is listed with them.
    pub(super) fn render_notification_focused_pane_row(
        &self,
        row: &RowCx,
        cx: &Context<Self>,
    ) -> AnyElement {
        let popup = Popup::NotificationFocusedPane;
        let action = SettingsAction::Select(popup);
        let current = self.config.notifications.focused_pane_after;
        let mut choices = FOCUSED_PANE_CHOICES.to_vec();
        if !choices.contains(&current) {
            choices.push(current);
            choices.sort_unstable();
        }
        let focused = self.settings_focused(row, action);
        let open_row = row.clone();
        let select = controls::select(
            "notification-focused-pane-select",
            choices
                .iter()
                .map(|seconds| controls::select_option(focused_pane_label(*seconds))),
            choices.iter().position(|seconds| *seconds == current),
            self.popup == Some(popup),
        )
        .placeholder(focused_pane_label(current))
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(move |this, index: &usize, _, cx| {
            if let Some(seconds) = choices.get(*index) {
                let mut settings = this.config.notifications;
                settings.focused_pane_after = *seconds;
                this.set_notification_settings(settings, cx);
            }
            cx.notify();
        }));
        let control = div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element();
        let detail = match current {
            0 => "The pane you're in stays quiet while Compi is the active app.".to_owned(),
            seconds => format!(
                "The pane you're in notifies too once you haven't typed, clicked or scrolled in its window for {}.",
                wait_text(seconds)
            ),
        };
        self.settings_row(row.title, detail, control, row.compact)
    }

    /// Sound: a dropdown (Off, or the system's notification sound) and Play, which plays
    /// it the way a notification shown in Compi would.
    pub(super) fn render_notification_sound_row(
        &self,
        row: &RowCx,
        cx: &Context<Self>,
    ) -> AnyElement {
        let popup = Popup::NotificationSound;
        let action = SettingsAction::Select(popup);
        let current = self.config.notifications.sound;
        let sounds = NotificationSound::choices();
        let focused = self.settings_focused(row, action);
        let open_row = row.clone();
        let select = controls::select(
            "notification-sound-select",
            sounds
                .iter()
                .map(|sound| controls::select_option(sound.label())),
            sounds.iter().position(|sound| *sound == current),
            self.popup == Some(popup),
        )
        .placeholder(current.label())
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(move |this, index: &usize, _, cx| {
            if let Some(sound) = sounds.get(*index) {
                let mut settings = this.config.notifications;
                settings.sound = *sound;
                this.set_notification_settings(settings, cx);
                this.notifications.sound_result = None;
            }
            cx.notify();
        }));
        let play = self.settings_button(row, 1, "Play", SettingsAction::PlayNotificationSound, cx);
        let control = div()
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .when(current != NotificationSound::Off, |control| {
                control.child(play)
            })
            .into_any_element();
        let detail = match (&self.notifications.sound_result, current) {
            (Some(result), _) => result.clone(),
            (None, NotificationSound::Off) => "Notifications are silent.".into(),
            (None, sound) if sound.on_this_system() != sound => format!(
                "{} is a {OTHER_SYSTEM_NAME} sound, so {SYSTEM_NAME} plays its default sound.",
                sound.label()
            ),
            (None, _) if cfg!(target_os = "macos") => "Plays once per notification, unless Focus is on or Compi's notification sounds are off in System Settings.".into(),
            (None, _) => format!(
                "Plays once per notification, as your {SYSTEM_NAME} sound scheme sets it, unless {SYSTEM_NAME} is quiet (Do Not Disturb, presenting, a full-screen app) or its notification sounds are off."
            ),
        };
        self.settings_row(row.title, detail, control, row.compact)
    }

    /// Play: the notification sound now, through the same checks as a notification.
    pub(super) fn play_notification_sound(&mut self, cx: &mut Context<Self>) {
        let sound = self.config.notifications.sound;
        let played = os_notifications::play_sound(sound);
        self.notifications.sound_result = Some(format!("{}.", sound_line(sound, played)));
        cx.notify();
    }

    /// Whether the system shows Compi's notifications, and the way to its settings.
    pub(super) fn render_notification_system_row(
        &self,
        row: &RowCx,
        cx: &Context<Self>,
    ) -> AnyElement {
        let control = match os_notifications::SETTINGS_URL {
            Some(_) => self.settings_button(
                row,
                0,
                "Open system settings",
                SettingsAction::OpenNotificationSystemSettings,
                cx,
            ),
            None => div().into_any_element(),
        };
        self.settings_row(
            row.title,
            self.notification_system_line(),
            control,
            row.compact,
        )
    }

    /// Toasts past their time go; runs on the window's timer.
    pub(super) fn expire_toasts(&mut self, cx: &mut Context<Self>) {
        let before = self.notifications.toasts.len();
        self.notifications
            .toasts
            .retain(|toast| toast.shown.elapsed() < TOAST_LIFETIME);
        if self.notifications.toasts.len() != before {
            cx.notify();
        }
    }

    /// Test notification: one sent the way Notifications settings deliver, with no
    /// terminal behind it, and what happened to it for Settings.
    pub(super) fn test_notification(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let notice = Notice {
            kind: NoticeKind::Test,
            target: None,
            title: "Compi notification test".into(),
            message: "Programs that need you, or finish, will notify like this.".into(),
            place: String::new(),
        };
        let mut result = self.deliver(notice, cx);
        if self.config.notifications.delivery == NotificationDelivery::Automatic {
            result.push_str(&format!(
                ". Automatic shows it here while Compi is active, and sends it to {SYSTEM_NAME} while Compi is in the background"
            ));
        }
        result.push('.');
        self.notifications.test_result = Some(result);
        cx.notify();
    }

    /// Opens the terminal `surface_id` from a notification and brings its window
    /// forward; a terminal that is gone opens nothing.
    pub(super) fn open_notice_target(
        &mut self,
        surface_id: &SurfaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.surface_place(surface_id).is_none() {
            return;
        }
        self.open_process_terminal(surface_id, window, cx);
        window.activate_window();
        cx.activate(true);
    }

    /// The toasts, bottom right, newest last, above the pasted image cards; web pages
    /// are cut around them.
    pub(super) fn render_toasts(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.notifications.toasts.is_empty() {
            return None;
        }
        let colors = *self.colors();
        let toasts = self.notifications.toasts.iter().map(|toast| {
            let id = toast.id;
            let notice = &toast.notice;
            let (icon, tint) = match notice.kind {
                NoticeKind::Attention => (IconName::CircleAlert, colors.modified),
                NoticeKind::Finished => (IconName::CircleCheck, colors.added),
                NoticeKind::Test => (IconName::Bell, colors.accent),
            };
            let target = notice.target.clone();
            div()
                .id(("notification-toast", id as usize))
                .w(px(300.0))
                .max_w_full()
                .p_3()
                .flex()
                .items_start()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .shadow_lg()
                .when(target.is_some(), |toast| toast.cursor_pointer())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.notifications.toasts.retain(|toast| toast.id != id);
                    if let Some((surface_id, _)) = &target {
                        this.open_notice_target(surface_id, window, cx);
                    }
                    cx.notify();
                }))
                .child(
                    gpui_component::Icon::new(icon)
                        .size(px(16.0))
                        .text_color(color(tint)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(notice.title.clone()),
                        )
                        .when(!notice.message.is_empty(), |text| {
                            text.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(notice.message.clone()),
                            )
                        })
                        .when(!notice.place.is_empty(), |text| {
                            text.child(
                                div()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(notice.place.clone()),
                            )
                        }),
                )
                .child(
                    div()
                        .id(("dismiss-notification", id as usize))
                        .flex_none()
                        .px_1()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Only the toast goes: the status stays on its tab and pane.
                            this.notifications.toasts.retain(|toast| toast.id != id);
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .child(chrome_icon(ChromeIcon::Close, color(colors.muted))),
                )
        });
        Some(
            div()
                .relative()
                .flex()
                .flex_col()
                .items_end()
                .gap_2()
                .children(self.record_browser_float(usize::MAX))
                .children(toasts)
                .into_any_element(),
        )
    }

    /// Settings' line about system notifications.
    pub(super) fn notification_system_line(&self) -> String {
        match self.notifications.os_status() {
            OsStatus::On => format!("{} shows Compi's notifications.", SYSTEM_NAME),
            OsStatus::Off(why) | OsStatus::Unknown(why) => why,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: Option<&str>, state: ProgramState) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: id.map(str::to_owned),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
        }
    }

    fn status(lifetime: &str, records: Vec<ProgramStatusRecord>) -> SurfaceProgramStatus {
        SurfaceProgramStatus {
            surface_id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new(lifetime),
            records,
        }
    }

    fn kind(
        before: Option<&SurfaceProgramStatus>,
        after: &SurfaceProgramStatus,
        attention: bool,
        finished: bool,
    ) -> Option<NoticeKind> {
        announced(before, after, attention, finished).map(|(kind, _)| kind)
    }

    #[test]
    fn only_entering_blocked_or_done_is_announced() {
        let working = status("l1", vec![record(None, ProgramState::Working)]);
        let blocked = status("l1", vec![record(None, ProgramState::Blocked)]);
        let done = status("l1", vec![record(None, ProgramState::Done)]);
        let idle = status("l1", vec![record(None, ProgramState::Idle)]);
        let failed = status("l1", vec![record(None, ProgramState::Error)]);
        assert_eq!(
            kind(Some(&working), &blocked, true, true),
            Some(NoticeKind::Attention)
        );
        assert_eq!(
            kind(Some(&working), &done, true, true),
            Some(NoticeKind::Finished)
        );
        // A refresh of the same state, working, idle and failures say nothing.
        assert_eq!(kind(Some(&blocked), &blocked, true, true), None);
        assert_eq!(kind(Some(&done), &done, true, true), None);
        assert_eq!(kind(Some(&idle), &working, true, true), None);
        assert_eq!(kind(Some(&working), &idle, true, true), None);
        assert_eq!(kind(Some(&working), &failed, true, true), None);
        // A terminal that reported nothing before.
        assert_eq!(
            kind(None, &blocked, true, true),
            Some(NoticeKind::Attention)
        );
    }

    #[test]
    fn each_switch_stands_alone() {
        let working = status("l1", vec![record(None, ProgramState::Working)]);
        let blocked = status("l1", vec![record(None, ProgramState::Blocked)]);
        let done = status("l1", vec![record(None, ProgramState::Done)]);
        assert_eq!(kind(Some(&working), &blocked, false, true), None);
        assert_eq!(kind(Some(&working), &done, true, false), None);
        assert_eq!(
            kind(Some(&working), &done, false, true),
            Some(NoticeKind::Finished)
        );
        // Both at once: what needs the user first.
        let both = status(
            "l1",
            vec![
                record(None, ProgramState::Done),
                record(Some("sub"), ProgramState::Blocked),
            ],
        );
        assert_eq!(
            kind(Some(&working), &both, true, true),
            Some(NoticeKind::Attention)
        );
        assert_eq!(
            kind(Some(&working), &both, false, true),
            Some(NoticeKind::Finished)
        );
    }

    #[test]
    fn records_are_told_apart_by_id_and_process() {
        let one_blocked = status(
            "l1",
            vec![
                record(None, ProgramState::Working),
                record(Some("a"), ProgramState::Blocked),
            ],
        );
        // Another record entering blocked while the first still is.
        let two_blocked = status(
            "l1",
            vec![
                record(None, ProgramState::Working),
                record(Some("a"), ProgramState::Blocked),
                record(Some("b"), ProgramState::Blocked),
            ],
        );
        assert_eq!(
            kind(Some(&one_blocked), &two_blocked, true, true),
            Some(NoticeKind::Attention)
        );
        assert_eq!(
            announced(Some(&one_blocked), &two_blocked, true, true)
                .map(|(_, record)| record.id.as_deref()),
            Some(Some("b"))
        );
        // A restarted process is not compared with the old one's records.
        let restarted = status("l2", vec![record(Some("a"), ProgramState::Blocked)]);
        assert_eq!(
            kind(Some(&one_blocked), &restarted, true, true),
            Some(NoticeKind::Attention)
        );
    }

    #[test]
    fn compi_plays_the_sound_only_when_no_system_notification_carries_it() {
        // In Compi only (Automatic while active, In-app only): Compi plays it.
        assert!(sound_in_compi(true, true, false));
        // Both: the system notification carries it, so it is asked for once.
        assert!(!sound_in_compi(true, true, true));
        // System only, or the system refused it with nothing shown in Compi.
        assert!(!sound_in_compi(true, false, true));
        assert!(!sound_in_compi(true, false, false));
        // Sound off.
        assert!(!sound_in_compi(false, true, false));
    }

    #[test]
    fn the_focused_pane_notifies_only_after_its_wait_without_input() {
        let secs = Duration::from_secs;
        // Never: in use whenever focused, however long ago the last input was.
        assert!(focused_pane_in_use(secs(3600), 0));
        // Typing just now keeps it quiet; the wait passing lets it notify.
        assert!(focused_pane_in_use(secs(0), 30));
        assert!(focused_pane_in_use(secs(29), 30));
        assert!(!focused_pane_in_use(secs(30), 30));
        assert!(!focused_pane_in_use(secs(31), 30));
    }

    #[test]
    fn delivery_follows_the_mode_and_whether_compi_is_active() {
        use NotificationDelivery::*;
        assert_eq!(channels(Automatic, true), (true, false));
        assert_eq!(channels(Automatic, false), (false, true));
        for active in [true, false] {
            assert_eq!(channels(InApp, active), (true, false));
            assert_eq!(channels(Os, active), (false, true));
            assert_eq!(channels(Both, active), (true, true));
        }
    }

    #[test]
    fn program_text_is_bounded_and_cleaned() {
        let mut blocked = record(None, ProgramState::Blocked);
        blocked.kind = Some(BlockedKind::Permission);
        blocked.app = Some("Claude\u{202E}Code".into());
        blocked.message = Some(format!("Run   rm?\n{}", "x".repeat(400)));
        let (title, message) = notice_text(NoticeKind::Attention, &blocked, &[], "pane");
        assert_eq!(title, "ClaudeCode needs permission");
        assert!(message.starts_with("Run rm? x"));
        assert_eq!(message.chars().count(), MAX_DETAIL_CHARS);
        assert!(message.ends_with('…'));
        // Nothing names the program: the pane's title does.
        let done = record(None, ProgramState::Done);
        assert_eq!(
            notice_text(NoticeKind::Finished, &done, &[], "build").0,
            "build finished"
        );
        assert_eq!(
            notice_text(NoticeKind::Finished, &done, &[], "").0,
            "A program finished"
        );
    }

    #[test]
    fn tags_are_short_stable_and_per_terminal() {
        let a = tag(&SurfaceId::new("surface-1a17e9e24a8-364c-4"));
        assert_eq!(a, tag(&SurfaceId::new("surface-1a17e9e24a8-364c-4")));
        assert_ne!(a, tag(&SurfaceId::new("surface-2")));
        assert!(a.len() <= 32 && a.is_ascii());
    }
}

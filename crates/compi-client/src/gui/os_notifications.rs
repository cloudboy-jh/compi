//! Native OS notifications for terminal programs that need attention or finish.
//!
//! Both platforms post directly instead of through GPUI's backend, which always
//! posts silent notifications.
//!
//! Windows posts toasts through `ToastNotificationManager`. GPUI would also need
//! the process AppUserModelID set with `SetCurrentProcessExplicitAppUserModelID`;
//! the installed Start-menu shortcut has no AppUserModelID, so an explicit process
//! ID would split Compi's taskbar button away from the pinned shortcut. Instead the
//! notifier is created for a per-user registered ID (`Compi.Compi`, or
//! `Compi.Compi.<instance>`) that only notifications use; the process and its
//! windows keep the shortcut's implicit ID.
//!
//! macOS posts through `UNUserNotificationCenter`, which asks for permission on the
//! first notification, cannot report that permission synchronously, and works only
//! when Compi runs from its app bundle. Compi owns the center's delegate, so GPUI's
//! system notification calls must stay unused.
//!
//! The notification sound travels inside the system notification, so Focus Assist,
//! Do Not Disturb and Focus silence or hold it together with the banner. A toast shown
//! only in Compi plays it through [`play_sound`], with the same quiet checks where the
//! system allows them.
//!
//! On both platforms a successful [`show`] means the OS took the notification,
//! not that it was presented: the user or a quiet mode may still hide it.

use crate::config::NotificationSound;
use gpui::App;

/// A notification to post. Posting the same `tag` again replaces the earlier
/// notification.
pub(in crate::gui) struct OsNotice {
    pub tag: String,
    pub title: String,
    pub body: String,
    /// What it plays; `Off` is silent.
    pub sound: NotificationSound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum OsSubmit {
    /// Handed to the OS; presentation is not confirmed.
    Submitted,
    /// Not handed over, with why, in a short sentence for Settings.
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum OsStatus {
    /// Only Windows reports the setting; macOS never answers On or Off.
    #[cfg_attr(not(windows), allow(dead_code))]
    On,
    /// Turned off by the user or policy: why.
    #[cfg_attr(not(windows), allow(dead_code))]
    Off(String),
    /// The OS gives no answer (macOS): a note.
    Unknown(String),
}

/// What became of a sound [`play_sound`] did not refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum SoundPlayed {
    /// Compi played it.
    Played,
    /// Handed to the system, which plays it unless it is quiet.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Handed,
}

/// The OS page for notification settings, when there is one.
#[cfg(windows)]
pub(in crate::gui) const SETTINGS_URL: Option<&str> = Some("ms-settings:notifications");
/// The OS page for notification settings, when there is one.
#[cfg(target_os = "macos")]
pub(in crate::gui) const SETTINGS_URL: Option<&str> =
    Some("x-apple.systempreferences:com.apple.preference.notifications");

/// Once at startup. `instance` is the Compi instance name (None or "" = default).
/// `on_click(tag, cx)` runs on the main thread when the user clicks a
/// notification this process posted.
pub(in crate::gui) fn init(
    cx: &mut App,
    instance: Option<&str>,
    on_click: impl Fn(&str, &mut App) + 'static,
) {
    #[cfg(windows)]
    toast::init(cx, instance, on_click);
    #[cfg(target_os = "macos")]
    {
        // The bundle identifier, not an instance name, identifies the app here.
        let _ = instance;
        mac::init(cx, on_click);
    }
}

pub(in crate::gui) fn show(notice: OsNotice) -> OsSubmit {
    let notice = OsNotice {
        sound: notice.sound.on_this_system(),
        ..notice
    };
    #[cfg(windows)]
    {
        toast::show(notice)
    }
    #[cfg(target_os = "macos")]
    {
        mac::show(notice)
    }
}

pub(in crate::gui) fn status() -> OsStatus {
    #[cfg(windows)]
    {
        toast::status()
    }
    #[cfg(target_os = "macos")]
    {
        OsStatus::Unknown(
            if mac::in_app_bundle() {
                "macOS asks for permission the first time Compi sends one; the answer can't be read here"
            } else {
                mac::NOT_BUNDLED
            }
            .to_owned(),
        )
    }
}

/// Plays `sound` for a notification shown only in Compi (a system notification carries
/// its own), unless the system wants quiet now: `Err` says why it did not play.
pub(in crate::gui) fn play_sound(sound: NotificationSound) -> Result<SoundPlayed, String> {
    let sound = sound.on_this_system();
    #[cfg(windows)]
    {
        toast::play_sound(sound).map(|()| SoundPlayed::Played)
    }
    #[cfg(target_os = "macos")]
    {
        mac::play_sound(sound)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{NotificationSound, OsNotice, OsSubmit, SoundPlayed};
    use block2::RcBlock;
    use gpui::App;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, class, define_class, msg_send};
    use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString, NSURL};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::cell::RefCell;
    use std::ffi::c_void;

    pub(super) const NOT_BUNDLED: &str =
        "macOS shows notifications only when Compi runs from its app bundle";

    /// The request that carries the sound of a toast shown only in Compi. The delegate
    /// presents it as sound alone, and each one replaces the last.
    const SOUND_ONLY: &str = "compi-sound";

    /// How long a sound-only notification stays delivered: macOS still keeps it in
    /// Notification Center's history, so it goes once its sound has played.
    const SOUND_ONLY_KEPT: std::time::Duration = std::time::Duration::from_secs(10);

    /// AudioToolbox's `kSystemSoundID_UserPreferredAlert`: the alert sound chosen in
    /// System Settings.
    const USER_PREFERRED_ALERT: u32 = 0x1000;

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        fn AudioServicesCreateSystemSoundID(url: *const c_void, sound: *mut u32) -> i32;
        fn AudioServicesPlayAlertSound(sound: u32);
    }

    struct State {
        center: Retained<UNUserNotificationCenter>,
        // The center holds its delegate weakly.
        _delegate: Retained<Delegate>,
        authorization_requested: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
        /// Alert sounds loaded for AudioServices, which keeps them for the process.
        static LOADED: RefCell<Vec<(NotificationSound, u32)>> = const { RefCell::new(Vec::new()) };
    }

    /// `UNUserNotificationCenter` aborts the process without a bundle identifier, as
    /// under `cargo run`.
    pub(super) fn in_app_bundle() -> bool {
        unsafe {
            let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
            let identifier: *mut AnyObject = msg_send![bundle, bundleIdentifier];
            !identifier.is_null()
        }
    }

    pub(super) fn init(cx: &mut App, on_click: impl Fn(&str, &mut App) + 'static) {
        if !in_app_bundle() {
            return;
        }
        let (clicks, receiver) = async_channel::unbounded::<String>();
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new(clicks);
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        STATE.set(Some(State {
            center,
            _delegate: delegate,
            authorization_requested: false,
        }));
        cx.spawn(async move |cx| {
            while let Ok(tag) = receiver.recv().await {
                cx.update(|cx| on_click(&tag, cx));
            }
        })
        .detach();
    }

    pub(super) fn show(notice: OsNotice) -> OsSubmit {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notice.title));
        content.setBody(&NSString::from_str(&notice.body));
        content.setSound(notification_sound(notice.sound).as_deref());
        match post(&notice.tag, &content) {
            Ok(()) => OsSubmit::Submitted,
            Err(why) => OsSubmit::Unavailable(why),
        }
    }

    /// While Compi is the active app the sound travels in a notification presented as
    /// sound alone, so Focus and Compi's notification sound switch in System Settings
    /// still silence it. A notification can't be sound alone while Compi is in the
    /// background, so then Compi plays the alert sound itself.
    pub(super) fn play_sound(sound: NotificationSound) -> Result<SoundPlayed, String> {
        let Some(notification_sound) = notification_sound(sound) else {
            return Ok(SoundPlayed::Played);
        };
        if app_active() && in_app_bundle() {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str("Compi"));
            content.setSound(Some(&notification_sound));
            post(SOUND_ONLY, &content)?;
            // Presented or held back by Focus, it lands in Notification Center's history.
            std::thread::spawn(|| {
                std::thread::sleep(SOUND_ONLY_KEPT);
                // The center is thread-safe.
                UNUserNotificationCenter::currentNotificationCenter()
                    .removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&[
                        NSString::from_str(SOUND_ONLY),
                    ]));
            });
            return Ok(SoundPlayed::Handed);
        }
        let id = alert_sound(sound)?;
        // SAFETY: `id` is a sound AudioServices created or a constant it defines.
        unsafe { AudioServicesPlayAlertSound(id) };
        Ok(SoundPlayed::Played)
    }

    /// The system's default notification sound, or an alert sound by its file name.
    fn notification_sound(sound: NotificationSound) -> Option<Retained<UNNotificationSound>> {
        match sound {
            NotificationSound::Off => None,
            NotificationSound::Default => Some(UNNotificationSound::defaultSound()),
            named => Some(UNNotificationSound::soundNamed(&NSString::from_str(
                named.label(),
            ))),
        }
    }

    /// `sound` loaded for AudioServices: the alert sound of System Settings for
    /// Default, else its file in `/System/Library/Sounds`.
    fn alert_sound(sound: NotificationSound) -> Result<u32, String> {
        if sound == NotificationSound::Default {
            return Ok(USER_PREFERRED_ALERT);
        }
        if let Some(id) = LOADED.with_borrow(|loaded| {
            loaded
                .iter()
                .find(|(loaded, _)| *loaded == sound)
                .map(|(_, id)| *id)
        }) {
            return Ok(id);
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&format!(
            "/System/Library/Sounds/{}.aiff",
            sound.label()
        )));
        let mut id = 0;
        // SAFETY: `NSURL` is toll-free bridged to the `CFURLRef` it takes, and `url`
        // outlives the call.
        let status =
            unsafe { AudioServicesCreateSystemSoundID(Retained::as_ptr(&url).cast(), &mut id) };
        if status != 0 {
            return Err(format!("macOS has no {} sound", sound.label()));
        }
        LOADED.with_borrow_mut(|loaded| loaded.push((sound, id)));
        Ok(id)
    }

    fn app_active() -> bool {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            msg_send![app, isActive]
        }
    }

    /// Asks for permission once, then hands `content` to the center as `identifier`.
    fn post(identifier: &str, content: &UNMutableNotificationContent) -> Result<(), String> {
        STATE.with_borrow_mut(|state| {
            let Some(state) = state.as_mut() else {
                return Err(NOT_BUNDLED.to_owned());
            };
            if !state.authorization_requested {
                state.authorization_requested = true;
                let completion = RcBlock::new(|granted: Bool, error: *mut NSError| {
                    // SAFETY: when non-null, `error` is valid for the callback.
                    if let Some(error) = unsafe { error.as_ref() } {
                        eprintln!(
                            "Compi: macOS notification permission failed: {}",
                            error.localizedDescription()
                        );
                    } else if !granted.as_bool() {
                        eprintln!("Compi: macOS notification permission denied");
                    }
                });
                state
                    .center
                    .requestAuthorizationWithOptions_completionHandler(
                        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                        &completion,
                    );
            }
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(identifier),
                content,
                None,
            );
            let completion = RcBlock::new(|error: *mut NSError| {
                // SAFETY: when non-null, `error` is valid for the callback.
                if let Some(error) = unsafe { error.as_ref() } {
                    eprintln!(
                        "Compi: macOS did not take a notification: {}",
                        error.localizedDescription()
                    );
                }
            });
            state
                .center
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            Ok(())
        })
    }

    struct DelegateIvars {
        clicks: async_channel::Sender<String>,
    }

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements and `Delegate` does not
        // implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "CompiNotificationDelegate"]
        #[ivars = DelegateIvars]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // The user clicked a notification, possibly on another thread.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive_notification_response(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion_handler: &block2::DynBlock<dyn Fn()>,
            ) {
                let tag = response.notification().request().identifier().to_string();
                if tag != SOUND_ONLY {
                    // Unbounded: only fails once the receiving task is gone.
                    let _ = self.ivars().clicks.try_send(tag);
                }
                completion_handler.call(());
            }

            // macOS asks only while Compi is the active app; without an answer it
            // presents nothing. Whether to notify while active is Delivery's decision.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present_notification(
                &self,
                _center: &UNUserNotificationCenter,
                notification: &UNNotification,
                completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                let options = if notification.request().identifier().to_string() == SOUND_ONLY {
                    UNNotificationPresentationOptions::Sound
                } else {
                    UNNotificationPresentationOptions::Banner
                        | UNNotificationPresentationOptions::List
                        | UNNotificationPresentationOptions::Sound
                };
                completion_handler.call((options,));
            }
        }
    );

    impl Delegate {
        fn new(clicks: async_channel::Sender<String>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(DelegateIvars { clicks });
            // SAFETY: `NSObject`'s `init` is its designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }
}

#[cfg(windows)]
mod toast {
    use super::{NotificationSound, OsNotice, OsStatus, OsSubmit};
    use gpui::App;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
        RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
    };
    use windows::core::{HSTRING, IInspectable, PCWSTR, h};

    /// Windows rejects toast tags longer than 64 characters.
    const TAG_LIMIT: usize = 64;
    const AUMID_PREFIX: &str = "Compi.Compi";
    /// An AppUserModelID may be at most 128 characters.
    const AUMID_LIMIT: usize = 128;
    const ICON: &[u8] = include_bytes!("../../../../assets/Compi-desktopappicon-v4.ico");

    struct Shown {
        serial: u64,
        // Holding the toast keeps its `Activated` handler registered.
        _toast: ToastNotification,
    }

    struct Click {
        tag: String,
        serial: u64,
    }

    struct State {
        aumid: String,
        notifier: Result<ToastNotifier, String>,
        clicks: async_channel::Sender<Click>,
        shown: HashMap<String, Shown>,
        next_serial: u64,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub(super) fn init(
        cx: &mut App,
        instance: Option<&str>,
        on_click: impl Fn(&str, &mut App) + 'static,
    ) {
        let aumid = app_user_model_id(instance);
        register(&aumid, &display_name(instance), icon_path());
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(&aumid))
            .map_err(|error| error.to_string());
        let (clicks, receiver) = async_channel::unbounded::<Click>();
        STATE.set(Some(State {
            aumid,
            notifier,
            clicks,
            shown: HashMap::new(),
            next_serial: 0,
        }));
        cx.spawn(async move |cx| {
            while let Ok(click) = receiver.recv().await {
                // A clicked toast leaves the Action Center; a newer one with
                // the same tag stays held.
                STATE.with_borrow_mut(|state| {
                    if let Some(state) = state
                        && state
                            .shown
                            .get(&click.tag)
                            .is_some_and(|shown| shown.serial == click.serial)
                    {
                        state.shown.remove(&click.tag);
                    }
                });
                cx.update(|cx| on_click(&click.tag, cx));
            }
        })
        .detach();
    }

    pub(super) fn show(notice: OsNotice) -> OsSubmit {
        let prepared = STATE.with_borrow_mut(|state| {
            let state = state
                .as_mut()
                .ok_or("System notifications are not set up yet")?;
            let notifier = state
                .notifier
                .as_ref()
                .map_err(|error| format!("Windows did not accept the notification: {error}"))?
                .clone();
            let serial = state.next_serial;
            state.next_serial += 1;
            Ok::<_, String>((notifier, state.aumid.clone(), state.clicks.clone(), serial))
        });
        let (notifier, aumid, clicks, serial) = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => return OsSubmit::Unavailable(reason),
        };
        if let OsStatus::Off(reason) = setting_status(&notifier, &aumid) {
            return OsSubmit::Unavailable(reason);
        }
        match post(&notifier, &notice, clicks, serial) {
            Ok(toast) => {
                STATE.with_borrow_mut(|state| {
                    if let Some(state) = state {
                        // Replaces, and so drops, the toast this one superseded.
                        state.shown.insert(
                            notice.tag,
                            Shown {
                                serial,
                                _toast: toast,
                            },
                        );
                    }
                });
                OsSubmit::Submitted
            }
            Err(error) => {
                OsSubmit::Unavailable(format!("Windows did not accept the notification: {error}"))
            }
        }
    }

    pub(super) fn status() -> OsStatus {
        let state = STATE.with_borrow(|state| {
            state
                .as_ref()
                .map(|state| (state.aumid.clone(), state.notifier.clone()))
        });
        match state {
            None => OsStatus::Unknown("System notifications are not set up yet".to_owned()),
            Some((_, Err(error))) => OsStatus::Unknown(format!(
                "Windows did not report the notification setting: {error}"
            )),
            Some((aumid, Ok(notifier))) => setting_status(&notifier, &aumid),
        }
    }

    fn setting_status(notifier: &ToastNotifier, aumid: &str) -> OsStatus {
        let off = |reason: &str| OsStatus::Off(reason.to_owned());
        match notifier.Setting() {
            Ok(NotificationSetting::Enabled) => OsStatus::On,
            Ok(NotificationSetting::DisabledForApplication) => {
                off("Notifications from Compi are turned off in Windows settings")
            }
            Ok(NotificationSetting::DisabledForUser) => {
                off("Notifications are turned off in Windows settings")
            }
            Ok(NotificationSetting::DisabledByGroupPolicy) => {
                off("Notifications are turned off by policy")
            }
            Ok(NotificationSetting::DisabledByManifest) => {
                off("Windows does not allow notifications from Compi")
            }
            Ok(other) => OsStatus::Unknown(format!(
                "Windows reported an unknown notification setting ({})",
                other.0
            )),
            // An app without a package or Start-menu shortcut of this ID cannot ask
            // (ERROR_NOT_FOUND); the switches Windows settings show are read instead.
            Err(_) => registry_status(aumid),
        }
    }

    /// The two switches of Windows settings → Notifications that stop Compi's toasts:
    /// notifications as a whole, and those of `aumid`. Unset means on.
    fn registry_status(aumid: &str) -> OsStatus {
        if user_dword(
            r"Software\Microsoft\Windows\CurrentVersion\PushNotifications",
            "ToastEnabled",
        ) == Some(0)
        {
            return OsStatus::Off("Notifications are turned off in Windows settings".into());
        }
        let app =
            format!(r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\{aumid}");
        if user_dword(&app, "Enabled") == Some(0) {
            return OsStatus::Off(
                "Notifications from Compi are turned off in Windows settings".into(),
            );
        }
        OsStatus::On
    }

    /// A DWORD under `HKEY_CURRENT_USER`, when it is set.
    fn user_dword(key: &str, value: &str) -> Option<u32> {
        let (key, value) = (HSTRING::from(key), HSTRING::from(value));
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `data` and `size` are valid out pointers for one DWORD.
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &value,
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut data).cast()),
                Some(&raw mut size),
            )
        };
        (read == ERROR_SUCCESS).then_some(data)
    }

    /// The sound scheme event of `sound`: what Windows calls Notification, Instant
    /// Message Notification, New Mail Notification, Calendar Reminder and New Text
    /// Message Notification. macOS's sounds arrive here already turned into Default
    /// (`on_this_system`).
    fn sound_event(sound: NotificationSound) -> Option<&'static str> {
        match sound {
            NotificationSound::Off => None,
            NotificationSound::Message => Some("Notification.IM"),
            NotificationSound::Mail => Some("Notification.Mail"),
            NotificationSound::Reminder => Some("Notification.Reminder"),
            NotificationSound::TextMessage => Some("Notification.SMS"),
            _ => Some("Notification.Default"),
        }
    }

    /// Plays `sound`'s event of the sound scheme at the system sounds volume, unless
    /// Windows says Compi should keep quiet now.
    pub(super) fn play_sound(sound: NotificationSound) -> Result<(), String> {
        use windows::Win32::Media::Audio::{
            PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT, SND_SYSTEM,
        };
        let Some(event) = sound_event(sound) else {
            return Ok(());
        };
        if let Some(reason) = quiet() {
            return Err(reason);
        }
        let event = HSTRING::from(event);
        // SAFETY: `event` is a NUL-terminated string that outlives the call; no module
        // is named.
        let played = unsafe {
            PlaySoundW(
                &event,
                None,
                SND_ALIAS | SND_ASYNC | SND_SYSTEM | SND_NODEFAULT,
            )
        };
        if played.as_bool() {
            Ok(())
        } else {
            Err("Windows has no sound set for it".into())
        }
    }

    /// Why Windows wants no notification sound from Compi now, if it doesn't: Compi's
    /// notifications or all notification sounds turned off, Do Not Disturb or quiet
    /// hours, a presentation, a full-screen app, or a locked screen.
    fn quiet() -> Option<String> {
        use windows::Win32::UI::Shell::{
            QUNS_ACCEPTS_NOTIFICATIONS, QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME,
            SHQueryUserNotificationState,
        };
        if let OsStatus::Off(reason) = status() {
            return Some(reason);
        }
        if user_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings",
            "NOC_GLOBAL_SETTING_ALLOW_NOTIFICATION_SOUND",
        ) == Some(0)
        {
            return Some("Notification sounds are turned off in Windows settings".into());
        }
        // SAFETY: no arguments beyond the out value the binding provides.
        match unsafe { SHQueryUserNotificationState() } {
            Ok(QUNS_ACCEPTS_NOTIFICATIONS) | Err(_) => None,
            Ok(QUNS_QUIET_TIME) => Some("Windows is in Do Not Disturb or quiet hours".into()),
            Ok(QUNS_PRESENTATION_MODE) => Some("Windows is in presentation mode".into()),
            Ok(QUNS_NOT_PRESENT) => Some("The screen is locked".into()),
            Ok(_) => Some("A full-screen app is running".into()),
        }
    }

    fn post(
        notifier: &ToastNotifier,
        notice: &OsNotice,
        clicks: async_channel::Sender<Click>,
        serial: u64,
    ) -> windows::core::Result<ToastNotification> {
        let toast = ToastNotification::CreateToastNotification(&toast_document(notice)?)?;
        toast.SetTag(&HSTRING::from(toast_tag(&notice.tag)))?;
        toast.SetGroup(h!("compi"))?;
        let tag = notice.tag.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, _| {
                // Unbounded: only fails once the receiving task is gone.
                let _ = clicks.try_send(Click {
                    tag: tag.clone(),
                    serial,
                });
                Ok(())
            },
        ))?;
        notifier.Show(&toast)?;
        Ok(toast)
    }

    /// Builds the toast with DOM calls so title and body stay text and are
    /// never parsed as XML.
    fn toast_document(notice: &OsNotice) -> windows::core::Result<XmlDocument> {
        let document = XmlDocument::new()?;
        let toast = document.CreateElement(h!("toast"))?;
        document.AppendChild(&toast)?;
        let visual = document.CreateElement(h!("visual"))?;
        toast.AppendChild(&visual)?;
        let binding = document.CreateElement(h!("binding"))?;
        binding.SetAttribute(h!("template"), h!("ToastGeneric"))?;
        visual.AppendChild(&binding)?;
        for text in [&notice.title, &notice.body] {
            let element = document.CreateElement(h!("text"))?;
            element.AppendChild(&document.CreateTextNode(&HSTRING::from(xml_text(text)))?)?;
            binding.AppendChild(&element)?;
        }
        let audio = document.CreateElement(h!("audio"))?;
        match sound_event(notice.sound) {
            Some(event) => audio.SetAttribute(
                h!("src"),
                &HSTRING::from(format!("ms-winsoundevent:{event}")),
            )?,
            None => audio.SetAttribute(h!("silent"), h!("true"))?,
        }
        toast.AppendChild(&audio)?;
        Ok(document)
    }

    /// Control characters other than tab and line breaks are not valid XML
    /// text, and terminal titles can carry them.
    fn xml_text(text: &str) -> String {
        text.chars()
            .filter(|character| !character.is_control() || matches!(character, '\t' | '\n' | '\r'))
            .collect()
    }

    fn toast_tag(tag: &str) -> String {
        tag.chars().take(TAG_LIMIT).collect()
    }

    fn app_user_model_id(instance: Option<&str>) -> String {
        match instance.filter(|instance| !instance.is_empty()) {
            None => AUMID_PREFIX.to_owned(),
            Some(instance) => {
                let suffix: String = instance
                    .chars()
                    .map(|character| {
                        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-') {
                            character
                        } else {
                            '-'
                        }
                    })
                    .collect();
                // Empty dot-separated parts are not valid in an ID.
                let suffix = suffix
                    .split('.')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>();
                let suffix = if suffix.is_empty() {
                    "-".to_owned()
                } else {
                    suffix.join(".")
                };
                format!("{AUMID_PREFIX}.{suffix}")
                    .chars()
                    .take(AUMID_LIMIT)
                    .collect::<String>()
                    .trim_end_matches('.')
                    .to_owned()
            }
        }
    }

    fn display_name(instance: Option<&str>) -> String {
        match instance.filter(|instance| !instance.is_empty()) {
            None => "Compi".to_owned(),
            Some(instance) => format!("Compi ({instance})"),
        }
    }

    /// The app icon as a file, for the notification registration. `None` when
    /// it can't be written; Windows then shows a generic icon.
    fn icon_path() -> Option<PathBuf> {
        let path = compi_protocol::paths::data_dir()
            .ok()?
            .join("notification-icon.ico");
        let current =
            std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == ICON.len() as u64);
        if !current && let Err(error) = std::fs::write(&path, ICON) {
            eprintln!("Could not write the notification icon: {error}");
            return None;
        }
        Some(path)
    }

    /// Registers the ID so Windows names notifications "Compi" and lists Compi
    /// in its notification settings.
    fn register(aumid: &str, display_name: &str, icon: Option<PathBuf>) {
        let subkey = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{aumid}"));
        let mut key = HKEY::default();
        // SAFETY: `subkey` outlives the call and `key` is a valid out pointer.
        let created = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &subkey,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if created != ERROR_SUCCESS {
            eprintln!("Could not register Compi for notifications: {created:?}");
            return;
        }
        let mut values = vec![(h!("DisplayName"), display_name.to_owned())];
        if let Some(icon) = icon {
            values.push((h!("IconUri"), icon.to_string_lossy().into_owned()));
        }
        for (name, value) in values {
            let data: Vec<u8> = value
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            // SAFETY: `key` is open with KEY_SET_VALUE; `data` is a
            // NUL-terminated UTF-16 string.
            let set = unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&data)) };
            if set != ERROR_SUCCESS {
                eprintln!("Could not register Compi for notifications: {set:?}");
            }
        }
        // SAFETY: `key` was opened above and is not used afterwards.
        unsafe {
            let _ = RegCloseKey(key);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn app_user_model_ids_are_per_instance_and_sanitized() {
            assert_eq!(app_user_model_id(None), "Compi.Compi");
            assert_eq!(app_user_model_id(Some("")), "Compi.Compi");
            assert_eq!(app_user_model_id(Some("dev-2")), "Compi.Compi.dev-2");
            assert_eq!(app_user_model_id(Some("my_test")), "Compi.Compi.my-test");
            assert_eq!(app_user_model_id(Some("a b/c")), "Compi.Compi.a-b-c");
            assert_eq!(app_user_model_id(Some("..x..y.")), "Compi.Compi.x.y");
            assert_eq!(app_user_model_id(Some("...")), "Compi.Compi.-");
            let long = app_user_model_id(Some(&"a.".repeat(100)));
            assert!(long.len() <= AUMID_LIMIT && !long.ends_with('.'));
            assert_eq!(display_name(None), "Compi");
            assert_eq!(display_name(Some("dev")), "Compi (dev)");
        }

        #[test]
        fn toast_tags_fit_the_windows_limit() {
            assert_eq!(toast_tag("pane-7"), "pane-7");
            assert_eq!(toast_tag(&"x".repeat(100)).len(), TAG_LIMIT);
            assert_eq!(toast_tag(&"é".repeat(100)).chars().count(), TAG_LIMIT);
        }

        #[test]
        fn toast_text_drops_control_characters() {
            assert_eq!(xml_text("a\u{1b}[1mb\u{7}\tc\r\nd"), "a[1mb\tc\r\nd");
        }
    }
}

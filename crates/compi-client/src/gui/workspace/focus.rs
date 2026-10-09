//! How the focused pane is marked when several are visible (`FocusIndicator`):
//! a short accent bar with an outline that fades as focus arrives (Marker), a
//! permanent outline, dimmed unfocused panes, or nothing beyond the cursor.
use super::*;
use gpui::{Animation, AnimationExt as _};

const MARKER_WIDTH: f32 = 28.0;
const MARKER_HEIGHT: f32 = 3.0;
/// How long the arrival outline stays before it is gone.
const FLASH_DURATION: Duration = Duration::from_millis(800);
/// Unfocused panes are darkened by this much with the Dim indicator.
const DIM_ALPHA: f32 = 0.12;

/// The platform asks for reduced motion; read once at launch.
static REDUCED_MOTION: LazyLock<bool> = LazyLock::new(reduced_motion);

impl CompiApp {
    /// Marker: a short accent bar centred on the pane's bottom edge, inside the
    /// padding band, so it never covers a cell or meets the grip at the top.
    pub(super) fn render_focus_marker(&self, width: f32) -> AnyElement {
        div()
            .absolute()
            .bottom(px(2.0))
            .left(px(((width - MARKER_WIDTH) / 2.0).max(0.0)))
            .w(px(MARKER_WIDTH))
            .h(px(MARKER_HEIGHT))
            .rounded_full()
            .bg(color(self.colors().accent))
            .into_any_element()
    }

    /// Marker: an accent outline that fades out over `FLASH_DURATION`. It is keyed
    /// by pane, so it plays whenever focus arrives at a pane (or the pane appears);
    /// with reduced motion it shows for the same time without fading.
    pub(super) fn render_focus_flash(&self, pane_id: &PaneId, radius: f32) -> AnyElement {
        let reduced = *REDUCED_MOTION;
        div()
            .absolute()
            .inset_0()
            .rounded(px(radius))
            .border_1()
            .border_color(color(self.colors().accent))
            .with_animation(
                SharedString::from(format!("focus-flash-{pane_id}")),
                Animation::new(FLASH_DURATION),
                move |outline, progress| {
                    outline.opacity(match (reduced, progress < 1.0) {
                        (_, false) => 0.0,
                        (true, true) => 1.0,
                        (false, true) => 1.0 - progress,
                    })
                },
            )
            .into_any_element()
    }

    /// Dim: a veil over an unfocused pane. It has no hitbox, so the pane stays
    /// fully interactive.
    pub(super) fn render_focus_dim(&self, radius: f32) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .rounded(px(radius))
            .bg(gpui::hsla(0.0, 0.0, 0.0, DIM_ALPHA))
            .into_any_element()
    }
}

#[cfg(target_os = "macos")]
fn reduced_motion() -> bool {
    unsafe {
        let workspace: *mut objc2::runtime::AnyObject =
            msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }
}

/// Windows turns off client-area animations under Settings → Accessibility →
/// Visual effects → Animation effects.
#[cfg(windows)]
fn reduced_motion() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            value: *mut core::ffi::c_void,
            update: u32,
        ) -> i32;
    }
    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled: i32 = 1;
    // SAFETY: the action writes one BOOL to the provided, correctly sized buffer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        )
    };
    ok != 0 && enabled == 0
}

#[cfg(not(any(windows, target_os = "macos")))]
fn reduced_motion() -> bool {
    false
}

//! What a scroll means: zoom, or move through the folder.
//!
//! Every platform sends scroll input in its own units and with its own idioms, and none of the
//! differences show up as a compile error. So the whole mapping lives here as data plus pure
//! functions — the modifier that means "zoom", the conversion from a raw delta to zoom steps and
//! images — and `app.rs` only routes the answer. Two of the three platforms can't open a window
//! in CI, so a pure core is the only way their behaviour gets checked at all.
//!
//! ## What each platform sends
//!
//! - **macOS**: a trackpad reports precise deltas, which winit hands over as `PixelDelta` in
//!   **physical** pixels (`scrollingDeltaY` scaled by the backing factor), so the same swipe
//!   arrives twice as large on a Retina panel as on a 1x display. A wheel gives `LineDelta`.
//! - **Windows**: winit maps `WM_MOUSEWHEEL` to `LineDelta` in notches (the raw delta over
//!   `WHEEL_DELTA`) and never sends `PixelDelta`. A precision touchpad sends that same message
//!   with **fractional** notches, which is why fractions have to accumulate rather than round.
//! - **Linux**: X11 turns button 4/5 clicks into whole `LineDelta` notches; Wayland sends
//!   `PixelDelta` for a touchpad and `LineDelta` for a wheel.
//!
//! ## Direction
//!
//! Positive means "towards the top of the document" everywhere: a wheel rolled away from the
//! user, or whichever trackpad gesture the OS maps to that. macOS ships natural scrolling on and
//! Windows ships it off, but each OS applies its own setting before the delta reaches an app, so
//! the sign already means what that platform's user expects. Nothing here flips it per platform:
//! that would send every Windows user backwards through their folder.
//!
//! ## Pinch to zoom
//!
//! `WindowEvent::PinchGesture` is macOS and iOS only. Windows needs no replacement because a
//! precision touchpad synthesises **Ctrl + wheel** for a pinch, which lands on the zoom path
//! below; opting into `WM_POINTER` would cost the gesture more than it bought. Linux has neither,
//! so Ctrl + scroll is the zoom there.

use winit::event::MouseScrollDelta;
use winit::keyboard::ModifiersState;

/// Zoom steps one wheel notch is worth. One step is `zoom::view::ZOOM_STEP` (5%).
const ZOOM_STEPS_PER_NOTCH: f32 = 1.0;

/// Logical pixels of pixel-precise travel worth one zoom step. Finer than a notch on purpose:
/// zoom is continuous, and a trackpad's whole point is that a slow drag zooms slowly.
const ZOOM_PIXELS_PER_STEP: f32 = 10.0;

/// Logical pixels of pixel-precise travel worth one image. About one notch of finger travel, so
/// a deliberate swipe moves a couple of images rather than a dozen.
const NAVIGATE_PIXELS_PER_IMAGE: f32 = 50.0;

/// Windows' own default for `SPI_GETWHEELSCROLLLINES`, and the baseline the zoom rate is
/// expressed against.
const DEFAULT_WHEEL_SCROLL_LINES: u32 = 3;

/// Images a single event may move. A wheel spin arrives as many events of a notch each, so
/// anything past this is a driver or a device being strange, and jumping half the folder on one
/// event is worse than dropping the excess.
const MAX_IMAGES_PER_EVENT: f32 = 10.0;

/// The modifier a platform's users hold to make a scroll zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomModifier {
    /// macOS: Command, the way Safari and Preview zoom. Ctrl is left alone there because macOS
    /// gives Ctrl + scroll to the system's own screen zoom.
    Command,
    /// Windows and Linux: Ctrl + wheel, which is near-universal on both and is also what a
    /// Windows precision touchpad sends for a pinch.
    Control,
}

impl ZoomModifier {
    /// Whether the held modifiers include this one.
    pub fn held(self, modifiers: &ModifiersState) -> bool {
        match self {
            // `super_key()` is Command on macOS, and the Windows key (and Meta on Linux)
            // everywhere else — which is why this can't be one shared check.
            Self::Command => modifiers.super_key(),
            Self::Control => modifiers.control_key(),
        }
    }
}

/// How one platform's scroll input maps onto viewer actions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollTuning {
    pub zoom_modifier: ZoomModifier,
    /// Zoom steps one wheel notch is worth.
    pub zoom_steps_per_notch: f32,
    /// Logical pixels of pixel-precise travel worth one zoom step.
    pub zoom_pixels_per_step: f32,
    /// Logical pixels of pixel-precise travel worth one image.
    pub navigate_pixels_per_image: f32,
}

impl ScrollTuning {
    // Each build constructs only its own platform's tuning. The other two are still compiled
    // everywhere, because the tests below assert about all three from whichever host runs them —
    // which is the point of keeping this module pure.
    /// macOS: Command to zoom, and the trackpad rates above.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    pub fn macos() -> Self {
        Self {
            zoom_modifier: ZoomModifier::Command,
            zoom_steps_per_notch: ZOOM_STEPS_PER_NOTCH,
            zoom_pixels_per_step: ZOOM_PIXELS_PER_STEP,
            navigate_pixels_per_image: NAVIGATE_PIXELS_PER_IMAGE,
        }
    }

    /// Windows: Ctrl to zoom, with the zoom rate scaled by the user's wheel-speed preference.
    ///
    /// `wheel_scroll_lines` is `SPI_GETWHEELSCROLLLINES`: how many lines of content one notch
    /// moves, three by default. It's the only place Windows lets someone say "my wheel is too
    /// slow", so the continuous quantity a notch drives here — zoom — follows it, clamped so an
    /// extreme setting can't make zooming unusable. Moving through images deliberately doesn't:
    /// a viewer shows one image at a time, so a notch is one image, however a list control would
    /// read the same number.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn windows(wheel_scroll_lines: u32) -> Self {
        // `WHEEL_PAGESCROLL` (u32::MAX, "one screen at a time") clamps to the top like any other
        // fast setting rather than being special-cased into a jump.
        let lines = wheel_scroll_lines.clamp(1, 9) as f32;
        Self {
            zoom_modifier: ZoomModifier::Control,
            zoom_steps_per_notch: lines / DEFAULT_WHEEL_SCROLL_LINES as f32,
            zoom_pixels_per_step: ZOOM_PIXELS_PER_STEP,
            navigate_pixels_per_image: NAVIGATE_PIXELS_PER_IMAGE,
        }
    }

    /// Linux: Ctrl to zoom. No desktop-wide wheel-speed setting exists to read, so the rates are
    /// the defaults.
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    pub fn linux() -> Self {
        Self {
            zoom_modifier: ZoomModifier::Control,
            ..Self::macos()
        }
    }

    /// The tuning for the platform this binary runs on.
    pub fn for_host() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::macos()
        }
        #[cfg(target_os = "windows")]
        {
            Self::windows(crate::platform::windows::wheel_scroll_lines())
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::linux()
        }
    }
}

/// What one scroll event asks the viewer to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollAction {
    /// Zoom by this many steps; positive zooms in.
    Zoom(f32),
    /// Move this many images; positive moves towards the end of the list.
    Navigate(i32),
}

/// One scroll event's vertical travel, in whichever unit the platform sent.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Travel {
    /// Wheel notches, possibly fractional (a Windows precision touchpad).
    Notches(f32),
    /// Logical pixels, already divided back out of the physical ones winit reports.
    Pixels(f32),
}

impl Travel {
    fn of(delta: MouseScrollDelta, scale_factor: f64) -> Self {
        match delta {
            MouseScrollDelta::LineDelta(_, y) => Self::Notches(y),
            // Physical pixels, so a Retina swipe would otherwise count double what the same
            // finger travel counts on a 1x display.
            MouseScrollDelta::PixelDelta(position) => {
                Self::Pixels((position.y / scale_factor.max(f64::EPSILON)) as f32)
            }
        }
    }

    /// Zoom steps this travel is worth; positive zooms in.
    fn zoom_steps(self, tuning: &ScrollTuning) -> f32 {
        match self {
            Self::Notches(notches) => notches * tuning.zoom_steps_per_notch,
            Self::Pixels(pixels) => pixels / tuning.zoom_pixels_per_step,
        }
    }

    /// Images this travel is worth; positive moves towards the end of the list. Scrolling down
    /// (a negative delta) is what moves forward, the way scrolling down a page moves into it.
    fn images_forward(self, tuning: &ScrollTuning) -> f32 {
        match self {
            Self::Notches(notches) => -notches,
            Self::Pixels(pixels) => -pixels / tuning.navigate_pixels_per_image,
        }
    }
}

/// The scroll input's live state: the platform's tuning, plus how far a pixel-precise device has
/// travelled towards the next image.
pub struct Scroll {
    tuning: ScrollTuning,
    /// Fractional images carried between events. A touchpad reports a swipe as a stream of small
    /// deltas, so without this the app either moves an image per event (dozens per swipe) or
    /// never moves at all.
    pending_images: f32,
}

impl Scroll {
    /// The host platform's scroll behaviour. Reads the system's wheel preference once, so a
    /// change to it during a session applies at the next launch.
    pub fn for_host() -> Self {
        Self::with_tuning(ScrollTuning::for_host())
    }

    pub fn with_tuning(tuning: ScrollTuning) -> Self {
        Self {
            tuning,
            pending_images: 0.0,
        }
    }

    /// Whether a scroll right now zooms rather than moving through the folder: the user either
    /// turned "Scroll to zoom" on, or is holding the platform's zoom modifier.
    pub fn zooms(&self, modifiers: &ModifiersState, scroll_to_zoom: bool) -> bool {
        scroll_to_zoom || self.tuning.zoom_modifier.held(modifiers)
    }

    /// Turn one scroll event into what it asks for, or `None` when it asks for nothing yet (a
    /// touchpad delta too small to have earned an image).
    pub fn interpret(
        &mut self,
        delta: MouseScrollDelta,
        scale_factor: f64,
        modifiers: &ModifiersState,
        scroll_to_zoom: bool,
    ) -> Option<ScrollAction> {
        let travel = Travel::of(delta, scale_factor);

        if self.zooms(modifiers, scroll_to_zoom) {
            // Whatever the fingers had going towards an image, they're zooming now.
            self.pending_images = 0.0;
            let steps = travel.zoom_steps(&self.tuning);
            return (steps.abs() > f32::EPSILON).then_some(ScrollAction::Zoom(steps));
        }

        let images = travel
            .images_forward(&self.tuning)
            .clamp(-MAX_IMAGES_PER_EVENT, MAX_IMAGES_PER_EVENT);
        if images.abs() <= f32::EPSILON {
            return None;
        }
        // A reversal answers immediately instead of first spending what the other direction had
        // banked.
        if images.signum() != self.pending_images.signum() {
            self.pending_images = 0.0;
        }
        self.pending_images += images;
        let whole = self.pending_images.trunc();
        self.pending_images -= whole;
        (whole != 0.0).then_some(ScrollAction::Navigate(whole as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    fn wheel(notches: f32) -> MouseScrollDelta {
        MouseScrollDelta::LineDelta(0.0, notches)
    }

    /// A trackpad delta, in the physical pixels winit reports.
    fn touchpad(physical_pixels: f64) -> MouseScrollDelta {
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, physical_pixels))
    }

    fn every_platform() -> [(&'static str, ScrollTuning); 3] {
        [
            ("macOS", ScrollTuning::macos()),
            ("Windows", ScrollTuning::windows(DEFAULT_WHEEL_SCROLL_LINES)),
            ("Linux", ScrollTuning::linux()),
        ]
    }

    // ── The zoom modifier ────────────────────────────────────────────────────────────────────

    /// The one that used to be wrong: `super_key()` is Command on macOS but the Windows key on
    /// Windows, so a shared check gave Windows a zoom gesture nobody performs and sent Ctrl +
    /// wheel through to navigation instead.
    #[test]
    fn each_platform_zooms_with_its_own_modifier() {
        let command = ModifiersState::SUPER;
        let control = ModifiersState::CONTROL;

        let macos = Scroll::with_tuning(ScrollTuning::macos());
        assert!(macos.zooms(&command, false), "macOS zooms with Command");
        assert!(
            !macos.zooms(&control, false),
            "and leaves Ctrl to the system's screen zoom"
        );

        for tuning in [ScrollTuning::windows(3), ScrollTuning::linux()] {
            let scroll = Scroll::with_tuning(tuning);
            assert!(scroll.zooms(&control, false), "Ctrl + wheel zooms");
            assert!(
                !scroll.zooms(&command, false),
                "the Windows / Meta key doesn't"
            );
        }
    }

    #[test]
    fn the_scroll_to_zoom_setting_needs_no_modifier_anywhere() {
        for (platform, tuning) in every_platform() {
            let scroll = Scroll::with_tuning(tuning);
            assert!(
                scroll.zooms(&ModifiersState::empty(), true),
                "{platform} honours the setting"
            );
        }
    }

    #[test]
    fn the_host_tuning_matches_the_platform_this_is_built_for() {
        let expected = if cfg!(target_os = "macos") {
            ZoomModifier::Command
        } else {
            ZoomModifier::Control
        };
        assert_eq!(ScrollTuning::for_host().zoom_modifier, expected);
    }

    // ── Direction ────────────────────────────────────────────────────────────────────────────

    /// Scrolling down moves forward on every platform, because each OS has already applied its
    /// own natural-scrolling setting to the sign. Flipping this per platform is the tempting
    /// wrong fix: it would reverse the wheel for every Windows user.
    #[test]
    fn scrolling_down_moves_forward_everywhere() {
        for (platform, tuning) in every_platform() {
            let mut scroll = Scroll::with_tuning(tuning);
            assert_eq!(
                scroll.interpret(wheel(-1.0), 1.0, &ModifiersState::empty(), false),
                Some(ScrollAction::Navigate(1)),
                "{platform}: a notch down is the next image"
            );
            assert_eq!(
                scroll.interpret(wheel(1.0), 1.0, &ModifiersState::empty(), false),
                Some(ScrollAction::Navigate(-1)),
                "{platform}: a notch up is the previous one"
            );
        }
    }

    #[test]
    fn scrolling_up_zooms_in_everywhere() {
        for (platform, tuning) in every_platform() {
            let mut scroll = Scroll::with_tuning(tuning);
            let action = scroll.interpret(wheel(1.0), 1.0, &ModifiersState::empty(), true);
            assert_eq!(
                action,
                Some(ScrollAction::Zoom(1.0)),
                "{platform}: a notch up is one step in"
            );
        }
    }

    // ── A wheel ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_notch_is_one_image_whatever_the_wheel_speed_setting_says() {
        // A viewer shows one image at a time, so the lines-per-notch preference doesn't multiply
        // it the way it would multiply a list control's rows.
        for lines in [1, 3, 9, u32::MAX] {
            let mut scroll = Scroll::with_tuning(ScrollTuning::windows(lines));
            assert_eq!(
                scroll.interpret(wheel(-1.0), 1.0, &ModifiersState::empty(), false),
                Some(ScrollAction::Navigate(1)),
                "{lines} lines per notch"
            );
        }
    }

    #[test]
    fn a_faster_wheel_setting_zooms_faster_on_windows() {
        let zoom_for = |lines| {
            let mut scroll = Scroll::with_tuning(ScrollTuning::windows(lines));
            scroll.interpret(wheel(1.0), 1.0, &ModifiersState::CONTROL, false)
        };
        assert_eq!(zoom_for(3), Some(ScrollAction::Zoom(1.0)), "the default");
        assert_eq!(zoom_for(9), Some(ScrollAction::Zoom(3.0)), "a fast wheel");
        assert_eq!(
            zoom_for(1),
            Some(ScrollAction::Zoom(1.0 / 3.0)),
            "a slow one"
        );
        // "One screen at a time" clamps like any other fast setting.
        assert_eq!(zoom_for(u32::MAX), zoom_for(9));
    }

    // ── A trackpad ───────────────────────────────────────────────────────────────────────────

    /// The old magic divisor's real cost: a swipe is dozens of small events, and each one moved
    /// an image. A full-folder jump per flick.
    #[test]
    fn a_swipe_accumulates_into_whole_images_rather_than_one_per_event() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        let mut moved = 0;
        // 50 logical pixels of travel, five pixels at a time.
        for _ in 0..10 {
            if let Some(ScrollAction::Navigate(images)) =
                scroll.interpret(touchpad(-5.0), 1.0, &ModifiersState::empty(), false)
            {
                moved += images;
            }
        }
        assert_eq!(moved, 1, "one image per notch's worth of finger travel");
    }

    /// winit reports macOS trackpad deltas in physical pixels, so the same gesture is twice the
    /// number on a Retina panel. Dividing it back out is what keeps a swipe worth the same on
    /// the built-in display and an external 1x one.
    #[test]
    fn the_same_swipe_is_worth_the_same_at_any_display_scale() {
        let steps_at = |scale| {
            let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
            scroll.interpret(
                touchpad(20.0 * scale),
                scale,
                &ModifiersState::empty(),
                true,
            )
        };
        assert_eq!(steps_at(1.0), Some(ScrollAction::Zoom(2.0)));
        assert_eq!(steps_at(2.0), steps_at(1.0), "Retina");
        assert_eq!(steps_at(1.5), steps_at(1.0), "a fractional Windows scale");
    }

    #[test]
    fn a_touchpad_zooms_more_finely_than_a_notch_at_a_time() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        let action = scroll.interpret(touchpad(3.0), 1.0, &ModifiersState::empty(), true);
        let Some(ScrollAction::Zoom(steps)) = action else {
            panic!("expected a zoom, got {action:?}");
        };
        assert!(
            steps > 0.0 && steps < 1.0,
            "a few pixels are a fraction of a step, got {steps}"
        );
    }

    #[test]
    fn travel_too_small_to_have_earned_an_image_asks_for_nothing() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        assert_eq!(
            scroll.interpret(touchpad(-1.0), 1.0, &ModifiersState::empty(), false),
            None
        );
    }

    #[test]
    fn reversing_direction_answers_at_once_instead_of_spending_what_was_banked() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        // Most of the way towards the next image, then a full notch backwards.
        assert_eq!(
            scroll.interpret(touchpad(-40.0), 1.0, &ModifiersState::empty(), false),
            None
        );
        assert_eq!(
            scroll.interpret(touchpad(50.0), 1.0, &ModifiersState::empty(), false),
            Some(ScrollAction::Navigate(-1)),
            "the previous image, not a stall while the banked forward travel is spent"
        );
    }

    #[test]
    fn a_zoom_doesnt_leave_the_folder_half_a_step_along() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        scroll.interpret(touchpad(-40.0), 1.0, &ModifiersState::empty(), false);
        scroll.interpret(touchpad(-40.0), 1.0, &ModifiersState::SUPER, false);
        assert_eq!(
            scroll.interpret(touchpad(-20.0), 1.0, &ModifiersState::empty(), false),
            None,
            "the pre-zoom travel is gone, so 20 pixels can't complete an image"
        );
    }

    // ── Bad input ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn one_absurd_event_cant_jump_the_whole_folder() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        let action = scroll.interpret(touchpad(-100_000.0), 1.0, &ModifiersState::empty(), false);
        assert_eq!(
            action,
            Some(ScrollAction::Navigate(MAX_IMAGES_PER_EVENT as i32))
        );
    }

    #[test]
    fn a_zero_delta_asks_for_nothing() {
        for (platform, tuning) in every_platform() {
            let mut scroll = Scroll::with_tuning(tuning);
            assert_eq!(
                scroll.interpret(wheel(0.0), 1.0, &ModifiersState::empty(), false),
                None,
                "{platform} navigating"
            );
            assert_eq!(
                scroll.interpret(wheel(0.0), 1.0, &ModifiersState::empty(), true),
                None,
                "{platform} zooming"
            );
        }
    }

    /// A scale factor of zero would otherwise divide a trackpad delta into infinity.
    #[test]
    fn a_nonsense_scale_factor_doesnt_produce_a_nonsense_jump() {
        let mut scroll = Scroll::with_tuning(ScrollTuning::macos());
        let action = scroll.interpret(touchpad(-10.0), 0.0, &ModifiersState::empty(), false);
        assert_eq!(
            action,
            Some(ScrollAction::Navigate(MAX_IMAGES_PER_EVENT as i32))
        );
    }
}

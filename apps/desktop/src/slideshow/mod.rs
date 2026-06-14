//! Slideshow: auto-advance through the directory on a timer, with an optional
//! crossfade between images.
//!
//! The feature is intentionally thin. It owns no navigation logic — when the
//! timer fires, `App::slideshow_advance` reuses the normal `navigate_by` /
//! `navigate_to_first` paths. This module owns the runtime state (is it
//! running, when does the next advance fire, is a crossfade in flight) plus
//! the small pure helpers (interval, speed stepping) that are worth testing in
//! isolation. See `CLAUDE.md` for the timing and crossfade design.

#[cfg(target_os = "macos")]
pub mod settings_panel;

use crate::settings::Settings;
use std::time::{Duration, Instant};

/// Shortest time-per-image the user can pick (Settings slider floor + `]` cap).
pub const MIN_SECONDS: u32 = 1;
/// Longest time-per-image the user can pick (Settings slider ceiling + `[` cap).
pub const MAX_SECONDS: u32 = 30;
/// Default time-per-image for a fresh install.
pub const DEFAULT_SECONDS: u32 = 4;
/// Crossfade duration between two images. Fixed for now; a future setting can
/// replace this constant (see `CLAUDE.md`).
pub const CROSSFADE_DURATION: Duration = Duration::from_millis(300);

/// Clamp a user-chosen seconds value into the supported range.
pub fn clamp_seconds(seconds: u32) -> u32 {
    seconds.clamp(MIN_SECONDS, MAX_SECONDS)
}

/// Step the time-per-image one notch. `faster` shortens the interval (the `]`
/// key / "Increase speed"); otherwise it lengthens it (`[` / "Decrease
/// speed"). Always lands inside `MIN_SECONDS..=MAX_SECONDS`.
pub fn stepped_seconds(current: u32, faster: bool) -> u32 {
    let stepped = if faster {
        current.saturating_sub(1)
    } else {
        current.saturating_add(1)
    };
    clamp_seconds(stepped)
}

/// Per-feature runtime state owned by `App`.
pub struct State {
    /// Whether the slideshow is currently auto-advancing.
    pub running: bool,
    /// Seconds each image stays on screen. Mirrors `Settings::slideshow_seconds`.
    pub seconds: u32,
    /// Whether advances crossfade (vs. cut). Mirrors `Settings::slideshow_crossfade`.
    pub crossfade_enabled: bool,
    /// Whether the slideshow wraps past the last image. Mirrors
    /// `Settings::slideshow_loop`.
    pub loop_enabled: bool,
    /// When the next auto-advance fires. `Some` only while `running`.
    pub next_advance: Option<Instant>,
    /// Start time of the in-flight crossfade, if any. Drives the per-frame
    /// fade factor in `App::drive_crossfade`.
    pub crossfade: Option<Instant>,
}

impl State {
    pub fn new() -> Self {
        Self {
            running: false,
            seconds: DEFAULT_SECONDS,
            crossfade_enabled: true,
            loop_enabled: true,
            next_advance: None,
            crossfade: None,
        }
    }

    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            seconds: clamp_seconds(settings.slideshow_seconds),
            crossfade_enabled: settings.slideshow_crossfade,
            loop_enabled: settings.slideshow_loop,
            ..Self::new()
        }
    }

    /// How long each image stays on screen.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.seconds as u64)
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_faster_shortens_and_clamps_at_floor() {
        assert_eq!(stepped_seconds(5, true), 4);
        assert_eq!(stepped_seconds(MIN_SECONDS, true), MIN_SECONDS);
        // Never underflows past the floor.
        assert_eq!(stepped_seconds(1, true), 1);
    }

    #[test]
    fn stepping_slower_lengthens_and_clamps_at_ceiling() {
        assert_eq!(stepped_seconds(5, false), 6);
        assert_eq!(stepped_seconds(MAX_SECONDS, false), MAX_SECONDS);
    }

    #[test]
    fn clamp_pins_out_of_range_values() {
        assert_eq!(clamp_seconds(0), MIN_SECONDS);
        assert_eq!(clamp_seconds(999), MAX_SECONDS);
        assert_eq!(clamp_seconds(15), 15);
    }

    #[test]
    fn from_settings_clamps_a_corrupt_value() {
        let s = Settings {
            slideshow_seconds: 100,
            ..Settings::default()
        };
        assert_eq!(State::from_settings(&s).seconds, MAX_SECONDS);
    }

    #[test]
    fn interval_matches_seconds() {
        let s = State {
            seconds: 7,
            ..State::new()
        };
        assert_eq!(s.interval(), Duration::from_secs(7));
    }
}

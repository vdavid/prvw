//! What the display behind the window can do with HDR, read out of wgpu's `DisplayHdrInfo`.
//!
//! Two numbers that mean different things, and conflating them re-decodes on every brightness
//! tweak (`docs/specs/hdr-images.md`, section 3):
//!
//! - **`capable`** (potential headroom): can this display show HDR at all? Static per display and
//!   display mode. It decides whether decoders produce HDR and whether the surface may go EDR, so
//!   crossing it clears the image cache.
//! - **`headroom_now`** (current headroom): how much brighter than SDR white the display goes right
//!   now. On macOS it moves with brightness. It only feeds the shader's per-frame uniform, so a
//!   change is one buffer write and a redraw.
//!
//! Pure, so every platform's reading is testable from any host.

/// The display's HDR state as the app tracks it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayHdr {
    /// The display can present HDR at all (potential headroom above 1.0, or HDR mode on).
    pub capable: bool,
    /// Current headroom over SDR white, as a multiplier, at least `1.0`.
    pub headroom_now: f32,
}

impl DisplayHdr {
    /// An SDR display, and what the app assumes before a renderer exists to ask.
    pub const SDR: Self = Self {
        capable: false,
        headroom_now: 1.0,
    };

    /// Read wgpu's answer.
    ///
    /// - **Capability** comes from whichever field the platform fills. macOS fills
    ///   `headroom` (all three `NSScreen` values), and `potential` is the static ceiling: 16.0 on
    ///   an XDR panel, 1.0 on an SDR one. wgpu's `coarse.high_dynamic_range` is derived from
    ///   `current` there, so it would flicker with brightness and is ignored whenever `headroom`
    ///   is present. Windows leaves `headroom` empty and sets `coarse.high_dynamic_range` from the
    ///   output's DXGI colour space (G2084 or G10, which is "HDR mode on"). A panel whose EDID
    ///   peak sits at or below its SDR white is still capable there.
    /// - **Current headroom** is wgpu's `tone_map_headroom()`: `current` on macOS, panel peak over
    ///   SDR white on Windows. Anything non-finite or under 1.0 reads as 1.0.
    pub fn from_info(info: &wgpu::DisplayHdrInfo) -> Self {
        let capable = match info.headroom {
            Some(headroom) => headroom
                .potential
                .is_some_and(|potential| potential.is_finite() && potential > 1.0),
            None => info.coarse.and_then(|coarse| coarse.high_dynamic_range) == Some(true),
        };
        let headroom_now = info
            .tone_map_headroom()
            .filter(|headroom| headroom.is_finite() && *headroom >= 1.0)
            .unwrap_or(1.0);
        Self {
            capable,
            headroom_now,
        }
    }
}

/// What the QA server can force in place of the display's own answer, field by field. CI displays
/// have no HDR, so this is how a test drives the headroom and capability paths.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DisplayHdrOverride {
    pub capable: Option<bool>,
    pub headroom_now: Option<f32>,
}

impl DisplayHdrOverride {
    /// The measured state with every overridden field replaced. A forced headroom is held to at
    /// least 1.0, like a measured one.
    pub fn apply(self, measured: DisplayHdr) -> DisplayHdr {
        DisplayHdr {
            capable: self.capable.unwrap_or(measured.capable),
            headroom_now: self
                .headroom_now
                .filter(|headroom| headroom.is_finite())
                .map_or(measured.headroom_now, |headroom| headroom.max(1.0)),
        }
    }
}

/// How often the current headroom is re-read while an HDR image is on screen. Neither macOS nor
/// Windows sends a notification for a brightness or SDR-white-level change, so this is a poll.
/// Half a second is what "the highlights adapt as you drag the brightness slider" needs, and one
/// `display_hdr_info` read per tick costs microseconds on macOS and an adapter walk on Windows.
pub const HEADROOM_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether the current headroom moved enough to be worth a redraw: more than 1% either way.
/// macOS reports the value with float noise, and a redraw per tick of noise is idle cost.
pub fn headroom_moved(old: f32, new: f32) -> bool {
    (new - old).abs() > old.abs() * 0.01
}

/// When the headroom poll should next fire, given whether it should run at all and when it was
/// last due. Stopping drops the deadline; starting, or a deadline that has passed, schedules one
/// [`HEADROOM_POLL`] from `now`.
pub fn next_headroom_poll(
    should_poll: bool,
    due: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    if !should_poll {
        return None;
    }
    match due {
        Some(due) if due > now => Some(due),
        _ => Some(now + HEADROOM_POLL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple(current: f32, potential: Option<f32>) -> wgpu::DisplayHdrInfo {
        wgpu::DisplayHdrInfo {
            headroom: Some(wgpu::DisplayHeadroom {
                current: Some(current),
                potential,
                reference: None,
            }),
            // What wgpu derives on macOS: `current > 1.0`.
            coarse: Some(wgpu::DisplayCoarseRange {
                high_dynamic_range: Some(current > 1.0),
                gamut: None,
            }),
            ..Default::default()
        }
    }

    fn windows(hdr_mode: bool, max_nits: f32, sdr_white_nits: f32) -> wgpu::DisplayHdrInfo {
        wgpu::DisplayHdrInfo {
            luminance: Some(wgpu::DisplayLuminance {
                max_nits: Some(max_nits),
                sdr_white_nits: Some(sdr_white_nits),
                ..Default::default()
            }),
            coarse: Some(wgpu::DisplayCoarseRange {
                high_dynamic_range: Some(hdr_mode),
                gamut: None,
            }),
            ..Default::default()
        }
    }

    /// The probe on David's MBP read current 1.0 and potential 16.0. Gating on current would have
    /// called the XDR panel SDR, which is the launch loop the spec's findings warn about.
    #[test]
    fn an_xdr_panel_at_full_brightness_is_capable_with_no_headroom_right_now() {
        let hdr = DisplayHdr::from_info(&apple(1.0, Some(16.0)));
        assert_eq!(
            hdr,
            DisplayHdr {
                capable: true,
                headroom_now: 1.0
            }
        );
    }

    #[test]
    fn a_mac_reads_its_current_headroom_as_headroom_now() {
        assert_eq!(
            DisplayHdr::from_info(&apple(2.5, Some(16.0))).headroom_now,
            2.5
        );
    }

    #[test]
    fn an_sdr_mac_display_is_not_capable() {
        assert_eq!(
            DisplayHdr::from_info(&apple(1.0, Some(1.0))),
            DisplayHdr::SDR
        );
    }

    /// Off the main thread wgpu answers with no potential; that's "unknown", not "capable".
    #[test]
    fn a_mac_with_no_potential_reading_is_not_capable_even_if_current_says_otherwise() {
        assert!(!DisplayHdr::from_info(&apple(2.0, None)).capable);
    }

    #[test]
    fn windows_in_hdr_mode_is_capable_and_reads_peak_over_sdr_white() {
        let hdr = DisplayHdr::from_info(&windows(true, 1000.0, 250.0));
        assert_eq!(
            hdr,
            DisplayHdr {
                capable: true,
                headroom_now: 4.0
            }
        );
    }

    /// The panel's EDID claims less than the SDR white the slider asks for, so there's no headroom
    /// to use right now, but HDR mode is on and the display stays capable.
    #[test]
    fn windows_in_hdr_mode_with_a_dim_edid_peak_is_still_capable() {
        let hdr = DisplayHdr::from_info(&windows(true, 200.0, 240.0));
        assert_eq!(
            hdr,
            DisplayHdr {
                capable: true,
                headroom_now: 1.0
            }
        );
    }

    #[test]
    fn windows_in_sdr_mode_is_not_capable() {
        assert_eq!(
            DisplayHdr::from_info(&windows(false, 400.0, 80.0)),
            DisplayHdr::SDR
        );
    }

    /// Linux (Vulkan) and anything else that reports nothing at all.
    #[test]
    fn a_display_that_reports_nothing_is_sdr() {
        assert_eq!(
            DisplayHdr::from_info(&wgpu::DisplayHdrInfo::default()),
            DisplayHdr::SDR
        );
    }

    #[test]
    fn an_empty_override_changes_nothing() {
        let measured = DisplayHdr {
            capable: true,
            headroom_now: 3.0,
        };
        assert_eq!(DisplayHdrOverride::default().apply(measured), measured);
    }

    #[test]
    fn an_override_replaces_only_the_fields_it_sets() {
        let forced = DisplayHdrOverride {
            capable: None,
            headroom_now: Some(4.0),
        };
        assert_eq!(
            forced.apply(DisplayHdr::SDR),
            DisplayHdr {
                capable: false,
                headroom_now: 4.0
            }
        );
        let forced = DisplayHdrOverride {
            capable: Some(true),
            headroom_now: Some(0.5),
        };
        assert_eq!(
            forced.apply(DisplayHdr::SDR),
            DisplayHdr {
                capable: true,
                headroom_now: 1.0
            }
        );
    }

    #[test]
    fn the_poll_stops_when_it_shouldnt_run() {
        let now = std::time::Instant::now();
        assert_eq!(
            next_headroom_poll(false, Some(now + HEADROOM_POLL), now),
            None
        );
        assert_eq!(next_headroom_poll(false, None, now), None);
    }

    #[test]
    fn the_poll_keeps_a_pending_deadline_and_reschedules_a_passed_one() {
        let now = std::time::Instant::now();
        let pending = now + std::time::Duration::from_millis(200);
        assert_eq!(next_headroom_poll(true, Some(pending), now), Some(pending));
        assert_eq!(
            next_headroom_poll(true, None, now),
            Some(now + HEADROOM_POLL)
        );
        assert_eq!(
            next_headroom_poll(true, Some(now), now),
            Some(now + HEADROOM_POLL)
        );
    }

    #[test]
    fn headroom_moves_only_past_one_percent() {
        assert!(!headroom_moved(2.0, 2.0));
        assert!(!headroom_moved(2.0, 2.019));
        assert!(headroom_moved(2.0, 2.03));
        assert!(headroom_moved(2.0, 1.97));
        assert!(headroom_moved(1.0, 16.0));
    }
}

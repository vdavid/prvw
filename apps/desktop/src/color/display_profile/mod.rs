//! Which colour profile the display in front of the user is running, and when that changed.
//!
//! Level 2 colour management transforms every image into the profile of the monitor it will be
//! shown on, so the rest of the app needs one answer from here: the ICC bytes for the display the
//! window is currently on. Every platform asks a different OS a different question, and every
//! platform can answer "nothing", so the shared surface is deliberately two functions plus the
//! pure helpers that decide when to ask again and whether an answer is worth adopting.
//!
//! - **macOS** ([`macos`]) reads `CGDisplayCopyColorSpace` for the window's `NSScreen`. That file
//!   also owns the `CAMetalLayer` side of colour (layer colourspace, EDR properties, the
//!   screen-change observer), because on macOS the two are the same conversation with the
//!   compositor. Every Mac ships a real per-display factory profile, so the transform is never a
//!   no-op there.
//! - **Windows** ([`windows`]) reads the ICC profile associated with the window's monitor through
//!   `GetICMProfileW`. Windows ships no per-display profile of its own, so an uncalibrated machine
//!   answers with the system sRGB profile and the transform costs nothing; on the calibrated
//!   monitors this app exists for, it is the whole product.
//! - **Linux** has no answer yet, so [`display_icc`] is `None` there and the transform target
//!   stays the generated sRGB profile from [`crate::color::srgb_icc_bytes`].
//!
//! ## Two rules that aren't one platform's business
//!
//! **A profile is adopted only if it parses.** macOS hands over bytes CoreGraphics built, but
//! Windows hands over a *file path* out of the registry, and the file behind it can be missing,
//! truncated, or not an ICC profile at all. `color::transform_icc` answers an unparseable target
//! by skipping the transform, which leaves the image in its own source space: further from right
//! than sRGB would have been. So [`usable_profile`] parses before adopting and lets the caller
//! fall back to generated sRGB. See [`MonitorTracker`] for the other one.

// Linux reads no display profile, so every helper below is dead there: the pure ones have no
// caller, and their tests aren't what `-D dead-code` counts. One allow for the module beats nine
// on individual items, and it costs nothing a Linux display-profile spec wouldn't immediately
// take back (it would give all of these a caller).
#![cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]

use winit::window::Window;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "macos")]
pub use macos::{
    register_screen_change_observer, restore_layer_colorspace, set_layer_colorspace,
    set_layer_edr_state, set_metal_layer_transparent,
};

/// The ICC bytes for the display `window` is on, or `None` when this platform can't say.
///
/// A `None` is ordinary rather than exceptional: a Mac over a screen-sharing session, a Windows
/// monitor whose profile file has been deleted out from under the registry entry, or Linux, where
/// nothing reads a display profile yet. The caller falls back to generated sRGB.
pub fn display_icc(window: &Window) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        macos::display_icc(window)
    }
    #[cfg(target_os = "windows")]
    {
        windows::display_icc(window)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = window;
        None
    }
}

/// Which display `window` is on, when this platform both can say and needs asking.
///
/// `None` off Windows, for a different reason on each platform. macOS is *told* when a window
/// changes screen: `NSWindowDidChangeScreenNotification` becomes `AppCommand::DisplayChanged`, so
/// it never has to watch moves. Linux reads no display profile at all, so there'd be nothing to
/// re-read. Windows has neither an event nor a way around one, which is what [`MonitorTracker`] is
/// for.
pub fn current_monitor(window: &Window) -> Option<MonitorId> {
    #[cfg(target_os = "windows")]
    {
        windows::current_monitor(window)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        None
    }
}

/// Vet bytes an OS handed us before they become the transform target.
///
/// Returns the bytes when `moxcms` can parse them as an ICC profile, and `None` otherwise, so a
/// caller can fall back to generated sRGB rather than adopting a target that would silently
/// disable the transform downstream.
pub fn usable_profile(bytes: Vec<u8>) -> Option<Vec<u8>> {
    match moxcms::ColorProfile::new_from_slice(&bytes) {
        Ok(_) => Some(bytes),
        Err(why) => {
            log::warn!(
                "Ignoring a {} byte display profile that doesn't parse ({why}), \
                 falling back to sRGB",
                bytes.len()
            );
            None
        }
    }
}

/// One platform's handle for a monitor, as an opaque number: a `CGDirectDisplayID` on macOS, an
/// `HMONITOR` on Windows. Only ever compared against another one from the same platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorId(pub u64);

/// Which display we last read a profile for, so a window drag doesn't re-read one per mouse move.
///
/// **Why this exists.** Windows has no "the window changed screens" event. The signal is
/// `WindowEvent::Moved`, which arrives for every pixel of a title-bar drag, and answering each one
/// by opening and parsing an ICC file would make dragging the window stutter. Asking the OS which
/// monitor a window is on is cheap; reading the profile is not. So the cheap question runs per
/// event and this decides whether the expensive one follows.
///
/// **Why [`forget`](Self::forget) exists.** A monitor handle is only unique among the monitors
/// that exist right now: Windows recycles `HMONITOR` values when the display configuration
/// changes, and the user can re-associate a profile with a display without any handle changing at
/// all. Both arrive as `WM_DISPLAYCHANGE` rather than as a move, and both leave a matching handle
/// meaning nothing, so that message drops what we know instead of comparing against it.
#[derive(Debug, Default)]
pub struct MonitorTracker {
    current: Option<MonitorId>,
}

impl MonitorTracker {
    pub const fn new() -> Self {
        Self { current: None }
    }

    /// The window is on `monitor` now. Answers whether that's a display we haven't read a profile
    /// for, and remembers it either way.
    pub fn moved_to(&mut self, monitor: MonitorId) -> bool {
        let changed = self.current != Some(monitor);
        self.current = Some(monitor);
        changed
    }

    /// Forget which display we were on, so the next [`moved_to`](Self::moved_to) reports a change
    /// whatever it's handed.
    pub fn forget(&mut self) {
        self.current = None;
    }
}

/// Everything up to (not including) the first NUL in a wide string the OS filled a buffer with.
///
/// Win32 "fill this buffer" calls report a capacity, not a length, so the tail past the
/// terminator is whatever was in the buffer before. Returns the whole slice when there's no
/// terminator, which is what a buffer filled exactly to capacity looks like.
///
/// Only Windows calls it, and it lives here rather than beside that caller so a Mac can test it.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn until_nul(buffer: &[u16]) -> &[u16] {
    let end = buffer.iter().position(|&unit| unit == 0);
    &buffer[..end.unwrap_or(buffer.len())]
}

/// A human-readable name for raw ICC bytes, for logging: `"Display P3"`, `"sRGB IEC61966-2.1"`.
/// `None` when the profile carries no readable `desc` tag.
///
/// Both spellings of that tag are read, because both turn up on a photographer's machine. ICC v2
/// stores an ASCII string (`desc` type), which is what Apple's display profiles and Microsoft's
/// shipped sRGB profile use. ICC v4 stores UTF-16 per language (`mluc` type), which is what
/// `moxcms` generates and what most calibration software has written since about 2010.
pub fn describe_icc(icc: &[u8]) -> Option<String> {
    let tag = find_tag(icc, b"desc")?;
    match tag.get(..4)? {
        b"desc" => ascii_description(tag),
        b"mluc" => unicode_description(tag),
        _ => None,
    }
}

/// The bytes of one tag out of an ICC profile's tag table, clamped to the profile. `None` when
/// the profile doesn't carry the tag, or is too short or malformed to say.
fn find_tag<'a>(icc: &'a [u8], signature: &[u8; 4]) -> Option<&'a [u8]> {
    const TABLE: usize = 128;
    const ENTRY: usize = 12;
    let count = be_u32(icc, TABLE)? as usize;
    // The table has to fit inside the profile it describes. That bounds the loop without an
    // arbitrary cap on how many tags a profile is allowed to carry.
    if TABLE + 4 + count.checked_mul(ENTRY)? > icc.len() {
        return None;
    }
    (0..count).find_map(|entry| {
        let base = TABLE + 4 + entry * ENTRY;
        if &icc[base..base + 4] != signature {
            return None;
        }
        let offset = be_u32(icc, base + 4)? as usize;
        let size = be_u32(icc, base + 8)? as usize;
        let end = offset.checked_add(size)?.min(icc.len());
        icc.get(offset..end)
    })
}

/// The ICC v2 `desc` tag: signature, four reserved bytes, a byte count, then ASCII.
fn ascii_description(tag: &[u8]) -> Option<String> {
    let length = be_u32(tag, 8)? as usize;
    let end = 12usize.checked_add(length)?.min(tag.len());
    let text = tag.get(12..end)?;
    Some(trimmed(String::from_utf8_lossy(text).into_owned()))
}

/// The ICC v4 `mluc` tag: signature, four reserved bytes, a record count, a record size, then
/// that many `{language, country, byte length, offset}` records pointing at UTF-16BE strings.
/// American English wins when it's there, since it's the one every profile writes; otherwise the
/// first record does, because a name in a language we didn't ask for still identifies the display.
fn unicode_description(tag: &[u8]) -> Option<String> {
    const RECORDS: usize = 16;
    let count = be_u32(tag, 8)? as usize;
    let stride = be_u32(tag, 12)? as usize;
    if count == 0 || stride < 12 || RECORDS + count.checked_mul(stride)? > tag.len() {
        return None;
    }
    let record = (0..count)
        .map(|index| RECORDS + index * stride)
        .find(|&base| &tag[base..base + 4] == b"enUS")
        .unwrap_or(RECORDS);
    let length = be_u32(tag, record + 4)? as usize;
    let offset = be_u32(tag, record + 8)? as usize;
    let end = offset.checked_add(length)?.min(tag.len());
    let utf16: Vec<u16> = tag
        .get(offset..end)?
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    Some(trimmed(String::from_utf16_lossy(&utf16)))
}

/// A big-endian `u32` at a byte offset, or `None` when the four bytes aren't there.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let four: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(four))
}

/// ICC pads its strings with NULs and its writers pad with spaces.
fn trimmed(text: String) -> String {
    text.trim_end_matches(['\0', ' ']).to_string()
}

/// A one-line summary of a profile for a log line: `" (Display P3)"`, or nothing at all.
pub(crate) fn describe_for_log(icc: &[u8]) -> String {
    describe_icc(icc)
        .map(|d| format!(" ({d})"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_srgb_profile_is_usable() {
        let srgb = crate::color::srgb_icc_bytes().to_vec();
        assert_eq!(usable_profile(srgb.clone()), Some(srgb));
    }

    /// The Windows failure this guards: the registry names a profile file that isn't one, or is
    /// half-written. Adopting it would make `transform_icc` skip the transform entirely, which
    /// leaves the image further from right than sRGB would have.
    #[test]
    fn garbage_is_not_a_usable_profile() {
        assert_eq!(usable_profile(b"not an ICC profile at all".to_vec()), None);
    }

    #[test]
    fn an_empty_file_is_not_a_usable_profile() {
        assert_eq!(usable_profile(Vec::new()), None);
    }

    #[test]
    fn a_truncated_profile_is_not_usable() {
        let mut truncated = crate::color::srgb_icc_bytes().to_vec();
        truncated.truncate(truncated.len() / 2);
        assert_eq!(usable_profile(truncated), None);
    }

    #[test]
    fn the_first_display_seen_is_always_a_change() {
        let mut tracker = MonitorTracker::new();
        assert!(tracker.moved_to(MonitorId(1)));
    }

    #[test]
    fn staying_on_one_display_is_not_a_change() {
        let mut tracker = MonitorTracker::new();
        tracker.moved_to(MonitorId(7));
        // A title-bar drag sends one of these per mouse move.
        assert!(!tracker.moved_to(MonitorId(7)));
        assert!(!tracker.moved_to(MonitorId(7)));
    }

    #[test]
    fn crossing_to_another_display_is_a_change() {
        let mut tracker = MonitorTracker::new();
        tracker.moved_to(MonitorId(7));
        assert!(tracker.moved_to(MonitorId(9)));
        assert!(!tracker.moved_to(MonitorId(9)));
        assert!(tracker.moved_to(MonitorId(7)));
    }

    /// `WM_DISPLAYCHANGE` can leave the same handle meaning a different monitor, or the same
    /// monitor carrying a different profile, so a match after one proves nothing.
    #[test]
    fn forgetting_makes_the_same_display_count_as_a_change() {
        let mut tracker = MonitorTracker::new();
        tracker.moved_to(MonitorId(7));
        tracker.forget();
        assert!(tracker.moved_to(MonitorId(7)));
    }

    #[test]
    fn a_wide_string_stops_at_its_terminator() {
        let buffer = [b'C' as u16, b':' as u16, 0, 0xDEAD, 0xBEEF];
        assert_eq!(until_nul(&buffer), &[b'C' as u16, b':' as u16]);
    }

    #[test]
    fn a_wide_string_filled_to_capacity_keeps_every_unit() {
        let buffer = [b'C' as u16, b':' as u16];
        assert_eq!(until_nul(&buffer), &buffer);
    }

    #[test]
    fn an_empty_buffer_is_an_empty_string() {
        assert!(until_nul(&[]).is_empty());
        assert!(until_nul(&[0]).is_empty());
    }

    /// `moxcms` writes ICC v4, so this is the `mluc` half of `describe_icc` against a profile we
    /// generate rather than a fixture. Before v4 support it returned `None` here.
    #[test]
    fn a_generated_srgb_profile_describes_itself() {
        assert_eq!(
            describe_icc(crate::color::srgb_icc_bytes()).as_deref(),
            Some("sRGB IEC61966-2.1")
        );
    }

    /// The ICC v2 spelling: Apple's display profiles and the `sRGB Color Space Profile.icm` that
    /// Windows answers `GetICMProfileW` with when nothing is calibrated both use it.
    #[test]
    fn an_icc_v2_profile_describes_itself() {
        let profile = profile_with_desc_tag(&ascii_desc_tag("Display P3"));
        assert_eq!(describe_icc(&profile).as_deref(), Some("Display P3"));
    }

    #[test]
    fn an_mluc_tag_prefers_american_english() {
        let tag = mluc_desc_tag(&[(*b"deDE", "Bildschirm"), (*b"enUS", "Display")]);
        let profile = profile_with_desc_tag(&tag);
        assert_eq!(describe_icc(&profile).as_deref(), Some("Display"));
    }

    /// A profile written for one locale still names the monitor, which is all a log line wants.
    #[test]
    fn an_mluc_tag_without_english_takes_the_first_record() {
        let tag = mluc_desc_tag(&[(*b"deDE", "Bildschirm")]);
        let profile = profile_with_desc_tag(&tag);
        assert_eq!(describe_icc(&profile).as_deref(), Some("Bildschirm"));
    }

    #[test]
    fn a_profile_with_no_desc_tag_describes_nothing() {
        let profile = profile_with_tag(*b"wtpt", &ascii_desc_tag("not the tag we want"));
        assert_eq!(describe_icc(&profile), None);
    }

    /// A tag table that claims more entries than the file can hold is the shape a truncated
    /// download takes, and it must not index past the end.
    #[test]
    fn a_lying_tag_table_describes_nothing() {
        let mut profile = profile_with_desc_tag(&ascii_desc_tag("Display P3"));
        profile[128..132].copy_from_slice(&10_000u32.to_be_bytes());
        assert_eq!(describe_icc(&profile), None);
    }

    #[test]
    fn a_tag_that_overruns_the_profile_is_clamped_rather_than_panicking() {
        let mut profile = profile_with_desc_tag(&ascii_desc_tag("Display P3"));
        let size = profile.len() as u32;
        profile[140..144].copy_from_slice(&(size * 4).to_be_bytes());
        // Whatever comes back, the point is that it comes back.
        let _ = describe_icc(&profile);
    }

    #[test]
    fn nothing_describable_logs_as_nothing() {
        assert_eq!(describe_for_log(b"too short"), "");
    }

    /// An ICC v2 `desc` tag: signature, four reserved bytes, the ASCII length including its NUL,
    /// then the string.
    fn ascii_desc_tag(text: &str) -> Vec<u8> {
        let mut tag = b"desc\0\0\0\0".to_vec();
        tag.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
        tag.extend_from_slice(text.as_bytes());
        tag.push(0);
        tag
    }

    /// An ICC v4 `mluc` tag: signature, four reserved bytes, the record count and size, the
    /// records, then the UTF-16BE strings they point at.
    fn mluc_desc_tag(entries: &[([u8; 4], &str)]) -> Vec<u8> {
        const HEADER: usize = 16;
        const RECORD: usize = 12;
        let mut tag = b"mluc\0\0\0\0".to_vec();
        tag.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        tag.extend_from_slice(&(RECORD as u32).to_be_bytes());
        let mut strings = Vec::new();
        for (locale, text) in entries {
            let utf16: Vec<u8> = text
                .encode_utf16()
                .flat_map(|unit| unit.to_be_bytes())
                .collect();
            tag.extend_from_slice(locale);
            tag.extend_from_slice(&(utf16.len() as u32).to_be_bytes());
            let offset = HEADER + entries.len() * RECORD + strings.len();
            tag.extend_from_slice(&(offset as u32).to_be_bytes());
            strings.extend_from_slice(&utf16);
        }
        tag.extend_from_slice(&strings);
        tag
    }

    fn profile_with_desc_tag(tag: &[u8]) -> Vec<u8> {
        profile_with_tag(*b"desc", tag)
    }

    /// The smallest thing `describe_icc` will read: a 128-byte header, a one-entry tag table, and
    /// the tag itself.
    fn profile_with_tag(signature: [u8; 4], tag: &[u8]) -> Vec<u8> {
        let mut profile = vec![0u8; 128];
        profile.extend_from_slice(&1u32.to_be_bytes());
        profile.extend_from_slice(&signature);
        profile.extend_from_slice(&144u32.to_be_bytes());
        profile.extend_from_slice(&(tag.len() as u32).to_be_bytes());
        profile.extend_from_slice(tag);
        profile
    }
}

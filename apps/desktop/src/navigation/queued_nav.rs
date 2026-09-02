//! Navigation asked for before the folder scan landed.
//!
//! Opening an image on a slow share shows the picture against a provisional list of one (see
//! `navigation::State::scan_pending`), so there's nowhere to move yet. Rather than dropping the
//! arrow key, the app records it here and applies it the moment the real folder arrives —
//! `App::install_scanned_folder` resolves the queued move and navigates through the normal path.
//!
//! Pure: no I/O, no app state, so every accumulation and edge rule below is unit-tested.

/// Where a queued move counts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAnchor {
    /// The image on screen. Arrow keys, the wheel, Next/Previous, and a slideshow advance all
    /// count from here.
    Current,
    /// The first image in the folder (Home, Navigate → Go to first).
    First,
    /// The last image in the folder (End, Navigate → Go to last).
    Last,
}

impl NavAnchor {
    /// The name this anchor goes by in the QA `/state` snapshot.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NavAnchor::Current => "current",
            NavAnchor::First => "first",
            NavAnchor::Last => "last",
        }
    }
}

/// A move the user asked for while the folder was still being scanned: an anchor plus a signed
/// step count away from it. Steps accumulate, so left-left-right during a long scan lands one
/// image back, and left-right lands nowhere at all ([`is_noop`](Self::is_noop)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedNav {
    pub anchor: NavAnchor,
    pub delta: i32,
}

impl QueuedNav {
    /// A relative move from the image on screen.
    #[must_use]
    pub fn step(delta: i32) -> Self {
        Self {
            anchor: NavAnchor::Current,
            delta,
        }
    }

    /// An absolute jump. Home / End replace whatever was queued, the same way pressing them on a
    /// scanned folder overrides where the arrows had walked to.
    #[must_use]
    pub fn jump(anchor: NavAnchor) -> Self {
        Self { anchor, delta: 0 }
    }

    /// Fold another relative step in. Saturating, so a wedged key can't overflow the count.
    #[must_use]
    pub fn add(self, delta: i32) -> Self {
        Self {
            anchor: self.anchor,
            delta: self.delta.saturating_add(delta),
        }
    }

    /// Nothing left to apply: still counting from the image on screen, with no net movement. The
    /// app drops a queued move that reaches this state, so the scan lands with no jump at all.
    #[must_use]
    pub fn is_noop(self) -> bool {
        matches!(self.anchor, NavAnchor::Current) && self.delta == 0
    }

    /// The index this lands on in a folder of `total` images currently showing `current`.
    ///
    /// Past the ends it wraps when loop navigation is on and clamps to the folder edges when it's
    /// off, which is exactly what the same key press does on a folder that's already scanned.
    /// `None` for an empty folder — there's nowhere to land.
    #[must_use]
    pub fn resolve(self, current: usize, total: usize, loop_on: bool) -> Option<usize> {
        if total == 0 {
            return None;
        }
        let len = total as i64;
        let anchor = match self.anchor {
            NavAnchor::Current => (current as i64).clamp(0, len - 1),
            NavAnchor::First => 0,
            NavAnchor::Last => len - 1,
        };
        let target = anchor.saturating_add(i64::from(self.delta));
        let target = if loop_on {
            target.rem_euclid(len)
        } else {
            target.clamp(0, len - 1)
        };
        Some(target as usize)
    }

    /// Which way to warm neighbors after the jump lands: forward, backward, or (for an anchor jump
    /// with no steps on it) neither, matching how Home / End preload both sides equally.
    #[must_use]
    pub fn direction_hint(self) -> Option<bool> {
        match self.delta {
            0 => None,
            d => Some(d > 0),
        }
    }
}

/// Fold a relative step into whatever move is already queued, starting one from the image on
/// screen when nothing is queued yet. Returns `None` when the steps net back to where the user
/// already is, so a left-then-right during a long scan leaves nothing to apply.
#[must_use]
pub fn with_step(queued: Option<QueuedNav>, delta: i32) -> Option<QueuedNav> {
    let folded = queued.unwrap_or_else(|| QueuedNav::step(0)).add(delta);
    (!folded.is_noop()).then_some(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_and_its_opposite_net_to_nothing() {
        // Left then right during a long scan: the scan lands and nothing moves.
        assert!(QueuedNav::step(-1).add(1).is_noop());
        assert!(!QueuedNav::step(-1).is_noop());
        assert!(!QueuedNav::step(-1).add(-1).is_noop());
    }

    #[test]
    fn folding_steps_drops_a_move_that_nets_to_nothing() {
        // Nothing queued yet: the first press starts a move from the image on screen.
        let one_left = with_step(None, -1);
        assert_eq!(one_left, Some(QueuedNav::step(-1)));
        // The opposite press cancels it outright, so the scan lands with nothing to apply.
        assert_eq!(with_step(one_left, 1), None);
        // A second left keeps walking back.
        assert_eq!(with_step(one_left, -1), Some(QueuedNav::step(-2)));
        // An anchor jump survives a pair that cancels out.
        let home = Some(QueuedNav::jump(NavAnchor::First));
        assert_eq!(with_step(with_step(home, -1), 1), home);
    }

    #[test]
    fn steps_accumulate() {
        assert_eq!(QueuedNav::step(1).add(1).add(-3).delta, -1);
        assert_eq!(
            QueuedNav::step(i32::MAX).add(10).delta,
            i32::MAX,
            "a wedged key saturates instead of overflowing"
        );
    }

    #[test]
    fn a_relative_move_counts_from_the_image_on_screen() {
        assert_eq!(QueuedNav::step(3).resolve(10, 100, false), Some(13));
        assert_eq!(QueuedNav::step(-4).resolve(10, 100, false), Some(6));
    }

    #[test]
    fn it_clamps_to_the_folder_edges_when_loop_is_off() {
        // The decision for this feature: past the end you stop at the end, same as pressing the
        // key on a folder that's already listed.
        assert_eq!(QueuedNav::step(50).resolve(0, 10, false), Some(9));
        assert_eq!(QueuedNav::step(-50).resolve(3, 10, false), Some(0));
    }

    #[test]
    fn it_wraps_past_the_edges_when_loop_is_on() {
        assert_eq!(QueuedNav::step(1).resolve(9, 10, true), Some(0));
        assert_eq!(QueuedNav::step(-1).resolve(0, 10, true), Some(9));
        assert_eq!(
            QueuedNav::step(23).resolve(0, 10, true),
            Some(3),
            "several laps around still land on the right image"
        );
        assert_eq!(QueuedNav::step(-23).resolve(0, 10, true), Some(7));
    }

    #[test]
    fn home_and_end_anchor_the_move() {
        assert_eq!(
            QueuedNav::jump(NavAnchor::First).resolve(5, 10, false),
            Some(0)
        );
        assert_eq!(
            QueuedNav::jump(NavAnchor::Last).resolve(5, 10, false),
            Some(9)
        );
        // End then three rights clamps at the last image; with loop on it wraps around.
        let end_then_rights = QueuedNav::jump(NavAnchor::Last).add(1).add(1).add(1);
        assert_eq!(end_then_rights.resolve(5, 10, false), Some(9));
        assert_eq!(end_then_rights.resolve(5, 10, true), Some(2));
        // Home then one left does the same at the other edge.
        let home_then_left = QueuedNav::jump(NavAnchor::First).add(-1);
        assert_eq!(home_then_left.resolve(5, 10, false), Some(0));
        assert_eq!(home_then_left.resolve(5, 10, true), Some(9));
    }

    #[test]
    fn an_anchor_survives_steps_that_net_to_zero() {
        // Home, then left and right: the jump to the first image still stands.
        let queued = QueuedNav::jump(NavAnchor::First).add(-1).add(1);
        assert!(!queued.is_noop());
        assert_eq!(queued.resolve(5, 10, false), Some(0));
    }

    #[test]
    fn an_empty_folder_has_nowhere_to_land() {
        assert_eq!(QueuedNav::step(1).resolve(0, 0, false), None);
        assert_eq!(QueuedNav::jump(NavAnchor::Last).resolve(0, 0, true), None);
    }

    #[test]
    fn the_direction_hint_follows_the_steps_not_the_anchor() {
        assert_eq!(QueuedNav::step(2).direction_hint(), Some(true));
        assert_eq!(QueuedNav::step(-2).direction_hint(), Some(false));
        assert_eq!(
            QueuedNav::jump(NavAnchor::Last).direction_hint(),
            None,
            "a bare Home / End is non-directional, so both sides warm equally"
        );
    }
}

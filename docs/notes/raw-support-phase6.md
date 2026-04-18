# RAW support — Phase 6

Phase 6 opens the RAW pipeline's tuning knobs to the user through the
Settings → RAW → Tuning section. The goal: let users nudge the parametric
stages (sharpening, saturation, tone curve shape) without editing JSON by
hand or rebuilding the binary.

## Phase 6.0 — user-facing tuning sliders (shipped 2026-04-17)

### What shipped

Three NSSlider widgets live under a new "Tuning" section in Settings →
RAW, wedged between the existing "Output" toggle and the "DCP profile"
row. Each drives one `f32` field on `RawPipelineFlags`:

| Slider              | Field                  | Range         | Step | Default            | Drives                                           |
| ------------------- | ---------------------- | ------------- | ---- | ------------------ | ------------------------------------------------ |
| Sharpening amount   | `sharpen_amount`       | 0.00 – 1.00   | 0.05 | `DEFAULT_AMOUNT` (0.30) | `color::sharpen::sharpen_rgba{8,16f}_inplace_with` |
| Saturation boost    | `saturation_boost_amount` | 0.00 – 0.30 | 0.01 | `DEFAULT_SATURATION_BOOST` (0.08) | `color::saturation::apply_saturation_boost` |
| Tone midtone anchor | `midtone_anchor`       | 0.20 – 0.50   | 0.01 | `DEFAULT_MIDTONE_ANCHOR` (0.40) | `color::tone_curve::apply_tone_curve`            |

Defaults match the constants used before Phase 6.0, so a user who leaves
the sliders alone sees bit-identical output. `RawPipelineFlags::clamp_knobs`
runs once per decode inside `raw.rs` and pulls hand-edited out-of-range
values back into range without rejecting the whole settings file.

### Why these three

I looked at every parametric knob the RAW pipeline touches today and
picked the three with the highest taste-to-risk ratio. Everything else
stays internal.

- **Sharpening amount, not σ.** Phase 2.4's Laplacian measurement showed
  amount dominates perceived crispness; σ (the Gaussian blur radius)
  trades halos for softness but the window between "blurry" and "haloed"
  is narrow. Exposing σ as a second slider would invite users into the
  bad-settings range where it's easy to make the image look worse. The
  production σ (`DEFAULT_SIGMA = 0.8 px`) stays fixed.
- **Saturation boost, not ProcessingStyle / DCP injection.** Users who
  want warm / cool / skin-tone shifts reach for a DCP profile or an
  editor, not a viewer. The one global-chroma knob handles "too muted"
  and "too much pop" preferences, which is what Preview.app / Photos
  users actually want to tune.
- **Midtone anchor, not filmic peak.** Peak is a display decision
  (`DEFAULT_PEAK_SDR = 1.0`, `DEFAULT_PEAK_HDR = 4.0` per Phase 5) —
  users don't have a taste opinion on highlight asymptote height.
  Anchor is a straightforward "brighter midtones vs. darker midtones"
  knob that everyone understands.

### UI decisions

- **Discrete commits, live label updates deferred.** `setContinuous(false)`
  means AppKit fires the slider action exactly once on mouse release. A
  single drag = a single decode. We considered live-updating the numeric
  label during drag (via `currentEvent.type` inspection or a separate
  tracking delegate) but shipped the simpler version — value clarity on
  release is enough, and avoiding decode-spam matters more on 20 MP RAWs
  where each decode costs tens of milliseconds.
- **Numeric label to 2 decimals.** All three ranges are narrow enough
  (0.00 – 1.00 at worst) that 2 decimals convey the full usable
  resolution without noise. The saturation range tops out at 0.30 so
  "0.08" and "0.12" read cleanly.
- **Slider minimum width = 160 px.** Without an explicit minimum the
  slider track collapses when the panel is narrow and long row titles
  "win" the stack-view space negotiation.
- **Ivar pointers + raw struct init.** Same pattern as the existing
  `RawDelegateIvars` — pointers into `retained_views` survive for the
  window's lifetime, so the delegate can read slider state without
  fighting Rust borrow rules across AppKit dispatch. Three slider
  pointers + three value-label pointers were added alongside the
  existing toggle pointers.
- **Reset to defaults covers sliders too.** The original
  `write_flags_to_switches` was renamed to `write_flags_to_all_widgets`
  and now covers sliders + value labels. A reset click snaps the full
  UI back in one step, matching the existing toggle behavior.

### Persistence

Each of the three floats carries a `#[serde(default = "...")]` pointing
at the corresponding constant, so older settings.json files (missing
the new keys) load silently without losing the user's other prefs. Two
round-trip tests pin this down: one at the `RawPipelineFlags` level
(`round_trip_preserves_values`, `round_trip_preserves_float_precision`
in `raw_flags.rs`), one at the outer `Settings` level
(`round_trip_preserves_raw_tuning_knobs` in `persistence.rs`).

### What's next

Phase 6.0 is feature-complete. Further knob exposure (per-image DCP
override, per-lens LensFun override, custom curves) would be Phase 6.1+
and isn't planned yet. A user requesting more control than the three
current sliders can always edit `settings.json` directly — the clamp
protects against out-of-range values either way.

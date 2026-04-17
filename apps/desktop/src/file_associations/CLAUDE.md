# File associations

macOS default-handler registration for each supported image UTI. Used by:

- **Onboarding** — "Set as default viewer" button
- **Settings → File associations panel** — two sections, each with a master toggle + per-UTI toggles

## UTI source of truth

`SUPPORTED_UTIS` is the combined list (standard + RAW) and the single place to edit. The
Settings panel slices it through `SUPPORTED_STANDARD_UTIS` (first six) and
`SUPPORTED_RAW_UTIS` (last 10). Keep in sync with:

- `CFBundleDocumentTypes` in `apps/desktop/Info.plist` (what Finder sees)
- `decoding::dispatch` extension whitelist (what the decoder accepts)

Adding a format means touching all three.

## Settings panel layout

Two sections, each built the same way:

- Section header label
- Master row: title (bold) + status secondary + optional "Mixed" pill + large `NSSwitch`
- One compact row per UTI: label + detail + small `NSSwitch`

`NSSwitch` has no native mixed/indeterminate state. When a section is partially enabled
we signal it by:

1. Rendering the master switch as Off with `alphaValue = 0.55` (dimmed)
2. Showing a "Mixed" pill label beside it

Click behavior on a master switch follows macOS Finder's "Select all" convention:

- `None` → all on
- `Mixed` → all on (promotes, rather than collapsing to off — avoids accidental
  widespread disables)
- `All` → all off

A 1-second `NSTimer` polls via `is_prvw_default` because the OS doesn't notify us when
handlers change elsewhere (another viewer, Get Info → "Open With…" → Change All).

## Approach

Direct CoreServices FFI via `objc2-core-services`:
- `LSCopyDefaultRoleHandlerForContentType` — query current handler
- `LSSetDefaultRoleHandlerForContentType` — set Prvw or restore

No Swift toolchain dependency, near-instant, deprecated but stable.

## Restore behavior

When the user turns a toggle OFF, we restore the **handler that was there before Prvw
took over**, tracked in `Settings.previous_handlers` (map of UTI → bundle ID). If we
never recorded a previous (upgrade from older version without this tracking, or the
UTI was installed after Prvw), falls back to `com.apple.Preview`.

## Onboarding coupling

`set_as_default_viewer()` claims every entry in `SUPPORTED_UTIS` — onboarding calls it
when the user clicks "Set as default viewer". Extending `SUPPORTED_UTIS` widens
onboarding's scope. That's intentional while RAW support is new; the onboarding flow
will get a dedicated revamp later.

## Gotchas

- **OSStatus != 0 is non-fatal.** Logged as a warning. The OS occasionally rejects
  handler changes during sign-in / login-item transitions.
- **Polling timer** in the settings panel checks every 1 second because the OS doesn't
  notify us when handlers change elsewhere.
- **`NSSwitch` ignores mixed state.** Setting `NSControlStateValueMixed` (= -1) is
  accepted by the API but renders as Off. That's why we also dim the switch and show
  a "Mixed" pill for the tri-state visual.

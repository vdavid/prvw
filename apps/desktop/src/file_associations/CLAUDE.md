# File associations

macOS default-handler registration for each supported image UTI. Used by:

- **Onboarding** — "Set as default viewer" button
- **Settings → File associations panel** — per-UTI toggles + "Set all"

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

## Gotchas

- **OSStatus != 0 is non-fatal.** Logged as a warning. The OS occasionally rejects
  handler changes during sign-in / login-item transitions.
- **Polling timer** in the settings panel checks every 1 second because the OS doesn't
  notify us when handlers change elsewhere.

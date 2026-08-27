# Exif info overlay

A reading panel listing the current image's camera, lens, exposure, time, and GPS metadata. Anchored top-right, under
the histogram when that's on and in its place when it isn't. Toggled via View → Exif info or the bare `E` key.

| File                 | Purpose                                                                                             |
| -------------------- | --------------------------------------------------------------------------------------------------- |
| `../exif_overlay.rs` | `exif_overlay::State { visible }`, seeded from `Settings::exif_visible`                             |
| `overlay.rs`         | Visual layout: backdrop pill, "Exif info" title, one label+value row per field, or the no-data line |

The metadata itself is parsed in `crate::decoding::exif_metadata` and travels on `DecodedImage::exif`. Panel width,
radius, margin, and backdrop color come from `render::overlay_style`, shared with the histogram so the two stack as one
column.

## Decision: an image with no Exif still gets a panel

**Decision:** `overlay::build` takes `Option<&ExifMetadata>`. `None` (and an all-default `ExifMetadata`, which
`parse_exif_metadata` never actually returns) draws the panel with one line, `NO_DATA_TEXT`, instead of the rows. The
panel stays away only while we don't yet know: `App::current_exif_state` returns the outer `None` when nothing is
displayed or the decode hasn't landed, so a preview placeholder never gets captioned "no Exif data".

**Why:** Drawing nothing left the user unable to tell a file without metadata from a feature that had stopped working,
and that ambiguity cost a real debugging session (see the gotcha below). The panel is toggled deliberately; answering
the question it was toggled to ask is cheaper than a wall of "n/a" rows and more honest than silence.

## Gotcha: "the histogram works but Exif doesn't" is not evidence of an Exif bug

**Why:** The two panels look like siblings and are toggled the same way, so when one appears and the other doesn't, the
difference reads as a fault in the one that's missing. It usually isn't. The histogram is computed from pixels, which
every image has; the Exif panel needs metadata, which plenty of images don't carry (PNG, GIF, BMP, plain WebP, a
screenshot, a re-encoded JPEG, anything an export pipeline stripped). Same toggle, different preconditions.

Before hunting for a platform fence, check which case you're in:

- The QA server's `/state` reports `exif_present` next to `exif_visible`.
- `AppCommand::ToggleExifInfo` logs both at info level, so a log from a machine you can't drive answers it too. On
  Windows a launch from Explorer writes that log to `%APPDATA%\Prvw\prvw.log`.
- Since the no-data panel landed, the window itself answers: a panel saying "no Exif data" means the file has none, and
  no panel at all with the menu item checked means something really is wrong.

The path from file bytes to the panel carries no `#[cfg]` at all (`decoding/CLAUDE.md` has the anchors that keep it that
way), so "it only fails on one platform" needs stronger evidence than one image.

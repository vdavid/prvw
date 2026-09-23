# Tags

Finder color tags on the image being viewed. Bare `1`–`7` (and the Tags menu) toggle red, orange, yellow, green, blue,
purple, and gray, in Finder's own order; dots in the window's bottom-left corner show what the file carries. The point
is triage: tag while viewing, then find the tagged files in Finder's sidebar and act on them there. macOS only.

- `mod.rs`: `TagColor` (digits, Finder's names), `Tag`, `has_color`, the `read_tags` / `toggle_color` seam (no-ops off
  macOS), and `State`, the per-path cache.
- `finder.rs` (macOS): the `com.apple.metadata:_kMDItemUserTags` attribute. Read, parse, encode, write, toggle. Ported
  from Cmdr's `file_system/tags.rs`, fixtures included.
- `overlay.rs`: the dots, as pure overlay-pill geometry.
- `app/tags_hook.rs`: the `App` glue (`tag_target`, `toggle_tag`, `refresh_tags`, `current_tags`).

## How it flows

- An image paints → `finalize_display` → `load_current_tags` reads its tags once (`State::ensure`, one `getxattr`).
- A digit or a Tags menu click → `AppCommand::ToggleTag(color)` → `App::toggle_tag` → `tags::toggle_color` (read,
  change, write) → `App::refresh_tags` (re-read, redraw, `update_shared_state`, which also re-ticks the Tags menu).
- **`App::refresh_tags(path)` is the one call for "this file's tags may have changed"**, whoever changed them, and
  everything that shows tags follows. `note_tags_changed` is the same without publishing `/state`, for a caller handling
  a batch (the folder watcher) that publishes once at the end.

## Decision: the disk is the source of truth

**Why:** a write can fail (read-only share, permissions), and a display that shows the tag we meant to write would lie
about what Finder will show. So every toggle ends in a re-read, and a failed write is logged at `warn` and otherwise
changes nothing.

## Decision: toggling reads strictly and refuses to overwrite what it can't read

**Why:** the write replaces the whole tag list, so reading a failed or undecodable attribute as "no tags" would throw
the file's other tags away. `finder::read_tags` returns an error for both, and `toggle_color` stops on it. Only the
display path (`State::refresh`) treats an error as no tags, because drawing nothing is safe. This is stricter than Cmdr,
which reads lossily on both paths.

## Decision: Finder's toggle rule, and only this one attribute

- A file "has" a color when any tag carries it, custom ones included, so toggling red on a file tagged "Important" (red)
  removes that tag rather than adding a second red. Adding appends Finder's built-in tag under Finder's own name, so it
  joins Finder's sidebar entry for the color.
- An empty list removes the attribute, the way Finder does, so no empty husk lingers.
- The attribute is a **binary** plist (`plist` writes XML by default, which Finder ignores).
- Only `_kMDItemUserTags` is touched. `com.apple.FinderInfo` carries a custom icon's flag, and Finder reads tags without
  it. `tagging_leaves_finder_info_alone` is the guard.
- No local-volume gating. An SMB share that supports extended attributes works like a local disk; one that doesn't fails
  the write, which the re-read then shows. A toggle costs three attribute calls on the main thread, which is fine for a
  keypress even over the network.

## Decision: the dots

- **Placement:** bottom-left, 10 px from both edges, which nothing else draws in (the title strip and zoom pill are at
  the top, the histogram and EXIF panels top-right, loading and the empty state in the center). In fullscreen the window
  is the screen, so it's the same code.
- **Shape:** 10 px dots, Finder's list-view size, each next one 6 px along so they overlap, leftmost on top (Cmdr's
  `TagDots` does the same). One dot per color, in the file's order; colorless tags draw nothing.
- **Contrast:** each dot sits on a 1.5 px halo of 45% black, drawn first. It reads as a thin outline on a bright photo,
  disappears on a dark one, and separates overlapping dots the way Finder's ring does.
- **Colors:** Cmdr's dark-appearance palette (`--color-tag-*` in its `app.css`), whatever the system appearance, because
  the dots sit on a photo rather than on a light or dark window, and the brighter set holds up over both. They are
  converted from sRGB to linear light, since the surface is an sRGB format and the pill shader writes linear.
- Up to 14 pills per frame, which is why the renderer's pill pool is 48 (`render/CLAUDE.md`).

## Tags set outside Prvw

A tag Finder sets (or anything else that writes the attribute) arrives through live folder sync: `folder_watch` reports
the file as modified, its size and mtime haven't moved, so `App::handle_folder_changed` calls it a metadata-only change
(`crate::file_stamp`) and calls `note_tags_changed` instead of evicting anything. Our own toggle comes back the same
way, a moment after its own re-read, and changes nothing. Neither decodes the image again. `note_tags_changed` re-reads
the image on screen right away and forgets every other file's cached tags, so they're read when that file next comes on
screen rather than on the main thread for a file nobody's looking at.

This only reaches files in the watched folder, which is the one on screen. It also assumes the attribute write leaves
mtime alone. That holds on APFS (`file_stamp`'s `an_extended_attribute_write_keeps_the_stamp` pins it); an SMB server
hasn't been checked, and one that does bump mtime costs a re-decode per tag, which is what every tag cost before.

## Gotchas

- **Browse mode has no tag target.** `tag_target` is `None` there, the Tags menu greys out, and `/state` reports
  `tags: null`. The digits belong to the focused pane.

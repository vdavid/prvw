# Prvw: native title/zoom overlay (and a sketch for a future sidebar)

Goal: the image title and zoom readout in the title-bar strip must stay readable (WCAG AA+ contrast) in **both** light
and dark macOS appearances, with no backdrop pill. Today they're GPU-rendered white text that assumes a dark strip, so
they're unreadable in Light mode.

This spec ships the title/zoom fix natively and, while we're learning the "AppKit views coexist with the wgpu surface"
plumbing, sketches the future file-tree sidebar that reuses the same boundary.

## Root cause

The title-bar strip is an `NSVisualEffectView` (material `Titlebar`, `add_titlebar_vibrancy` in `window.rs`) that
follows the system appearance: dark glass in Dark mode, light glass in Light mode. The overlay text is built in
`App::build_text_overlay` (`app.rs`) as glyphon `TextBlock`s with the hardcoded white default (`TextBlock::new` →
`[255,255,255,240]`) and no pill when `title_bar` is on. White-on-dark reads; white-on-light does not. The current code
even documents the wrong assumption: "the glass title-bar strip … gives it enough contrast on its own".

A GPU-side fix (appearance-adaptive color, or a drop shadow) is possible but second-best: a 1px shadow doesn't clear AA+
on flat light glass, and adapting color by hand re-implements what AppKit already does for free.

## The mental model: one window, two ways to mix GPU and AppKit

A wgpu surface is a `CAMetalLayer` living in the window's normal view/layer tree, composited by AppKit alongside every
other view. There are exactly two spatial relationships, chosen per element:

- **Layered (overlay):** GPU region and AppKit view share a rectangle, stacked in Z. Used here for the title/zoom HUD.
- **Tiled (split):** disjoint rectangles, side by side, no Z relationship. Used by the future sidebar.

Prvw already runs a layered setup: the image-quad is drawn into a wgpu viewport **clipped below the title-bar strip**,
so the strip region of the Metal layer stays transparent, and `push_metal_layer_above_vibrancy` puts the Metal layer at
`zPosition = 1.0` in front of the full-window vibrancy (so the image composites over it). Compositing here is governed
by sibling-layer `zPosition` under the contentView's root layer. **A transparent Metal pixel still occludes normal
in-window content behind it — only the server-composited `BehindWindow` vibrancy blur bleeds through.** So a HUD AppKit
view isn't made visible by sitting _behind_ the transparent strip; its layer must sit _in front of_ the Metal layer
(higher `zPosition`). That is the lever this change pulls.

## This change: native title/zoom text

Render the title and zoom as `NSTextField` labels added to the contentView (siblings of the wgpu Metal layer), each
label's layer `zPosition` raised above the Metal layer (`TITLEBAR_LABEL_Z_POSITION = 2.0` &gt; the Metal layer's `1.0`)
so they composite in front of the transparent strip region. They can't be subviews of the vibrancy strip: the strip is
behind the Metal layer, and a transparent Metal pixel occludes in-window content behind it (lifting the strip view's own
`zPosition` does not lift its subviews above the sibling Metal layer — verified). The `BehindWindow` blur stays behind
the window regardless, so the strip still reads as glass.

Why this is the right fix, not just a workaround:

- **`labelColor` / `secondaryLabelColor` are vibrancy- and appearance-aware semantic colors.** They auto-contrast in
  both appearances and update on a live theme switch with zero observer code. The original bug is solved by
  construction.
- Side wins: native middle-truncation (`lineBreakMode = .byTruncatingMiddle`) replaces the hand-rolled
  `max_render_width` ellipsis for these labels; VoiceOver can read the title; truly native vibrant text blending.
- It builds the AppKit-coexists-with-wgpu plumbing (lifetimes, positioning, live state sync, hit-testing) on a tiny,
  low-risk element before the sidebar needs it.

### Design (hooks into the existing `window.rs` vibrancy pattern)

`window.rs` already owns the strip: it creates the vibrancy view with an `identifier`, toggles it by id
(`set_subview_hidden_by_id`), and lets the view hierarchy own the retains (no `Retained<>` stored in `App`). The labels
follow the same pattern exactly.

1. **Create the labels in `configure_macos_window` on BOTH paths.** Add two non-editable, non-selectable, non-bordered,
   transparent-background `NSTextField` labels **as contentView subviews** (siblings of the Metal layer), layer-back
   each and raise its `zPosition` above the Metal layer's `1.0`. Crucially, this runs for both the Liquid Glass and
   legacy window paths — `add_titlebar_vibrancy` (the legacy strip) is gated behind `!liquid_glass_available()`, so
   building the labels there would silently skip them on macOS 26+. `set_titlebar_vibrancy_visible` toggles the labels'
   `hidden` in lockstep with the strip (title-bar off, fullscreen):
   - Title label: `identifier = "prvw.titlebar_title"`, `textColor = labelColor`, bold system font (13.5pt),
     `lineBreakMode = .byTruncatingMiddle`. Auto Layout (pinned to the contentView): leading
     `= contentView.leading + 88` (right of the traffic lights), `centerY = contentView.top + 17` (the strip's vertical
     middle is 16; +1 nudges it to align with the traffic lights).
   - Zoom label: `identifier = "prvw.titlebar_zoom"`, `textColor = secondaryLabelColor` (the readout is secondary info),
     bold system font. Auto Layout: trailing `= contentView.trailing - 12`, `centerY = contentView.top + 17`, and
     `leading >= title.trailing + 12` (the gap), with the title's horizontal compression resistance lower than the
     zoom's so the **title** truncates, never the zoom.
   - Hierarchy retains the labels; no `Retained<>` in `App`. Look them up by identifier to update.

2. **Update path: `window::set_titlebar_text(window, title: &str, zoom: &str)`.** Finds both labels by identifier (same
   lookup as `set_subview_hidden_by_id`) and sets `stringValue`. Cache-guard: skip the set when the value is unchanged
   to avoid a needless relayout (store last-set strings, or compare against current `stringValue`).

3. **Wire it from the render path.** In the `RedrawRequested` arm of `app.rs` (where `build_text_overlay` is called),
   when `self.title_bar` is true and not fullscreen, call `window::set_titlebar_text` with the same `title` /
   `zoom_text` strings `build_text_overlay` computes today. Refactor the title/zoom string construction out of
   `build_text_overlay` into a small helper both paths can use (or compute in the render arm and pass down), so the
   strings have one source.

4. **Don't double-draw.** `build_text_overlay` must build the glyphon title/zoom blocks **only when `title_bar` is off**
   (the float-over-image case, where the dark pill is still correct). When `title_bar` is on, the native labels own the
   title/zoom; glyphon builds neither. The centered "Loading…" overlay stays glyphon in both cases.

5. **Hide with the strip.** Because the labels are contentView subviews (not strip children), they don't hide
   automatically. `set_titlebar_vibrancy_visible` toggles their `hidden` flag alongside the strip's, so the
   title-bar-off and fullscreen paths (which already call it) hide the labels too.

### Gotchas to defend against

- **Hit-testing / window drag.** A label in the strip must not swallow title-bar drags or clicks routed to the native
  window zoom (`pointer_in_title_bar` double-click). Make the labels non-interactive (`setEditable:false`,
  `setSelectable:false`) and, if drag is still blocked, override `hitTest:` to return nil so events pass through to the
  strip/window.
- **`Retained<>` lifetime.** As with the existing vibrancy view, after `addSubview` the hierarchy owns the retain; local
  `Retained` handles can drop at end of scope. Do **not** store them in a modal-style Vec — these live for the window's
  life via the hierarchy. (The modal-session Vec rule is about hand-run `runModalForWindow`, not hierarchy subviews.)
- **Flipped coordinates.** winit's contentView is flipped (Y=0 at top). The labels use Auto Layout (centerY, leading,
  trailing) like `add_titlebar_vibrancy` does, sidestepping manual flipped-Y math.
- **Screenshots.** GPU `capture_screenshot` readback won't include the AppKit labels (see `render/CLAUDE.md`). QA pixel
  tests of the title must use the `screenshot_window` MCP tool (`screencapture -l`), which captures the composited
  window.

## Test plan

- Unit: the title/zoom string builder (folder-position prefix, middle-truncation input) stays pure and testable; keep
  its existing coverage.
- Manual: launch in Dark mode and Light mode (System Settings → Appearance), confirm AA+ readability of both labels in
  each; toggle appearance live and confirm the text recolors without a redraw nudge; toggle the title bar off (labels
  vanish, glyphon pills return over the image); enter fullscreen (labels vanish with the strip); navigate a folder
  (position prefix updates) and zoom (readout updates) and confirm both labels track; open a very long filename and
  confirm the **title** middle-truncates while the zoom stays intact.
- `./scripts/check.sh` (all checks) green before commit.

## Future (not in this change): file-tree sidebar via the tiled model

A file tree is `NSOutlineView` in an `NSScrollView` — native disclosure, selection, keyboard nav, scrolling,
accessibility. It must **not** go through glyphon. Use the **tiled** model, not an overlay: a persistent structural pane
shouldn't float over the image, and we shouldn't keep allocating full-window GPU pixels behind it.

Two ways to bound the GPU to the image pane:

- **Cheap:** keep the full-window Metal layer, inset the wgpu viewport from the left too (same technique already used
  for the top strip), and put the `NSOutlineView` in front over the now-transparent left region. Reuses today's viewport
  approach; downsides are a full-window surface you don't fully draw, plus manual front-view Z-order and hit-testing.
- **Ideal (target):** an `NSSplitView` with the tree on the left and a dedicated **render-pane `NSView` hosting the
  `CAMetalLayer`** on the right. The wgpu surface is sized to the image pane; AppKit owns the divider, drag-to-resize,
  focus, and hit-testing — exactly how Finder/Preview are built. The one real engineering task: today winit owns the
  contentView and hardwires its `CAMetalLayer` to the full view, so the clean path is to stop letting winit's
  full-window layer be the surface and instead own a render sub-view whose layer wgpu targets, letting `NSSplitView`
  drive its frame. Once that boundary exists, pane resizing through the splitter is just the existing `renderer.resize`
  path firing with new dimensions.

The native-title change above is deliberately the smallest instance of the same AppKit/wgpu coexistence the sidebar
needs, so it de-risks that later work.

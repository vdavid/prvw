//! Text rendering via glyphon. Wraps font system, atlas, and renderer into a single API
//! that the main renderer can call to draw text overlays (header bar).

use crate::pixels::Logical;
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, fontdb,
};
use std::sync::{Mutex, OnceLock};

/// A block of text to render at a specific position.
/// All coordinates and sizes are in **logical points** (not physical pixels).
/// The text renderer scales them by the display scale factor automatically.
pub struct TextBlock {
    pub text: String,
    pub x: Logical<f32>,
    pub y: Logical<f32>,
    pub font_size: f32,
    pub line_height: f32,
    pub color: [u8; 4], // RGBA
    pub max_width: Option<Logical<f32>>,
    pub bold: bool,
    /// Drop shadow: renders the text twice — dark shadow offset by 1px, then the main color on top.
    /// This guarantees readability on any background without a backdrop blur or pill.
    pub shadow: bool,
    /// Maximum rendered width in logical pixels. If text exceeds this, truncate with
    /// middle ellipsis: "long_filen…photo.jpg". None = no truncation.
    pub max_render_width: Option<Logical<f32>>,
    /// If set, draw a semi-transparent pill (rounded rect) behind the text.
    pub pill: Option<PillStyle>,
    /// If set, `x` is the RIGHT edge of the pill (text + padding), and the block is
    /// repositioned leftward after measuring the actual text width.
    pub align_right: bool,
    /// If set, `x` is the CENTER of the text, and the block is repositioned
    /// leftward by half the measured text width. Useful for centered
    /// status messages like "Loading...".
    pub align_center: bool,
}

pub struct PillStyle {
    pub color: [f32; 4],             // RGBA, each 0..1
    pub padding_x: Logical<f32>,     // horizontal padding in logical pts
    pub padding_y: Logical<f32>,     // vertical padding in logical pts
    pub corner_radius: Logical<f32>, // in logical pts
}

/// A measured pill rect, computed from actual text width after shaping.
pub struct MeasuredPill {
    pub x: Logical<f32>,      // logical pts
    pub y: Logical<f32>,      // logical pts
    pub width: Logical<f32>,  // logical pts
    pub height: Logical<f32>, // logical pts
    pub color: [f32; 4],
    pub corner_radius: Logical<f32>,
}

/// A standalone rounded-rectangle pill drawn through the same overlay
/// pipeline as `MeasuredPill`, but specified directly without text
/// measurement. Used for backdrops behind non-text overlays (the histogram
/// panel, axis tick marks, legend swatches).
#[derive(Clone, Copy, Debug)]
pub struct StandalonePill {
    pub x: Logical<f32>,
    pub y: Logical<f32>,
    pub width: Logical<f32>,
    pub height: Logical<f32>,
    pub corner_radius: Logical<f32>,
    pub color: [f32; 4],
}

impl TextBlock {
    /// Create a text block with sensible defaults. Font: 13.5pt, white, not bold, no shadow/pill/truncation.
    pub fn new(text: impl Into<String>, x: Logical<f32>, y: Logical<f32>) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            font_size: 13.5,
            line_height: 18.5,
            color: [255, 255, 255, 240],
            max_width: None,
            bold: false,
            shadow: false,
            max_render_width: None,
            pill: None,
            align_right: false,
            align_center: false,
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn pill(
        mut self,
        color: [f32; 4],
        padding_x: Logical<f32>,
        padding_y: Logical<f32>,
        corner_radius: Logical<f32>,
    ) -> Self {
        self.pill = Some(PillStyle {
            color,
            padding_x,
            padding_y,
            corner_radius,
        });
        self
    }

    pub fn max_render_width(mut self, width: Logical<f32>) -> Self {
        self.max_render_width = Some(width);
        self
    }

    pub fn align_right(mut self) -> Self {
        self.align_right = true;
        self
    }

    pub fn align_center(mut self) -> Self {
        self.align_center = true;
        self
    }
}

/// Measure the rendered width of shaped text in logical points.
fn measure_text_width(buffer: &Buffer) -> f32 {
    buffer.layout_runs().fold(0.0f32, |max_w, run| {
        let run_w = run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0);
        max_w.max(run_w)
    })
}

/// The font every overlay string is shaped with: the platform's own UI font, so the title strip,
/// the zoom pill, the EXIF panel, and the histogram labels look like the rest of the desktop.
///
/// Resolved once, by [`build_font_system`], against the fonts that machine actually has. Callers
/// that somehow ask before then get the first candidate unverified, which is the best guess we
/// have and no worse than not asking.
fn ui_family() -> Family<'static> {
    Family::Name(
        UI_FAMILY
            .get()
            .map(String::as_str)
            .unwrap_or_else(|| ui_font_candidates()[0]),
    )
}

static UI_FAMILY: OnceLock<String> = OnceLock::new();

/// UI font names to try, best first.
///
/// macOS: "System Font" is the English family name fontdb reads out of `SFNS.ttf`; the localized
/// spellings are registered alongside it, so this matches whatever the locale is.
///
/// Windows: `lfMessageFont` is the honest answer, and it reports "Segoe UI" on both Windows 10 and
/// 11 (the Segoe UI Variable switch is a XAML-layer thing, not a system-font one). The rest are
/// there for a machine where that query fails.
///
/// Linux has no single answer, so this is the union of what the big desktops ship, most
/// distinctive first. cosmic-text's own default is "Open Sans", which stock installs rarely have.
fn ui_font_candidates() -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    let names = vec!["System Font", "Helvetica Neue", "Helvetica"];
    #[cfg(target_os = "windows")]
    let names = {
        let mut names = vec!["Segoe UI", "Tahoma", "Arial"];
        if let Some(preferred) = crate::platform::windows::system_ui_font_name() {
            names.insert(0, preferred);
        }
        names
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let names = vec![
        "Cantarell",
        "Ubuntu",
        "Noto Sans",
        "DejaVu Sans",
        "Liberation Sans",
        "FreeSans",
    ];
    names
}

/// The first candidate the database can actually match. `None` means none of them are installed.
fn resolve_ui_family(db: &fontdb::Database, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|name| {
            db.query(&fontdb::Query {
                families: &[Family::Name(name)],
                ..Default::default()
            })
            .is_some()
        })
        .map(|name| (*name).to_string())
}

/// Build a `FontSystem` and settle which family the overlay draws in.
///
/// Every `FontSystem` in the process comes from here, so measurement and rendering shape with the
/// same faces. They used to differ: only the renderer loaded the macOS bold alias below, so a
/// wrapped-line count could disagree with what got drawn.
fn build_font_system() -> FontSystem {
    #[allow(unused_mut)] // mut needed on macOS for load_font_source
    let mut font_system = FontSystem::new();

    // Load the macOS system font (SF Pro). SFNS.ttf is a variable font with a `wght`
    // axis, but fontdb registers it as a single weight-400 face. cosmic-text applies
    // the `wght` variation at render time, but fontdb's query won't SELECT the face
    // when asked for bold (weight 700) because it only sees weight 400.
    //
    // Fix: load it twice — fontdb deduplicates the data but creates two face entries.
    // We then find the second entry's ID and re-register it with weight=700 via
    // push_face_info, so fontdb will match it for bold queries.
    //
    // Windows needs no equivalent: Segoe UI ships separate static weights, so a bold query
    // finds a real bold face.
    #[cfg(target_os = "macos")]
    {
        use glyphon::fontdb::{FaceInfo, Source};
        let path = std::path::Path::new("/System/Library/Fonts/SFNS.ttf");
        if path.exists() {
            let data = std::fs::read(path).unwrap();
            let ids = font_system
                .db_mut()
                .load_font_source(Source::Binary(std::sync::Arc::new(data)));
            // For each registered face, add a bold alias pointing to the same source
            for id in ids {
                if let Some(face) = font_system.db().face(id) {
                    let bold_face = FaceInfo {
                        id: fontdb::ID::dummy(),
                        source: face.source.clone(),
                        index: face.index,
                        families: face.families.clone(),
                        post_script_name: face.post_script_name.clone(),
                        style: face.style,
                        weight: fontdb::Weight(700),
                        stretch: face.stretch,
                        monospaced: face.monospaced,
                    };
                    font_system.db_mut().push_face_info(bold_face);
                }
            }
        }
    }

    let candidates = ui_font_candidates();
    match resolve_ui_family(font_system.db(), &candidates) {
        Some(family) => {
            log::debug!("Overlay font: {family}");
            let _ = UI_FAMILY.set(family);
        }
        None => log::error!(
            "None of the overlay font candidates {candidates:?} are installed, so the overlay text \
             will be shaped with whichever face the font database happens to return first"
        ),
    }
    font_system
}

/// Shared `FontSystem` for measurement-only callers (overlay layout passes
/// that need the wrapped line count before the renderer runs). Initialized
/// lazily on first use, then reused for the process lifetime — `FontSystem`
/// scans system fonts on construction, which is too expensive to repeat.
fn measurement_font_system() -> &'static Mutex<FontSystem> {
    static FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    FONT_SYSTEM.get_or_init(|| Mutex::new(build_font_system()))
}

/// Count the visual lines `text` would occupy after shaping at `font_size`
/// and wrapping to `max_width` in logical points. Returns at least 1 even
/// for empty input. Used by overlay builders that need a wrap-aware panel
/// height before the renderer runs.
pub fn count_wrapped_lines(text: &str, font_size: f32, line_height: f32, max_width: f32) -> usize {
    let mut fs = measurement_font_system()
        .lock()
        .expect("font system poisoned");
    let metrics = Metrics::new(font_size, line_height);
    let mut buffer = Buffer::new(&mut fs, metrics);
    buffer.set_size(&mut fs, Some(max_width), None);
    let attrs = Attrs::new().family(ui_family());
    buffer.set_text(&mut fs, text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(&mut fs, false);
    buffer.layout_runs().count().max(1)
}

/// Owns all glyphon state and provides a simple `render_text` method.
pub struct GlyphonRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
}

impl GlyphonRenderer {
    /// Create a new text renderer. Call once during renderer init.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let font_system = build_font_system();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        Self {
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            viewport,
        }
    }

    /// Shape a buffer with the given text and return the display text (possibly truncated).
    fn shape_and_truncate(
        font_system: &mut FontSystem,
        buffer: &mut Buffer,
        text: &str,
        attrs: &Attrs,
        max_render_width: Option<f32>,
    ) -> String {
        // A render-width cap means the block is single-line and middle-truncated to fit.
        // Such a block must NEVER wrap, so lay it out unbounded (no wrap width) before
        // measuring. The caller sizes the buffer's wrap width to the distance to the screen
        // edge, which is wider than this cap (it reserves room for the zoom pill); leaving
        // that wrap width active makes glyphon break the title onto two lines whose longest
        // line still fits the cap, so truncation never triggers — a band of window widths
        // showed the title wrapped instead of ellipsized.
        if max_render_width.is_some() {
            buffer.set_size(font_system, None, None);
        }

        buffer.set_text(font_system, text, attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(font_system, false);

        let max_w = match max_render_width {
            Some(w) => w,
            None => return text.to_string(),
        };

        let width = measure_text_width(buffer);
        if width <= max_w {
            return text.to_string();
        }

        // Middle-truncation via binary search.
        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();
        if total <= 2 {
            return text.to_string();
        }

        // Binary search for the maximum number of chars we can keep (split ~50/50).
        let mut lo: usize = 1; // at minimum keep 1 char total (degenerate)
        let mut hi: usize = total;
        let mut best_text = "\u{2026}".to_string();

        while lo <= hi {
            let mid = (lo + hi) / 2;
            let prefix_len = mid.div_ceil(2);
            let suffix_len = mid - prefix_len;
            let candidate: String = chars[..prefix_len]
                .iter()
                .chain(std::iter::once(&'\u{2026}'))
                .chain(chars[total - suffix_len..].iter())
                .collect();

            buffer.set_text(font_system, &candidate, attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(font_system, false);
            let w = measure_text_width(buffer);

            if w <= max_w {
                best_text = candidate;
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
        }

        // Re-shape with the final truncated text so the buffer is ready for rendering.
        buffer.set_text(font_system, &best_text, attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(font_system, false);
        best_text
    }

    /// Prepare text for rendering. All `TextBlock` values are in logical points.
    /// The `scale_factor` (from `window.scale_factor()`) converts them to physical pixels.
    /// Returns measured pill rects for blocks that requested a pill background.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texts: &[TextBlock],
        screen_width: u32,
        screen_height: u32,
        scale_factor: f64,
    ) -> Vec<MeasuredPill> {
        let sf = scale_factor as f32;

        self.viewport.update(
            queue,
            Resolution {
                width: screen_width,
                height: screen_height,
            },
        );

        let mut measured_pills: Vec<MeasuredPill> = Vec::new();
        // Per-block x offset (for right-aligned blocks, shifted left by measured text width)
        let mut x_offsets: Vec<Logical<f32>> = Vec::with_capacity(texts.len());

        // Build a glyphon Buffer for each TextBlock.
        // Blocks with shadow=true get a second buffer for the shadow copy.
        let mut buffers: Vec<Buffer> = Vec::with_capacity(texts.len() * 2);
        for block in texts {
            let metrics = Metrics::new(block.font_size, block.line_height);
            let max_w = block.max_width.unwrap_or_else(|| {
                if block.align_right {
                    // x is the right edge — the text can use most of the screen width
                    block.x
                } else {
                    Logical(screen_width as f32 / sf) - block.x
                }
            });
            let attrs = if block.bold {
                Attrs::new().family(ui_family()).weight(Weight::BOLD)
            } else {
                Attrs::new().family(ui_family())
            };

            let max_render_w_raw = block.max_render_width.map(|w| w.0);

            // Shadow buffer (identical text, rendered first at an offset)
            if block.shadow {
                let mut shadow_buf = Buffer::new(&mut self.font_system, metrics);
                shadow_buf.set_size(
                    &mut self.font_system,
                    Some(max_w.0),
                    Some(screen_height as f32 / sf),
                );
                Self::shape_and_truncate(
                    &mut self.font_system,
                    &mut shadow_buf,
                    &block.text,
                    &attrs,
                    max_render_w_raw,
                );
                buffers.push(shadow_buf);
            }

            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            buffer.set_size(
                &mut self.font_system,
                Some(max_w.0),
                Some(screen_height as f32 / sf),
            );
            Self::shape_and_truncate(
                &mut self.font_system,
                &mut buffer,
                &block.text,
                &attrs,
                max_render_w_raw,
            );

            // Measure actual text width and compute position adjustments.
            let text_width = Logical(measure_text_width(&buffer));
            let actual_x = if block.align_right {
                // x is the right edge — shift left by text width + pill padding
                let pad = block
                    .pill
                    .as_ref()
                    .map(|s| s.padding_x)
                    .unwrap_or(Logical(0.0));
                block.x - text_width - pad
            } else if block.align_center {
                // x is the desired text center — shift left by half the
                // measured width so the text truly centers on x.
                block.x - text_width * 0.5
            } else {
                block.x
            };
            x_offsets.push(actual_x);

            if let Some(ref style) = block.pill {
                measured_pills.push(MeasuredPill {
                    x: actual_x - style.padding_x,
                    y: block.y - style.padding_y,
                    width: text_width + style.padding_x * 2.0,
                    height: Logical(block.line_height) + style.padding_y * 2.0,
                    color: style.color,
                    corner_radius: style.corner_radius,
                });
            }

            buffers.push(buffer);
        }

        // Build TextAreas: shadow entries first (offset, dark), then main text on top.
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: screen_width as i32,
            bottom: screen_height as i32,
        };
        let mut text_areas: Vec<TextArea> = Vec::with_capacity(buffers.len());
        let mut buf_idx = 0;
        for (block_idx, block) in texts.iter().enumerate() {
            let actual_x = x_offsets[block_idx];
            if block.shadow {
                text_areas.push(TextArea {
                    buffer: &buffers[buf_idx],
                    left: (actual_x.0 + 0.5) * sf,
                    top: (block.y.0 + 0.5) * sf,
                    scale: sf,
                    bounds,
                    default_color: Color::rgba(0, 0, 0, 180),
                    custom_glyphs: &[],
                });
                buf_idx += 1;
            }
            let [r, g, b, a] = block.color;
            text_areas.push(TextArea {
                buffer: &buffers[buf_idx],
                left: actual_x.0 * sf,
                top: block.y.0 * sf,
                scale: sf,
                bounds,
                default_color: Color::rgba(r, g, b, a),
                custom_glyphs: &[],
            });
            buf_idx += 1;
        }

        self.text_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("Failed to prepare text");

        measured_pills
    }

    /// Render the prepared text into the render pass.
    pub fn render<'a>(&'a self, render_pass: &mut wgpu::RenderPass<'a>) {
        self.text_renderer
            .render(&self.atlas, &self.viewport, render_pass)
            .expect("Failed to render text");
    }

    /// Trim the atlas after each frame to free unused glyphs.
    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay has to name a font that exists on this machine. When it doesn't, cosmic-text
    /// picks whichever face its database returns first, which is how the text ended up in an
    /// arbitrary font off macOS.
    #[test]
    fn the_overlay_font_exists_on_this_host() {
        let fs = measurement_font_system()
            .lock()
            .expect("font system poisoned");
        if fs.db().is_empty() {
            // A machine with no fonts at all (a bare container). Nothing to assert.
            return;
        }
        let family = UI_FAMILY
            .get()
            .expect("building a font system resolves the overlay family");
        assert!(
            resolve_ui_family(fs.db(), &[family.as_str()]).is_some(),
            "the overlay font {family:?} isn't installed, candidates were {:?}",
            ui_font_candidates()
        );
    }

    /// A missing font is skipped rather than taken, which is what makes the list a fallback chain
    /// instead of a guess.
    #[test]
    fn the_candidate_list_skips_fonts_that_are_missing() {
        let fs = measurement_font_system()
            .lock()
            .expect("font system poisoned");
        let Some(installed) = fs
            .db()
            .faces()
            .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
        else {
            return; // no fonts on this machine
        };

        const ABSENT: &str = "Definitely Not An Installed Font 4242";
        assert_eq!(
            resolve_ui_family(fs.db(), &[ABSENT, &installed]),
            Some(installed.clone())
        );
        assert_eq!(resolve_ui_family(fs.db(), &[ABSENT]), None);
    }

    /// A render-width-capped block (like the title) must stay on a single line and
    /// middle-truncate, even when the buffer's wrap width is set wider than the cap (as the
    /// overlay layout does — it reserves room for the zoom pill). Pre-fix, the wider wrap
    /// width let glyphon break the title onto two lines for a band of window widths.
    #[test]
    fn render_capped_text_never_wraps() {
        let mut fs = FontSystem::new();
        let metrics = Metrics::new(13.5, 18.5);
        let mut buffer = Buffer::new(&mut fs, metrics);
        // Simulate the overlay sizing the buffer to a finite wrap width. Pre-fix, glyphon
        // wrapped at this width; when the wrapped lines fit under the cap, truncation never
        // fired and the title rendered on two lines. Using cap == wrap width is the clean,
        // font-independent way to land in that band: every wrapped line is <= the cap, so the
        // only way to stay single-line is for the fix to disable wrapping.
        let wrap_width = 150.0;
        buffer.set_size(&mut fs, Some(wrap_width), Some(40.0));
        let attrs = Attrs::new().family(ui_family()).weight(Weight::BOLD);

        let text = "6 / 39 \u{2013} 2026-04-17_at_12.58.27_125827@2x.png";
        let cap = wrap_width;

        let out =
            GlyphonRenderer::shape_and_truncate(&mut fs, &mut buffer, text, &attrs, Some(cap));

        assert_eq!(
            buffer.layout_runs().count(),
            1,
            "render-capped title must stay on one line, never wrap"
        );
        assert!(
            out.contains('\u{2026}'),
            "an over-long title should be middle-truncated with an ellipsis, got {out:?}"
        );
        assert!(
            measure_text_width(&buffer) <= cap + 1.0,
            "the truncated title must fit within the cap"
        );
    }

    /// Without a render-width cap, wrapping still works (multi-line blocks like overlays
    /// rely on the buffer's wrap width).
    #[test]
    fn uncapped_text_still_wraps() {
        let mut fs = FontSystem::new();
        let metrics = Metrics::new(13.5, 18.5);
        let mut buffer = Buffer::new(&mut fs, metrics);
        buffer.set_size(&mut fs, Some(120.0), Some(200.0));
        let attrs = Attrs::new().family(ui_family());

        let text = "The quick brown fox jumps over the lazy dog repeatedly all day long";
        let out = GlyphonRenderer::shape_and_truncate(&mut fs, &mut buffer, text, &attrs, None);

        assert_eq!(out, text, "no cap means no truncation");
        assert!(
            buffer.layout_runs().count() > 1,
            "uncapped long text should still wrap to multiple lines"
        );
    }
}

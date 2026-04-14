//! Text rendering via glyphon. Wraps font system, atlas, and renderer into a single API
//! that the main renderer can call to draw text overlays (header bar).

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};

/// A block of text to render at a specific position.
/// All coordinates and sizes are in **logical points** (not physical pixels).
/// The text renderer scales them by the display scale factor automatically.
pub struct TextBlock {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub font_size: f32,
    pub line_height: f32,
    pub color: [u8; 4], // RGBA
    pub max_width: Option<f32>,
    pub bold: bool,
    /// Drop shadow: renders the text twice — dark shadow offset by 1px, then the main color on top.
    /// This guarantees readability on any background without a backdrop blur or pill.
    pub shadow: bool,
    /// Maximum rendered width in logical pixels. If text exceeds this, truncate with
    /// middle ellipsis: "long_filen…photo.jpg". None = no truncation.
    pub max_render_width: Option<f32>,
    /// If set, draw a semi-transparent pill (rounded rect) behind the text.
    pub pill: Option<PillStyle>,
    /// If set, `x` is the RIGHT edge of the pill (text + padding), and the block is
    /// repositioned leftward after measuring the actual text width.
    pub align_right: bool,
}

pub struct PillStyle {
    pub color: [f32; 4],    // RGBA, each 0..1
    pub padding_x: f32,     // horizontal padding in logical pts
    pub padding_y: f32,     // vertical padding in logical pts
    pub corner_radius: f32, // in logical pts
}

/// A measured pill rect, computed from actual text width after shaping.
#[allow(dead_code)]
pub struct MeasuredPill {
    pub x: f32,      // logical pts
    pub y: f32,      // logical pts
    pub width: f32,  // logical pts
    pub height: f32, // logical pts
    pub color: [f32; 4],
    pub corner_radius: f32,
}

/// Measure the rendered width of shaped text in logical points.
fn measure_text_width(buffer: &Buffer) -> f32 {
    buffer.layout_runs().fold(0.0f32, |max_w, run| {
        let run_w = run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0);
        max_w.max(run_w)
    })
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
        #[allow(unused_mut)] // mut needed on macOS for load_font_source
        let mut font_system = FontSystem::new();

        // Load the macOS system font (SF Pro) directly from disk. fontdb's automatic
        // scanning finds SFNS.ttf but doesn't fully expose its variable font weight axis,
        // so requesting Weight::BOLD falls back to the wrong font. Loading the bytes
        // explicitly makes all weight variations available.
        // Load the macOS system font (SF Pro). SFNS.ttf is a variable font with a `wght`
        // axis, but fontdb registers it as a single weight-400 face. cosmic-text applies
        // the `wght` variation at render time, but fontdb's query won't SELECT the face
        // when asked for bold (weight 700) because it only sees weight 400.
        //
        // Fix: load it twice — fontdb deduplicates the data but creates two face entries.
        // We then find the second entry's ID and re-register it with weight=700 via
        // push_face_info, so fontdb will match it for bold queries.
        #[cfg(target_os = "macos")]
        {
            use glyphon::fontdb::{self, FaceInfo, Source};
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
        let mut x_offsets: Vec<f32> = Vec::with_capacity(texts.len());

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
                    screen_width as f32 / sf - block.x
                }
            });
            let attrs = if block.bold {
                Attrs::new()
                    .family(Family::Name("System Font"))
                    .weight(Weight::BOLD)
            } else {
                Attrs::new().family(Family::Name("System Font"))
            };

            // Shadow buffer (identical text, rendered first at an offset)
            if block.shadow {
                let mut shadow_buf = Buffer::new(&mut self.font_system, metrics);
                shadow_buf.set_size(
                    &mut self.font_system,
                    Some(max_w),
                    Some(screen_height as f32 / sf),
                );
                Self::shape_and_truncate(
                    &mut self.font_system,
                    &mut shadow_buf,
                    &block.text,
                    &attrs,
                    block.max_render_width,
                );
                buffers.push(shadow_buf);
            }

            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            buffer.set_size(
                &mut self.font_system,
                Some(max_w),
                Some(screen_height as f32 / sf),
            );
            Self::shape_and_truncate(
                &mut self.font_system,
                &mut buffer,
                &block.text,
                &attrs,
                block.max_render_width,
            );

            // Measure actual text width and compute position adjustments.
            let text_width = measure_text_width(&buffer);
            let actual_x = if block.align_right {
                // x is the right edge — shift left by text width + pill padding
                let pad = block.pill.as_ref().map(|s| s.padding_x).unwrap_or(0.0);
                block.x - text_width - pad
            } else {
                block.x
            };
            x_offsets.push(actual_x);

            if let Some(ref style) = block.pill {
                measured_pills.push(MeasuredPill {
                    x: actual_x - style.padding_x,
                    y: block.y - style.padding_y,
                    width: text_width + style.padding_x * 2.0,
                    height: block.line_height + style.padding_y * 2.0,
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
                    left: (actual_x + 0.5) * sf,
                    top: (block.y + 0.5) * sf,
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
                left: actual_x * sf,
                top: block.y * sf,
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

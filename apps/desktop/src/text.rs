//! Text rendering via glyphon. Wraps font system, atlas, and renderer into a single API
//! that the main renderer can call to draw text overlays (header bar, onboarding screen).

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

/// A block of text to render at a specific position.
pub struct TextBlock {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub font_size: f32,
    pub line_height: f32,
    pub color: [u8; 4], // RGBA
    pub max_width: Option<f32>,
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
        let font_system = FontSystem::new();
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

    /// Prepare and render text blocks into the given render pass.
    /// Call `prepare` before beginning the render pass, then `render` inside it.
    /// This method does both in sequence, so it must be called with an active render pass.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texts: &[TextBlock],
        screen_width: u32,
        screen_height: u32,
    ) {
        self.viewport.update(
            queue,
            Resolution {
                width: screen_width,
                height: screen_height,
            },
        );

        // Build a glyphon Buffer for each TextBlock
        let mut buffers: Vec<Buffer> = Vec::with_capacity(texts.len());
        for block in texts {
            let metrics = Metrics::new(block.font_size, block.line_height);
            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            let max_w = block.max_width.unwrap_or(screen_width as f32 - block.x);
            buffer.set_size(
                &mut self.font_system,
                Some(max_w),
                Some(screen_height as f32),
            );
            buffer.set_text(
                &mut self.font_system,
                &block.text,
                &Attrs::new().family(Family::SansSerif),
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            buffers.push(buffer);
        }

        // Build TextAreas referencing the buffers
        let text_areas: Vec<TextArea> = texts
            .iter()
            .zip(buffers.iter())
            .map(|(block, buffer)| {
                let [r, g, b, a] = block.color;
                TextArea {
                    buffer,
                    left: block.x,
                    top: block.y,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: screen_width as i32,
                        bottom: screen_height as i32,
                    },
                    default_color: Color::rgba(r, g, b, a),
                    custom_glyphs: &[],
                }
            })
            .collect();

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

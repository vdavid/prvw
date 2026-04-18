use super::text::{GlyphonRenderer, TextBlock};
use crate::decoding::{DecodedImage, PixelBuffer};
use crate::pixels::{Logical, Physical};
use crate::zoom::view::TransformUniform;
use image::ImageEncoder;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

/// GPU-side uniform for the overlay shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniform {
    pos: [f32; 4],    // x, y, width, height in physical pixels
    color: [f32; 4],  // RGBA 0..1
    params: [f32; 4], // corner_radius, screen_w, screen_h, 0
}

/// Owns all wgpu state: device, queue, surface, pipeline, texture, and uniform buffer.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// SDR surface format that was chosen at init time (for example
    /// `Bgra8UnormSrgb` on macOS). We store it so the HDR→SDR transition
    /// can flip back to the exact same format the platform preferred.
    sdr_format: wgpu::TextureFormat,
    /// Whether the adapter / surface combination supports `Rgba16Float`
    /// as a surface format. If `false`, `reconfigure_surface_format(true)`
    /// is a no-op — we stay SDR no matter what. Captured once at init.
    hdr_surface_supported: bool,
    /// Cached shader modules. Pipeline rebuilds on format change reuse
    /// these so we don't recompile WGSL on every EDR toggle.
    image_shader: wgpu::ShaderModule,
    overlay_shader: wgpu::ShaderModule,
    /// Cached pipeline layouts — also format-agnostic.
    image_pipeline_layout: wgpu::PipelineLayout,
    overlay_pipeline_layout: wgpu::PipelineLayout,
    render_pipeline: wgpu::RenderPipeline,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    text_renderer: GlyphonRenderer,
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_buffers: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    scale_factor: f64,
}

/// Build the image-quad render pipeline against a specific surface format.
/// Extracted so `reconfigure_surface_format` can rebuild on EDR transitions.
fn build_image_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("image pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Build the overlay pipeline (rounded-rect pills) against a specific
/// surface format. The pills blend alpha over whatever's underneath, so
/// the blend state is format-independent.
fn build_overlay_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("overlay pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Renderer {
    /// Create the renderer. Must be called in `resumed()` after the window exists.
    /// Uses `pollster::block_on` for the async wgpu initialization.
    pub fn new(window: Arc<Window>) -> Self {
        pollster::block_on(Self::init_async(window))
    }

    async fn init_async(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let scale_factor = window.scale_factor();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });

        let surface = instance
            .create_surface(window)
            .expect("Failed to create wgpu surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable GPU adapter found");

        let adapter_name = adapter.get_info().name;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("prvw device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("Failed to create wgpu device");

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        // Phase 5.1: the EDR surface path wants `Rgba16Float`. Check the
        // adapter actually supports it; log once at init so we know whether
        // dynamic HDR switching is possible on this machine. When the
        // format is missing (older Intel Mac, unusual GPUs), the surface
        // stays SDR-only forever and `Renderer::reconfigure_surface_format`
        // silently refuses the HDR switch.
        let hdr_surface_supported = surface_caps
            .formats
            .contains(&wgpu::TextureFormat::Rgba16Float);
        log::info!(
            "GPU surface formats: {:?} (HDR-capable: {})",
            surface_caps.formats,
            hdr_surface_supported,
        );

        // Prefer a non-opaque alpha mode so the title bar area can show vibrancy through
        // the transparent clear color. Falls back to the first available mode (typically
        // Opaque) on platforms that don't support compositing.
        let alpha_mode = surface_caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| {
                matches!(
                    m,
                    wgpu::CompositeAlphaMode::PostMultiplied
                        | wgpu::CompositeAlphaMode::PreMultiplied
                )
            })
            .unwrap_or(surface_caps.alpha_modes[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        log::info!(
            "GPU: {adapter_name}, surface: {}x{}, format: {:?}",
            config.width,
            config.height,
            surface_format
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transform uniform"),
            contents: bytemuck::bytes_of(&TransformUniform {
                col0: [1.0, 0.0, 0.0, 1.0],
                col1: [0.0, 0.0, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("image pipeline layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let render_pipeline =
            build_image_pipeline(&device, &shader, &image_pipeline_layout, surface_format);

        // Overlay pipeline for drawing semi-transparent rounded-rectangle pills behind text
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });

        let overlay_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("overlay bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let empty_uniform = OverlayUniform {
            pos: [0.0; 4],
            color: [0.0; 4],
            params: [0.0; 4],
        };
        let overlay_buffers: Vec<(wgpu::Buffer, wgpu::BindGroup)> = (0..8)
            .map(|i| {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(&format!("overlay uniform {i}")),
                    contents: bytemuck::bytes_of(&empty_uniform),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("overlay bind group {i}")),
                    layout: &overlay_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                (buffer, bind_group)
            })
            .collect();

        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay pipeline layout"),
                bind_group_layouts: &[Some(&overlay_bind_group_layout)],
                immediate_size: 0,
            });

        let overlay_pipeline = build_overlay_pipeline(
            &device,
            &overlay_shader,
            &overlay_pipeline_layout,
            surface_format,
        );

        let text_renderer = GlyphonRenderer::new(&device, &queue, surface_format);

        Self {
            surface,
            device,
            queue,
            config,
            sdr_format: surface_format,
            hdr_surface_supported,
            image_shader: shader,
            overlay_shader,
            image_pipeline_layout,
            overlay_pipeline_layout,
            render_pipeline,
            bind_group: None,
            bind_group_layout,
            uniform_buffer,
            sampler,
            text_renderer,
            overlay_pipeline,
            overlay_buffers,
            scale_factor,
        }
    }

    /// Flip the wgpu surface between the platform's SDR format (from init)
    /// and `Rgba16Float` for EDR output. Rebuilds the three render pipelines
    /// that reference the surface format (image-quad, overlay, glyphon text).
    /// Returns `true` if the format actually changed.
    ///
    /// `want_hdr == true` switches to `Rgba16Float`. `false` returns to the
    /// SDR format captured at init. Callers (the app's EDR-transition handler)
    /// are responsible for pairing this with the matching
    /// `CAMetalLayer.wantsExtendedDynamicRangeContent` / `pixelFormat` /
    /// colorspace changes on macOS.
    pub fn reconfigure_surface_format(&mut self, want_hdr: bool) -> bool {
        // Refuse HDR on adapters that don't advertise `Rgba16Float` as a
        // surface format. Configuring with an unsupported format would
        // either panic or silently produce a blank surface.
        let effective_hdr = want_hdr && self.hdr_surface_supported;
        if want_hdr && !self.hdr_surface_supported {
            log::debug!(
                "render: HDR surface requested but adapter doesn't support Rgba16Float — staying SDR"
            );
        }

        let target = if effective_hdr {
            wgpu::TextureFormat::Rgba16Float
        } else {
            self.sdr_format
        };
        if target == self.config.format {
            return false;
        }

        let from = self.config.format;
        log::info!(
            "render: surface format: {:?} -> {:?} ({} EDR)",
            from,
            target,
            if want_hdr { "enabling" } else { "disabling" },
        );

        self.config.format = target;
        self.surface.configure(&self.device, &self.config);

        self.render_pipeline = build_image_pipeline(
            &self.device,
            &self.image_shader,
            &self.image_pipeline_layout,
            target,
        );
        self.overlay_pipeline = build_overlay_pipeline(
            &self.device,
            &self.overlay_shader,
            &self.overlay_pipeline_layout,
            target,
        );
        // Rebuild the glyphon renderer — its TextAtlas pins the format at
        // construction time, so we recreate it rather than reach into its
        // internals.
        self.text_renderer = GlyphonRenderer::new(&self.device, &self.queue, target);

        true
    }

    /// Upload a decoded image as a GPU texture and create the bind group.
    ///
    /// `PixelBuffer::Rgba8` uploads to `Rgba8UnormSrgb`. `PixelBuffer::Rgba16F`
    /// uploads to `Rgba16Float` — the fragment shader samples it as
    /// `vec4<f32>` either way, so the same shader works for both paths.
    ///
    /// Phase 5.1: on EDR-capable displays, the surface itself is
    /// `Rgba16Float` (see `reconfigure_surface_format`) and
    /// `CAMetalLayer.wantsExtendedDynamicRangeContent = YES`, so values
    /// above 1.0 land on the compositor as true peak-white headroom. On
    /// SDR displays the surface stays `Bgra8UnormSrgb` and highlights
    /// quantise at the final blend — the wide-gamut cache still pays off
    /// for the tone-curve and ICC-transform stages upstream.
    pub fn set_image(&mut self, image: &DecodedImage) {
        let texture_size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let (format, bytes_per_pixel) = match &image.pixels {
            PixelBuffer::Rgba8(_) => (wgpu::TextureFormat::Rgba8UnormSrgb, 4u32),
            PixelBuffer::Rgba16F(_) => (wgpu::TextureFormat::Rgba16Float, 8u32),
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image texture"),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let pixel_bytes: &[u8] = match &image.pixels {
            PixelBuffer::Rgba8(v) => v.as_slice(),
            PixelBuffer::Rgba16F(v) => bytemuck::cast_slice(v.as_slice()),
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixel_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_pixel * image.width),
                rows_per_image: Some(image.height),
            },
            texture_size,
        );

        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }));
    }

    /// Update the transform uniform buffer with the current view state.
    pub fn update_transform(&self, transform: &TransformUniform) {
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(transform));
    }

    /// Handle window resize: update stored dimensions and reconfigure the surface.
    pub fn resize(&mut self, width: Physical<u32>, height: Physical<u32>) {
        if width.0 == 0 || height.0 == 0 {
            return;
        }
        if width.0 != self.config.width || height.0 != self.config.height {
            self.config.width = width.0;
            self.config.height = height.0;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Render the current image with optional text overlays. Returns false if the surface
    /// isn't ready. Pill backgrounds are computed from actual text measurements.
    /// Render the current frame. `content_offset_y` is the area reserved at the top in logical
    /// pixels — the image renders below it while pills/text render across the full surface.
    pub fn render(&mut self, text_blocks: &[TextBlock], content_offset_y: Logical<f32>) -> bool {
        let surface_texture = self.surface.get_current_texture();
        let output = match surface_texture {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            other => {
                log::trace!("wgpu surface status: {other:?}");
                return false;
            }
        };

        // Prepare text and get measured pill rects (computed from actual shaped text width)
        let measured_pills = if !text_blocks.is_empty() {
            self.text_renderer.prepare(
                &self.device,
                &self.queue,
                text_blocks,
                self.config.width,
                self.config.height,
                self.scale_factor,
            )
        } else {
            Vec::new()
        };

        // Write pill overlay uniforms BEFORE the render pass so they take effect
        let sf = self.scale_factor as f32;
        for (i, pill) in measured_pills.iter().enumerate() {
            if i >= self.overlay_buffers.len() {
                break;
            }
            let uniform = OverlayUniform {
                pos: [
                    pill.x.0 * sf,
                    pill.y.0 * sf,
                    pill.width.0 * sf,
                    pill.height.0 * sf,
                ],
                color: pill.color,
                params: [
                    pill.corner_radius.0 * sf,
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                ],
            };
            self.queue
                .write_buffer(&self.overlay_buffers[i].0, 0, bytemuck::bytes_of(&uniform));
        }

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("image render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Transparent clear so the title bar area shows the
                        // NSVisualEffectView vibrancy behind the Metal layer.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Draw image if loaded — confined to the image area below the title bar
            if let Some(bind_group) = &self.bind_group {
                let offset_px = (content_offset_y.0 as f64 * self.scale_factor) as f32;
                let sw = self.config.width as f32;
                let sh = self.config.height as f32;
                pass.set_viewport(0.0, offset_px, sw, (sh - offset_px).max(1.0), 0.0, 1.0);
                pass.set_pipeline(&self.render_pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
                // Reset viewport to full surface for pills and text
                pass.set_viewport(0.0, 0.0, sw, sh, 0.0, 1.0);
            }

            // Draw pill backgrounds (between image and text), each with its own bind group
            for i in 0..measured_pills.len().min(self.overlay_buffers.len()) {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_bind_group(0, &self.overlay_buffers[i].1, &[]);
                pass.draw(0..6, 0..1);
            }

            // Draw text overlay on top
            if !text_blocks.is_empty() {
                self.text_renderer.render(&mut pass);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        if !text_blocks.is_empty() {
            self.text_renderer.trim();
        }

        true
    }

    /// Capture the current scene as a PNG image. Returns empty Vec if no image is loaded.
    pub fn capture_screenshot(&self) -> Vec<u8> {
        let Some(bind_group) = &self.bind_group else {
            return Vec::new();
        };

        let width = self.config.width;
        let height = self.config.height;
        if width == 0 || height == 0 {
            return Vec::new();
        }

        // Screenshots always go through an SDR target so PNG readback +
        // BGRA→RGBA swizzle stay straightforward. When the live surface is
        // `Rgba16Float` (EDR path), build a one-shot SDR pipeline for the
        // capture pass — values above 1.0 clip to display-white, which is
        // the right thing for a PNG screenshot anyway.
        let screenshot_format = self.sdr_format;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: screenshot_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("screenshot encoder"),
            });

        // If the live pipeline already targets the SDR format, reuse it.
        // Otherwise, build a one-shot SDR pipeline.
        let screenshot_pipeline_owned;
        let screenshot_pipeline: &wgpu::RenderPipeline = if self.config.format == screenshot_format
        {
            &self.render_pipeline
        } else {
            screenshot_pipeline_owned = build_image_pipeline(
                &self.device,
                &self.image_shader,
                &self.image_pipeline_layout,
                screenshot_format,
            );
            &screenshot_pipeline_owned
        };

        // Render the scene to the offscreen SDR texture
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screenshot render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(screenshot_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        // Copy texture to a staging buffer
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = bytes_per_pixel * width;
        let padded_bytes_per_row = (unpadded_bytes_per_row + 255) & !255; // align to 256
        let buffer_size = (padded_bytes_per_row * height) as u64;

        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot staging buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(std::iter::once(encoder.finish()));

        // Map the buffer and read the pixels
        let buffer_slice = staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());

        if rx.recv().map(|r| r.is_err()).unwrap_or(true) {
            log::error!("Failed to map screenshot buffer");
            return Vec::new();
        }

        let data = buffer_slice.get_mapped_range();

        // Strip row padding and collect pixels. The surface format is BGRA, so swap R and B
        // to produce RGBA for the PNG encoder.
        let mut rgba_pixels = Vec::with_capacity((width * height * bytes_per_pixel) as usize);
        for row in 0..height {
            let start = (row * padded_bytes_per_row) as usize;
            let end = start + unpadded_bytes_per_row as usize;
            rgba_pixels.extend_from_slice(&data[start..end]);
        }
        drop(data);
        staging_buffer.unmap();

        // BGRA -> RGBA: swap R and B channels
        for pixel in rgba_pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        // Encode as PNG using the image crate
        let mut png_bytes: Vec<u8> = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        if let Err(e) =
            encoder.write_image(&rgba_pixels, width, height, image::ColorType::Rgba8.into())
        {
            log::error!("Failed to encode screenshot PNG: {e}");
            return Vec::new();
        }

        png_bytes
    }

    pub fn logical_width(&self) -> Logical<f32> {
        Physical(self.config.width).to_logical_f32(self.scale_factor)
    }

    pub fn logical_height(&self) -> Logical<f32> {
        Physical(self.config.height).to_logical_f32(self.scale_factor)
    }
}

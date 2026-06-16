use super::text::{GlyphonRenderer, StandalonePill, TextBlock};
use crate::decoding::{DecodedImage, PixelBuffer};
use crate::histogram::HistogramData;
use crate::pixels::{Logical, Physical};
use crate::zoom::view::TransformUniform;
use image::ImageEncoder;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

/// One histogram bar plot to draw on top of the overlay pills. The data is
/// passed by reference so the renderer can stream the 768 counts into the
/// storage buffer without taking ownership.
pub struct HistogramDrawCall<'a> {
    pub rect: super::text::StandalonePill,
    pub data: &'a HistogramData,
}

/// GPU-side uniform for the overlay shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniform {
    pos: [f32; 4],    // x, y, width, height in physical pixels
    color: [f32; 4],  // RGBA 0..1
    params: [f32; 4], // corner_radius, screen_w, screen_h, 0
}

/// GPU-side uniform for the histogram shader (rect + scaling params).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistogramUniform {
    /// (x, y, width, height) in physical pixels.
    rect: [f32; 4],
    /// (1 / max_count, screen_w, screen_h, edge_softness_px).
    params: [f32; 4],
}

/// Number of u32 entries in the histogram storage buffer (R + G + B = 3 × 256).
const HISTOGRAM_BIN_COUNT: usize = 256 * 3;

/// GPU state for an in-flight slideshow crossfade: the outgoing image's
/// texture plus a bind group that samples it through `prev_uniform_buffer`
/// (which holds the outgoing image's transform with fade = 1.0). The incoming
/// image keeps using the renderer's main `bind_group` / `uniform_buffer`,
/// whose fade factor ramps 0→1 via `set_crossfade`.
struct CrossfadeState {
    prev_texture: wgpu::Texture,
    prev_bind_group: wgpu::BindGroup,
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
    /// The GPU texture backing the currently displayed image. Stored so we
    /// can call `Texture::destroy()` when a new image loads — on macOS
    /// (unified memory) the old texture's backing stays resident until
    /// explicitly destroyed, which bloats RSS over a long session.
    image_texture: Option<wgpu::Texture>,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    /// Separate transform uniform for the outgoing image during a crossfade.
    /// Holds the outgoing transform with fade = 1.0 so it draws opaque while
    /// the incoming image fades in over it.
    prev_uniform_buffer: wgpu::Buffer,
    /// In-flight crossfade, if any. `Some` only between `begin_crossfade` and
    /// `end_crossfade`.
    crossfade: Option<CrossfadeState>,
    sampler: wgpu::Sampler,
    text_renderer: GlyphonRenderer,
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_buffers: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    /// Dedicated overlay buffer + bind group for the opaque-black image-area
    /// fill drawn when no image is bound (browse→image reveal with no ready
    /// target). Separate from `overlay_buffers` so the pill draw loop can't
    /// clobber it.
    black_fill: (wgpu::Buffer, wgpu::BindGroup),
    histogram_shader: wgpu::ShaderModule,
    histogram_pipeline_layout: wgpu::PipelineLayout,
    histogram_pipeline: wgpu::RenderPipeline,
    histogram_uniform: wgpu::Buffer,
    histogram_storage: wgpu::Buffer,
    histogram_bind_group: wgpu::BindGroup,
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
                // Alpha blending (not REPLACE) so the slideshow crossfade can
                // draw the incoming image at fade < 1.0 over the outgoing one.
                // For a normal single image the fragment outputs alpha 1.0,
                // which makes this identical to REPLACE over the transparent
                // clear.
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

/// Build the histogram pipeline. Uses alpha blending so the bars composite
/// on top of the backdrop pill cleanly.
fn build_histogram_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("histogram pipeline"),
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

        let initial_transform = TransformUniform {
            col0: [1.0, 0.0, 0.0, 1.0],
            col1: [0.0, 0.0, 1.0, 0.0], // col1.z = fade = 1.0
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transform uniform"),
            contents: bytemuck::bytes_of(&initial_transform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let prev_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("crossfade prev transform uniform"),
            contents: bytemuck::bytes_of(&initial_transform),
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
                    // The vertex stage reads the transform; the fragment stage
                    // reads the fade factor (col1.z) for the slideshow crossfade.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let overlay_buffers: Vec<(wgpu::Buffer, wgpu::BindGroup)> = (0..16)
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

        // Dedicated buffer + bind group for the opaque-black image-area fill.
        let black_fill_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("black fill uniform"),
            contents: bytemuck::bytes_of(&empty_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let black_fill_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("black fill bind group"),
            layout: &overlay_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: black_fill_buffer.as_entire_binding(),
            }],
        });

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

        // Histogram pipeline (Phase: histogram overlay).
        let histogram_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("histogram shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("histogram.wgsl").into()),
        });

        let histogram_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("histogram uniform"),
            contents: bytemuck::bytes_of(&HistogramUniform {
                rect: [0.0; 4],
                params: [0.0; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let histogram_storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("histogram storage"),
            size: (HISTOGRAM_BIN_COUNT * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let histogram_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("histogram bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let histogram_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("histogram bind group"),
            layout: &histogram_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: histogram_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: histogram_storage.as_entire_binding(),
                },
            ],
        });

        let histogram_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("histogram pipeline layout"),
                bind_group_layouts: &[Some(&histogram_bind_group_layout)],
                immediate_size: 0,
            });

        let histogram_pipeline = build_histogram_pipeline(
            &device,
            &histogram_shader,
            &histogram_pipeline_layout,
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
            image_texture: None,
            bind_group_layout,
            uniform_buffer,
            prev_uniform_buffer,
            crossfade: None,
            sampler,
            text_renderer,
            overlay_pipeline,
            overlay_buffers,
            black_fill: (black_fill_buffer, black_fill_bind_group),
            histogram_shader,
            histogram_pipeline_layout,
            histogram_pipeline,
            histogram_uniform,
            histogram_storage,
            histogram_bind_group,
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
        self.histogram_pipeline = build_histogram_pipeline(
            &self.device,
            &self.histogram_shader,
            &self.histogram_pipeline_layout,
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
        // Drop the bind group first (releases its TextureView ref) and then
        // explicitly destroy the previous image texture so its GPU / unified-
        // memory backing returns to the OS. Without this, Metal keeps the
        // old texture resident and long sessions balloon RSS by ~80 MB per
        // 20 MP image flipped through.
        self.bind_group = None;
        if let Some(old) = self.image_texture.take() {
            old.destroy();
        }

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
        self.image_texture = Some(texture);
    }

    /// Update the transform uniform buffer with the current view state.
    pub fn update_transform(&self, transform: &TransformUniform) {
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(transform));
    }

    /// Whether an image is currently loaded (has a bind group to draw).
    pub fn has_image(&self) -> bool {
        self.bind_group.is_some()
    }

    /// Drop the currently-bound image so the next `render` draws nothing in the
    /// image area (which `render` then fills with opaque black instead of the
    /// transparent clear). Releases the bind group and destroys the texture's
    /// backing, so a stale image can never composite under a new image's
    /// geometry. Used by the browse→image reveal when the target isn't ready
    /// and no usable placeholder exists — the user sees clean black, never the
    /// previous image stretched to the new transform. No-op if no image is set.
    pub fn clear_image(&mut self) {
        self.bind_group = None;
        if let Some(old) = self.image_texture.take() {
            old.destroy();
        }
        // Any in-flight crossfade references the now-gone image; drop it too so
        // `render` doesn't blend against a destroyed outgoing texture.
        if let Some(cf) = self.crossfade.take() {
            cf.prev_texture.destroy();
        }
    }

    /// Start a slideshow crossfade. Takes ownership of the currently-displayed
    /// image's texture (so the upcoming `set_image` won't destroy it) and
    /// builds a bind group that samples it through `prev_uniform_buffer` with
    /// the outgoing image's transform at full opacity. Call this *before*
    /// `set_image` uploads the incoming image. No-op if no image is loaded.
    ///
    /// The caller is responsible for only starting a crossfade when the
    /// surface size is unchanged between the two images — the outgoing
    /// transform is captured as-is and isn't recomputed for a new size.
    pub fn begin_crossfade(&mut self, prev_transform: &TransformUniform) {
        // Drop any crossfade still in flight (its prev texture).
        if let Some(old) = self.crossfade.take() {
            old.prev_texture.destroy();
        }
        let Some(prev_texture) = self.image_texture.take() else {
            // No image to fade from — nothing to do.
            return;
        };

        // The outgoing image draws opaque: force fade = 1.0 in its transform.
        let mut opaque = *prev_transform;
        opaque.col1[2] = 1.0;
        self.queue
            .write_buffer(&self.prev_uniform_buffer, 0, bytemuck::bytes_of(&opaque));

        let prev_view = prev_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let prev_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crossfade prev bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.prev_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&prev_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.crossfade = Some(CrossfadeState {
            prev_texture,
            prev_bind_group,
        });
    }

    /// Set the crossfade progress (0.0 = outgoing image fully shown, 1.0 =
    /// incoming image fully shown). `base` is the incoming image's transform;
    /// only its fade factor is overridden.
    pub fn set_crossfade(&self, base: &TransformUniform, progress: f32) {
        let mut t = *base;
        t.col1[2] = progress.clamp(0.0, 1.0);
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&t));
    }

    /// Finish the crossfade: drop the outgoing texture. The incoming image's
    /// fade is left at whatever the last `set_crossfade` wrote (1.0 at the end
    /// of the animation); the next `update_transform` restores a clean value.
    pub fn end_crossfade(&mut self) {
        if let Some(cf) = self.crossfade.take() {
            cf.prev_texture.destroy();
        }
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
    ///
    /// `standalone_pills` are extra rounded rects drawn through the same overlay pool —
    /// used for non-text backdrops (the histogram panel). `histogram` is an optional
    /// bar-plot draw call rendered above the pills and below the text.
    pub fn render(
        &mut self,
        text_blocks: &[TextBlock],
        standalone_pills: &[StandalonePill],
        histogram: Option<HistogramDrawCall<'_>>,
        content_offset_y: Logical<f32>,
    ) -> bool {
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

        // Write pill overlay uniforms BEFORE the render pass so they take effect.
        // Standalone pills go in first, then text-measured pills — pill_count is
        // the total used count for the draw loop below.
        let sf = self.scale_factor as f32;
        let mut pill_count = 0usize;
        for sp in standalone_pills {
            if pill_count >= self.overlay_buffers.len() {
                break;
            }
            let uniform = OverlayUniform {
                pos: [sp.x.0 * sf, sp.y.0 * sf, sp.width.0 * sf, sp.height.0 * sf],
                color: sp.color,
                params: [
                    sp.corner_radius.0 * sf,
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                ],
            };
            self.queue.write_buffer(
                &self.overlay_buffers[pill_count].0,
                0,
                bytemuck::bytes_of(&uniform),
            );
            pill_count += 1;
        }
        for pill in measured_pills.iter() {
            if pill_count >= self.overlay_buffers.len() {
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
            self.queue.write_buffer(
                &self.overlay_buffers[pill_count].0,
                0,
                bytemuck::bytes_of(&uniform),
            );
            pill_count += 1;
        }

        // When no image is bound, fill the image area with opaque black (written
        // here, drawn in the pass) so a reveal never shows the stale pre-browse
        // frame bleeding through the transparent clear. Skipped entirely when an
        // image IS bound (the image quad covers the area).
        let black_fill_active = self.bind_group.is_none();
        if black_fill_active {
            let offset_px = (content_offset_y.0 as f64 * self.scale_factor) as f32;
            let uniform = OverlayUniform {
                // Cover the full width, from the title-bar strip's bottom down.
                pos: [
                    0.0,
                    offset_px,
                    self.config.width as f32,
                    (self.config.height as f32 - offset_px).max(0.0),
                ],
                // Opaque black. Alpha 1.0 over the transparent clear reads as black.
                color: [0.0, 0.0, 0.0, 1.0],
                // No rounding — a plain rect over the image area.
                params: [
                    0.0,
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                ],
            };
            self.queue
                .write_buffer(&self.black_fill.0, 0, bytemuck::bytes_of(&uniform));
        }

        // Stream the histogram counts + uniform if a draw call is present.
        if let Some(draw) = histogram.as_ref() {
            let mut counts = [0u32; HISTOGRAM_BIN_COUNT];
            counts[..256].copy_from_slice(&draw.data.r);
            counts[256..512].copy_from_slice(&draw.data.g);
            counts[512..768].copy_from_slice(&draw.data.b);
            self.queue
                .write_buffer(&self.histogram_storage, 0, bytemuck::cast_slice(&counts));
            let max_recip = if draw.data.max_count == 0 {
                0.0
            } else {
                1.0 / draw.data.max_count as f32
            };
            let uniform = HistogramUniform {
                rect: [
                    draw.rect.x.0 * sf,
                    draw.rect.y.0 * sf,
                    draw.rect.width.0 * sf,
                    draw.rect.height.0 * sf,
                ],
                params: [
                    max_recip,
                    self.config.width as f32,
                    self.config.height as f32,
                    1.0_f32, // 1px edge softness for AA
                ],
            };
            self.queue
                .write_buffer(&self.histogram_uniform, 0, bytemuck::bytes_of(&uniform));
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
                // During a crossfade, draw the outgoing image first (opaque),
                // then the incoming image over it at its current fade factor.
                if let Some(cf) = &self.crossfade {
                    pass.set_bind_group(0, &cf.prev_bind_group, &[]);
                    pass.draw(0..6, 0..1);
                }
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
                // Reset viewport to full surface for pills and text
                pass.set_viewport(0.0, 0.0, sw, sh, 0.0, 1.0);
            } else {
                // No image bound: fill the image area (below the title-bar strip)
                // with OPAQUE black instead of leaving the transparent clear, which
                // would show whatever the compositor last had behind the Metal layer
                // (the stale pre-browse frame on a reveal). The title-bar strip stays
                // transparent so its vibrancy still shows through. Black-fill was
                // already written before the pass began (see `black_fill_active`).
                if black_fill_active {
                    pass.set_pipeline(&self.overlay_pipeline);
                    pass.set_bind_group(0, &self.black_fill.1, &[]);
                    pass.draw(0..6, 0..1);
                }
            }

            // Draw pill backgrounds (between image and text), each with its own bind group
            for i in 0..pill_count {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_bind_group(0, &self.overlay_buffers[i].1, &[]);
                pass.draw(0..6, 0..1);
            }

            // Draw histogram bars (between pills and text).
            if histogram.is_some() {
                pass.set_pipeline(&self.histogram_pipeline);
                pass.set_bind_group(0, &self.histogram_bind_group, &[]);
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

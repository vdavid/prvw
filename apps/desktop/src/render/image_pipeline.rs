//! The image quad's GPU side: the shader, both bind group layouts, the per-frame uniform, and
//! the upload that turns a `DecodedImage` into something a bind group can draw.
//!
//! Factored out of `Renderer` so it needs only a device and a queue, never a window or a surface:
//! the GPU parity tests at the bottom of this file render through exactly this code into an
//! offscreen target.
//!
//! **Bindings** (`shader.wgsl` declares the same):
//!
//! - Group 0, per image, owned by that image's bind group, so a crossfade's outgoing image keeps
//!   its own: 0 transform uniform, 1 image texture, 2 sampler, 3 gain-map texture (a 1×1
//!   placeholder until an encoding carries a map), 4 [`ImageParams`] uniform.
//! - Group 1, per frame, one for the whole renderer: 0 [`FrameUniform`]. A headroom change writes
//!   this one buffer; nothing per image is touched.

use crate::color::hdr::fragment::ShaderMode;
use crate::decoding::{DecodedImage, PixelBuffer};
use wgpu::util::DeviceExt;

/// Group 0, binding 4: which shader branch draws this image, and (from M3 on) that branch's
/// parameters. Appending fields is how a mode gets its inputs; keep the WGSL struct in step.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ImageParams {
    pub mode: u32,
    pub _pad: [u32; 3],
}

impl ImageParams {
    pub fn for_mode(mode: ShaderMode) -> Self {
        Self {
            mode: mode as u32,
            _pad: [0; 3],
        }
    }
}

/// Group 1, binding 0: what every image on screen shares about the display.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FrameUniform {
    /// The display's current headroom over SDR white, at least 1.0.
    pub headroom_now: f32,
    pub _pad: [f32; 3],
}

impl FrameUniform {
    pub fn with_headroom(headroom_now: f32) -> Self {
        Self {
            headroom_now,
            _pad: [0.0; 3],
        }
    }
}

/// One image as the GPU holds it. The texture is destroyed explicitly (see `destroy`) because
/// Metal keeps unified-memory backing resident until it is.
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub params: wgpu::Buffer,
}

impl GpuImage {
    /// Return the texture's memory now, not whenever the last reference drops.
    pub fn destroy(self) {
        self.texture.destroy();
        self.params.destroy();
    }
}

/// Everything about the image quad that doesn't depend on the surface format, built once.
/// [`Self::build`] makes a pipeline per format, which is what an EDR switch rebuilds.
pub struct ImagePipeline {
    shader: wgpu::ShaderModule,
    image_layout: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    /// Bound at binding 3 for every image without a gain map, which is every image until M3.
    /// `Rgba8Unorm`, not `Srgb`: a gain map is data, so the shader must read its raw values.
    placeholder_gain_map: wgpu::TextureView,
    frame_buffer: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    /// Group 1 pinned at headroom 1.0, for renders that must show the SDR rendition whatever the
    /// display is doing: the screenshot readback.
    sdr_frame_bind_group: wgpu::BindGroup,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let uniform_entry = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image bind group layout"),
            entries: &[
                // The vertex stage reads the transform; the fragment stage reads the fade factor
                // (col1.z) for the slideshow crossfade.
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(3),
                uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame bind group layout"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image pipeline layout"),
            bind_group_layouts: &[Some(&image_layout), Some(&frame_layout)],
            immediate_size: 0,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // A gain of "no change" whatever a future mode reads from it: all zeros.
        let placeholder = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("placeholder gain map"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[0, 0, 0, 255],
        );
        let placeholder_gain_map = placeholder.create_view(&wgpu::TextureViewDescriptor::default());

        let frame_group = |label, headroom| {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::bytes_of(&FrameUniform::with_headroom(headroom)),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &frame_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, group)
        };
        let (frame_buffer, frame_bind_group) = frame_group("frame uniform", 1.0);
        let (_sdr_frame_buffer, sdr_frame_bind_group) = frame_group("SDR frame uniform", 1.0);

        Self {
            shader,
            image_layout,
            layout,
            sampler,
            placeholder_gain_map,
            frame_buffer,
            frame_bind_group,
            sdr_frame_bind_group,
        }
    }

    /// A render pipeline drawing into `format`.
    pub fn build(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image pipeline"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Alpha blending (not REPLACE) so the slideshow crossfade can draw the
                    // incoming image at fade < 1.0 over the outgoing one. For a normal single
                    // image the fragment outputs alpha 1.0, which makes this identical to REPLACE
                    // over the transparent clear.
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

    /// Tell every image on screen the display's current headroom. One buffer write; the caller
    /// asks for a redraw.
    pub fn set_headroom_now(&self, queue: &wgpu::Queue, headroom_now: f32) {
        queue.write_buffer(
            &self.frame_buffer,
            0,
            bytemuck::bytes_of(&FrameUniform::with_headroom(headroom_now)),
        );
    }

    /// Group 1 for the live display.
    pub fn frame_bind_group(&self) -> &wgpu::BindGroup {
        &self.frame_bind_group
    }

    /// Group 1 at headroom 1.0, so a render shows the SDR rendition.
    pub fn sdr_frame_bind_group(&self) -> &wgpu::BindGroup {
        &self.sdr_frame_bind_group
    }

    /// Upload `image`. The texture format follows storage (`Rgba8` → `Rgba8UnormSrgb`, `Rgba16F`
    /// → `Rgba16Float`), the shader mode follows meaning (`PixelEncoding::shader_mode`).
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &DecodedImage,
    ) -> GpuImage {
        let (format, bytes_per_pixel, pixel_bytes): (_, u32, &[u8]) = match &image.pixels {
            PixelBuffer::Rgba8(v) => (wgpu::TextureFormat::Rgba8UnormSrgb, 4, v.as_slice()),
            PixelBuffer::Rgba16F(v) => (
                wgpu::TextureFormat::Rgba16Float,
                8,
                bytemuck::cast_slice(v.as_slice()),
            ),
        };
        let size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
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
            size,
        );

        let mode = image.encoding.shader_mode();
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("image params uniform"),
            contents: bytemuck::bytes_of(&ImageParams::for_mode(mode)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        GpuImage { texture, params }
    }

    /// A group 0 bind group drawing `image` with the transform in `transform`.
    pub fn bind(
        &self,
        device: &wgpu::Device,
        image: &GpuImage,
        transform: &wgpu::Buffer,
        label: &str,
    ) -> wgpu::BindGroup {
        let view = image
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.image_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: transform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.placeholder_gain_map),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: image.params.as_entire_binding(),
                },
            ],
        })
    }
}

/// The GPU parity harness (`docs/specs/hdr-images.md`, section 8): render a strip of known texels
/// through the real image pipeline into an `Rgba16Float` target at a forced headroom, read it
/// back, and compare with `color::hdr::fragment::shade`.
///
/// **Adding a case:** build a `DecodedImage` one pixel high (`strip_rgba8` / `strip_f16`, or a
/// new helper for a new storage), pick the headrooms, and call `assert_matches_reference`. One row
/// at exactly the target's width means every fragment lands on a texel centre, so bilinear
/// filtering returns the texel itself and the comparison tests the colour math alone. A mode that
/// samples a second texture (M3's gain map) needs its reference to sample the same way.
///
/// **Where it runs:** macOS (Metal) and Windows (DX12, WARP on a GPU-less runner) must have an
/// adapter, and a missing one fails the test. Linux CI runners have no GPU and no software Vulkan
/// installed, so the tests are `ignore`d there by an explicit `cfg_attr` rather than skipped on a
/// missing adapter, which would pass silently everywhere.
#[cfg(test)]
mod gpu_parity {
    use super::*;
    use crate::color::hdr::fragment::{FrameInputs, shade};
    use crate::decoding::PixelEncoding;
    use crate::render::gpu::GpuPolicy;
    use crate::zoom::view::TransformUniform;
    use half::f16;

    fn headless_gpu() -> (wgpu::Device, wgpu::Queue) {
        let policy = GpuPolicy::for_host();
        for request in policy.attempts() {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: request.backends,
                flags: wgpu::InstanceFlags::default()
                    .difference(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                backend_options: wgpu::BackendOptions::default(),
                display: None,
            });
            let Ok(adapter) =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: request.power_preference,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                }))
            else {
                continue;
            };
            return pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("gpu parity device"),
                ..Default::default()
            }))
            .expect("the parity test's adapter refused a device");
        }
        panic!(
            "No GPU adapter for the parity test. macOS and Windows always have one (Windows CI \
             falls back to WARP); if this host genuinely has none, gate it off with a cfg like \
             Linux, never by skipping here"
        );
    }

    /// Render `image` (one row) at `headroom_now` and read back linear RGB per pixel.
    fn render(image: &DecodedImage, headroom_now: f32) -> Vec<[f32; 3]> {
        assert_eq!(image.height, 1, "the harness renders one-row strips");
        let (device, queue) = headless_gpu();
        let pipeline = ImagePipeline::new(&device, &queue);
        let format = wgpu::TextureFormat::Rgba16Float;
        let render_pipeline = pipeline.build(&device, format);
        pipeline.set_headroom_now(&queue, headroom_now);

        // Identity transform, fade 1.0: the quad covers the target exactly.
        let transform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("parity transform"),
            contents: bytemuck::bytes_of(&TransformUniform {
                col0: [1.0, 0.0, 0.0, 1.0],
                col1: [0.0, 0.0, 1.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let gpu_image = pipeline.upload(&device, &queue, image);
        let bind_group = pipeline.bind(&device, &gpu_image, &transform, "parity image");

        let width = image.width;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity target"),
            size: wgpu::Extent3d {
                width,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let bytes_per_row = (width * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity readback"),
            size: u64::from(bytes_per_row),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("parity pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&render_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_bind_group(1, pipeline.frame_bind_group(), &[]);
            pass.draw(0..6, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |result| {
            result.expect("map the parity readback")
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("wait for the parity render");
        let mapped = slice.get_mapped_range().expect("read the parity readback");
        let halves: &[u16] = bytemuck::cast_slice(&mapped[..(width * 8) as usize]);
        halves
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| {
                [
                    f16::from_bits(px[0]).to_f32(),
                    f16::from_bits(px[1]).to_f32(),
                    f16::from_bits(px[2]).to_f32(),
                ]
            })
            .collect()
    }

    /// What the shader's texture sample hands the colour math: the storage's own decode.
    fn sampled(image: &DecodedImage) -> Vec<[f32; 3]> {
        match &image.pixels {
            PixelBuffer::Rgba8(bytes) => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| [0, 1, 2].map(|c| srgb_decode(px[c])))
                .collect(),
            PixelBuffer::Rgba16F(halves) => halves
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| [0, 1, 2].map(|c| f16::from_bits(px[c]).to_f32()))
                .collect(),
        }
    }

    /// The `Rgba8UnormSrgb` texture decode, per the sRGB spec.
    fn srgb_decode(byte: u8) -> f32 {
        let v = f32::from(byte) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Relative error under 1%, or absolute under 1e-3 near black (section 8's bounds).
    fn assert_matches_reference(image: &DecodedImage, headrooms: &[f32]) {
        let mode = image.encoding.shader_mode();
        let inputs = image.encoding.shader_inputs(image.width, image.height);
        for &headroom_now in headrooms {
            let got = render(image, headroom_now);
            let frame = FrameInputs { headroom_now };
            for (i, (got, sample)) in got.iter().zip(sampled(image)).enumerate() {
                let uv = [(i as f32 + 0.5) / image.width as f32, 0.5];
                let want = shade(&inputs, sample, uv, frame);
                for c in 0..3 {
                    let error = (got[c] - want[c]).abs();
                    assert!(
                        error < 1e-3 || error < want[c].abs() * 0.01,
                        "{mode:?} at headroom {headroom_now}, pixel {i} channel {c}: GPU {} vs \
                         reference {}",
                        got[c],
                        want[c]
                    );
                }
            }
        }
    }

    fn strip_rgba8(values: &[u8]) -> DecodedImage {
        let pixels = values.iter().flat_map(|&v| [v, v / 2, 255 - v, 255]);
        DecodedImage::from_rgba8(values.len() as u32, 1, pixels.collect())
    }

    fn strip_f16(values: &[f32]) -> Vec<u16> {
        values
            .iter()
            .flat_map(|&v| [v, v * 0.5, v * 0.25, 1.0])
            .map(|v| f16::from_f32(v).to_bits())
            .collect()
    }

    const HEADROOMS: [f32; 3] = [1.0, 2.0, 8.0];

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_0_rgba8_matches_the_reference() {
        let values: Vec<u8> = (0..=255).step_by(5).collect();
        let image = strip_rgba8(&values);
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        assert_matches_reference(&image, &HEADROOMS);
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_0_half_float_matches_the_reference() {
        let values = [0.0, 0.0005, 0.01, 0.18, 0.5, 0.75, 1.0];
        let image =
            DecodedImage::from_display_sdr_rgba16f(values.len() as u32, 1, strip_f16(&values));
        assert_matches_reference(&image, &HEADROOMS);
    }

    /// RAW HDR's above-white values reach the target unclipped, at every headroom.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_1_keeps_highlights_and_matches_the_reference() {
        let values = [0.0, 0.0005, 0.18, 1.0, 2.0, 4.0, 12.0];
        let image =
            DecodedImage::from_linear_hdr_rgba16f(values.len() as u32, 1, strip_f16(&values));
        assert_eq!(image.encoding, PixelEncoding::LinearHdrDisplay);
        assert_matches_reference(&image, &HEADROOMS);
    }
}

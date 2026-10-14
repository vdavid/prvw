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

use crate::color::hdr::gain_map::Mat3;
use crate::decoding::{DecodedImage, PixelBuffer, PixelEncoding};
use wgpu::util::DeviceExt;

/// Group 0, binding 4: which shader branch draws this image, and that branch's parameters.
/// Appending fields is how a mode gets its inputs; keep the WGSL struct in step. Every field is
/// 16 bytes or a multiple, so the `repr(C)` layout is the uniform's.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ImageParams {
    pub mode: u32,
    /// Mode 2: 0 for ISO 21496-1's log2 curve, 1 for Apple's (`GainCurve`).
    pub gain_curve: u32,
    pub _pad: [u32; 2],
    /// Mode 2, per channel in `xyz`: log2 gain at map value 0 and 1, the map gamma, and the
    /// two offsets (`color::hdr::gain_map::ChannelParams`).
    pub gain_min: [f32; 4],
    pub gain_max: [f32; 4],
    pub gamma: [f32; 4],
    pub base_offset: [f32; 4],
    pub alternate_offset: [f32; 4],
    /// Mode 2: `x` the base rendition's headroom, `y` the alternate's, both log2.
    pub headroom: [f32; 4],
    /// Mode 2's matrices, one row per `vec4` (`xyz` used): `GainMapSpaces`.
    pub p3_to_app: [[f32; 4]; 3],
    pub app_to_display: [[f32; 4]; 3],
    pub p3_to_display: [[f32; 4]; 3],
    /// Mode 3: the source primaries into the HDR display space, one row per `vec4`
    /// (`color::hdr::tone_map::SceneLinearParams`).
    pub source_to_display: [[f32; 4]; 3],
    /// Mode 3: `xyz` the source's luminance weights, `w` the content peak.
    pub scene: [f32; 4],
}

impl ImageParams {
    pub fn for_encoding(encoding: &PixelEncoding) -> Self {
        let mut params = Self::zeroed_with_mode(encoding.shader_mode() as u32);
        match encoding {
            PixelEncoding::DisplaySdr | PixelEncoding::LinearHdrDisplay => {}
            PixelEncoding::SceneLinear(s) => {
                params.source_to_display = rows(&s.source_to_display);
                let [r, g, b] = s.luminance_weights;
                params.scene = [r, g, b, s.content_peak];
            }
            PixelEncoding::GainMapped(map) => {
                let p = &map.params;
                params.gain_curve = match p.curve {
                    crate::color::hdr::gain_map::GainCurve::Log2 => 0,
                    crate::color::hdr::gain_map::GainCurve::Apple => 1,
                };
                let per_channel = |f: fn(&crate::color::hdr::gain_map::ChannelParams) -> f32| {
                    [f(&p.channels[0]), f(&p.channels[1]), f(&p.channels[2]), 0.0]
                };
                params.gain_min = per_channel(|c| c.min);
                params.gain_max = per_channel(|c| c.max);
                params.gamma = per_channel(|c| c.gamma);
                params.base_offset = per_channel(|c| c.base_offset);
                params.alternate_offset = per_channel(|c| c.alternate_offset);
                params.headroom = [p.base_headroom, p.alternate_headroom, 0.0, 0.0];
                params.p3_to_app = rows(&map.spaces.p3_to_app);
                params.app_to_display = rows(&map.spaces.app_to_display);
                params.p3_to_display = rows(&map.spaces.p3_to_display);
            }
        }
        params
    }

    fn zeroed_with_mode(mode: u32) -> Self {
        Self {
            mode,
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

fn rows(m: &Mat3) -> [[f32; 4]; 3] {
    m.map(|[a, b, c]| [a, b, c, 0.0])
}

/// What `shader.wgsl` reads: its `ImageParams` must be exactly this size, or the GPU reads the
/// wrong fields with no error anywhere.
const _: () = assert!(std::mem::size_of::<ImageParams>() == 16 + 6 * 16 + 9 * 16 + 4 * 16);

/// Group 1, binding 0: what every image on screen shares about the display.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FrameUniform {
    /// The display's current headroom over SDR white, at least 1.0.
    pub headroom_now: f32,
    /// What every fragment's final colour is multiplied by: SDR white in the surface's units
    /// (`DisplayHdr::sdr_white_scale`). Above 1.0 only on an scRGB EDR surface.
    pub sdr_white_scale: f32,
    pub _pad: [f32; 2],
}

impl FrameUniform {
    /// What a surface that can't show HDR gets: SDR white at 1.0, no headroom.
    pub const SDR: Self = Self::new(1.0, 1.0);

    pub const fn new(headroom_now: f32, sdr_white_scale: f32) -> Self {
        Self {
            headroom_now,
            sdr_white_scale,
            _pad: [0.0; 2],
        }
    }
}

/// One image as the GPU holds it. The texture is destroyed explicitly (see `destroy`) because
/// Metal keeps unified-memory backing resident until it is.
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub params: wgpu::Buffer,
    /// The gain map, for a `GainMapped` image; every other image binds the placeholder.
    pub gain_map: Option<wgpu::Texture>,
}

impl GpuImage {
    /// Return the textures' memory now, not whenever the last reference drops.
    pub fn destroy(self) {
        self.texture.destroy();
        self.params.destroy();
        if let Some(map) = self.gain_map {
            map.destroy();
        }
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

        let frame_group = |label, frame: FrameUniform| {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::bytes_of(&frame),
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
        let (frame_buffer, frame_bind_group) = frame_group("frame uniform", FrameUniform::SDR);
        let (_sdr_frame_buffer, sdr_frame_bind_group) =
            frame_group("SDR frame uniform", FrameUniform::SDR);

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

    /// Tell every image on screen what the display is doing now. One buffer write; the caller
    /// asks for a redraw.
    pub fn set_frame(&self, queue: &wgpu::Queue, frame: FrameUniform) {
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&frame));
    }

    /// Group 1 at headroom 1.0, so a render shows the SDR rendition.
    pub fn sdr_frame_bind_group(&self) -> &wgpu::BindGroup {
        &self.sdr_frame_bind_group
    }

    /// Group 1 for a render into a surface that is (or isn't) in EDR mode. Only an EDR surface can
    /// show anything above SDR white, so every other one gets the headroom-1 group: decoding gates
    /// on the display being capable, but the surface can still stay SDR (macOS refusing EDR, a
    /// Windows adapter that fell back from DX12 to Vulkan), and a gain map applied at the live
    /// headroom would then clip at 1.0.
    pub fn frame_bind_group_for(&self, edr_surface: bool) -> &wgpu::BindGroup {
        if edr_surface {
            &self.frame_bind_group
        } else {
            &self.sdr_frame_bind_group
        }
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

        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("image params uniform"),
            contents: bytemuck::bytes_of(&ImageParams::for_encoding(&image.encoding)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let gain_map = match &image.encoding {
            PixelEncoding::DisplaySdr
            | PixelEncoding::LinearHdrDisplay
            | PixelEncoding::SceneLinear(_) => None,
            // `Rgba8Unorm`, not `Srgb`: the shader must read the map's raw values.
            PixelEncoding::GainMapped(map) => Some(device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("gain map"),
                    size: wgpu::Extent3d {
                        width: map.width,
                        height: map.height,
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
                &map.rgba,
            )),
        };
        GpuImage {
            texture,
            params,
            gain_map,
        }
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
        let gain_map_view = image
            .gain_map
            .as_ref()
            .map(|map| map.create_view(&wgpu::TextureViewDescriptor::default()));
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
                    resource: wgpu::BindingResource::TextureView(
                        gain_map_view.as_ref().unwrap_or(&self.placeholder_gain_map),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: image.params.as_entire_binding(),
                },
            ],
        })
    }
}

/// The GPU parity harness (`docs/specs/hdr-images.md`, section 8): render a small image of known
/// texels through the real image pipeline into an `Rgba16Float` target at a forced headroom, read
/// it back, and compare with `color::hdr::fragment::shade`.
///
/// **Adding a case:** build a `DecodedImage` (`strip_rgba8` / `strip_f16` / `gain_mapped`, or a
/// new helper for a new storage), pick the headrooms, and call `assert_matches_reference`. The
/// target is exactly the image's size, so every fragment lands on a texel centre, bilinear
/// filtering returns the texel itself, and the comparison tests the colour math alone. The gain
/// map is read with `textureLoad` at computed coordinates, which the reference mirrors.
///
/// **Where it runs:** macOS (Metal) and Windows (DX12, WARP on a GPU-less runner) must have an
/// adapter, and a missing one fails the test. Linux CI runners have no GPU and no software Vulkan
/// installed, so the tests are `ignore`d there by an explicit `cfg_attr` rather than skipped on a
/// missing adapter, which would pass silently everywhere.
#[cfg(test)]
mod gpu_parity {
    use super::*;
    use crate::color::hdr::fragment::{FrameInputs, shade};
    use crate::color::hdr::gain_map::{ChannelParams, GainMapParams, GainMapSource, GainMapSpaces};
    use crate::color::hdr::tone_map::SceneLinearParams;
    use crate::decoding::GainMapPayload;
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

    /// Render `image` at `headroom_now` into an EDR target and read back linear RGB per pixel, row
    /// by row.
    fn render(image: &DecodedImage, headroom_now: f32) -> Vec<[f32; 3]> {
        render_onto(image, FrameUniform::new(headroom_now, 1.0), true)
    }

    /// [`render`] with the whole live frame uniform, choosing group 1 the way the renderer does for
    /// a surface that is or isn't EDR.
    fn render_onto(image: &DecodedImage, frame: FrameUniform, edr_surface: bool) -> Vec<[f32; 3]> {
        let (device, queue) = headless_gpu();
        let pipeline = ImagePipeline::new(&device, &queue);
        let format = wgpu::TextureFormat::Rgba16Float;
        let render_pipeline = pipeline.build(&device, format);
        pipeline.set_frame(&queue, frame);

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

        let (width, height) = (image.width, image.height);
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity target"),
            size,
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
            size: u64::from(bytes_per_row * height),
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
            pass.set_bind_group(1, pipeline.frame_bind_group_for(edr_surface), &[]);
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
                    rows_per_image: Some(height),
                },
            },
            size,
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
        mapped
            .chunks(bytes_per_row as usize)
            .flat_map(|row| {
                let halves: &[u16] = bytemuck::cast_slice(&row[..(width * 8) as usize]);
                halves.as_chunks::<4>().0.to_vec()
            })
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
                let (x, y) = (i as u32 % image.width, i as u32 / image.width);
                let uv = [
                    (x as f32 + 0.5) / image.width as f32,
                    (y as f32 + 0.5) / image.height as f32,
                ];
                let want = shade(&inputs, sample, uv, frame);
                for c in 0..3 {
                    let error = (got[c] - want[c]).abs();
                    assert!(
                        error < 1e-3 || error < want[c].abs() * 0.01,
                        "{mode:?} at headroom {headroom_now}, pixel ({x}, {y}) channel {c}: GPU \
                         {} vs reference {}",
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

    /// Neighbouring values that differ a lot, so a sampler that isn't the reference's shows.
    fn noise(i: u32) -> u8 {
        (i.wrapping_mul(2_654_435_761) >> 24) as u8
    }

    /// A busy base over a busy map, with per-channel parameters and the real color conversions
    /// for an sRGB base (so `p3_to_app` and `app_to_display` aren't the identity).
    fn gain_mapped() -> DecodedImage {
        gain_mapped_sized(16, 8, 4, 2)
    }

    fn gain_mapped_sized(width: u32, height: u32, map_w: u32, map_h: u32) -> DecodedImage {
        let base = (0..width * height * 4)
            .map(|i| if i % 4 == 3 { 255 } else { noise(i) })
            .collect();
        let rgba = (0..map_w * map_h * 4)
            .map(|i| if i % 4 == 3 { 255 } else { noise(i + 99) })
            .collect();
        let mut channels = [ChannelParams {
            max: 2.0,
            ..ChannelParams::default()
        }; 3];
        channels[1].gamma = 0.7;
        channels[2].min = -0.5;
        channels[2].max = 1.5;
        channels[2].alternate_offset = 0.03;
        let params = GainMapParams {
            channels,
            base_headroom: 0.0,
            alternate_headroom: 2.0,
            use_base_color_space: true,
            curve: crate::color::hdr::gain_map::GainCurve::Log2,
        };
        DecodedImage::from_gain_mapped(
            width,
            height,
            base,
            GainMapPayload {
                width: map_w,
                height: map_h,
                rgba,
                params,
                source: GainMapSource::Iso21496,
                spaces: GainMapSpaces::new(crate::color::srgb_icc_bytes(), None, true),
            },
        )
    }

    /// The same busy base and map, read as Apple's maker-note flavor: its own curve, no offsets.
    fn apple_gain_mapped() -> DecodedImage {
        let mut image = gain_mapped();
        if let PixelEncoding::GainMapped(map) = &mut image.encoding {
            map.params = crate::color::hdr::apple_params(1.7678).expect("usable");
            map.source = GainMapSource::Apple;
        }
        image
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

    /// The gain map on the GPU, per channel and between map texels, at three headrooms, and at
    /// headrooms in between, where the weight is partial.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_2_matches_the_reference() {
        let image = gain_mapped();
        assert_eq!(image.encoding.shader_mode() as u32, 2);
        assert_matches_reference(&image, &[1.0, 1.4, 2.0, 3.0, 8.0]);
        let brightest = render(&image, 8.0)
            .into_iter()
            .flatten()
            .fold(0.0f32, f32::max);
        assert!(
            brightest > 1.5,
            "the gain must lift highlights: {brightest}"
        );
    }

    /// At a width that isn't a power of two the interpolated texture coordinate lands a hair off
    /// each pixel centre, and Shepard's weighting jumps at a cell edge: the shader has to snap
    /// onto the pixel, as the reference does, or it reads the wrong neighbour (the iPhone's 776
    /// over 388).
    /// Apple's curve on the GPU, at partial and full weight.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_2_with_apples_curve_matches_the_reference() {
        assert_matches_reference(&apple_gain_mapped(), &[1.0, 1.5, 2.5, 8.0]);
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_2_lands_on_pixel_centres_at_any_width() {
        assert_matches_reference(&gain_mapped_sized(776, 4, 388, 2), &[8.0]);
    }

    /// A scene-linear strip in BT.2020 with a 6× content peak: deep shadows, reference white, and
    /// highlights past the peak, in every channel ratio `strip_f16` makes.
    fn scene_linear() -> DecodedImage {
        let values = [0.0, 0.0004, 0.02, 0.18, 0.5, 1.0, 2.0, 3.5, 6.0, 9.0];
        let source = crate::color::profiles::linear_rec2020_profile();
        let display = match crate::color::profiles::HdrDisplaySpace::for_host() {
            crate::color::profiles::HdrDisplaySpace::LinearDisplayP3 => {
                moxcms::ColorProfile::new_display_p3()
            }
            crate::color::profiles::HdrDisplaySpace::LinearSrgb => moxcms::ColorProfile::new_srgb(),
        };
        let m = source.transform_matrix(&display).v;
        DecodedImage::from_scene_linear_rgba16f(
            values.len() as u32,
            1,
            strip_f16(&values),
            SceneLinearParams {
                source_to_display: m.map(|row| row.map(|v| v as f32)),
                content_peak: 6.0,
                luminance_weights: [0.2627, 0.6780, 0.0593],
            },
        )
    }

    /// The tone map on the GPU: headrooms well under the content peak (the knee bites), at it,
    /// and above it (only the primaries change).
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_3_matches_the_reference() {
        let image = scene_linear();
        assert_eq!(image.encoding.shader_mode() as u32, 3);
        assert_matches_reference(&image, &[1.0, 1.5, 2.0, 4.0, 6.0, 16.0]);
        let brightest = |headroom| {
            render(&image, headroom)
                .into_iter()
                .flatten()
                .fold(0.0f32, f32::max)
        };
        let (fits, squeezed) = (brightest(16.0), brightest(2.0));
        assert!(fits > 6.0, "content that fits keeps its highlights: {fits}");
        // The curve bounds luminance, not channels: the strip's saturated orange keeps its red
        // above the headroom, as the RAW tone curve's does.
        assert!(
            squeezed < fits / 2.0,
            "the tone map brings them down toward the headroom: {squeezed} vs {fits}"
        );
    }

    /// On an scRGB EDR surface (1.0 = 80 nits) every mode's output is scaled so SDR white lands
    /// on the desktop's SDR white: 250 nits here, 3.125 units. The headroom stays in SDR-white
    /// units, so the gain and the tone map don't move.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn an_edr_surface_scales_every_mode_to_sdr_white() {
        let scale = 250.0 / 80.0;
        let raw = DecodedImage::from_linear_hdr_rgba16f(4, 1, strip_f16(&[0.0, 0.18, 1.0, 4.0]));
        for image in [raw, gain_mapped(), scene_linear()] {
            let plain = render(&image, 4.0);
            let scaled = render_onto(&image, FrameUniform::new(4.0, scale), true);
            for (want, got) in plain.iter().flatten().zip(scaled.iter().flatten()) {
                let want = want * scale;
                assert!(
                    (got - want).abs() <= 1e-3 + want.abs() * 2e-3,
                    "{:?}: {got} vs {want}",
                    image.encoding.shader_mode()
                );
            }
        }
    }

    /// An HDR image on a surface that stayed SDR (macOS refusing EDR, Windows' DX12 to Vulkan
    /// fallback) draws its headroom-1 rendition, whatever the display's headroom says: the gain
    /// map's SDR base and the scene-linear tone map onto SDR white, where the live headroom would
    /// clip at the surface's 1.0.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn an_sdr_surface_shows_the_sdr_rendition_at_any_headroom() {
        for image in [gain_mapped(), scene_linear()] {
            let want = render(&image, 1.0);
            let got = render_onto(&image, FrameUniform::new(8.0, 3.0), false);
            assert_eq!(got, want, "{:?}", image.encoding.shader_mode());
        }
    }

    /// At headroom 1 a gain-mapped image is its SDR base. On macOS, where the HDR display space
    /// is Display P3 itself, that's the same bits mode 0 draws from the same texture.
    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux CI runners have no GPU adapter; macOS and Windows run this"
    )]
    fn mode_2_at_headroom_1_is_the_sdr_base() {
        let image = gain_mapped();
        assert_matches_reference(&image, &[1.0]);
        if crate::color::profiles::HdrDisplaySpace::for_host()
            == crate::color::profiles::HdrDisplaySpace::LinearDisplayP3
        {
            let PixelBuffer::Rgba8(base) = &image.pixels else {
                unreachable!()
            };
            let plain = DecodedImage::from_rgba8(image.width, image.height, base.clone());
            assert_eq!(render(&image, 1.0), render(&plain, 1.0));
        }
    }
}

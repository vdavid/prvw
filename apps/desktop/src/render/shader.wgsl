struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
};

struct Transform {
    // Column 0: scale_x, 0
    // Column 1: 0, scale_y
    // Column 2: translate_x, translate_y
    // Packed as vec4 + vec2 for alignment
    col0: vec4<f32>,  // (scale_x, 0, 0, scale_y)
    col1: vec4<f32>,  // (translate_x, translate_y, fade, 0)
};

@group(0) @binding(0)
var<uniform> transform: Transform;

@group(0) @binding(1)
var t_diffuse: texture_2d<f32>;

@group(0) @binding(2)
var s_diffuse: sampler;

// Per-image (group 0, so a crossfade's outgoing image keeps its own). Rust side:
// `render::image_pipeline::ImageParams`; append fields there and here together.
struct ImageParams {
    mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

// The image's gain map, or a 1x1 placeholder. `Rgba8Unorm`, so samples are raw map values.
@group(0) @binding(3)
var t_gain_map: texture_2d<f32>;

@group(0) @binding(4)
var<uniform> image: ImageParams;

// Per frame (group 1, one for the whole renderer). Rust side: `render::image_pipeline::FrameUniform`.
struct Frame {
    // The display's current headroom over SDR white, >= 1.0.
    headroom_now: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(1) @binding(0)
var<uniform> frame: Frame;

// Mode numbers mirror `color::hdr::fragment::ShaderMode`, which is also the Rust reference for
// every branch below. The GPU parity tests in `render/image_pipeline.rs` keep the two in step.
const MODE_DISPLAY_SDR: u32 = 0u;
const MODE_LINEAR_HDR_DISPLAY: u32 = 1u;

// Full-screen quad vertices (two triangles), generated from vertex index.
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Triangle strip positions for a quad: 0,1,2 and 2,1,3
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );

    var tex_coords = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );

    let pos = positions[vertex_index];
    let scale = vec2<f32>(transform.col0.x, transform.col0.w);
    let translate = vec2<f32>(transform.col1.x, transform.col1.y);
    let transformed = pos * scale + translate;

    var output: VertexOutput;
    output.position = vec4<f32>(transformed, 0.0, 1.0);
    output.tex_coords = tex_coords[vertex_index];
    return output;
}

// Checkerboard pattern for transparent image regions (Photoshop-style).
// Computed in screen-space so it doesn't move when zooming/panning.
fn checkerboard(screen_pos: vec2<f32>) -> vec3<f32> {
    let square_size = 8.0;
    let checker = (floor(screen_pos.x / square_size) + floor(screen_pos.y / square_size)) % 2.0;
    // Light gray / dark gray, like Photoshop
    return select(vec3<f32>(0.4, 0.4, 0.4), vec3<f32>(0.25, 0.25, 0.25), checker == 0.0);
}

// The mode's colour math, before the mode-independent checkerboard and crossfade.
fn shade(sample: vec4<f32>, uv: vec2<f32>) -> vec3<f32> {
    switch image.mode {
        // Display-encoded SDR (either storage) and linear HDR in the display's space: the
        // texture's own decode already produced what the surface wants.
        case MODE_DISPLAY_SDR, MODE_LINEAR_HDR_DISPLAY: {
            return sample.rgb;
        }
        // 2u: gain map (M3), sampling `t_gain_map` at `uv` and weighting by
        // `frame.headroom_now`. 3u: scene-linear tone map (M4) down to `frame.headroom_now`.
        default: {
            return sample.rgb;
        }
    }
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_diffuse, s_diffuse, input.tex_coords);
    let shaded = shade(color, input.tex_coords);
    // `fade` is 1.0 in normal rendering. During a slideshow crossfade the
    // incoming image draws with fade < 1.0 so it alpha-blends over the
    // outgoing image (drawn first, fade = 1.0).
    let fade = transform.col1.z;
    var rgb: vec3<f32>;
    if color.a >= 1.0 {
        rgb = shaded;
    } else {
        // Composite image over the checkerboard for transparent regions.
        let bg = checkerboard(input.position.xy);
        rgb = shaded * color.a + bg * (1.0 - color.a);
    }
    return vec4<f32>(rgb, fade);
}

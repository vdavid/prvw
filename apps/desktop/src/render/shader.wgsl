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
    // Mode 2, per channel in xyz (`color::hdr::gain_map::ChannelParams`).
    gain_min: vec4<f32>,
    gain_max: vec4<f32>,
    gamma: vec4<f32>,
    base_offset: vec4<f32>,
    alternate_offset: vec4<f32>,
    // Mode 2: x the base rendition's headroom, y the alternate's, both log2.
    headroom: vec4<f32>,
    // Mode 2's matrices (`GainMapSpaces`), one row per vec4, xyz used.
    p3_to_app: array<vec4<f32>, 3>,
    app_to_display: array<vec4<f32>, 3>,
    p3_to_display: array<vec4<f32>, 3>,
    // Mode 3 (`color::hdr::tone_map::SceneLinearParams`): the source primaries into the HDR
    // display space, one row per vec4, and the source's luminance weights (xyz) with the content
    // peak (w).
    source_to_display: array<vec4<f32>, 3>,
    scene: vec4<f32>,
};

// The image's gain map, or a 1x1 placeholder. `Rgba8Unorm`, so texels are raw map values. Read
// with `textureLoad`, never the sampler: see `sample_gain`.
@group(0) @binding(3)
var t_gain_map: texture_2d<f32>;

@group(0) @binding(4)
var<uniform> image: ImageParams;

// Per frame (group 1, one for the whole renderer). Rust side: `render::image_pipeline::FrameUniform`.
struct Frame {
    // The display's current headroom over SDR white, >= 1.0.
    headroom_now: f32,
    // SDR white in the surface's units: 1.0 except on an scRGB EDR surface, where 1.0 is 80 nits
    // and SDR white is the user's SDR content brightness (`DisplayHdr::sdr_white_scale`).
    sdr_white_scale: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(1) @binding(0)
var<uniform> frame: Frame;

// Mode numbers mirror `color::hdr::fragment::ShaderMode`, which is also the Rust reference for
// every branch below. The GPU parity tests in `render/image_pipeline.rs` keep the two in step.
const MODE_DISPLAY_SDR: u32 = 0u;
const MODE_LINEAR_HDR_DISPLAY: u32 = 1u;
const MODE_GAIN_MAP: u32 = 2u;
const MODE_SCENE_LINEAR: u32 = 3u;

// ── Mode 2: the gain map. Each function ports its namesake in `color::hdr::gain_map`. ──

fn rows_times(m: array<vec4<f32>, 3>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m[0].xyz, v), dot(m[1].xyz, v), dot(m[2].xyz, v));
}

// How much of the gain applies at the current headroom: `ultrahdr-core`'s `calculate_weight` for
// an SDR base, libavif's formula for an HDR one.
fn gain_weight() -> f32 {
    let h = log2(max(frame.headroom_now, 1.0));
    let base = image.headroom.x;
    let alternate = image.headroom.y;
    if base > alternate {
        return clamp((h - base) / (alternate - base), 0.0, 1.0);
    }
    let lo = max(base, 0.0);
    let hi = max(alternate, 0.0);
    if hi <= lo {
        return 1.0;
    }
    return clamp((h - lo) / (hi - lo), 0.0, 1.0);
}

// The linear gain one map texel stands for, per channel (`GainMapLut::new`'s formula).
fn texel_gain(v: vec3<f32>, w: f32) -> vec3<f32> {
    let gamma = image.gamma.xyz;
    // pow(0, y) is undefined in WGSL even for y > 0, so a zero code stays zero explicitly.
    let powed = select(pow(v, 1.0 / gamma), vec3<f32>(0.0), v <= vec3<f32>(0.0));
    let use_gamma = (gamma != vec3<f32>(1.0)) & (gamma > vec3<f32>(0.0));
    let recovery = select(v, powed, use_gamma);
    let log2_gain = image.gain_min.xyz + recovery * (image.gain_max.xyz - image.gain_min.xyz);
    return exp2(clamp(log2_gain * w, vec3<f32>(-30.0), vec3<f32>(30.0)));
}

// Shepard's inverse-distance weighting of the four neighbouring texels' linear gains, with the
// map's texels top-left aligned to the image's pixels. Four `textureLoad`s, because hardware
// bilinear filtering of the encoded values is a different function (spec Findings, M3).
fn sample_gain(uv: vec2<f32>, w: f32) -> vec3<f32> {
    let image_dims = textureDimensions(t_diffuse);
    let image_size = vec2<f32>(image_dims);
    let map_dims = textureDimensions(t_gain_map);
    // The weighting below jumps at a cell edge, so a pixel a hair on the wrong side of one takes
    // the wrong texels. A fragment within 1/64 px of a pixel centre (`PIXEL_SNAP`) is that pixel,
    // and its cell comes from integer arithmetic; the interpolated coordinate and this GPU's
    // division would both round across the edge.
    let raw = uv * image_size - 0.5;
    let nearest = round(raw);
    let on_pixel = abs(raw - nearest) < vec2<f32>(1.0 / 64.0);
    let scaled = vec2<u32>(max(nearest, vec2<f32>(0.0))) * map_dims;
    let at = max(raw, vec2<f32>(0.0)) * vec2<f32>(map_dims) / image_size;
    let cell = select(vec2<u32>(floor(at)), scaled / image_dims, on_pixel);
    let fr = select(at - floor(at), vec2<f32>(scaled % image_dims) / image_size, on_pixel);
    let i0 = min(cell, map_dims - 1u);
    let i1 = min(i0 + 1u, map_dims - 1u);

    var weights = vec4<f32>(1.0, 0.0, 0.0, 0.0);
    let d_tl = length(fr);
    if d_tl != 0.0 {
        // A neighbour clamped onto the same texel at the edge moves its distance, as
        // `ultrahdr-core`'s edge tables do.
        let far = fr - vec2<f32>(i1 - i0);
        let d = vec4<f32>(d_tl, length(vec2<f32>(fr.x, far.y)), length(vec2<f32>(far.x, fr.y)), length(far));
        let inverse = 1.0 / d;
        weights = inverse / (inverse.x + inverse.y + inverse.z + inverse.w);
    }
    let tl = texel_gain(textureLoad(t_gain_map, vec2<u32>(i0.x, i0.y), 0).rgb, w);
    let bl = texel_gain(textureLoad(t_gain_map, vec2<u32>(i0.x, i1.y), 0).rgb, w);
    let tr = texel_gain(textureLoad(t_gain_map, vec2<u32>(i1.x, i0.y), 0).rgb, w);
    let br = texel_gain(textureLoad(t_gain_map, vec2<u32>(i1.x, i1.y), 0).rgb, w);
    return tl * weights.x + bl * weights.y + tr * weights.z + br * weights.w;
}

fn shade_gain_map(sdr: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let w = gain_weight();
    // No gain at all: the base rendition as it is, with no offsets nudging it.
    if w == 0.0 {
        return rows_times(image.p3_to_display, sdr);
    }
    let x = rows_times(image.p3_to_app, sdr);
    let y = (x + image.base_offset.xyz) * sample_gain(uv, w) - image.alternate_offset.xyz;
    return rows_times(image.app_to_display, y);
}

// ── Mode 3: scene-linear tone map. Each function ports its namesake in `color::hdr::transfer`
// and `color::hdr::tone_map`. ──

const REFERENCE_WHITE_NITS: f32 = 203.0;
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

fn pq_eotf(signal: f32) -> f32 {
    let p = pow(max(signal, 0.0), 1.0 / PQ_M2);
    let ratio = max(p - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * p);
    // pow(0, y) is undefined in WGSL, so black stays black explicitly.
    return select(10000.0 * pow(ratio, 1.0 / PQ_M1), 0.0, ratio <= 0.0);
}

fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = select(pow(max(nits, 0.0) / 10000.0, PQ_M1), 0.0, nits <= 0.0);
    return pow((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y), PQ_M2);
}

// BT.2390's EETF on a luminance relative to SDR white, from `content_peak` down to `target_peak`.
fn eetf(y: f32, content_peak: f32, target_peak: f32) -> f32 {
    if content_peak <= target_peak || y <= 0.0 {
        return y;
    }
    let source_pq = pq_inverse_eotf(content_peak * REFERENCE_WHITE_NITS);
    let max_lum = pq_inverse_eotf(target_peak * REFERENCE_WHITE_NITS) / source_pq;
    let start = max(1.5 * max_lum - 0.5, 0.0);
    let e1 = min(pq_inverse_eotf(y * REFERENCE_WHITE_NITS) / source_pq, 1.0);
    var e2 = e1;
    if e1 >= start {
        let t = (e1 - start) / (1.0 - start);
        let t2 = t * t;
        let t3 = t2 * t;
        e2 = min(
            (2.0 * t3 - 3.0 * t2 + 1.0) * start + (t3 - 2.0 * t2 + t) * (1.0 - start)
                + (-2.0 * t3 + 3.0 * t2) * max_lum,
            max_lum,
        );
    }
    return pq_eotf(e2 * source_pq) / REFERENCE_WHITE_NITS;
}

fn shade_scene_linear(source: vec3<f32>) -> vec3<f32> {
    let y = dot(image.scene.xyz, source);
    var ratio = 1.0;
    if y > 0.0 {
        ratio = eetf(y, image.scene.w, max(frame.headroom_now, 1.0)) / y;
    }
    return rows_times(image.source_to_display, source * ratio);
}

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
        // A linear Display P3 base (sRGB texture decode) plus its gain map.
        case MODE_GAIN_MAP: {
            return shade_gain_map(sample.rgb, uv);
        }
        // Linear light in the source primaries, tone-mapped down to the current headroom.
        case MODE_SCENE_LINEAR: {
            return shade_scene_linear(sample.rgb);
        }
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
    // Every mode works in SDR-white units; this puts them in the surface's. A surface that can't
    // show HDR binds the frame with 1.0 here (`ImagePipeline::frame_bind_group_for`).
    return vec4<f32>(rgb * frame.sdr_white_scale, fade);
}

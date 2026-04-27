// Histogram bar shader.
//
// Draws three RGB curves into a rect using additive blending. The fragment
// position is mapped to a bin index along X and a normalized count along Y;
// each channel contributes if its normalized count covers the current Y.
//
// Uniform:
//   - rect: (x, y, w, h) in physical pixels
//   - params: (max_count_recip, screen_w, screen_h, edge_softness_px)
// Storage:
//   - counts: 768 u32s, R[0..256], G[256..512], B[512..768]

struct Params {
    rect: vec4<f32>,
    params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> u: Params;

@group(0) @binding(1)
var<storage, read> counts: array<u32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    return vec4(positions[vi], 0.0, 1.0);
}

fn channel_alpha(count_u: u32, max_recip: f32, ny: f32, edge: f32) -> f32 {
    let count = f32(count_u);
    let h = clamp(count * max_recip, 0.0, 1.0);
    // Filled below `h`. Soft edge of `edge` (in normalized Y units) for AA.
    return clamp((h - ny) / edge, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    let rx = u.rect.x;
    let ry = u.rect.y;
    let rw = u.rect.z;
    let rh = u.rect.w;

    let local_x = frag_pos.x - rx;
    let local_y = frag_pos.y - ry;
    if local_x < 0.0 || local_x >= rw || local_y < 0.0 || local_y >= rh {
        discard;
    }

    // Bin index 0..255.
    let bin_f = clamp(local_x / rw * 256.0, 0.0, 255.999);
    let bin = u32(bin_f);

    // Normalized Y, 0 at the bottom of the rect, 1 at the top.
    let ny = 1.0 - (local_y / rh);
    let max_recip = u.params.x;
    // Edge softness in normalized Y units. `params.w` carries the softness
    // expressed in physical pixels; convert to fraction of `rh`.
    let edge = max(u.params.w / rh, 1.0 / rh);

    let r_a = channel_alpha(counts[bin], max_recip, ny, edge);
    let g_a = channel_alpha(counts[256u + bin], max_recip, ny, edge);
    let b_a = channel_alpha(counts[512u + bin], max_recip, ny, edge);

    // Additive-style blend across channels: each channel contributes its own
    // alpha-weighted color. Composite the three together as one RGBA so the
    // pipeline blend mode (alpha blending) lays it nicely over the backdrop.
    let strength = 0.78;
    let col_r = vec3<f32>(1.0, 0.30, 0.30) * r_a;
    let col_g = vec3<f32>(0.36, 0.95, 0.45) * g_a;
    let col_b = vec3<f32>(0.42, 0.62, 1.0)  * b_a;

    let rgb = (col_r + col_g + col_b) * strength;
    let a = max(max(r_a, g_a), b_a) * 0.92;

    if a < 0.005 {
        discard;
    }

    return vec4(rgb, a);
}

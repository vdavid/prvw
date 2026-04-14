// Renders a semi-transparent rounded rectangle (pill) for text backgrounds.
// The rect is specified in pixel coordinates via a uniform.

struct Rect {
    // (x, y, width, height) in physical pixels
    pos: vec4<f32>,
    // RGBA color, each component 0..1
    color: vec4<f32>,
    // (corner_radius, screen_width, screen_height, 0)
    params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> rect: Rect;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Full-screen triangle pair — the fragment shader does the clipping via SDF
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    return vec4(positions[vi], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    let rx = rect.pos.x;
    let ry = rect.pos.y;
    let rw = rect.pos.z;
    let rh = rect.pos.w;
    let radius = rect.params.x;

    // Pixel position relative to the rect center
    let center = vec2(rx + rw / 2.0, ry + rh / 2.0);
    let half_size = vec2(rw / 2.0, rh / 2.0);
    let p = frag_pos.xy - center;

    // Signed distance to a rounded rectangle
    let q = abs(p) - half_size + vec2(radius);
    let dist = min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - radius;

    // Smooth edge (1px anti-aliasing)
    let alpha = 1.0 - smoothstep(-0.5, 0.5, dist);

    if alpha < 0.001 {
        discard;
    }

    return vec4(rect.color.rgb, rect.color.a * alpha);
}

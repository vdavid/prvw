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
    col1: vec4<f32>,  // (translate_x, translate_y, 0, 0)
};

@group(0) @binding(0)
var<uniform> transform: Transform;

@group(0) @binding(1)
var t_diffuse: texture_2d<f32>;

@group(0) @binding(2)
var s_diffuse: sampler;

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

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_diffuse, s_diffuse, input.tex_coords);
    if color.a >= 1.0 {
        return color;
    }
    // Blend image over checkerboard using standard alpha compositing
    let bg = checkerboard(input.position.xy);
    let blended = color.rgb * color.a + bg * (1.0 - color.a);
    return vec4<f32>(blended, 1.0);
}

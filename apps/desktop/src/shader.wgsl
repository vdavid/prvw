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

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_diffuse, s_diffuse, input.tex_coords);
}

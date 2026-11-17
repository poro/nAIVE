// Equivalent precompiled artifact for smoke runs without the Slang SDK.
// --slang deletes this source artifact before building the bundle.
@group(0) @binding(0) var ldr_texture: texture_2d<f32>;
@group(0) @binding(1) var ldr_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertexIndex: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((vertexIndex << 1u) & 2u), f32(vertexIndex & 2u));
    var output: VertexOutput;
    output.position = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    output.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if (input.uv.x < 0.2) { return vec4<f32>(0.0, 1.0, 1.0, 1.0); }
    if (input.uv.x > 0.8) { return vec4<f32>(1.0, 0.0, 1.0, 1.0); }
    return textureSample(ldr_texture, ldr_sampler, input.uv);
}

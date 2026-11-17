// Deliberately outside shaders/: pipeline references determine required shaders.
import release_palette;

[[vk::binding(0, 0)]] Texture2D<float4> ldr_texture;
[[vk::binding(1, 0)]] SamplerState ldr_sampler;

struct VertexOutput {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

[shader("vertex")]
VertexOutput vs_main(uint vertexIndex : SV_VertexID) {
    VertexOutput output;
    float2 uv = float2(float((vertexIndex << 1) & 2), float(vertexIndex & 2));
    output.position = float4(uv * 2.0 - 1.0, 0.0, 1.0);
    output.uv = float2(uv.x, 1.0 - uv.y);
    return output;
}

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    if (input.uv.x < 0.2) return releaseMarkerColor(true);
    if (input.uv.x > 0.8) return releaseMarkerColor(false);
    return ldr_texture.Sample(ldr_sampler, input.uv);
}

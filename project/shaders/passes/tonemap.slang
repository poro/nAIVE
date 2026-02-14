// Tone mapping pass: HDR -> SDR via ACES + bloom composite + vignette + chromatic aberration

[[vk::binding(0, 0)]] Texture2D<float4> hdr_texture;
[[vk::binding(1, 0)]] SamplerState      hdr_sampler;
[[vk::binding(2, 0)]] Texture2D<float4> bloom_texture;

struct VertexOutput {
    float4 position : SV_Position;
    float2 uv       : TEXCOORD0;
};

[shader("vertex")]
VertexOutput vs_main(uint vertexIndex : SV_VertexID) {
    VertexOutput output;
    float2 uv = float2(float((vertexIndex << 1) & 2), float(vertexIndex & 2));
    output.position = float4(uv * 2.0 - 1.0, 0.0, 1.0);
    output.uv = float2(uv.x, 1.0 - uv.y);
    return output;
}

float3 acesTonemap(float3 x) {
    float a = 2.51;
    float b = 0.03;
    float c = 2.43;
    float d = 0.59;
    float e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), float3(0.0), float3(1.0));
}

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    float2 uv = input.uv;
    float2 center_offset = uv - float2(0.5, 0.5);
    float dist_from_center = length(center_offset);

    // Chromatic aberration: offset R and B channels outward from center
    float ca_strength = 0.002;
    float2 ca_offset = center_offset * ca_strength;
    float r = hdr_texture.Sample(hdr_sampler, uv - ca_offset).r;
    float g = hdr_texture.Sample(hdr_sampler, uv).g;
    float b = hdr_texture.Sample(hdr_sampler, uv + ca_offset).b;
    float3 hdr_color = float3(r, g, b);

    // Add bloom (bilinear-filtered from half-res gives natural soft glow)
    float3 bloom = bloom_texture.Sample(hdr_sampler, uv).rgb;
    float bloom_strength = 0.7;
    hdr_color += bloom * bloom_strength;

    // ACES tonemap
    float3 sdr_color = acesTonemap(hdr_color);

    // Vignette: smooth darkening at edges
    float vignette = 1.0 - smoothstep(0.4, 0.9, dist_from_center);
    sdr_color *= vignette;

    return float4(sdr_color, 1.0);
}

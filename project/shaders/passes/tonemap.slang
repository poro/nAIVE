// Tone mapping pass: HDR -> SDR via ACES

[[vk::binding(0, 0)]] Texture2D<float4> hdr_texture;
[[vk::binding(1, 0)]] SamplerState      hdr_sampler;

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
    float3 hdr_color = hdr_texture.Sample(hdr_sampler, input.uv).rgb;
    float3 sdr_color = acesTonemap(hdr_color);
    return float4(sdr_color, 1.0);
}

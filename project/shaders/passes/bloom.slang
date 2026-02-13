// Bloom extraction pass: threshold bright pixels + 13-tap tent filter downsample
// Reads HDR at full resolution, outputs to half-resolution bloom buffer

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

float luminance(float3 c) {
    return dot(c, float3(0.2126, 0.7152, 0.0722));
}

// Soft knee threshold: smoothly ramps from 0 at threshold to 1 above threshold+knee
float3 threshold_color(float3 color, float threshold, float knee) {
    float lum = luminance(color);
    float soft = lum - threshold + knee;
    soft = clamp(soft, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 0.0001);
    float contribution = max(soft, lum - threshold) / max(lum, 0.0001);
    return color * max(contribution, 0.0);
}

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    // 13-tap tent filter (Jimenez 2014 — Call of Duty bloom)
    // Texel size for the source (full-res HDR) texture
    uint2 dims;
    hdr_texture.GetDimensions(dims.x, dims.y);
    float2 texel = 1.0 / float2(dims);

    float2 uv = input.uv;

    // 13-tap pattern: 4 corner samples + 4 edge samples + 1 center + 4 diagonal near
    float3 a = hdr_texture.Sample(hdr_sampler, uv + float2(-2, -2) * texel).rgb;
    float3 b = hdr_texture.Sample(hdr_sampler, uv + float2( 0, -2) * texel).rgb;
    float3 c = hdr_texture.Sample(hdr_sampler, uv + float2( 2, -2) * texel).rgb;
    float3 d = hdr_texture.Sample(hdr_sampler, uv + float2(-1, -1) * texel).rgb;
    float3 e = hdr_texture.Sample(hdr_sampler, uv).rgb;
    float3 f = hdr_texture.Sample(hdr_sampler, uv + float2( 1, -1) * texel).rgb;
    float3 g = hdr_texture.Sample(hdr_sampler, uv + float2(-2,  0) * texel).rgb;
    float3 h = hdr_texture.Sample(hdr_sampler, uv + float2( 2,  0) * texel).rgb;
    float3 i = hdr_texture.Sample(hdr_sampler, uv + float2(-1,  1) * texel).rgb;
    float3 j = hdr_texture.Sample(hdr_sampler, uv + float2( 1,  1) * texel).rgb;
    float3 k = hdr_texture.Sample(hdr_sampler, uv + float2(-2,  2) * texel).rgb;
    float3 l = hdr_texture.Sample(hdr_sampler, uv + float2( 0,  2) * texel).rgb;
    float3 m = hdr_texture.Sample(hdr_sampler, uv + float2( 2,  2) * texel).rgb;

    // Weighted downsample (anti-firefly: weighted by luminance groups)
    float3 result = e * 0.125;                                    // center
    result += (d + f + i + j) * 0.125;                           // inner diamond
    result += (a + c + k + m) * 0.03125;                         // corners
    result += (b + g + h + l) * 0.0625;                          // edges

    // Apply bloom threshold with soft knee
    result = threshold_color(result, 0.8, 0.5);

    return float4(result, 1.0);
}

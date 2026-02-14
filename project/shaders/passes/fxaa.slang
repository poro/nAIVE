// FXAA 3.11 — Fast Approximate Anti-Aliasing (Timothy Lottes / NVIDIA)
// Reads LDR buffer, outputs anti-aliased image to swapchain

[[vk::binding(0, 0)]] Texture2D<float4> ldr_texture;
[[vk::binding(1, 0)]] SamplerState      ldr_sampler;

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

float fxaaLuma(float3 c) {
    return dot(c, float3(0.299, 0.587, 0.114));
}

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    uint2 dims;
    ldr_texture.GetDimensions(dims.x, dims.y);
    float2 texel = 1.0 / float2(dims);
    float2 uv = input.uv;

    // Sample center and cardinal neighbors
    float3 rgbM = ldr_texture.Sample(ldr_sampler, uv).rgb;
    float3 rgbN = ldr_texture.Sample(ldr_sampler, uv + float2(0.0, -texel.y)).rgb;
    float3 rgbS = ldr_texture.Sample(ldr_sampler, uv + float2(0.0,  texel.y)).rgb;
    float3 rgbW = ldr_texture.Sample(ldr_sampler, uv + float2(-texel.x, 0.0)).rgb;
    float3 rgbE = ldr_texture.Sample(ldr_sampler, uv + float2( texel.x, 0.0)).rgb;

    float lumaM = fxaaLuma(rgbM);
    float lumaN = fxaaLuma(rgbN);
    float lumaS = fxaaLuma(rgbS);
    float lumaW = fxaaLuma(rgbW);
    float lumaE = fxaaLuma(rgbE);

    float lumaMin = min(lumaM, min(min(lumaN, lumaS), min(lumaW, lumaE)));
    float lumaMax = max(lumaM, max(max(lumaN, lumaS), max(lumaW, lumaE)));
    float lumaRange = lumaMax - lumaMin;

    // FXAA quality thresholds — skip anti-aliasing on low-contrast regions
    float FXAA_EDGE_THRESHOLD = 0.125;
    float FXAA_EDGE_THRESHOLD_MIN = 0.0625;

    if (lumaRange < max(FXAA_EDGE_THRESHOLD_MIN, lumaMax * FXAA_EDGE_THRESHOLD)) {
        return float4(rgbM, 1.0);
    }

    // Sample diagonal neighbors for edge direction detection
    float3 rgbNW = ldr_texture.Sample(ldr_sampler, uv + float2(-texel.x, -texel.y)).rgb;
    float3 rgbNE = ldr_texture.Sample(ldr_sampler, uv + float2( texel.x, -texel.y)).rgb;
    float3 rgbSW = ldr_texture.Sample(ldr_sampler, uv + float2(-texel.x,  texel.y)).rgb;
    float3 rgbSE = ldr_texture.Sample(ldr_sampler, uv + float2( texel.x,  texel.y)).rgb;

    float lumaNW = fxaaLuma(rgbNW);
    float lumaNE = fxaaLuma(rgbNE);
    float lumaSW = fxaaLuma(rgbSW);
    float lumaSE = fxaaLuma(rgbSE);

    // Edge direction: horizontal vs vertical Sobel-like filter
    float edgeH = abs(-2.0 * lumaW + lumaNW + lumaSW) +
                  abs(-2.0 * lumaM + lumaN + lumaS) * 2.0 +
                  abs(-2.0 * lumaE + lumaNE + lumaSE);
    float edgeV = abs(-2.0 * lumaN + lumaNW + lumaNE) +
                  abs(-2.0 * lumaM + lumaW + lumaE) * 2.0 +
                  abs(-2.0 * lumaS + lumaSW + lumaSE);

    bool isHorizontal = (edgeH >= edgeV);

    // Subpixel blend factor — smooths sub-pixel aliasing
    float lumaL = (lumaN + lumaS + lumaW + lumaE) * 0.25;
    float rangeL = abs(lumaL - lumaM);
    float blendL = max(0.0, (rangeL / lumaRange) - 0.25);
    blendL = min(0.75, blendL * 1.3333);

    // Blend perpendicular to detected edge
    float2 blend_dir;
    if (isHorizontal) {
        if (abs(lumaN - lumaM) >= abs(lumaS - lumaM)) {
            blend_dir = float2(0.0, -texel.y);
        } else {
            blend_dir = float2(0.0, texel.y);
        }
    } else {
        if (abs(lumaW - lumaM) >= abs(lumaE - lumaM)) {
            blend_dir = float2(-texel.x, 0.0);
        } else {
            blend_dir = float2(texel.x, 0.0);
        }
    }

    float3 result = ldr_texture.Sample(ldr_sampler, uv + blend_dir * blendL).rgb;
    return float4(result, 1.0);
}

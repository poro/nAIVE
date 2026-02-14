// FXAA 3.11 — Fast Approximate Anti-Aliasing (Timothy Lottes / NVIDIA)
// Full implementation with edge endpoint search

[[vk::binding(0, 0)]] Texture2D<float4> ldr_texture;
[[vk::binding(1, 0)]] SamplerState      ldr_sampler;

static const float FXAA_EDGE_THRESHOLD = 0.063;
static const float FXAA_EDGE_THRESHOLD_MIN = 0.0312;
static const int   FXAA_SEARCH_STEPS = 8;
static const float FXAA_SEARCH_THRESHOLD = 0.25;
static const float FXAA_SUBPIX_QUALITY = 0.75;

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

    if (lumaRange < max(FXAA_EDGE_THRESHOLD_MIN, lumaMax * FXAA_EDGE_THRESHOLD)) {
        return float4(rgbM, 1.0);
    }

    // Sample diagonal neighbors
    float3 rgbNW = ldr_texture.Sample(ldr_sampler, uv + float2(-texel.x, -texel.y)).rgb;
    float3 rgbNE = ldr_texture.Sample(ldr_sampler, uv + float2( texel.x, -texel.y)).rgb;
    float3 rgbSW = ldr_texture.Sample(ldr_sampler, uv + float2(-texel.x,  texel.y)).rgb;
    float3 rgbSE = ldr_texture.Sample(ldr_sampler, uv + float2( texel.x,  texel.y)).rgb;

    float lumaNW = fxaaLuma(rgbNW);
    float lumaNE = fxaaLuma(rgbNE);
    float lumaSW = fxaaLuma(rgbSW);
    float lumaSE = fxaaLuma(rgbSE);

    float lumaNS = lumaN + lumaS;
    float lumaWE = lumaW + lumaE;

    // Subpixel aliasing detection
    float lumaL = (lumaNS + lumaWE) * 0.25;
    float rangeL = abs(lumaL - lumaM);
    float subpixBlend = max(0.0, (rangeL / lumaRange) - FXAA_SEARCH_THRESHOLD);
    subpixBlend = min(FXAA_SUBPIX_QUALITY, subpixBlend * subpixBlend * (3.0 - 2.0 * subpixBlend));

    // Edge direction detection
    float lumaNWSW = lumaNW + lumaSW;
    float lumaNESE = lumaNE + lumaSE;
    float lumaNWNE = lumaNW + lumaNE;
    float lumaSWSE = lumaSW + lumaSE;

    float edgeH = abs(lumaNWSW - 2.0 * lumaW) +
                  abs(lumaNS - 2.0 * lumaM) * 2.0 +
                  abs(lumaNESE - 2.0 * lumaE);
    float edgeV = abs(lumaNWNE - 2.0 * lumaN) +
                  abs(lumaWE - 2.0 * lumaM) * 2.0 +
                  abs(lumaSWSE - 2.0 * lumaS);

    bool isHorizontal = (edgeH >= edgeV);

    // Select edge normal direction (perpendicular to edge)
    float luma1 = isHorizontal ? lumaN : lumaW;
    float luma2 = isHorizontal ? lumaS : lumaE;
    float grad1 = luma1 - lumaM;
    float grad2 = luma2 - lumaM;
    bool is1Steepest = abs(grad1) >= abs(grad2);

    float gradScaled = 0.25 * max(abs(grad1), abs(grad2));

    // Step size along the edge (tangent direction)
    float stepLength = isHorizontal ? texel.y : texel.x;
    float lumaLocalAvg;

    if (is1Steepest) {
        stepLength = -stepLength;
        lumaLocalAvg = 0.5 * (luma1 + lumaM);
    } else {
        lumaLocalAvg = 0.5 * (luma2 + lumaM);
    }

    // Shift UV to the edge boundary (half a texel perpendicular)
    float2 edgeUV = uv;
    if (isHorizontal) {
        edgeUV.y += stepLength * 0.5;
    } else {
        edgeUV.x += stepLength * 0.5;
    }

    // Edge tangent direction (along the edge)
    float2 edgeStep = isHorizontal ? float2(texel.x, 0.0) : float2(0.0, texel.y);

    // Search along edge in positive direction
    float2 uvP = edgeUV + edgeStep;
    float lumaEndP = fxaaLuma(ldr_texture.Sample(ldr_sampler, uvP).rgb) - lumaLocalAvg;
    bool doneP = abs(lumaEndP) >= gradScaled;

    [unroll]
    for (int i = 1; i < FXAA_SEARCH_STEPS && !doneP; i++) {
        uvP += edgeStep;
        lumaEndP = fxaaLuma(ldr_texture.Sample(ldr_sampler, uvP).rgb) - lumaLocalAvg;
        doneP = abs(lumaEndP) >= gradScaled;
    }

    // Search along edge in negative direction
    float2 uvN = edgeUV - edgeStep;
    float lumaEndN = fxaaLuma(ldr_texture.Sample(ldr_sampler, uvN).rgb) - lumaLocalAvg;
    bool doneN = abs(lumaEndN) >= gradScaled;

    [unroll]
    for (int j = 1; j < FXAA_SEARCH_STEPS && !doneN; j++) {
        uvN -= edgeStep;
        lumaEndN = fxaaLuma(ldr_texture.Sample(ldr_sampler, uvN).rgb) - lumaLocalAvg;
        doneN = abs(lumaEndN) >= gradScaled;
    }

    // Compute distances to edge endpoints
    float distP, distN;
    if (isHorizontal) {
        distP = uvP.x - uv.x;
        distN = uv.x - uvN.x;
    } else {
        distP = uvP.y - uv.y;
        distN = uv.y - uvN.y;
    }

    bool isCloserToP = distP < distN;
    float distClosest = min(distP, distN);
    float edgeLength = distP + distN;

    // Is the closer endpoint's luma delta in the same direction as the local gradient?
    float lumaEndClosest = isCloserToP ? lumaEndP : lumaEndN;
    bool goodSpan = (lumaEndClosest < 0.0) != (lumaM - lumaLocalAvg < 0.0);

    // Edge blend factor
    float edgeBlend = goodSpan ? (0.5 - distClosest / edgeLength) : 0.0;

    // Use the maximum of edge blend and subpixel blend
    float finalBlend = max(edgeBlend, subpixBlend);

    // Final sample: blend perpendicular to the edge
    float2 finalUV = uv;
    if (isHorizontal) {
        finalUV.y += stepLength * finalBlend;
    } else {
        finalUV.x += stepLength * finalBlend;
    }

    float3 result = ldr_texture.Sample(ldr_sampler, finalUV).rgb;
    return float4(result, 1.0);
}

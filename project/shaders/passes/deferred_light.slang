// Deferred lighting pass: reads G-buffer textures, evaluates point lights
import camera;

struct PointLight {
    float3 position;
    float range;
    float3 color;
    float intensity;
};

struct LightingUniforms {
    uint light_count;
    uint3 _pad;
    PointLight lights[32];
};

[[vk::binding(0, 0)]] ConstantBuffer<CameraUniform> camera;

[[vk::binding(0, 1)]] Texture2D<float4> gbuffer_albedo;
[[vk::binding(1, 1)]] Texture2D<float4> gbuffer_normal;
[[vk::binding(2, 1)]] Texture2D<float>  gbuffer_depth;
[[vk::binding(3, 1)]] SamplerState      gbuffer_sampler;

[[vk::binding(0, 2)]] ConstantBuffer<LightingUniforms> lighting;

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

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    int2 texCoords = int2(input.position.xy);
    float3 albedo = gbuffer_albedo.Load(int3(texCoords, 0)).rgb;
    float3 normalRaw = gbuffer_normal.Load(int3(texCoords, 0)).rgb;
    float3 normal = normalize(normalRaw * 2.0 - 1.0);
    float depth = gbuffer_depth.Load(int3(texCoords, 0)).r;

    // Skip empty pixels
    if (depth >= 1.0) discard;

    // Ambient light
    float3 color = albedo * float3(0.08, 0.08, 0.1);

    // Accumulate point lights
    for (uint i = 0; i < lighting.light_count; i++) {
        PointLight light = lighting.lights[i];
        float3 lightDir = normalize(light.position);
        float ndotl = max(dot(normal, lightDir), 0.0);
        color += albedo * light.color * ndotl * light.intensity;
    }

    // Default directional light if no lights
    if (lighting.light_count == 0) {
        float3 lightDir = normalize(float3(0.3, 1.0, 0.5));
        float ndotl = max(dot(normal, lightDir), 0.0);
        color = albedo * (float3(0.15, 0.15, 0.18) + ndotl * 0.85);
    }

    return float4(color, 1.0);
}

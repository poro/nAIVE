// Deferred lighting pass: proper point lights with PBR-like shading
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
[[vk::binding(4, 1)]] Texture2D<float4> gbuffer_emission;

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

float3 reconstructWorldPos(float2 uv, float depth) {
    float4 clip = float4(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth, 1.0);
    float4 world_h = mul(camera.inv_view_projection, clip);
    return world_h.xyz / world_h.w;
}

[shader("fragment")]
float4 fs_main(VertexOutput input) : SV_Target0 {
    int2 texCoords = int2(input.position.xy);

    float4 albedoRough = gbuffer_albedo.Load(int3(texCoords, 0));
    float4 normalMetal = gbuffer_normal.Load(int3(texCoords, 0));
    float4 emissionVal = gbuffer_emission.Load(int3(texCoords, 0));
    float depth = gbuffer_depth.Load(int3(texCoords, 0)).r;

    // Skip empty pixels
    if (depth >= 1.0) discard;

    float3 albedo    = albedoRough.rgb;
    float  roughness = albedoRough.a;
    float3 normal    = normalize(normalMetal.rgb * 2.0 - 1.0);
    float  metallic  = normalMetal.a;
    float3 emission  = emissionVal.rgb;

    // Reconstruct world position
    float3 worldPos = reconstructWorldPos(input.uv, depth);
    float3 viewDir  = normalize(camera.position - worldPos);

    // PBR-like base reflectance
    float3 F0 = lerp(float3(0.04, 0.04, 0.04), albedo, metallic);
    float3 diffuseColor = albedo * (1.0 - metallic);

    // Ambient
    float3 color = diffuseColor * float3(0.03, 0.03, 0.04);

    // Accumulate point lights
    for (uint i = 0; i < lighting.light_count; i++) {
        PointLight light = lighting.lights[i];
        float3 toLight = light.position - worldPos;
        float dist = length(toLight);

        if (dist > light.range) continue;

        float3 lightDir = toLight / dist;
        float3 halfVec  = normalize(lightDir + viewDir);

        float ndotl = max(dot(normal, lightDir), 0.0);
        float ndoth = max(dot(normal, halfVec), 0.0);

        // Attenuation: inverse-square with smooth range falloff
        float distAtten = 1.0 / (1.0 + dist * dist);
        float rangeFactor = saturate(1.0 - pow(dist / light.range, 4.0));
        float attenuation = light.intensity * distAtten * rangeFactor;

        // Diffuse
        float3 diffuse = diffuseColor * light.color * ndotl;

        // Blinn-Phong specular (roughness controls exponent)
        float specPower = max(2.0 / (roughness * roughness + 0.001) - 2.0, 1.0);
        float specular = pow(ndoth, specPower);
        float3 spec = F0 * light.color * specular * ndotl;

        color += (diffuse + spec) * attenuation;
    }

    // Fallback directional light when no lights in scene
    if (lighting.light_count == 0) {
        float3 lightDir = normalize(float3(0.3, 1.0, 0.5));
        float ndotl = max(dot(normal, lightDir), 0.0);
        color = diffuseColor * (float3(0.15, 0.15, 0.18) + ndotl * 0.85);
    }

    // Add emission (unlit, straight to output)
    color += emission;

    return float4(color, 1.0);
}

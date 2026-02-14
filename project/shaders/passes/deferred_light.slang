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
    uint has_directional;
    uint2 _pad_a;
    uint4 _pad_b;
    float3 dir_light_direction;
    float dir_light_intensity;
    float3 dir_light_color;
    float _pad_c;
    float4x4 light_vp;
    PointLight lights[32];
};

[[vk::binding(0, 0)]] ConstantBuffer<CameraUniform> camera;

[[vk::binding(0, 1)]] Texture2D<float4> gbuffer_albedo;
[[vk::binding(1, 1)]] Texture2D<float4> gbuffer_normal;
[[vk::binding(2, 1)]] Texture2D<float>  gbuffer_depth;
[[vk::binding(3, 1)]] SamplerState      gbuffer_sampler;
[[vk::binding(4, 1)]] Texture2D<float4> gbuffer_emission;

[[vk::binding(0, 2)]] ConstantBuffer<LightingUniforms> lighting;
[[vk::binding(1, 2)]] Texture2D<float> shadow_map;
[[vk::binding(2, 2)]] SamplerComparisonState shadow_sampler;

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

// Cook-Torrance BRDF helper functions
float distributionGGX(float NdotH, float roughness) {
    float a = roughness * roughness;
    float a2 = a * a;
    float NdotH2 = NdotH * NdotH;
    float denom = NdotH2 * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * denom * denom + 0.0001);
}

float geometrySmith(float NdotV, float NdotL, float roughness) {
    float r = roughness + 1.0;
    float k = (r * r) / 8.0;
    float ggxV = NdotV / (NdotV * (1.0 - k) + k);
    float ggxL = NdotL / (NdotL * (1.0 - k) + k);
    return ggxV * ggxL;
}

float3 fresnelSchlick(float cosTheta, float3 F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

float sampleShadowPCF(float3 worldPos) {
    float4 lightClip = mul(lighting.light_vp, float4(worldPos, 1.0));
    float3 lightNdc = lightClip.xyz / lightClip.w;
    float2 shadowUV = float2(lightNdc.x * 0.5 + 0.5, -lightNdc.y * 0.5 + 0.5);
    float shadowDepth = lightNdc.z;

    if (shadowUV.x < 0.0 || shadowUV.x > 1.0 || shadowUV.y < 0.0 || shadowUV.y > 1.0)
        return 1.0;

    uint w, h;
    shadow_map.GetDimensions(w, h);
    float2 texelSize = 1.0 / float2(w, h);

    float shadow = 0.0;
    for (int y = -2; y <= 2; y++) {
        for (int x = -2; x <= 2; x++) {
            float2 offset = float2(x, y) * texelSize;
            shadow += shadow_map.SampleCmpLevelZero(shadow_sampler, shadowUV + offset, shadowDepth - 0.005);
        }
    }
    return shadow / 25.0;
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
    float  roughness = max(albedoRough.a, 0.04);
    float3 normal    = normalize(normalMetal.rgb * 2.0 - 1.0);
    float  metallic  = normalMetal.a;
    float3 emission  = emissionVal.rgb;

    // Reconstruct world position
    float3 worldPos = reconstructWorldPos(input.uv, depth);
    float3 viewDir  = normalize(camera.position - worldPos);

    // PBR base reflectance
    float3 F0 = lerp(float3(0.04, 0.04, 0.04), albedo, metallic);
    float3 diffuseColor = albedo * (1.0 - metallic);

    // Ambient
    float3 color = diffuseColor * float3(0.02, 0.02, 0.025);

    float NdotV = max(dot(normal, viewDir), 0.001);

    // Accumulate point lights with Cook-Torrance BRDF
    for (uint i = 0; i < lighting.light_count; i++) {
        PointLight light = lighting.lights[i];
        float3 toLight = light.position - worldPos;
        float dist = length(toLight);

        if (dist > light.range) continue;

        float3 lightDir = toLight / dist;
        float3 halfVec  = normalize(lightDir + viewDir);

        float NdotL = max(dot(normal, lightDir), 0.0);
        float NdotH = max(dot(normal, halfVec), 0.0);
        float HdotV = max(dot(halfVec, viewDir), 0.0);

        // Attenuation: inverse-square with smooth range falloff
        float distAtten = 1.0 / (1.0 + dist * dist);
        float rangeFactor = saturate(1.0 - pow(dist / light.range, 4.0));
        float attenuation = light.intensity * distAtten * rangeFactor;

        // Cook-Torrance specular BRDF
        float D = distributionGGX(NdotH, roughness);
        float G = geometrySmith(NdotV, NdotL, roughness);
        float3 F = fresnelSchlick(HdotV, F0);

        float3 numerator = D * G * F;
        float denominator = 4.0 * NdotV * NdotL + 0.0001;
        float3 specular = numerator / denominator;

        // Energy conservation: diffuse reduced by Fresnel
        float3 kD = (float3(1.0) - F) * (1.0 - metallic);
        float3 diffuse = kD * diffuseColor / 3.14159265;

        color += (diffuse + specular) * light.color * NdotL * attenuation;
    }

    // Directional light with Cook-Torrance BRDF + shadows
    if (lighting.has_directional != 0) {
        float3 dirLightDir = normalize(-lighting.dir_light_direction);
        float3 halfVecD = normalize(dirLightDir + viewDir);

        float NdotLD = max(dot(normal, dirLightDir), 0.0);
        float NdotHD = max(dot(normal, halfVecD), 0.0);
        float HdotVD = max(dot(halfVecD, viewDir), 0.0);

        float Dd = distributionGGX(NdotHD, roughness);
        float Gd = geometrySmith(NdotV, NdotLD, roughness);
        float3 Fd = fresnelSchlick(HdotVD, F0);

        float3 specD = (Dd * Gd * Fd) / (4.0 * NdotV * NdotLD + 0.0001);
        float3 kDd = (float3(1.0) - Fd) * (1.0 - metallic);
        float3 diffD = kDd * diffuseColor / 3.14159265;

        float shadow = sampleShadowPCF(worldPos);
        color += (diffD + specD) * lighting.dir_light_color * NdotLD * lighting.dir_light_intensity * shadow;
    }

    // Modulate emission by lighting to preserve 3D form (must survive ACES tone mapping)
    float emit_lum = dot(emission, float3(0.2126, 0.7152, 0.0722));
    float blendE = emit_lum / (emit_lum + 0.5);
    float emit_shade = 0.15;
    if (lighting.has_directional != 0) {
        float emit_NdotL = max(dot(normal, normalize(-lighting.dir_light_direction)), 0.0);
        emit_shade = 0.1 + 0.9 * emit_NdotL;
    }
    float rimE = pow(1.0 - NdotV, 3.0);
    emit_shade = emit_shade + 0.35 * rimE;
    color = color * (1.0 - blendE) + emission * lerp(1.0, emit_shade, blendE);

    return float4(color, 1.0);
}

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
    // Shadow cascades: each one's light view-projection, the view depth where
    // each one ends and its depth bias. Cascade i uses tile (i % 2, i / 2).
    float4x4 cascade_vp[4];
    float4 cascade_splits;
    float4 cascade_bias;
    // Sky: enabled (0 or 1), intensity, highest mip level, unused; then its
    // diffuse light as nine harmonics coefficients.
    float4 sky;
    float4 sky_irradiance[9];
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
[[vk::binding(3, 2)]] Texture2D<float4> sky_map;
[[vk::binding(4, 2)]] SamplerState sky_sampler;

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

// Diffuse light from the sky for a surface facing n, divided by pi.
float3 skyDiffuse(float3 n) {
    float3 e = lighting.sky_irradiance[0].rgb * 0.282095
        + lighting.sky_irradiance[1].rgb * (0.488603 * n.y)
        + lighting.sky_irradiance[2].rgb * (0.488603 * n.z)
        + lighting.sky_irradiance[3].rgb * (0.488603 * n.x)
        + lighting.sky_irradiance[4].rgb * (1.092548 * n.x * n.y)
        + lighting.sky_irradiance[5].rgb * (1.092548 * n.y * n.z)
        + lighting.sky_irradiance[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0))
        + lighting.sky_irradiance[7].rgb * (1.092548 * n.x * n.z)
        + lighting.sky_irradiance[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y));
    return max(e, float3(0.0, 0.0, 0.0));
}

// The sky seen in direction d, blurred to mip level lod.
float3 sampleSky(float3 d, float lod) {
    float2 uv = float2(atan2(d.z, d.x) * 0.15915494 + 0.5, acos(clamp(d.y, -1.0, 1.0)) * 0.31830988);
    return sky_map.SampleLevel(sky_sampler, uv, lod).rgb;
}

// Share of reflected sky light (Karis' analytic split-sum fit).
float3 envBRDF(float3 F0, float roughness, float NdotV) {
    float4 c0 = float4(-1.0, -0.0275, -0.572, 0.022);
    float4 c1 = float4(1.0, 0.0425, 1.04, -0.04);
    float4 r = roughness * c0 + c1;
    float a004 = min(r.x * r.x, exp2(-9.28 * NdotV)) * r.x + r.y;
    float2 ab = float2(-1.04, 1.04) * a004 + r.zw;
    return F0 * ab.x + ab.y;
}

// Light that does not come from the scene's lights: the sky's, when there
// is one, else a faint constant.
float3 ambientLight(float3 diffuseColor, float3 F0, float roughness, float3 normal, float3 viewDir) {
    if (lighting.sky.x < 0.5)
        return diffuseColor * float3(0.02, 0.02, 0.025);
    float NdotV = max(dot(normal, viewDir), 0.001);
    float3 diffuse = diffuseColor * skyDiffuse(normal);
    float3 reflected = reflect(-viewDir, normal);
    float3 specular = sampleSky(reflected, roughness * lighting.sky.z) * envBRDF(F0, roughness, NdotV);
    return (diffuse + specular) * lighting.sky.y;
}

// What the camera sees where there is no geometry: the sky and the sun.
float3 skyBackground(float3 dir) {
    float3 color = sampleSky(dir, 0.0) * lighting.sky.y;
    if (lighting.has_directional != 0) {
        float3 sun = normalize(-lighting.dir_light_direction);
        float c = max(dot(dir, sun), 0.0);
        float disc = smoothstep(0.9995, 0.9998, c) * 8.0 + pow(c, 256.0) * 0.5;
        color += lighting.dir_light_color * lighting.dir_light_intensity * disc;
    }
    return color;
}

float sampleShadowPCF(float3 worldPos) {
    // The nearest cascade that reaches this point's distance from the camera.
    float viewDepth = -mul(camera.view, float4(worldPos, 1.0)).z;
    int cascade = 4;
    for (int i = 0; i < 4; i++) {
        if (viewDepth <= lighting.cascade_splits[i]) {
            cascade = i;
            break;
        }
    }
    // Beyond the last cascade shadows end.
    if (cascade >= 4)
        return 1.0;

    float4 lightClip = mul(lighting.cascade_vp[cascade], float4(worldPos, 1.0));
    float3 lightNdc = lightClip.xyz / lightClip.w;
    float2 localUV = float2(lightNdc.x * 0.5 + 0.5, -lightNdc.y * 0.5 + 0.5);
    float shadowDepth = lightNdc.z;

    // Nothing outside the cascade, beside it or past its far plane, was
    // drawn into it, so such points are lit.
    if (localUV.x < 0.0 || localUV.x > 1.0 || localUV.y < 0.0 || localUV.y > 1.0
        || shadowDepth > 1.0)
        return 1.0;

    uint w, h;
    shadow_map.GetDimensions(w, h);
    float2 texelSize = 1.0 / float2(w, h);
    float2 tile = float2(float(cascade % 2), float(cascade / 2)) * 0.5;
    // Keep the 5x5 filter inside this cascade's tile.
    float2 shadowUV = clamp(tile + localUV * 0.5, tile + texelSize * 2.5,
                            tile + float2(0.5, 0.5) - texelSize * 2.5);

    float shadow = 0.0;
    for (int y = -2; y <= 2; y++) {
        for (int x = -2; x <= 2; x++) {
            float2 offset = float2(x, y) * texelSize;
            shadow += shadow_map.SampleCmpLevelZero(shadow_sampler, shadowUV + offset, shadowDepth - lighting.cascade_bias[cascade]);
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

    // Where there is no geometry: the sky, or nothing.
    if (depth >= 1.0) {
        if (lighting.sky.x < 0.5) discard;
        float3 farPoint = reconstructWorldPos(input.uv, 1.0);
        return float4(skyBackground(normalize(farPoint - camera.position)), 1.0);
    }

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

    // Ambient: the sky's light, or a faint constant without a sky.
    float3 color = ambientLight(diffuseColor, F0, roughness, normal, viewDir);

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

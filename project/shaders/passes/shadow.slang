// Shadow depth pass: renders geometry from light's perspective (depth-only)

struct ShadowUniforms {
    float4x4 light_view_projection;
};

struct DrawUniforms {
    float4x4 model_matrix;
    float4x4 normal_matrix;
    float4 base_color;
    float roughness;
    float metallic;
    float2 _pad;
    float4 emission;
    float4 _padding[5];
};

[[vk::binding(0, 0)]] ConstantBuffer<ShadowUniforms> shadow;
[[vk::binding(0, 1)]] ConstantBuffer<DrawUniforms> draw;

[shader("vertex")]
float4 vs_main(float3 position : POSITION, float3 normal : NORMAL, float2 texCoords : TEXCOORD0, float4 color : COLOR0) : SV_Position {
    return mul(shadow.light_view_projection, mul(draw.model_matrix, float4(position, 1.0)));
}

[shader("fragment")]
void fs_main() {
    // Depth-only pass — no color output needed
}

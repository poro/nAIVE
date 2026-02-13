// shaders/passes/triangle.slang
// A simple vertex-colored triangle for Phase 1 validation.

struct VSInput {
    float2 position : POSITION;
    float3 color    : COLOR;
};

struct VSOutput {
    float4 sv_position : SV_Position;
    float3 color       : COLOR;
};

[shader("vertex")]
VSOutput vs_main(VSInput input) {
    VSOutput output;
    output.sv_position = float4(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

[shader("fragment")]
float4 fs_main(VSOutput input) : SV_Target {
    return float4(input.color, 1.0);
}

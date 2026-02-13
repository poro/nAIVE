// Shared camera uniform module
struct CameraUniform {
    float4x4 view;
    float4x4 projection;
    float4x4 view_projection;
    float3 position;
    float near_plane;
    float far_plane;
    float _pad1;
    float2 viewport_size;
    float _padding;
    float3 _pad2;
};

// Import-only module: it has no render entrypoints of its own.
float4 releaseMarkerColor(bool left) {
    return left ? float4(0.0, 1.0, 1.0, 1.0) : float4(1.0, 0.0, 1.0, 1.0);
}

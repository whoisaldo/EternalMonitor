#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

// Generates a fullscreen quad from vertex_id (no vertex buffer needed).
// Draw with .triangleStrip, vertexCount: 4.
vertex VertexOut fullscreen_vertex(uint vid [[vertex_id]]) {
    const float2 positions[4] = {
        float2(-1.0, -1.0),  // bottom-left
        float2( 1.0, -1.0),  // bottom-right
        float2(-1.0,  1.0),  // top-left
        float2( 1.0,  1.0),  // top-right
    };
    const float2 texCoords[4] = {
        float2(0.0, 1.0),
        float2(1.0, 1.0),
        float2(0.0, 0.0),
        float2(1.0, 0.0),
    };

    VertexOut out;
    out.position = float4(positions[vid], 0.0, 1.0);
    out.texCoord = texCoords[vid];
    return out;
}

// Passthrough fragment shader — samples the decoded video frame texture.
fragment half4 display_fragment(VertexOut in [[stage_in]],
                                texture2d<half> tex [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    return tex.sample(s, in.texCoord);
}

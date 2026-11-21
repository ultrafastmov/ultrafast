// ultrafast particles: instanced sprite quads, vertex-pulled from per-frame
// particle data computed on the CPU in closed form (deterministic: any frame
// can be rendered in any order).
//
// Draw ABI: P params, V particles (12 floats each), T0 sprite, S sampler.
// Params: [4..6] region origin (frame px), [6..8] target size (texels),
// [8] shutter span in seconds under motion blur (0: none).
// Particle: center (2), half size (1), rotation (1), color premultiplied
// linear (4), velocity in px/s (2), stretch (1), shape (1: 0 circle,
// 1 square, 2 spark, 3 sprite).
//
// Under motion blur a particle draws its own blur: the quad covers its path
// over the shutter (centered on where it is at the frame's time) and the
// fragment averages the shape at taps along that path, about every 1.5 px.

StructuredBuffer<float> P;
StructuredBuffer<float> V;
Texture2D<float4> T0;
SamplerState S;

struct POut {
    float4 pos : SV_Position;
    float2 off : TEXCOORD0;    // px from the particle's center, frame axes
    float4 color : COLOR0;
    float4 axes : TEXCOORD1;   // the shape's own x axis (xy) and its half extents (zw)
    float3 motion : TEXCOORD2; // path over the shutter (xy, px) and tap count
    float shape : TEXCOORD3;
};

[shader("vertex")]
POut vs_particle(uint vid: SV_VertexID) {
    uint p = vid / 6;
    uint corner = vid % 6;
    float2 c = float2(corner == 1 || corner == 2 || corner == 4 ? 1.0 : -1.0,
                      corner == 2 || corner == 4 || corner == 5 ? 1.0 : -1.0);
    uint b = p * 12;
    float2 center = float2(V[b], V[b + 1]);
    float half_size = V[b + 2];
    float rot = V[b + 3];
    float2 vel = float2(V[b + 8], V[b + 9]);
    float stretch = V[b + 10];
    float shape = V[b + 11];
    // Sparks stretch along their velocity; others rotate.
    float2 sx = shape > 1.5 && shape < 2.5 ? normalize(vel + float2(1e-6, 0.0)) : float2(cos(rot), sin(rot));
    float2 ext = float2(half_size * (1.0 + stretch), half_size);
    float2 path = vel * P[8];
    float len = length(path);
    // The quad: the shape's own when still; its path's when moving (aligned
    // with the motion, as long as the path plus the shape's reach).
    float2 ax, half2;
    if (len > 0.5) {
        ax = path / len;
        float reach = length(ext);
        half2 = float2(reach + len * 0.5, reach) + 1.0;
    } else {
        ax = sx;
        half2 = ext + 1.0;
        path = float2(0.0);
        len = 0.0;
    }
    float2 ay = float2(-ax.y, ax.x);
    float2 off = ax * c.x * half2.x + ay * c.y * half2.y;
    float2 local = center + off - float2(P[4], P[5]);
    float2 size = float2(P[6], P[7]);
    POut o;
    o.pos = float4(local.x / size.x * 2.0 - 1.0, 1.0 - local.y / size.y * 2.0, 0.0, 1.0);
    o.off = off;
    o.color = float4(V[b + 4], V[b + 5], V[b + 6], V[b + 7]);
    o.axes = float4(sx, max(ext, float2(1e-3)));
    o.motion = float3(path, len > 0.5 ? clamp(ceil(len / 1.5), 2.0, 16.0) : 1.0);
    o.shape = shape;
    return o;
}

// The shape at `q` (px from its center, frame axes): coverage with 1 px
// antialiasing, or the sprite's color.
float4 shape_at(float2 q, float2 sx, float2 ext, float shape) {
    float2 uv = float2(dot(q, sx), dot(q, float2(-sx.y, sx.x)));
    if (shape < 0.5 || (shape > 1.5 && shape < 2.5)) {
        // Circle / spark (ellipse): signed distance in px.
        float d = (length(uv / ext) - 1.0) * min(ext.x, ext.y);
        return float4(saturate(0.5 - d));
    }
    if (shape < 1.5) {
        float2 d = abs(uv) - ext;
        return float4(saturate(0.5 - max(d.x, d.y)));
    }
    float2 t = uv / ext;
    if (any(abs(t) > 1.0)) return float4(0.0);
    return T0.SampleLevel(S, t * 0.5 + 0.5, 0);
}

[shader("fragment")]
float4 fs_particle(POut i) : SV_Target {
    float2 sx = i.axes.xy;
    float2 ext = i.axes.zw;
    int n = int(i.motion.z + 0.5);
    float4 acc = float4(0.0);
    for (int k = 0; k < n; k++) {
        float f = n > 1 ? (float(k) + 0.5) / float(n) - 0.5 : 0.0;
        acc += shape_at(i.off - i.motion.xy * f, sx, ext, i.shape);
    }
    acc /= float(n);
    // Sprites carry their own color; other shapes their coverage.
    return i.shape > 2.5 ? acc * i.color : i.color * acc.x;
}

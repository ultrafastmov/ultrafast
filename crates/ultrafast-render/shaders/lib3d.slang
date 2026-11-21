// ultrafast 3D pass: vertex-pulled meshes with physically based shading.
//
// Draw ABI (both backends): P params, V vertices (8 floats: position,
// normal, uv), T0 albedo texture, S sampler.
//
// Params (per draw):
//   [0..4]     frame header (time, frame, width, height)
//   [4..20]    model matrix, row-major
//   [20..36]   view-projection matrix, row-major
//   [36..52]   normal matrix (inverse-transpose of model), row-major
//   [52..56]   base color (linear, straight alpha)
//   [56] metallic  [57] roughness  [58] has texture  [59] opacity
//   [60..63]   emissive (linear)
//   [64..67]   camera position  [67] light count
//   [68..71]   ambient sky color  [71] ambient intensity
//   [72..75]   ambient ground color
//   [76..78]   texture origin (frame px)  [78..80] frame size
//   [80..96]   light view-projection (shadow map), row-major
//   [96] shadows on  [97] depth bias  [98] shadow map texel  [99] strength
//   [100] shadow normal offset (world units)  [101] environment on
//   [102] environment intensity  [103] environment rotation (degrees)
//   [104..106] environment origin (frame px)  [106] fog on
//   [107] fog start  [108] fog end (world distance from the camera)
//   [109..112] fog color (linear)
//   [112..114] blurred environment origin (frame px)
//   [114] transmission (glass)  [115] index of refraction  [116] thickness
//   (world units the refracted ray travels)  [117] dispersion  [118..120] unused
//   (glass draws in a second pass with T0 = the finished opaque scene)
//   [120..]    lights, 8 floats each: kind (0 directional, 1 point),
//              x, y, z (direction towards the light, or position),
//              r, g, b, intensity. Light 0 casts the shadow.

StructuredBuffer<float> P;
StructuredBuffer<float> V;
Texture2D<float4> T0;
SamplerState S;
// Shadow map: light-space depth in r (1 = far).
Texture2D<float4> T1;
// Environment: any ultrafast image, read as an equirectangular panorama, and a
// blurred copy of it for rough reflections and diffuse light.
Texture2D<float4> T2;
Texture2D<float4> T3;

static const float PI = 3.14159265358979;

struct VOut {
    float4 pos : SV_Position;
    float3 normal : NORMAL;
    float2 uv : TEXCOORD0;
    float3 world : TEXCOORD1;
};

float4x4 mat(uint o) {
    return float4x4(P[o], P[o + 1], P[o + 2], P[o + 3],
                    P[o + 4], P[o + 5], P[o + 6], P[o + 7],
                    P[o + 8], P[o + 9], P[o + 10], P[o + 11],
                    P[o + 12], P[o + 13], P[o + 14], P[o + 15]);
}

float3 p3(uint o) { return float3(P[o], P[o + 1], P[o + 2]); }

[shader("vertex")]
VOut vs_mesh(uint vid: SV_VertexID) {
    uint b = vid * 8;
    float4 world = mul(mat(4), float4(V[b], V[b + 1], V[b + 2], 1.0));
    VOut o;
    o.pos = mul(mat(20), world);
    o.normal = mul(mat(36), float4(V[b + 3], V[b + 4], V[b + 5], 0.0)).xyz;
    o.uv = float2(V[b + 6], V[b + 7]);
    o.world = world.xyz;
    return o;
}

float d_ggx(float nh, float a) {
    float a2 = a * a;
    float d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-6);
}

float g_smith(float nv, float nl, float a) {
    float k = (a + 1.0) * (a + 1.0) / 8.0;
    return (nv / (nv * (1.0 - k) + k)) * (nl / (nl * (1.0 - k) + k));
}

float3 fresnel(float vh, float3 f0) {
    return f0 + (1.0 - f0) * pow(1.0 - saturate(vh), 5.0);
}

// Fraction of light 0 reaching a world point: 3x3 PCF over the shadow map.
float shadow_factor(float3 world, float3 n) {
    if (P[96] < 0.5) return 1.0;
    // Normal offset (in world units) keeps lit surfaces from self-shadowing.
    float4 lp = mul(mat(80), float4(world + n * P[100], 1.0));
    float3 ndc = lp.xyz / lp.w;
    float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (any(uv < 0.0) || any(uv > 1.0) || ndc.z > 1.0) return 1.0;
    float texel = P[98];
    float bias = P[97];
    float lit = 0.0;
    for (int y = -1; y <= 1; y++) {
        for (int x = -1; x <= 1; x++) {
            float d = T1.SampleLevel(S, uv + float2(x, y) * texel, 0).r;
            lit += ndc.z - bias <= d ? 1.0 : 0.0;
        }
    }
    return lerp(1.0, lit / 9.0, P[99]);
}

[shader("vertex")]
VOut vs_shadow(uint vid: SV_VertexID) {
    uint b = vid * 8;
    float4 world = mul(mat(4), float4(V[b], V[b + 1], V[b + 2], 1.0));
    VOut o;
    o.pos = mul(mat(20), world);
    o.normal = float3(0.0);
    o.uv = float2(0.0);
    o.world = world.xyz;
    return o;
}

// Distance from the camera ([36..39] in the distance pass), for depth of
// field.
[shader("fragment")]
float4 fs_dist(VOut i) : SV_Target {
    return float4(length(i.world - p3(36)), 0.0, 0.0, 1.0);
}

[shader("fragment")]
float4 fs_shadow(VOut i) : SV_Target {
    return float4(i.pos.z, 0.0, 0.0, 1.0);
}

float2 env_uv(float3 d) {
    return float2(atan2(d.x, -d.z) / (2.0 * PI) + 0.5 + P[103] / 360.0, acos(clamp(d.y, -1.0, 1.0)) / PI);
}

float4 env_tap(Texture2D<float4> tex, float2 uv, float2 origin) {
    float2 frame = float2(P[78], P[79]);
    uint tw, th;
    tex.GetDimensions(tw, th);
    float2 px = float2(frac(uv.x), clamp(uv.y, 0.002, 0.998)) * frame - origin;
    return tex.SampleLevel(S, px / float2(tw, th), 0.0);
}

// Environment radiance towards direction d for a surface of roughness
// `rough`: the sharp image for glossy surfaces, the prefiltered one for
// rough ones (a few taps on each, never enough to show as banding).
float3 env_at(float3 d, float rough) {
    float2 uv = env_uv(d);
    float3 sharp = float3(0.0);
    float4 soft = float4(0.0);
    float rs = min(rough, 0.3) * 0.1;
    for (int k = 0; k < 8; k++) {
        float r = sqrt((float(k) + 0.5) / 8.0);
        float a = float(k) * 2.39996;
        float2 o = float2(cos(a) * 0.5, sin(a)) * r;
        sharp += env_tap(T2, uv + o * rs, float2(P[104], P[105])).rgb;
        soft += env_tap(T3, uv + o * 0.04, float2(P[112], P[113]));
    }
    // The blur fades towards the image's edges (the panorama's seam and
    // poles); dividing by its coverage renormalizes it there.
    float3 soft_rgb = soft.rgb / max(soft.a, 1e-4) * 8.0;
    return lerp(sharp, soft_rgb, smoothstep(0.08, 0.45, rough)) / 8.0 * P[102];
}

// Where a world point lands on screen, in frame px.
float2 screen_of(float3 w) {
    float4 c = mul(mat(20), float4(w, 1.0));
    float2 ndc = c.xy / max(c.w, 1e-4);
    return float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * float2(P[78], P[79]);
}

float4 scene_at(float2 px) {
    uint tw, th;
    T0.GetDimensions(tw, th);
    return T0.SampleLevel(S, px / float2(tw, th), 0.0);
}

// Glass: the scene behind, bent by refraction through `thickness` of
// material (per channel with dispersion), frosted by roughness, tinted by
// the base color; reflections and highlights on top by Fresnel.
float4 glass(VOut i, float3 n, float3 v, float4 base) {
    float rough = clamp(P[57], 0.0, 1.0);
    float ior = max(P[115], 1.0);
    float thick = P[116];
    float2 here = screen_of(i.world);
    float3 col = float3(0.0);
    float disp = P[117];
    for (int c = 0; c < 3; c++) {
        float eta = 1.0 / (ior * (1.0 + disp * (float(c) - 1.0) * 0.02));
        float3 t = refract(-v, n, eta);
        if (dot(t, t) < 1e-6) t = reflect(-v, n);
        float2 off = screen_of(i.world + t * thick) - screen_of(i.world - v * thick);
        float acc = 0.0;
        int taps = rough > 0.02 ? 8 : 1;
        for (int k = 0; k < taps; k++) {
            float r = sqrt((float(k) + 0.5) / float(taps)) * rough * 60.0;
            float a = float(k) * 2.39996;
            acc += scene_at(here + off + r * float2(cos(a), sin(a)))[c];
        }
        col[c] = acc / float(taps);
    }
    // Beer-ish tint, stronger where the glass is seen edge-on.
    float nv = max(dot(n, v), 1e-4);
    float3 tint = pow(max(base.rgb, 1e-3), float3(0.6 + 0.8 * (1.0 - nv)));
    col *= lerp(float3(1.0), tint, P[114]);
    // Reflection by Fresnel (Schlick, from the index of refraction).
    float f0 = pow((ior - 1.0) / (ior + 1.0), 2.0);
    float fr = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    float3 r = reflect(-v, n);
    float3 refl = P[101] > 0.5 ? env_at(r, rough) : lerp(p3(72), p3(68), smoothstep(-0.2, 0.2, r.y)) * P[71];
    col = lerp(col, refl, saturate(fr));
    int count = int(P[67] + 0.5);
    float a2 = max(rough * rough, 0.002);
    for (int k = 0; k < count; k++) {
        uint o = 120 + uint(k) * 8;
        float3 l = P[o] < 0.5 ? normalize(p3(o + 1)) : normalize(p3(o + 1) - i.world);
        float nl = dot(n, l);
        if (nl <= 0.0) continue;
        float3 h = normalize(v + l);
        float3 f = fresnel(dot(v, h), float3(f0));
        float3 spec = d_ggx(max(dot(n, h), 0.0), a2) * g_smith(nv, nl, a2) * f / max(4.0 * nv * nl, 1e-4);
        col += spec * float3(P[o + 4], P[o + 5], P[o + 6]) * P[o + 7] * nl;
    }
    col += p3(60);
    float alpha = saturate(base.a * P[59]);
    return float4(col * alpha, alpha);
}

[shader("fragment")]
float4 fs_mesh(VOut i) : SV_Target {
    float3 n = normalize(i.normal);
    float3 v = normalize(p3(64) - i.world);
    // Double-sided: light the side facing the camera.
    if (dot(n, v) < 0.0) n = -n;

    float4 base = float4(P[52], P[53], P[54], P[55]);
    if (P[114] > 0.0) return glass(i, n, v, base);
    if (P[58] > 0.5) {
        // Albedo from a ultrafast image (linear, premultiplied, placed in frame).
        float2 frame = float2(P[78], P[79]);
        uint tw, th;
        T0.GetDimensions(tw, th);
        float2 px = float2(i.uv.x, i.uv.y) * frame - float2(P[76], P[77]);
        // The texture has no mip chain: where it is minified (seen small
        // or at a grazing angle), average a grid of taps across the pixel's
        // footprint so fine detail does not shimmer.
        float2 dx = ddx(px), dy = ddy(px);
        float foot = max(length(dx), length(dy));
        float4 t;
        if (foot <= 1.5) {
            t = T0.Sample(S, px / float2(tw, th));
        } else {
            int n = int(clamp(ceil(foot), 2.0, 6.0));
            t = float4(0.0);
            for (int a = 0; a < n; a++) {
                for (int b = 0; b < n; b++) {
                    float2 o = (float2(a, b) + 0.5) / float(n) - 0.5;
                    t += T0.SampleLevel(S, (px + dx * o.x + dy * o.y) / float2(tw, th), 0.0);
                }
            }
            t /= float(n * n);
        }
        float3 straight = t.a > 1e-6 ? t.rgb / t.a : float3(0.0);
        base = float4(base.rgb * straight, base.a * t.a);
    }
    float metallic = saturate(P[56]);
    float rough = clamp(P[57], 0.04, 1.0);
    float a = rough * rough;
    float3 f0 = lerp(float3(0.04), base.rgb, metallic);
    float nv = max(dot(n, v), 1e-4);

    float3 color = float3(0.0);
    int count = int(P[67] + 0.5);
    float shadow = shadow_factor(i.world, n);
    for (int k = 0; k < count; k++) {
        uint o = 120 + uint(k) * 8;
        float3 l;
        float3 radiance = float3(P[o + 4], P[o + 5], P[o + 6]) * P[o + 7];
        if (P[o] < 0.5) {
            l = normalize(p3(o + 1));
        } else {
            float3 d = p3(o + 1) - i.world;
            float dist2 = max(dot(d, d), 1e-4);
            l = d * rsqrt(dist2);
            radiance /= dist2;
        }
        float nl = dot(n, l);
        if (nl <= 0.0) continue;
        float3 h = normalize(v + l);
        float3 f = fresnel(dot(v, h), f0);
        float3 spec = d_ggx(max(dot(n, h), 0.0), a) * g_smith(nv, nl, a) * f / max(4.0 * nv * nl, 1e-4);
        float3 kd = (1.0 - f) * (1.0 - metallic);
        float lit = k == 0 ? shadow : 1.0;
        color += (kd * base.rgb / PI + spec) * radiance * nl * lit;
    }
    // Hemisphere ambient: diffuse from the normal, specular from the
    // reflection vector (so metals reflect the sky/ground gradient).
    float3 sky = p3(68), ground = p3(72);
    float3 r = reflect(-v, n);
    float3 diffuse_env, spec_env;
    if (P[101] > 0.5) {
        diffuse_env = env_at(n, 1.0);
        spec_env = env_at(r, rough);
    } else {
        diffuse_env = lerp(ground, sky, n.y * 0.5 + 0.5) * P[71];
        spec_env = lerp(ground, sky, smoothstep(-0.2, 0.2, r.y)) * P[71];
    }
    float3 fa = f0 + (max(float3(1.0 - rough), f0) - f0) * pow(1.0 - nv, 5.0);
    color += diffuse_env * base.rgb * (1.0 - metallic) + spec_env * fa * (1.0 - 0.6 * rough);
    color += p3(60);
    if (P[106] > 0.5) {
        // Distance fog: far surfaces melt into the backdrop.
        float fd = smoothstep(P[107], P[108], length(i.world - p3(64)));
        color = lerp(color, p3(109), fd);
    }
    float alpha = saturate(base.a * P[59]);
    return float4(color * alpha, alpha);
}

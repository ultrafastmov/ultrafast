// ultrafast op library (Slang).
//
// Every kernel ultrafast runs is generated from these functions: single-op
// kernels (tier 0, compiled once) and fused kernels (one per fusion group,
// compiled in the background on structural edits).
//
// Conventions
//   - Working space: linear-light, premultiplied RGBA in float.
//   - Pixel (0,0) is top-left; positions are in output pixels, centers at +0.5.
//   - Params live in P. P[0..4] is the frame header (time, frame, width,
//     height); each op reads a fixed-size block starting at offset `o`.
//   - Ctx.px is the size of one output pixel in the current coordinate space.
//     Warps scale it so antialiasing stays one output pixel wide.
//
// Bindings (must match ultrafast-gpu): OUT texture(0), T0.. texture(1..),
// S sampler(0), P buffer(0), B1.. buffer(1..).

// Output is write-only: faster, and works with display formats (BGRA8).
WTexture2D<float4> OUT;
Texture2D<float4> T0;
Texture2D<float4> T1;
Texture2D<float4> T2;
Texture2D<float4> T3;
Texture2D<float4> T4;
Texture2D<float4> T5;
Texture2D<float4> T6;
Texture2D<float4> T7;
Texture2D<float4> T8;
Texture2D<float4> T9;
Texture2D<float4> T10;
Texture2D<float4> T11;
Texture2D<float4> T12;
Texture2D<float4> T13;
Texture2D<float4> T14;
Texture2D<float4> T15;
SamplerState S;
StructuredBuffer<float> P;
// Coverage accumulator for the path rasterizer: (width + 2) x height cells
// of 16.16 fixed point signed area.
RWStructuredBuffer<int> B1;
// Path segments: pairs of float4 (x0, y0, x1, y1), (weight, group, 0, 0).
StructuredBuffer<float4> B2;

static const float PI = 3.14159265358979;

struct Ctx {
    float2 pos;
    float px;
};

float pf(uint i) { return P[i]; }
float2 pf2(uint i) { return float2(P[i], P[i + 1]); }
float4 pf4(uint i) { return float4(P[i], P[i + 1], P[i + 2], P[i + 3]); }
int pi(uint i) { return int(P[i] + 0.5); }

float frame_time() { return P[0]; }
bool in_rect(float2 p, float4 r) { return p.x >= r.x && p.y >= r.y && p.x <= r.z && p.y <= r.w; }
float2 resolution() { return float2(P[2], P[3]); }

// Header [1] is the size of one output texel in frame pixels: 1 normally,
// more when a group renders at reduced resolution.
Ctx root_ctx(uint2 gid) {
    Ctx c;
    c.pos = (float2(gid) + 0.5) * P[1];
    c.px = P[1];
    return c;
}

bool outside_frame(uint2 gid) {
    return float(gid.x) * P[1] >= P[2] || float(gid.y) * P[1] >= P[3];
}

// Texture access. Every input texture is *placed*: it covers the frame
// starting at an origin (P[o], P[o+1]) and is transparent outside its own
// extent. Frame-sized intermediates have origin (0, 0); rasterized paths and
// text use textures sized to their bounds.
// `load_o` is for the identity context (no resampling); `sample_o`
// resamples at a pixel position.
float4 load_o(Texture2D<float4> t, uint2 gid, uint o) {
    int2 p = int2(gid) - int2(pf2(o));
    uint w, h;
    t.GetDimensions(w, h);
    if (p.x < 0 || p.y < 0 || p.x >= int(w) || p.y >= int(h)) return float4(0.0);
    return t.Load(int3(p, 0));
}
float4 sample_o(Texture2D<float4> t, float2 pos, uint o) {
    uint w, h;
    t.GetDimensions(w, h);
    return t.SampleLevel(S, (pos - pf2(o)) / float2(w, h), 0);
}
// A fused kernel's inputs also carry a texel size, P[o+2] (frame px per
// texel): 1, or more for a blur kept at the resolution it was computed at,
// which is sampled where a full-size texture would be loaded.
float4 load_t(Texture2D<float4> t, uint2 gid, uint o) {
    float s = P[o + 2];
    if (s == 1.0) return load_o(t, gid, o);
    uint w, h;
    t.GetDimensions(w, h);
    return t.SampleLevel(S, (float2(gid) + 0.5 - pf2(o)) / (float2(w, h) * s), 0);
}
float4 sample_t(Texture2D<float4> t, float2 pos, uint o) {
    uint w, h;
    t.GetDimensions(w, h);
    return t.SampleLevel(S, (pos - pf2(o)) / (float2(w, h) * P[o + 2]), 0);
}
float4 load(Texture2D<float4> t, uint2 gid) { return t.Load(int3(int2(gid), 0)); }
float4 sample_px(Texture2D<float4> t, float2 pos) {
    return t.SampleLevel(S, pos / resolution(), 0);
}
float4 sample_uv(Texture2D<float4> t, float2 uv) { return t.SampleLevel(S, uv, 0); }

// ---------------------------------------------------------------- color

float srgb_to_linear1(float x) {
    return x <= 0.04045 ? x / 12.92 : pow((x + 0.055) / 1.055, 2.4);
}
float linear_to_srgb1(float x) {
    x = max(x, 0.0);
    return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055;
}
float3 srgb_to_linear(float3 c) { return float3(srgb_to_linear1(c.r), srgb_to_linear1(c.g), srgb_to_linear1(c.b)); }
float3 linear_to_srgb(float3 c) { return float3(linear_to_srgb1(c.r), linear_to_srgb1(c.g), linear_to_srgb1(c.b)); }

float3 unpremul(float4 c) { return c.a > 1e-6 ? c.rgb / c.a : float3(0.0); }
float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

// Premultiplied linear <-> premultiplied sRGB-encoded.
float4 to_encoded(float4 c) { return float4(linear_to_srgb(unpremul(c)) * c.a, c.a); }
float4 from_encoded(float4 c) { return float4(srgb_to_linear(unpremul(c)) * c.a, c.a); }

// ---------------------------------------------------------------- generators

// solid [4]: color
float4 g_solid(Ctx c, uint o) { return pf4(o); }

// Gradients [46]: [0] stop count, [1] space (0 linear, 1 sRGB-encoded),
// [2..6] geometry, [6..46] 8 stops of (position, premultiplied rgba in the
// interpolation space).
float4 gradient_eval(float u, uint o) {
    int n = clamp(pi(o), 1, 8);
    uint s = o + 6;
    float4 c = pf4(s + 1);
    if (u > P[s]) {
        c = pf4(s + uint(n - 1) * 5 + 1);
        for (int k = 0; k < n - 1; k++) {
            uint a = s + uint(k) * 5;
            uint b = a + 5;
            if (u <= P[b]) {
                float t = saturate((u - P[a]) / max(P[b] - P[a], 1e-6));
                c = lerp(pf4(a + 1), pf4(b + 1), t);
                break;
            }
        }
    }
    return pi(o + 1) == 1 ? from_encoded(c) : c;
}

// linear geometry: start, end
float4 g_gradient_linear(Ctx c, uint o) {
    float2 s = pf2(o + 2), e = pf2(o + 4);
    float2 d = e - s;
    return gradient_eval(saturate(dot(c.pos - s, d) / max(dot(d, d), 1e-6)), o);
}

// radial geometry: center, radius
float4 g_gradient_radial(Ctx c, uint o) {
    return gradient_eval(saturate(length(c.pos - pf2(o + 2)) / max(pf(o + 4), 1e-3)), o);
}

// conic geometry: center, start angle (rad)
float4 g_gradient_conic(Ctx c, uint o) {
    float2 d = c.pos - pf2(o + 2);
    return gradient_eval(frac((atan2(d.y, d.x) - pf(o + 4)) / (2.0 * PI)), o);
}

float sd_round_box(float2 q, float2 b, float r) {
    r = min(r, min(b.x, b.y));
    float2 d = abs(q) - b + r;
    return length(max(d, float2(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

float sd_ellipse(float2 q, float2 r) {
    r = max(r, float2(1e-3));
    float k0 = length(q / r);
    float k1 = length(q / (r * r));
    return k0 < 1e-6 ? -min(r.x, r.y) : k0 * (k0 - 1.0) / k1;
}

// shape [13]: color, center, half size, corner radius, kind (0 rect, 1
// ellipse), softness (px), rotation (rad), stroke width (0 = fill)
float4 g_shape(Ctx c, uint o) {
    float2 q = c.pos - pf2(o + 4);
    float2 hs = pf2(o + 6);
    float soft = max(pf(o + 10), 1e-3) * c.px;
    float stroke = pf(o + 12);
    // Cheap reject outside the rotated bounding circle.
    float reach = length(hs) + soft + stroke;
    if (dot(q, q) > reach * reach) return float4(0.0);
    float cr = cos(-pf(o + 11)), sr = sin(-pf(o + 11));
    q = float2(cr * q.x - sr * q.y, sr * q.x + cr * q.y);
    float d = pf(o + 9) > 0.5 ? sd_ellipse(q, hs) : sd_round_box(q, hs, pf(o + 8));
    if (stroke > 0.0) d = abs(d + stroke * 0.5) - stroke * 0.5;
    float cov = saturate(0.5 - d / soft);
    // A fill thinner than the filter covers its thickness, not half a
    // pixel: shapes growing from zero start invisible.
    float m = min(hs.x, hs.y);
    if (stroke <= 0.0 && 2.0 * m < soft) cov *= (2.0 * m / soft) / (0.5 + m / soft);
    return pf4(o) * cov;
}

// checker [9]: a, b, cell size
float4 g_checker(Ctx c, uint o) {
    float2 cell = floor(c.pos / max(pf(o + 8), 1.0));
    return (int(cell.x + cell.y) & 1) == 0 ? pf4(o) : pf4(o + 4);
}

// Simplex noise 3D (Gustavson / Ashima, MIT).
float3 mod289(float3 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
float4 mod289_4(float4 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
float4 permute(float4 x) { return mod289_4(((x * 34.0) + 10.0) * x); }
float4 taylor_inv_sqrt(float4 r) { return 1.79284291400159 - 0.85373472095314 * r; }

float snoise(float3 v) {
    const float2 C = float2(1.0 / 6.0, 1.0 / 3.0);
    float3 i = floor(v + dot(v, C.yyy));
    float3 x0 = v - i + dot(i, C.xxx);
    float3 g = step(x0.yzx, x0.xyz);
    float3 l = 1.0 - g;
    float3 i1 = min(g.xyz, l.zxy);
    float3 i2 = max(g.xyz, l.zxy);
    float3 x1 = x0 - i1 + C.xxx;
    float3 x2 = x0 - i2 + C.yyy;
    float3 x3 = x0 - 0.5;
    i = mod289(i);
    float4 p = permute(permute(permute(i.z + float4(0.0, i1.z, i2.z, 1.0)) + i.y + float4(0.0, i1.y, i2.y, 1.0))
                       + i.x + float4(0.0, i1.x, i2.x, 1.0));
    float4 j = p - 49.0 * floor(p * (1.0 / 49.0));
    float4 x_ = floor(j * (1.0 / 7.0));
    float4 y_ = floor(j - 7.0 * x_);
    float4 x = (x_ * 2.0 + 0.5) / 7.0 - 1.0;
    float4 y = (y_ * 2.0 + 0.5) / 7.0 - 1.0;
    float4 h = 1.0 - abs(x) - abs(y);
    float4 b0 = float4(x.xy, y.xy);
    float4 b1 = float4(x.zw, y.zw);
    float4 s0 = floor(b0) * 2.0 + 1.0;
    float4 s1 = floor(b1) * 2.0 + 1.0;
    float4 sh = -step(h, float4(0.0));
    float4 a0 = b0.xzyw + s0.xzyw * sh.xxyy;
    float4 a1 = b1.xzyw + s1.xzyw * sh.zzww;
    float3 g0 = float3(a0.xy, h.x);
    float3 g1 = float3(a0.zw, h.y);
    float3 g2 = float3(a1.xy, h.z);
    float3 g3 = float3(a1.zw, h.w);
    float4 norm = taylor_inv_sqrt(float4(dot(g0, g0), dot(g1, g1), dot(g2, g2), dot(g3, g3)));
    g0 *= norm.x; g1 *= norm.y; g2 *= norm.z; g3 *= norm.w;
    float4 m = max(0.5 - float4(dot(x0, x0), dot(x1, x1), dot(x2, x2), dot(x3, x3)), 0.0);
    m = m * m;
    return 105.0 * dot(m * m, float4(dot(g0, x0), dot(g1, x1), dot(g2, x2), dot(g3, x3)));
}

// noise [14]: a, b, scale (px per cell), offset, z (evolution), octaves
float4 g_noise(Ctx c, uint o) {
    float2 p = (c.pos + pf2(o + 10)) / max(pf2(o + 8), float2(1e-3));
    float z = pf(o + 12);
    int oct = clamp(pi(o + 13), 1, 8);
    float sum = 0.0, amp = 0.5, norm = 0.0;
    float3 q = float3(p, z);
    for (int k = 0; k < oct; k++) {
        sum += amp * snoise(q);
        norm += amp;
        q = q * 2.0 + float3(17.1, 31.7, 0.0);
        amp *= 0.5;
    }
    float u = saturate(0.5 + 0.5 * sum / norm);
    return lerp(pf4(o), pf4(o + 4), u);
}

// image [10]: dest rect (x, y, w, h) in px, opacity, interpolation space
// (0 linear, 1 sRGB-encoded), the part of the image shown in the rect
// (u0, v0, u1, v1; 0,0,1,1 is all of it). `t` is the prepared (linear,
// premultiplied) image texture.
float4 texel_encoded(Texture2D<float4> t, int2 p, uint2 dim) {
    if (p.x < 0 || p.y < 0 || p.x >= int(dim.x) || p.y >= int(dim.y)) return float4(0.0);
    return to_encoded(t.Load(int3(p, 0)));
}
float4 g_image(Ctx c, uint o, Texture2D<float4> t) {
    float4 r = pf4(o);
    float2 uv = (c.pos - r.xy) / max(r.zw, float2(1e-3));
    float4 win = pf4(o + 6);
    float cover = 1.0;
    if (win.x > 0.0 || win.y > 0.0 || win.z < 1.0 || win.w < 1.0) {
        // Part of the image (cover in a box): the rect crops it, its edges
        // antialiased over a pixel as the image's own edges are.
        float2 d = min(c.pos - r.xy, r.xy + r.zw - c.pos);
        cover = saturate(min(d.x, d.y) + 0.5);
        if (cover <= 0.0) return float4(0.0);
        uv = win.xy + uv * (win.zw - win.xy);
    }
    if (pi(o + 5) == 1) {
        // Bilinear on the encoded values, as browsers scale images.
        uint w, h;
        t.GetDimensions(w, h);
        uint2 dim = uint2(w, h);
        float2 p = uv * float2(dim) - 0.5;
        int2 i = int2(floor(p));
        float2 f = p - floor(p);
        float4 a = lerp(texel_encoded(t, i, dim), texel_encoded(t, i + int2(1, 0), dim), f.x);
        float4 b = lerp(texel_encoded(t, i + int2(0, 1), dim), texel_encoded(t, i + int2(1, 1), dim), f.x);
        return from_encoded(lerp(a, b, f.y)) * (pf(o + 4) * cover);
    }
    return sample_uv(t, uv) * (pf(o + 4) * cover);
}

// ---------------------------------------------------------------- pointwise

// opacity [1]
float4 f_opacity(Ctx c, float4 a, uint o) { return a * pf(o); }

// over [2]: fg opacity, space (0 linear, 1 sRGB-encoded)
float4 f_over(Ctx c, float4 fg, float4 bg, uint o) {
    fg *= pf(o);
    if (pi(o + 1) == 1) {
        float4 f = to_encoded(fg), b = to_encoded(bg);
        return from_encoded(f + b * (1.0 - f.a));
    }
    return fg + bg * (1.0 - fg.a);
}

float3 set_lum(float3 c, float l) {
    float d = l - luma(c);
    c += d;
    float lc = luma(c);
    float n = min(min(c.r, c.g), c.b);
    float x = max(max(c.r, c.g), c.b);
    if (n < 0.0) c = lc + (c - lc) * lc / max(lc - n, 1e-6);
    if (x > 1.0) c = lc + (c - lc) * (1.0 - lc) / max(x - lc, 1e-6);
    return c;
}
float sat(float3 c) { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }
float3 set_sat(float3 c, float s) {
    float mx = max(max(c.r, c.g), c.b);
    float mn = min(min(c.r, c.g), c.b);
    float range = mx - mn;
    if (range <= 1e-6) return float3(0.0);
    return (c - mn) * s / range;
}

float soft_light1(float b, float s) {
    if (s <= 0.5) return b - (1.0 - 2.0 * s) * b * (1.0 - b);
    float d = b <= 0.25 ? ((16.0 * b - 12.0) * b + 4.0) * b : sqrt(b);
    return b + (2.0 * s - 1.0) * (d - b);
}
float dodge1(float b, float s) { return b <= 0.0 ? 0.0 : (s >= 1.0 ? 1.0 : min(1.0, b / (1.0 - s))); }
float burn1(float b, float s) { return b >= 1.0 ? 1.0 : (s <= 0.0 ? 0.0 : 1.0 - min(1.0, (1.0 - b) / s)); }
float hard1(float b, float s) { return s <= 0.5 ? b * 2.0 * s : 1.0 - (1.0 - b) * (1.0 - (2.0 * s - 1.0)); }

// W3C compositing blend functions B(cb, cs), straight color.
float3 blend_fn(int m, float3 b, float3 s) {
    switch (m) {
    case 1: return b * s;
    case 2: return b + s - b * s;
    case 3: return float3(hard1(s.r, b.r), hard1(s.g, b.g), hard1(s.b, b.b));
    case 4: return min(b, s);
    case 5: return max(b, s);
    case 6: return float3(dodge1(b.r, s.r), dodge1(b.g, s.g), dodge1(b.b, s.b));
    case 7: return float3(burn1(b.r, s.r), burn1(b.g, s.g), burn1(b.b, s.b));
    case 8: return float3(hard1(b.r, s.r), hard1(b.g, s.g), hard1(b.b, s.b));
    case 9: return float3(soft_light1(b.r, s.r), soft_light1(b.g, s.g), soft_light1(b.b, s.b));
    case 10: return abs(b - s);
    case 11: return b + s - 2.0 * b * s;
    case 12: return set_lum(set_sat(s, sat(b)), luma(b));
    case 13: return set_lum(set_sat(b, sat(s)), luma(b));
    case 14: return set_lum(s, luma(b));
    case 15: return set_lum(b, luma(s));
    case 16: return b + s;
    case 17: return max(b - s, float3(0.0));
    case 18: return float3(s.r > 0 ? b.r / s.r : 1.0, s.g > 0 ? b.g / s.g : 1.0, s.b > 0 ? b.b / s.b : 1.0);
    default: return s;
    }
}

// blend [3]: mode, fg opacity, space (0 linear, 1 sRGB-encoded)
float4 f_blend(Ctx c, float4 src, float4 dst, uint o) {
    int mode = pi(o);
    bool enc = pi(o + 2) == 1;
    src *= pf(o + 1);
    if (enc) { src = to_encoded(src); dst = to_encoded(dst); }
    float3 cs = unpremul(src), cb = unpremul(dst);
    float3 mixed = (1.0 - dst.a) * cs + dst.a * blend_fn(mode, cb, cs);
    float4 r = float4(src.a * mixed + (1.0 - src.a) * dst.rgb, src.a + dst.a * (1.0 - src.a));
    return enc ? from_encoded(r) : r;
}

// composite [1]: Porter-Duff op (0 in, 1 out, 2 atop, 3 xor, 4 plus, 5 over)
float4 f_composite(Ctx c, float4 s, float4 d, uint o) {
    switch (pi(o)) {
    case 0: return s * d.a;
    case 1: return s * (1.0 - d.a);
    case 2: return s * d.a + d * (1.0 - s.a);
    case 3: return s * (1.0 - d.a) + d * (1.0 - s.a);
    case 4: return min(s + d, float4(1.0e4));
    default: return s + d * (1.0 - s.a);
    }
}

// mask [1]: mode (0 alpha, 1 inverted alpha, 2 luma, 3 inverted luma)
float4 f_mask(Ctx c, float4 src, float4 m, uint o) {
    int mode = pi(o);
    float k = mode >= 2 ? luma(unpremul(m)) * m.a : m.a;
    if (mode == 1 || mode == 3) k = 1.0 - k;
    return src * k;
}

// mix [1]: t (0 = a, 1 = b)
float4 f_mix(Ctx c, float4 a, float4 b, uint o) { return lerp(a, b, pf(o)); }

// adjust [6]: exposure (stops, linear), contrast, saturation, hue (rad),
// brightness, gamma. All but exposure act on sRGB-encoded straight color.
float4 f_adjust(Ctx c, float4 v, uint o) {
    if (v.a <= 0.0) return v;
    float3 rgb = unpremul(v) * exp2(pf(o));
    rgb = linear_to_srgb(rgb);
    rgb = (rgb - 0.5) * pf(o + 1) + 0.5 + pf(o + 4);
    float l = luma(rgb);
    rgb = lerp(float3(l), rgb, pf(o + 2));
    float h = pf(o + 3);
    if (h != 0.0) {
        // Rotate around the grey axis (Rodrigues).
        float3 k = float3(0.57735);
        float ch = cos(h), sh = sin(h);
        rgb = rgb * ch + cross(k, rgb) * sh + k * dot(k, rgb) * (1.0 - ch);
    }
    rgb = pow(max(rgb, float3(0.0)), float3(1.0 / max(pf(o + 5), 1e-3)));
    return float4(srgb_to_linear(saturate(rgb)) * v.a, v.a);
}

// levels [5]: in black, in white, gamma, out black, out white (encoded)
float4 f_levels(Ctx c, float4 v, uint o) {
    if (v.a <= 0.0) return v;
    float3 e = linear_to_srgb(unpremul(v));
    e = saturate((e - pf(o)) / max(pf(o + 1) - pf(o), 1e-4));
    e = pow(e, float3(1.0 / max(pf(o + 2), 1e-3)));
    e = lerp(float3(pf(o + 3)), float3(pf(o + 4)), e);
    return float4(srgb_to_linear(e) * v.a, v.a);
}

// color_matrix [20]: 4x5 row-major on straight linear RGBA
float4 f_color_matrix(Ctx c, float4 v, uint o) {
    float4 s = float4(unpremul(v), v.a);
    float4 r;
    for (int i = 0; i < 4; i++) {
        uint b = o + uint(i) * 5;
        r[i] = dot(s, float4(P[b], P[b + 1], P[b + 2], P[b + 3])) + P[b + 4];
    }
    r.a = saturate(r.a);
    return float4(max(r.rgb, float3(0.0)) * r.a, r.a);
}

// lut [3]: amount, size N, space (0: LUT expects sRGB-encoded values, 1:
// linear). `t` holds the 3D LUT as N*N x N texels: blue slices side by side,
// red along x, green along y. Trilinear: bilinear within two blue slices.
float4 f_lut(Ctx c, float4 v, uint o, Texture2D<float4> t) {
    if (v.a <= 0.0) return v;
    float n = P[o + 1];
    if (n < 2.0) return v;
    bool encoded = P[o + 2] < 0.5;
    float3 straight = unpremul(v);
    float3 x = saturate(encoded ? linear_to_srgb(straight) : straight) * (n - 1.0);
    float b0 = floor(x.b);
    float b1 = min(b0 + 1.0, n - 1.0);
    float fb = x.b - b0;
    float2 size = float2(n * n, n);
    float2 rg = float2(x.r, x.g) + 0.5;
    float3 c0 = t.SampleLevel(S, (rg + float2(b0 * n, 0.0)) / size, 0).rgb;
    float3 c1 = t.SampleLevel(S, (rg + float2(b1 * n, 0.0)) / size, 0).rgb;
    float3 graded = lerp(c0, c1, fb);
    if (encoded) graded = srgb_to_linear(saturate(graded));
    return float4(lerp(straight, graded, P[o]) * v.a, v.a);
}

// invert [1]: amount (encoded rgb)
float4 f_invert(Ctx c, float4 v, uint o) {
    if (v.a <= 0.0) return v;
    float3 e = linear_to_srgb(unpremul(v));
    e = lerp(e, 1.0 - e, pf(o));
    return float4(srgb_to_linear(e) * v.a, v.a);
}

// tint [8]: color for shadows, color for highlights (premultiplied linear;
// only their rgb is used), mapped by luminance
float4 f_tint(Ctx c, float4 v, uint o) {
    float l = luma(linear_to_srgb(unpremul(v)));
    float3 rgb = lerp(unpremul(pf4(o)), unpremul(pf4(o + 4)), l);
    return float4(rgb * v.a, v.a);
}

// threshold [2]: level, softness (encoded luminance)
float4 f_threshold(Ctx c, float4 v, uint o) {
    float l = luma(linear_to_srgb(unpremul(v)));
    float w = max(pf(o + 1), 1e-4);
    float k = smoothstep(pf(o) - w, pf(o) + w, l);
    return float4(float3(k) * v.a, v.a);
}

// crop [5]: x, y, w, h, softness (px)
float4 f_crop(Ctx c, float4 v, uint o) {
    float4 r = pf4(o);
    float2 hs = r.zw * 0.5;
    float d = sd_round_box(c.pos - (r.xy + hs), hs, 0.0);
    return v * saturate(0.5 - d / (max(pf(o + 4), 1e-3) * c.px));
}

// ---------------------------------------------------------------- warps
// A warp maps the output context to the context its input is evaluated in.

// transform [7]: inverse affine (a, b, c, d, e, f): p' = (a*x + c*y + e, b*x + d*y + f),
// footprint scale
Ctx w_transform(Ctx c, uint o) {
    Ctx r;
    r.pos = float2(P[o] * c.pos.x + P[o + 2] * c.pos.y + P[o + 4],
                   P[o + 1] * c.pos.x + P[o + 3] * c.pos.y + P[o + 5]);
    r.px = c.px * P[o + 6];
    return r;
}

// corner_pin [10]: inverse homography (row-major 3x3), footprint scale
Ctx w_corner_pin(Ctx c, uint o) {
    float3 p = float3(c.pos, 1.0);
    float3 q = float3(dot(float3(P[o], P[o + 1], P[o + 2]), p),
                      dot(float3(P[o + 3], P[o + 4], P[o + 5]), p),
                      dot(float3(P[o + 6], P[o + 7], P[o + 8]), p));
    Ctx r;
    // Points behind the projection map far outside (transparent).
    r.pos = q.z > 1e-6 ? q.xy / q.z : float2(-1e9);
    r.px = c.px * P[o + 9];
    return r;
}

// wave [4]: amplitude (px), wavelength (px), phase (rad), direction (rad)
Ctx w_wave(Ctx c, uint o) {
    float a = pf(o + 3);
    float2 dir = float2(cos(a), sin(a));
    float2 perp = float2(-dir.y, dir.x);
    Ctx r = c;
    r.pos += perp * pf(o) * sin(dot(c.pos, dir) * 2.0 * PI / max(pf(o + 1), 1e-3) + pf(o + 2));
    return r;
}

// twirl [4]: center, radius, angle (rad)
Ctx w_twirl(Ctx c, uint o) {
    float2 d = c.pos - pf2(o);
    float r = length(d);
    float k = saturate(1.0 - r / max(pf(o + 2), 1e-3));
    float a = pf(o + 3) * k * k;
    float ca = cos(a), sa = sin(a);
    Ctx out_c = c;
    out_c.pos = pf2(o) + float2(ca * d.x - sa * d.y, sa * d.x + ca * d.y);
    return out_c;
}

// tile [4]: origin, size: repeats that region across the frame
Ctx w_tile(Ctx c, uint o) {
    float2 org = pf2(o), size = max(pf2(o + 2), float2(1.0));
    Ctx r = c;
    float2 rel = c.pos - org;
    r.pos = org + rel - floor(rel / size) * size;
    return r;
}

// displace [2]: amount (px) along x and y, driven by a map's red and green
Ctx w_displace(Ctx c, float4 map, uint o) {
    float3 m = unpremul(map);
    Ctx r = c;
    r.pos += (m.rg - 0.5) * 2.0 * pf2(o);
    return r;
}

// ---------------------------------------------------------------- barriers

// Separable gaussian blur. Params [4]: sigma (px), direction x, direction y,
// unused; [8..10] input origin. Uses linear sampling to fetch two taps per
// sample.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_blur(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    float sigma = max(pf(4), 1e-3);
    float2 dir = pf2(5);
    float2 pos = float2(gid.xy) + 0.5;
    int radius = int(ceil(sigma * 3.0));
    float inv2s2 = 1.0 / (2.0 * sigma * sigma);
    float4 acc = sample_o(T0, pos, 8);
    float wsum = 1.0;
    for (int i = 1; i <= radius; i += 2) {
        float w0 = exp(-float(i * i) * inv2s2);
        float w1 = exp(-float((i + 1) * (i + 1)) * inv2s2);
        float w = w0 + w1;
        float off = float(i) + w1 / max(w, 1e-8);
        acc += (sample_o(T0, pos + dir * off, 8) + sample_o(T0, pos - dir * off, 8)) * w;
        wsum += 2.0 * w;
    }
    OUT.Store(gid.xy, acc / wsum);
}

// Converts an uploaded RGBA8 sRGB straight-alpha image into the working
// space. Params [1]: 1 if the source is already premultiplied.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_prep_srgb8(uint3 gid: SV_DispatchThreadID) {
    uint w, h;
    OUT.GetDimensions(w, h);
    if (gid.x >= w || gid.y >= h) return;
    float4 c = T0.Load(int3(int2(gid.xy), 0));
    float3 rgb = pf(4) > 0.5 ? (c.a > 0.0 ? c.rgb / c.a : float3(0.0)) : c.rgb;
    OUT.Store(gid.xy, float4(srgb_to_linear(rgb) * c.a, c.a));
}

// A decoded video frame -> working space, upright. P[4]: layout (0: the
// texture T0, 1: BGRA bytes in B1, 2: 8-bit YCbCr 4:2:0 in B1, planes
// tightly packed, 3: the same with Cb and Cr interleaved (NV12)), [5]
// clockwise rotation in degrees, [6..8] coded size,
// [8] Kr and [9] Kb of the YCbCr matrix, [10] 1 for full-range samples.
// Chroma is sited left of its luma pair and between rows (H.264's default)
// and interpolated bilinearly.
uint video_byte(uint i) { return (uint(B1[i >> 2]) >> ((i & 3u) * 8u)) & 0xFFu; }
[shader("compute")]
[numthreads(16, 16, 1)]
void k_prep_video(uint3 gid: SV_DispatchThreadID) {
    uint w, h;
    OUT.GetDimensions(w, h);
    if (gid.x >= w || gid.y >= h) return;
    int rot = pi(5);
    uint cw = uint(P[6]);
    uint ch = uint(P[7]);
    uint2 c = gid.xy;
    if (rot == 90) c = uint2(gid.y, ch - 1 - gid.x);
    else if (rot == 180) c = uint2(cw - 1 - gid.x, ch - 1 - gid.y);
    else if (rot == 270) c = uint2(cw - 1 - gid.y, gid.x);
    int layout = pi(4);
    if (layout == 0) {
        float4 t = T0.Load(int3(int2(c), 0));
        OUT.Store(gid.xy, float4(srgb_to_linear(t.rgb) * t.a, t.a));
        return;
    }
    if (layout == 1) {
        uint v = uint(B1[c.y * cw + c.x]);
        float4 t = float4(float((v >> 16) & 0xFFu), float((v >> 8) & 0xFFu), float(v & 0xFFu), float(v >> 24)) / 255.0;
        OUT.Store(gid.xy, float4(srgb_to_linear(t.rgb) * t.a, t.a));
        return;
    }
    uint cw2 = (cw + 1) / 2;
    uint ch2 = (ch + 1) / 2;
    uint u0 = cw * ch;
    uint v0 = u0 + cw2 * ch2;
    float fx = float(c.x) * 0.5;
    float fy = max(float(c.y) * 0.5 - 0.25, 0.0);
    uint x0 = min(uint(fx), cw2 - 1);
    uint y0 = min(uint(fy), ch2 - 1);
    uint x1 = min(x0 + 1, cw2 - 1);
    uint y1 = min(y0 + 1, ch2 - 1);
    float tx = fx - float(x0);
    float ty = fy - float(y0);
    // Byte offsets of a chroma sample's Cb and Cr.
    bool nv12 = layout == 3;
    uint ustep = nv12 ? 2u : 1u;
    uint vbase = nv12 ? u0 + 1u : v0;
    uint row = cw2 * ustep;
    float2 c00 = float2(float(video_byte(u0 + y0 * row + x0 * ustep)), float(video_byte(vbase + y0 * row + x0 * ustep)));
    float2 c10 = float2(float(video_byte(u0 + y0 * row + x1 * ustep)), float(video_byte(vbase + y0 * row + x1 * ustep)));
    float2 c01 = float2(float(video_byte(u0 + y1 * row + x0 * ustep)), float(video_byte(vbase + y1 * row + x0 * ustep)));
    float2 c11 = float2(float(video_byte(u0 + y1 * row + x1 * ustep)), float(video_byte(vbase + y1 * row + x1 * ustep)));
    float2 cbcr = lerp(lerp(c00, c10, tx), lerp(c01, c11, tx), ty) - 128.0;
    float y = float(video_byte(c.y * cw + c.x));
    bool full = P[10] > 0.5;
    y = full ? y / 255.0 : (y - 16.0) / 219.0;
    cbcr /= full ? 255.0 : 224.0;
    float kr = P[8];
    float kb = P[9];
    float r = y + 2.0 * (1.0 - kr) * cbcr.y;
    float b = y + 2.0 * (1.0 - kb) * cbcr.x;
    float g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    OUT.Store(gid.xy, float4(srgb_to_linear(saturate(float3(r, g, b))), 1.0));
}

// Final pass: working space -> sRGB-encoded straight-alpha RGBA8.
// Params [4..6]: input origin; [6] 1 = flatten over black (opaque video).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_encode_srgb8(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    float4 c = load_o(T0, gid.xy, 4);
    if (P[6] > 0.5) {
        OUT.Store(gid.xy, float4(linear_to_srgb(saturate(c.rgb)), 1.0));
        return;
    }
    float a = saturate(c.a);
    OUT.Store(gid.xy, float4(linear_to_srgb(saturate(unpremul(c))), a));
}

// Encodes the final image as NV12 (BT.709, limited range) into B1, the
// layout hardware encoders read: a Y plane of `pitch` x `height` bytes,
// then interleaved Cb Cr at half resolution. Values are sRGB-encoded first,
// as on every other export path, but converted from float with no 8-bit
// RGB step. One thread per 4x2 block writes whole 32-bit words.
// P: header, origin (4, 5), _ (6), pitch (7), height rounded up to even (8).
float3 rgb_to_ycbcr709(float3 c) {
    float y = dot(c, float3(0.2126, 0.7152, 0.0722));
    return float3(y, (c.b - y) / 1.8556, (c.r - y) / 1.5748);
}
uint pack4(float4 v) {
    uint4 b = uint4(clamp(round(v), 0.0, 255.0));
    return b.x | (b.y << 8) | (b.z << 16) | (b.w << 24);
}
[shader("compute")]
[numthreads(16, 16, 1)]
void k_encode_nv12(uint3 gid: SV_DispatchThreadID) {
    uint pitch = uint(P[7]);
    uint h2 = uint(P[8]);
    uint x0 = gid.x * 4;
    uint y0 = gid.y * 2;
    if (x0 >= pitch || y0 >= h2) return;
    int2 last = int2(int(P[2]) - 1, int(P[3]) - 1);
    float4 ys[2];
    // Sums of the two 2x2 chroma blocks.
    float3 left = float3(0.0);
    float3 right = float3(0.0);
    for (uint j = 0; j < 2; j++) {
        float3 c4[4];
        for (uint i = 0; i < 4; i++) {
            uint2 p = uint2(min(int2(x0 + i, y0 + j), last));
            // Opaque output: premultiplied color is the image over black.
            c4[i] = linear_to_srgb(saturate(load_o(T0, p, 4).rgb));
            ys[j][i] = 16.0 + 219.0 * dot(c4[i], float3(0.2126, 0.7152, 0.0722));
        }
        left += c4[0] + c4[1];
        right += c4[2] + c4[3];
    }
    float3 a = rgb_to_ycbcr709(left * 0.25);
    float3 b = rgb_to_ycbcr709(right * 0.25);
    float4 uv = float4(128.0 + 224.0 * a.y, 128.0 + 224.0 * a.z, 128.0 + 224.0 * b.y, 128.0 + 224.0 * b.z);
    B1[(y0 * pitch + x0) / 4] = int(pack4(ys[0]));
    B1[((y0 + 1) * pitch + x0) / 4] = int(pack4(ys[1]));
    B1[(h2 * pitch + (y0 / 2) * pitch + x0) / 4] = int(pack4(uv));
}

// Clears the output to transparent (used when a kernel is unavailable).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_clear(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    OUT.Store(gid.xy, float4(0.0));
}

// ---------------------------------------------------------------- paths
// Exact-area coverage rasterization (the font-rs accumulation algorithm),
// on the GPU: every segment deposits signed area into the accumulator
// (one thread per segment), then each row is prefix-summed into coverage.
// Fixed-point integer atomics keep it portable (no float atomics needed).

static const float ACC_SCALE = 65536.0;

void acc_add(int idx, float v) {
    int dummy;
    InterlockedAdd(B1[idx], int(round(v * ACC_SCALE)), dummy);
}

// Params: [4] segment count, [5..7] region size, [7..9] region origin in
// the frame. The accumulator covers the region, which may extend past the
// frame (content placed off-screen and moved in by later transforms), once
// per color group (one plane after another).
[shader("compute")]
[numthreads(64, 1, 1)]
void k_raster_lines(uint3 tid: SV_DispatchThreadID) {
    uint n = uint(P[4]);
    if (tid.x >= n) return;
    int w = int(P[5]);
    int h = int(P[6]);
    int stride = w + 2;
    float4 seg = B2[tid.x * 2] - float4(P[7], P[8], P[7], P[8]);
    float4 meta = B2[tid.x * 2 + 1];
    float weight = meta.x;
    int plane = int(meta.y + 0.5) * stride * h;
    // Clamp x into the frame: area left of x = 0 lands in column 0, which
    // the row prefix sum carries to every pixel on its right.
    float2 p0 = float2(clamp(seg.x, 0.0, float(w)), seg.y);
    float2 p1 = float2(clamp(seg.z, 0.0, float(w)), seg.w);
    if (p0.y == p1.y) return;
    float dir = weight;
    if (p0.y > p1.y) {
        float2 t = p0; p0 = p1; p1 = t;
        dir = -weight;
    }
    if (p1.y <= 0.0 || p0.y >= float(h)) return;
    float dxdy = (p1.x - p0.x) / (p1.y - p0.y);
    float x = p0.x;
    if (p0.y < 0.0) x -= p0.y * dxdy;
    int y0 = max(int(floor(p0.y)), 0);
    int y1 = min(int(ceil(p1.y)), h);
    for (int y = y0; y < y1; y++) {
        int line = plane + y * stride;
        float dy = min(float(y + 1), p1.y) - max(float(y), p0.y);
        float xnext = x + dxdy * dy;
        float d = dy * dir;
        float x0 = min(x, xnext);
        float x1 = max(x, xnext);
        float x0floor = floor(x0);
        int x0i = int(x0floor);
        float x1ceil = ceil(x1);
        int x1i = int(x1ceil);
        if (x1i <= x0i + 1) {
            float xmf = 0.5 * (x + xnext) - x0floor;
            acc_add(line + x0i, d - d * xmf);
            acc_add(line + x0i + 1, d * xmf);
        } else {
            float s = 1.0 / (x1 - x0);
            float x0f = x0 - x0floor;
            float a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
            float x1f = x1 - x1ceil + 1.0;
            float am = 0.5 * s * x1f * x1f;
            acc_add(line + x0i, d * a0);
            if (x1i == x0i + 2) {
                acc_add(line + x0i + 1, d * (1.0 - a0 - am));
            } else {
                float a1 = s * (1.5 - x0f);
                acc_add(line + x0i + 1, d * (a1 - a0));
                for (int xi = x0i + 2; xi < x1i - 1; xi++) {
                    acc_add(line + xi, d * s);
                }
                float a2 = a1 + float(x1i - x0i - 3) * s;
                acc_add(line + x1i - 1, d * (1.0 - a2 - am));
            }
            acc_add(line + x1i, d * am);
        }
        x = xnext;
    }
}

// Copies T0 into OUT at (P[0], P[1]): a color glyph bitmap into the glyph
// atlas. P[2..4] is its size.
[shader("compute")]
[numthreads(8, 8, 1)]
void k_atlas_put(uint3 id: SV_DispatchThreadID) {
    if (id.x >= uint(P[2]) || id.y >= uint(P[3])) return;
    OUT.Store(id.xy + uint2(uint(P[0]), uint(P[1])), T0.Load(int3(id.xy, 0)));
}

groupshared float row_sums[256];

// Color glyphs (emoji) over resolved type at region texel (x, y). From
// P[ib]: glyph count, raster origin (frame px), opacity. Per glyph, three
// float4s in B2: the map from frame px to the bitmap's unit square (u's
// x, y and constant terms, then weight; v's terms), and its rect in the
// atlas T0 (x, y, w, h texels). Minified bitmaps are box filtered over
// the pixel's footprint.
float4 glyph_images(float4 res, int x, int y, int ib) {
    int n = int(P[ib] + 0.5);
    float3 p = float3(float(x) + 0.5 + P[ib + 1], float(y) + 0.5 + P[ib + 2], 1.0);
    float alpha = P[ib + 3];
    uint aw, ah;
    T0.GetDimensions(aw, ah);
    float2 inv = 1.0 / float2(aw, ah);
    for (int i = 0; i < n; i++) {
        float4 a = B2[i * 3];
        float4 b = B2[i * 3 + 1];
        float4 r = B2[i * 3 + 2];
        float2 uv = float2(dot(a.xyz, p), dot(b.xyz, p));
        // The pixel's footprint in the bitmap (unit square per frame px).
        float2 dx = float2(a.x, b.x);
        float2 dy = float2(a.y, b.y);
        float2 reach = abs(dx) + abs(dy);
        if (any(uv < -reach) || any(uv > 1.0 + reach)) continue;
        float texels = max(max(abs(dx.x), abs(dy.x)) * r.z, max(abs(dx.y), abs(dy.y)) * r.w);
        int taps = clamp(int(ceil(texels)), 1, 4);
        float4 sum = float4(0.0);
        for (int j = 0; j < taps; j++) {
            for (int k = 0; k < taps; k++) {
                float2 o = (float2(j, k) + 0.5) / float(taps) - 0.5;
                float2 q = uv + o.x * dx + o.y * dy;
                if (any(q < 0.0) || any(q > 1.0)) continue;
                float2 t = clamp(r.xy + q * r.zw, r.xy + 0.5, r.xy + r.zw - 0.5);
                sum += T0.SampleLevel(S, t * inv, 0);
            }
        }
        float4 c = sum / float(taps * taps) * (a.w * alpha);
        res = c + res * (1.0 - c.a);
    }
    return res;
}

// One threadgroup of 256 per row of the output texture, which holds the
// raster region (accumulator texel (x, y) = texture texel (x, y)). Each
// thread prefix-sums a chunk of the row, chunk totals are scanned in shared
// memory, then coverage is written. Texels outside the region are zero, so
// the whole texture is valid (it may be larger than the region).
// Params: [4] fill rule (0 nonzero, 1 even-odd), [5..9] color (linear
// premultiplied), [9..11] region size, [11] color groups, then a color per
// group, then color glyphs (see glyph_images). With several groups each
// plane's coverage is kept in place, then the groups are composited in
// order, each over the ones before.
[shader("compute")]
[numthreads(256, 1, 1)]
void k_raster_resolve(uint3 gtid: SV_GroupThreadID, uint3 gid: SV_GroupID) {
    int rw = int(P[9]);
    int rh = int(P[10]);
    int groups = max(int(P[11] + 0.5), 1);
    int ib = 12 + groups * 4;
    bool images = P[ib] > 0.5;
    uint tw, th;
    OUT.GetDimensions(tw, th);
    int y = int(gid.y);
    if (y >= int(th)) return;
    bool row_ok = y < rh;
    int stride = rw + 2;
    int plane = stride * rh;
    int chunk = (int(tw) + 255) / 256;
    int start = int(gtid.x) * chunk;
    int end = min(start + chunk, int(tw));
    bool even_odd = P[4] > 0.5;
    for (int k = 0; k < groups; k++) {
        int base = k * plane + y * stride;
        float local = 0.0;
        if (row_ok) {
            for (int x = start; x < min(end, rw); x++) local += float(B1[base + x]);
        }
        row_sums[gtid.x] = local;
        GroupMemoryBarrierWithGroupSync();
        // Hillis-Steele inclusive scan over the 256 chunk totals.
        for (uint off = 1; off < 256; off <<= 1) {
            float v = gtid.x >= off ? row_sums[gtid.x - off] : 0.0;
            GroupMemoryBarrierWithGroupSync();
            row_sums[gtid.x] += v;
            GroupMemoryBarrierWithGroupSync();
        }
        float acc = gtid.x > 0 ? row_sums[gtid.x - 1] : 0.0;
        GroupMemoryBarrierWithGroupSync();
        float4 color = float4(P[5], P[6], P[7], P[8]);
        for (int x = start; x < end; x++) {
            bool ok = row_ok && x < rw;
            if (ok) acc += float(B1[base + x]);
            float a = abs(acc) / ACC_SCALE;
            float cov = even_odd ? 1.0 - abs(1.0 - (a - 2.0 * floor(a * 0.5))) : min(a, 1.0);
            if (groups == 1) {
                float4 v = ok ? color * cov : float4(0.0);
                if (ok && images) v = glyph_images(v, x, y, ib);
                OUT.Store(uint2(x, y), v);
            } else if (ok) {
                // Only this thread reads these cells again.
                B1[base + x] = int(round(cov * ACC_SCALE));
            }
        }
    }
    if (groups == 1) return;
    for (int x = start; x < end; x++) {
        float4 res = float4(0.0);
        if (row_ok && x < rw) {
            for (int k = 0; k < groups; k++) {
                float cov = float(B1[k * plane + y * stride + x]) / ACC_SCALE;
                int c = 12 + k * 4;
                float4 col = float4(P[c], P[c + 1], P[c + 2], P[c + 3]) * cov;
                res = col + res * (1.0 - col.a);
            }
            if (images) res = glyph_images(res, x, y, ib);
        }
        OUT.Store(uint2(x, y), res);
    }
}

// Presents a placed frame into a display drawable (BGRA8, sRGB-encoded),
// letterboxed. Params: [4..6] source origin, [6..10] destination rect in
// drawable pixels (x, y, w, h), [10..12] drawable size, [12..15] backdrop
// color (encoded).
// The player's timeline, drawn over the presented frame (sRGB). Params:
// [15] on, [16] playhead 0..1, [17] loop in, [18] loop out (in >= out: no
// loop), [19] emphasis (0..1, hover or scrubbing), [20] playing.
float3 present_hud(float2 p, float2 dsize, float3 o) {
    if (P[15] < 0.5) return o;
    float e = P[19];
    float x0 = 24.0, x1 = dsize.x - 24.0;
    float yc = dsize.y - 26.0;
    float half_h = 2.0 + 2.0 * e;
    // A soft dark scrim so the bar reads on any frame.
    float scrim = smoothstep(dsize.y - 90.0, dsize.y, p.y) * 0.55;
    o = lerp(o, float3(0.0), scrim);
    float u = (p.x - x0) / (x1 - x0);
    float on_bar = step(abs(p.y - yc), half_h) * step(0.0, u) * step(u, 1.0);
    float3 track = float3(1.0) * 0.28;
    float3 played = float3(1.0) * 0.92;
    float3 col = u <= P[16] ? played : track;
    if (P[17] < P[18] && u >= P[17] && u <= P[18]) col = lerp(col, float3(1.0, 0.78, 0.25), 0.65);
    o = lerp(o, col, on_bar * 0.9);
    // Playhead.
    float2 head = float2(x0 + P[16] * (x1 - x0), yc);
    float r = 5.0 + 3.0 * e;
    float d = length(p - head);
    o = lerp(o, float3(1.0), saturate(r - d + 0.5));
    // Loop brackets.
    if (P[17] < P[18]) {
        for (int k = 0; k < 2; k++) {
            float bx = x0 + P[17 + k] * (x1 - x0);
            float m = step(abs(p.x - bx), 1.5) * step(abs(p.y - yc), 9.0);
            o = lerp(o, float3(1.0, 0.78, 0.25), m);
        }
    }
    // Agent edits: a fading outline around what just changed. Params: [21]
    // count, then (x0, y0, x1, y1, alpha) each from [22].
    for (int k = 0; k < min(int(P[21] + 0.5), 4); k++) {
        uint m = 22 + uint(k) * 5;
        float2 c = (pf2(m) + pf2(m + 2)) * 0.5;
        float2 hs = (pf2(m + 2) - pf2(m)) * 0.5;
        float2 q = abs(p - c) - hs + 10.0;
        float sd = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - 10.0;
        float a = P[m + 4];
        // A negative alpha is a selection: steady, dashed, in cyan.
        float3 col = float3(1.0, 0.36, 0.48);
        if (a < 0.0) {
            a = -a;
            col = float3(0.35, 0.86, 1.0);
            a *= step(frac((p.x + p.y) / 14.0), 0.62);
        }
        float line = saturate(1.5 - abs(sd)) * a;
        float halo = saturate(1.0 - abs(sd) / 14.0) * 0.25 * a;
        o = lerp(o, col, saturate(line + halo));
    }
    // Paused: two bars at the left end.
    if (P[20] < 0.5) {
        float2 q = p - float2(x0, yc - 22.0);
        float bars = (step(0.0, q.x) * step(q.x, 4.0) + step(8.0, q.x) * step(q.x, 12.0)) * step(abs(q.y), 7.0);
        o = lerp(o, float3(1.0), saturate(bars));
    }
    return o;
}

[shader("compute")]
[numthreads(16, 16, 1)]
void k_present(uint3 gid: SV_DispatchThreadID) {
    float2 dsize = pf2(10);
    if (gid.x >= uint(dsize.x) || gid.y >= uint(dsize.y)) return;
    float4 r = pf4(6);
    float2 p = float2(gid.xy) + 0.5;
    float3 back = float3(P[12], P[13], P[14]);
    float2 uv = (p - r.xy) / r.zw;
    if (any(uv < 0.0) || any(uv > 1.0)) {
        OUT.Store(gid.xy, float4(present_hud(p, dsize, back), 1.0));
        return;
    }
    float4 c = sample_o(T0, uv * resolution(), 4);
    // Composite over a checkerboard so transparency is visible.
    float2 cell = floor(p / 12.0);
    float check = (int(cell.x + cell.y) & 1) == 0 ? 0.18 : 0.24;
    float3 lin = c.rgb + srgb_to_linear(float3(check)) * (1.0 - c.a);
    float3 o = linear_to_srgb(saturate(lin));
    OUT.Store(gid.xy, float4(present_hud(p, dsize, o), 1.0));
}

// Depth of field: T0 color, T1 camera distance (r). Params: [4] focus
// distance, [5] aperture (blur radius in px far from focus), [6] taps.
// A gather where each tap counts only if its own circle of confusion
// reaches this pixel, so sharp foreground does not smear and blurred
// foreground spills over what is behind it.
float coc_px(float d) {
    return min(P[5], P[5] * abs(d - P[4]) / max(d, 1e-3));
}

// The gather runs at reduced resolution (header [1] = output texel size in
// frame px); k_dof_mix brings it back over the sharp image by each pixel's
// own blur, so focused detail stays full resolution.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_dof(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    float2 p = (float2(gid.xy) + 0.5) * P[1];
    uint2 c0 = uint2(min(p, resolution() - 1.0));
    float own_d = load(T1, c0).r;
    float own = coc_px(own_d);
    float4 acc = load(T0, c0);
    float wsum = 1.0;
    int n = int(P[6]);
    // A per-pixel rotation turns the sparse disk's ghost images into fine
    // noise.
    float spin = frac(sin(dot(float2(gid.xy), float2(12.9898, 78.233))) * 43758.5453) * 6.2831853;
    for (int k = 0; k < n; k++) {
        float r = P[5] * sqrt((float(k) + 0.5) / float(n));
        float a = float(k) * 2.39996 + spin;
        int2 qi = int2(p + r * float2(cos(a), sin(a)));
        if (any(qi < 0) || any(qi >= int2(resolution()))) continue;
        float d = load(T1, uint2(qi)).r;
        float c = coc_px(d);
        // What is behind us blurs no further onto us than we are blurred.
        if (d > own_d * 1.02) c = min(c, own);
        float w = saturate(c - r + 1.0);
        acc += load(T0, uint2(qi)) * w;
        wsum += w;
    }
    OUT.Store(gid.xy, acc / wsum);
}

// T0 sharp, T1 blurred (texel size [4] frame px), T2 distance.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_dof_mix(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    float c = coc_px(load(T2, gid.xy).r);
    float4 sharp = load(T0, gid.xy);
    float4 soft = sample_region(T1, float2(gid.xy) + 0.5, float2(0.0), P[7]);
    OUT.Store(gid.xy, lerp(sharp, soft, smoothstep(0.6, 2.5, c)));
}

// Motion blur accumulation: OUT = (first ? 0 : T1) + T0 * weight, where T0
// is a placed sample (origin [4..6]) and T1 the running sum. Params: [6]
// weight, [7] first.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_accumulate(uint3 gid: SV_DispatchThreadID) {
    if (outside_frame(gid.xy)) return;
    float4 acc = P[7] > 0.5 ? float4(0.0) : load(T1, gid.xy);
    OUT.Store(gid.xy, acc + load_o(T0, gid.xy, 4) * P[6]);
}

// ---------------------------------------------------------------- regions
// Region-limited, multi-resolution blur. Textures here cover a region of the
// frame: origin O (frame px) and texel size s (frame px per texel).

float4 sample_region(Texture2D<float4> t, float2 p, float2 origin, float scale) {
    uint w, h;
    t.GetDimensions(w, h);
    return t.SampleLevel(S, (p - origin) / scale / float2(w, h), 0);
}

// Separable gaussian over a region. Params: [4] sigma (frame px), [5..7]
// direction, [7] output texel size, [8..10] output origin, [10..12] input
// origin, [12] input texel size.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_blur_region(uint3 gid: SV_DispatchThreadID) {
    uint w, h;
    OUT.GetDimensions(w, h);
    if (gid.x >= w || gid.y >= h) return;
    float s_out = P[7];
    float2 p = pf2(8) + (float2(gid.xy) + 0.5) * s_out;
    float2 dir = pf2(5);
    float2 in_o = pf2(10);
    float s_in = P[12];
    float sig = max(P[4] / s_out, 1e-3);
    int radius = int(ceil(sig * 3.0));
    float inv2s2 = 1.0 / (2.0 * sig * sig);
    float4 acc = sample_region(T0, p, in_o, s_in);
    float wsum = 1.0;
    for (int i = 1; i <= radius; i += 2) {
        float w0 = exp(-float(i * i) * inv2s2);
        float w1 = exp(-float((i + 1) * (i + 1)) * inv2s2);
        float wt = w0 + w1;
        float off = (float(i) + w1 / max(wt, 1e-8)) * s_out;
        acc += (sample_region(T0, p + dir * off, in_o, s_in) + sample_region(T0, p - dir * off, in_o, s_in)) * wt;
        wsum += 2.0 * wt;
    }
    OUT.Store(gid.xy, acc / wsum);
}

// Resamples a region texture onto another grid, averaging n x n bilinear
// taps per output texel (n = 1 for upsampling). Params: [4] output texel
// size, [5..7] output origin, [7..9] input origin, [9] input texel size,
// [10] n.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_resample(uint3 gid: SV_DispatchThreadID) {
    uint w, h;
    OUT.GetDimensions(w, h);
    if (gid.x >= w || gid.y >= h) return;
    float s_out = P[4];
    float2 base = pf2(5) + float2(gid.xy) * s_out;
    int n = max(int(P[10] + 0.5), 1);
    float4 acc = float4(0.0);
    for (int y = 0; y < n; y++) {
        for (int x = 0; x < n; x++) {
            float2 p = base + (float2(x, y) + 0.5) / float(n) * s_out;
            acc += sample_region(T0, p, pf2(7), P[9]);
        }
    }
    OUT.Store(gid.xy, acc / float(n * n));
}

groupshared int diff_sums[256];

// Squared error between the frame (T0, placed, origin [4..6]) and a
// reference (T1, sRGB-encoded RGBA8, frame-sized), both composited over
// black and compared in 8-bit sRGB units, as viewers see them. Each 16x16
// threadgroup writes its partial sum to B1[group index].
[shader("compute")]
[numthreads(16, 16, 1)]
void k_diff(uint3 gid: SV_DispatchThreadID, uint3 gtid: SV_GroupThreadID, uint3 grp: SV_GroupID) {
    uint li = gtid.y * 16 + gtid.x;
    int e = 0;
    if (!outside_frame(gid.xy)) {
        float4 c = load_o(T0, gid.xy, 4);
        float3 ours = round(linear_to_srgb(saturate(c.rgb)) * 255.0);
        float4 r = load(T1, gid.xy);
        float3 theirs = round(r.rgb * r.a * 255.0);
        float3 d = ours - theirs;
        e = int(dot(d, d));
    }
    diff_sums[li] = e;
    GroupMemoryBarrierWithGroupSync();
    for (uint s = 128; s > 0; s >>= 1) {
        if (li < s) diff_sums[li] += diff_sums[li + s];
        GroupMemoryBarrierWithGroupSync();
    }
    if (li == 0) {
        uint groups_x = (uint(P[2]) + 15) / 16;
        B1[grp.y * groups_x + grp.x] = diff_sums[0];
    }
}

// ---------------------------------------------------------------- fluid
// Stable-fluids solver on grids covering the frame. A grid texel spans
// P[1] frame px (the output grid's); positions are frame px, velocities
// frame px per second, y down. Common params: [4] dt, [5] velocity texel.

bool outside_grid(uint2 gid) {
    uint w, h;
    OUT.GetDimensions(w, h);
    return gid.x >= w || gid.y >= h;
}

float4 grid_at(Texture2D<float4> t, int2 c) {
    uint w, h;
    t.GetDimensions(w, h);
    return t.Load(int3(clamp(c, int2(0), int2(w, h) - 1), 0));
}

// Vorticity: OUT.x = dvy/dx - dvx/dy of the velocity T0.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_curl(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    int2 c = int2(gid.xy);
    float r = grid_at(T0, c + int2(1, 0)).y, l = grid_at(T0, c - int2(1, 0)).y;
    float d = grid_at(T0, c + int2(0, 1)).x, u = grid_at(T0, c - int2(0, 1)).x;
    OUT.Store(gid.xy, float4((r - l - (d - u)) / (2.0 * P[1]), 0.0, 0.0, 0.0));
}

// Forces on the velocity T0: vorticity confinement (curl T1), curl-noise
// turbulence, buoyancy of the dye (T2, texel [6]), and a push away from /
// swirl around the edges of the stir image (T3, placed, origin [12..14]).
// Params: [7] vorticity, [8] turbulence (px/s^2), [9] turbulence scale (px),
// [10] noise time, [11] buoyancy (px/s^2 per unit dye alpha), [14] push,
// [15] swirl, [16] stir connected.
float turb_potential(float2 q, float z) {
    return snoise(float3(q, z)) + 0.5 * snoise(float3(q * 2.03 + 11.7, z * 1.3));
}

[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_force(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    int2 c = int2(gid.xy);
    float h = P[1];
    float dt = P[4];
    float2 p = (float2(gid.xy) + 0.5) * h;
    float2 v = grid_at(T0, c).xy;
    // Vorticity confinement: spin eddies back up. The gradient of |curl|
    // is taken over two texels so grid-scale noise is not amplified.
    float w = grid_at(T1, c).x;
    float2 g = float2(abs(grid_at(T1, c + int2(2, 0)).x) - abs(grid_at(T1, c - int2(2, 0)).x),
                      abs(grid_at(T1, c + int2(0, 2)).x) - abs(grid_at(T1, c - int2(0, 2)).x));
    g /= length(g) + 1e-3 * abs(w) + 1e-5;
    float2 f = 0.25 * P[7] * h * float2(g.y, -g.x) * w;
    // Divergence-free turbulence from a noise potential.
    if (P[8] != 0.0) {
        float2 q = p / max(P[9], 1.0);
        float e = 0.02;
        float z = P[10];
        float a = turb_potential(q + float2(0.0, e), z) - turb_potential(q - float2(0.0, e), z);
        float b = turb_potential(q + float2(e, 0.0), z) - turb_potential(q - float2(e, 0.0), z);
        f += P[8] * float2(a, -b) / (2.0 * e) * 0.3;
    }
    // Buoyancy: dye rises.
    if (P[11] != 0.0) {
        f.y -= P[11] * sample_region(T2, p, float2(0.0), P[6]).a;
    }
    // Stir: the edges of the stir image push the fluid out and around.
    if (P[16] > 0.5) {
        float ax = sample_o(T3, p + float2(h, 0.0), 12).a - sample_o(T3, p - float2(h, 0.0), 12).a;
        float ay = sample_o(T3, p + float2(0.0, h), 12).a - sample_o(T3, p - float2(0.0, h), 12).a;
        float2 ga = float2(ax, ay) * 0.5;
        f += -P[14] * ga + P[15] * float2(-ga.y, ga.x);
    }
    OUT.Store(gid.xy, float4(v + f * dt, 0.0, 0.0));
}

// Divergence of the velocity T0.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_div(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    int2 c = int2(gid.xy);
    float r = grid_at(T0, c + int2(1, 0)).x, l = grid_at(T0, c - int2(1, 0)).x;
    float d = grid_at(T0, c + int2(0, 1)).y, u = grid_at(T0, c - int2(0, 1)).y;
    OUT.Store(gid.xy, float4((r - l + d - u) / (2.0 * P[1]), 0.0, 0.0, 0.0));
}

// One Jacobi iteration of the pressure Poisson equation: pressure T0,
// divergence T1. [7] scales the previous pressure (warm start decay).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_jacobi(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    int2 c = int2(gid.xy);
    float s = P[7];
    float sum = grid_at(T0, c + int2(1, 0)).x + grid_at(T0, c - int2(1, 0)).x
              + grid_at(T0, c + int2(0, 1)).x + grid_at(T0, c - int2(0, 1)).x;
    float div = grid_at(T1, c).x;
    OUT.Store(gid.xy, float4((sum * s - div * P[1] * P[1]) * 0.25, 0.0, 0.0, 0.0));
}

// Subtracts the pressure gradient (T1) from the velocity (T0).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_project(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    int2 c = int2(gid.xy);
    float2 gp = float2(grid_at(T1, c + int2(1, 0)).x - grid_at(T1, c - int2(1, 0)).x,
                       grid_at(T1, c + int2(0, 1)).x - grid_at(T1, c - int2(0, 1)).x) / (2.0 * P[1]);
    OUT.Store(gid.xy, float4(grid_at(T0, c).xy - gp, 0.0, 0.0));
}

// Semi-Lagrangian advection of the field T1 (texel [7]) through the
// velocity T0 plus wind [8..10], times [12] (dissipation). With [14] set,
// the source T2 (placed, origin [10..12]) is laid over it at rate [13]:
// out = src * k + field * (1 - src.a * k).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_fl_advect(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    float2 p = (float2(gid.xy) + 0.5) * P[1];
    float2 v = sample_region(T0, p, float2(0.0), P[5]).xy + pf2(8);
    float4 f = sample_region(T1, p - P[4] * v, float2(0.0), P[7]) * P[12];
    if (P[14] > 0.5) {
        float k = P[13];
        float4 src = sample_o(T2, p, 10);
        f = src * k + f * (1.0 - src.a * k);
    }
    OUT.Store(gid.xy, f);
}

// Catmull-Rom (bicubic) sample at texel coordinates `p`: repeated
// resampling (feedback zooms and rotations) stays sharp instead of
// diffusing like bilinear does.
float4 sample_sharp(Texture2D<float4> t, float2 p) {
    float2 q = p - 0.5;
    float2 i = floor(q);
    float2 f = q - i;
    float2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    float2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    float2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    float2 w3 = f * f * (-0.5 + 0.5 * f);
    float4 acc = float4(0.0);
    float wx[4] = { w0.x, w1.x, w2.x, w3.x };
    float wy[4] = { w0.y, w1.y, w2.y, w3.y };
    uint w, h;
    t.GetDimensions(w, h);
    for (int y = 0; y < 4; y++) {
        for (int x = 0; x < 4; x++) {
            int2 c = int2(i) + int2(x - 1, y - 1);
            float4 v = (any(c < 0) || c.x >= int(w) || c.y >= int(h)) ? float4(0.0) : t.Load(int3(c, 0));
            acc += v * wx[x] * wy[y];
        }
    }
    return acc;
}

// OUT = T0 over T1, both frame-sized and frame-aligned.
[shader("compute")]
[numthreads(16, 16, 1)]
void k_over_frames(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    float4 a = load(T0, gid.xy);
    OUT.Store(gid.xy, a + load(T1, gid.xy) * (1.0 - a.a));
}

// Copies T0 into OUT texel for texel (checkpoints).
[shader("compute")]
[numthreads(16, 16, 1)]
void k_copy(uint3 gid: SV_DispatchThreadID) {
    if (outside_grid(gid.xy)) return;
    OUT.Store(gid.xy, load(T0, gid.xy));
}

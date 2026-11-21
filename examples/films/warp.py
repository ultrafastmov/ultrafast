"""WARP — a jump to light speed: a procedural starfield whose streaks
stretch with speed, words that rush through the camera, radial chromatic
aberration, and the drop out of warp. 1080x1920, 8 s."""

import math

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(6, 0.5))
W, H = f.W, f.H


def speed(t):
    # cruise, punch to warp, hold, drop out
    if t < 1.0:
        return 0.25
    if t < 2.0:
        u = (t - 1.0) / 1.0
        return 0.25 + 2.9 * u * u
    if t < 5.6:
        return 3.15
    if t < 6.4:
        u = (t - 5.6) / 0.8
        return 3.15 + (0.12 - 3.15) * (1 - (1 - u) ** 3)
    return 0.12


travel, s_keys, d_keys = 0.0, [], []
for i in range(0, 161):
    t = i * 0.05
    s_keys.append((t, round(speed(t), 4)))
    d_keys.append((t, round(travel, 4)))
    travel += speed(t) * 0.05 * 0.35

STARS = """
float h1(float n) { return frac(sin(n * 12.9898) * 43758.5453); }
float4 shade(float2 pos, float px) {
  float2 c = size() * 0.5;
  float2 d = pos - c;
  float r = length(d) / size().y;
  float a = atan2(d.y, d.x) / 6.2831853 + 0.5;
  float travel = param(0), speed = param(1);
  float3 col = float3(0.0);
  for (int L = 0; L < 4; L++) {
    float sectors = 180.0 + 150.0 * float(L);
    float fs = a * sectors;
    float s = floor(fs);
    float h = h1(s * 3.7 + float(L) * 19.1);
    float z = frac(h * 9.1 + travel * (0.8 + 0.25 * float(L)));
    float rr = 0.015 + pow(z, 2.2) * 1.15;
    float len = 0.002 + speed * 0.09 * pow(z, 1.6);
    float across = abs(frac(fs) - 0.5) / sectors * 6.2831853 * r * size().y;
    float w = 0.5 + 1.6 * z;
    float lat = smoothstep(w + px, 0.0, across);
    float lon = smoothstep(0.0, 0.004, r - (rr - len)) * smoothstep(0.004, 0.0, r - rr);
    float3 tint = lerp(float3(0.55, 0.7, 1.0), float3(1.0, 0.85, 0.7), h1(s + 7.0));
    col += tint * lat * lon * (0.35 + 1.8 * z * z) * step(0.25, h1(s * 1.3 + float(L)));
  }
  // The tunnel's heart.
  col += float3(0.25, 0.4, 1.0) * 0.6 * exp(-r * 9.0) * (0.3 + 0.3 * speed);
  return float4(col, 1.0);
}"""

CA = """
float4 shade(float2 pos, float px) {
  float2 c = size() * 0.5;
  float2 dir = (pos - c) / size().y;
  float k = param(0);
  float4 g = input(0, pos);
  float2 lo = float2(0.5), hi = size() - 0.5;
  float r = input(0, clamp(pos + dir * k, lo, hi)).r;
  float b = input(0, clamp(pos - dir * k, lo, hi)).b;
  return float4(r, g.g, b, g.a);
}"""

stars = f.add("shader", {"code": STARS, "u0": keys(*d_keys), "u1": keys(*s_keys)}, name="stars")

words = [("SHADERS", 2.3), ("PARTICLES", 3.0), ("3D", 3.7), ("TYPE", 4.4), ("ONE GRAPH", 5.0)]
layers = []
for word, t0 in words:
    tx = f.text(word, "Unbounded-VF.ttf", 120, [W / 2, H / 2 + 42], align="center", color="#eaf1ff", weight=700,
                tracking=6, name="fly")
    # Exponential approach: the word rushes from the vanishing point through the lens.
    ks = [(t0 + 0.9 * i / 12, 0.3 * math.exp(3.2 * i / 12)) for i in range(13)]
    xf = f.add("transform", {"anchor": [W / 2, H / 2], "position": [W / 2, H / 2],
                             "scale": keys(*[(t, round(s, 4)) for t, s in ks])}, {"source": tx}, name="fly_xf")
    fade = f.add("opacity", {"opacity": keys((t0, 0.0, "linear"), (t0 + 0.25, 1.0), (t0 + 0.7, 1.0, "linear"), (t0 + 0.9, 0.0))},
                 {"source": xf}, name="fly_o")
    layers.append((fade, None, {"in": t0, "out": t0 + 0.9}))

flash = f.add("solid", {"color": "#cfe0ff"}, name="flash")
mark = f.text("ultrafast", "Unbounded-VF.ttf", 210, [W / 2, H / 2 + 70], align="center", color="#ffffff", weight=800, tracking=-6,
              stagger=0.05, stagger_order="center", glyph_scale=keys((0.0, 2.2, OUT), (0.6, 1.0)),
              glyph_opacity=keys((0.0, 0.0), (0.3, 1.0)), name="mark")
f.nodes[mark]["timing"] = {"offset": 6.1}
tag = f.text("0 → 3,200 FPS", "GeistMono-VF.ttf", 36, [W / 2, H / 2 + 170], align="center", color="#cfe0ffb0", weight=600,
             tracking=5, name="tag")
tag = f.add("opacity", {"opacity": keys((6.7, 0.0, SOFT), (7.3, 1.0))}, {"source": tag}, name="tag_o")

comp = f.stack(stars, layers + [(flash, {"opacity": keys((5.55, 0.0, "linear"), (5.62, 0.85), (6.1, 0.0))}, {"in": 5.55, "out": 6.1}),
                                (mark, None, {"in": 6.1}), (tag, None, {"in": 6.7})])
bloom = f.add("blur", {"radius": 18}, {"source": comp}, name="bloom")
comp = f.blend(bloom, comp, "add", opacity=0.6, name="bloomed")
ca = f.add("shader", {"code": CA, "u0": keys(*[(t, round(4 + 9 * s, 3)) for t, s in s_keys[::4]])}, {"in0": comp}, name="aberration")
f.output = finish(f, ca, grain=0.06)
f.save(os.path.join(FILMS_DIR, "warp.json"))
print("ok", len(f.nodes), "nodes")

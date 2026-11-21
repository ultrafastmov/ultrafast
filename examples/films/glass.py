"""GLASS — refracting, frosted glass over living color. Each pane is a
shader that bends and frosts the live composition beneath it. 1080x1920, 8 s."""

import math

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(10, 0.5))
W, H = f.W, f.H

GLASS = """
float4 shade(float2 pos, float px) {
  float2 c = float2(param(0), param(1));
  float2 hs = float2(param(2), param(3)) * 0.5;
  float r = param(4);
  float2 q = pos - c;
  float d = sd_round_box(q, hs, r);
  if (d > 2.0) return float4(0.0);
  float aa = saturate(0.5 - d / max(px, 1e-3));
  float e = 1.0;
  float2 g = float2(sd_round_box(q + float2(e, 0.0), hs, r) - sd_round_box(q - float2(e, 0.0), hs, r),
                    sd_round_box(q + float2(0.0, e), hs, r) - sd_round_box(q - float2(0.0, e), hs, r));
  float2 n = g / max(length(g), 1e-5);
  // The rim is a lens: bend light inwards near the edge.
  float h = saturate(1.0 + d / param(6));
  float2 sp = pos - n * h * h * param(5);
  // Frost: a golden-angle disk of taps.
  float rad = param(7);
  float4 acc = float4(0.0);
  for (int k = 0; k < 16; k++) {
    float rr = rad * sqrt((float(k) + 0.5) / 16.0);
    float a = float(k) * 2.39996;
    acc += input(0, sp + rr * float2(cos(a), sin(a)));
  }
  float3 col = acc.rgb / 16.0;
  col = col * param(8) + param(9);
  // Light catches the upper-left rim; a softer return on the lower right.
  float rim = pow(h, 5.0);
  float lit = saturate(dot(n, float2(-0.55, -0.83)));
  col += rim * (0.55 * lit + 0.12) * param(10);
  // A hairline at the very edge.
  col += smoothstep(1.8, 0.0, abs(d + 1.0)) * 0.22 * param(10);
  return float4(col * aa, aa);
}"""


def blob(color, path, radius, t0=0.0):
    cx, cy, ax, ay, fx, fy, ph = path
    ks = []
    for i in range(0, 41):
        t = 8.0 * i / 40
        ks.append((t, [cx + ax * math.sin(fx * t + ph), cy + ay * math.cos(fy * t + ph * 1.3)]))
    return f.add("gradient", {"kind": "radial", "center": keys(*ks), "radius": radius,
                              "stops": f"0:{color}ff, 0.45:{color}90, 1:{color}00"}, name="blob")


base = f.add("gradient", {"kind": "linear", "start": [0, 0], "end": [W, H], "stops": "0:#0a0f2e, 1:#160a2a"}, name="base")
blobs = [
    blob("#2f6bff", (300, 500, 220, 260, 0.55, 0.42, 0.0), 760),
    blob("#ff4fa3", (820, 900, 260, 300, 0.47, 0.61, 1.7), 680),
    blob("#ff9a3c", (420, 1450, 300, 200, 0.62, 0.38, 3.1), 640),
    blob("#8a3ffc", (760, 1650, 200, 260, 0.41, 0.53, 4.4), 720),
    blob("#34e0ff", (540, 260, 320, 160, 0.66, 0.47, 5.2), 520),
]
bg = f.add("noise", {"a": "#ffffff00", "b": "#ffffff14", "scale": [520, 520], "octaves": 3,
                     "evolution": keys((0, 0), (8, 2.4))}, name="mist")
sky = f.stack(base, blobs + [bg])


def pane(under, center, size, radius, frost=26, bend=36, bevel=34, gain=0.92, lift=0.06, light=1.0, name="pane"):
    return f.add("shader", {"code": GLASS, "u0": center[0], "u1": center[1], "u2": size[0], "u3": size[1], "u4": radius,
                            "u5": bend, "u6": bevel, "u7": frost, "u8": gain, "u9": lift, "u10": light},
                 {"in0": under}, name=name)


def anim_xy(ks):
    """keys of [x, y] -> (x keys, y keys) for the shader's scalar params."""
    xs = keys(*[(k[0], k[1][0]) + tuple(k[2:]) for k in ks])
    ys = keys(*[(k[0], k[1][1]) + tuple(k[2:]) for k in ks])
    return xs, ys


def shadow(center_ks, size, radius, name="shadow"):
    s = f.add("shape", {"kind": "rect", "center": center_ks, "size": size, "radius": radius, "color": "#04030a"}, name=name)
    s = f.add("transform", {"position": [W / 2, H / 2 + 36], "anchor": [W / 2, H / 2]}, {"source": s}, name=name + "_drop")
    return f.add("blur", {"radius": 46}, {"source": s}, name=name + "_soft")


geist, mono = "Geist-VF.ttf", "GeistMono-VF.ttf"

# Pane 1: the player.
p1 = [(0.0, [W / 2, 2400], OUT), (1.1, [W / 2, 560]), (5.6, [W / 2, 560], IN), (6.3, [W / 2, -700])]
p1x, p1y = anim_xy(p1)
c1 = keys(*p1)
sh1 = shadow(c1, [920, 600], 64, "sh1")
g1 = pane(f.stack(sky, [(sh1, {"opacity": 0.55}, None)]), (0, 0), (920, 600), 64, gain=0.62, lift=0.0, name="glass1")
f.nodes[g1]["params"].update({"u0": p1x, "u1": p1y})


def ui(text, fnt, size, dx, dy, center_ks, color="#ffffffe6", **kw):
    t = f.text(text, fnt, size, [W / 2 + dx, H / 2 + dy], color=color, **kw)
    ks = [(k[0], [k[1][0] - W / 2, k[1][1] - H / 2]) + tuple(k[2:]) for k in center_ks]
    return f.add("transform", {"position": keys(*[(k[0], [W / 2 + k[1][0], H / 2 + k[1][1]]) + tuple(k[2:]) for k in ks]),
                               "anchor": [W / 2, H / 2]}, {"source": t}, name="ui")


u1 = [
    ui("NOW RENDERING", mono, 26, -400, -210, p1, color="#ffffffd0", weight=500, tracking=3),
    ui("Liquid Glass", geist, 92, -404, -110, p1, weight=650, tracking=-3),
    ui("8 panes · 16 taps · every pixel", geist, 34, -402, -40, p1, color="#ffffffe0", weight=400),
    ui("0.41 ms", mono, 30, 400, 228, p1, color="#ffffffc0", weight=500, align="right"),
    ui("1080 × 1920", mono, 30, -400, 228, p1, color="#ffffffc0", weight=500),
]
track = f.add("shape", {"kind": "rect", "center": [W / 2, H / 2 + 150], "size": [800, 10], "radius": 5, "color": "#ffffff30"}, name="track")
fill = f.add("shape", {"kind": "rect", "center": keys((1.0, [W / 2 - 400, H / 2 + 150], SOFT), (5.4, [W / 2 - 40, H / 2 + 150])),
                       "size": keys((1.0, [0, 10], SOFT), (5.4, [720, 10])), "radius": 5, "color": "#ffffff"}, name="fill")
bar = f.add("transform", {"position": c1, "anchor": [W / 2, H / 2]}, {"source": f.stack(track, [fill])}, name="bar")

# Pane 2: a square control, and pane 3: a pill, side by side.
p2 = [(0.0, [-500, 1230], OUT), (1.5, [300, 1230]), (5.7, [300, 1230], IN), (6.4, [300, -500])]
p3 = [(0.0, [1600, 1230], OUT), (1.7, [770, 1230]), (5.75, [770, 1230], IN), (6.45, [770, -500])]
sh2 = shadow(keys(*p2), [400, 400], 90, "sh2")
sh3 = shadow(keys(*p3), [400, 400], 200, "sh3")
under2 = f.stack(sky, [(sh1, {"opacity": 0.55}, None), g1] + u1 + [bar, (sh2, {"opacity": 0.5}, None),
                                                                   (sh3, {"opacity": 0.5}, None)])
g2 = pane(under2, (0, 0), (400, 400), 90, frost=34, name="glass2")
g3 = pane(under2, (0, 0), (400, 400), 200, frost=10, bend=60, bevel=60, gain=1.02, lift=0.02, name="glass3")
for g, p in ((g2, p2), (g3, p3)):
    x, y = anim_xy(p)
    f.nodes[g]["params"].update({"u0": x, "u1": y})

play = f.add("path", {"kind": "polygon", "center": [W / 2, H / 2], "radius": 70, "sides": 3, "color": "#ffffffee",
                      "rotation": keys((1.5, 0, OUT), (2.4, 360))}, name="play")
play = f.add("transform", {"position": keys(*[(k[0], [k[1][0] + 8, k[1][1]]) + tuple(k[2:]) for k in p2]),
                           "anchor": [W / 2, H / 2]}, {"source": play}, name="play_at")
lens_txt = ui("f/1.4", geist, 72, 0, 24, p3, weight=300, align="center", tracking=-2)

# Finale: one wide pane carries the wordmark.
p4 = [(5.9, [W / 2, 2300], OUT), (6.9, [W / 2, 960])]
p4x, p4y = anim_xy(p4)
sh4 = shadow(keys(*p4), [880, 360], 180, "sh4")
under4 = f.stack(sky, [(sh4, {"opacity": 0.6}, {"in": 5.9})])
g4 = pane(under4, (0, 0), (880, 360), 180, frost=30, bend=70, bevel=80, name="glass4")
f.nodes[g4]["params"].update({"u0": p4x, "u1": p4y})
mark = ui("ultrafast", geist, 200, 0, 66, p4, weight=keys((6.4, 200, OUT), (7.6, 720)), align="center", tracking=-8)
tag = f.text("glass, live, on the GPU", mono, 34, [W / 2, 1260], align="center", color="#ffffff", weight=500, tracking=2, name="tag")
tag = f.add("drop_shadow", {"color": "#1a0626c0", "radius": 18, "offset": [0, 4]}, {"source": tag}, name="tag_shadow")
tag = f.add("opacity", {"opacity": keys((6.5, 0.0, SOFT), (6.9, 1.0))}, {"source": tag}, name="tag_in")

comp = f.stack(under2, [(g2, None, {"in": 0.2, "out": 6.5}), (g3, None, {"in": 0.2, "out": 6.5}), play, lens_txt,
                        (sh4, {"opacity": 0.6}, {"in": 5.9}), (g4, None, {"in": 5.9}), (mark, None, {"in": 5.9}),
                        (tag, None, {"in": 6.5})])
f.output = finish(f, comp, grain=0.06)
f.save(os.path.join(FILMS_DIR, "glass.json"))
print("ok", len(f.nodes), "nodes")

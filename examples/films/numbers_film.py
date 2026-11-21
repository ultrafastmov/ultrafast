"""NUMBERS — editorial data visualization: a line chart that draws itself
with a glowing head and a gradient area, bars that grow with counting
labels, a donut that sweeps in, and counters everywhere. 1080x1920, 12 s.
(The figures are illustrative.)"""

import math

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=12.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX = W / 2
SCORE = "../../out/films/hero_score.wav"
MONO, SANS, UNB, SERIF_I = "GeistMono-VF.ttf", "Geist-VF.ttf", "Unbounded-VF.ttf", "InstrumentSerif-Italic.ttf"
BG, INK, DIM = "#0b1020", "#f3f1ec", "#f3f1ec80"
AMBER, CORAL, TEAL = "#ffb547", "#ff5c7a", "#2ee6c5"
CUTS = [0.0, 3.4, 6.6, 9.6, 12.0]

f.add("audio", {"src": SCORE, "start": 12.0, "gain": keys((0, 0.0), (0.5, 0.5), (11.0, 0.5), (12.0, 0.0))}, name="score")

bg = f.add("gradient", {"kind": "radial", "center": [CX, 700], "radius": 1500,
                        "stops": "0:#16203d, 0.6:#0b1020, 1:#05070f"}, name="bg")
GRID = """
float4 shade(float2 pos, float px) {
  float2 g = abs(frac(pos / 90.0 + 0.5) - 0.5) * 90.0;
  float line = smoothstep(1.2, 0.2, min(g.x, g.y));
  return float4(float3(1.0), 1.0) * line * 0.045;
}"""
grid = f.add("shader", {"code": GRID}, name="grid")
base = f.over(grid, bg, name="base")


def label(text, x, y, size=30, color=DIM, align="left", font_=MONO, name="label", **kw):
    return f.text(text, font_, size, [x, y], color=color, align=align, weight=500, tracking=2, name=name, **kw)


def fade_in(nid, t, dur=0.4, name="fade"):
    return f.add("opacity", {"opacity": keys((t, 0.0, SOFT), (t + dur, 1.0))}, {"source": nid}, name=name)


def window(i):
    return {"in": CUTS[i], "out": CUTS[i + 1]}


# ---------------------------------------------------------------- 1 line chart
t = CUTS[0]
vals = [12, 18, 15, 26, 22, 34, 31, 45, 41, 58, 63, 77]
x0, x1, ybase, ytop = 110, 970, 1300, 760
pts = [(x0 + (x1 - x0) * i / (len(vals) - 1), ybase - (ybase - ytop) * v / max(vals)) for i, v in enumerate(vals)]


def smooth(points):
    d = f"M {points[0][0]:.1f} {points[0][1]:.1f}"
    for i in range(len(points) - 1):
        p0 = points[max(i - 1, 0)]
        p1, p2 = points[i], points[i + 1]
        p3 = points[min(i + 2, len(points) - 1)]
        c1 = (p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6)
        c2 = (p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6)
        d += f" C {c1[0]:.1f} {c1[1]:.1f} {c2[0]:.1f} {c2[1]:.1f} {p2[0]:.1f} {p2[1]:.1f}"
    return d


line_d = smooth(pts)
draw0, draw1 = t + 0.5, t + 2.4
DRAW = {"bezier": [0.45, 0.0, 0.2, 1.0]}
line = f.add("path", {"d": line_d, "color": AMBER, "stroke": 7, "cap": "round", "join": "round",
                      "trim_end": keys((draw0, 0.0, DRAW), (draw1, 1.0))}, name="line")
glow_line = f.add("blur", {"radius": 14}, {"source": line}, name="line_glow")
# Area under the line, revealed left to right with it.
area = f.add("path", {"d": line_d + f" L {x1} {ybase} L {x0} {ybase} Z", "color": "#ffffff"}, name="area")
area_fill = f.add("gradient", {"kind": "linear", "start": [0, ytop], "end": [0, ybase],
                               "stops": "0:#ffb54766, 1:#ffb54700"}, name="area_fill")
area = f.add("mask", {}, {"source": area_fill, "matte": area}, name="area_masked")


def ease_draw(u):
    # cubic-bezier(0.45, 0, 0.2, 1), solved numerically
    lo, hi = 0.0, 1.0
    for _ in range(40):
        mid = (lo + hi) / 2
        x = 3 * (1 - mid) ** 2 * mid * 0.45 + 3 * (1 - mid) * mid ** 2 * 0.2 + mid ** 3
        lo, hi = (mid, hi) if x < u else (lo, mid)
    s = (lo + hi) / 2
    return 3 * (1 - s) * s ** 2 * 1.0 + s ** 3


# The head follows the drawn end: sample the curve by arc length.
samples = []
for i in range(len(pts) - 1):
    p0 = pts[max(i - 1, 0)]
    p1, p2 = pts[i], pts[i + 1]
    p3 = pts[min(i + 2, len(pts) - 1)]
    c1 = (p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6)
    c2 = (p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6)
    for k in range(40):
        u = k / 40
        a = [(1 - u) ** 3 * p1[j] + 3 * (1 - u) ** 2 * u * c1[j] + 3 * (1 - u) * u ** 2 * c2[j] + u ** 3 * p2[j] for j in range(2)]
        samples.append(a)
samples.append(list(pts[-1]))
cum = [0.0]
for a, b in zip(samples, samples[1:]):
    cum.append(cum[-1] + math.dist(a, b))
L = cum[-1]


def at_length(s):
    for i in range(len(cum) - 1):
        if cum[i + 1] >= s:
            u = (s - cum[i]) / max(cum[i + 1] - cum[i], 1e-9)
            return [samples[i][j] + (samples[i + 1][j] - samples[i][j]) * u for j in range(2)]
    return samples[-1]


head_keys = []
for k in range(0, 58):
    tt = draw0 + (draw1 - draw0) * k / 57
    head_keys.append((round(tt, 3), [round(v, 1) for v in at_length(L * ease_draw(k / 57))]))
area_crop = f.crop(area, [0, 0, 1, H], name="area_crop")
f.nodes[area_crop]["params"]["rect"] = keys(*[(tt, [0, 0, max(1.0, v[0]), H]) for tt, v in head_keys])
head = f.add("shape", {"kind": "ellipse", "center": keys(*head_keys), "size": [26, 26], "color": "#fff3dc"}, name="head")
head_glow = f.add("shape", {"kind": "ellipse", "center": keys(*head_keys), "size": [110, 110], "color": "#ffb54755"},
                  name="head_halo")
head_glow = f.add("blur", {"radius": 18}, {"source": head_glow}, name="head_halo_soft")
axis = f.add("shape", {"kind": "rect", "center": [CX, ybase + 2], "size": [x1 - x0 + 40, 2], "color": "#ffffff30"},
             name="axis")
months = [label(m, x0 + (x1 - x0) * i / 11, ybase + 56, size=24, align="center", name="month")
          for i, m in enumerate("JFMAMJJASOND")]
big = f.text("", UNB, 150, [x0 - 6, 520], color=INK, weight=800, tracking=-6, name="big",
             number=keys((draw0, 0.0, DRAW), (draw1, 1284567.0)), thousands=",")
big_lab = label("FRAMES RENDERED THIS YEAR", x0, 610, size=28, name="big_label")
kicker = f.text("A year in frames", SERIF_I, 80, [x0, 330], color=INK, name="kicker")
kl, kw = line_reveal(f, kicker, x0, 330, 80, t + 0.15, dur=0.7)
s1 = f.stack(base, [axis] + months + [area_crop, glow_line, line, head_glow, head, big, fade_in(big_lab, draw0),
                                      (kl, None, kw)])

# ---------------------------------------------------------------- 2 bars
t = CUTS[1]
days = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"]
counts = [182, 240, 211, 309, 356, 128, 96]
bw, gap = 96, 30
total_w = len(days) * bw + (len(days) - 1) * gap
bx0 = CX - total_w / 2 + bw / 2
bbase, bmax = 1320, 640
bars = []
for i, (d, c) in enumerate(zip(days, counts)):
    x = bx0 + i * (bw + gap)
    h = bmax * c / max(counts)
    t0 = t + 0.35 + i * 0.09
    GROW = {"bezier": [0.2, 1.0, 0.3, 1.0]}
    col = CORAL if c == max(counts) else "#ffffff2c"
    bars.append(f.add("shape", {"kind": "rect", "radius": 14, "color": col,
                                "center": keys((t0, [x, bbase], GROW), (t0 + 0.9, [x, bbase - h / 2])),
                                "size": keys((t0, [bw, 0], GROW), (t0 + 0.9, [bw, h]))}, name="bar"))
    num = f.text("", UNB, 34, [x, bbase - h - 26], color=INK if c != max(counts) else CORAL, align="center", weight=700,
                 name="bar_num", number=keys((t0, 0.0, GROW), (t0 + 0.9, float(c))),
                 position=keys((t0, [x, bbase - 26], GROW), (t0 + 0.9, [x, bbase - h - 26])))
    bars.append(num)
    bars.append(label(d, x, bbase + 54, size=24, align="center", name="day"))
head2 = f.text("Busiest on Friday", SERIF_I, 80, [110, 330], color=INK, name="h2")
h2l, h2w = line_reveal(f, head2, 110, 330, 80, t + 0.1, dur=0.7)
sub2 = label("EXPORTS PER DAY, LAST WEEK", 110, 420, size=28, name="sub2")
s2 = f.stack(base, [f.add("shape", {"kind": "rect", "center": [CX, bbase + 2], "size": [total_w + 60, 2],
                                    "color": "#ffffff30"}, name="axis2")] + bars + [(h2l, None, h2w), fade_in(sub2, t + 0.3)])

# ---------------------------------------------------------------- 3 donut
t = CUTS[2]
shares = [(0.62, TEAL, "GPU KERNELS"), (0.23, AMBER, "3D PASSES"), (0.15, CORAL, "ENCODE")]
R, DC = 300, [CX, 930]
rings = [f.add("path", {"kind": "circle", "center": DC, "radius": R, "color": "#ffffff14", "stroke": 54}, name="track")]
start = 0.0
legend = []
for k, (share, col, name) in enumerate(shares):
    s0 = t + 0.3 + k * 0.25
    gap_ = 0.006
    rings.append(f.add("path", {"kind": "circle", "center": DC, "radius": R, "color": col, "stroke": 54, "cap": "butt",
                                "anchor": DC, "position": DC, "rotation": -90,
                                "trim_start": start + gap_,
                                "trim_end": keys((s0, start + gap_, {"bezier": [0.3, 0.0, 0.1, 1.0]}),
                                                 (s0 + 0.8, start + share - gap_))}, name="arc"))
    start += share
    ly = 1420 + k * 70
    sw = f.add("shape", {"kind": "rect", "center": [150, ly - 10], "size": [26, 26], "radius": 7, "color": col}, name="swatch")
    lt = label(name, 190, ly, size=28, color=INK, name="legend")
    pct = f.text("", UNB, 30, [950, ly], color=INK, align="right", weight=700, name="pct",
                 number=keys((s0, 0.0, OUT), (s0 + 0.8, share * 100)), suffix="%")
    legend.append(fade_in(f.stack(sw, [lt, pct]), s0, name="legend_in"))
center_num = f.text("", UNB, 120, [CX, 960], color=INK, align="center", valign="middle", weight=800, tracking=-4,
                    name="center_num", number=keys((t + 0.3, 0.0, OUT), (t + 1.6, 0.31)), decimals=2)
center_lab = label("MS PER FRAME", CX, 1050, size=26, align="center", name="center_label")
head3 = f.text("Where the time goes", SERIF_I, 80, [110, 330], color=INK, name="h3")
h3l, h3w = line_reveal(f, head3, 110, 330, 80, t + 0.1, dur=0.7)
s3 = f.stack(base, rings + legend + [center_num, fade_in(center_lab, t + 0.5), (h3l, None, h3w)])

# ---------------------------------------------------------------- 4 summary
t = CUTS[3]
stats = [(116, "", "FPS EXPORT", AMBER), (0, "", "READBACKS", TEAL), (4, "", "BACKENDS TESTED", CORAL)]
layers = []
for k, (v, suf, lab, col) in enumerate(stats):
    y = 720 + k * 300
    t0 = t + 0.15 + k * 0.18
    n = f.text("", UNB, 190, [110, y], color=col, weight=800, tracking=-6, name="stat",
               number=keys((t0, 0.0, OUT), (t0 + 0.9, float(v))), suffix=suf)
    layers.append(fade_in(n, t0, 0.2))
    layers.append(fade_in(label(lab, 116, y + 70, size=28, color=INK, name="stat_label"), t0 + 0.2))
    layers.append(fade_in(f.add("shape", {"kind": "rect", "center": [CX, y + 125], "size": [W - 220, 2],
                                          "color": "#ffffff1a"}, name="rule"), t0))
mark = f.add("svg", {"src": os.path.relpath(os.path.join(FILMS_DIR, "..", "assets", "svg", "ultrafast-mark.svg"), FILMS_DIR),
                     "size": [96, 96], "position": [110, 1640]}, name="mark")
word = f.text("ultrafast", UNB, 64, [228, 1712], color=INK, weight=800, tracking=-2, name="word")
layers.append(fade_in(f.stack(mark, [word]), t + 0.9))
s4 = f.stack(base, layers)

shots = [s1, s2, s3, s4]
comp = f.add("solid", {"color": "#000000"}, name="black")
for i, s in enumerate(shots):
    comp = f.over(s, comp, timing=window(i), name="at")
f.output = finish(f, comp, grain=0.05)
f.save(os.path.join(FILMS_DIR, "numbers.json"))
print("ok", len(f.nodes), "nodes")

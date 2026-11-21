"""RISO — a risograph print that moves. Three spot-ink plates (fluoro pink,
blue, yellow), each a live ultrafast layer screened to halftone dots at its own
angle, overprinted on paper with a registration that wanders. 1080x1920, 8 s."""

import random

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(4, 0.35))
W, H = f.W, f.H
rnd = random.Random(4)

HALFTONE = """
float4 shade(float2 pos, float px) {
  float ang = param(0) * 0.01745329;
  float cell = param(1);
  float2x2 R = float2x2(cos(ang), -sin(ang), sin(ang), cos(ang));
  float2 q = mul(R, pos) / cell;
  float2 c = floor(q) + 0.5;
  float2 cpos = mul(transpose(R), c * cell);
  float d = saturate(input(0, cpos).a);
  // Dot radius from coverage (area-true up to touching dots, then filling in).
  float r = d < 0.785 ? sqrt(d / 3.14159) : 0.5 + (d - 0.785) * 1.1;
  float dist = length(q - c);
  float cov = saturate((r - dist) * cell / max(px, 1e-3) + 0.5);
  // Riso ink is uneven: mottle it.
  float m = 0.86 + 0.14 * snoise(float3(pos * 0.035, param(5)));
  cov *= m;
  float3 ink = srgb_to_linear(float3(param(2), param(3), param(4)));
  return float4(ink * cov, cov);
}"""

PAPER = """
float4 shade(float2 pos, float px) {
  float fib = snoise(float3(pos.x * 0.004, pos.y * 0.09, 1.0)) * 0.5 + snoise(float3(pos * 0.02, 4.0)) * 0.5;
  float speck = step(0.996, frac(sin(dot(floor(pos / 2.0), float2(12.9898, 78.233))) * 43758.5453));
  float3 c = float3(0.945, 0.922, 0.878) * (0.975 + 0.025 * fib) - speck * 0.25;
  return float4(srgb_to_linear(c), 1.0);
}"""

INKS = {
    "pink": ([1.0, 0.28, 0.69], 15, 9.0),
    "blue": ([0.0, 0.47, 0.75], 75, 9.0),
    "yellow": ([1.0, 0.91, 0.0], 0, 10.0),
}


def wander(amount, every=1 / 12, until=8.0, seed=0):
    """Stepped misregistration: a new offset 12 times a second."""
    r = random.Random(seed)
    ks, t = [], 0.0
    while t <= until + 1e-6:
        ks.append((t, [W / 2 + r.uniform(-amount, amount), H / 2 + r.uniform(-amount, amount)], "hold"))
        t += every
    return keys(*ks)


def plate(name, layers, seed):
    """layers: density nodes (white, any alpha) -> a screened, wandering ink."""
    dens = f.stack(layers[0], layers[1:]) if len(layers) > 1 else layers[0]
    color, angle, cell = INKS[name]
    ht = f.add("shader", {"code": HALFTONE, "u0": angle, "u1": cell, "u2": color[0], "u3": color[1],
                          "u4": color[2], "u5": float(seed)}, {"in0": dens}, name=f"{name}_screen")
    return f.add("transform", {"position": wander(2.5, seed=seed), "anchor": [W / 2, H / 2]}, {"source": ht},
                 name=f"{name}_plate")


WHITE = "#ffffff"

# --- density layers -------------------------------------------------------
# Pink: a huge disc that rises and splits; a second, smaller disc later.
disc = f.add("shape", {"kind": "circle", "color": WHITE, "size": [980, 980],
                       "center": keys((0.0, [W / 2, H + 600], OUT), (1.1, [W / 2, 1180]), (3.6, [W / 2, 1180], INOUT),
                                      (4.4, [W / 2 + 40, 640]))}, name="disc")
half_l = f.add("crop", {"rect": [0, 0, W / 2, H]}, {"source": disc}, name="half_l")
half_r = f.add("crop", {"rect": [W / 2, 0, W / 2, H]}, {"source": disc}, name="half_r")
split_l = f.add("transform", {"anchor": [W / 2, H / 2], "position": keys((5.0, [W / 2, H / 2], INOUT), (5.8, [W / 2 - 160, H / 2]),
                                                                       (6.1, [W / 2 - 160, H / 2], INOUT), (6.9, [W / 2 - 160, H / 2 - 260]))},
                {"source": half_l}, name="split_l")
split_r = f.add("transform", {"anchor": [W / 2, H / 2], "position": keys((5.0, [W / 2, H / 2], INOUT), (5.8, [W / 2 + 160, H / 2]),
                                                                       (6.1, [W / 2 + 160, H / 2], INOUT), (6.9, [W / 2 + 160, H / 2 + 420]))},
                {"source": half_r}, name="split_r")
tint_ramp = f.add("gradient", {"kind": "linear", "start": [0, 1300], "end": [0, 1920],
                               "stops": "0:#ffffff00, 1:#ffffffcc"}, name="ramp")

# Blue: the words, stamped in, and a band of stripes.
anton = "Anton-Regular.ttf"
words = []
for i, (w, y, t) in enumerate([("PRINT", 520, 0.5), ("IS NOT", 830, 0.75), ("DEAD.", 1140, 1.0)]):
    tx = f.text(w, anton, 330, [76, y], color=WHITE, tracking=-2, name="word")
    st = f.add("transform", {"anchor": [76, y - 120], "position": [76, y - 120],
                             "scale": keys((t, 1.18, OUT), (t + 0.35, 1.0)),
                             "rotation": keys((t, -3, OUT), (t + 0.4, 0))}, {"source": tx}, name="stamp")
    words.append((st, {"opacity": keys((t, 0.0, "hold"), (t + 0.02, 1.0), (3.3 + i * 0.06, 1.0, IN), (3.6 + i * 0.06, 0.0))},
                  {"in": t, "out": 3.7 + i * 0.06}))
stripes = f.add("shader", {"code": """float4 shade(float2 pos, float px) {
  float a = 0.5 + 0.5 * sin((pos.x * 0.7071 + pos.y * 0.7071) * 0.12566 - time() * 3.0);
  float s = saturate((a - 0.5) / (px * 0.0628) + 0.5);
  return float4(s, s, s, s);
}"""}, name="stripes")
band = f.add("shape", {"kind": "rect", "color": WHITE, "center": keys((0.0, [W / 2, 1640]), (8.0, [W / 2, 1600])),
                       "size": keys((0.2, [0, 150], OUT), (1.4, [W, 150]))}, name="band")
striped = f.add("mask", {"mode": "alpha"}, {"source": stripes, "matte": band}, name="striped")

# Act 2 words.
w2 = []
for i, (w, y, t) in enumerate([("EVERY", 1180, 4.3), ("FRAME,", 1450, 4.45), ("RE-INKED.", 1720, 4.6)]):
    tx = f.text(w, anton, 250, [76, y], color=WHITE, tracking=-1, name="word2")
    lay, win = line_reveal(f, tx, 76, y, 250, t, t_out=5.9 + i * 0.06, dur=0.55, ascent=0.82, descent=0.1)
    w2.append((lay, None, win))

# Yellow: a sun, a rule, the proof strip.
sun = f.add("shape", {"kind": "circle", "color": WHITE, "size": keys((0.8, [0, 0], OUT), (1.8, [520, 520]), (3.6, [520, 520], INOUT), (4.4, [700, 700])),
                      "center": keys((0.8, [800, 420], OUT), (1.8, [780, 420]), (3.6, [780, 420], INOUT), (4.4, [W / 2 - 40, 1240]))}, name="sun")
sun_soft = f.add("blur", {"radius": 40}, {"source": sun}, name="sun_soft")
proof = []
for k in range(3):
    proof.append(f.add("shape", {"kind": "rect", "color": WHITE, "center": [760 + k * 92, 1832], "size": [80, 40]}, name="swatch"))

# The ending: wordmark and printer's marks, in all three plates.
mark = f.text("ultrafast", anton, 420, [W / 2, 1130], align="center", color=WHITE, tracking=4, name="mark")
mark_l, mark_w = line_reveal(f, mark, W / 2, 1130, 420, 6.35, dur=0.6, ascent=0.82, descent=0.25)
reg = []
for cx, cy in [(110, 150), (W - 110, 150), (110, H - 150), (W - 110, H - 150)]:
    reg.append(f.add("path", {"kind": "circle", "center": [cx, cy], "radius": 22, "stroke": 3, "color": WHITE}, name="reg"))
    reg.append(f.add("path", {"kind": "polyline", "points": [cx - 40, cy, cx + 40, cy], "stroke": 3, "color": WHITE}, name="regh"))
    reg.append(f.add("path", {"kind": "polyline", "points": [cx, cy - 40, cx, cy + 40], "stroke": 3, "color": WHITE}, name="regv"))
regs = f.stack(reg[0], reg[1:])
# Small type prints solid (it would break up in the screen).
label = f.text("3 INKS · SCREENED LIVE ON THE GPU", "GeistMono-VF.ttf", 30, [76, 1846], color="#0078bf",
               weight=700, tracking=1, name="label")

blank = f.add("solid", {"color": "#00000000"}, name="blank")
pink = plate("pink", [f.stack(blank, [(disc, None, {"out": 5.0}), (split_l, None, {"in": 5.0}), (split_r, None, {"in": 5.0}),
                                      (mark_l, None, mark_w), regs])], seed=1)
blue = plate("blue", [f.stack(blank, words + [(striped, {"opacity": keys((3.4, 1.0, SOFT), (4.0, 0.0))}, {"out": 4.0})]
                              + w2 + [(mark_l, {"opacity": 0.55}, mark_w), regs])], seed=2)
yellow = plate("yellow", [f.stack(blank, [sun_soft, (tint_ramp, {"opacity": 0.45}, None)] + proof + [regs])], seed=3)

paper = f.add("shader", {"code": PAPER}, name="paper")
out = f.blend(yellow, paper, "multiply", name="print_y")
out = f.blend(pink, out, "multiply", name="print_p")
out = f.blend(blue, out, "multiply", name="print_b")
out = f.blend(label, out, "multiply", name="print_label")
f.output = finish(f, out, grain=0.05, vignette=False)
f.save(os.path.join(FILMS_DIR, "riso.json"))
print("ok", len(f.nodes), "nodes")

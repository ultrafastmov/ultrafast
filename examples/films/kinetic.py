"""KINETIC — variable type as motion: marquees whose weight breathes in a
wave, a word that grows to fill the frame, stacked claims. 1080x1920, 8 s."""

import math

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(8, 0.5))
W, H = f.W, f.H
PAPER, INK, RED = "#ece8df", "#111111", "#ee2a10"
SYNE, UNB, MONO = "Syne-VF.ttf", "Unbounded-VF.ttf", "GeistMono-VF.ttf"

paper = f.add("solid", {"color": PAPER}, name="paper")

# Act 1: twelve marquee rows; weights ripple down the stack.
rows = []
ROWS, ROW_H = 12, 142
for r in range(ROWS):
    y = 150 + r * ROW_H + 112
    word = ["MOVE", "TYPE", "LIVE", "EDIT"][r % 4]
    text = ("  ".join([word] * 14))
    direction = 1 if r % 2 else -1
    speed = 160 + 40 * (r % 3)
    x0 = -400 if direction > 0 else -900
    wk = []
    for i in range(0, 41):
        t = 3.0 * i / 40
        wk.append((t, 400 + 400 * math.sin(t * 3.2 - r * 0.55)))
    color = RED if r == 5 else INK
    tx = f.text(text, SYNE, 128, keys((0.0, [x0, y]), (3.0, [x0 + direction * speed * 3.0, y])), color=color,
                weight=keys(*wk), tracking=-2, name="row")
    rows.append((tx, {"opacity": keys((0.0 + r * 0.04, 0.0, OUT), (0.25 + r * 0.04, 1.0))}, {"out": 2.75}))

# Act 2: one word grows to fill the frame; the page turns to ink.
night = f.add("solid", {"color": INK}, name="night")
wipe = f.add("shape", {"kind": "rect", "color": "#ffffff", "center": [W / 2, H / 2],
                       "size": keys((2.45, [W + 40, 0], INOUT), (2.85, [W + 40, H + 40]))}, name="wipe")
night_in = f.add("mask", {"mode": "alpha"}, {"source": night, "matte": wipe}, name="night_in")
fast = f.text("FAST", SYNE, 250, [W / 2, 1050], align="center", color=PAPER,
              weight=keys((2.8, 400, OUT), (4.6, 800)), tracking=keys((2.8, 60, OUT), (4.6, -18)), name="fast")
fast_xf = f.add("transform", {"anchor": [W / 2, 960], "position": [W / 2, 960],
                              "scale": keys((2.8, 0.35, OUT), (4.0, 1.0), (4.6, 1.0, IN), (5.0, 3.6))}, {"source": fast},
                name="fast_xf")
bar = f.add("shape", {"kind": "rect", "color": RED, "center": keys((3.4, [-600, 1230], OUT), (4.0, [W / 2, 1230]), (4.6, [W / 2, 1230], IN), (5.0, [W + 700, 1230])),
                      "size": [W, 34]}, name="bar")
spec = f.text("0.31 ms / frame", MONO, 34, [W / 2, 1330], align="center", color=PAPER, weight=500, tracking=2, name="spec")
spec_o = f.add("opacity", {"opacity": keys((3.6, 0.0, SOFT), (4.0, 1.0), (4.5, 1.0, SOFT), (4.7, 0.0))}, {"source": spec}, name="spec_o")

# Act 3: claims, stacked on paper again.
paper2 = f.add("solid", {"color": PAPER}, name="paper2")
claims = []
S = 88
for i, (num, word, y, t) in enumerate([("1", "GRAPH", 760, 5.05), ("0", "BROWSERS", 930, 5.2), ("∞", "EDITS", 1100, 5.35)]):
    n = f.text(num, UNB if num != "∞" else "Geist-VF.ttf", S, [72, y], color=RED, weight=800, name="num")
    nl, nw = line_reveal(f, n, 72, y, S, t, dur=0.55, ascent=0.8, descent=0.12)
    w = f.text(word, SYNE, S, [72 + 150, y], color=INK, weight=keys((t, 400, OUT), (t + 0.9, 720)),
               tracking=-3, name="claim")
    wl, ww = line_reveal(f, w, 72 + 150, y, S, t + 0.06, dur=0.55, ascent=0.8, descent=0.12)
    claims += [(nl, None, nw), (wl, None, ww)]
rule = f.add("shape", {"kind": "rect", "color": INK, "center": keys((5.6, [72, 1210]), (6.4, [W / 2, 1210])),
                       "size": keys((5.6, [0, 4], OUT), (6.4, [W - 144, 4]))}, name="rule")
mark = f.text("ultrafast", SYNE, 150, [72, 1400], color=INK, weight=keys((6.3, 400, OUT), (7.4, 800)), tracking=-4, name="mark")
ml, mw = line_reveal(f, mark, 72, 1400, 150, 6.3, dur=0.6, ascent=0.8, descent=0.25)
tag = f.text("VIDEO AS A LIVE DOCUMENT", MONO, 28, [76, 1480], color=INK, weight=600, tracking=3, name="tag")
tag_o = f.add("opacity", {"opacity": keys((6.5, 0.0, SOFT), (6.8, 1.0))}, {"source": tag}, name="tag_o")

comp = f.stack(paper, rows + [
    (night_in, None, {"in": 2.45, "out": 5.0}),
    (fast_xf, None, {"in": 2.8, "out": 5.0}),
    (bar, None, {"in": 3.4, "out": 5.0}),
    (spec_o, None, {"in": 3.6, "out": 4.7}),
    (paper2, None, {"in": 5.0}),
] + claims + [(rule, None, {"in": 5.6}), (ml, None, mw), (tag_o, None, {"in": 6.5})])
f.output = finish(f, comp, grain=0.06, vignette=False)
f.save(os.path.join(FILMS_DIR, "kinetic.json"))
print("ok", len(f.nodes), "nodes")

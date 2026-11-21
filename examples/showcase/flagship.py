"""ULTRAFAST — the flagship film: what ultrafast is, shown with ultrafast. One brand
system throughout (ink black, bone white, one acid accent; Unbounded for
display, Geist for words, Geist Mono for the document chrome) and mostly
motion graphics that are hard to make anywhere else: a node graph that comes
alive, type that behaves like nothing else, type poured into a fluid
simulation, a shader written live, an agent editing the film while it plays,
and everything collapsing into the wordmark. 1920x1080, 44 s, on an
original 120 BPM score (ElevenLabs Music).

The score lives outside the repo: $ULTRAFAST_SHOWCASE/flagship/audio/score/take-1.mp3
(the score is re-cut to 58 s on bar lines: showcase_score.wav, built from take-1.mp3)

    python3 flagship.py && ultrafast export $ULTRAFAST_SHOWCASE/flagship/flagship.json $ULTRAFAST_SHOWCASE/flagship/flagship.mp4
"""

import math
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "flagship")

f = Film(1920, 1080, fps=30, duration=58.5, motion_blur=(8, 0.5))
W, H = f.W, f.H
CX, CY = W / 2, H / 2

# ------------------------------------------------------------------ the brand
INK, BONE, ACID, GREY = "#08080a", "#f2f0ea", "#d4ff3a", "#8b8b90"
DIM = "#f2f0ea22"
WIDE, SANS, MONO, SERIF = "ultrafast:wide", "ultrafast:sans", "ultrafast:mono", "ultrafast:serif"
# The word that matters: Instrument Serif italic, in the accent.
EM = {"em_font": SERIF, "em_style": "italic", "em_weight": 400, "em_color": ACID}
SNAP = {"bezier": [0.16, 1.0, 0.3, 1.0]}   # decisive arrivals
SWIFT = {"bezier": [0.7, 0.0, 0.84, 0.0]}  # decisive exits
B = 0.5
# Beats fall between frames (half a frame in), so a hard cut is never
# inside a frame's shutter (the music's grid is 13 ms later; unnoticed).
beat = lambda k: 1 / 60 + k * B


def between_frames(t, fps=30):
    """The nearest moment halfway between two frames: cut there and no
    frame's motion-blur shutter straddles the cut."""
    return (round(t * fps - 0.5) + 0.5) / fps
CUTS = [0.0, beat(16), beat(28), beat(44), beat(56), beat(68), beat(80), 44.0]
NAMES = ["BOOT", "DOCUMENT", "TYPE", "SIMULATION", "SHADERS", "LIVE", "ULTRAFAST"]
# The showcase cut: two reels of fast cuts are spliced in (the score is
# re-cut on bar lines to match), so sections authored on the CUTS timeline
# play later by SHIFT[k] seconds.
SHIFT = [0.0, 8.0, 8.0, 8.0, 8.0, 14.0, 14.0]
REEL1 = (beat(16), beat(32))   # 8.03 – 16.03, on the drop
REEL2 = (beat(84), beat(96))   # 42.03 – 48.03, at the peak
NEW_CUTS = [CUTS[k] + SHIFT[k] for k in range(len(NAMES))] + [58.5]


def shifted(k, build):
    """Build a section on the CUTS timeline, then move it to where it plays
    now: every node it made runs SHIFT[k] seconds later."""
    before = set(f.nodes)
    out = build()
    d = SHIFT[k]
    if d:
        for nid in set(f.nodes) - before:
            tm = dict(f.nodes[nid].get("timing") or {})
            for key in ("in", "out"):
                if key in tm:
                    tm[key] += d
            tm["offset"] = tm.get("offset", 0.0) + d
            f.nodes[nid]["timing"] = tm
    return out


def mono(text, x, y, t0, t1, size=20, color=GREY, align="left", nid="mono", **extra):
    p = {"text": text, "font": MONO, "size": size, "tracking": 2, "weight": 500, "color": color, "align": align,
         "valign": "baseline", "position": [x, y]}
    p.update(extra)
    return f.add("text", p, timing={"in": t0, "out": t1}, name=nid)


SPLIT = """
// Chromatic split: red one way, blue the other, by param(0) px.
float4 shade(float2 pos, float px) {
  float2 o = float2(param(0), 0.0);
  float4 c = input(0, pos), r = input(0, pos + o), b = input(0, pos - o);
  return float4(r.r, c.g, b.b, max(c.a, max(r.a, b.a)));
}"""


def split(node, t_land, t_out, peak=9.0, rest=1.2):
    """A line landing with a chromatic burst (at t_land) that settles to a
    resting split; the split lives as long as the line does."""
    t_in = f.nodes[node].get("timing", {}).get("in", t_land)
    return f.add("shader", {"code": SPLIT, "u0": keys((t_land, 0.0), (t_land + 0.12, peak), (t_land + 0.7, rest, OUT))},
                 {"in0": node}, timing={"in": min(t_in, t_land), "out": t_out}, name=f"{node}_split")


def slate(k):
    """A stand-in for a section not built yet: the film plays whole."""
    t0, t1 = CUTS[k], CUTS[k + 1]
    bg = f.add("solid", {"color": INK}, timing={"in": t0, "out": t1}, name=f"slate{k}")
    label = f.add("text", {"text": f"{k + 1:02d}  {NAMES[k]}", "font": WIDE, "weight": 300, "size": 64,
                           "color": DIM, "align": "center", "valign": "middle", "position": [CX, CY]},
                  timing={"in": t0, "out": t1}, name=f"slate{k}_label")
    return f.over(label, bg, name=f"slate{k}_card")


sections = {}


# ================================================================== 01 BOOT (0 – 8.03 s)
# A cursor on black; two commands type out; then the frame's dot grid comes
# up beat by beat, a playhead sweeps it, and it all draws into the center
# for the drop.
def boot():
    t0, t1 = CUTS[0], CUTS[1]
    L = []
    SZ = 54
    ADV = SZ * 0.6  # Geist Mono advance
    X0 = round(CX - ADV * 26 / 2)
    lines = [("$ ultrafast new flagship.json", 0.3, 1.1, BONE), ("wrote flagship.json", 1.55, 0.0, GREY),
             ("$ ultrafast live flagship.json", 1.9, 1.0, BONE), ("live on 127.0.0.1:7878", 3.0, 0.0, ACID)]
    caret = []
    for i, (text, ta, dur, color) in enumerate(lines):
        y = 400 + i * 86
        n = len(text)
        p = {"text": text, "font": MONO, "size": SZ, "weight": 500, "color": color, "valign": "baseline",
             "position": [X0, y], "opacity": keys((3.4, 1.0, SWIFT), (3.85, 0.0)),
             "glyph_offset": keys((3.4, [0, 0], SWIFT), (3.85, [0, -40]))}
        if dur > 0:
            p["reveal"] = keys((ta, 0.0), (ta + dur, float(n), "linear"))
            caret += [(ta, [X0 + 6, y - SZ * 0.36]), (ta + dur, [X0 + ADV * n + 8, y - SZ * 0.36], "linear")]
        else:
            p["glyph_opacity"] = keys((ta, 0.0), (ta + 0.15, 1.0))
            p["stagger"] = 0.006
        L.append(f.add("text", p, timing={"in": ta, "out": 3.9}, name="term"))
    # The caret: a block that blinks on the beat while it waits.
    caret = [(0.0, [X0 + 6, 400 - SZ * 0.36])] + caret + [(3.3, [X0 + 6, 400 + 4 * 86 - SZ * 0.36])]
    blink = []
    for k in range(7):
        blink += [(beat(k), 1.0), (beat(k) + 0.24, 1.0), (beat(k) + 0.25, 0.0), (beat(k) + 0.49, 0.0)]
    L.append(f.add("shape", {"kind": "rect", "size": [ADV * 0.9, SZ * 1.05], "color": ACID,
                             "center": keys(*caret), "opacity": keys(*blink)},
                   timing={"in": 0.0, "out": 3.4}, name="caret"))
    # The dot grid: 17 x 9, rising from the center out, one ring per beat.
    G = 96
    cols, rows = 17, 9
    gx0, gy0 = CX - (cols - 1) * G / 2, CY - (rows - 1) * G / 2
    sweep0, sweep1 = beat(10), beat(15)
    for r in range(rows):
        for c in range(cols):
            x, y = gx0 + c * G, gy0 + r * G
            ring = max(abs(c - cols // 2), abs(r - rows // 2))
            ta = beat(7) + ring * 0.15
            hit = sweep0 + (x / W) * (sweep1 - sweep0)
            L.append(f.add("shape", {"kind": "circle", "size": [7, 7], "color": BONE,
                                     "opacity": keys((ta, 0.0), (ta + 0.2, 0.35), (hit - 0.05, 0.35), (hit, 1.0),
                                                     (hit + 0.4, 0.35)),
                                     "scale": keys((ta, 0.0, SNAP), (ta + 0.35, 1.0), (hit, 1.8, SNAP), (hit + 0.4, 1.0)),
                                     "center": keys((beat(15), [x, y], SWIFT), (beat(16) - 0.02, [CX, CY]))},
                           timing={"in": ta, "out": t1}, name="dot"))
    # The playhead: a lime line that scrubs the grid.
    L.append(f.add("shape", {"kind": "rect", "size": [3, (rows - 1) * G + 80], "color": ACID,
                             "center": keys((sweep0, [0, CY], "linear"), (sweep1, [W, CY]))},
                   timing={"in": sweep0, "out": sweep1}, name="playhead"))
    L.append(f.add("glow", {"radius": 14, "intensity": 0.8}, {"source": L[-1]}, timing={"in": sweep0, "out": sweep1},
                   name="playhead_glow"))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="boot_bg")
    return f.stack(bg, L)


sections[0] = shifted(0, boot)


def bez_len(p0, p1, p2, p3, n=64):
    pts = []
    for i in range(n + 1):
        u = i / n
        a, b, c, d = (1 - u) ** 3, 3 * u * (1 - u) ** 2, 3 * u * u * (1 - u), u ** 3
        pts.append((a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]))
    return sum(math.hypot(x1 - x0, y1 - y0) for (x0, y0), (x1, y1) in zip(pts, pts[1:]))


# ================================================================== 02 DOCUMENT (8.03 – 14.03 s)
# On the drop a node graph assembles card by card, its wires drawing on the
# beat and pulses of data running down them; then the output card opens
# into the picture the graph makes.
def document():
    t0, t1 = CUTS[1], CUTS[2]
    L = []
    CW, CH = 270, 96
    cards = {
        "text": ((380, 400), '"ultrafast"', beat(16)),
        "marble": ((380, 690), "filaments 0.35", beat(17)),
        "fluid": ((800, 400), "vorticity 9", beat(18)),
        "over": ((1190, 545), "fg  ·  bg", beat(19)),
        "export": ((1570, 545), "h264  ·  1080p", beat(20)),
    }
    for name, ((x, y), sub, ta) in cards.items():
        pop = {"scale": keys((ta, 0.6, SNAP), (ta + 0.35, 1.0)), "opacity": keys((ta, 0.0), (ta + 0.12, 1.0))}
        L.append(f.add("shape", dict(pop, kind="rect", center=[x, y], size=[CW, CH], radius=14, color="#141418"),
                       timing={"in": ta, "out": t1}, name="card"))
        L.append(f.add("shape", dict(pop, kind="rect", center=[x, y], size=[CW, CH], radius=14,
                                     color=ACID if name == "export" else "#f2f0ea55", stroke=1.6),
                       timing={"in": ta, "out": t1}, name="card_edge"))
        L.append(f.add("text", {"text": name, "font": MONO, "size": 28, "weight": 600, "color": BONE,
                                "valign": "baseline", "position": [x - CW / 2 + 24, y - 6],
                                "reveal": keys((ta + 0.1, 0.0), (ta + 0.35, float(len(name)), "linear"))},
                       timing={"in": ta, "out": t1}, name="card_op"))
        L.append(f.add("text", {"text": sub, "font": MONO, "size": 17, "color": GREY, "valign": "baseline",
                                "position": [x - CW / 2 + 24, y + 28],
                                "opacity": keys((ta + 0.25, 0.0), (ta + 0.5, 1.0))},
                       timing={"in": ta, "out": t1}, name="card_sub"))
        for side in ([-1, 1] if name not in ("text", "marble") else [1]) if name != "export" else [-1]:
            L.append(f.add("shape", dict(pop, kind="circle", center=[x + side * CW / 2, y], size=[11, 11],
                                         color=ACID), timing={"in": ta, "out": t1}, name="port"))
    wires = [("text", "fluid", 0, beat(18)), ("fluid", "over", -22, beat(19)), ("marble", "over", 22, beat(19)),
             ("over", "export", 0, beat(20))]
    for a, b, dy, ta in wires:
        (ax, ay), (bx, by) = cards[a][0], cards[b][0]
        p0, p3 = (ax + CW / 2, ay), (bx - CW / 2, by + dy)
        k = (p3[0] - p0[0]) * 0.5
        p1, p2 = (p0[0] + k, p0[1]), (p3[0] - k, p3[1])
        d = f"M {p0[0]} {p0[1]} C {p1[0]} {p1[1]} {p2[0]} {p2[1]} {p3[0]} {p3[1]}"
        L.append(f.add("path", {"d": d, "stroke": 2.0, "color": BONE, "opacity": 0.55,
                                "trim_end": keys((ta, 0.0, SNAP), (ta + 0.45, 1.0))},
                       timing={"in": ta, "out": t1}, name="wire"))
        # Data running down the wire, one pulse a beat.
        n = bez_len(p0, p1, p2, p3)
        pulse = {"keys": [{"t": ta + 0.4, "v": [0.0]}, {"t": ta + 0.9, "v": [-(n + 60)]}], "repeat": "cycle"}
        L.append(f.add("path", {"d": d, "stroke": 5.0, "color": ACID, "cap": "round", "dash": [46, n + 200],
                                "dash_offset": pulse}, timing={"in": ta + 0.4, "out": t1}, name="pulse"))
    L.append(f.add("text", {"text": "A video is a *document.*", "font": SANS, "weight": 500, "size": 52, "color": BONE,
                            "valign": "baseline", "position": [380 - CW / 2, 900], "em_size": 70, **EM,
                            "glyph_opacity": keys((beat(21), 0.0), (beat(21) + 0.4, 1.0)), "stagger": 0.015,
                            "glyph_offset": keys((beat(21), [0, 24], SNAP), (beat(21) + 0.6, [0, 0]))},
                   timing={"in": beat(21), "out": beat(23)}, name="doc_line"))
    # Grouped and pushed in slowly; the vectors re-rasterize at each scale.
    graph = L[0]
    for lid in L[1:]:
        graph = f.over(lid, graph, name="doc_graph")
    graph = f.add("transform", {"anchor": [975, 560], "position": [CX, CY + 10],
                                "scale": 1.0}, {"source": graph},
                  timing={"in": t0, "out": t1}, name="doc_push")
    graph = f.over(graph, f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="doc_bg"), name="doc_graph_bg")
    # The picture the graph makes: type poured over living marble, in the
    # brand's colors; it opens out of the export card.
    tb = beat(23)
    marble = f.add("marble", {"filaments": 0.35}, timing={"in": tb - 0.2, "out": t1}, name="doc_marble")
    marble = f.add("levels", {"in_black": 0.38, "in_white": 0.95, "gamma": 1.4}, {"source": marble},
                   timing={"in": tb - 0.2, "out": t1}, name="doc_crush")
    marble = f.add("tint", {"dark": INK, "light": ACID}, {"source": marble}, timing={"in": tb - 0.2, "out": t1},
                   name="doc_tint")
    big = []
    for i, line in enumerate(["Every frame", "is *live.*"]):
        big.append(f.add("text", {"text": line, "font": WIDE, "weight": 700, "size": 150, "color": BONE,
                                  **EM, "em_size": 210,
                                  "valign": "baseline", "position": [150, 470 + i * 170], "tracking": -4,
                                  "glyph_offset": keys((tb + 0.1 + i * 0.25, [0, 150], SNAP), (tb + 0.7 + i * 0.25, [0, 0])),
                                  "glyph_opacity": keys((tb + 0.1 + i * 0.25, 0.0), (tb + 0.3 + i * 0.25, 1.0)),
                                  "stagger": 0.03},
                          timing={"in": tb, "out": t1}, name="doc_big"))
    picture = f.stack(marble, [split(b, tb + 0.15 + i * 0.25, t1) for i, b in enumerate(big)])
    (ex, ey) = cards["export"][0]
    window = f.add("shape", {"kind": "rect", "color": "#ffffff",
                             "center": keys((tb - 0.25, [ex, ey], SNAP), (tb + 0.3, [CX, CY])),
                             "size": keys((tb - 0.25, [CW, CH], SNAP), (tb + 0.3, [W + 4, H + 4])),
                             "radius": keys((tb - 0.25, 14.0, SNAP), (tb + 0.3, 0.0))},
                   timing={"in": tb - 0.25, "out": t1}, name="doc_window")
    picture = f.add("mask", {"mode": "alpha"}, {"source": picture, "matte": window},
                    timing={"in": tb - 0.25, "out": t1}, name="doc_open")
    return f.over(picture, graph, timing={"in": t0, "out": t1}, name="document")


sections[1] = shifted(1, document)


# ================================================================== 03 TYPE (14.03 – 22.03 s)
# Four beats of type each: a field of rows rippling in weight and scale; a
# line whose every glyph flies in on its own; type running round a circle
# about a letter breathing through its weights; a swarm that becomes a word.
def type_section():
    t0, t1 = CUTS[2], CUTS[3]
    L = []
    # (a) the field: rows of TYPE, weights rolling, glyphs rippling.
    a0, a1 = t0, beat(32)
    ROWS = 7
    for r in range(ROWS):
        y = 170 + r * 128 + 96
        word = ["TYPE", "MOVES", "LIKE", "THIS"][r % 4]
        text = "   ".join([word] * 9)
        d = 1 if r % 2 else -1
        x0 = -260 if d > 0 else -700
        wk = [(a0 + i * 0.1, 450 + 420 * math.sin((a0 + i * 0.1) * 5.0 - r * 0.7)) for i in range(int((a1 - a0) / 0.1) + 1)]
        ripple = {"keys": [{"t": a0, "v": [1.0, 1.0]}, {"t": a0 + 0.25, "v": [1.0, 1.45]}, {"t": a0 + 0.5, "v": [1.0, 1.0]}],
                  "repeat": "cycle"}
        L.append(f.add("text", {"text": text, "font": WIDE, "size": 96, "color": ACID if r == 3 else BONE,
                                "valign": "baseline", "position": keys((a0, [x0, y]), (a1, [x0 + d * 520, y], "linear")),
                                "weight": keys(*wk), "tracking": -2, "glyph_scale": ripple, "stagger": 0.035,
                                "opacity": keys((a0 + r * 0.03, 0.0), (a0 + r * 0.03 + 0.15, 1.0))},
                       timing={"in": a0, "out": a1}, name="field"))
    # (b) every glyph a layer: each arrives on its own path, in random order.
    b0, b1 = beat(32), beat(36)
    for i, (line, size, y) in enumerate([("Every glyph", 150, 470), ("is a *layer.*", 150, 670)]):
        L.append(f.add("text", {"text": line, "font": WIDE, "weight": 800, "size": size, "align": "center",
                                "valign": "baseline", "position": [CX, y], "tracking": -4, **EM, "em_size": 220,
                                "color": BONE, "stagger": 0.045, "stagger_order": "random",
                                "glyph_offset": keys((b0 + i * 0.12, [0, -520], SNAP), (b0 + i * 0.12 + 0.55, [0, 0])),
                                "glyph_rotation": keys((b0 + i * 0.12, -120.0, SNAP), (b0 + i * 0.12 + 0.55, 0.0)),
                                "glyph_scale": keys((b0 + i * 0.12, [2.4, 2.4], SNAP), (b0 + i * 0.12 + 0.55, [1.0, 1.0])),
                                "glyph_opacity": keys((b0 + i * 0.12, 0.0), (b0 + i * 0.12 + 0.1, 1.0))},
                       timing={"in": b0, "out": b1}, name="glyphs"))
        L[-1] = split(L[-1], b0 + i * 0.12 + 0.3, b1)
    # (c) a circle of type round a letter breathing through every weight.
    c0, c1 = beat(36), beat(40)
    R = 360
    ring = f"M {CX - R} {CY} A {R} {R} 0 1 1 {CX + R} {CY} A {R} {R} 0 1 1 {CX - R} {CY}"
    L.append(f.add("text", {"text": "TYPE ON ANY PATH  ·  VARIABLE FONTS  ·  EXACT KERNING  ·  " * 2, "font": MONO,
                            "size": 30, "weight": 600, "tracking": 3.6, "color": BONE, "on_path": ring,
                            "path_offset": keys((c0, 0.0), (c1, -0.18, "linear")),
                            "reveal": keys((c0, 0.0), (c0 + 0.7, 120.0, "linear"))},
                   timing={"in": c0, "out": c1}, name="ring_text"))
    L.append(f.add("path", {"kind": "circle", "center": [CX, CY], "radius": R - 40, "stroke": 1.5, "color": ACID,
                            "trim_end": keys((c0, 0.0, SNAP), (c0 + 0.8, 1.0))}, timing={"in": c0, "out": c1},
                   name="ring_rule"))
    wk = [(c0 + i * 0.05, 520 + 380 * math.sin((i * 0.05) * 6.283 / 1.0 - 1.57)) for i in range(int((c1 - c0) / 0.05) + 1)]
    L.append(f.add("text", {"text": "Aa", "font": WIDE, "size": 300, "align": "center", "valign": "middle",
                            "position": [CX, CY - 10], "color": BONE, "weight": keys(*wk),
                            "glyph_scale": keys((c0, [0.4, 0.4], SNAP), (c0 + 0.5, [1.0, 1.0]))},
                   timing={"in": c0, "out": c1}, name="breathe"))
    # (d) a swarm: four thousand points that become a word, then let go.
    d0, d1 = beat(40), t1
    L.append(f.add("swarm", {"text": "ANY WORD.", "font": WIDE, "weight": 800, "size": 210, "align": "center",
                             "valign": "middle", "position": [CX, CY], "count": 14000, "scatter": 900,
                             "particle_size": [2.6, 3.6], "color": BONE, "color_scatter": ACID, "turbulence": 60,
                             "shimmer": 0.3, "stretch": 0.6, "blend": "add",
                             "progress": keys((d0, 0.0, SNAP), (d0 + 1.0, 1.0), (d1 - 0.5, 1.0, SWIFT), (d1, 0.0))},
                   timing={"in": d0, "out": d1}, name="swarm"))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="type_bg")
    return f.stack(bg, L)


sections[2] = shifted(2, type_section)


# ================================================================== 04 SIMULATION (22.03 – 28.03 s)
# Type poured into a fluid simulation and carried off as smoke; then type
# that grows, a reaction-diffusion seeded by the word.
RD = r"""
float hash21(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
float4 update(float2 pos, float px) {
    float mask = input(0, pos).a;
    if (step_index() == 0) {
        float spot = step(0.982, hash21(floor(pos / (px * 3.0))));
        return float4(1.0, spot * mask, 0.0, 1.0);
    }
    float2 c = prev(pos).xy;
    float2 lap = -c
        + 0.2 * (prev(pos + float2(px, 0)).xy + prev(pos - float2(px, 0)).xy
               + prev(pos + float2(0, px)).xy + prev(pos - float2(0, px)).xy)
        + 0.05 * (prev(pos + float2(px, px)).xy + prev(pos - float2(px, px)).xy
                + prev(pos + float2(px, -px)).xy + prev(pos + float2(-px, px)).xy);
    float a = c.x, b = c.y;
    float kill = lerp(param(2), param(1), mask);
    float r = a * b * b;
    a += lap.x - r + param(0) * (1.0 - a);
    b += 0.5 * lap.y + r - (kill + param(0)) * b;
    return float4(saturate(a), saturate(b), 0.0, 1.0);
}
"""
GROWTH = r"""
// The growth, lit as a relief: acid where it is thick, bone at the tips,
// on ink.
float4 shade(float2 pos, float px) {
    float e = 2.5;
    float b = input(0, pos).y;
    float bx = input(0, pos + float2(e, 0)).y - input(0, pos - float2(e, 0)).y;
    float by = input(0, pos + float2(0, e)).y - input(0, pos - float2(0, e)).y;
    float3 n = normalize(float3(-bx * 5.0, -by * 5.0, 1.0));
    float3 l = normalize(float3(-0.45, -0.75, 0.55));
    float diff = saturate(dot(n, l));
    float spec = pow(saturate(dot(reflect(-l, n), float3(0, 0, 1))), 30.0);
    float m = smoothstep(0.1, 0.3, b);
    float3 ink = srgb_to_linear(float3(0.031, 0.031, 0.039));
    float3 acid = srgb_to_linear(float3(0.83, 1.0, 0.23));
    float3 bone = srgb_to_linear(float3(0.95, 0.94, 0.92));
    float3 col = lerp(acid, bone, smoothstep(0.3, 0.5, b)) * (0.3 + 0.9 * diff) + spec * 0.6;
    float sh = input(0, pos - float2(-10.0, -16.0)).y;
    ink *= 1.0 - 0.6 * smoothstep(0.1, 0.35, sh);
    return float4(lerp(ink, col, m), 1.0);
}
"""


def simulation():
    t0, t1 = CUTS[3], CUTS[4]
    L = []
    # (a) FLOW: crisp for a beat, then poured into the fluid as dye and lifted
    # away as smoke.
    a1 = beat(50)
    word = f.add("text", {"text": "FLOW", "font": WIDE, "weight": 800, "size": 400, "align": "center",
                          "valign": "middle", "position": [CX, CY + 40], "color": BONE, "tracking": -10,
                          "glyph_offset": keys((t0, [0, 220], SNAP), (t0 + 0.45, [0, 0])), "stagger": 0.05},
                  timing={"in": t0, "out": a1}, name="flow_word")
    pour = f.add("opacity", {"opacity": keys((t0, 0.0), (beat(45) - 0.05, 0.0), (beat(45), 1.0), (beat(46), 1.0),
                                             (beat(47), 0.0))}, {"source": word}, timing={"in": t0, "out": a1},
                 name="flow_pour")
    smoke = f.add("fluid", {"start": beat(45) - t0, "emit": 9, "dissipation": 0.12, "velocity_dissipation": 0.3,
                            "vorticity": 8, "turbulence": keys((beat(45), 200.0), (beat(50), 900.0)),
                            "turbulence_scale": 260, "buoyancy": 900, "stir": 1400, "swirl": 600,
                            "resolution": 0.25, "dye_resolution": 0.6, "seed": 5},
                  {"dye": pour, "stir": pour}, timing={"in": t0, "out": a1, "offset": t0}, name="flow_sim")
    smoke = f.add("tint", {"dark": INK, "light": BONE}, {"source": smoke}, timing={"in": t0, "out": a1},
                  name="flow_tint")
    crisp = f.add("opacity", {"opacity": keys((beat(45), 1.0, SWIFT), (beat(47), 0.0))}, {"source": word},
                  timing={"in": t0, "out": a1}, name="flow_crisp")
    flow = f.over(crisp, smoke, timing={"in": t0, "out": a1}, name="flow")
    L.append(flow)
    L.append(f.add("text", {"text": "Simulation is *a node.*", "font": SANS, "weight": 500, "size": 56,
                            "color": BONE, **EM, "em_size": 76, "valign": "baseline", "position": [150, 840],
                            "glyph_opacity": keys((beat(47), 0.0), (beat(47) + 0.4, 1.0)), "stagger": 0.015,
                            "glyph_offset": keys((beat(47), [0, 26], SNAP), (beat(47) + 0.6, [0, 0]))},
                   timing={"in": beat(47), "out": a1}, name="sim_line"))
    L.append(mono("fluid  ·  vorticity 8  ·  buoyancy 900  ·  stepped every frame", 150, 895, beat(48), a1,
                  size=18, color=GREY, nid="sim_params",
                  reveal=keys((beat(48), 0.0), (beat(48) + 0.5, 70.0, "linear"))))
    # (b) GROW: the word seeds a reaction-diffusion that grows out of it.
    b0 = a1
    seed = f.add("text", {"text": "GROW", "font": WIDE, "weight": 900, "size": 380, "align": "center",
                          "valign": "middle", "position": [CX, CY + 20], "color": "#ffffff", "tracking": -8},
                 timing={"in": b0 - 0.1, "out": t1}, name="grow_seed")
    rd = f.add("feedback", {"code": RD, "start": 0.0, "resolution": 0.6, "substeps": 26, "u": [0.0545, 0.062, 0.09],
                            "u2": keys((b0, 0.09), (b0 + 1.0, 0.09), (b0 + 2.6, 0.0635))},
               {"in0": seed}, timing={"in": b0, "out": t1, "offset": b0}, name="grow_rd")
    grow = f.add("shader", {"code": GROWTH}, {"in0": rd}, timing={"in": b0, "out": t1}, name="grow_lit")
    L.append(grow)
    L.append(f.add("text", {"text": "Grown, *not drawn.*", "font": SANS, "weight": 500, "size": 56,
                            "color": BONE, **EM, "em_size": 76, "valign": "baseline", "position": [150, 920],
                            "glyph_opacity": keys((b0 + 1.2, 0.0), (b0 + 1.6, 1.0)), "stagger": 0.015},
                   timing={"in": b0 + 1.2, "out": t1}, name="grow_line"))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="sim_bg")
    return f.stack(bg, L)


sections[3] = shifted(3, simulation)


# ================================================================== 05 SHADERS (28.03 – 34.03 s)
# A shader typed out line by line, its result beside it changing as each
# line lands; then the result takes the frame.
SHADER_LINES = [
    "float4 shade(float2 pos, float px) {",
    "  float2 uv = (pos - size() * 0.5) / size().y;",
    "  float r = length(uv) + 0.1 * snoise(float3(uv * 3.0, time() * 0.3));",
    "  float rings = sin(r * 60.0 - time() * 5.0);",
    "  float lit = smoothstep(0.55, 0.95, rings);",
    "  return float4(ACID * lit, lit);",
    "}",
]
STAGES = {
    1: "return float4(ACID * saturate(0.55 + uv.x * 0.8 + uv.y * 0.4) * 0.5, 1.0);",
    2: "float r = length(uv) + 0.1 * snoise(float3(uv * 3.0, time() * 0.3)); return float4(ACID * saturate(1.0 - r * 1.4) * 0.7, 1.0);",
    3: "float r = length(uv) + 0.1 * snoise(float3(uv * 3.0, time() * 0.3)); float rings = sin(r * 60.0 - time() * 5.0); return float4(ACID * (rings * 0.5 + 0.5) * 0.6, 1.0);",
    5: "float r = length(uv) + 0.1 * snoise(float3(uv * 3.0, time() * 0.3)); float rings = sin(r * 60.0 - time() * 5.0); float lit = smoothstep(0.55, 0.95, rings); return float4(ACID * lit, 1.0);",
}


def stage_code(body):
    return ("static const float3 ACID = float3(0.66, 1.0, 0.04);\nfloat4 shade(float2 pos, float px) {\n"
            "  float2 uv = (pos - size() * 0.5) / size().y;\n  " + body + "\n}")


def shaders():
    t0, t1 = CUTS[4], CUTS[5]
    L = []
    at, speed = t0 + 0.2, 100.0
    code = "\n".join(SHADER_LINES)
    L.append(f.add("code", {"code": code, "language": "slang", "theme": "ultrafast", "title": "rings.slang",
                            "type_speed": speed, "at": at, "size": 24, "position": [110, 250], "width": 980},
                   timing={"in": t0, "out": beat(64)}, name="shader_code"))
    # When each line is typed (characters so far, newlines included).
    done, n = [], 0
    for line in SHADER_LINES:
        n += len(line) + 1
        done.append(at + n / speed)
    # The preview: each stage takes over as its line lands.
    WX, WY, WW, WH = 1160, 250, 650, 560
    starts = sorted(STAGES)
    stage_nodes = []
    for i, k in enumerate(starts):
        a = done[k]
        b = done[starts[i + 1]] if i + 1 < len(starts) else t1
        stage_nodes.append(f.add("shader", {"code": stage_code(STAGES[k]), "resolution": 0.75},
                                 timing={"in": a, "out": b}, name=f"stage{k}"))
    preview = f.stack(f.add("solid", {"color": "#0e0e12"}, timing={"in": t0, "out": t1}, name="preview_bg"),
                      stage_nodes)
    flash = f.add("solid", {"color": "#ffffff"}, name="stage_flash")
    flash = f.add("opacity", {"opacity": keys(*[kv for k in starts for kv in
                                                 ((done[k], 0.0), (done[k] + 0.01, 0.35), (done[k] + 0.25, 0.0))])},
                  {"source": flash}, timing={"in": t0, "out": t1}, name="stage_flash_o")
    preview = f.over(flash, preview, timing={"in": t0, "out": t1}, name="preview_flash")
    # The window opens to the whole frame on the bar.
    e0 = beat(64)
    win = f.add("shape", {"kind": "rect", "color": "#ffffff", "radius": keys((e0, 18.0, SNAP), (e0 + 0.5, 0.0)),
                          "center": keys((e0, [WX + WW / 2, WY + WH / 2], SNAP), (e0 + 0.5, [CX, CY])),
                          "size": keys((e0, [WW, WH], SNAP), (e0 + 0.5, [W + 4, H + 4]))},
                  timing={"in": t0, "out": t1}, name="preview_window")
    shown = f.add("mask", {"mode": "alpha"}, {"source": preview, "matte": win}, timing={"in": t0, "out": t1},
                  name="preview_shown")
    L.append(f.add("shape", {"kind": "rect", "center": [WX + WW / 2, WY + WH / 2], "size": [WW + 2, WH + 2],
                             "radius": 18, "stroke": 1.5, "color": "#f2f0ea40"},
                   timing={"in": t0 + 0.3, "out": e0}, name="preview_edge"))
    L.append(shown)
    L.append(mono("live", WX + WW - 20, WY - 18, t0 + 0.3, e0, size=18, color=ACID, align="right", nid="preview_live"))
    L.append(f.add("shape", {"kind": "circle", "center": [WX + WW - 70, WY - 24], "size": [10, 10], "color": ACID,
                             "opacity": {"keys": [{"t": t0, "v": [1.0]}, {"t": t0 + 0.25, "v": [0.2]},
                                                  {"t": t0 + 0.5, "v": [1.0]}], "repeat": "cycle"}},
                   timing={"in": t0 + 0.3, "out": e0}, name="preview_dot"))
    L.append(f.add("text", {"text": "Shaders, *written live.*", "font": WIDE, "weight": 700, "size": 110,
                            "color": BONE, **EM, "em_size": 150, "valign": "baseline",
                            "position": [150, 600], "tracking": -3,
                            "glyph_opacity": keys((e0 + 0.4, 0.0), (e0 + 0.7, 1.0)), "stagger": 0.02,
                            "glyph_offset": keys((e0 + 0.4, [0, 60], SNAP), (e0 + 0.9, [0, 0]))},
                   timing={"in": e0 + 0.4, "out": t1}, name="shader_line"))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="shader_bg")
    return f.stack(bg, L)


sections[4] = shifted(4, shaders)


# ================================================================== 06 LIVE (34.03 – 40.03 s)
# The player, with an agent editing the film while it plays: each edit lands
# on a beat, the picture changes in the next frame, the change is outlined,
# and the edit is logged.
def live():
    t0, t1 = CUTS[5], CUTS[6]
    L = []
    PX, PY, PW, PH = 150, 210, 1040, 585  # the picture, 16:9
    pcx, pcy = PX + PW / 2, PY + PH / 2
    edits = [beat(70), beat(72), beat(74), beat(76)]
    e1, e2, e3, e4 = edits
    # Inside the player: a tiny composition, edited four times.
    inner = []
    inner.append(f.add("gradient", {"kind": "radial", "center": [pcx, pcy], "radius": 700,
                                    "stops": "0:#2a2a33, 1:#0c0c10"}, timing={"in": t0, "out": e3}, name="live_bg"))
    m = f.add("marble", {"filaments": 0.4}, timing={"in": e3, "out": t1}, name="live_marble")
    m = f.add("levels", {"in_black": 0.38, "in_white": 0.95, "gamma": 1.4}, {"source": m}, timing={"in": e3, "out": t1},
              name="live_crush")
    inner.append(f.add("tint", {"dark": INK, "light": ACID}, {"source": m}, timing={"in": e3, "out": t1},
                       name="live_tint"))
    title = {"font": WIDE, "weight": 800, "size": 84, "align": "center", "valign": "middle", "position": [pcx, pcy],
             "tracking": -2}
    inner.append(f.add("text", dict(title, text="Hello, world.", color=BONE), timing={"in": t0, "out": e1},
                       name="live_t0"))
    inner.append(f.add("text", dict(title, text="Hello, world.", color=ACID), timing={"in": e1, "out": e2},
                       name="live_t1"))
    inner.append(f.add("text", dict(title, text="Make it yours.", color=ACID,
                                    glyph_offset=keys((e2, [0, 30], SNAP), (e2 + 0.3, [0, 0])), stagger=0.012),
                       timing={"in": e2, "out": e3}, name="live_t2"))
    inner.append(f.add("text", dict(title, text="Make it yours.", color=BONE), timing={"in": e3, "out": t1},
                       name="live_t3"))
    inner.append(f.add("particles", {"seed": 3, "rate": 900, "max": 20000, "life": [1.0, 2.2],
                                     "position": [pcx, pcy + 120], "emitter_size": [PW, 40], "direction": -90,
                                     "spread": 25, "speed": [80, 260], "gravity": [0, -60], "size": [7, 2],
                                     "color_start": BONE, "color_end": ACID, "blend": "add", "emit_start": e4 - 0.1},
                       timing={"in": e4, "out": t1}, name="live_particles"))
    picture = inner[0]
    for lid in inner[1:]:
        picture = f.over(lid, picture, name="live_pic")
    frame = f.add("shape", {"kind": "rect", "center": [pcx, pcy], "size": [PW, PH], "radius": 10, "color": "#ffffff"},
                  name="live_frame")
    L.append(f.add("mask", {"mode": "alpha"}, {"source": picture, "matte": frame}, timing={"in": t0, "out": t1},
                   name="live_picture"))
    L.append(f.add("shape", {"kind": "rect", "center": [pcx, pcy], "size": [PW + 2, PH + 2], "radius": 10,
                             "stroke": 1.5, "color": "#f2f0ea30"}, timing={"in": t0, "out": t1}, name="live_edge"))
    # The timeline under it, playhead running.
    ty = PY + PH + 46
    L.append(f.add("shape", {"kind": "rect", "center": [pcx, ty], "size": [PW, 4], "radius": 2, "color": "#f2f0ea30"},
                   timing={"in": t0, "out": t1}, name="live_track"))
    L.append(f.add("shape", {"kind": "rect", "size": [PW, 4], "radius": 2, "color": ACID,
                             "center": keys((t0, [PX - PW / 2, ty], "linear"), (t1, [PX + PW * 0.5, ty]))},
                   timing={"in": t0, "out": t1}, name="live_progress"))
    clip = f.add("shape", {"kind": "rect", "center": [pcx, ty], "size": [PW + 2, 40], "color": "#ffffff"},
                 name="live_track_clip")
    L[-1] = f.add("mask", {"mode": "alpha"}, {"source": L[-1], "matte": clip}, timing={"in": t0, "out": t1},
                  name="live_progress_clip")
    # Each edit: an outline round what changed, flashing in and fading.
    boxes = [([pcx, pcy], [700, 130]), ([pcx, pcy], [760, 130]), ([pcx, pcy], [PW - 20, PH - 20]),
             ([pcx, pcy + 60], [PW - 60, 360])]
    for (c, sz), e in zip(boxes, edits):
        L.append(f.add("shape", {"kind": "rect", "center": c, "size": sz, "radius": 6, "stroke": 2.5, "color": ACID,
                                 "opacity": keys((e, 1.0), (e + 0.9, 0.0)),
                                 "scale": keys((e, 1.04, SNAP), (e + 0.3, 1.0))},
                       timing={"in": e, "out": e + 0.95}, name="live_outline"))
    # The agent's log, one line per edit.
    LX = PX + PW + 70
    L.append(mono("AGENT", LX, PY + 6, t0, t1, size=16, color=GREY, nid="log_head"))
    log = [('set_param  title.color  "#d4ff3a"', e1), ('set_param  title.text  "Make it yours."', e2),
           ("add_node  marble  ->  background", e3), ("add_layer  particles  count 20000", e4)]
    for i, (line, e) in enumerate(log):
        y = PY + 70 + i * 92
        L.append(mono("▸ " + line, LX, y, e, t1, size=22, color=BONE, nid="log_line",
                      reveal=keys((e, 0.0), (e + 0.35, float(len(line) + 2), "linear"))))
        L.append(mono("applied  ·  next frame  ·  no recompile" if i < 2 else "applied  ·  next frame", LX, y + 32,
                      e + 0.3, t1, size=17, color=ACID, nid="log_ack",
                      opacity=keys((e + 0.3, 0.0), (e + 0.45, 1.0))))
    L.append(f.add("text", {"text": "Edit it *while it plays.*", "font": WIDE, "weight": 700, "size": 64,
                            "color": BONE, **EM, "em_size": 88, "valign": "baseline", "position": [PX, 150],
                            "tracking": -2, "glyph_opacity": keys((t0, 0.0), (t0 + 0.4, 1.0)), "stagger": 0.015,
                            "glyph_offset": keys((t0, [0, 40], SNAP), (t0 + 0.6, [0, 0]))},
                   timing={"in": t0, "out": t1}, name="live_line"))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="live_bgfill")
    return f.stack(bg, L)


sections[5] = shifted(5, live)


# ================================================================== 07 ULTRAFAST (40.03 – 44 s)
# The final hit: the dot grid returns, rushes into the letters, and the
# wordmark lands; then the line, and what it is.
def finale():
    t0, t1 = CUTS[6], CUTS[7]
    L = []
    L.append(f.add("swarm", {"text": "ultrafast", "font": WIDE, "weight": 800, "size": 330, "align": "center",
                             "valign": "middle", "position": [CX, CY - 40], "count": 16000, "scatter": 1100,
                             "particle_size": [2.4, 3.2], "color": BONE, "color_scatter": ACID, "turbulence": 40,
                             "stretch": 0.8, "blend": "add",
                             "progress": keys((t0 - 0.02, 0.15, SNAP), (t0 + 0.55, 1.0))},
                   timing={"in": t0 - 0.02, "out": t0 + 0.8}, name="final_swarm"))
    L[-1] = f.add("opacity", {"opacity": keys((t0 + 0.5, 1.0), (t0 + 0.8, 0.0))}, {"source": L[-1]},
                  timing={"in": t0 - 0.02, "out": t0 + 0.8}, name="final_swarm_fade")
    # The boot's dot grid comes back, faint, ring by ring.
    G, cols, rows = 96, 17, 9
    gx0, gy0 = CX - (cols - 1) * G / 2, CY - (rows - 1) * G / 2
    for r in range(rows):
        for c in range(cols):
            ring = max(abs(c - cols // 2), abs(r - rows // 2))
            ta = t0 + 0.9 + ring * 0.07
            L.insert(0, f.add("shape", {"kind": "circle", "size": [6, 6], "color": BONE, "center": [gx0 + c * G, gy0 + r * G],
                                        "opacity": keys((ta, 0.0), (ta + 0.3, 0.16))},
                              timing={"in": ta, "out": t1}, name="final_dot"))
    word = f.add("text", {"text": "ultrafast", "font": WIDE, "weight": 800, "size": 330, "align": "center",
                          "valign": "middle", "position": [CX, CY - 40], "color": BONE, "tracking": -10,
                          "opacity": keys((t0 + 0.45, 0.0), (t0 + 0.7, 1.0))},
                  timing={"in": t0 + 0.45, "out": t1}, name="final_word")
    L.append(f.add("glow", {"radius": 30, "intensity": keys((t0 + 0.5, 1.2), (t0 + 1.6, 0.25))},
                   {"source": split(word, t0 + 0.45, t1, peak=14.0)},
                   timing={"in": t0 + 0.45, "out": t1}, name="final_glow"))
    L.append(f.add("shape", {"kind": "rect", "color": ACID, "center": [CX, CY + 140],
                             "size": keys((t0 + 0.6, [0, 10], SNAP), (t0 + 1.1, [620, 10]))},
                   timing={"in": t0 + 0.6, "out": t1}, name="final_bar"))
    L.append(f.add("text", {"text": "Every frame is a *live document.*", "font": SANS, "weight": 500, "size": 46,
                            "color": BONE, **EM, "em_size": 62, "align": "center", "valign": "baseline", "position": [CX, CY + 240],
                            "glyph_opacity": keys((t0 + 1.0, 0.0), (t0 + 1.4, 1.0)), "stagger": 0.012},
                   timing={"in": t0 + 1.0, "out": t1}, name="final_line"))
    L.append(mono("open source  ·  built for agents  ·  made entirely in ultrafast", CX, CY + 310, t0 + 1.6, t1, size=20,
                  color=GREY, align="center", nid="final_mono",
                  reveal=keys((t0 + 1.6, 0.0), (t0 + 2.3, 70.0, "linear"))))
    bg = f.add("solid", {"color": "#00000000"}, timing={"in": t0, "out": t1}, name="final_bg")
    flash = f.add("solid", {"color": ACID}, timing={"in": t0, "out": t0 + 0.25}, name="final_flash")
    flash = f.add("opacity", {"opacity": keys((t0, 0.9), (t0 + 0.25, 0.0))}, {"source": flash},
                  timing={"in": t0, "out": t0 + 0.25}, name="final_flash_o")
    return f.stack(bg, L + [flash])


sections[6] = shifted(6, finale)

# ================================================================== the camera
# One camera over the whole film (one continuous motion): it pushes,
# trucks and rolls with intent inside each section; seams cut mid-move and
# carry their speed across (the current runs left; zoom-through goes deeper
# into an idea; the inverse zoom is reserved for arrivals; up is the end).
def e_lin(u): return u
def e_in3(u): return u ** 3
def e_in4(u): return u ** 4
def e_out4(u): return 1 - (1 - u) ** 4
def e_expo(u): return 1.0 if u >= 1 else 1 - 2 ** (-10 * u)
def e_inout(u): return 4 * u ** 3 if u < 0.5 else 1 - (-2 * u + 2) ** 3 / 2
def e_drift(u): return u * u * (3 - 2 * u)


class Camera:
    """A pan (world point at frame center), zoom and roll, as a sequence of
    non-overlapping moves; kicks add short zoom impulses on beats."""

    def __init__(self, p, z=1.0, r=0.0):
        self.start = (p[0], p[1], z, r)
        self.moves, self.kicks = [], []
        # Moves are written on a section's CUTS timeline; `offset` moves them
        # to where that section plays (`at` and `view` take film time).
        self.offset = 0.0

    def to(self, ta, tb, p=None, z=None, r=None, ease=e_inout):
        self.moves.append((ta + self.offset, tb + self.offset, p, z, r, ease))
        return self

    def set(self, t, p=None, z=None, r=None):
        return self.to(t, t, p, z, r, e_lin)

    def kick(self, t, amount=0.025, decay=0.2):
        self.kicks.append((t + self.offset, amount, decay))
        return self

    def at(self, t):
        x, y, z, r = self.start
        for ta, tb, p, zz, rr, ease in self.moves:
            if t < ta:
                break
            tgt = (p[0] if p else x, p[1] if p else y, z if zz is None else zz, r if rr is None else rr)
            if tb <= ta or t >= tb:
                x, y, z, r = tgt
            else:
                u = ease((t - ta) / (tb - ta))
                x, y, z, r = [a + (b - a) * u for a, b in zip((x, y, z, r), tgt)]
                break
        for kt, amt, dec in self.kicks:
            if t >= kt:
                z *= 1 + amt * min(1.0, (t - kt) / 0.05) * math.exp(-(t - kt) / dec)
        return x, y, z, r

    def view(self, t, depth=1.0):
        """(anchor, scale, rotation) of the transform for a plane at `depth`."""
        x, y, z, r = self.at(t)
        return [CX + (x - CX) / depth, CY + (y - CY) / depth], 1 + (z - 1) / depth, r

    def screen(self, t, q):
        """Where world point q (on the focal plane) lands in the frame."""
        (ax, ay), s, r = self.view(t)
        c, sn = math.cos(math.radians(r)), math.sin(math.radians(r))
        dx, dy = (q[0] - ax) * s, (q[1] - ay) * s
        return [CX + c * dx - sn * dy, CY + sn * dx + c * dy]

    def local(self, t):
        """State at section time t (on the CUTS timeline)."""
        return self.at(t + self.offset)

    def times(self, t0=0.0, t1=58.5, rate=60):
        ts = {round(t0 + i / rate, 4) for i in range(int((t1 - t0) * rate) + 1)}
        for ta, tb, *_ in self.moves:
            ts |= {ta, tb, round(ta - 0.0005, 4)}
        return sorted(t for t in ts if t0 <= t <= t1)


def cut_left(cam, t, z=1.0, out=0.3, inn=0.42, travel=230):
    """Cut the curve, leftward: the outgoing shot accelerates left, the cut
    lands mid-move, the incoming shot arrives from the right decelerating."""
    x, y, zz, r = cam.local(t - out)
    cam.to(t - out, t, p=(x + travel / zz, y), ease=e_in4)
    seams.append((t + cam.offset, out, 0.02, 9.0))

    def enter(p, z=z, r=0.0):
        # The incoming side may play later (after a reel): its own offset.
        cam.set(t, p=(p[0] - travel / z, p[1]), z=z, r=r).to(t, t + inn, p=p, ease=e_out4)
        seams.append((t + cam.offset, 0.02, inn, 9.0))
    return enter


def zoom_through(cam, t, p, z_in=1.0, r=0.0, out=0.24, inn=0.5):
    """Deeper into the same idea: everything grows across the swap."""
    x, y, zz, rr = cam.local(t - out)
    cam.to(t - out, t, z=zz * 1.22, ease=e_in3)
    cam.set(t, p=p, z=z_in * 0.78, r=r).to(t, t + inn, z=z_in, ease=e_expo)
    seams.append((t + cam.offset, out, inn * 0.6, 10.0))


def inverse_zoom(cam, t, p, z_in=1.0, out=0.26, inn=0.6):
    """An arrival: the outgoing shot recedes, the incoming one lands from
    just behind the camera."""
    x, y, zz, rr = cam.local(t - out)
    cam.to(t - out, t, z=zz * 0.82, ease=e_in3)
    cam.set(t, p=p, z=z_in * 1.28, r=0.0).to(t, t + inn, z=z_in, ease=e_expo)
    seams.append((t + cam.offset, out, inn * 0.6, 10.0))


seams = []
cam = Camera((CX - 160, 390), 1.75)
# 01 BOOT: tight on the prompt, following the typing across and each new
# line down; a breath on "live"; zoom through into the grid; truck with the
# playhead; push through the dots into the drop.
cam.to(0.3, 1.45, p=(CX + 40, 400), z=1.72, ease=e_drift)
cam.to(1.5, 1.85, p=(CX - 60, 455), z=1.62)
cam.to(1.9, 2.95, p=(CX + 40, 500), z=1.55, ease=e_drift)
cam.to(2.95, 3.4, p=(CX, 545), z=1.42, ease=e_expo)
zoom_through(cam, 3.85, (CX, CY), 1.0)
cam.to(4.35, 7.5, p=(CX + 60, CY - 10), z=1.05, ease=e_drift)
cam.to(7.5, CUTS[1], p=(CX, CY), z=1.7, ease=e_in4)
seams.append((CUTS[1], 0.3, 0.02, 10.0))
cam.offset = SHIFT[1]
# 02 DOCUMENT: arrive tight on the first card, travel with the graph as it
# builds (leaving an 8th early, landing before the beat), pull wide for the
# line, lean in toward the export card, then recenter as it opens.
cam.set(CUTS[1], p=(450, 470), z=1.25).to(CUTS[1], CUTS[1] + 0.37, z=1.6, ease=e_expo)
cam.to(8.40, 8.75, p=(480, 560), z=1.45)
cam.to(8.78, 9.20, p=(760, 500), z=1.3)
cam.to(9.25, 9.70, p=(1080, 530), z=1.18)
cam.to(9.75, 10.45, p=(960, 560), z=1.0)
cam.to(10.63, 11.48, p=(1160, 545), z=1.1, ease=e_drift)
cam.to(11.53, 12.08, p=(CX, CY), z=1.0, ease=e_expo)
cam.to(12.13, CUTS[2] - 0.3, z=1.06, ease=e_drift)
enter = cut_left(cam, CUTS[2])
# 03 TYPE: the field rolls and pushes; zoom through to the glyphs; cut left
# to the ring, rolling with it; zoom through into the swarm.
enter((CX, CY))
cam.to(CUTS[2] + 0.42, beat(32) - 0.24, z=1.16, r=-5.0, ease=e_drift)
zoom_through(cam, beat(32), (CX, CY), 1.0)
cam.to(beat(32) + 0.5, beat(36) - 0.3, z=1.06, ease=e_drift)
cut_left(cam, beat(36))((CX, CY))
cam.to(beat(36) + 0.42, beat(40) - 0.24, z=1.14, r=7.0, ease=e_drift)
zoom_through(cam, beat(40), (CX, CY), 1.0)
cam.to(beat(40) + 0.5, CUTS[3] - 0.3, z=1.05, ease=e_drift)
# 04 SIMULATION: the camera tilts up with the rising smoke; GROW lands as an
# arrival; a slow push into the growth.
cut_left(cam, CUTS[3])((CX, CY))
cam.to(CUTS[3] + 0.45, beat(50) - 0.26, p=(CX, CY - 90), z=1.07, ease=e_drift)
inverse_zoom(cam, beat(50), (CX, CY), 1.0)
cam.to(beat(50) + 0.6, CUTS[4] - 0.3, z=1.08, ease=e_drift)
# 05 SHADERS: tight on the code, following the typing down; across to the
# preview as it changes; wide for the click; the preview opens.
cut_left(cam, CUTS[4], z=1.5)((600, 400), z=1.5)
cam.to(CUTS[4] + 0.45, beat(60) - 0.1, p=(640, 470), z=1.45, ease=e_drift)
cam.to(beat(60) - 0.1, beat(61) + 0.1, p=(1290, 530), z=1.3)
cam.to(beat(62) - 0.1, beat(63) - 0.1, p=(CX, CY), z=1.0)
cam.to(beat(64) + 0.55, CUTS[5] - 0.3, z=1.07, ease=e_drift)
# 06 LIVE: from the picture across to the agent's log as it writes, a kick
# on every edit.
enter = cut_left(cam, CUTS[5], z=1.15)
cam.offset = SHIFT[5]
enter((720, 520), z=1.15)
cam.to(CUTS[5] + 0.45, beat(78), p=(1000, 500), z=1.08, ease=e_drift)
for e in (beat(70), beat(72), beat(74), beat(76)):
    cam.kick(e)
# 07 ULTRAFAST: the wordmark arrives; a slow push; up and out for the end.
inverse_zoom(cam, CUTS[6], (CX, CY), 1.0)
cam.to(CUTS[6] + 0.7, 42.7, z=1.04, ease=e_drift)
cam.to(42.7, 43.8, p=(CX, CY + 180), ease=e_in3)


def camera_on(src, depth=1.0, nid="camera"):
    ts = cam.times()
    views = [(t, cam.view(t, depth)) for t in ts]
    return f.add("transform", {"position": [CX, CY],
                               "anchor": keys(*[(t, v[0], "linear") for t, v in views]),
                               "scale": keys(*[(t, v[1], "linear") for t, v in views]),
                               "rotation": keys(*[(t, v[2], "linear") for t, v in views])},
                 {"source": src}, name=nid)


# The far plane: a faint field of points at three times the distance, drawn
# analytically from the camera (it never runs out at the edges), so every
# camera move reads as depth.
FAR = """
float4 shade(float2 pos, float px) {
  float2 c = size() * 0.5;
  float s = param(2), a = radians(param(3));
  float2 d = pos - c;
  float2 q = float2(cos(a) * d.x + sin(a) * d.y, -sin(a) * d.x + cos(a) * d.y) / s + float2(param(0), param(1));
  float2 cell = q / 120.0;
  float2 g = (frac(cell) - 0.5) * 120.0;
  float dot_ = smoothstep(2.4, 1.2, length(g) * s);
  float a2 = dot_ * 0.16 * param(4);
  return float4(float3(0.95, 0.94, 0.92) * a2, a2);
}"""
far_views = [(t, cam.view(t, 3.0)) for t in cam.times(rate=30)]
far = f.add("shader", {"code": FAR,
                       "u0": keys(*[(t, v[0][0], "linear") for t, v in far_views]),
                       "u1": keys(*[(t, v[0][1], "linear") for t, v in far_views]),
                       "u2": keys(*[(t, v[1], "linear") for t, v in far_views]),
                       "u3": keys(*[(t, v[2], "linear") for t, v in far_views]),
                       "u4": keys((NEW_CUTS[1], 0.0), (NEW_CUTS[1] + 0.3, 1.0), (NEW_CUTS[6], 1.0),
                                  (NEW_CUTS[6] + 0.3, 0.0))},
          timing={"in": NEW_CUTS[1], "out": NEW_CUTS[6] + 0.3}, name="far")


# The oversized cursor: enters from below, its tip homes on a target as the
# camera moves, taps (0.84, 0.1 s in, 0.22 s out), and its click ignites the
# next beat on the same frame; then it leaves the frame.
ARROW = [(0, 0), (0, 17), (4.2, 13.2), (6.9, 19.6), (9.6, 18.5), (7.0, 12.3), (12.6, 12.3)]
CURSOR_D = "M " + " L ".join(f"{x * 6.4:.1f} {y * 6.4:.1f}" for x, y in ARROW) + " Z"


def cursor(target, t_in, t_arrive, t_click, t_leave, t_gone, nid="cursor"):
    start = [target[0] + 260, H + 160]
    out_to = [W + 140, H + 220]
    ts = [t_in + i / 60 for i in range(int((t_gone - t_in) * 60) + 1)] + [t_gone]
    pos = []
    for t in ts:
        tgt = cam.screen(t, target)
        if t < t_arrive:
            u = 1 - (1 - (t - t_in) / (t_arrive - t_in)) ** 3
            p = [start[0] + (tgt[0] - start[0]) * u, start[1] + (tgt[1] - start[1]) * u]
        elif t < t_leave:
            p = tgt
        else:
            u = ((t - t_leave) / (t_gone - t_leave)) ** 2
            p = [tgt[0] + (out_to[0] - tgt[0]) * u, tgt[1] + (out_to[1] - tgt[1]) * u]
        pos.append((t, p, "linear"))
    tap = keys((t_click, 1.0), (t_click + 0.1, 0.84), (t_click + 0.32, 1.0))
    body = f.add("path", {"d": CURSOR_D, "color": "#1c1c1c", "position": keys(*pos), "scale": tap},
                 timing={"in": t_in, "out": t_gone}, name=f"{nid}_body")
    edge = f.add("path", {"d": CURSOR_D, "stroke": 3.2, "join": "round", "color": "#ffffff",
                          "position": keys(*pos), "scale": tap}, timing={"in": t_in, "out": t_gone}, name=f"{nid}_edge")
    arrow = f.over(edge, body, name=f"{nid}_arrow")
    return f.add("drop_shadow", {"opacity": 0.45, "offset": [0, 8], "radius": 14}, {"source": arrow},
                 timing={"in": t_in, "out": t_gone}, name=nid)


def press(center, size, t, nid="press"):
    """The clicked thing answers on the click's frame: a flash of light."""
    flash = f.add("shape", {"kind": "rect", "center": center, "size": size, "radius": 14, "color": ACID,
                            "opacity": keys((t, 0.0), (t + 0.04, 0.55), (t + 0.45, 0.0))},
                  timing={"in": t, "out": t + 0.45}, name=nid)
    return flash



# ================================================================== the reels
# Fast cuts through ultrafast's own films, on the beat and then on eighths:
# each shot is the source film itself, pulled in as a component and
# played from a chosen moment, punched in on the cut, landing with a
# chromatic burst, named as it lands.
import json as _json  # noqa: E402

SOURCES = {"engine": "engine/engine.json", "meridian": "meridian/meridian.json", "aurora": "aurora/aurora.json"}
for name, rel in SOURCES.items():
    src = _json.load(open(os.path.join(ROOT, rel)))
    nodes = {k: v for k, v in src["nodes"].items() if v["op"] != "audio"}
    f.components[f"film_{name}"] = {"nodes": nodes, "output": src["output"]}

SHOTS = {
    "E1": ("engine", 8.8, "LIQUID CHROME"), "E2": ("engine", 10.8, "GLASS PRISMS"),
    "E3": ("engine", 12.7, "IRIDESCENT FOIL"), "E4": ("engine", 15.3, "RAYMARCHED SCULPTURE"),
    "E5": ("engine", 17.2, "FLUID SMOKE"), "E6": ("engine", 19.6, "EIGHT LIVE PLATES"),
    "E7": ("engine", 22.0, "CHROME TYPE"),
    "M1": ("meridian", 5.6, "BRASS, RAYMARCHED"), "M2": ("meridian", 11.2, "PROCEDURAL CHART"),
    "M3": ("meridian", 14.8, "RESOLUTION-FREE"), "M4": ("meridian", 18.8, "SHADER SEA, STAR TRAILS"),
    "M5": ("meridian", 22.6, "ARMILLARY"), "M6": ("meridian", 27.2, "VOLUMETRIC BEAM"),
    "M7": ("meridian", 29.6, "TITLE SEQUENCE"),
    "A1": ("aurora", 3.0, "PRODUCT RENDER"), "A2": ("aurora", 6.2, "WOVEN RELIEF"),
    "A3": ("aurora", 10.2, "GOLD, SOFT SHADOWS"), "A4": ("aurora", 15.0, "EXPLODED VIEW"),
    "A5": ("aurora", 18.6, "THREE FINISHES"),
}


def reel(t0, t1, cuts, nid):
    """`cuts`: [(at, shot id, extra source seconds)]. Returns the reel."""
    shots, punch, burst, labels = [], [], [], []
    cuts = [(between_frames(at), sid, extra) for at, sid, extra in cuts]
    ends = [c[0] for c in cuts[1:]] + [t1]
    for i, ((at, sid, extra), end) in enumerate(zip(cuts, ends)):
        film_name, src_t, label = SHOTS[sid]
        shots.append(f.add(f"film_{film_name}", {}, timing={"in": at, "out": end, "offset": at - (src_t + extra)},
                           name=f"{nid}_{sid}"))
        punch += [(at, 1.07, SNAP), (min(at + 0.4, end), 1.0)]
        burst += [(at, 14.0), (at + 0.12, 3.0, OUT), (min(at + 0.3, end), 0.8)]
        if end - at >= 0.45:
            # Named at the bottom center, clear of each film's own chrome.
            plate = f.add("shape", {"kind": "rect", "center": [CX, H - 92], "size": [len(label) * 15.5 + 70, 40],
                                    "radius": 4, "color": "#08080ad0"},
                          timing={"in": at + 0.03, "out": end}, name=f"{nid}_plate")
            labels.append(plate)
            labels.append(f.add("shape", {"kind": "rect", "center": [CX - len(label) * 7.75 - 14, H - 92],
                                          "size": [10, 10], "color": ACID},
                                timing={"in": at + 0.03, "out": end}, name=f"{nid}_tick"))
            labels.append(mono(f"{label}", CX + 10, H - 85, at + 0.03, end, size=20, color=BONE, align="center",
                               nid=f"{nid}_label", tracking=3,
                               reveal=keys((at + 0.03, 0.0), (at + 0.22, float(len(label)), "linear"))))
    stack = f.stack(f.add("solid", {"color": INK}, timing={"in": t0, "out": t1}, name=f"{nid}_ground"), shots)
    # A full-frame crop holds the camera move above the films (they are
    # moved as pictures, not re-laid out).
    stack = f.add("crop", {"rect": [0, 0, W, H], "softness": 0.0}, {"source": stack},
                  timing={"in": t0, "out": t1}, name=f"{nid}_frame")
    stack = f.add("transform", {"anchor": [CX, CY], "position": [CX, CY], "scale": keys(*punch)}, {"source": stack},
                  timing={"in": t0, "out": t1}, name=f"{nid}_punch")
    stack = f.add("shader", {"code": SPLIT, "u0": keys(*burst)}, {"in0": stack}, timing={"in": t0, "out": t1},
                  name=f"{nid}_burst")
    return f.stack(stack, labels)


M_ = 56
# Reel I, on the drop: a shot a beat for twelve beats, then eighths.
order1 = ["E1", "M4", "A1", "E2", "M1", "E3", "A4", "M5", "E5", "A2", "M6", "E4",
          "E7", "A3", "M2", "E6", "A5", "M3", "E1", "M7"]
cuts1 = [(beat(16 + i), sid, 0.0) for i, sid in enumerate(order1[:12])]
cuts1 += [(beat(28) + 0.25 * i, sid, 0.6 if sid == "E1" else 0.0) for i, sid in enumerate(order1[12:])]
reel1 = reel(REEL1[0], REEL1[1], cuts1, "reel1")
# Reel II, at the peak: eighths throughout, later moments of the same films.
order2 = ["A1", "E2", "M4", "E3", "A3", "M1", "E5", "M5", "A4", "E1", "M6", "A2",
          "E7", "M2", "A5", "E4", "M3", "E6", "A1", "M7", "E3", "A3", "M4", "E2"]
extra2 = [1.0, 0.8, 1.5, 1.0, 1.0, 1.5, 0.7, 1.0, 0.8, 1.2, 1.0, 1.0,
          1.0, 1.2, 0.6, 0.6, 0.6, 0.5, 2.2, 0.5, 1.6, 1.8, 2.4, 1.6]
cuts2 = [(beat(84) + 0.25 * i, sid, extra2[i]) for i, sid in enumerate(order2)]
reel2 = reel(REEL2[0], REEL2[1], cuts2, "reel2")

# ------------------------------------------------------------------ assemble
f.add("audio", {"src": os.path.join(D, "audio", "showcase_score.wav"), "gain": 1.0}, name="score")
ground = f.add("solid", {"color": INK}, name="ground")
clear = f.add("solid", {"color": "#00000000"}, name="clear")
layers = []
for k in range(len(NAMES)):
    layers.append(sections.get(k) or slate(k))
# The clicks happen in the world (they move with the camera).
layers.append(press([1555, 525], [270, 96], beat(22) + SHIFT[1], nid="press_export"))
layers.append(press([1485, 530], [650, 560], beat(63) + 0.2 + SHIFT[4], nid="press_preview"))
world = f.stack(clear, layers)
film = f.over(camera_on(world), f.over(far, ground, name="far_ground"), name="filmed")
film = f.stack(film, [reel1, reel2])
# Seams blur on both sides at the swap (the eye reads speed, not a cut).
for i, (t, a, b_, peak) in enumerate(seams):
    film = f.add("blur", {"radius": keys((t - a, 0.0), (t, peak, "in_quad"), (t + b_, 0.0, "out_quad"))},
                 {"source": film}, timing={"in": t - a, "out": t + b_}, name=f"seam{i}")
# On every cut after the drop, an acid slab wipes across on the beat.
wipes = []
for t in [NEW_CUTS[k] for k in (2, 3, 4)]:
    wipes.append(f.add("shape", {"kind": "rect", "color": ACID, "size": [260, H + 40],
                                 "center": keys((t - 0.14, [W + 200, CY], "linear"), (t + 0.14, [-200, CY]))},
                       timing={"in": t - 0.14, "out": t + 0.14}, name="wipe"))
film = f.stack(film, wipes)
s1, s4 = SHIFT[1], SHIFT[4]
film = f.stack(film, [cursor([1555, 525], beat(21) - 0.05 + s1, beat(22) - 0.1 + s1, beat(22) + s1, beat(22) + 0.25 + s1,
                             beat(23) + 0.1 + s1, nid="cursor_doc"),
                      cursor([1485, 530], beat(62) + 0.3 + s4, beat(63) + 0.1 + s4, beat(63) + 0.2 + s4,
                             beat(63) + 0.4 + s4, beat(64) + 0.2 + s4, nid="cursor_shader")])

# The document chrome, over everything once ultrafast has booted: corner
# marks, the file name, a frame counter, the section, the format.
chrome = []
M = 56
for (x, y) in [(M, M), (W - M, M), (M, H - M), (W - M, H - M)]:
    sx, sy = (1 if x < CX else -1), (1 if y < CY else -1)
    chrome.append(f.add("path", {"d": f"M {x} {y + 22 * sy} L {x} {y} L {x + 22 * sx} {y}", "stroke": 2,
                                 "color": BONE, "opacity": 0.8,
                                 "trim_end": keys((beat(12), 0.0, SNAP), (beat(13), 1.0))},
                        timing={"in": beat(12), "out": NEW_CUTS[6]}, name="corner"))
chrome.append(mono("flagship.json", M, M - 18, beat(13), NEW_CUTS[6], size=18, nid="hud_file",
                   reveal=keys((beat(13), 0.0), (beat(13) + 0.4, 13.0, "linear"))))
chrome.append(mono("", W - M, M - 18, beat(13), NEW_CUTS[6], size=18, align="right", nid="hud_frame", prefix="F ",
                   number=keys((beat(13), 0.0), (58.5, 30 * (58.5 - beat(13)), "linear"))))
spans = [(NEW_CUTS[1], NEW_CUTS[2], "02 / DOCUMENT"), (NEW_CUTS[2], NEW_CUTS[3], "03 / TYPE"),
         (NEW_CUTS[3], NEW_CUTS[4], "04 / SIMULATION"), (NEW_CUTS[4], REEL2[0], "05 / SHADERS"),
         (NEW_CUTS[5], NEW_CUTS[6], "06 / LIVE"), (REEL1[0], REEL1[1], "SHOWCASE"), (REEL2[0], REEL2[1], "SHOWCASE")]
for a, b, label in spans:
    chrome.append(mono(label, M, H - M + 34, a, b, size=18, color=BONE, nid="hud_section",
                       reveal=keys((a, 0.0), (a + 0.35, 20.0, "linear"))))
chrome.append(mono("1920 x 1080  ·  30 fps  ·  live", W - M, H - M + 34, beat(13), NEW_CUTS[6], size=18, align="right",
                   nid="hud_format"))
film = f.stack(film, chrome)
film = f.add("grain", {"amount": 0.035}, {"source": film}, name="grain")
f.output = film
print(f.save(os.path.join(D, "flagship.json")))

"""ASSEMBLE — every shape can become any other: letterforms morph one into
the next, the last one dissolves into particles, and the particles
assemble into the wordmark. 1080x1920, 10 s, with sound."""

import json
import subprocess

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=10.0, motion_blur=(6, 0.5))
W, H = f.W, f.H
ULTRAFAST = os.path.join(FILMS_DIR, "..", "..", "target", "release", "ultrafast")
SCORE = "../../out/films/hero_score.wav"
FR = "Fraunces-VF.ttf"
CX, BASE, SIZE = W / 2, 1290, 760


def outline(ch, fnt=FR, weight=900):
    params = json.dumps({"text": ch, "font": font(fnt), "weight": weight, "size": SIZE, "position": [CX, BASE],
                         "align": "center", "valign": "baseline", "axis_SOFT": 100, "axis_opsz": 144})
    return subprocess.run([ULTRAFAST, "outline", params, "--base", FILMS_DIR], capture_output=True, text=True,
                          check=True).stdout.strip()


f.add("audio", {"src": SCORE, "start": 8.0, "gain": keys((0, 0.0), (0.4, 1.0), (9.0, 1.0), (10.0, 0.0))}, name="score")

ground = f.add("gradient", {"kind": "radial", "center": [CX, 1000], "radius": 1400,
                            "stops": "0:#2a1a12, 0.55:#0e0a08, 1:#040303"}, name="ground")

# Opening lines.
serif, italic = "InstrumentSerif-Regular.ttf", "InstrumentSerif-Italic.ttf"
lines = []
for i, (txt, fnt, t0, col) in enumerate([("Every shape", serif, 0.3, "#f1eee8"), ("can become", italic, 0.45, "#ffd7a8")]):
    y = 300 + i * 150
    tx = f.text(txt, fnt, 140, [84, y], color=col, tracking=-3, name="line")
    lay, win = line_reveal(f, tx, 84, y, 140, t0, t_out=6.1 + i * 0.06, dur=0.7)
    lines.append((lay, None, win))
another = f.text("any other.", italic, 140, [84, 1700], color="#ffd7a8", tracking=-3, name="line")
lay, win = line_reveal(f, another, 84, 1700, 140, 2.0, t_out=6.2, dur=0.7)
lines.append((lay, None, win))

# The morph chain: each path holds one transition.
glyphs = ["8", "&", "§", "@", "g"]
outs = [outline(g) for g in glyphs]
chain = []
times = [1.0, 2.2, 3.4, 4.6]  # each morph starts here and takes 0.8 s
for k, t0 in enumerate(times):
    end = times[k + 1] if k + 1 < len(times) else 6.25
    p = f.add("path", {"d": outs[k], "to": outs[k + 1], "color": "#ffcf73",
                       "morph": keys((t0, 0.0, INOUT), (t0 + 0.8, 1.0))}, name="morph")
    chain.append((p, None, {"in": 0.0 if k == 0 else t0, "out": end}))
glyph_in = chain[0][0]
f.nodes[glyph_in]["params"]["color"] = "#ffcf73"
fade_in = {"opacity": keys((0.2, 0.0, SOFT), (0.9, 1.0))}
chain[0] = (glyph_in, fade_in, chain[0][2])

# The last glyph breaks into particles, which then become the wordmark.
dissolve = f.add("swarm", {"text": "g", "font": font(FR), "weight": 900, "size": SIZE, "position": [CX, BASE], "align": "center",
                           "valign": "baseline", "axis_SOFT": 100, "axis_opsz": 144, "count": 7000,
                           "particle_size": [2.4, 2.4], "color": "#ffcf73", "color_scatter": "#ff8a5b", "blend": "add",
                           "scatter": 900, "stagger": 0.6, "turbulence": 60, "stretch": 0.5,
                           "progress": keys((6.2, 1.0, IN), (7.4, 0.0))}, name="dissolve")
word = f.add("swarm", {"text": "ultrafast", "font": font("Unbounded-VF.ttf"), "weight": 800, "size": 210, "position": [CX, 1050],
                       "align": "center", "valign": "baseline", "count": 11000, "particle_size": [2.2, 2.6],
                       "color": "#fff1dc", "color_scatter": "#ff8a5b", "blend": "add", "scatter": 1100, "stagger": 0.55,
                       "turbulence": 50, "shimmer": 0.35, "stretch": 0.5,
                       "progress": keys((6.9, 0.0, OUT), (8.9, 1.0))}, name="word")
tag = f.text("ANY SHAPE, LIVE", "GeistMono-VF.ttf", 30, [CX, 1170], align="center", color="#fff1dcb0", weight=500, tracking=6,
             stagger=0.03, glyph_opacity=keys((0.0, 0, "hold"), (0.04, 1)), name="tag")
f.nodes[tag]["timing"] = {"offset": 8.9}

comp = f.stack(ground, chain + lines + [(dissolve, None, {"in": 6.2, "out": 7.6}), (word, None, {"in": 6.9}), (tag, None, {"in": 8.9})])
comp = f.add("bloom", {"threshold": 0.55, "radius": 30, "intensity": 0.7, "wide": 0.35}, {"source": comp}, name="bloom")
f.output = finish(f, comp, grain=0.08)
f.save(os.path.join(FILMS_DIR, "assemble.json"))
print("ok", len(f.nodes), "nodes")

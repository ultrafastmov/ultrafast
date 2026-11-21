"""HERO — every style in one live document. Each film is pulled in as a
component and instanced on the timeline with its own local time, cut on
the beat of an original 120 BPM score. 1080x1920, 26 s."""

import json

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=26.0, motion_blur=(6, 0.5))
W, H = f.W, f.H

# (film, hero in, hero out, film time at hero in)
CUTS = [
    ("ink", 4.0, 6.5, 3.85),
    ("riso", 6.5, 8.5, 0.9),
    ("glass", 8.5, 10.5, 1.0),
    ("clay", 10.5, 12.5, 0.8),
    ("kinetic", 12.5, 14.5, 0.25),
    ("chrome", 14.5, 16.5, 5.0),
    ("signal", 16.5, 18.0, 0.25),
    ("terminal", 18.0, 20.0, 0.6),
    ("warp", 20.0, 26.0, 2.0),
]

for name, *_ in CUTS:
    doc = json.load(open(os.path.join(FILMS_DIR, f"{name}.json")))
    f.components[name] = {"params": {}, "inputs": [], "output": doc["output"], "nodes": doc["nodes"]}

# Opener: three lines on black, on the beat.
black = f.add("solid", {"color": "#070707"}, name="black")
serif, italic = "InstrumentSerif-Regular.ttf", "InstrumentSerif-Italic.ttf"
lines = []
for i, (txt, fnt, t0, col) in enumerate([("Video,", serif, 0.5, "#f1eee8"), ("as a live", serif, 1.0, "#f1eee8"),
                                         ("document.", italic, 1.5, "#ffd7a8")]):
    y = 760 + i * 230
    tx = f.text(txt, fnt, 230, [80, y], color=col, tracking=-5, name="open")
    layer, win = line_reveal(f, tx, 80, y, 230, t0, t_out=3.45 + i * 0.06, dur=0.6)
    lines.append((layer, None, win))
chrome = []
for y in (150, 1770):
    chrome.append(f.add("shape", {"kind": "rect", "center": [W / 2, y], "size": keys((0.1, [0, 2], OUT), (1.2, [W - 144, 2])),
                                  "color": "#f1eee82e"}, name="hair"))
chrome.append(f.text("ultrafast", "Geist-VF.ttf", 30, [72, 128], weight=600, name="folio"))
chrome.append(f.text("N°00 — EVERY STYLE, ONE GRAPH", "GeistMono-VF.ttf", 24, [W - 72, 128], align="right",
                     color="#f1eee88f", tracking=2, name="folio"))
opener = f.stack(black, lines + chrome)

# The films, cut on the beat.
comp = opener
for name, t_in, t_out, film_t in CUTS:
    inst = f.add(name, None, None, {"offset": round(t_in - film_t, 4)}, name=f"{name}_cut")
    comp = f.over(inst, comp, timing={"in": t_in, "out": t_out}, name=f"at_{name}")

# A flash on the bigger cuts.
flash = f.add("solid", {"color": "#ffffff"}, name="flash")
for t in (4.0, 8.5, 14.5, 20.0):
    comp = f.over(flash, comp, timing={"in": t, "out": t + 0.12}, name="flash_at",
                  opacity=keys((t, 0.7, "linear"), (t + 0.12, 0.0)))

score = f.add("audio", {"src": "../../out/films/hero_score.wav", "gain": 1.0}, name="score")
f.output = comp
f.save(os.path.join(FILMS_DIR, "hero.json"))
print("ok", len(f.nodes), "nodes,", len(f.components), "components")

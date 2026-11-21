"""BLOCKS — the built-in blocks in sequence over one backdrop: a title, a
lower third, a pull quote, a stat, a chapter marker and an end card, each a
single node placed in time with a timing offset. 1080x1920, 18 s."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=20.5, motion_blur=(4, 0.5))
W, H = f.W, f.H
MARK = os.path.relpath(os.path.join(FILMS_DIR, "..", "assets", "svg", "ultrafast-mark.svg"), FILMS_DIR)

bg = f.add("gradient", {"kind": "radial", "center": [W * 0.3, 500], "radius": 1700,
                        "stops": "0:#2a1b3d, 0.55:#120d1e, 1:#07060c"}, name="bg")
silk = f.add("marble", {"filaments": 0.35}, name="silk")
silk = f.add("adjust", {"exposure": -2.2, "saturation": 0.6}, {"source": silk}, name="silk_dim")
base = f.over(silk, bg, opacity=0.5, name="backdrop")

blocks = [
    (0.3, {"kind": "title", "duration": 4.6, "kicker": "ULTRAFAST · BLOCKS", "title": "Built-in blocks, one house style",
           "subtitle": "A title, a lower third, a quote, a stat, a chapter and an end card. Each is one node."}),
    (5.4, {"kind": "lower_third", "name": "Ada Okafor", "role": "MOTION DIRECTOR"}),
    (5.2, {"kind": "chapter", "number": "01", "title": "On speed", "position": [108, 1180]}),
    (8.2, {"kind": "quote", "quote": "The fastest render is the one you never had to wait for.",
           "author": "A video engineer", "width": 860}),
    (13.7, {"kind": "stat", "value": 235, "suffix": " fps", "label": "EXPORT, 6-SAMPLE MOTION BLUR", "accent": "#2ee6c5"}),
    (17.4, {"kind": "end_card", "title": "ultrafast", "tagline": "the video engine for agents", "cta": "Make something",
            "logo": MARK}),
]
layers = []
for t0, params in blocks:
    params = dict(params)
    if params["kind"] == "chapter":
        params["duration"] = 2.6
    nid = f.add("block", params, timing={"offset": t0}, name=params["kind"])
    layers.append(nid)
comp = f.stack(base, layers)
f.output = finish(f, comp, grain=0.05)
f.save(os.path.join(FILMS_DIR, "blocks.json"))
print("ok", len(f.nodes), "nodes")

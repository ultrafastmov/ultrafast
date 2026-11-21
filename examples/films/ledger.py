"""LEDGER — an editorial data story on paper: a title with a serif italic
accent, a bar chart, a line chart and a donut that build themselves, and a
closing line under a marker sweep. Every chart is one `block` node with
`values` and `labels`. 1920x1080, 24 s."""

from ultrafastfilm import *

f = Film(1920, 1080, fps=30, duration=24.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
PAPER, INK, RED = "#f2ede3", "#16141a", "#e2412b"
STYLE = {"ink": INK, "accent": RED, "muted": "#16141ad0"}

paper = f.add("gradient", {"kind": "radial", "center": [W * 0.35, H * 0.3], "radius": 2100,
                           "stops": "0:#f7f3ea, 0.6:#efe9dd, 1:#e3dccd"}, name="paper")
# A faint printed grid, like a ledger page.
grid = f.add("shader", {"code": """
float4 shade(float2 pos, float px) {
  float2 g = abs(frac(pos / 48.0 + 0.5) - 0.5) * 48.0;
  float l = 1.0 - smoothstep(0.0, 1.2, min(g.x, g.y));
  return float4(0.09, 0.08, 0.1, 1.0) * l * 0.07;
}"""}, name="grid")
page = f.over(grid, paper, name="page")

shots = [
    (0.3, {"kind": "title", "duration": 3.6, "kicker": "THE STATE OF VIDEO · 2026",
           "title": "Video became *software*", "subtitle": "Four charts on how films get made when agents make them.",
           "position": [192, 330], "width": 1300}),
    (4.4, {"kind": "bar_chart", "duration": 4.4, "kicker": "01 · THROUGHPUT", "title": "Frames per second, 1080p export with motion blur",
           "values": [24, 31, 58, 265], "labels": "Browser capture|Headless DOM|Filter graph|ultrafast", "suffix": " fps",
           "height": 420}),
    (9.4, {"kind": "line_chart", "duration": 4.4, "kicker": "02 · ADOPTION", "title": "Films composed by agents each week, thousands",
           "values": [3, 5, 4, 9, 14, 13, 22, 31, 47], "labels": "Jan,Feb,Mar,Apr,May,Jun,Jul,Aug,Sep", "suffix": "k",
           "height": 420}),
    (14.4, {"kind": "donut", "duration": 4.2, "kicker": "03 · ONE FRAME", "title": "Where 3.8 ms of GPU time goes",
            "values": [46, 22, 18, 14], "labels": "Type & vectors|Fused looks|Motion blur|Encode",
            "colors": f"{RED},#16141a,#8a8478,#c9c1b1", "height": 520}),
]
layers = []
for t0, params in shots:
    params = {**STYLE, **params}
    layers.append(f.add("block", params, timing={"offset": t0}, name=params["kind"]))

# The closing line: a marker sweeps under the words that matter.
close = f.add("text", {"text": "Every frame is a *live document*\nyou can edit while it plays.",
                       "font": "ultrafast:display", "size": 104, "weight": 600, "axis_opsz": 144, "tracking": -0.5,
                       "line_height": 1.08, "color": INK, "position": [192, 420],
                       "em_style": "italic", "em_highlight": "#e2412b40",
                       "em_draw": keys((0.9, 0, "out_expo"), (2.2, 1)),
                       "glyph_offset": keys((0, [0, 40], {"bezier": [0.16, 1, 0.3, 1]}), (0.8, [0, 0])),
                       "glyph_opacity": keys((0, 0), (0.5, 1)), "stagger": 0.018},
               timing={"in": 19.3, "offset": 19.3}, name="close")
sig = f.add("text", {"text": "ultrafast — the video engine for agents", "font": "ultrafast:mono", "size": 26,
                     "tracking": 3, "color": "#16141ad0", "position": [196, 720],
                     "reveal": keys((20.6, 0), (21.6, 40))},
            timing={"in": 19.3}, name="signature")
layers += [close, sig]

# A progress rule across the top, and the page number of each shot.
rule = f.add("path", {"d": f"M 192 96 L {W - 192} 96", "stroke": 3, "cap": "round", "color": RED,
                      "trim_end": keys((0, 0), (24, 1))}, name="progress")
layers.append(rule)
for i, (t0, t1) in enumerate([(4.4, 9.2), (9.4, 14.2), (14.4, 18.8)]):
    n = f.add("text", {"text": f"{i + 1:02d} / 03", "font": "ultrafast:mono", "size": 24, "tracking": 3,
                       "color": "#16141ad0", "align": "right", "position": [W - 192, 140],
                       "reveal": keys((t0, 0), (t0 + 0.4, 8))}, timing={"in": t0, "out": t1}, name=f"page{i + 1}")
    layers.append(n)

comp = f.stack(page, layers)
# Paper wants grain more than vignette.
f.output = finish(f, comp, grain=0.06, vignette=False)
f.save(os.path.join(FILMS_DIR, "ledger.json"))
print("ok", len(f.nodes), "nodes")

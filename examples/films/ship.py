"""SHIP — a developer launch: the film's own source types itself into a
code window while a live preview beside it fills in line by line, then a
chart of export speed, the headline number and an end card, over slow
violet smoke. Uses `code`, chart `block`s, inline-styled type and a
`fluid` simulation. 1920x1080, 22 s."""

from ultrafastfilm import *

f = Film(1920, 1080, fps=30, duration=22.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
INK, ACCENT, VIOLET = "#f3efe8", "#ff5c7a", "#8a6bff"
OUT = {"bezier": [0.16, 1, 0.3, 1]}
STYLE = {"ink": INK, "accent": ACCENT}

# ------------------------------------------------------------ backdrop
night = f.add("gradient", {"kind": "radial", "center": [W * 0.5, H * 1.1], "radius": 1700,
                           "stops": "0:#1a1230, 0.55:#0b0a12, 1:#060509"}, name="night")
# Two soft embers drift along the bottom and feed rising smoke.
embers = None
for i, (x0, x1, col) in enumerate([(300, 820, "#7d5cff"), (1650, 1100, "#ff5c7a")]):
    e = f.add("shape", {"kind": "circle", "size": 150, "color": col,
                        "center": keys((0, [x0, 1130]), (22, [x1, 1130]))}, name=f"ember{i}")
    embers = e if embers is None else f.over(e, embers, name="embers")
smoke = f.add("fluid", {"emit": 2.2, "dissipation": 0.45, "velocity_dissipation": 0.2, "vorticity": 7,
                        "turbulence": 520, "turbulence_scale": 260, "buoyancy": 900, "stir": 0,
                        "resolution": 0.25, "dye_resolution": 0.4, "seed": 11},
              {"dye": embers}, name="smoke")
smoke = f.add("opacity", {"opacity": 0.55}, {"source": smoke}, name="smoke_dim")
base = f.blend(smoke, night, "screen", name="backdrop")

layers = []

# ------------------------------------------------------------ 01 title
layers.append(f.add("block", {**STYLE, "kind": "title", "duration": 3.0, "kicker": "ULTRAFAST · OPEN SOURCE",
                              "title": "Write a film the way you write *code*",
                              "subtitle": "A GPU engine where every frame is a document you can edit.",
                              "position": [180, 360], "width": 1400},
                    timing={"offset": 0.3}, name="title"))

# ------------------------------------------------------------ 02 code + live preview
T2 = 3.6
SRC = """{
  "nodes": {
    "sky": { "op": "gradient",
      "params": { "stops": "0:#2b1550, 1:#ff7a59" } },
    "sun": { "op": "shape",
      "params": { "kind": "circle", "size": 300,
        "scale": { "ease": "spring" } } },
    "hi": { "op": "text",
      "params": { "text": "Hello, *world*" } }
  }
}"""
SPEED = 72.0
lines = SRC.split("\n")
done_at = []  # when each line has been typed
n = 0
for l in lines:
    n += len(l)
    done_at.append(T2 + 0.4 + n / SPEED)
code_end = done_at[-1]
layers.append(f.add("code", {"code": SRC, "language": "json", "title": "film.json", "type_speed": SPEED,
                             "at": T2 + 0.4, "size": 25, "position": [150, 250],
                             "highlight": "6-7", "highlight_at": done_at[6]},
                    timing={"in": T2, "out": 10.6}, name="code"))

# The preview: what the document says, appearing as it is typed.
PX, PY, PW, PH = 1180, 250, 620, 520
pcx, pcy = PX + PW / 2, PY + PH / 2
sky = f.add("gradient", {"kind": "linear", "start": [0, PY], "end": [0, PY + PH],
                         "stops": "0:#2b1550, 1:#ff7a59"}, name="p_sky")
sky = f.add("opacity", {"opacity": keys((done_at[3], 0), (done_at[3] + 0.5, 1, "out_cubic"))},
            {"source": sky}, name="p_sky_in")
sun = f.add("shape", {"kind": "circle", "center": [pcx, pcy - 40], "color": "#ffd9a8",
                      "size": keys((done_at[6], [0, 0], "spring"), (done_at[6] + 1.1, [300, 300]))}, name="p_sun")
hello = f.add("text", {"text": "Hello, *world*", "font": "ultrafast:sans", "size": 72, "weight": 650,
                       "em_font": "ultrafast:serif", "em_style": "italic", "em_size": 84, "color": "#2b1550",
                       "align": "center", "valign": "middle", "position": [pcx, pcy + 185],
                       "glyph_offset": keys((done_at[8], [0, 30], OUT), (done_at[8] + 0.7, [0, 0])),
                       "glyph_opacity": keys((done_at[8], 0), (done_at[8] + 0.4, 1)), "stagger": 0.03},
              name="p_hello")
art = f.stack(sky, [sun, hello])
frame = f.add("shape", {"kind": "rect", "center": [pcx, pcy], "size": [PW, PH], "radius": 18,
                        "color": "#ffffff"}, name="p_frame")
art = f.add("mask", {}, {"source": art, "matte": frame}, name="p_masked")
empty = f.add("shape", {"kind": "rect", "center": [pcx, pcy], "size": [PW, PH], "radius": 18,
                        "color": "#15131c"}, name="p_empty")
edge = f.add("shape", {"kind": "rect", "center": [pcx, pcy], "size": [PW, PH], "radius": 18, "stroke": 1.5,
                       "color": "#ffffff26"}, name="p_edge")
preview = f.stack(empty, [art, edge])
label = f.add("text", {"text": "LIVE PREVIEW", "font": "ultrafast:mono", "size": 24, "tracking": 4, "color": "#f3efe8c0",
                       "valign": "baseline", "position": [PX + 26, PY - 20]}, name="p_label")
dot = f.add("shape", {"kind": "circle", "center": [PX + 8, PY - 28], "size": [12, 12], "color": ACCENT},
            name="p_dot")
dot = f.add("opacity", {"opacity": {"expr": "0.35 + 0.65 * (fract(t * 0.9) < 0.6)"}}, {"source": dot}, name="p_dot_blink")
preview = f.stack(preview, [label, dot])
preview = f.add("opacity", {"opacity": keys((T2 + 0.2, 0), (T2 + 0.7, 1, "out_cubic"), (10.1, 1), (10.5, 0))},
                {"source": preview}, name="preview_in")
layers.append((preview, None, {"in": T2, "out": 10.6}))
cap = f.add("text", {"text": "Edit the document. *The frame follows.*", "font": "ultrafast:sans", "size": 34,
                     "color": "#f3efe8d8", "em_color": INK, "em_weight": 600, "position": [150, 900],
                     "reveal": keys((code_end - 0.2, 0), (code_end + 0.9, 40))},
            timing={"in": T2, "out": 10.6}, name="caption")
layers.append(cap)

# ------------------------------------------------------------ 03 the chart
layers.append(f.add("block", {**STYLE, "kind": "bar_chart", "duration": 4.0, "kicker": "EXPORT SPEED",
                              "title": "Frames per second, 1080p with motion blur",
                              "values": [24, 31, 58, 570], "labels": "Browser capture|Headless DOM|Filter graph|ultrafast",
                              "suffix": " fps", "height": 430, "position": [180, 190], "width": 1560},
                    timing={"offset": 10.8}, name="chart"))

# ------------------------------------------------------------ 04 the number
layers.append(f.add("block", {**STYLE, "kind": "stat", "duration": 2.6, "value": 0, "label": "READBACKS PER FRAME",
                              "position": [180, 330]}, timing={"offset": 15.2}, name="stat"))
side = f.add("text", {"text": "Frames go from the GPU *straight into the encoder.* Nothing comes back to the CPU.",
                      "font": "ultrafast:sans", "size": 44, "line_height": 1.3, "color": "#f3efe8c8",
                      "em_color": INK, "em_weight": 600, "em_highlight": "#ff5c7a3a",
                      "em_draw": keys((0.9, 0, "out_expo"), (1.9, 1)),
                      "position": [860, 410], "width": 860,
                      "reveal": keys((0.2, 0), (1.0, 90))},
              timing={"in": 15.2, "out": 19.5, "offset": 15.2}, name="side")
layers.append(side)

# ------------------------------------------------------------ 05 end card
layers.append(f.add("block", {**STYLE, "kind": "end_card", "title": "ultrafast", "tagline": "the video engine for agents",
                              "cta": "Rust · Metal · Vulkan", "logo": os.path.relpath(
                                  os.path.join(FILMS_DIR, "..", "assets", "svg", "ultrafast-mark.svg"), FILMS_DIR)},
                    timing={"offset": 19.4}, name="end"))

comp = f.stack(base, layers)
comp = f.add("bloom", {"threshold": 0.8, "radius": 40, "intensity": 0.35, "wide": 0.3}, {"source": comp}, name="bloom")
f.output = finish(f, comp, grain=0.055)
f.save(os.path.join(FILMS_DIR, "ship.json"))
print("ok", len(f.nodes), "nodes")

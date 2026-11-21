"""ENGINE — a material study cut to a 120 BPM score, every image made in
ultrafast: liquid chrome, glass prisms and iridescent foil as raymarched scenes,
smoke as a fluid simulation, three sculptures for FORM, one type system over
all of it, a beat-locked montage, and the wordmark in 3D chrome reflecting
the foil. 1920x1080, 26 s, with sound (an original score, ElevenLabs Music).

The score lives outside the repo: $ULTRAFAST_SHOWCASE/engine/audio/score/take-1.mp3
(drop at 8 s, stops at 24 s)

    python3 engine.py && ultrafast export $ULTRAFAST_SHOWCASE/engine/engine.json $ULTRAFAST_SHOWCASE/engine/engine.mp4
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "engine")

f = Film(1920, 1080, fps=30, duration=26.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX, CY = W / 2, H / 2
BLACK, WHITE = "#050506", "#f4f3f0"
WIDE, SANS, MONO = "ultrafast:wide", "ultrafast:sans", "ultrafast:mono"
SNAP = {"bezier": [0.12, 1.0, 0.24, 1.0]}  # hard, decisive arrivals
B = 0.5  # one beat at 120 BPM
T0 = 0.011  # the first beat
beat = lambda k: T0 + k * B
DROP = beat(16)  # 8.0 s
END = beat(48)  # 24.0 s: the music stops


# ------------------------------------------------------------------ the materials, made in ultrafast
CHROME = """
// Liquid chrome: six blobs melting into one another as they drift.
float2 map(float3 p) {
  float t = time() * 0.45;
  float d = 1e9;
  for (int k = 0; k < 6; k++) {
    float fk = float(k);
    float3 c = float3(sin(t * 1.1 + fk * 1.7) * 1.5, sin(t * 0.8 + fk * 2.3) * 0.9, cos(t * 1.3 + fk * 1.1) * 1.1);
    d = smin(d, sdSphere(p - c, 0.62 + 0.18 * sin(t * 1.7 + fk)), 0.8);
  }
  return float2(d, 10.0);
}"""

PRISMS = """
// Glass prisms turning over black lacquer, splitting the light bars.
float sdTriPrism(float3 p, float2 h) { float3 q = abs(p); return max(q.z - h.y, max(q.x * 0.866025 + p.y * 0.5, -p.y) - h.x * 0.5); }
float2 map(float3 p) {
  float2 r = float2(p.y + 1.15, 6.0);
  for (int k = 0; k < 3; k++) {
    float fk = float(k);
    float3 q = rotY(p - float3((fk - 1.0) * 1.7, 0.0, sin(fk * 2.1) * 0.6), time() * (0.3 + 0.12 * fk) + fk * 1.3);
    r = opU(r, float2(sdTriPrism(q, float2(1.15, 0.55)) - 0.02, 9.0));
  }
  return r;
}"""

FOIL = """
// Holographic foil: a sheet in slow waves, its color a thin film that
// shifts with every tilt.
float h(float2 q, float t) {
  return 0.38 * sin(q.x * 0.9 + t * 1.1) * cos(q.y * 0.6 - t * 0.8) + 0.14 * sin(q.x * 1.7 - q.y * 1.3 + t * 1.5)
       + 0.05 * fbm3(float3(q * 0.4, t * 0.2), 2);
}
float2 map(float3 p) { return float2((p.y - h(p.xz, time() * 0.6)) * 0.55, 11.0); }
void material(float id, float3 p, float3 n, inout float3 alb, inout float metal, inout float rough) {
  float x = n.x * 0.8 + n.z * 0.55 + p.x * 0.05 + time() * 0.03;
  alb = 0.6 + 0.4 * cos(6.2831853 * (x + float3(0.0, 0.33, 0.67)));
  metal = 1.0; rough = 0.22;
}"""

SCULPTURES = {
    # A gold sphere cut to a gyroid lattice.
    "gyroid": """
float2 map(float3 p) {
  p = rotY(p, time() * 0.5);
  float3 q = p * 3.2;
  float g = abs(dot(sin(q), cos(q.yzx))) / 3.2 - 0.05;
  return float2(max(sdSphere(p, 1.55), g), 4.0);
}""",
    # A chrome column, twisted.
    "twist": """
float2 map(float3 p) {
  p = rotY(p, time() * 0.4);
  float3 q = rotY(p, p.y * 1.25);
  return float2(sdBox(q, float3(0.5, 1.65, 0.5), 0.06) * 0.7, 10.0);
}""",
    # Two glass rings, linked.
    "links": """
float2 map(float3 p) {
  p = rotY(p, time() * 0.45);
  float a = sdTorus(rotX(p - float3(-0.55, 0.0, 0.0), 1.5708), 1.0, 0.24);
  float b = sdTorus(p - float3(0.55, 0.0, 0.0), 1.0, 0.24);
  return float2(min(a, b), 9.0);
}""",
}


def material_plate(name, nid):
    if name == "chrome":
        return f.add("raymarch", {"scene": CHROME, "environment": "studio", "background": "#040405",
                                  "orbit": [15, 8, 6.5], "fov": 38, "light": [-3, 4, 4], "light_intensity": 30},
                     name=nid)
    if name == "glass":
        return f.add("raymarch", {"scene": PRISMS, "environment": "lightbox", "background": "#030304",
                                  "orbit": [12, 12, 7.2], "target": [0, -0.1, 0], "fov": 36, "dispersion": 2.5,
                                  "light": [-3, 5, 4], "light_intensity": 40}, name=nid)
    if name == "holo":
        # Smooth color with no edges: one ray a pixel at three-quarter size.
        return f.add("raymarch", {"scene": FOIL, "environment": "lightbox", "background": "#000000",
                                  "orbit": [0, 38, 5.0], "fov": 46, "light": [-2, 5, 3], "light_intensity": 25,
                                  "samples": 1, "resolution": 0.75}, name=nid)
    if name == "smoke":
        # A slow wisp of dye low in the frame, rising with buoyancy.
        src = f.add("shape", {"kind": "ellipse", "size": [240, 80], "color": "#d6e2ff", "softness": 40,
                              "center": {"oscillate": {"amplitude": [260, 30], "frequency": 0.13,
                                                       "base": [CX, H - 220], "phase": [0.0, 0.25]}}}, name=f"{nid}_wisp")
        smoke = f.add("fluid", {"emit": 3.5, "dissipation": 0.3, "velocity_dissipation": 0.15, "vorticity": 9,
                                "turbulence": 650, "turbulence_scale": 210, "buoyancy": 1500, "wind": [24, 0],
                                "resolution": 0.25, "dye_resolution": 0.5, "seed": 7}, {"dye": src}, name=nid)
        return f.add("bloom", {"threshold": 0.3, "radius": 50, "intensity": 0.7}, {"source": smoke},
                     name=f"{nid}_bloom")
    return f.add("raymarch", {"scene": SCULPTURES[name], "environment": "lightbox" if name != "gyroid" else "studio",
                              "background": "#050506", "orbit": [20, 10, 8.6], "fov": 34, "dispersion": 2.0,
                              "light": [-3, 4, 4], "light_intensity": 35}, name=nid)


def plate(name, t_in, t_out, rect=None, speed=0.8, start=0.0, scale=None, nid=None):
    """A material, playing from `start` at `speed`, full frame or fitted to
    cover `rect` (the caller crops it there)."""
    before = set(f.nodes)
    v = material_plate(name, nid or name)
    # Every node the material made lives (and keeps its own clock) only in
    # this window: an effect outside its window would pass its source on.
    for k in set(f.nodes) - before:
        if f.nodes[k]["op"] != "audio":
            f.nodes[k]["timing"] = {"in": t_in, "out": t_out, "offset": t_in - start / speed, "speed": speed}
    c = [CX, CY]
    if rect:
        c = [rect[0] + rect[2] / 2, rect[1] + rect[3] / 2]
        fit = max(rect[2] / W, rect[3] / H)
        # Drawn small, so render small: only the pixels the cell shows.
        if f.nodes[v]["op"] == "raymarch":
            full = f.nodes[v]["params"].get("resolution", 1.0)
            f.nodes[v]["params"]["resolution"] = round(min(full, fit * 1.15), 3)
        v = f.add("transform", {"anchor": [CX, CY], "position": c, "scale": fit}, {"source": v},
                  timing={"in": t_in, "out": t_out}, name=f"{nid or name}_fit")
    if scale is not None:
        v = f.add("transform", {"anchor": c, "position": c, "scale": scale}, {"source": v},
                  timing={"in": t_in, "out": t_out}, name=f"{nid or name}_push")
    return v


def window(src, rect, t_in, t_out, name):
    """A hard-edged window onto `src`: opens from its center line on a
    beat, shuts the same way."""
    x, y, w, h = rect
    r = keys((t_in, [x, y + h / 2, w, 0], SNAP), (t_in + 0.35, [x, y, w, h]),
             (t_out - 0.25, [x, y, w, h], IN), (t_out, [x, y + h / 2, w, 0]))
    return f.add("crop", {"rect": r, "softness": 0.0}, {"source": src}, timing={"in": t_in, "out": t_out}, name=name)


def label(text, x, y, t_in, t_out, size=18, color="#f4f3f0aa", align="left", name="label"):
    return f.add("text", {"text": text, "font": MONO, "size": size, "tracking": 4, "weight": 500, "color": color,
                          "align": align, "position": [x, y], "valign": "baseline",
                          "reveal": keys((t_in, 0.0), (t_in + 0.25, float(len(text)), "linear"))},
                 timing={"in": t_in, "out": t_out}, name=name)


def slam(word, t, t_out, size=330, weight=900, font=WIDE, y=None, stroke=None, name="word"):
    """A word arriving glyph by glyph on the beat: each letter drops in
    from above with a snap, then holds."""
    p = {"text": word, "font": font, "size": size, "weight": weight, "align": "center", "valign": "baseline",
         "position": [CX, y or CY + size * 0.36], "color": "#ffffff", "tracking": -size * 0.02,
         "glyph_offset": keys((t, [0, -size * 0.6], SNAP), (t + 0.28, [0, 0])),
         "glyph_opacity": keys((t, 0.0), (t + 0.06, 1.0)), "stagger": B / max(len(word), 1) * 0.9}
    if stroke:
        p["stroke"] = stroke
    return f.add("text", p, timing={"in": t, "out": t_out}, name=name)


# ------------------------------------------------------------------ sound
f.add("audio", {"src": os.path.join(D, "audio", "score", "take-1.mp3"), "gain": 0.9}, name="score")
ground = f.add("solid", {"color": BLACK}, name="ground")
L = []

# ------------------------------------------------------------------ chrome: the frame of the film
# A 12-column grid of hairlines and crop marks, drawn on in the intro and
# kept faintly through everything.
cols = 12
for i in range(cols + 1):
    x = 96 + i * (W - 192) / cols
    t = beat(i % 8) * 0.5 + 0.2
    L.append(f.add("path", {"d": f"M {x:.1f} 72 L {x:.1f} {H - 72}", "stroke": 1, "color": "#ffffff14",
                            "trim_end": keys((t, 0.0, OUT), (t + 1.2, 1.0))}, name="col"))
for (x, y) in [(96, 72), (W - 96, 72), (96, H - 72), (W - 96, H - 72)]:
    sx, sy = (1 if x < CX else -1), (1 if y < CY else -1)
    L.append(f.add("path", {"d": f"M {x} {y + 22 * sy} L {x} {y} L {x + 22 * sx} {y}", "stroke": 2,
                            "color": "#ffffffaa"}, name="corner"))
# Running frame counter and title block.
L.append(f.add("text", {"text": "F", "font": MONO, "size": 18, "tracking": 3, "color": "#f4f3f0aa", "align": "right",
                        "valign": "baseline", "position": [W - 96, 52], "prefix": "F ", "decimals": 0,
                        "number": keys((0.0, 0.0), (26.0, 780.0, "linear"))}, name="frame_no"))
L.append(label("ULTRAFAST / MATERIAL STUDY 01", 96, 52, 0.4, 26.0, name="title_block"))

# ------------------------------------------------------------------ 1 intro (0–8 s, the music ticking)
# A window onto the chrome grows in steps on the beats; words of the
# brief type out underneath.
# Before it, a point at the center ticks with the first four beats; the
# window opens out of it.
tick = [(0.0, 0.0)]
for k in range(4):
    tick += [(beat(k), 0.0), (beat(k) + 0.02, 1.0, OUT), (beat(k) + 0.42, 0.18)]
tick += [(beat(4) - 0.01, 1.0), (beat(4), 0.0)]
L.append(f.add("shape", {"kind": "rect", "center": [CX, CY], "size": [12, 12], "color": WHITE,
                         "opacity": keys(*tick),
                         "scale": keys(*[kk for k in range(1, 4) for kk in
                                         ((beat(k), 1.0 + 0.5 * (k - 1), SNAP), (beat(k) + 0.25, 1.0 + 0.5 * k))])},
               timing={"in": 0.0, "out": beat(4)}, name="tick"))
steps = [(beat(4), [CX - 120, CY - 70, 240, 140]), (beat(8), [CX - 300, CY - 170, 600, 340]),
         (beat(12), [CX - 520, CY - 290, 1040, 580])]
ks = [(beat(4) - 0.01, [CX, CY, 0, 0])]
for t, r in steps:
    ks.append((t, ks[-1][1], SNAP))
    ks.append((t + 0.3, r))
chrome_intro = plate("chrome", beat(4), DROP, speed=0.7, nid="chrome_intro")
L.append(f.add("crop", {"rect": keys(*ks), "softness": 0.0}, {"source": chrome_intro},
               timing={"in": beat(4), "out": DROP}, name="grow"))
for i, word in enumerate(["ONE GRAPH", "EVERY MATERIAL", "ON THE GPU"]):
    t = beat(4 + 4 * i)
    L.append(label(word, CX, H - 140, t, t + 2.0 - 0.02, size=22, color="#f4f3f0", align="center", name="brief"))

# ------------------------------------------------------------------ 2 the drop: one material per bar
flash = f.add("solid", {"color": "#ffffff"}, timing={"in": DROP, "out": DROP + 0.07}, name="flash")
SECTIONS = [
    ("chrome", "LIQUID", 0.9, {}),
    ("glass", "LIGHT", 0.8, {"weight": 200}),
    ("holo", "COLOR", 0.8, {"stroke": 3}),
]
for i, (clip, word, speed, style) in enumerate(SECTIONS):
    t, t_out = DROP + 2.0 * i, DROP + 2.0 * (i + 1)
    bg = plate(clip, t, t_out, speed=speed, scale=keys((t, 1.12, SNAP), (t_out, 1.0)), nid=f"plate_{clip}")
    w = slam(word, t + 0.02, t_out, weight=style.get("weight", 900), stroke=style.get("stroke"), name=f"w_{word}")
    # White type in difference: the material inverts inside the letters.
    L.append(f.blend(w, bg, "difference", name=f"inv_{word}"))
    L.append(label(f"0{i + 1} / 05", 96, H - 48, t, t_out, name="count"))
    L.append(label(f"MATERIAL — {word}", W - 96, H - 48, t, t_out, align="right", name="mat"))

# 14–16 s: FORM. The stills in a triptych, each on its beat.
t, t_out = DROP + 6.0, DROP + 8.0
TW, TH, G = 520, 700, 32
tx0 = CX - (3 * TW + 2 * G) / 2
for i, name in enumerate(["gyroid", "twist", "links"]):
    x = tx0 + i * (TW + G)
    rect = [x, 190, TW, TH]
    tb = t + B * i
    # Each sculpture exists from its own beat (before it, the panel's crop
    # would be off and pass the whole render through).
    before = set(f.nodes)
    img = material_plate(name, f"form_{name}")
    for k in set(f.nodes) - before:
        f.nodes[k]["timing"] = {"in": tb, "out": t_out, "offset": t}
    # Rendered full frame, the sculpture centered: slide it to its panel.
    img = f.add("transform", {"anchor": [CX, CY], "position": [x + TW / 2, 540]}, {"source": img},
                timing={"in": tb, "out": t_out}, name=f"place_{name}")
    img = f.add("transform", {"anchor": [x + TW / 2, 540], "position": [x + TW / 2, 540],
                              "scale": keys((tb, 1.25, SNAP), (t_out, 1.05))}, {"source": img},
                timing={"in": tb, "out": t_out}, name=f"push_{name}")
    L.append(window(img, rect, tb, t_out, f"win_{name}"))
    L.append(label(f"FIG. {i + 1}", x, 925, tb + 0.1, t_out, name="fig"))
L.append(slam("FORM", t + 0.02, t_out, size=150, y=150, name="w_FORM"))
L.append(label("04 / 05", 96, H - 48, t, t_out, name="count"))

# 16–18 s: AIR. Smoke, the word soft and wide.
t, t_out = DROP + 8.0, DROP + 10.0
# The smoke has been rising for a few seconds when the shot opens.
L.append(plate("smoke", t, t_out, speed=0.9, start=4.0, scale=keys((t, 1.0), (t_out, 1.1)), nid="plate_smoke"))
air = slam("AIR", t + 0.02, t_out, size=420, weight=300, name="w_AIR")
L.append(f.add("glow", {"radius": 40, "intensity": 0.6}, {"source": air}, name="air_glow"))
L.append(label("05 / 05", 96, H - 48, t, t_out, name="count"))

# 18–20 s: the montage. Eight windows, cutting on every beat.
t, t_out = DROP + 10.0, DROP + 12.0
order = ["chrome", "holo", "glass", "smoke", "holo", "chrome", "smoke", "glass"]
MW, MH, MG = 400, 300, 24
mx0, my0 = CX - (4 * MW + 3 * MG) / 2, CY - (2 * MH + MG) / 2
for k in range(8):
    col, row = k % 4, k // 4
    rect = [mx0 + col * (MW + MG), my0 + row * (MH + MG), MW, MH]
    tb = t + B * (k % 4) * 0.5
    name = order[(k + int((t - DROP) / B)) % 8]
    v = plate(name, tb, t_out, rect=rect, speed=1.0, start=k * 0.4 + (4.0 if name == "smoke" else 0.0), nid="cell")
    L.append(window(v, rect, tb, t_out, "cellwin"))

# ------------------------------------------------------------------ 3 the wordmark in chrome (20–24 s)
t, t_out = DROP + 12.0, END
# The word is drawn in ultrafast, softened, and read by a raymarch scene as a
# distance field, extruded into a solid with rounded edges, and turned:
# chrome reflecting a sky over a hot horizon.
WORD3D = """
float2 map(float3 p) {
  float3 q = rotY(rotX(p, radians(knob(1))), radians(knob(0)));
  float d2 = max(layerDistance(0, q.xy, 200.0, 9.0), sdBox2(q.xy, float2(3.2, 1.3), 0.0));
  return float2(extrude(d2, q.z, 0.5, 0.035) * 0.7, 10.0);
}"""
word = f.add("text", {"text": "ultrafast", "font": WIDE, "weight": 800, "size": 360, "align": "center", "valign": "middle",
                      "position": [CX, CY], "color": "#ffffff", "tracking": -8}, name="word2d")
word = f.add("blur", {"radius": 9}, {"source": word}, name="word_soft")
scene = f.add("raymarch", {"scene": WORD3D, "environment": "horizon", "background": BLACK,
                           "orbit": [0, -2, 11.5], "fov": 30, "light": [-3, 4, 6], "light_intensity": 30,
                           "k0": keys((t, -42.0, SNAP), (t + 1.4, 6.0), (t_out, -6.0)),
                           "k1": keys((t, 6.0, SNAP), (t + 1.4, 0.0))},
              {"in0": word}, timing={"in": t, "out": 26.0}, name="scene3d")
scene = f.add("bloom", {"threshold": 0.85, "radius": 36, "intensity": 0.5}, {"source": scene}, name="bloom3d")
# Each downbeat a small kick of scale.
kicks = [(t, 0.92, SNAP), (t + 0.5, 1.0)]
for k in range(4):
    tb = t + 1.0 + k * 1.0
    kicks += [(tb, 1.0, SNAP), (tb + 0.04, 1.025, OUT), (tb + 0.45, 1.0)]
scene = f.add("transform", {"anchor": [CX, CY], "position": [CX, CY], "scale": keys(*kicks)}, {"source": scene},
              timing={"in": t, "out": 26.0}, name="kick")
L.append(scene)
L.append(f.add("text", {"text": "Every frame is a live document.", "font": SANS, "size": 34, "weight": 500,
                        "color": WHITE, "align": "center", "valign": "baseline", "position": [CX, 860],
                        "glyph_opacity": keys((t + 1.0, 0.0), (t + 1.3, 1.0)), "stagger": 0.012,
                        "glyph_offset": keys((t + 1.0, [0, 18], OUT), (t + 1.6, [0, 0]))},
               timing={"in": t + 1.0, "out": 26.0}, name="tagline"))
# In the silence after the music stops: hold, then out.
L.append(f.add("solid", {"color": BLACK}, timing={"in": 24.6}, name="black_in"))
fade = f.add("opacity", {"opacity": keys((24.6, 0.0), (25.6, 1.0))}, {"source": L.pop()}, timing={"in": 24.6},
             name="to_black")
L.append(fade)
L.insert(len(L) - 1, flash)

comp = f.stack(ground, L)
comp = f.add("aberration", {"amount": 0.6}, {"source": comp}, name="aberration")
comp = f.add("vignette", {"inner": "#00000000", "outer": "#00000080"}, {"source": comp}, name="vignette")
comp = f.add("grain", {"amount": 0.04}, {"source": comp}, name="grain")
f.output = comp
for n in f.nodes.values():
    if n["op"] == "text":
        n["params"].setdefault("valign", "baseline")
print(f.save(os.path.join(D, "engine.json")))

"""AURORA — a product launch for a speaker that does not exist, every image
made in ultrafast: the product is a raymarched scene (porcelain, woven fabric,
a gold ring, a woofer inside), shot by shot — a reveal by a sweeping light,
a fabric macro, the ring from above, an exploded view with callouts pinned
to the 3D parts, three colorways — with copy set beside it. 1920x1080,
24 s, with an original score (ElevenLabs Music).

The score lives outside the repo: $ULTRAFAST_SHOWCASE/aurora/audio/score/take-1.mp3

    python3 aurora.py && ultrafast export $ULTRAFAST_SHOWCASE/aurora/aurora.json $ULTRAFAST_SHOWCASE/aurora/aurora.mp4
"""

import math
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "aurora")

f = Film(1920, 1080, fps=30, duration=24.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX, CY = W / 2, H / 2
INK, PAPER = "#1d1d1f", "#f5f5f7"
DISPLAY, SANS, MONO = "ultrafast:display", "ultrafast:sans", "ultrafast:mono"
EXPO = {"bezier": [0.16, 1.0, 0.3, 1.0]}
SINE = "in_out_sine"

# The speaker. knob(0): turn (degrees); knob(1): explode (0..1); knob(2):
# colorway (0 porcelain, 1 graphite, 2 sage); knob(3): how much light the
# floor and wall take (0 black stage, 1 studio sweep).
SPEAKER = """
float3 body_tint(float k) {
  return k < 0.5 ? float3(0.86, 0.85, 0.82) : (k < 1.5 ? float3(0.07, 0.07, 0.075) : float3(0.55, 0.62, 0.52));
}
// The fabric's weave as relief: threads crossing over and under.
float weave(float3 q) {
  float a = atan2(q.z, q.x) * 110.0, v = q.y * 170.0;
  float over = step(0.0, sin(a * 0.5) * sin(v * 0.5));
  float warp = 0.5 + 0.5 * cos(v), weft = 0.5 + 0.5 * cos(a);
  return lerp(warp, weft, over);
}
float2 speaker(float3 p, float e) {
  // Body and cap melt into one form when closed; apart, each is its own.
  float body = sdCylinder(p - float3(0.0, -0.25, 0.0), 0.9, 0.75, 0.22);
  float cap = sdCylinder(p - float3(0.0, 0.72 + 1.1 * e, 0.0), 0.9, 0.18, 0.12);
  float band = abs(p.y + 0.17) < 0.47 ? 1.0 : 0.0;
  body -= band * weave(p) * 0.004;
  float2 r = float2(smin(body, cap, 0.3 * (1.0 - e) + 0.001), band > 0.5 && p.y < 0.3 ? 20.0 : 22.0);
  r = opU(r, float2(sdTorus(p - float3(0.0, 0.9 + 1.6 * e, 0.0), 0.62, 0.035), 4.0));
  r = opU(r, float2(sdCylinder(p - float3(0.0, 0.895 + 1.6 * e, 0.0), 0.56, 0.012, 0.006), 6.0));
  // The woofer, seen only when the cap lifts: a surround, a cone, a cap.
  float3 w = p - float3(0.0, 0.6 + 0.55 * e, 0.0);
  r = opU(r, float2(sdTorus(w, 0.64, 0.055), 8.0));
  r = opU(r, float2(max(sdCylinder(w, 0.62, 0.05, 0.01), w.y - 0.02 + length(w.xz) * 0.12), 23.0));
  r = opU(r, float2(length(w * float3(1.0, 1.8, 1.0)) - 0.2, 3.0));
  r = opU(r, float2(sdCylinder(p - float3(0.0, -1.03, 0.0), 0.7, 0.03, 0.01), 8.0));
  return r;
}
float2 map(float3 p) {
  float2 r = float2(p.y + 1.06, 21.0);
  r = opU(r, float2(p.z + 7.0, 21.0));
  return opU(r, speaker(rotY(p, radians(knob(0))), knob(1)));
}
void material(float id, float3 p, float3 n, inout float3 albedo, inout float metallic, inout float roughness) {
  presetMaterial(id, albedo, metallic, roughness);
  float3 q = rotY(p, radians(knob(0)));
  if (id > 19.5 && id < 20.5) {
    // Woven acoustic fabric: a fine basket weave with a little fuzz.
    float fuzz = snoise(float3(q * 90.0)) * 0.5 + 0.5;
    albedo = body_tint(knob(2)) * float3(0.13, 0.135, 0.145) * (0.75 + 0.35 * weave(q)) * (0.9 + 0.1 * fuzz) + 0.012;
    metallic = 0.0; roughness = 0.92;
  } else if (id > 20.5 && id < 21.5) {
    albedo = float3(0.80, 0.79, 0.77) * knob(3); metallic = 0.0; roughness = 0.65;
  } else if (id > 21.5 && id < 22.5) {
    albedo = body_tint(knob(2)); metallic = 0.0; roughness = knob(2) > 0.5 && knob(2) < 1.5 ? 0.45 : 0.22;
  } else if (id > 22.5) {
    albedo = float3(0.05, 0.05, 0.055); metallic = 0.0; roughness = 0.55;
  }
}"""

COLORWAYS = SPEAKER.replace("""float2 map(float3 p) {
  float2 r = float2(p.y + 1.06, 21.0);
  r = opU(r, float2(p.z + 7.0, 21.0));
  return opU(r, speaker(rotY(p, radians(knob(0))), knob(1)));
}""", """float2 map(float3 p) {
  float2 r = float2(p.y + 1.06, 21.0);
  r = opU(r, float2(p.z + 7.0, 21.0));
  for (int k = 0; k < 3; k++) {
    float2 s = speaker(rotY(p - float3((float(k) - 1.0) * 2.5, 0.0, 0.0), radians(knob(0) + float(k) * 25.0)), 0.0);
    r = opU(r, s);
  }
  return r;
}""").replace("float3 q = rotY(p, radians(knob(0)));",
              "float slot = clamp(floor(p.x / 2.5 + 0.5), -1.0, 1.0);\n"
              "  float3 q = rotY(p - float3(slot * 2.5, 0.0, 0.0), radians(knob(0) + (slot + 1.0) * 25.0));").replace(
    "body_tint(knob(2))", "body_tint(slot + 1.0)").replace(
    "roughness = knob(2) > 0.5 && knob(2) < 1.5 ? 0.45 : 0.22;", "roughness = slot > -0.5 && slot < 0.5 ? 0.45 : 0.22;")


def shot(t0, t1, params, name, scene=SPEAKER):
    p = {"scene": scene, "environment": "studio", "background": "environment", "background_level": 0.0,
         "fov": 30, "light": [-3, 5, 4], "light_intensity": 45, "samples": 2, "k3": 1.0}
    p.update(params)
    return f.add("raymarch", p, timing={"in": t0, "out": t1}, name=name)


def project(p, orbit, target, fov):
    """Where a world point lands in the frame, for the camera the raymarch
    op builds from `orbit`, `target` and `fov` (callouts pin to it)."""
    az, el, dist = math.radians(orbit[0]), math.radians(orbit[1]), orbit[2]
    eye = [target[0] + dist * math.sin(az) * math.cos(el), target[1] + dist * math.sin(el),
           target[2] + dist * math.cos(az) * math.cos(el)]
    sub = lambda a, b: [a[i] - b[i] for i in range(3)]
    dot = lambda a, b: sum(a[i] * b[i] for i in range(3))
    cross = lambda a, b: [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    norm = lambda a: [x / math.sqrt(dot(a, a)) for x in a]
    fw = norm(sub(target, eye))
    rt = norm(cross(fw, [0, 1, 0]))
    up = cross(rt, fw)
    v = sub(p, eye)
    z = dot(v, fw)
    focal = 1 / math.tan(math.radians(fov) / 2)
    return [W / 2 + dot(v, rt) / z * focal * H / 2, H / 2 - dot(v, up) / z * focal * H / 2]


def copy(text, x, y, t0, t1, size=56, color=INK, font=SANS, weight=500, align="left", nid="copy"):
    fade = keys((t0, 0.0), (t0 + 0.5, 1.0), (t1 - 0.4, 1.0), (t1, 0.0))
    return f.add("text", {"text": text, "font": font, "size": size, "weight": weight, "color": color, "align": align,
                          "valign": "baseline", "position": [x, y], "tracking": -size * 0.01, "opacity": fade,
                          "glyph_opacity": keys((t0, 0.0), (t0 + 0.6, 1.0)), "stagger": 0.02,
                          "glyph_offset": keys((t0, [0, size * 0.35], EXPO), (t0 + 0.9, [0, 0]))},
                 timing={"in": t0, "out": t1}, name=nid)


f.add("audio", {"src": os.path.join(D, "audio", "score", "take-1.mp3"), "gain": 1.0}, name="score")
WHITE = f.add("solid", {"color": PAPER}, name="white")

# ------------------------------------------------------------------ 1 reveal (0–4.6 s)
# Out of black: a light swings round from behind and finds the shape.
S1 = shot(0.0, 4.9, {"background": "#000000", "k3": 0.06, "k0": keys((0.0, -30.0), (4.9, 10.0, "linear")),
                      "orbit": keys((0.0, [-12.0, 6.0, 7.4], SINE), (4.9, [-4.0, 8.0, 6.4])), "target": [0, -0.05, 0],
                      "light": keys((0.0, [-1.5, 2.0, -5.0], SINE), (4.0, [-3.5, 4.0, 4.0])),
                      "exposure": keys((0.0, 0.0), (1.6, 1.0, SINE))}, "reveal")
S1 = f.stack(S1, [copy("Aurora", CX, 960, 2.6, 4.7, size=88, color=PAPER, font=DISPLAY, weight=400, align="center",
                       nid="t_name")])

# ------------------------------------------------------------------ 2 the fabric, close (4.3–8.8 s)
S2 = shot(4.3, 8.8, {"orbit": keys((4.3, [-28.0, 4.0, 2.0], SINE), (8.8, [8.0, 10.0, 1.7])), "fov": 28,
                      "target": [0.0, -0.2, 0.0], "light": [-3.0, 0.6, 2.4], "light_intensity": 12,
                      "k0": keys((4.3, 0.0), (8.8, 12.0, "linear"))}, "fabric")
S2 = f.stack(S2, [copy("Woven acoustic fabric.", 120, 930, 5.0, 8.6, size=60, color=PAPER, nid="t_fabric"),
                  copy("Sound passes through. Dust doesn't.", 124, 990, 5.5, 8.6, size=30, color="#c7c7cc",
                       weight=400, nid="t_fabric2")])

# ------------------------------------------------------------------ 3 the ring, from above (8.5–12.3 s)
S3 = shot(8.5, 12.3, {"orbit": keys((8.5, [20.0, 62.0, 3.6], SINE), (12.3, [-20.0, 55.0, 3.0])), "fov": 32,
                       "target": [0.0, 0.75, 0.0], "light": keys((8.5, [-4.0, 4.0, -1.0], SINE), (12.3, [3.0, 4.0, 2.0])),
                       "light_intensity": 35, "k0": 0.0}, "ring")
S3 = f.stack(S3, [copy("Touch the ring.", 120, 160, 9.1, 12.1, size=64, nid="t_ring"),
                  copy("Turn it to set the volume.", 124, 222, 9.6, 12.1, size=30, color="#48484a", weight=400,
                       nid="t_ring2")])

# ------------------------------------------------------------------ 4 exploded (12.0–16.8 s)
# The parts lift apart; callouts draw out to each, pinned to where the
# camera sees it (projected with the op's own camera).
ORB, TGT, FOV = [-24.0, 12.0, 8.6], [0.0, 0.55, 0.0], 30.0
ex0, ex1 = 12.4, 13.7
S4 = shot(12.0, 16.8, {"orbit": ORB, "target": TGT, "fov": FOV, "k0": 20.0,
                        "k1": keys((ex0, 0.0, EXPO), (ex1, 1.0))}, "exploded")
yaw = math.radians(20.0)
turn = lambda x, y, z: [math.cos(yaw) * x + math.sin(yaw) * z, y, -math.sin(yaw) * x + math.cos(yaw) * z]
parts = [("Gold touch ring", turn(0.62, 0.9 + 1.6, 0.0)), ("Porcelain cap", turn(0.88, 0.72 + 1.1, 0.0)),
         ("4-inch woofer", turn(0.62, 0.6 + 0.55, 0.0)), ("Acoustic fabric", turn(0.9, -0.2, 0.0))]
call = []
LX = 1360
for k, (label, pt) in enumerate(parts):
    x, y = project(pt, ORB, TGT, FOV)
    t = ex1 + 0.15 + k * 0.2
    d = f"M {x:.1f} {y:.1f} L {x + 60:.1f} {y:.1f} L {LX - 16} {y:.1f}"
    call.append(f.add("path", {"d": d, "stroke": 1.6, "color": INK, "opacity": 0.8,
                               "trim_end": keys((t, 0.0, EXPO), (t + 0.6, 1.0))}, timing={"in": t, "out": 16.8},
                      name="leader"))
    call.append(f.add("shape", {"kind": "circle", "center": [x, y], "size": [9, 9], "color": INK,
                                "scale": keys((t, 0.0, EXPO), (t + 0.3, 1.0))}, timing={"in": t, "out": 16.8},
                      name="pin"))
    call.append(copy(label, LX, y + 9, t + 0.25, 16.7, size=30, weight=500, nid="t_part"))
S4 = f.stack(S4, call)

# ------------------------------------------------------------------ 5 colorways (16.4–20.8 s)
S5 = shot(16.4, 20.8, {"scene": COLORWAYS, "orbit": keys((16.4, [-6.0, 10.0, 11.5], SINE), (20.8, [6.0, 12.0, 10.6])),
                        "fov": 30, "target": [0.0, -0.1, 0.0], "light": [-4, 6, 5], "light_intensity": 60,
                        "k0": keys((16.4, -20.0), (20.8, 20.0, "linear"))}, "colorways")
labels = []
for k, (name, x) in enumerate([("Porcelain", -2.5), ("Graphite", 0.0), ("Sage", 2.5)]):
    px, _ = project([x, -1.05, 0.9], [0.0, 11.0, 11.05], [0.0, -0.1, 0.0], 30.0)
    labels.append(copy(name, px, 930, 17.0 + k * 0.25, 20.6, size=30, weight=500, align="center", nid="t_color"))
S5 = f.stack(S5, labels + [copy("Three finishes.", CX, 170, 16.8, 20.6, size=64, align="center", nid="t_colors")])

# ------------------------------------------------------------------ 6 end card (20.5–24 s)
S6 = shot(20.5, 24.0, {"orbit": keys((20.5, [0.0, 8.0, 9.5], SINE), (24.0, [0.0, 8.0, 9.0])), "fov": 30,
                        "target": [-2.6, -0.2, 0.0], "k0": keys((20.5, -10.0), (24.0, 10.0, "linear"))}, "end")
S6 = f.stack(S6, [copy("Aurora", 220, 520, 20.9, 24.0, size=150, font=DISPLAY, weight=400, nid="t_end"),
                  copy("Room-filling sound, in porcelain.", 226, 590, 21.3, 24.0, size=38, color="#48484a",
                       weight=400, nid="t_end2"),
                  copy("From $349  ·  October 30", 226, 690, 21.8, 24.0, size=26, color="#6e6e73", font=MONO,
                       weight=400, nid="t_end3")])

# ------------------------------------------------------------------ cut on the music
T = f.add("transition", {"kind": "crossfade", "at": 4.3, "duration": 0.6}, {"a": S1, "b": S2}, name="t1")
T = f.add("transition", {"kind": "crossfade", "at": 8.5, "duration": 0.3}, {"a": T, "b": S3}, name="t2")
T = f.add("transition", {"kind": "push", "at": 12.0, "duration": 0.6, "direction": 180}, {"a": T, "b": S4}, name="t3")
T = f.add("transition", {"kind": "crossfade", "at": 16.4, "duration": 0.4}, {"a": T, "b": S5}, name="t4")
T = f.add("transition", {"kind": "crossfade", "at": 20.5, "duration": 0.6}, {"a": T, "b": S6}, name="t5")
T = f.add("transition", {"kind": "crossfade", "at": 22.8, "duration": 1.2}, {"a": T, "b": WHITE}, name="t_out")
f.output = f.add("film_look", {"halation": 0.35, "grit": 0.12, "lift": 0.0, "saturation": 0.95, "grain": 0.035,
                               "vignette": "#00000038", "aberration": 0.25}, {"source": T}, name="finish")
print(f.save(os.path.join(D, "aurora.json")))

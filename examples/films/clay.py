"""CLAY — 3D pastel type in a soft studio, built from the same graph as
everything else: each letter is an extruded mesh, the studio light is a
shader, chrome balls reflect it. 1080x1920, 8 s."""

import json
import subprocess

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(8, 0.5))
W, H = f.W, f.H
ULTRAFAST = os.path.join(FILMS_DIR, "..", "..", "target", "release", "ultrafast")
FONT = "Unbounded-VF.ttf"


def measure(text, size=100, weight=800):
    params = json.dumps({"text": text, "font": font(FONT), "size": size, "weight": weight})
    out = subprocess.run([ULTRAFAST, "measure", params, "--base", FILMS_DIR], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


STUDIO = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float y = uv.y;
  float3 sky = float3(1.0, 0.93, 0.88), horizon = float3(0.98, 0.74, 0.64), floor = float3(0.78, 0.46, 0.40);
  float3 c = y < 0.5 ? lerp(sky, horizon, smoothstep(0.0, 0.5, y)) : lerp(horizon, floor, smoothstep(0.5, 0.75, y));
  // Two tall windows and an overhead box, for the chrome to catch.
  float w1 = smoothstep(0.01, 0.0, abs(uv.x - 0.30) - 0.05) * smoothstep(0.01, 0.0, abs(y - 0.32) - 0.12);
  float w2 = smoothstep(0.01, 0.0, abs(uv.x - 0.82) - 0.035) * smoothstep(0.01, 0.0, abs(y - 0.35) - 0.10);
  float top = smoothstep(0.02, 0.0, abs(y - 0.06) - 0.05);
  c += (w1 + w2) * float3(2.6, 2.5, 2.4) + top * float3(1.4);
  return float4(srgb_to_linear(saturate(c)) * (1.0 + (w1 + w2) * 2.5), 1.0);
}"""

studio = f.add("shader", {"code": STUDIO}, name="studio")

PASTELS = ["#ff8fab", "#7bdff2", "#b8f2a3", "#ffd166", "#a594f9", "#ff9e7a", "#8fd3c9"]
SCALE = 0.011  # world units per px of a 100 px measurement

meshes, colors_i = {}, 0
rows = [("make", 2.35, 0.3), ("it", 1.05, 1.0), ("move", -0.25, 1.5)]
letter_ids = []
for word, y, t0 in rows:
    m = measure(word)
    total = m["advance"]
    for k, g in enumerate(m["glyphs"]):
        if not g["ink"]:
            continue
        cx = (g["x"] + g["advance"] / 2 - total / 2) * SCALE
        t = t0 + k * 0.09
        # Drop, land, one small bounce, settle.
        drop = keys((t, [cx, y + 7, 0], "in_quad"), (t + 0.42, [cx, y, 0], "out_quad"), (t + 0.58, [cx, y + 0.28, 0], "in_quad"),
                    (t + 0.74, [cx, y, 0]))
        spin_t = 4.2 + len(letter_ids) * 0.07
        rot = keys((t, [0, 0, 18 * (1 if k % 2 else -1)], OUT), (t + 0.7, [0, 0, 0]), (spin_t, [0, 0, 0], INOUT),
                   (spin_t + 1.1, [0, 360, 0]))
        sq = keys((t + 0.40, [1, 1, 1], OUT), (t + 0.46, [1.12, 0.84, 1.12], OUT), (t + 0.66, [1, 1, 1]))
        lid = f.add("mesh", {"primitive": "text", "text": g["text"], "font": font(FONT), "weight": 800, "size": 1.1,
                             "depth": 0.42, "color": PASTELS[colors_i % len(PASTELS)], "roughness": 0.62, "metallic": 0.0,
                             "position": drop, "rotation": rot, "scale": sq}, name="letter")
        colors_i += 1
        letter_ids.append(lid)

floor = f.add("mesh", {"primitive": "plane", "rotation": [-90, 0, 0], "position": [0, -0.85, 0], "scale": 80,
                       "color": "#f0a990", "roughness": 0.95}, name="floor")
ball1 = f.add("mesh", {"primitive": "sphere", "segments": 64, "scale": 0.75, "color": "#f2f2f5", "metallic": 1.0, "roughness": 0.06,
                       "position": keys((0.0, [-5.5, -0.48, 2.6], OUT), (2.6, [-1.35, -0.48, 2.6]), (8.0, [-1.15, -0.48, 2.7]))}, name="ball")
ball2 = f.add("mesh", {"primitive": "sphere", "segments": 64, "scale": 0.45, "color": "#ffd6c8", "metallic": 1.0, "roughness": 0.16,
                       "position": keys((0.6, [6.0, -0.67, 3.4], OUT), (3.2, [1.45, -0.67, 3.3]), (8.0, [1.3, -0.67, 3.4]))}, name="ball")
torus = f.add("mesh", {"primitive": "torus", "segments": 64, "scale": 0.7, "color": "#a594f9", "roughness": 0.5,
                       "position": keys((0, [1.25, 4.35, -0.8]), (8, [1.05, 4.6, -0.8])), "rotation": keys((0, [70, 0, 0]), (8, [70, 140, 30]))}, name="torus")
key = f.add("light", {"direction": [-0.55, 1.0, 0.75], "intensity": 2.2, "color": "#fff1e4"}, name="key")
cam = f.add("camera", {"target": [0, 1.15, 0], "fov": 34,
                       "orbit": keys((0.0, [-10, 14, 15.5], INOUT), (4.0, [8, 18, 14.5], INOUT), (8.0, [-4, 11, 14.0]))}, name="cam")
inputs = {"camera": cam, "floor": floor, "key": key, "ball1": ball1, "ball2": ball2, "torus": torus, "environment": studio}
for i, lid in enumerate(letter_ids):
    inputs[f"l{i}"] = lid
scene = f.add("render3d", {"background": "#f5c0ab", "environment_intensity": 0.5, "shadow_strength": 0.6, "fog": [17, 42]}, inputs, name="scene")

# Overlay: quiet 2D type, same graph.
mono = "GeistMono-VF.ttf"
cap = f.text("3D, TYPE, LIGHT — ONE GRAPH", mono, 28, [W / 2, 1790], align="center", color="#4e1d13", weight=600, tracking=3, name="cap")
cap = f.add("opacity", {"opacity": keys((2.2, 0.0, SOFT), (3.0, 0.85))}, {"source": cap}, name="cap_in")
top = f.text("ultrafast", "Geist-VF.ttf", 34, [72, 140], color="#7a3b2e", weight=650, name="folio")
num = f.text("CLAY — N°04", mono, 24, [W - 72, 140], align="right", color="#5a2418d8", tracking=2, name="folio")
comp = f.stack(scene, [cap, top, num])
f.output = finish(f, comp, grain=0.05, vignette=False)
f.save(os.path.join(FILMS_DIR, "clay.json"))
print("ok", len(f.nodes), "nodes,", len(letter_ids), "letters")

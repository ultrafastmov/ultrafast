"""PRISM — refractive glass type: the word is solid glass that bends,
splits and frosts what moves behind it, lit by a studio that reflects in
its bevels. 1080x1920, 8 s."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(4, 0.5))
W, H = f.W, f.H

BANDS = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float x = uv.x * 5.0 + uv.y * 1.4 + sin(uv.y * 4.0 + time() * 0.6) * 0.25 - time() * 0.35;
  float k = floor(x);
  float f = frac(x);
  // Wide bands of saturated color with thin bright gaps between them.
  float3 pal[4] = { float3(1.0, 0.36, 0.28), float3(1.0, 0.78, 0.25), float3(0.25, 0.62, 1.0), float3(0.75, 0.3, 1.0) };
  float3 c = srgb_to_linear(pal[int(fmod(fmod(k, 4.0) + 4.0, 4.0))]);
  c *= 0.35 + 0.65 * smoothstep(0.0, 0.5, f) * smoothstep(1.0, 0.6, f);
  c += smoothstep(0.07, 0.0, abs(f - 0.03)) * 1.6;
  return float4(c, 1.0);
}"""
STUDIO = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float y = uv.y;
  float3 c = lerp(float3(0.05, 0.05, 0.07), float3(0.9, 0.9, 1.0), pow(saturate(1.0 - y * 1.6), 3.0));
  for (int k = 0; k < 3; k++) {
    float cu = 0.18 + 0.32 * float(k);
    float d = abs(frac(uv.x - cu + 0.5) - 0.5);
    c += smoothstep(0.012, 0.0, d - 0.02) * smoothstep(0.62, 0.3, y) * float3(6.0);
  }
  return float4(c, 1.0);
}"""
bands = f.add("shader", {"code": BANDS}, name="bands")
studio = f.add("shader", {"code": STUDIO}, name="studio")
wall = f.add("mesh", {"primitive": "plane", "scale": [16, 28, 1], "position": [0, 0, -3.0], "roughness": 1.0,
                      "emissive": "#000000"}, {"texture": bands}, name="wall")
word = f.add("mesh", {"primitive": "text", "text": "prism", "font": "ultrafast:wide", "weight": 800, "size": 0.68,
                      "depth": 0.55, "bevel": 0.07, "color": "#f4f8ff", "roughness": keys((0, 0.02), (5.0, 0.02, SOFT), (6.5, 0.35)),
                      "transmission": 1.0, "ior": 1.52, "thickness": 1.1, "dispersion": 1.6,
                      "rotation": keys((0.0, [6, -38, -3], INOUT), (8.0, [-4, 30, 2]))}, name="word")
key = f.add("light", {"direction": [-0.5, 0.9, 0.5], "intensity": 2.0}, name="key")
rim = f.add("light", {"direction": [0.7, 0.2, -0.6], "intensity": 1.4, "color": "#9fc3ff"}, name="rim")
cam = f.add("camera", {"target": [0, 0, 0], "fov": 30, "orbit": keys((0.0, [-8, 4, 10.5], INOUT), (8.0, [8, 6, 10.0]))}, name="cam")
scene = f.add("render3d", {"background": "#07060c", "environment_intensity": 1.0, "shadows": False,
                           "environment_rotation": keys((0, -30.0), (8, 60.0))},
              {"camera": cam, "wall": wall, "word": word, "key": key, "rim": rim, "environment": studio}, name="scene")
scene = f.add("bloom", {"threshold": 0.85, "radius": 40, "intensity": 0.5}, {"source": scene}, name="bloom")
cap = f.text("solid glass, rendered live", "ultrafast:serif", 64, [W / 2, 1640], color="#f3f1ec", align="center",
             style="italic", name="cap")
cap_l, cap_w = line_reveal(f, cap, W / 2, 1640, 64, 1.2, dur=0.8)
lab = f.text("IOR 1.52  ·  DISPERSION 1.6", "ultrafast:mono", 26, [W / 2, 1730], color="#f3f1ece8", align="center",
             tracking=3, name="lab")
lab = f.add("opacity", {"opacity": keys((1.6, 0.0, SOFT), (2.2, 1.0))}, {"source": lab}, name="lab_in")
comp = f.stack(scene, [(cap_l, None, cap_w), lab])
f.output = finish(f, comp, grain=0.05)
f.save(os.path.join(FILMS_DIR, "prism.json"))
print("ok", len(f.nodes), "nodes")

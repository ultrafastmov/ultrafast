"""CHROME — bevelled liquid-metal type in a studio whose lights sweep
across it (the environment is a shader, rotated over time), with a
mirrored reflection and bloom. 1080x1920, 8 s."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=(6, 0.5))
W, H = f.W, f.H

STUDIO = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float y = uv.y, u = uv.x;
  // Classic chrome: a bright sky that peaks at a hard horizon, dark ground
  // below. Tops and upper rims catch the sky; fronts mirror the ground.
  float3 sky = lerp(float3(0.55, 0.58, 0.66), float3(2.6, 2.5, 2.35), pow(saturate(y / 0.5), 2.2));
  float3 ground = lerp(float3(0.10, 0.085, 0.075), float3(0.008, 0.008, 0.01), smoothstep(0.5, 0.58, y));
  float3 c = y < 0.5 ? sky : ground;
  // Soft vertical light panels in the sky, sweeping as the world turns.
  for (int k = 0; k < 4; k++) {
    float cu = (float(k) + 0.5) / 4.0;
    float dx = abs(frac(u - cu + 0.5) - 0.5);
    c *= y < 0.5 ? 1.0 - 0.55 * smoothstep(0.03, 0.0, dx - 0.04) : 1.0;
  }
  c += smoothstep(0.05, 0.0, length((uv - float2(0.62, 0.36)) * float2(1.0, 2.0)) - 0.03) * float3(16.0, 8.0, 2.8);
  return float4(c, 1.0);
}"""

studio = f.add("shader", {"code": STUDIO}, name="studio")
FONT = font("Unbounded-VF.ttf")


def word(mirror=False):
    sy = -1 if mirror else 1
    return f.add("mesh", {"primitive": "text", "text": "ultrafast", "font": FONT, "weight": 800, "size": 1.15, "depth": 0.42,
                          "bevel": 0.05, "color": "#f4f4f6", "metallic": 1.0, "roughness": 0.07,
                          "position": [0, sy * 0.62, 0],
                          "scale": [1, sy, 1],
                          "rotation": keys((0.0, [0, -30, 0], INOUT), (8.0, [0, 22, 0]))}, name="word_m" if mirror else "word")


key = f.add("light", {"direction": [-0.3, 1.0, 0.6], "intensity": 0.6}, name="key")
cam = f.add("camera", {"target": [0, 0.35, 0], "fov": 30,
                       "orbit": keys((0.0, [-24, 3, 15.5], INOUT), (8.0, [16, 11, 15.2]))}, name="cam")
rot = keys((0.0, -40, INOUT), (8.0, 70))
scene = f.add("render3d", {"background": "#000000", "environment_intensity": 1.0, "environment_rotation": rot,
                           "shadows": False}, {"camera": cam, "word": word(), "key": key, "environment": studio}, name="scene")
# The mirror world: the same type flipped under the floor line.
mirror = f.add("render3d", {"background": "#00000000", "environment_intensity": 1.0, "environment_rotation": rot,
                            "shadows": False}, {"camera": cam, "word": word(True), "key": key, "environment": studio}, name="mirror")
fade = f.add("gradient", {"kind": "linear", "start": [0, 1000], "end": [0, 1500], "stops": "0:#ffffffff, 1:#ffffff00"}, name="fade")
mirror = f.add("mask", {"mode": "alpha"}, {"source": mirror, "matte": fade}, name="mirror_fade")
mirror = f.add("blur", {"radius": 3}, {"source": mirror}, name="mirror_soft")

stage = f.add("gradient", {"kind": "radial", "center": [W / 2, 1150], "radius": 1100,
                           "stops": "0:#1b1d24, 0.5:#0b0c10, 1:#020203"}, name="stage")
floor_line = f.add("gradient", {"kind": "linear", "start": [0, 960], "end": [0, 1300],
                                "stops": "0:#ffffff00, 0.15:#ffffff0c, 1:#ffffff00"}, name="floor_glow")
comp = f.stack(stage, [floor_line, (mirror, {"opacity": 0.55}, None), scene])

# Bloom: only what is brighter than paper white.
hot = f.add("levels", {"in_black": 0.8, "in_white": 2.0}, {"source": comp}, name="hot")
bloom = f.add("blur", {"radius": 38}, {"source": hot}, name="bloom")
comp = f.blend(bloom, comp, "add", opacity=0.55, name="bloomed")
wide = f.add("blur", {"radius": 120}, {"source": hot}, name="bloom_wide")
comp = f.blend(wide, comp, "add", opacity=0.35, name="bloomed_wide")

serif_i = "InstrumentSerif-Italic.ttf"
line1 = f.text("Liquid metal,", serif_i, 96, [W / 2, 1560], align="center", color="#f1eee8", name="l1")
l1, w1 = line_reveal(f, line1, W / 2, 1560, 96, 1.2, dur=0.8)
line2 = f.text("rendered live.", serif_i, 96, [W / 2, 1660], align="center", color="#f1eee8", name="l2")
l2, w2 = line_reveal(f, line2, W / 2, 1660, 96, 1.35, dur=0.8)
mono = "GeistMono-VF.ttf"
lab1 = f.text("ULTRAFAST / CHROME", mono, 24, [72, 150], color="#f1eee890", tracking=3, weight=500, name="folio")
lab2 = f.text("ENV ROTATION 110°  ·  BEVEL 0.05", mono, 24, [W - 72, 150], align="right", color="#f1eee860", tracking=2, name="folio")
comp = f.stack(comp, [(l1, None, w1), (l2, None, w2), lab1, lab2])
f.output = finish(f, comp, grain=0.07)
f.save(os.path.join(FILMS_DIR, "chrome.json"))
print("ok", len(f.nodes), "nodes")

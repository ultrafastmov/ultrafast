"""PULSE — a film that listens. Everything is driven by the soundtrack:
type weight by the hats, rings and chrome orbs by the kick, bloom by the
bass, the ink's veins by the mids; orbs orbit on oscillators and the lens
racks focus. 1080x1920, 12 s, with sound."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=12.0, motion_blur=(6, 0.5))
W, H = f.W, f.H
SCORE = "../../out/films/hero_score.wav"
START = 4.0  # into the score, where the kick starts


def audio(band, lo, hi, smooth=0.12):
    return {"audio": {"src": SCORE, "band": band, "range": [lo, hi], "smooth": smooth, "start": START}}


f.add("audio", {"src": SCORE, "start": START}, name="score")

ink = f.add("marble", {"filaments": 0.55, "veins": audio("mid", 0.2, 1.6, 0.25)}, name="ink")
ink_dim = f.add("adjust", {"exposure": -1.6, "saturation": 0.7}, {"source": ink}, name="ink_dim")
base = f.add("gradient", {"kind": "radial", "center": [W / 2, 820], "radius": 1400, "stops": "0:#1a1030, 0.6:#07060d, 1:#020203"}, name="base")
dim = f.over(ink_dim, base, opacity=0.45, name="ink_over")

# Rings that breathe with the kick, each a little later and softer.
rings = []
for i in range(5):
    r0 = 260 + i * 70
    ring = f.add("path", {"kind": "circle", "center": [W / 2, 820], "radius": audio("low", r0, r0 + 90 + i * 25, 0.1 + i * 0.05),
                          "stroke": 4 - i * 0.5, "color": "#ffd2f4"}, name="ring")
    rings.append((ring, {"opacity": audio("low", 0.1, 0.95 - i * 0.12, 0.1 + i * 0.05)}, None))

# Chrome orbs on oscillators, swelling on the kick; the lens racks focus.
STUDIO = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float y = uv.y;
  float3 c = lerp(float3(0.5, 0.45, 0.6), float3(2.4, 2.2, 2.6), pow(saturate(y / 0.5), 2.0));
  c = y < 0.5 ? c : lerp(float3(0.12, 0.05, 0.12), float3(0.01), smoothstep(0.5, 0.6, y));
  c += smoothstep(0.05, 0.0, length((uv - float2(0.3, 0.35)) * float2(1.0, 2.0)) - 0.03) * float3(12.0, 3.0, 6.0);
  c += smoothstep(0.05, 0.0, length((uv - float2(0.8, 0.4)) * float2(1.0, 2.0)) - 0.02) * float3(3.0, 6.0, 12.0);
  return float4(c, 1.0);
}"""
studio = f.add("shader", {"code": STUDIO}, name="studio")
inputs = {"environment": studio}
N = 8
for k in range(N):
    ph = k / N
    pos = {"oscillate": {"frequency": 0.18, "amplitude": [2.6, 0.5, 2.6], "base": [0, 0, 0], "phase": [ph, ph * 2, ph + 0.25]}}
    orb = f.add("mesh", {"primitive": "sphere", "segments": 48, "color": "#f2f2f5", "metallic": 1.0, "roughness": 0.08,
                         "position": pos, "scale": audio("low", 0.42, 0.78, 0.14)}, name="orb")
    inputs[f"orb{k}"] = orb
cam = f.add("camera", {"target": [0, 0, 0], "fov": 34, "orbit": keys((0, [-20, 12, 10.5], INOUT), (12, [25, 6, 9.5]))}, name="cam")
inputs["camera"] = cam
key = f.add("light", {"direction": [0.3, 1.0, 0.5], "intensity": 1.2}, name="key")
inputs["key"] = key
orbs = f.add("render3d", {"background": "#00000000", "shadows": False, "environment_intensity": 1.0,
                          "environment_rotation": {"oscillate": {"frequency": 0.05, "amplitude": 60}},
                          "aperture": 16, "focus": {"oscillate": {"frequency": 0.12, "amplitude": 2.4, "base": 10.3}}},
             inputs, name="orbs")
orbs_at = f.add("transform", {"position": [W / 2, 820], "anchor": [W / 2, H / 2]}, {"source": orbs}, name="orbs_at")

# The words: weight follows the hats, tracking breathes.
UNB = "Unbounded-VF.ttf"
words = []
for i, (w, y) in enumerate([("FEEL", 1430), ("THE", 1600), ("BEAT", 1770)]):
    tx = f.text(w, UNB, 150, [W / 2, y], align="center", color="#ffffff",
                weight=audio("high", 250, 900, 0.08 + i * 0.03),
                tracking={"oscillate": {"frequency": 0.25, "amplitude": 6, "base": 2, "phase": i * 0.2}}, name="word")
    lay, win = line_reveal(f, tx, W / 2, y, 150, 0.5 + i * 0.25, dur=0.7, ascent=0.8, descent=0.15)
    words.append((lay, None, win))

top = f.text("PULSE — DRIVEN BY THE SCORE", "GeistMono-VF.ttf", 26, [W / 2, 140], align="center", color="#ffffffa0",
             weight=500, tracking=4, name="folio")
comp = f.stack(dim, rings + [orbs_at] + words + [top])
comp = f.add("bloom", {"threshold": 0.7, "radius": 36, "intensity": audio("low", 0.2, 1.1, 0.2), "wide": 0.35},
             {"source": comp}, name="bloom")
comp = f.add("aberration", {"amount": audio("low", 1, 9, 0.12)}, {"source": comp}, name="ca")
f.output = finish(f, comp, grain=0.07)
f.save(os.path.join(FILMS_DIR, "pulse.json"))
print("ok", len(f.nodes), "nodes")

"""INK — a shader world with editorial type. 1080x1920, 9 s."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=9.0, motion_blur=(6, 0.5))
W, H = f.W, f.H

INK = """
float fbm(float3 p) {
  float s = 0.0, a = 0.5;
  for (int i = 0; i < 4; i++) { s += a * snoise(p); p = p * 2.07 + float3(17.1, 3.7, 9.2); a *= 0.5; }
  return s;
}
float4 shade(float2 pos, float px) {
  float2 uv = (pos - size() * 0.5) / size().y;
  float t = time() * 0.07;
  float3 p = float3(uv * 0.6 + float2(0.0, t * 0.5), t);
  float2 q = float2(fbm(p), fbm(p + float3(5.2, 1.3, 2.0)));
  float2 r = float2(fbm(p + float3(2.6 * q, 0.0) + float3(1.7, 9.2, t)),
                    fbm(p + float3(2.6 * q, 0.0) + float3(8.3, 2.8, -t)));
  float v = fbm(p + float3(2.4 * r, 0.0));
  // Marbled ink: dark body, luminous filaments where the field crosses zero.
  float3 deep = float3(0.003, 0.004, 0.012);
  float3 blue = float3(0.015, 0.04, 0.16);
  float body = smoothstep(-0.25, 0.55, v);
  float fil = pow(saturate(1.0 - abs(v + 0.08) * 4.0), 7.0);
  float fil2 = pow(saturate(1.0 - abs(r.y - 0.1) * 5.0), 9.0) * 0.6;
  float3 hue = lerp(float3(0.95, 0.20, 0.32), float3(1.0, 0.66, 0.32), saturate(q.x * 1.4 + 0.5));
  float3 c = lerp(deep, blue, body * 0.8);
  c += hue * fil * param(1) * 1.4 + float3(0.55, 0.65, 1.0) * fil2 * param(2);
  return float4(c, 1.0);
}"""

MOLTEN = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float t = time() * 0.25;
  float n = snoise(float3(uv * float2(1.2, 2.2), t)) * 0.5 + snoise(float3(uv * 3.1 + 7.0, t * 1.3)) * 0.25;
  float3 gold = float3(1.0, 0.70, 0.36), rose = float3(1.0, 0.28, 0.42), white = float3(1.0, 0.94, 0.86);
  float3 c = lerp(rose, gold, saturate(n + 0.5));
  c = lerp(c, white, pow(saturate(n * 1.3 + 0.35), 4.0));
  return float4(c * 1.15, 1.0);
}"""

ink = f.add("shader", {"code": INK, "resolution": 0.5, "u1": keys((3.4, 0.85, SOFT), (4.2, 0.4), (6.2, 0.4, SOFT), (7.2, 0.7)),
                        "u2": 1.0}, name="ink")
# Molten light seen through the letters.
hot = f.add("shader", {"code": MOLTEN}, name="molten")

black = f.add("solid", {"color": "#050507"}, name="black")
world = f.over(ink, black, name="world", opacity=keys((0.0, 0.0, SOFT), (1.2, 1.0)))
shade_top = f.add("gradient", {"kind": "linear", "start": [0, 300], "end": [0, 1350],
                               "stops": "0:#000000d0, 0.55:#00000070, 1:#00000000"}, name="shade_top")
world = f.over(shade_top, world, name="world_shaded", opacity=keys((3.2, 1.0, SOFT), (4.0, 0.35)))

# Act 1: three lines, masked reveals, waterfall exit.
serif, italic = "InstrumentSerif-Regular.ttf", "InstrumentSerif-Italic.ttf"
X = 84
lines = []
specs = [
    ("Every", serif, 250, 640, 0.6),
    ("frame is", serif, 250, 860, 0.72),
]
for i, (txt, fnt, size, base, t_in) in enumerate(specs):
    t = f.text(txt, fnt, size, [X, base], tracking=-5.5, name="line")
    layer, win = line_reveal(f, t, X, base, size, t_in, t_out=3.35 + i * 0.07)
    lines.append((layer, None, win))
# "a shader." — the italic word that matters.
a = f.text("a", serif, 250, [X, 1080], tracking=-5.5, name="line")
la, wa = line_reveal(f, a, X, 1080, 250, 0.84, t_out=3.49)
s = f.text("shader.", italic, 250, [X + 150, 1080], tracking=-4, color="#ffd7a8", name="line")
ls_, ws = line_reveal(f, s, X + 150, 1080, 250, 0.9, t_out=3.52)
lines += [(la, None, wa), (ls_, None, ws)]

# Act 2: one word, its weight and softness breathing; the hot world shows
# through its letters and the cold world dims around them.
word = f.text("liquid", "Fraunces-VF.ttf", 330, [W / 2, 1110], align="center", color="#ffffff",
              weight=keys((3.8, 260, INOUT), (5.2, 900, INOUT), (6.2, 380)),
              axis_SOFT=keys((3.8, 0, SOFT), (5.6, 100)), axis_opsz=144, axis_WONK=1,
              tracking=keys((3.8, 24, OUT), (5.0, -6)), name="word")
word_xf = f.add("transform", {"scale": keys((3.6, 0.92, OUT), (6.2, 1.04)), "anchor": [W / 2, 1000],
                              "position": [W / 2, 1000]}, {"source": word}, name="word_xf")
through = f.add("mask", {"mode": "alpha"}, {"source": hot, "matte": word_xf}, name="through")
edge = f.add("blur", {"radius": 46}, {"source": through}, name="edge")
dim = f.add("solid", {"color": "#000000"}, name="dim")
act2_win = {"in": 3.7, "out": 6.45}
act2 = [
    (dim, {"opacity": keys((3.6, 0.0, SOFT), (4.3, 0.6), (5.9, 0.6, SOFT), (6.45, 0.0))}, act2_win),
    (edge, {"opacity": keys((3.8, 0.0, SOFT), (4.7, 0.9), (5.9, 0.9, SOFT), (6.45, 0.0))}, act2_win),
    (through, {"opacity": keys((3.75, 0.0, SOFT), (4.15, 1.0), (6.0, 1.0, SOFT), (6.45, 0.0))}, act2_win),
]

# Embers drifting up through the whole film.
embers = f.add("particles", {
    "seed": 7, "rate": 34, "life": [3.5, 6.0], "position": [W / 2, H + 40], "emitter_size": [W * 1.2, 40],
    "direction": -90, "spread": 26, "speed": [70, 190], "gravity": [0, -12], "drag": 0.08,
    "size": [7, 2], "size_jitter": 0.6, "color_start": "#ffc89a", "color_end": "#ff5a7a00",
    "fade_in": 0.15, "fade_out": 0.5, "blend": "add", "emit_start": -6}, name="embers")
embers_glow = f.add("glow", {"radius": 10, "intensity": 1.4}, {"source": embers}, name="embers_glow")

# Act 3: the lockup.
mark = f.text("ultrafast", "Fraunces-VF.ttf", 300, [W / 2, 1050], align="center", color="#f1eee8",
              weight=keys((6.4, 200, OUT), (7.6, 640)), axis_SOFT=100, axis_opsz=144, axis_WONK=1,
              tracking=keys((6.4, 30, OUT), (7.8, -4)), name="mark")
mark_l, mark_w = line_reveal(f, mark, W / 2, 1050, 300, 6.45, dur=0.9)
tag = f.text("GPU-NATIVE  ·  LIVE  ·  EDITABLE", "GeistMono-VF.ttf", 30, [W / 2, 1150], align="center",
             color="#f1eee88f", weight=500, tracking=keys((6.9, 2, OUT), (8.2, 6)), name="tag")
tag_o = f.add("opacity", {"opacity": keys((6.9, 0.0, SOFT), (7.6, 1.0))}, {"source": tag}, name="tag_o")
rule = f.add("shape", {"kind": "rect", "center": [W / 2, 1102], "size": keys((6.8, [0, 2], OUT), (7.9, [520, 2])),
                       "color": "#f1eee840"}, name="rule")

# A pool of dark behind the lockup.
pool = f.add("gradient", {"kind": "radial", "center": [W / 2, 1060], "radius": 760,
                          "stops": "0:#000000c8, 0.6:#00000060, 1:#00000000"}, name="pool")

# Folio chrome.
chrome = []
for y in (150, 1770):
    chrome.append(f.add("shape", {"kind": "rect", "center": [W / 2, y], "size": [W - 144, 2], "color": "#f1eee82e"},
                        name="hair"))
chrome.append(f.text("ultrafast", "Geist-VF.ttf", 30, [72, 128], weight=600, name="folio"))
chrome.append(f.text("INK — N°01", "GeistMono-VF.ttf", 24, [W - 72, 128], align="right", color="#f1eee88f",
                     tracking=2, name="folio"))
chrome.append(f.text("SHADER WORLDS", "GeistMono-VF.ttf", 24, [W - 72, 1812], align="right", color="#f1eee88f",
                     tracking=2, name="folio"))
chrome.append(f.text("01 — 03", "GeistMono-VF.ttf", 24, [72, 1812], color="#f1eee88f", tracking=2, name="folio"))
chrome_g = f.stack(chrome[0], chrome[1:])
chrome_o = f.add("opacity", {"opacity": keys((0.2, 0.0, SOFT), (1.0, 1.0))}, {"source": chrome_g}, name="chrome")

comp = f.stack(world, act2 + lines + [embers_glow,
                                      (pool, {"opacity": keys((6.2, 0.0, SOFT), (7.0, 1.0))}, {"in": 6.2}),
                                      (mark_l, None, mark_w), (rule, None, {"in": 6.8}),
                                      (tag_o, None, {"in": 6.9}), chrome_o])
f.output = finish(f, comp, grain=0.09)
f.save(os.path.join(FILMS_DIR, "ink.json"))
print("ok", len(f.nodes), "nodes")

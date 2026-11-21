"""TERMINAL — a phosphor CRT: typed lines, a block cursor, a dot-matrix
wordmark, all pushed through a curved-glass shader with scanlines, an
aperture grille and phosphor bloom. 1080x1920, 9 s."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=9.0, motion_blur=None)
W, H = f.W, f.H
VT = "VT323-Regular.ttf"
SIZE = 64
ADV = 0.40 * SIZE  # VT323 advance per character
LINE = 78
GREEN = "#7dff9b"
DIM = "#3fae5f"

CRT = """
float4 shade(float2 pos, float px) {
  float2 sz = size();
  float2 uv = pos / sz * 2.0 - 1.0;
  // Curved glass.
  uv *= 1.0 + float2(0.045, 0.07) * float2(uv.y * uv.y, uv.x * uv.x);
  float2 p = (uv * 0.5 + 0.5) * sz;
  float2 e = abs(uv);
  float inside = smoothstep(1.0, 0.985, max(e.x, e.y));
  float4 c = input(0, p);
  // Beam rows and an aperture grille.
  float row = 0.62 + 0.38 * pow(abs(sin(pos.y * 3.14159 / 3.0)), 0.8);
  float m = fmod(floor(pos.x), 3.0);
  float3 grille = m < 1.0 ? float3(1.0, 0.72, 0.72) : (m < 2.0 ? float3(0.72, 1.0, 0.72) : float3(0.72, 0.72, 1.0));
  float flick = 0.97 + 0.03 * sin(time() * 115.0) + 0.02 * sin(time() * 7.0);
  float vig = 1.0 - 0.35 * dot(uv, uv) * 0.5;
  c.rgb *= grille * row * flick * vig;
  // The glass itself: a faint sheen and a dark bezel.
  c.rgb += 0.012 * float3(0.7, 1.0, 0.8) * (1.0 - smoothstep(0.0, 1.2, length(uv - float2(-0.4, -0.5))));
  return float4(c.rgb * inside, 1.0) + float4(float3(0.004), 1.0) * (1.0 - inside);
}"""

DOTS = """
float4 shade(float2 pos, float px) {
  // Re-sample the wordmark on an LED dot grid.
  float cell = param(0);
  float2 c = (floor(pos / cell) + 0.5) * cell;
  float a = input(0, c).a;
  float d = length(pos - c) / cell;
  float dot_ = smoothstep(0.42, 0.30, d) * step(0.35, a);
  float3 col = srgb_to_linear(float3(0.49, 1.0, 0.61));
  return float4(col * dot_, dot_);
}"""

lines = [
    ("$ ultrafast render film.json", 0.4, 1.4, GREEN),
    ("  planning 55 nodes -> 17 kernels", 1.6, 0.25, DIM),
    ("  compiling slang ........... ok", 1.95, 0.5, DIM),
    ("  frame 0001   0.31 ms   gpu", 2.55, 0.12, DIM),
    ("  frame 0002   0.29 ms   gpu", 2.7, 0.12, DIM),
    ("  frame 0240   0.30 ms   gpu", 2.85, 0.12, DIM),
    ("  0 readbacks. 3,200 fps.", 3.1, 0.3, GREEN),
    ("$ ultrafast live film.json", 3.8, 0.9, GREEN),
    ("  edit -> next frame. no recompile.", 4.9, 0.4, DIM),
]
X, Y0 = 84, 330
layers = []
for i, (txt, t0, dur, col) in enumerate(lines):
    n = len(txt)
    tx = f.text(txt, VT, SIZE, [X, Y0 + i * LINE], color=col,
                reveal=keys((t0, 0, "linear"), (t0 + dur, n)), reveal_softness=0.01, name="line")
    layers.append((tx, None, {"in": t0}))

# A block cursor that follows the typing, then blinks on the last prompt.
cursor_keys = []
for i, (txt, t0, dur, col) in enumerate(lines):
    y = Y0 + i * LINE - SIZE * 0.32
    cursor_keys += [(t0, [X + ADV * 0.5, y], "linear"), (t0 + dur, [X + ADV * (len(txt) + 0.5), y], "hold")]
last_y = Y0 + len(lines) * LINE - SIZE * 0.32
cursor_keys += [(5.5, [X + ADV * 2.5, last_y], "hold")]
cursor = f.add("shape", {"kind": "rect", "color": GREEN, "size": [ADV * 0.95, SIZE * 0.78],
                         "center": keys(*cursor_keys)}, name="cursor")
prompt = f.text("$", VT, SIZE, [X, Y0 + len(lines) * LINE], color=GREEN, name="prompt")
blink = f.add("opacity", {"opacity": keys(*[(5.5 + i * 0.4, (i + 1) % 2, "hold") for i in range(10)])},
              {"source": cursor}, name="blink")

# The wordmark on a dot grid, rising in.
mark = f.text("ultrafast", "Unbounded-VF.ttf", 230, [W / 2, 1420], align="center", color="#ffffff", weight=800, tracking=-4,
              stagger=0.06, glyph_offset=keys((0.0, [0, 60], OUT), (0.5, [0, 0])), glyph_opacity=keys((0.0, 0, "hold"), (0.06, 1)),
              name="mark")
f.nodes[mark]["timing"] = {"offset": 5.8}
dots = f.add("shader", {"code": DOTS, "u0": 11.0}, {"in0": mark}, name="dots")
tag = f.text("VIDEO, COMPILED.", VT, 64, [W / 2, 1540], align="center", color=GREEN, reveal=keys((6.8, 0, "linear"), (7.4, 16)),
             reveal_softness=0.01, name="tag")

bg = f.add("gradient", {"kind": "radial", "center": [W / 2, H / 2], "radius": 1300, "stops": "0:#04140a, 1:#010402"}, name="bg")
screen = f.stack(bg, layers + [(prompt, None, {"in": 5.5}), (cursor, None, {"out": 5.5}), (blink, None, {"in": 5.5}),
                               (dots, None, {"in": 5.8}), (tag, None, {"in": 6.8})])
# Phosphor bloom, then the tube.
glow = f.add("blur", {"radius": 14}, {"source": screen}, name="phosphor")
screen = f.blend(glow, screen, "add", opacity=0.9, name="bloom")
glow2 = f.add("blur", {"radius": 60}, {"source": screen}, name="halo")
screen = f.blend(glow2, screen, "add", opacity=0.35, name="halo_add")
crt = f.add("shader", {"code": CRT}, {"in0": screen}, name="crt")
f.output = finish(f, crt, grain=0.06, vignette=False)
f.save(os.path.join(FILMS_DIR, "terminal.json"))
print("ok", len(f.nodes), "nodes")

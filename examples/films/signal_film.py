"""SIGNAL — a broadcast breaking up: the whole frame runs through a glitch
shader (band tearing, RGB split, scanlines, tracking roll) whose intensity
bursts on a seeded schedule; type flickers in glyph by glyph. 1080x1920, 8 s."""

import random

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=8.0, motion_blur=None)
W, H = f.W, f.H
rnd = random.Random(11)

GLITCH = """
float h2(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
float4 shade(float2 pos, float px) {
  float t = time();
  float amt = param(0);
  float frame = floor(t * 30.0);
  // Tear the frame into bands of random height and slide some sideways.
  float coarse = floor(pos.y / 64.0);
  float bandH = 6.0 + 90.0 * h2(float2(coarse, frame));
  float band = floor(pos.y / bandH);
  float pick = h2(float2(band, frame + 1.0));
  float shift = pick > 1.0 - 0.4 * amt ? (h2(float2(band, frame + 7.0)) - 0.5) * 260.0 * amt : 0.0;
  float2 p = pos + float2(shift, 0.0);
  // Split the channels.
  float split = (2.0 + 22.0 * amt) * (0.6 + 0.8 * h2(float2(frame, 3.0)));
  float4 g = input(0, p);
  float r = input(0, p + float2(split, 0.0)).r;
  float b = input(0, p - float2(split, 0.0)).b;
  float4 c = float4(r, g.g, b, g.a);
  // Blocks of the picture freeze to a flat color now and then.
  float2 blk = floor(pos / 24.0);
  if (h2(blk + frame * 0.37) > 1.0 - 0.05 * amt * amt) c.rgb = input(0, (blk + 0.5) * 24.0).rgb * 1.3;
  // Scanlines, a rolling tracking band, and snow.
  c.rgb *= 0.86 + 0.14 * step(1.5, fmod(pos.y, 3.0));
  float roll = frac(t * param(1)) * (size().y + 200.0) - 100.0;
  float trk = smoothstep(46.0, 0.0, abs(pos.y - roll)) * param(2);
  c.rgb = lerp(c.rgb, float3(h2(float2(pos.x * 0.05, frame + pos.y))) * 0.6, trk * 0.7);
  c.rgb += (h2(pos + frame * 13.0) - 0.5) * (0.04 + 0.12 * amt);
  return float4(max(c.rgb, 0.0), c.a);
}"""


def bursts(spans, base=0.04):
    """Glitch amount: quiet at `base`, flickering bursts in [t0, t1]."""
    ks, t = [(0.0, base, "hold")], 0.0
    for t0, t1, peak in spans:
        ks.append((t0, base, "hold"))
        x = t0
        while x < t1:
            ks.append((x, rnd.uniform(0.25, 1.0) * peak, "hold"))
            x += rnd.choice([1 / 30, 2 / 30, 3 / 30])
        ks.append((t1, base, "hold"))
    return keys(*sorted(ks, key=lambda k: k[0]))


bars = f.add("video", {"src": "../assets/testsrc.mp4", "fit": "fill", "loop": True}, name="bars")
graded = f.add("adjust", {"saturation": 1.25, "contrast": 1.15, "exposure": -0.4}, {"source": bars}, name="graded")
black = f.add("solid", {"color": "#050505"}, name="black")
dark = f.add("solid", {"color": "#000000"}, name="dark")

ARCH, MONO = "ArchivoBlack-Regular.ttf", "GeistMono-VF.ttf"
flicker = dict(stagger=0.035, stagger_order="random",
               glyph_opacity=keys((0.0, 0, "hold"), (0.03, 1, "hold"), (0.07, 0, "hold"), (0.1, 1)))
no = f.text("NO", ARCH, 330, [76, 820], color="#ffffff", **flicker, name="no")
sig = f.text("SIGNAL", ARCH, 250, [76, 1060], color="#ffffff", **flicker, name="sig")
for n, t0 in ((no, 0.4), (sig, 0.55)):
    f.nodes[n]["timing"] = {"offset": t0}
ch = f.text("CH 03  ·  REC  ·  SP", MONO, 30, [76, 1700], color="#ffffffd0", weight=600, tracking=2, name="osd")
rec = f.add("shape", {"kind": "circle", "center": [W - 110, 160], "size": [26, 26], "color": "#ff2a2a",
                      "softness": 1}, name="rec")
rec_blink = f.add("opacity", {"opacity": keys(*[(i * 0.5, i % 2, "hold") for i in range(17)])}, {"source": rec}, name="rec_blink")

# Act 2: the clean word, then it breaks again.
word = f.text("ultrafast", ARCH, 300, [W / 2, 1060], align="center", color="#f2f2f2", tracking=-6,
              stagger=0.05, stagger_order="center",
              glyph_offset=keys((0.0, [0, 140], OUT), (0.5, [0, 0])), glyph_opacity=keys((0.0, 0), (0.15, 1)), name="word")
f.nodes[word]["timing"] = {"offset": 3.0}
sub = f.text("BROADCAST FROM THE GPU", MONO, 34, [W / 2, 1160], align="center", color="#ffffffc0", weight=600, tracking=4,
             stagger=0.03, glyph_opacity=keys((0.0, 0, "hold"), (0.02, 1)), name="sub")
f.nodes[sub]["timing"] = {"offset": 3.6}
redbar = f.add("shape", {"kind": "rect", "color": "#ff2a2a", "center": [W / 2, 1210],
                         "size": keys((3.7, [0, 8], OUT), (4.3, [560, 8]))}, name="redbar")

act1 = f.stack(graded, [(dark, {"opacity": 0.45}, None), (no, None, {"in": 0.4}), (sig, None, {"in": 0.55})])
act2 = f.stack(black, [(word, None, {"in": 3.0}), (sub, None, {"in": 3.6}), (redbar, None, {"in": 3.7})])
# Act 3: the bars come back through the word.
through = f.add("mask", {"mode": "alpha"}, {"source": graded, "matte": word}, name="through")
act3 = f.stack(act2, [(through, {"opacity": keys((5.6, 0.0, "hold"), (5.62, 1.0, "hold"), (5.75, 0.0, "hold"), (5.8, 1.0))}, {"in": 5.6})])

screen = f.stack(act1, [(act3, None, {"in": 2.9})])
screen = f.stack(screen, [ch, rec_blink])
glitch = f.add("shader", {"code": GLITCH,
                          "u0": bursts([(0.0, 0.7, 1.0), (1.6, 2.1, 0.7), (2.7, 3.15, 1.0), (4.9, 5.1, 0.5), (5.55, 6.0, 0.9), (7.4, 7.55, 0.4)]),
                          "u1": 0.35, "u2": keys((0, 1.0), (2.9, 1.0, "hold"), (3.0, 0.25))},
                {"in0": screen}, name="glitch")
f.output = finish(f, glitch, grain=0.08)
f.save(os.path.join(FILMS_DIR, "signal.json"))
print("ok", len(f.nodes), "nodes")

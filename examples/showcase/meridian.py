"""MERIDIAN — the main title of a prestige drama that does not exist, every
image of it made in ultrafast: a brass compass and an armillary sphere in 3D
(extruded vector rings, glass, depth of field, dust in candlelight), a sea
chart drawn as shader parchment and islands under vector ink (a compass rose,
rhumb lines, soundings, a cartouche), a route that inks itself across it, a
night sea under star trails with a wireframe globe, and a lighthouse whose
beam turns through fog to light the title. 1920x1080 in 2.39:1, 32 s, with an
original score (ElevenLabs Music).

The score lives outside the repo: $ULTRAFAST_SHOWCASE/meridian/audio/score/take-1.mp3

    python3 meridian.py && ultrafast export $ULTRAFAST_SHOWCASE/meridian/meridian.json $ULTRAFAST_SHOWCASE/meridian/meridian.mp4

MERIDIAN_PART=chart (or compass, sea, armillary, light) previews one shot.
"""

import math
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "meridian")
PART = os.environ.get("MERIDIAN_PART")

f = Film(1920, 1080, fps=30, duration=32.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX, CY = W / 2, H / 2
BAR = round((H - W / 2.39) / 2)  # 2.39:1 inside 16:9
BONE, INK = "#efe6d2", "#3a2414"
SERIF, TITLE, MONO = "Cormorant Garamond", "Bodoni Moda", "ultrafast:mono"
SOFT = {"bezier": [0.45, 0.0, 0.2, 1.0]}
parts = {}

# Shared Slang: fractal noise.
FBM = """
float fbm2(float2 p, int oct) {
  float s = 0.0, a = 0.5;
  for (int i = 0; i < oct; i++) { s += a * snoise(float3(p, 1.7 * float(i))); p = p * 2.03 + float2(5.1, 1.7); a *= 0.5; }
  return s;
}
float hash2(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
"""


def credit(name, x, y, t_in, t_out, role=None, align="left", size=44, color=BONE, nid="credit"):
    """A name in spaced capitals that settles in letter by letter, breathes
    wider while it holds, and fades; an optional small role line above."""
    out = []
    fade = keys((t_in, 0.0), (t_in + 0.9, 1.0), (t_out - 0.7, 1.0, IN), (t_out, 0.0))
    p = {"text": name, "font": SERIF, "weight": 500, "size": size, "color": color, "align": align,
         "valign": "baseline", "position": [x, y], "opacity": fade, "stagger": 0.045, "stagger_order": "center",
         "tracking": keys((t_in, size * 0.22), (t_out, size * 0.30, "linear")),
         "glyph_opacity": keys((t_in, 0.0), (t_in + 0.8, 1.0)),
         "glyph_offset": keys((t_in, [0, 6], OUT), (t_in + 1.0, [0, 0]))}
    out.append(f.add("text", p, timing={"in": t_in, "out": t_out}, name=nid))
    if role:
        out.append(f.add("text", {"text": role, "font": SERIF, "weight": 500, "size": size * 0.4, "color": color,
                                  "align": align, "valign": "baseline", "position": [x, y - size * 1.25],
                                  "tracking": size * 0.16, "opacity": keys((t_in, 0.0), (t_in + 0.9, 0.7),
                                                                            (t_out - 0.7, 0.7, IN), (t_out, 0.0))},
                       timing={"in": t_in, "out": t_out}, name=f"{nid}_role"))
    return out


def shot(base, layers):
    return f.stack(base, layers)


def group(layers, name):
    """Layers composited over each other (no backdrop)."""
    cur = layers[0]
    for lid in layers[1:]:
        cur = f.over(lid, cur, name=name)
    return cur


def poly(pts):
    return "M " + " L ".join(f"{x:.2f} {y:.2f}" for x, y in pts) + " Z"


# ================================================================== the chart
# Chart space is the frame: 1920x1080. Islands are fractal noise lifted
# by bumps at chosen places, so the route can run between them.
ISLES = [(300, 600, 70), (400, 560, 45), (225, 655, 42), (560, 795, 150), (465, 735, 100), (1625, 500, 112),
         (1705, 565, 70), (1390, 182, 92), (1482, 150, 60), (1300, 468, 24), (1378, 502, 20), (905, 925, 36)]
ROSE = (960, 520)

PAPER = FBM + """
float land(float2 pos) {
  // Domain-warped noise for bays and headlands; the rose keeps open water.
  float2 q = pos / 250.0 + 0.7 * float2(fbm2(pos / 420.0 + 3.0, 3), fbm2(pos / 420.0 + 8.0, 3));
  float h = fbm2(q, 7) * 0.8 - 0.5;
  h -= exp(-dot(pos - float2(960.0, 520.0), pos - float2(960.0, 520.0)) / 90000.0) * 0.8;
""" + "".join(f"  h += exp(-dot(pos - float2({x:.1f}, {y:.1f}), pos - float2({x:.1f}, {y:.1f})) / {r * r:.1f}) * 0.9;\n"
              for x, y, r in ISLES) + """  return h;
}
float4 shade(float2 pos, float px) {
  float2 sz = size();
  // Laid paper: warm, mottled, fibrous.
  float m = fbm2(pos / 240.0 + 11.0, 4);
  float3 c = lerp(float3(0.85, 0.76, 0.58), float3(0.94, 0.88, 0.73), saturate(0.6 + m * 0.6));
  c *= 1.0 + snoise(float3(pos / float2(2.5, 9.0), 4.0)) * 0.025;
  // Foxing: a few brown specks per cell.
  float2 cell = floor(pos / 70.0);
  float r = hash2(cell);
  float2 at = (cell + float2(hash2(cell + 3.1), hash2(cell + 7.7))) * 70.0;
  float spot = r < 0.22 ? smoothstep(2.0 + r * 30.0, 0.5, length(pos - at)) : 0.0;
  c = lerp(c, float3(0.58, 0.38, 0.20), spot * 0.45);
  // Edges browned with age.
  float e = min(min(pos.x, pos.y), min(sz.x - pos.x, sz.y - pos.y)) + fbm2(pos / 70.0, 3) * 40.0;
  c = lerp(c, float3(0.45, 0.28, 0.14), smoothstep(95.0, 0.0, e) * 0.8);
  // Land: a sepia wash, darker along the coast, an inked coastline and
  // the water lines that follow it out to sea.
  float h = land(pos);
  float2 g = float2(land(pos + float2(1.5, 0.0)) - land(pos - float2(1.5, 0.0)),
                    land(pos + float2(0.0, 1.5)) - land(pos - float2(0.0, 1.5))) / 3.0;
  float dist = h / max(length(g), 1e-5);  // px, positive inland
  float3 ink = float3(0.22, 0.13, 0.07);
  if (dist > 0.0) {
    float3 wash = lerp(float3(0.92, 0.84, 0.68), float3(0.66, 0.48, 0.29), smoothstep(0.16, 0.0, h));
    c *= wash * (1.0 - 0.06 * saturate(snoise(float3(pos / 3.0, 9.0))));
  } else {
    float d = -dist;
    float lines = smoothstep(0.9, 0.2, abs(frac(d / 6.5 + 0.5) - 0.5) * 6.5) * saturate(1.0 - d / 30.0) * step(3.0, d);
    c = lerp(c, ink, lines * 0.5);
  }
  c = lerp(c, ink, smoothstep(1.7, 0.5, abs(dist)) * 0.9);
  return from_srgb(float4(c, 1.0));
}"""


def build_chart():
    paper = f.add("shader", {"code": PAPER}, name="chart_paper")
    ink = []
    S = dict(color=INK)
    # Neatline: a double rule with alternating degree bars between.
    for inset, w in [(46, 2.2), (62, 0.9)]:
        ink.append(f.add("path", {"d": f"M {inset} {inset} H {W - inset} V {H - inset} H {inset} Z", "stroke": w, **S},
                         name="neat"))
    bars = []
    for k in range(int((W - 108) // 48)):
        if k % 2 == 0:
            x = 54 + k * 48
            bars.append(f"M {x} 48 h 48 v 12 h -48 Z M {x} {H - 60} h 48 v 12 h -48 Z")
    for k in range(int((H - 108) // 48)):
        if k % 2 == 0:
            y = 54 + k * 48
            bars.append(f"M 48 {y} h 12 v 48 h -12 Z M {W - 60} {y} h 12 v 48 h -12 Z")
    ink.append(f.add("path", {"d": " ".join(bars), "opacity": 0.85, **S}, name="neat_bars"))
    # Graticule.
    grid = [f"M {x} 62 V {H - 62}" for x in range(240, W - 60, 240)] + \
           [f"M 62 {y} H {W - 62}" for y in range(180, H - 60, 180)]
    ink.append(f.add("path", {"d": " ".join(grid), "stroke": 0.7, "opacity": 0.3, **S}, name="graticule"))
    # Rhumb lines out of the rose, and of a lesser rose to the south-east.
    for (cx, cy), n, op in [(ROSE, 32, 0.42), ((1560, 880), 16, 0.3)]:
        line = f.add("path", {"d": f"M {cx} {cy} L {cx} {cy - 2400}", "stroke": 0.8, "opacity": op, **S}, name="rhumb")
        ink.append(f.add("repeater", {"count": n, "rotate": 360.0 / n, "cx": cx, "cy": cy, "fade": 1.0},
                         {"source": line}, name="rhumbs"))
    # The rose: rings of degree ticks, then three tiers of points, each
    # point a light and a dark half.
    rx, ry = ROSE
    for r, w in [(250, 1.3), (236, 0.8), (128, 0.8)]:
        ink.append(f.add("path", {"kind": "circle", "center": [rx, ry], "radius": r, "stroke": w, **S}, name="rose_ring"))
    for count, r0, r1, w in [(120, 236, 250, 0.8), (36, 230, 256, 1.3)]:
        tick = f.add("path", {"d": f"M {rx} {ry - r0} L {rx} {ry - r1}", "stroke": w, **S}, name="rose_tick")
        ink.append(f.add("repeater", {"count": count, "rotate": 360.0 / count, "cx": rx, "cy": ry, "fade": 1.0},
                         {"source": tick}, name="rose_ticks"))
    for n, R, inner, rot in [(8, 112, 0.26, 22.5), (4, 168, 0.2, 45.0), (4, 228, 0.15, 0.0)]:
        light, dark = [], []
        for k in range(n):
            a = math.radians(rot + k * 360.0 / n)
            da = math.pi / 4 if n == 4 else math.pi / 8
            tip = (rx + R * math.sin(a), ry - R * math.cos(a))
            ri = R * inner
            left = (rx + ri * math.sin(a - da), ry - ri * math.cos(a - da))
            right = (rx + ri * math.sin(a + da), ry - ri * math.cos(a + da))
            light.append(poly([(rx, ry), tip, right]))
            dark.append(poly([(rx, ry), left, tip]))
        # Each tier is drawn whole before the next one covers it.
        ink.append(f.add("path", {"d": " ".join(light), "color": "#f1e6cc"}, name="rose_light"))
        ink.append(f.add("path", {"d": " ".join(light), "stroke": 1.2, **S}, name="rose_light_line"))
        ink.append(f.add("path", {"d": " ".join(dark), "opacity": 0.85, **S}, name="rose_dark"))
    for r, fill in [(16, False), (6, True)]:
        p = {"kind": "circle", "center": [rx, ry], "radius": r, **S}
        if not fill:
            p["stroke"] = 1.2
        ink.append(f.add("path", p, name="rose_hub"))
    ink.append(f.add("text", {"text": "N", "font": SERIF, "style": "italic", "weight": 600, "size": 44, "align": "center",
                              "valign": "baseline", "position": [rx, ry - 262], **S}, name="rose_n"))
    # Names, soundings, the cartouche.
    for text, x, y, size, track, rot in [("Isles of Corva", 300, 505, 24, 2, -6), ("HALVARD", 560, 800, 30, 14, -4),
                                         ("Lumen", 1640, 505, 26, 4, 0), ("The Sisters", 1430, 262, 22, 2, 3),
                                         ("Mare Meridianum", 1180, 860, 34, 6, -7)]:
        ink.append(f.add("text", {"text": text, "font": SERIF, "style": "italic", "weight": 500, "size": size,
                                  "tracking": track, "align": "center", "valign": "middle", "position": [x, y],
                                  "rotation": rot, "opacity": 0.85, **S}, name="place"))
    rng = random.Random(7)
    sound = []
    while len(sound) < 46:
        x, y = rng.uniform(110, W - 110), rng.uniform(110, H - 110)
        if math.hypot(x - rx, y - ry) < 300 or any(math.hypot(x - a, y - b) < r * 1.45 for a, b, r in ISLES):
            continue
        if x > 1380 and y > 820:
            continue
        sound.append((x, y, rng.choice([4, 7, 9, 12, 15, 18, 21, 24, 30, 36, 42])))
    for x, y, v in sound:
        ink.append(f.add("text", {"text": str(v), "font": SERIF, "style": "italic", "size": 15, "align": "center",
                                  "valign": "middle", "position": [x, y], "opacity": 0.7, **S}, name="sounding"))
    bx, by, bw, bh = 1420, 852, 420, 128
    ink.append(f.add("path", {"d": f"M {bx} {by} h {bw} v {bh} h {-bw} Z", "color": "#f1e6cc"}, name="cartouche_bg"))
    for inset, w in [(0, 1.8), (7, 0.8)]:
        ink.append(f.add("path", {"d": f"M {bx + inset} {by + inset} h {bw - 2 * inset} v {bh - 2 * inset} "
                                       f"h {-(bw - 2 * inset)} Z", "stroke": w, **S}, name="cartouche"))
    ink.append(f.add("text", {"text": "MARE MERIDIANUM", "font": SERIF, "weight": 600, "size": 27, "tracking": 7,
                              "align": "center", "valign": "baseline", "position": [bx + bw / 2, by + 58], **S},
                     name="cartouche_title"))
    ink.append(f.add("text", {"text": "Tabula nautica  ·  anno MDCCXII", "font": SERIF, "style": "italic", "size": 19,
                              "align": "center", "valign": "baseline", "position": [bx + bw / 2, by + 92],
                              "opacity": 0.85, **S}, name="cartouche_sub"))
    return paper, group(ink, "chart_ink")



# ================================================================== the compass card, and the compass as meshes
# (the meshes only for the comparison of looks at the end)
def circle_pts(r, n=96, cw=True):
    pts = [(r * math.sin(2 * math.pi * k / n), -r * math.cos(2 * math.pi * k / n)) for k in range(n)]
    return pts if cw else pts[::-1]


def ring_d(r_out, r_in, n=128):
    """An annulus as path data (outer clockwise, inner counter-clockwise), px."""
    return poly(circle_pts(r_out, n)) + " " + poly(circle_pts(r_in, n, cw=False))


WARM_ROOM = FBM + """
float4 shade(float2 pos, float px) {
  // Equirectangular: a dark study lit by a candle low on one side, a tall
  // window's cold light opposite, a soft warm skylight band, and a few
  // brass-catching strips so metal always has something to show.
  float2 uv = pos / size();
  float lon = (uv.x - 0.5) * 6.2831853, lat = (0.5 - uv.y) * 3.14159265;
  float2 q = float2(lon, lat);
  float3 c = lerp(float3(0.012, 0.007, 0.004), float3(0.09, 0.05, 0.028), saturate(lat * 0.9 + 0.25));
  float2 cand = float2(-0.9, 0.12);
  c += float3(1.0, 0.55, 0.22) * 9.0 * exp(-dot(q - cand, q - cand) / 0.02);
  c += float3(1.0, 0.60, 0.28) * 0.9 * exp(-dot(q - cand, q - cand) / 0.5);
  c += float3(0.95, 0.70, 0.45) * 0.45 * smoothstep(0.25, 0.45, lat) * smoothstep(1.1, 0.7, lat);
  float wx = abs(lon - 2.1), wy = abs(lat - 0.45);
  c += float3(0.55, 0.65, 0.85) * 1.6 * smoothstep(0.30, 0.24, wx) * smoothstep(0.42, 0.34, wy);
  for (int k = 0; k < 3; k++) {
    float x = -2.6 + float(k) * 1.9;
    c += float3(1.0, 0.78, 0.52) * 1.2 * smoothstep(0.07, 0.02, abs(lon - x)) * smoothstep(0.9, 0.5, abs(lat - 0.2));
  }
  c *= 1.0 + fbm2(pos / 60.0, 3) * 0.25;
  return float4(c, 1.0);
}"""

WOOD = FBM + """
float4 shade(float2 pos, float px) {
  // Dark walnut: long grain, a few figure swirls, waxed.
  float2 p = pos / float2(900.0, 26.0);
  float g = fbm2(float2(p.x, p.y + fbm2(pos / 300.0, 3) * 2.0), 5);
  float rings = sin((p.y + g * 3.0) * 6.2831853) * 0.5 + 0.5;
  float3 c = lerp(float3(0.10, 0.05, 0.025), float3(0.24, 0.13, 0.065), rings * 0.6 + g * 0.5 + 0.3);
  return from_srgb(float4(c, 1.0));
}"""


def build_dial_card():
    return build_dial(widen=False)


def build_dial(widen=True):
    """The compass card, drawn flat in a 1080 square at the frame center,
    then widened to fill the frame: a cylinder's cap maps the whole frame
    onto its disc, so the circle comes back round."""
    S = dict(color="#1d1410")
    L = [f.add("solid", {"color": "#efe4c8"}, name="dial_paper")]
    for r, w in [(528, 3.0), (512, 1.2), (410, 1.2), (396, 0.8), (300, 1.0)]:
        L.append(f.add("path", {"kind": "circle", "center": [CX, CY], "radius": r, "stroke": w, **S}, name="dial_ring"))
    for count, r0, r1, w in [(360, 512, 498, 1.0), (72, 512, 486, 1.6), (36, 512, 470, 2.4)]:
        tick = f.add("path", {"d": f"M {CX} {CY - r0} L {CX} {CY - r1}", "stroke": w, **S}, name="dial_tick")
        L.append(f.add("repeater", {"count": count, "rotate": 360.0 / count, "cx": CX, "cy": CY, "fade": 1.0},
                       {"source": tick}, name="dial_ticks"))
    for k in range(12):
        a = math.radians(k * 30)
        L.append(f.add("text", {"text": str(k * 30), "font": SERIF, "weight": 600, "size": 40, "align": "center",
                                "valign": "middle", "position": [CX + 440 * math.sin(a), CY - 440 * math.cos(a)],
                                "rotation": k * 30, **S}, name="dial_deg"))
    for k, lt in enumerate("NESW"):
        a = math.radians(k * 90)
        L.append(f.add("text", {"text": lt, "font": TITLE, "weight": 500, "size": 92, "align": "center",
                                "valign": "middle", "position": [CX + 345 * math.sin(a), CY - 345 * math.cos(a)],
                                "rotation": k * 90, "color": "#7a1810" if lt == "N" else S["color"]}, name="dial_card"))
    light, dark = [], []
    for n, R, inner, rot in [(4, 250, 0.16, 45.0), (4, 290, 0.13, 0.0)]:
        for k in range(n):
            a = math.radians(rot + k * 360.0 / n)
            tip = (CX + R * math.sin(a), CY - R * math.cos(a))
            ri = R * inner
            left = (CX + ri * math.sin(a - math.pi / 4), CY - ri * math.cos(a - math.pi / 4))
            right = (CX + ri * math.sin(a + math.pi / 4), CY - ri * math.cos(a + math.pi / 4))
            light.append(poly([(CX, CY), tip, right]))
            dark.append(poly([(CX, CY), left, tip]))
        L.append(f.add("path", {"d": " ".join(light), "stroke": 1.4, **S}, name="dial_star"))
        L.append(f.add("path", {"d": " ".join(dark), "opacity": 0.9, **S}, name="dial_star_dark"))
        light, dark = [], []
    L.append(f.add("text", {"text": "INSTRUMENTUM NAUTICUM  ·  MERIDIAN  ·  ANNO MDCCXII  ·  ", "font": SERIF,
                            "weight": 600, "size": 24, "tracking": 5, "color": S["color"],
                            "on_path": f"M {CX} {CY - 352} A 352 352 0 1 1 {CX - 0.01} {CY - 352}"},
                   name="dial_motto"))
    card = group(L, "dial")
    if not widen:
        return f.add("blur", {"radius": 1.0}, {"source": card}, name="dial_card_prefilter")
    wide = f.add("transform", {"anchor": [CX, CY], "position": [CX, CY], "scale": [W / H, 1.0]}, {"source": card},
                 name="dial_wide")
    # Seen at a grazing angle the card is minified; a touch of blur keeps
    # its hairlines from shimmering.
    return f.add("blur", {"radius": 1.4}, {"source": wide}, name="dial_prefilter")


def needle_d(half):
    """One half of the needle (north or south) as a lozenge, px, pointing up."""
    L, w = 168, 9
    # Lying flat (rotated -90 about x), the page's down becomes the far
    # side of the dial, so north is drawn pointing down the page.
    return poly([(0, L), (w, 0), (0, -4), (-w, 0)]) if half == "n" else poly([(0, -L), (w, 0), (0, 4), (-w, 0)])


def build_compass(t0, t1):
    env = f.add("shader", {"code": WARM_ROOM}, name="room")
    wood = f.add("shader", {"code": WOOD}, name="walnut")
    dial = build_dial()
    BRASS = dict(color="#c49a58", metallic=1.0)
    # The needle hunts and settles on north: a damped swing.
    swing = []
    for k in range(0, 61):
        t = t0 + (t1 - t0) * k / 60
        u = t - t0
        swing.append((t, [0.0, 38.0 * math.exp(-u * 0.75) * math.cos(u * 3.1) + 2.0 * math.sin(u * 0.9), 0.0]))
    m = {
        "table": f.add("mesh", {"primitive": "plane", "scale": [44, 26, 1], "rotation": [-90, 8, 0], "color": "#c0a894",
                                "roughness": 0.5, "position": [0, 0, 0]}, {"texture": wood}, name="table"),
        "body": f.add("mesh", {"primitive": "cylinder", "scale": [4.3, 0.55, 4.3], "position": [0, 0.275, 0],
                               "roughness": 0.38, "segments": 64, **BRASS}, name="body"),
        "bezel": f.add("mesh", {"primitive": "path", "d": ring_d(222, 186), "units": 100, "depth": 0.26, "bevel": 0.05,
                                "rotation": [-90, 0, 0], "position": [0, 0.63, 0], "roughness": 0.22, **BRASS},
                       name="bezel"),
        "dial": f.add("mesh", {"primitive": "cylinder", "scale": [3.76, 0.02, 3.76], "position": [0, 0.56, 0],
                               "color": "#ffffff", "metallic": 0.15, "roughness": 0.5, "segments": 64},
                      {"texture": dial}, name="dial_disc"),
        "north": f.add("mesh", {"primitive": "path", "d": needle_d("n"), "units": 100, "depth": 0.035, "bevel": 0.012,
                                "rotation": keys(*[(t, [-90.0, v[1], 0.0]) for t, v in swing]),
                                "position": [0, 0.66, 0], "color": "#7a1b14", "metallic": 0.85, "roughness": 0.14},
                       name="needle_n"),
        "south": f.add("mesh", {"primitive": "path", "d": needle_d("s"), "units": 100, "depth": 0.035, "bevel": 0.012,
                                "rotation": keys(*[(t, [-90.0, v[1], 0.0]) for t, v in swing]),
                                "position": [0, 0.66, 0], "color": "#c9ccd2", "metallic": 1.0, "roughness": 0.18},
                       name="needle_s"),
        "pivot": f.add("mesh", {"primitive": "sphere", "scale": 0.17, "position": [0, 0.69, 0], "roughness": 0.2, **BRASS},
                       name="pivot"),
        "glass": f.add("mesh", {"primitive": "cylinder", "scale": [3.74, 0.05, 3.74], "position": [0, 0.76, 0],
                                "color": "#ffffff", "transmission": 1.0, "roughness": 0.02, "ior": 1.5,
                                "thickness": 0.012, "segments": 64}, name="glass"),
        "key": f.add("light", {"direction": [-0.75, 0.55, 0.35], "color": "#ffc489", "intensity": 2.4}, name="key"),
        "candle": f.add("light", {"kind": "point", "position": [-4.5, 2.2, 2.0], "color": "#ffb064", "intensity": 16.0},
                        name="candle"),
        "fill": f.add("light", {"direction": [0.6, 0.9, -0.4], "color": "#8fa6c8", "intensity": 0.45}, name="window"),
    }
    cam = f.add("camera", {"target": [0.25, 0.6, 0.35], "fov": 28,
                           "orbit": keys((t0, [-24.0, 30.0, 8.6]), (t1, [-10.0, 24.0, 7.2], "linear"))}, name="macro")
    m["camera"] = cam
    m["environment"] = env
    scene = f.add("render3d", {"background": "#060403", "ambient": 0.04, "environment_intensity": 1.6,
                               "shadows": True, "shadow_strength": 0.75, "aperture": 16, "focus": 7.6},
                  m, timing={"in": t0, "out": t1}, name="compass3d")
    scene = f.add("bloom", {"threshold": 0.9, "radius": 30, "intensity": 0.35}, {"source": scene},
                  timing={"in": t0, "out": t1}, name="compass_bloom")
    # Dust turning in the candlelight: fine motes, and a few soft ones close
    # to the lens.
    motes = f.add("particles", {"seed": 4, "rate": 16, "life": [5.0, 8.0], "position": [CX, CY],
                                "emitter_size": [W, H], "direction": -80, "spread": 60, "speed": [6, 22],
                                "gravity": [3, -2], "size": [3.6, 2.4], "color_start": "#ffd9a0aa",
                                "fade_in": 0.3, "fade_out": 0.4, "blend": "add", "emit_start": t0 - 8.0},
                    timing={"in": t0, "out": t1}, name="motes")
    near = f.add("particles", {"seed": 9, "rate": 2.5, "life": [4.0, 6.0], "position": [CX, CY],
                               "emitter_size": [W, H], "direction": -60, "spread": 80, "speed": [10, 30],
                               "gravity": [0, 0], "size": [34.0, 26.0], "color_start": "#ffcf9022",
                               "fade_in": 0.4, "fade_out": 0.5, "blend": "add", "emit_start": t0 - 6.0},
                   timing={"in": t0, "out": t1}, name="bokeh")
    near = f.add("blur", {"radius": 6}, {"source": near}, timing={"in": t0, "out": t1}, name="bokeh_soft")
    # Dust shows only where the candle's light crosses the air.
    shaft = f.add("gradient", {"start": [200, 150], "end": [1300, 900],
                               "stops": "0:#ffffff00, 0.3:#ffffffff, 0.62:#ffffff60, 1:#ffffff00"}, name="shaft")
    dust = f.add("mask", {"mode": "alpha"}, {"source": f.over(near, motes, name="dust"), "matte": shaft},
                 timing={"in": t0, "out": t1}, name="dust_lit")
    # Candlelight falls off fast: the room beyond the compass goes dark.
    pool = f.add("gradient", {"kind": "radial", "center": [CX - 120, CY + 60], "radius": 1150, "space": "srgb",
                              "stops": "0:#ffffff, 0.45:#e2cdb4, 0.8:#4a3424, 1:#0e0805"}, name="compass_pool")
    scene = f.blend(pool, scene, "multiply", name="compass_lit")
    return shot(scene, [(dust, {"opacity": 1.0}, None)])




# ================================================================== the night sea (shaders)
NIGHT = FBM + """
// A night sea from a cliff: star trails wheeling round a pole above the
// frame (a timelapse, longer each second), a warm glow on the horizon, and
// swells that mirror the sky and break the glow into a glitter path.
static const float HZ = 640.0;
float3 sky(float elev) {
  // elev: radians above the horizon.
  float3 c = lerp(float3(0.020, 0.032, 0.065), float3(0.002, 0.004, 0.014), saturate(pow(elev / 0.5, 0.6)));
  return c;
}
float3 glow(float x, float elev) {
  float gx = exp(-pow((x - 1180.0) / 420.0, 2.0));
  return float3(0.85, 0.42, 0.16) * gx * (exp(-abs(elev) / 0.012) * 1.6 + exp(-abs(elev) / 0.06) * 0.35);
}
float4 shade(float2 pos, float px) {
  float2 sz = size();
  float t = time();
  float f = 1150.0;  // focal length, px
  float3 c;
  if (pos.y < HZ) {
    float elev = (HZ - pos.y) / f;
    c = sky(elev) + glow(pos.x, elev);
    // Star trails: one possible star per thin ring round the pole.
    float2 P = float2(640.0, -620.0);
    float2 d = pos - P;
    float r = length(d);
    float th = atan2(d.y, d.x);
    float sweep = 0.015 + 0.05 * t;  // radians of trail so far
    for (int k = -1; k <= 1; k++) {
      float i = floor(r / 1.7) + float(k);
      float h = hash2(float2(i, 17.0));
      if (h > 0.42) continue;
      float rr = (i + 0.5) * 1.7;
      float line = smoothstep(0.95, 0.15, abs(r - rr));
      float th0 = lerp(0.55, 2.55, hash2(float2(i, 3.0)));
      float a = (th - th0) / sweep;  // 0 at the tail, 1 at the head
      float on = smoothstep(0.0, 0.08, a) * smoothstep(1.02, 0.98, a);
      float b = pow(hash2(float2(i, 9.0)), 6.0) * 2.4 + 0.035;
      float3 tint = lerp(float3(1.0, 0.86, 0.72), float3(0.72, 0.84, 1.0), hash2(float2(i, 5.0)));
      c += tint * line * on * b * (0.55 + 0.45 * a) * smoothstep(0.0, 0.03, elev);
    }
    // Haze low on the horizon.
    c = lerp(c, float3(0.06, 0.07, 0.10), exp(-elev / 0.03) * 0.5);
  } else {
    // Swells: a plane seen from above; each pixel looks at a patch of water
    // at depth z, sloped by a few travelling waves.
    float z = f * 22.0 / max(pos.y - HZ, 0.5);
    float X = (pos.x - sz.x * 0.5) * z / f;
    float2 w = float2(X, z);
    float2 g = float2(0.0, 0.0);
    float2 dirs[5] = { float2(0.2, 1.0), float2(-0.5, 0.85), float2(0.9, 0.4), float2(-0.1, 1.0), float2(0.6, 0.8) };
    float lens[5] = { 41.0, 23.0, 12.0, 6.3, 3.1 };
    float foot = z / f;  // water units per pixel
    for (int k = 0; k < 5; k++) {
      float2 dk = normalize(dirs[k]);
      float kk = 6.2831853 / lens[k];
      float ph = dot(w, dk) * kk - t * sqrt(9.8 * kk) * 0.9 + float(k) * 1.7;
      // Waves finer than a couple of pixels would only alias: drop them.
      float keep = smoothstep(0.5, 0.25, foot / lens[k]);
      g += dk * cos(ph) * 0.24 * keep / (1.0 + float(k) * 0.5);
    }
    g += float2(fbm2(w / 7.0 + t * 0.1, 3), fbm2(w / 7.0 + 7.0 - t * 0.1, 3)) * 0.1 * smoothstep(1.2, 0.4, foot);
    // Reflected ray: looking down at angle a, tilted by the slope.
    float down = (pos.y - HZ) / f;
    float elev = max(down + g.y * 0.9, -0.02);
    float fres = 0.02 + 0.98 * pow(1.0 - saturate(down * 2.2 + 0.15), 5.0);
    // The glow's glitter path: only facets tilted toward it catch it.
    float glit = exp(-pow((pos.x + g.x * 140.0 - 1180.0) / (90.0 + 260.0 * down), 2.0));
    glit *= smoothstep(0.02, 0.16, -g.y + 0.04) * (0.6 + 0.4 * exp(-down * 3.0));
    float3 refl = sky(abs(elev)) * (0.35 + fres);
    c = float3(0.0015, 0.003, 0.007) + refl + float3(0.95, 0.48, 0.18) * glit * 0.9;
    // Fade the finest detail out with distance (it would only alias).
    c = lerp(c, sky(0.0) + glow(pos.x, 0.0) * 0.4, exp(-(pos.y - HZ) / 5.0) * 0.85);
  }
  return float4(c, 1.0);
}"""

GLOBE = """
float4 shade(float2 pos, float px) {
  float R = param(0);
  float2 c = float2(param(1), param(2));
  float rot = radians(param(3));
  float tilt = radians(param(4));
  float2 d = (pos - c) / R;
  float r = length(d);
  float aa = px / R;
  float acc = saturate(1.0 - abs(r - 1.0) / (aa * 1.3)) * 0.9;
  if (r < 1.0) {
    float ct = cos(tilt), st = sin(tilt);
    for (int side = 0; side < 2; side++) {
      float z = sqrt(max(1.0 - r * r, 0.0)) * (side == 0 ? 1.0 : -1.0);
      float3 p = float3(d.x, -d.y, z);
      float3 q = float3(p.x, p.y * ct + p.z * st, -p.y * st + p.z * ct);
      float lat = asin(clamp(q.y, -1.0, 1.0));
      float lon = atan2(q.x, q.z) + rot;
      float stp = radians(15.0);
      float w = aa * 1.1 / max(abs(z), 0.12);
      float dl = abs(frac(lat / stp + 0.5) - 0.5) * stp;
      float dm = abs(frac(lon / stp + 0.5) - 0.5) * stp * cos(lat);
      float line = max(saturate(1.0 - dl / w), saturate(1.0 - dm / w));
      line = max(line, saturate(1.0 - abs(lat) / (w * 1.8)));
      acc = max(acc, line * (side == 0 ? 0.85 : 0.22));
    }
  }
  float a = acc * param(5);
  return float4(float3(0.92, 0.74, 0.48) * a, a);
}"""


def build_sea(t0, t1):
    night = f.add("shader", {"code": NIGHT}, timing={"in": t0, "out": t1, "offset": t0 - 3.0}, name="night")
    globe = f.add("shader", {"code": GLOBE, "u0": 240, "u1": 1400, "u2": 360,
                             "u3": keys((t0, -30.0), (t1, 25.0, "linear")), "u4": 23.4,
                             "u5": keys((t0 + 0.8, 0.0, SOFT), (t0 + 2.2, 0.7), (t1 - 1.0, 0.7), (t1 - 0.4, 0.0))},
                  timing={"in": t0, "out": t1}, name="globe")
    glow = f.add("glow", {"radius": 18, "intensity": 0.5}, {"source": globe}, timing={"in": t0, "out": t1},
                 name="globe_glow")
    sea = f.add("transform", {"anchor": [CX, CY], "position": [CX, CY], "scale": keys((t0, 1.0), (t1, 1.05, "linear"))},
                {"source": night}, timing={"in": t0, "out": t1}, name="sea_push")
    return shot(sea, [f.blend(glow, sea, "screen", name="globe_screen")])


parts["sea"] = build_sea(15.8, 21.4)


# ================================================================== the lighthouse (shader + vector)
LAMP = (1590.0, 352.0)
NIGHT_FOG = FBM + """
// Night fog round a lighthouse: a turning beam (two, back to back) seen
// from the side, thick where the fog is thick, flaring when it swings
// toward us; the lamp's halo; drifting banks of mist.
float4 shade(float2 pos, float px) {
  float t = time();
  float2 lamp = float2(param(1), param(2));
  float phi = radians(param(3));
  float3 c = lerp(float3(0.003, 0.006, 0.013), float3(0.012, 0.022, 0.040), saturate(pos.y / 900.0));
  float fog = fbm2(pos / 420.0 + float2(t * 0.05, 0.0), 5) * 0.5 + 0.5;
  float fog2 = fbm2(pos / 160.0 + float2(t * 0.11, 3.0), 4) * 0.5 + 0.5;
  c += float3(0.03, 0.05, 0.08) * fog * fog * 0.6;
  float2 d = pos - lamp;
  float r = length(d);
  for (int k = 0; k < 2; k++) {
    float a = phi + 3.14159265 * float(k);
    // Projected direction: the beam turns in a level plane; seen from the
    // side its far half shortens and tips a little up.
    float2 dir = normalize(float2(cos(a), -0.08 * sin(a)));
    float toward = sin(a);  // 1: pointing at us
    float along = dot(d, dir);
    float across = abs(dot(d, float2(-dir.y, dir.x)));
    float width = 5.0 + along * 0.05;
    float len = 2400.0 * (1.0 - 0.75 * max(toward, 0.0));
    float cone = smoothstep(width, width * 0.55, across) * step(0.0, along) * exp(-along / len);
    cone += smoothstep(width * 2.2, 0.0, across) * step(0.0, along) * exp(-along / (len * 0.5)) * 0.25;
    float dens = 0.25 + 1.1 * fog * fog2;
    c += float3(0.95, 0.90, 0.78) * cone * dens * 0.75;
  }
  // The lamp and its halo; brighter as a beam faces us.
  float face = pow(saturate(max(sin(phi), sin(phi + 3.14159265))), 6.0);
  c += float3(1.0, 0.94, 0.80) * (exp(-r / 14.0) * 3.0 + exp(-r / 90.0) * (0.25 + 1.4 * face) + exp(-r / 420.0) * 0.05 * (1.0 + 2.0 * face));
  return float4(c, 1.0);
}"""

MIST = FBM + """
// Low mist in front of the headland: soft, drifting, lit a little by the
// lamp above it.
float4 shade(float2 pos, float px) {
  float t = time();
  float m = fbm2(pos / float2(520.0, 140.0) + float2(t * 0.06, 0.0), 5) * 0.5 + 0.5;
  float band = smoothstep(560.0, 820.0, pos.y) * smoothstep(1150.0, 860.0, pos.y);
  float a = saturate(m * 1.4 - 0.35) * band * 0.75;
  float3 col = float3(0.035, 0.05, 0.075);
  return float4(col * a, a);
}"""


def build_light(t0, t1):
    lx, ly = LAMP
    sky = f.add("shader", {"code": NIGHT_FOG, "u1": lx, "u2": ly,
                           "u3": keys((t0, -160.0), (t1, 250.0, "linear"))},
                timing={"in": t0, "out": t1}, name="fog_night")
    # The headland: a ragged cliff rising to the right, and the tower on it.
    rng = random.Random(11)
    ridge = []
    for k in range(0, 41):
        x = -40 + k * 52
        base = 860 - 250 * (1 / (1 + math.exp(-(x - 1330) / 150))) - 26 * math.sin(x / 140)
        if x > 1800:
            base += (x - 1800) * 0.55
        if 1470 <= x <= 1780:
            base = 648  # the level ground the light stands on
        ridge.append((x, base + rng.uniform(-14, 14) * (0.2 if 1470 <= x <= 1780 else 1.0)))
    land = "M -40 1100 " + " ".join(f"L {x:.1f} {y:.1f}" for x, y in ridge) + " L 2000 1100 Z"
    cliff = f.add("path", {"d": land, "color": "#05070b"}, name="headland")
    base_y = 640.0
    tower_d = (f"M {lx - 30} {base_y} L {lx + 30} {base_y} L {lx + 17} {ly + 30} L {lx - 17} {ly + 30} Z "
               f"M {lx - 27} {ly + 22} h 54 v 8 h -54 Z "             # gallery
               f"M {lx - 15} {ly - 18} h 30 v 40 h -30 Z "            # lantern room
               f"M {lx - 19} {ly - 18} L {lx} {ly - 42} L {lx + 19} {ly - 18} Z "  # cap
               f"M {lx - 1.5} {ly - 42} v -14 h 3 v 14 Z")             # vane
    house = f"M {lx + 52} {base_y + 8} h 120 v -50 l -60 -32 l -60 32 Z"
    tower = f.add("path", {"d": tower_d + " " + house, "color": "#06080c"}, name="tower")
    # Lit glass in the lantern room, seen through the bars.
    glass = f.add("path", {"d": f"M {lx - 12} {ly - 15} h 24 v 34 h -24 Z", "color": "#fff3d6"}, name="lantern")
    bars = f.add("path", {"d": f"M {lx - 4} {ly - 16} v 36 M {lx + 4} {ly - 16} v 36", "stroke": 2.0,
                          "color": "#06080c"}, name="lantern_bars")
    mist = f.add("shader", {"code": MIST}, timing={"in": t0, "out": t1}, name="mist")
    scene = shot(sky, [cliff, tower, glass, bars, (mist, {"opacity": 1.0}, None)])
    scene = f.add("bloom", {"threshold": 0.8, "radius": 40, "intensity": 0.55}, {"source": scene},
                  timing={"in": t0, "out": t1}, name="light_bloom")
    return f.add("transform", {"anchor": [lx, ly], "position": [lx, ly],
                               "scale": keys((t0, 1.0), (t1, 1.06, "linear"))}, {"source": scene},
                 timing={"in": t0, "out": t1}, name="light_push")


# ================================================================== raymarched 3D
# The 3D shots are `raymarch` nodes: the scene is a distance function, and
# ultrafast adds the camera, soft shadows, occlusion, reflections between the
# brass parts, the candle-lit room they reflect, and antialiasing.
COMPASS_SCENE = FBM + """
// Materials: 0 walnut, 1 brass, 2 polished brass, 3 card, 4 red steel,
// 5 bright steel. knob(0): the needle's swing, degrees.
float ndot2(float2 a, float2 b) { return a.x * b.x - a.y * b.y; }
float sdRhombus(float2 p, float2 b) {
  p = abs(p);
  float h = clamp(ndot2(b - 2.0 * p, b) / dot(b, b), -1.0, 1.0);
  float d = length(p - 0.5 * b * float2(1.0 - h, 1.0 + h));
  return d * sign(p.x * b.y + p.y * b.x - b.x * b.y);
}
float2 map(float3 p) {
  float2 r = float2(p.y, 0.0);
  r = opU(r, float2(sdCylinder(p - float3(0.0, 0.25, 0.0), 2.15, 0.25, 0.06), 1.0));
  r = opU(r, float2(sdBox2(float2(length(p.xz) - 2.02, p.y - 0.60), float2(0.17, 0.11), 0.055), 2.0));
  r = opU(r, float2(sdCylinder(p - float3(0.0, 0.50, 0.0), 1.87, 0.03, 0.01), 3.0));
  float3 q = rotY(p - float3(0.0, 0.62, 0.0), radians(knob(0)));
  float needle = max(sdRhombus(q.xz, float2(0.085, 1.62)), abs(q.y) - 0.016) - 0.006;
  r = opU(r, float2(needle, q.z < 0.0 ? 4.0 : 5.0));
  return opU(r, float2(length(p - float3(0.0, 0.645, 0.0)) - 0.085, 2.0));
}
void material(float id, float3 p, float3 n, inout float3 alb, inout float metal, inout float rough) {
  metal = 0.0; rough = 0.5;
  if (id < 0.5) {
    // Quarter-sawn walnut: long straight grain, slow figure, waxed.
    float2 w = p.xz * float2(0.18, 7.0);
    float g = fbm2(float2(w.x, w.y + fbm2(p.xz * float2(0.15, 0.5), 3) * 0.8), 4);
    float grain = sin((w.y + g * 1.6) * 6.2831853) * 0.5 + 0.5;
    float fine = snoise(float3(p.x * 0.8, p.z * 60.0, 3.0)) * 0.5 + 0.5;
    alb = lerp(float3(0.035, 0.017, 0.009), float3(0.11, 0.055, 0.026), grain * 0.55 + fine * 0.2 + g * 0.35 + 0.15);
    rough = 0.42 + fine * 0.1;
  } else if (id < 1.5) {
    alb = float3(0.80, 0.56, 0.26); metal = 1.0;
    rough = 0.30 + fbm2(p.xz * 14.0 + p.y * 9.0, 3) * 0.1;
  } else if (id < 2.5) {
    alb = float3(0.86, 0.62, 0.30); metal = 1.0; rough = 0.14;
  } else if (id < 3.5) {
    // The card: drawn in ultrafast (type, rings, ticks) and read from in0.
    alb = input(0, float2(960.0, 540.0) + p.xz / 1.86 * 540.0).rgb;
    rough = 0.6;
  } else if (id < 4.5) {
    alb = float3(0.45, 0.05, 0.03); metal = 0.6; rough = 0.2;
  } else {
    alb = float3(0.85, 0.86, 0.88); metal = 1.0; rough = 0.12;
  }
}"""

ARMILLARY_SCENE = FBM + """
// A fixed horizon and meridian; inside them, turning on the tilted pole
// (knob(0), degrees), the equator, two colures, the tropics and the broad
// ecliptic band; the earth at the center wrapped in the chart (in0),
// turning by knob(1). Materials: 1 brass, 2 polished brass, 5 steel,
// 6 engraved ecliptic, 7 globe, 8 dark wood.
static const float TILT = 0.408;  // 23.4 degrees
float3 inner(float3 p) { return rotY(rotZ(p, -TILT), -radians(knob(0))); }
float2 map(float3 p) {
  float2 r = float2(sdBand(p, float3(0.0, 1.0, 0.0), 3.42, 0.17, 0.045), 1.0);
  r = opU(r, float2(sdBand(p, float3(0.0, 0.0, 1.0), 3.16, 0.12, 0.06), 2.0));
  float3 q = inner(p);
  r = opU(r, float2(sdBand(q, float3(0.0, 1.0, 0.0), 2.84, 0.07, 0.035), 2.0));
  r = opU(r, float2(min(sdBand(q, float3(1.0, 0.0, 0.0), 2.76, 0.055, 0.03),
                        sdBand(q, float3(0.0, 0.0, 1.0), 2.76, 0.055, 0.03)), 1.0));
  for (int k = -1; k <= 1; k += 2) {
    float3 o = q - float3(0.0, float(k) * 2.76 * sin(TILT), 0.0);
    r = opU(r, float2(sdBand(o, float3(0.0, 1.0, 0.0), 2.76 * cos(TILT), 0.04, 0.025), 1.0));
  }
  r = opU(r, float2(sdBand(rotX(q, TILT), float3(0.0, 1.0, 0.0), 2.66, 0.2, 0.022), 6.0));
  float3 ax = float3(-sin(TILT), cos(TILT), 0.0);
  r = opU(r, float2(sdCapsule(p, -ax * 3.7, ax * 3.7, 0.045), 5.0));
  r = opU(r, float2(length(p) - 0.95, 7.0));
  r = opU(r, float2(sdCapsule(p, float3(0.0, -3.44, 0.0), float3(0.0, -5.2, 0.0), 0.13), 1.0));
  return opU(r, float2(sdCylinder(p - float3(0.0, -5.32, 0.0), 1.5, 0.13, 0.05), 8.0));
}
void material(float id, float3 p, float3 n, inout float3 alb, inout float metal, inout float rough) {
  metal = 1.0; rough = 0.28;
  if (id < 1.5) {
    alb = float3(0.80, 0.56, 0.26);
    rough = 0.26 + fbm2(p.xz * 12.0 + p.y * 7.0, 3) * 0.1;
  } else if (id < 2.5) {
    alb = float3(0.86, 0.62, 0.30); rough = 0.13;
  } else if (id < 5.5) {
    alb = float3(0.85, 0.86, 0.88); rough = 0.12;
  } else if (id < 6.5) {
    // The ecliptic, engraved with degrees and the twelve signs' bounds.
    float3 e = rotX(inner(p), TILT);
    float ang = atan2(e.z, e.x) / 6.2831853 + 0.5;
    float t1 = smoothstep(0.35, 0.0, abs(frac(ang * 72.0) - 0.5) * 2.0 - 0.75);
    float t2 = smoothstep(0.2, 0.0, abs(frac(ang * 12.0) - 0.5) * 2.0 - 0.97);
    alb = float3(0.88, 0.66, 0.34) * (1.0 - 0.55 * max(t1 * step(0.08, abs(length(e.xz) - 2.66)), t2));
    rough = 0.2;
  } else if (id < 7.5) {
    float3 g = normalize(rotY(inner(p), -radians(knob(1))));
    float lon = atan2(g.x, g.z), lat = asin(clamp(g.y, -1.0, 1.0));
    alb = input(0, float2((lon / 6.2831853 + 0.5) * 1920.0, (0.5 - lat / 3.14159265) * 1080.0)).rgb * 0.72;
    metal = 0.0; rough = 0.45;
  } else {
    alb = float3(0.05, 0.025, 0.012); metal = 0.0; rough = 0.35;
  }
}"""

def build_armillary_rm(t0, t1, globe_tex):
    scene = f.add("raymarch", {"scene": ARMILLARY_SCENE, "environment": "room", "background_level": 0.12,
                               "orbit": keys((t0, [-38.0, 10.0, 20.5]), (t1, [-8.0, 17.0, 18.2], "linear")),
                               "target": [0.0, -0.45, 0.0], "fov": 30, "light": [-6.0, 3.0, 5.0],
                               "light_color": "#ffd199", "light_intensity": 85, "exposure": 1.1, "samples": 2,
                               "max_distance": 70, "k0": keys((t0, 0.0), (t1, 46.0, "linear")),
                               "k1": keys((t0, 0.0), (t1, 40.0, "linear"))},
                  {"in0": globe_tex}, timing={"in": t0, "out": t1}, name="armillary_rm")
    scene = f.add("bloom", {"threshold": 0.8, "radius": 34, "intensity": 0.4}, {"source": scene},
                  timing={"in": t0, "out": t1}, name="armillary_bloom")
    pool = f.add("gradient", {"kind": "radial", "center": [CX - 60, CY], "radius": 1100, "space": "srgb",
                              "stops": "0:#ffffff, 0.55:#e0ccb4, 0.88:#3a281a, 1:#0c0704"}, name="armillary_pool")
    return f.blend(pool, scene, "multiply", name="armillary_lit")


def build_compass_rm(t0, t1):
    dial = build_dial_card()
    swing = []
    for k in range(0, 61):
        t = t0 + (t1 - t0) * k / 60
        u = t - t0
        swing.append((t, 38.0 * math.exp(-u * 0.75) * math.cos(u * 3.1) + 2.0 * math.sin(u * 0.9)))
    scene = f.add("raymarch", {"scene": COMPASS_SCENE, "environment": "room", "background_level": 0.2,
                               "orbit": keys((t0, [-28.0, 30.0, 8.8]), (t1, [-12.0, 24.0, 7.3], "linear")),
                               "target": [0.15, 0.5, 0.25], "fov": 30, "light": [-4.2, 2.8, 2.6],
                               "light_color": "#ffce95", "light_intensity": 30, "exposure": 1.1, "samples": 2,
                               "k0": keys(*swing)},
                  {"in0": dial}, timing={"in": t0, "out": t1}, name="compass_rm")
    # Dust turning in the candlelight: fine motes, and soft ones close to
    # the lens (a soft sprite, so nothing needs blurring).
    soft = f.add("gradient", {"kind": "radial", "center": [32, 32], "radius": 32,
                              "stops": "0:#ffffffff, 0.5:#ffffff70, 1:#ffffff00"}, name="bokeh_sprite")
    motes = f.add("particles", {"seed": 4, "rate": 16, "life": [5.0, 8.0], "position": [CX, CY],
                                "emitter_size": [W, H], "direction": -80, "spread": 60, "speed": [6, 22],
                                "gravity": [3, -2], "size": [3.6, 2.4], "color_start": "#ffd9a0aa",
                                "fade_in": 0.3, "fade_out": 0.4, "blend": "add", "emit_start": t0 - 8.0},
                    timing={"in": t0, "out": t1}, name="motes")
    near = f.add("particles", {"seed": 9, "rate": 2.5, "life": [4.0, 6.0], "position": [CX, CY],
                               "emitter_size": [W, H], "direction": -60, "spread": 80, "speed": [10, 30],
                               "gravity": [0, 0], "size": [44.0, 34.0], "color_start": "#ffcf9030",
                               "shape": "sprite", "fade_in": 0.4, "fade_out": 0.5, "blend": "add",
                               "emit_start": t0 - 6.0}, {"sprite": soft}, timing={"in": t0, "out": t1}, name="bokeh")
    shaft = f.add("gradient", {"start": [200, 150], "end": [1300, 900],
                               "stops": "0:#ffffff00, 0.3:#ffffffff, 0.62:#ffffff60, 1:#ffffff00"}, name="shaft")
    dust = f.add("mask", {"mode": "alpha"}, {"source": f.over(near, motes, name="dust"), "matte": shaft},
                 timing={"in": t0, "out": t1}, name="dust_lit")
    scene = f.add("bloom", {"threshold": 0.8, "radius": 24, "intensity": 0.35}, {"source": scene},
                  timing={"in": t0, "out": t1}, name="compass_bloom")
    # A long lens: the far table falls out of focus.
    far = f.add("blur", {"radius": 7}, {"source": scene}, timing={"in": t0, "out": t1}, name="compass_far")
    focus = f.add("gradient", {"start": [0, 140], "end": [0, 520], "stops": "0:#ffffffff, 1:#ffffff00"},
                  name="compass_focus")
    far = f.add("mask", {"mode": "alpha"}, {"source": far, "matte": focus}, timing={"in": t0, "out": t1},
                name="compass_far_mask")
    scene = f.over(far, scene, name="compass_dof")
    return f.over(dust, scene, name="compass_dusty")


# ------------------------------------------------------------------ sound
f.add("audio", {"src": os.path.join(D, "audio", "score", "take-1.mp3"), "gain": 1.0}, name="score")
BLACK = f.add("solid", {"color": "#030304"}, name="black")

# ------------------------------------------------------------------ 0  the crosshair (0–4.6 s)
# An equator and a meridian draw out of the center; the series card sits
# in the corner they make, like a label on a chart.
lines = []
for (dx, dy, t) in [(-1, 0, 0.4), (1, 0, 0.4), (0, -1, 1.3), (0, 1, 1.3)]:
    L = W * 0.5 if dx else (H * 0.5 - BAR)
    lines.append(f.add("path", {"d": f"M {CX} {CY} L {CX + dx * L} {CY + dy * L}", "stroke": 1.2, "color": BONE,
                                "opacity": keys((3.2, 0.55), (4.2, 0.0)),
                                "trim_end": keys((t, 0.0, SOFT), (t + 2.2, 1.0))}, name="hair"))
lines += credit("A ULTRAFAST ORIGINAL SERIES", CX + 26, CY + 44, 1.5, 4.0, size=19, nid="series")
S0 = shot(BLACK, lines)

# ------------------------------------------------------------------ 1  the compass (iris out of the crosshair)
parts["compass"] = build_compass_rm(3.4, 9.4)
S1 = shot(parts["compass"], credit("ELENA VOSS", 190, 780, 5.0, 8.2))
T = f.add("transition", {"kind": "iris", "at": 3.6, "duration": 1.6, "softness": 0.25}, {"a": S0, "b": S1},
          name="t_iris")

# ------------------------------------------------------------------ 2  the chart table (8–14 s)
# The chart lies on a table seen in perspective; a route inks itself from
# the western isles round the rose to Lumen, a ship's mark riding its
# head, the position reading out beneath.
paper, chart_ink = build_chart()
route = [((330, 640), (560, 560), (760, 330), (1060, 352)),
         ((1060, 352), (1330, 372), (1440, 430), (1515, 492))]


def bez(seg, u):
    (x0, y0), (x1, y1), (x2, y2), (x3, y3) = seg
    a, b, c, d = (1 - u) ** 3, 3 * u * (1 - u) ** 2, 3 * u * u * (1 - u), u ** 3
    return (a * x0 + b * x1 + c * x2 + d * x3, a * y0 + b * y1 + c * y2 + d * y3)


pts = [bez(sg, i / 200) for sg in route for i in range(201)]
arc = [0.0]
for (xa, ya), (xb, yb) in zip(pts, pts[1:]):
    arc.append(arc[-1] + math.hypot(xb - xa, yb - ya))
total = arc[-1]


def at_length(sl):
    for i in range(1, len(arc)):
        if arc[i] >= sl:
            k = (sl - arc[i - 1]) / max(arc[i] - arc[i - 1], 1e-9)
            return [pts[i - 1][0] + k * (pts[i][0] - pts[i - 1][0]), pts[i - 1][1] + k * (pts[i][1] - pts[i - 1][1])]
    return list(pts[-1])


r0, r1 = 9.0, 13.2
d = "M {} {} ".format(*route[0][0]) + " ".join("C {} {} {} {} {} {}".format(*sg[1], *sg[2], *sg[3]) for sg in route)
ease = lambda u: u * u * (3 - 2 * u)
head = keys(*[(r0 + (r1 - r0) * k / 40, at_length(total * ease(k / 40))) for k in range(41)])
inked = group([chart_ink,
               f.add("path", {"d": d, "stroke": 4.2, "color": INK, "cap": "round", "dash": [16, 10],
                              "trim_end": keys(*[(r0 + (r1 - r0) * k / 40, ease(k / 40)) for k in range(41)])},
                     timing={"in": r0}, name="route"),
               f.add("shape", {"kind": "circle", "size": [16, 16], "color": INK, "center": head},
                     timing={"in": r0}, name="ship"),
               f.add("path", {"kind": "circle", "radius": 30, "stroke": 3.4, "color": INK, "dash": [8, 6],
                              "center": [0, 0], "position": head, "rotation": keys((r0, 0.0), (14.0, 220.0, "linear"))},
                     timing={"in": r0}, name="ship_ring")], "chart_inked")
chart = f.blend(inked, paper, "multiply", name="chart")
parts["chart"] = chart
# Pinned flat on a table, seen from a chair: the far edge narrower, a slow
# drift as if the camera breathes.
table = f.add("corner_pin", {"corners": keys(
    (7.8, [150, 104, 1770, 104, 2520, 1250, -600, 1250]),
    (14.4, [40, 84, 1700, 140, 2480, 1290, -660, 1180], "linear"))}, {"source": chart}, name="table")
far = f.add("blur", {"radius": 10}, {"source": table}, name="table_far")
focus = f.add("gradient", {"start": [0, 140], "end": [0, 620], "stops": "0:#ffffffff, 1:#ffffff00"}, name="focus")
far = f.add("mask", {"mode": "alpha"}, {"source": far, "matte": focus}, name="far_mask")
table = f.over(far, table, name="table_dof")
pool = f.add("gradient", {"kind": "radial", "center": [CX + 20, CY + 60], "radius": 1120, "space": "srgb",
                          "stops": "0:#fff1d6, 0.4:#d9ac78, 0.8:#5a3a20, 1:#0c0704"}, name="candle_pool")
table = f.blend(pool, table, "multiply", name="lit_table")
readout = dict(font=MONO, size=20, tracking=4, color="#2b1709", valign="baseline", decimals=2,
               opacity=keys((r0 - 0.4, 0.0), (r0 + 0.2, 0.85), (13.6, 0.85), (14.0, 0.0)))
lat = f.add("text", dict(readout, text="", position=[760, H - BAR - 46], align="right", suffix="° N",
                         number=keys((r0, 41.38), (r1, 38.92, SOFT))), timing={"in": r0 - 0.4, "out": 14.0}, name="lat")
lon = f.add("text", dict(readout, text="", position=[800, H - BAR - 46], align="left", suffix="° W",
                         number=keys((r0, 31.06), (r1, 9.14, SOFT))), timing={"in": r0 - 0.4, "out": 14.0}, name="lon")
# Credits here are printed on the chart, in its ink.
S2 = shot(table, [lat, lon] + credit("TOMÁS ARDEN", W - 300, 330, 9.3, 12.6, align="right", color="#2b1709"))
T = f.add("transition", {"kind": "dissolve", "at": 7.8, "duration": 1.8, "grain": 520, "glow": 1.6}, {"a": T, "b": S2},
          name="t_burn")

# ------------------------------------------------------------------ 3  the chart, close (13.4–16.8 s)
# The same chart, three times closer, gliding: the shader and the type are
# evaluated at the new scale, so nothing softens.
close = f.add("transform", {"anchor": [600, 760], "position": keys((13.2, [CX + 120, CY + 40]), (17.6, [CX - 200, CY - 30],
                                                                                                    "linear")),
                            "scale": keys((13.2, 3.2), (17.6, 3.55, "linear")), "rotation": -8.0},
              {"source": chart}, timing={"in": 13.2, "out": 17.6}, name="chart_close")
near = f.add("blur", {"radius": 9}, {"source": close}, timing={"in": 13.2, "out": 17.6}, name="close_blur")
band = f.add("gradient", {"start": [0, 140], "end": [0, 940],
                          "stops": "0:#ffffffff, 0.36:#ffffff00, 0.62:#ffffff00, 1:#ffffffff"}, name="close_band")
near = f.add("mask", {"mode": "alpha"}, {"source": near, "matte": band}, timing={"in": 13.2, "out": 17.6},
             name="close_dof")
close = f.over(near, close, name="close_tiltshift")
close = f.blend(f.add("gradient", {"kind": "radial", "center": [CX - 200, CY], "radius": 1300, "space": "srgb",
                                   "stops": "0:#ffe9c8, 0.6:#b68a5c, 1:#1a0e06"}, name="close_light"),
                close, "multiply", name="close_lit")
S3 = shot(close, credit("IDA KESTREL", W - 260, 820, 14.0, 16.4, align="right", color="#2b1709"))
T = f.add("transition", {"kind": "crossfade", "at": 13.4, "duration": 1.0}, {"a": T, "b": S3}, name="t_close")

# ------------------------------------------------------------------ 4  the sea at night, and the world (16–21 s)
S4 = shot(parts["sea"], credit("JONAH LIRE", 190, 470, 16.8, 20.2) + credit("HALE OKAFOR", 190, 550, 17.2, 20.2))
T = f.add("transition", {"kind": "dissolve", "at": 15.8, "duration": 1.6, "grain": 700, "glow": 0.8,
                         "color": "#9cc4ff"}, {"a": T, "b": S4}, name="t_sea")

# ------------------------------------------------------------------ 5  the armillary sphere (20.2–24.6 s)
parts["armillary"] = build_armillary_rm(20.2, 24.8, chart)
S5 = shot(parts["armillary"], credit("SAOIRSE LIND", W - 180, 790, 21.0, 23.8, role="MUSIC BY", align="right"))
T = f.add("transition", {"kind": "dissolve", "at": 20.2, "duration": 1.4, "grain": 600, "glow": 1.2}, {"a": T, "b": S5},
          name="t_armillary")

# ------------------------------------------------------------------ 6  the light, and the title (23.3–32 s)
parts["light"] = light = build_light(23.1, 32.0)
TT = 25.0  # on the score's last chord
title_p = {"text": "MERIDIAN", "font": TITLE, "weight": 400, "size": 128, "align": "center", "valign": "baseline",
           "position": [CX - 60, CY + 92], "color": BONE, "stagger": 0.09, "stagger_order": "center",
           "tracking": keys((TT, 66.0), (32.0, 50.0, "linear")),
           "glyph_opacity": keys((TT, 0.0), (TT + 1.6, 1.0)),
           "glyph_offset": keys((TT, [0, 10], OUT), (TT + 2.0, [0, 0]))}
title = f.add("text", title_p, timing={"in": TT}, name="title")
# The beam passes across the letters: a hot copy of the title seen only
# through a soft band moving left to right.
hot = f.add("text", dict(title_p, color="#fff8e8"), timing={"in": TT}, name="title_hot")
band = f.add("gradient", {"kind": "linear", "start": keys((TT + 0.8, [-900, 0]), (TT + 3.4, [1900, 0], SOFT)),
                          "end": keys((TT + 0.8, [-300, 120]), (TT + 3.4, [2500, 120], SOFT)),
                          "stops": "0:#ffffff00, 0.5:#ffffffff, 1:#ffffff00"}, timing={"in": TT}, name="beam_band")
hot = f.add("mask", {"mode": "alpha"}, {"source": hot, "matte": band}, timing={"in": TT}, name="hot_mask")
hot = f.add("glow", {"radius": 12, "intensity": 0.7}, {"source": hot}, timing={"in": TT}, name="hot_glow")
rule = f.add("path", {"d": f"M {CX - 320} {CY + 144} L {CX + 200} {CY + 144}", "stroke": 1.0, "color": BONE,
                      "opacity": 0.7, "trim_start": keys((TT + 1.2, 0.5, SOFT), (TT + 2.6, 0.0)),
                      "trim_end": keys((TT + 1.2, 0.5, SOFT), (TT + 2.6, 1.0))}, timing={"in": TT + 1.2}, name="rule")
shade = f.add("gradient", {"kind": "radial", "center": [CX, CY + 60], "radius": 900,
                           "stops": "0:#00000088, 1:#00000000"}, timing={"in": TT - 0.5}, name="title_shade")
S6 = shot(light, [(shade, {"opacity": keys((TT - 0.5, 0.0), (TT + 1.0, 1.0))}, None), title, (hot, {"opacity": 1.0}, None),
                  rule] + credit("M. HALVORSEN", CX - 60, CY + 210, TT + 2.2, 31.6, role="CREATED BY", align="center",
                                 size=26, nid="creator"))
T = f.add("transition", {"kind": "crossfade", "at": 23.3, "duration": 1.6}, {"a": T, "b": S6}, name="t_light")
T = f.add("transition", {"kind": "dip", "at": 30.6, "duration": 2.8, "color": "#000000"}, {"a": T, "b": BLACK},
          name="t_out")

# ------------------------------------------------------------------ finish: grade, grain, the bars
comp = f.add("adjust", {"contrast": 1.06, "saturation": 0.92}, {"source": T}, name="grade")
comp = f.add("vignette", {"inner": "#00000000", "outer": "#00000090"}, {"source": comp}, name="vignette")
comp = f.add("aberration", {"amount": 0.35}, {"source": comp}, name="aberration")
comp = f.add("grain", {"amount": 0.05}, {"source": comp}, name="grain")
bars = f.add("path", {"d": f"M 0 0 H {W} V {BAR} H 0 Z M 0 {H - BAR} H {W} V {H} H 0 Z", "color": "#000000"},
             name="letterbox")
f.output = f.over(bars, comp, name="frame")

# ------------------------------------------------------------------ looks over raster 3D (comparison)
# The raster compass, raw and under the two built-in looks that hide what
# makes plain 3D look rudimentary.
if PART in ("raw3d", "film3d", "engrave3d"):
    raw = build_compass(3.4, 9.4)
    parts["raw3d"] = raw
    parts["film3d"] = f.add("film_look", {}, {"source": raw}, name="film")
    parts["engrave3d"] = f.add("engrave", {}, {"source": raw}, name="engraving")

# The film itself always goes to meridian.json (a live session may be
# playing it); a part preview goes to its own file.
if PART:
    f.output = parts[PART]
    print(f.save(os.path.join(D, f"meridian_{PART}.json")))
else:
    print(f.save(os.path.join(D, "meridian.json")))

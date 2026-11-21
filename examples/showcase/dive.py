"""DIVE — one endless zoom through everything ultrafast composes. Every world
holds the next one inside it, and the camera never stops falling: through
the full stop of a line of type, an iris of vector blades, the hole of a
raymarched chrome torus, the eye of a fluid vortex, a field of shader
contours and a vortex of type rings, to the wordmark, whose dot holds the
first frame again. The film loops seamlessly.

Every world is drawn in its own 1920x1080 frame; the next one is placed in
its portal, scaled so that it fills the frame exactly when the portal does.
The camera's zoom runs fast at every hand-over (the seams land mid-move)
and slows inside each world. Type, paths and shapes stay vector at any
zoom; the shader and the raymarch take the camera themselves, so they stay
sharp too. 1920x1080, 28 s, on an original 120 BPM score (ElevenLabs
Music): the drop lands on the torus, the final hit on the wordmark.

    python3 dive.py && ultrafast live $ULTRAFAST_SHOWCASE/dive/dive.json
"""

import json
import math
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "dive")
ULTRAFAST = os.path.join(HERE, "..", "..", "target", "release", "ultrafast")

FPS = 30
DUR = 29.0
f = Film(1920, 1080, fps=FPS, duration=DUR, motion_blur=(8, 0.4))
W, H = f.W, f.H
C0 = (W / 2, H / 2)

INK, BONE, GREY = "#08080a", "#f2f0ea", "#8b8b90"
# Two accents, used rarely and at full strength: a signal vermilion and a
# deep cobalt.
VERM, COBALT = "#ff4f2e", "#2f54ff"
WIDE, SANS, SERIF = "ultrafast:wide", "ultrafast:sans", "ultrafast:serif"
EM = {"em_font": SERIF, "em_style": "italic", "em_weight": 400, "em_color": BONE}

# Worlds, in order; world k plays as the main shot from T[k] to T[k+1].
T = [0.0, 4.0, 8.0, 12.0, 16.0, 20.0, 24.0, 29.0]
NW = 7
# The score's beat grid: its heartbeat pulse lands every 0.5 s from 50 ms
# (120 BPM, measured on the low band of the score).
BEAT0, BEAT = 0.05, 0.5
BEATS = [BEAT0 + BEAT * n for n in range(int(DUR / BEAT) + 2)]
# Hand-overs: the camera is the film's own, not the music's.
HO = list(T)
# A beat clock for drivers: steps one unit per beat, each step a quick
# ease-out right on the beat.
BEAT_U = f"((t - {BEAT0}) * {1 / BEAT:g})"
BEAT_CLOCK = f"(floor({BEAT_U}) + 1 - pow(1 - min(fract({BEAT_U}) / 0.45, 1), 3))"
BAR_PULSE = f"exp(-fract((t - {BEAT0}) / 2) * 2 / 0.22)"
BEAT_PULSE = f"exp(-fract({BEAT_U}) * {BEAT} / 0.12)"
# Sonar pings from the well, one per beat from the moment it shows in the
# ink's eye until the camera leaves it.
PINGS = [b for b in BEATS if 15.0 <= b < 19.9]
# A portal fills the frame when its radius reaches the half-diagonal (and a
# little more, so nothing of the outer world lingers in the corners).
RSTAR = 1135.0


def measure(params):
    out = subprocess.run([ULTRAFAST, "measure", json.dumps(params)], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def rot(v, deg):
    a = math.radians(deg)
    c, s = math.cos(a), math.sin(a)
    return (c * v[0] - s * v[1], s * v[0] + c * v[1])


def smooth(x):
    x = min(1.0, max(0.0, x))
    return x * x * (3 - 2 * x)


def ease_out_expo(x):
    x = min(1.0, max(0.0, x))
    return 1.0 if x >= 1 else 1 - 2 ** (-10 * x)


def frames(t0, t1):
    n = int(round((t1 - t0) * FPS))
    return [t0 + i / FPS for i in range(n + 1)]


def K(samples, ease="linear"):
    """Per-frame keys from (t, value) samples."""
    out = []
    for t, v in samples:
        out.append({"t": round(t, 4), "v": v if isinstance(v, list) else [v], "ease": ease})
    return {"keys": out}


# ---------------------------------------------------------------- the camera
class Portal:
    def __init__(self, P, r, theta):
        self.P, self.r, self.theta = P, r, theta


portals = {}
cams = {}  # k -> list of (t, Z, C, phi)
speed = []  # (t, e-folds per s), for fringes only in motion


# Zoom speed (e-folds per second) at each hand-over: the camera whips
# through every portal and settles inside each world; the wordmark arrives
# on the hit and brakes; the loop seam (0 = 29) dives hardest.
VB = [2.8, 2.2, 2.4, 2.2, 2.2, 2.3, 1.1, 2.8]
# How sharp the whips are (higher is a shorter, harder burst); the last
# world's exit is a long gathering ramp.
P_IN, P_OUT = 8, {6: 1}


def bump_mean(p):
    """Mean over one half of ((1 + cos 2 pi u) / 2) ** p."""
    return math.comb(2 * p, p) / 4 ** p / 2


def camera(k):
    p = portals[k]
    t0, t1 = T[k], T[k + 1]
    Dk = t1 - t0
    L = math.log(RSTAR / p.r)
    vs, ve = VB[k], VB[k + 1]
    pi, pe = P_IN, P_OUT.get(k, P_IN)
    bump_in = lambda u: ((1 + math.cos(2 * math.pi * u)) / 2) ** pi
    bump_out = lambda u: ((1 + math.cos(2 * math.pi * u)) / 2) ** pe
    m_in, m_out = bump_mean(pi), bump_mean(pe)
    vmid = (L / Dk - vs * m_in - ve * m_out) / (1 - m_in - m_out)
    assert vmid > 0, (k, vmid)
    v = lambda u: vmid + (vs - vmid) * (bump_in(u) if u < 0.5 else 0) + (ve - vmid) * (bump_out(u) if u >= 0.5 else 0)
    n = 4000
    acc, lnz = 0.0, [0.0]
    for i in range(n):
        acc += v((i + 0.5) / n) * Dk / n
        lnz.append(acc)
    scale = L / acc
    out = []
    for t in frames(t0, t1):
        u = (t - t0) / Dk
        lz = lnz[min(n, int(round(u * n)))] * scale
        lam = lz / L
        Z = math.exp(lz)
        phi = -p.theta * smooth(lam)
        g = 1 - smooth(lam)
        off = rot((p.P[0] - C0[0], p.P[1] - C0[1]), -phi)
        C = (p.P[0] - off[0] * g / Z, p.P[1] - off[1] * g / Z)
        out.append((t, Z, C, phi))
        speed.append((t, v(u) * scale))
    cams[k] = out
    return out


def cam_transform(src, k, name):
    c = cams[k]
    return f.add("transform", {"position": list(C0),
                               "anchor": K([(t, [C[0], C[1]]) for t, Z, C, phi in c]),
                               "scale": K([(t, Z) for t, Z, C, phi in c]),
                               "rotation": K([(t, phi) for t, Z, C, phi in c])},
                 {"source": src}, name=name)


def cam_at(k, t):
    """The camera of world k at time t (held outside its segment)."""
    c = cams[k]
    if t <= c[0][0]:
        return c[0]
    if t >= c[-1][0]:
        return c[-1]
    i = int((t - c[0][0]) * FPS)
    return c[min(i, len(c) - 1)]


# ---------------------------------------------------------------- helpers
def text(s, x, y, size, color=BONE, font=SANS, weight=400, name="text", **extra):
    p = {"text": s, "font": font, "size": size, "weight": weight, "color": color, "valign": "baseline",
         "position": [x, y]}
    p.update(extra)
    return f.add("text", p, name=name)


def cascade(node, t_in, stagger=0.018, rise=26):
    """Glyphs rise into place one after another."""
    p = f.nodes[node]["params"]
    p["glyph_opacity"] = keys((t_in, 0.0), (t_in + 0.45, 1.0, None))
    p["glyph_offset"] = keys((t_in, [0, rise], OUT), (t_in + 0.7, [0, 0]))
    p["stagger"] = stagger


def ground(color, name="ground"):
    """A world's background: a vast vector rect, so it stays infinite when
    the world is placed small inside another one (a solid ends at its frame)."""
    return f.add("shape", {"kind": "rect", "center": list(C0), "size": [60000, 60000], "color": color}, name=name)


def circle(c, r, color="#ffffff", name="disk", **extra):
    p = {"kind": "circle", "center": list(c), "size": [2 * r, 2 * r], "color": color}
    p.update(extra)
    return f.add("shape", p, name=name)


def ring_path(c, r):
    x, y = c
    return f"M {x - r:.2f} {y:.2f} A {r:.2f} {r:.2f} 0 1 0 {x + r:.2f} {y:.2f} A {r:.2f} {r:.2f} 0 1 0 {x - r:.2f} {y:.2f} Z"


def ticks_d(c, r1, r_minor, r_major, n, major_every=5, a0=0.0, span=360.0):
    """A scale of n ticks round c (absolute path data): minor ticks reach
    r_minor, every major_every-th reaches r_major."""
    out = []
    for i in range(n):
        ang = math.radians(a0 + span * i / n)
        r2 = r_major if i % major_every == 0 else r_minor
        out.append(f"M {c[0] + r1 * math.cos(ang):.2f} {c[1] + r1 * math.sin(ang):.2f} "
                   f"L {c[0] + r2 * math.cos(ang):.2f} {c[1] + r2 * math.sin(ang):.2f}")
    return " ".join(out)


def scale_ring(c, r, n, color, width, length=0.035, major_every=5, name="scale", **extra):
    """A hairline ring with a tick scale inside it, all proportional to r
    (so it reads the same at every zoom)."""
    d = ring_path(c, r) + " " + ticks_d(c, r, r * (1 - length), r * (1 - 2.2 * length), n, major_every)
    p = {"d": d, "stroke": width, "color": color}
    p.update(extra)
    return f.add("path", p, name=name)


def ring_text(c, rho, s, size, color, font=SANS, weight=400, style="normal", offset=0.0, name="ring_text",
              **extra):
    """A line of type round a circle, starting at offset px along it."""
    p = {"text": s, "font": font, "size": size, "weight": weight, "style": style, "color": color,
         "markup": False, "valign": "middle", "on_path": ring_path(c, rho), "path_offset": offset}
    p.update(extra)
    return f.add("text", p, name=name)


def stack(layers):
    cur = layers[0]
    for l in layers[1:]:
        cur = f.over(l, cur)
    return cur


def cam_of(k):
    if len(portals) < NW:
        return [(0.0, 1.0, C0, 0.0)]  # first pass: portals are still being laid out
    if k not in cams:
        camera(k)
    return cams[k]


def label_at(k, t, sx, sy, size):
    """Where to set a caption in world k so that at time t (as the camera
    slows) it sits at screen (sx, sy), size px, upright: before that it
    rides in small and close, after it flies past."""
    _, Z, C, phi = min(cam_of(k), key=lambda c: abs(c[0] - t))
    d = rot((sx - C0[0], sy - C0[1]), -phi)
    return (C[0] + d[0] / Z, C[1] + d[1] / Z), size / Z, -phi


def caption(k, s, t_hold, sx, sy, size=44, color=BONE, name="label", em=None, **extra):
    (x, y), sz, r = label_at(k, t_hold, sx, sy, size)
    node = text(s, x, y, sz, color, name=name, em_size=sz * 54 / 44, rotation=r, **{**EM, **(em or {})}, **extra)
    return node


# Surface textures, drawn by shaders that take the camera themselves so
# they stay sharp at any zoom. Each has a flat twin (the world at rest) for
# when its world is seen inside another's portal; that one fades out at
# the frame edge, where it ends. params: 0..3 camera (zoom, center x, y,
# roll), 7 edge fade, the rest per texture.
TEX_HEAD = """
float2 world(float2 pos) {
  float a = -radians(param(3));
  float2 d = (pos - size() * 0.5) / param(0);
  return float2(param(1), param(2)) + float2(cos(a) * d.x - sin(a) * d.y, sin(a) * d.x + cos(a) * d.y);
}
float edgefade(float2 wp) {
  return lerp(1.0, smoothstep(0.0, 220.0, min(min(wp.x, 1920.0 - wp.x), min(wp.y, 1080.0 - wp.y))), param(7));
}
"""

# A drafting grid that repeats at every scale: each level is five times
# finer than the last and shows only while its spacing resolves on screen,
# so there is always a grid, at every depth. 4 base spacing, 5 strength,
# 6 ink (0 bone lines, 1 ink lines).
GRAPH = TEX_HEAD + """
float4 shade(float2 pos, float px) {
  float2 wp = world(pos);
  float Z = param(0);
  float lz = log(Z) / log(5.0);
  float a = 0.0;
  for (int i = -1; i <= 3; i++) {
    float l = floor(lz) + float(i);
    float sp = param(4) * pow(5.0, -l);
    float spx = sp * Z;
    float vis = smoothstep(5.0, 24.0, spx) * (1.0 - smoothstep(700.0, 2600.0, spx));
    float2 g = abs(frac(wp / sp + 0.5) - 0.5) * spx;
    float ln = saturate(0.85 - min(g.x, g.y));
    float w = smoothstep(24.0, 320.0, spx);
    float plus = ((g.x < 0.75 && g.y < 6.0) || (g.y < 0.75 && g.x < 6.0)) ? 1.0 : 0.0;
    a = max(a, vis * max(ln * (0.3 + 0.7 * w), plus * w * 1.6));
  }
  a = saturate(a * param(5) * (1.0 + 0.9 * param(8)) * edgefade(wp));
  float3 c = lerp(float3(0.89, 0.87, 0.83), float3(0.0024, 0.0024, 0.003), param(6));
  return float4(c * a, a);
}
"""

# Spun metal: fine turned grooves round a center (several scales, each
# only where it resolves) lit by an anisotropic highlight that stays put on
# screen while the grooves turn under it. Added as light. 4,5 center,
# 6 inner radius, 8 outer radius, 9 strength, 10 light speed.
SPUN = TEX_HEAD + """
float4 shade(float2 pos, float px) {
  float2 wp = world(pos);
  float Z = param(0);
  float2 d = wp - float2(param(4), param(5));
  float r = length(d);
  float th = atan2(d.y, d.x) + radians(param(3));
  float g = 0.0, ws = 0.0;
  for (int i = 0; i < 4; i++) {
    float sp = 0.7 * pow(4.0, float(i));
    float vis = smoothstep(1.3, 4.0, sp * Z) / (1.0 + float(i));
    g += vis * snoise(float3(r / sp, float(i) * 7.1, 0.0));
    ws += vis;
  }
  g = g / max(ws, 1e-3) * 0.5 + 0.5;
  float L = time() * param(10) + 0.6 + param(11) * 0.42;
  float lobe = pow(abs(cos(th - L)), 14.0) + 0.5 * pow(abs(cos(th - L - 1.3)), 40.0);
  float edge = 1.5 / Z;
  float band = smoothstep(param(6) - edge, param(6), r) * (1.0 - smoothstep(param(8), param(8) + edge, r));
  float a = (0.12 + lobe) * (0.25 + 0.75 * g) * band * param(9) * edgefade(wp);
  return float4(float3(0.92, 0.91, 0.88) * a, a);
}
"""

# Paper: cloudy formation and fibres, multiplied into the sheet.
PAPER = TEX_HEAD + """
float4 shade(float2 pos, float px) {
  float2 wp = world(pos);
  float n = 0.5 * snoise(float3(wp / 110.0, 1.0)) + 0.3 * snoise(float3(wp / 27.0, 2.0)) + 0.2 * snoise(float3(wp / 7.0, 3.0));
  float fib = 0.0;
  for (int i = 0; i < 4; i++) {
    float an = float(i) * 1.7 + 0.4;
    float2 dir = float2(cos(an), sin(an));
    float2 q = float2(dot(wp, dir), dot(wp, float2(-dir.y, dir.x)));
    fib += smoothstep(0.5, 0.95, snoise(float3(q.x / 46.0, q.y / 1.9, float(i) * 3.3)));
  }
  float tone = 1.0 - 0.045 * n - 0.06 * fib;
  tone = lerp(1.0, tone, edgefade(wp));
  return float4(tone, tone, tone * 0.996, 1.0);
}
"""


def texture(k, key, code, params, mode="add", opacity=1.0):
    """A surface texture of world k: a shader instanced per shot with the
    camera that shows the world there. Grids flare on the bar; spun metal's
    highlight steps round on every beat."""
    if code is GRAPH:
        params = {"u8": {"expr": BAR_PULSE}, **params}
    if code is SPUN:
        params = {"u11": {"expr": BEAT_CLOCK}, **params}
    return ("tex", code, params, mode, opacity)


# A world is a list of layers from the back: ("world", id) layers live in
# the world's own frame and move with the camera; ("screen", world_id,
# screen_id) layers are rasters that also have a twin which takes the
# camera itself (so a shader stays sharp at any zoom). ("portal",) marks
# where the next world goes. Builders also set portals[k].
worlds = {}


# ================================================================ 0 · TYPE
def world0(sh):
    L = []
    L.append(("world", ground(INK, name="w0_bg")))
    head = {"text": "Every *frame*", "font": WIDE, "size": 120, "weight": 300, "em_size": 176, **EM}
    m = measure(head)
    r0 = 13.0
    gap = 12.0
    total = m["advance"] + gap + 2 * r0
    x0 = C0[0] - total / 2
    base = 590.0
    P = (x0 + m["advance"] + gap + r0, base - r0 - 1)
    portals[0] = Portal(P, r0, 0.0)
    L.append(texture(0, "w0_grid", GRAPH, {"u4": 600.0, "u5": 0.075}))
    # Specimen type set smaller and smaller round the full stop: grey
    # texture from afar, readable as the camera reaches each one.
    notes = [
        (P[0] + 36, P[1] + 62, 7.2, 250, "ultrafast composes every kind of pixel in one graph: type, vectors, 3D, "
         "simulations, shaders and footage. Every node stays live while the film plays, so an agent can change "
         "any part of any frame and see it on the next one."),
        (P[0] - 168, P[1] + 46, 4.0, 120, "This film is one document. Seven worlds sit inside each other, and the "
         "camera only ever moves forward."),
        (P[0] + 17, P[1] + 20, 1.6, 64, "Type stays type at any zoom: these words are drawn from their outlines, "
         "not from pixels."),
        (P[0] + 16, P[1] - 15, 0.62, 26, "You are now about eighty times closer than where you started."),
    ]
    for x, y, sz, wd, body in notes:
        L.append(("world", text(body, x, y, sz, "#f2f0ea66", width=wd, line_height=1.45, name="w0_note")))
    # Dashed rings in a geometric series around the full stop: the same at
    # every zoom, so the dive reads as endless.
    for n in range(1, 12):
        rr = r0 * 1.62 ** n
        circ = 2 * math.pi * rr
        L.append(("world", f.add("path", {"d": ring_path(P, rr), "stroke": max(0.6, rr * 0.0045),
                                          "color": "#f2f0ea1f" if n % 2 else "#f2f0ea12",
                                          "dash": [circ / 96 * 0.55, circ / 96 * 0.45],
                                          "dash_offset": {"expr": f"{circ / 96:.3f} * {(1 if n % 2 else -1) * 0.25} * {BEAT_CLOCK.replace('(t ', f'(t - {sh} ')}"}},
                                 name="w0_ring")))
        # Each ring is measured: a tick scale and its zoom factor, set at the
        # ring's own scale, so a new one becomes legible at every step in.
        if n >= 2:
            L.append(("world", f.add("path", {"d": ticks_d(P, rr * 1.035, rr * 1.06, rr * 1.10, 48, 4, a0=-90),
                                              "stroke": max(0.4, rr * 0.003), "color": "#f2f0ea30"},
                                     name="w0_ticks")))
            L.append(("world", ring_text(P, rr * 1.17, f"×{1.62 ** n:.1f}", rr * 0.055, "#f2f0ea5a",
                                         offset=2 * math.pi * rr * 1.17 * (0.13 + 0.07 * (n % 3)), name="w0_zoom")))
    # Typographic construction: baseline, x-height and cap height, and the
    # side bearings of every glyph, faint behind the line.
    size_ratio = head["size"] / 150.0
    xh, cap = 84.45 * size_ratio, 112.5 * size_ratio
    guides = []
    for y, label in ((base, "baseline"), (base - xh, "x-height"), (base - cap, "cap height")):
        guides.append(f"M -400 {y:.2f} L 2320 {y:.2f}")
        L.append(("world", text(label, 64, y - 7, 15, "#f2f0ea55", name="w0_guide_label")))
    for g in m["glyphs"]:
        gx = x0 + g["x"]
        guides.append(f"M {gx:.2f} {base + 26:.2f} L {gx:.2f} {base - cap - 26:.2f}")
    gx = x0 + m["advance"]
    guides.append(f"M {gx:.2f} {base + 26:.2f} L {gx:.2f} {base - cap - 26:.2f}")
    L.append(("world", f.add("path", {"d": " ".join(guides), "stroke": 0.45, "color": "#f2f0ea2a"},
                             name="w0_guides")))
    # A registration mark on the full stop.
    cross = (f"M {P[0] - r0 * 4.2:.2f} {P[1]:.2f} L {P[0] - r0 * 1.6:.2f} {P[1]:.2f} "
             f"M {P[0] + r0 * 1.6:.2f} {P[1]:.2f} L {P[0] + r0 * 4.2:.2f} {P[1]:.2f} "
             f"M {P[0]:.2f} {P[1] - r0 * 4.2:.2f} L {P[0]:.2f} {P[1] - r0 * 1.6:.2f} "
             f"M {P[0]:.2f} {P[1] + r0 * 1.6:.2f} L {P[0]:.2f} {P[1] + r0 * 4.2:.2f}")
    L.append(("world", f.add("path", {"d": cross, "stroke": 0.3, "color": "#f2f0ea99"}, name="w0_cross")))
    L.append(("world", circle(P, r0, VERM, name="w0_dot")))
    L.append(("portal", circle(P, r0 * 0.985, name="w0_portal")))
    L.append(("world", f.add("text", {**head, "color": BONE, "valign": "baseline", "position": [x0, base]},
                             name="w0_head")))
    return L


# ================================================================ 1 · VECTORS
def world1(sh):
    L = []
    P = (1190.0, 540.0)
    R = 372.0
    N = 9
    a_open = 132.0
    portals[1] = Portal(P, a_open, 32.0)
    L.append(("world", ground(INK, name="w1_bg")))
    t_a, t_b = 4.25 + sh, 6.1 + sh

    def aperture(t):
        return 5.0 + (a_open - 5.0) * ease_out_expo((t - t_a) / (t_b - t_a))

    def blade_pts(i, a):
        psi = -1.25 * a / R
        phi = 2 * math.pi * i / N + psi
        n = (math.cos(phi), math.sin(phi))
        d = (-math.sin(phi), math.cos(phi))
        T_ = (P[0] + a * n[0], P[1] + a * n[1])
        s = a * math.tan(math.pi / N)
        far = 2.4 * R
        v0 = (T_[0] - d[0] * s, T_[1] - d[1] * s)
        v1 = (T_[0] + d[0] * far, T_[1] + d[1] * far)
        v2 = (v1[0] + n[0] * far, v1[1] + n[1] * far)
        v3 = (v0[0] + n[0] * far, v0[1] + n[1] * far)
        return [round(c, 2) for v in (v0, v1, v2, v3) for c in v]

    def poly_pts(a):
        psi = -1.25 * a / R
        pts = []
        for i in range(N):
            phi = 2 * math.pi * i / N + psi
            rr = a / math.cos(math.pi / N)
            ang = phi - math.pi / N
            pts += [round(P[0] + rr * math.cos(ang), 2), round(P[1] + rr * math.sin(ang), 2)]
        return pts

    ts = [t_a - 1 / FPS] + frames(t_a, t_b)
    blades = []
    for i in range(N):
        blades.append(f.add("path", {"kind": "polygon_points",
                                     "points": K([(t, blade_pts(i, aperture(t))) for t in ts]),
                                     "color": "#17171b" if i % 2 else "#1c1c21"}, name="w1_blade"))
        # Machined hatching along each blade's leading edge.
        def hatch(a, i=i):
            psi = -1.25 * a / R
            phi = 2 * math.pi * i / N + psi
            n = (math.cos(phi), math.sin(phi))
            d = (-math.sin(phi), math.cos(phi))
            s = a * math.tan(math.pi / N)
            pts = []
            for j in range(1, 7):
                o = a + 9.0 * j
                T_ = (P[0] + o * n[0], P[1] + o * n[1])
                pts += [(T_[0] - d[0] * s, T_[1] - d[1] * s), (T_[0] + d[0] * 2.4 * R, T_[1] + d[1] * 2.4 * R)]
            return pts
        for j in range(6):
            blades.append(f.add("path", {"kind": "line",
                                         "points": K([(t, [round(c, 2) for v in hatch(aperture(t))[2 * j:2 * j + 2]
                                                           for c in v]) for t in ts]),
                                         "stroke": 0.6, "color": "#f2f0ea%02x" % (26 - 3 * j)}, name="w1_hatch"))
        blades.append(f.add("path", {"kind": "polygon_points",
                                     "points": K([(t, blade_pts(i, aperture(t))) for t in ts]),
                                     "stroke": 1.4, "join": "miter", "color": "#f2f0ea55"}, name="w1_edge"))
    iris = f.add("mask", {"mode": "alpha"}, {"source": stack(blades), "matte": circle(P, R, name="w1_clip")},
                 name="w1_iris")
    matte = f.add("path", {"kind": "polygon_points", "points": K([(t, poly_pts(aperture(t))) for t in ts]),
                           "color": "#ffffff"}, name="w1_aperture")
    lip = f.add("path", {"kind": "polygon_points", "points": K([(t, poly_pts(aperture(t))) for t in ts]),
                         "stroke": 1.6, "join": "miter", "color": "#f2f0eab0"}, name="w1_lip")
    # The blades lock open with a click: the aperture's edge flashes.
    lock = f.add("path", {"kind": "polygon_points", "points": K([(t, poly_pts(aperture(t))) for t in ts]),
                          "stroke": 2.6, "join": "miter", "color": BONE,
                          "opacity": keys((t_b - 0.03, 0.0), (t_b, 1.0, OUT), (t_b + 0.45, 0.0))},
                  timing={"in": t_b - 0.03, "out": t_b + 0.45}, name="w1_lock")
    # The lens barrel: hairline rings and a scale of ticks, turning as the
    # blades open.
    ticks = []
    for i in range(120):
        ang = 2 * math.pi * i / 120
        r1, r2 = R + 16, R + (34 if i % 10 == 0 else 24)
        ticks.append(f"M {r1 * math.cos(ang):.2f} {r1 * math.sin(ang):.2f} L {r2 * math.cos(ang):.2f} {r2 * math.sin(ang):.2f}")
    # Pivot rivets, one per blade, just inside the rim.
    rivets = []
    for i in range(N):
        ang = 2 * math.pi * (i + 0.5) / N
        c = (P[0] + (R - 30) * math.cos(ang), P[1] + (R - 30) * math.sin(ang))
        rivets += [circle(c, 7.5, "#25252b", name="w1_rivet"),
                   f.add("path", {"d": ring_path(c, 7.5), "stroke": 1.0, "color": "#f2f0ea70"}, name="w1_rivet_rim"),
                   circle(c, 1.6, "#f2f0eaa0", name="w1_rivet_pin")]
    turn = keys((t_a, 0.0, {"bezier": [0.3, 0, 0.2, 1]}), (t_b + 0.4, -40.0))
    circ_n = 2 * math.pi * (R + 80)
    stops = "1.4      2      2.8      4      5.6      8      11      16      22"
    knurl = []
    for i in range(240):
        ang = 2 * math.pi * i / 240
        knurl.append(f"M {(R + 108) * math.cos(ang):.2f} {(R + 108) * math.sin(ang):.2f} "
                     f"L {(R + 128) * math.cos(ang + 0.02):.2f} {(R + 128) * math.sin(ang + 0.02):.2f}")
    barrel = [
        f.add("path", {"d": ring_path(P, R + 3), "stroke": 2.0, "color": "#f2f0ea66"}, name="w1_rim"),
        f.add("path", {"d": ring_path(P, R + 9), "stroke": 0.7, "color": "#f2f0ea30"}, name="w1_rim2"),
        f.add("path", {"d": ring_path(P, R + 100), "stroke": 0.8, "color": "#f2f0ea30"}, name="w1_band"),
        f.add("path", {"d": " ".join(knurl), "stroke": 1.0, "color": "#f2f0ea24", "position": list(P),
                       "rotation": keys((t_a, 0.0, {"bezier": [0.3, 0, 0.2, 1]}), (t_b + 0.4, 25.0))},
              name="w1_knurl"),
        f.add("path", {"d": ring_path(P, R + 136), "stroke": 0.8, "color": "#f2f0ea30"}, name="w1_band2"),
        f.add("path", {"d": ring_path(P, R + 142), "stroke": 0.5, "color": "#f2f0ea1c"}, name="w1_band3"),
        ring_text(P, R + 80, stops, 19, "#f2f0ea9a", weight=500, tracking=1.0,
                  offset=keys((t_a, circ_n * 0.58, {"bezier": [0.3, 0, 0.2, 1]}), (t_b + 0.4, circ_n * (0.58 - 40 / 360))),
                  name="w1_stops"),
        ring_text(P, R + 55, "ultrafast optics  ·  nine blades  ·  every edge a curve  ·  every curve exact  ·  ",
                  12.5, "#f2f0ea66", tracking=2.0, offset=0.0, name="w1_engraving"),
        *rivets,
        f.add("path", {"d": " ".join(ticks), "stroke": 1.3, "color": "#f2f0ea55", "position": list(P),
                       "rotation": keys((t_a, 0.0, {"bezier": [0.3, 0, 0.2, 1]}), (t_b + 0.4, -40.0))}, name="w1_ticks"),
        f.add("path", {"d": ring_path(P, R + 65), "stroke": 0.8, "color": "#f2f0ea26"}, name="w1_ring"),
    ]
    # The outer barrel: a distance scale, a lime index, screws, more rings.
    circ_d = 2 * math.pi * (R + 168)
    dist = "∞      10      5      3      2      1.5      1      0.7      0.5 m"
    barrel += [
        f.add("path", {"d": ticks_d(P, R + 150, R + 156, R + 162, 180, 5), "stroke": 0.7, "color": "#f2f0ea40",
                       "position": [0, 0]}, name="w1_dist_ticks"),
        ring_text(P, R + 172, dist, 14, "#f2f0ea80", tracking=1.0, offset=circ_d * 0.62, name="w1_dist"),
        f.add("path", {"d": ring_path(P, R + 186), "stroke": 0.8, "color": "#f2f0ea2c"}, name="w1_band4"),
        f.add("path", {"d": ring_path(P, R + 214), "stroke": 1.4, "color": "#f2f0ea3a"}, name="w1_band5"),
        circle((P[0], P[1] - R - 92), 4.0, BONE, name="w1_index"),
        f.add("path", {"d": f"M {P[0]:.1f} {P[1] - R - 146:.1f} L {P[0]:.1f} {P[1] - R - 160:.1f}", "stroke": 2.0,
                       "color": BONE}, name="w1_index_line"),
    ]
    for i in range(8):
        ang = 2 * math.pi * (i + 0.25) / 8
        c = (P[0] + (R + 200) * math.cos(ang), P[1] + (R + 200) * math.sin(ang))
        slot = (math.cos(ang + 0.7) * 4.2, math.sin(ang + 0.7) * 4.2)
        barrel += [circle(c, 6.0, "#1d1d22", name="w1_screw"),
                   f.add("path", {"d": ring_path(c, 6.0), "stroke": 0.8, "color": "#f2f0ea66"}, name="w1_screw_rim"),
                   f.add("path", {"d": f"M {c[0] - slot[0]:.2f} {c[1] - slot[1]:.2f} L {c[0] + slot[0]:.2f} {c[1] + slot[1]:.2f}",
                                  "stroke": 1.1, "color": "#f2f0ea80"}, name="w1_screw_slot")]
    # Spun metal on the blades and the face of the barrel; a drafting grid
    # behind the lens.
    inner = K([(t, aperture(t) / math.cos(math.pi / N)) for t in ts])
    spun_blades = texture(1, "w1_spun_blades", SPUN, {"u4": P[0], "u5": P[1], "u6": inner, "u8": R,
                                                       "u9": 0.11, "u10": 0.35})
    spun_face = texture(1, "w1_spun_face", SPUN, {"u4": P[0], "u5": P[1], "u6": R + 3, "u8": R + 214,
                                                     "u9": 0.13, "u10": 0.35})
    label = caption(1, "vectors, *sharp at any zoom*", 5.7 + sh, 140, 560, name="w1_label")
    cascade(label, 4.4 + sh)
    L.insert(1, texture(1, "w1_grid", GRAPH, {"u4": 600.0, "u5": 0.06}))
    L.append(("portal", matte))
    L.append(("world", iris))
    L.append(spun_blades)
    L.append(spun_face)
    L.append(("world", lip))
    L.append(("world", lock))
    for b in barrel:
        L.append(("world", b))
    L.append(("world", label))
    return L


# ================================================================ 2 · 3D
FOV = 35.0
FPX = (H / 2) / math.tan(math.radians(FOV / 2))
TOR_R, TOR_r = 1.0, 0.32
HOLE = TOR_R - TOR_r


def world2(sh):
    L = []
    P = C0
    r2 = 128.0
    portals[2] = Portal(P, r2, 0.0)
    d0 = FPX * HOLE / r2
    L.append(("world", ground(INK, name="w2_bg")))
    L.append(("portal", circle(P, r2, name="w2_hole")))
    # The torus: its camera dollies so that the hole grows exactly as the
    # 2D camera zooms (the portal sits in the plane of the ring).
    dolly = [(T[2] - 1 / FPS, [0.0, 0.0, d0])] + [(t, [0.0, 0.0, d0 / Z]) for t, Z, C, phi in cam_of(2)]
    # A knurled chrome ring inside two gimbal hoops carrying ball bearings;
    # the hoops lie flat (outside the hole) by the time the camera passes.
    scene = """float2 map(float3 p) {
  float3 q = rotY(rotX(p, 1.5708 + knob(0)), knob(1));
  float ang = atan2(q.z, q.x);
  float tube = length(float2(length(q.xz) - %.3f, q.y));
  float knurl = 0.0045 * sin(ang * 140.0) * smoothstep(0.12, 0.3, abs(q.y) + 0.3 * (length(q.xz) - %.3f));
  float2 r = float2((tube - %.3f + knurl) * 0.8, 10.0);
  // Gimbal hoops: flat rings in the ring's plane, tipped by knob(0), turning.
  float3 g1 = rotY(rotX(p, 1.5708 + knob(0) * 1.5), knob(2));
  float3 g2 = rotY(rotX(rotY(p, knob(1) * 1.4), 1.5708 - knob(0) * 1.2), -knob(2) * 0.7);
  r = opU(r, float2(sdTorus(g1, 1.52, 0.022), 10.0));
  r = opU(r, float2(sdTorus(g2, 1.86, 0.016), 10.0));
  // Bearings riding the inner hoop.
  float a1 = atan2(g1.z, g1.x);
  float sec = 6.2831853 / 12.0;
  float a2 = (floor(a1 / sec) + 0.5) * sec;
  float3 bc = float3(cos(a2) * 1.52, 0.0, sin(a2) * 1.52);
  r = opU(r, float2(sdSphere(g1 - bc, 0.045), 6.0));
  return r;
}""" % (TOR_R, TOR_R, TOR_r)
    torus = once("torus", lambda: f.add("raymarch", {"scene": scene, "orbit": K(dolly), "target": [0, 0, 0], "fov": FOV,
                               "environment": "studio", "background": "transparent", "samples": 2,
                               "light": [3.0, 4.0, 6.0], "light_intensity": 52,
                               "exposure": keys((T[1], 0.35), (T[2] - 0.01, 0.35), (T[2] + 0.03, 1.15, OUT),
                                                (T[2] + 0.8, 0.35)),
                               "k0": keys((T[1], 0.62), (T[2] + 0.05, 0.62, OUT), (T[2] + 2.6, 0.0)),
                               "k1": keys((T[1], -0.5), (T[2] + 0.05, -0.5, OUT), (T[2] + 2.6, 0.0)),
                               "k2": {"expr": f"0.26 * {BEAT_CLOCK}"}},
                  timing={"in": T[1], "out": T[3]}, name="w2_torus"))
    L.append(("raster", torus, torus))
    # The drop: a lime shockwave leaves the ring.
    shock = f.add("path", {"d": ring_path(P, 200), "stroke": keys((T[2], 6.0, OUT), (T[2] + 0.9, 0.5)),
                           "color": COBALT, "scale": keys((T[2], 1.0, OUT), (T[2] + 0.9, 3.2)),
                           "opacity": keys((T[2], 0.9), (T[2] + 0.9, 0.0)), "position": list(P), "anchor": list(P)},
                  timing={"in": T[2], "out": T[2] + 0.9}, name="w2_shock")
    # An instrument dial behind the ring: scales, degree marks, spokes.
    ring_px = FPX * TOR_R / d0
    dial = []
    for j, (rr, n, col) in enumerate(((ring_px * 1.75, 72, "#f2f0ea2a"), (ring_px * 2.1, 180, "#f2f0ea1e"),
                                      (ring_px * 2.55, 360, "#f2f0ea14"), (ring_px * 3.3, 120, "#f2f0ea10"))):
        dial.append(("world", scale_ring(P, rr, n, col, max(0.5, rr * 0.0018), length=0.025, major_every=6 if n != 360 else 10,
                                      name="w2_dial")))
    degs = "   ".join(f"{d:03d}" for d in range(0, 360, 30)) + "   "
    dr = ring_px * 1.92
    dtrack = (2 * math.pi * dr - measure({"text": degs, "font": SANS, "size": 13})["advance"]) / len(degs)
    dial.append(("world", ring_text(P, dr, degs, 13, "#f2f0ea60", tracking=dtrack,
                                 offset={"expr": f"{2 * math.pi * ring_px * 1.92 * 0.004:.3f} * t"}, name="w2_degrees")))
    spokes = ticks_d(P, ring_px * 1.4, ring_px * 3.2, ring_px * 4.2, 24, 3)
    dial.append(("world", f.add("path", {"d": spokes, "stroke": 0.6, "color": "#f2f0ea0e"}, name="w2_spokes")))
    L[1:1] = [texture(2, "w2_grid", GRAPH, {"u4": 600.0, "u5": 0.055})] + dial
    label = caption(2, "real 3D, *raymarched*", 8.5, 1400, 1000, size=42, name="w2_label")
    cascade(label, T[2] + 0.12)
    # Drafting notes on the ring: a hole diameter, leaders to its specs.
    fade = keys((T[2] + 1.8, 1.0), (T[2] + 2.6, 0.0))
    hx, hy = P
    dim = (f"M {hx - r2:.1f} {hy:.1f} L {hx + r2:.1f} {hy:.1f} M {hx - r2:.1f} {hy - 6:.1f} L {hx - r2:.1f} {hy + 6:.1f} "
           f"M {hx + r2:.1f} {hy - 6:.1f} L {hx + r2:.1f} {hy + 6:.1f}")
    notes = [f.add("path", {"d": dim, "stroke": 0.35, "color": "#f2f0eaaa", "opacity": fade}, name="w2_dim"),
             text("ø 1.36", hx, hy - 7, 9, "#f2f0eacc", name="w2_dim_text", align="center", opacity=fade)]
    lead = [((-38, 262), (hx + 330, hy - 285), (hx + 520, hy - 285), "R 1.00   r 0.32", "knurled chrome, 140 ridges"),
            ((150, 262), (hx - 330, hy + 270), (hx - 540, hy + 270), "gimbal hoops ×2", "bearings ×12, black lacquer")]
    for (adeg, rad), (bx, by), (ex, ey), t1, t2 in lead:
        a_ = math.radians(adeg)
        sx_, sy_ = hx + rad * math.cos(a_), hy + rad * math.sin(a_)
        notes.append(f.add("path", {"d": f"M {sx_:.1f} {sy_:.1f} L {bx:.1f} {by:.1f} L {ex:.1f} {ey:.1f}",
                                    "stroke": 0.7, "color": "#f2f0ea66"}, name="w2_leader"))
        notes.append(circle((sx_, sy_), 2.2, "#f2f0eaaa", name="w2_leader_dot"))
        al = "left" if ex > bx else "right"
        tx = min(bx, ex) if al == "left" else max(bx, ex)
        notes.append(text(t1, tx, by - 7, 13, "#f2f0eacc", name="w2_spec", align=al))
        notes.append(text(t2, tx, by + 16, 11, "#f2f0ea80", name="w2_spec2", align=al))
    for nt in notes:
        L.append(("world", nt))
    L.append(("world", shock))
    L.append(("world", label))
    return L


# ================================================================ 3 · INK
def world3(sh):
    L = []
    P = (820.0, 560.0)
    r3 = 122.0
    portals[3] = Portal(P, r3, -40.0)
    L.append(("world", ground(BONE, name="w3_paper")))
    L.append(texture(3, "w3_fibre", PAPER, {}, mode="multiply"))
    L.append(texture(3, "w3_grid", GRAPH, {"u4": 600.0, "u5": 0.32, "u6": 1.0}, mode=None))
    # A plotted velocity field: a small arrow at every grid point, swirling
    # round the eye and drawn in toward it, faster closer in.
    arrows = []
    sp = 46.0
    for gy in range(-10, 34):
        for gx in range(-10, 52):
            x, y = gx * sp + 12, gy * sp + 8
            dx, dy = x - P[0], y - P[1]
            r = math.hypot(dx, dy)
            if r < r3 * 1.45 or r > 1500:
                continue
            tx, ty = -dy / r, dx / r
            ix, iy = -dx / r, -dy / r
            ux, uy = tx * 0.93 + ix * 0.36, ty * 0.93 + iy * 0.36
            n = math.hypot(ux, uy)
            ux, uy = ux / n, uy / n
            ln = 9 + 14 * min(1.0, 320 / r)
            x0_, y0_ = x - ux * ln / 2, y - uy * ln / 2
            x1_, y1_ = x + ux * ln / 2, y + uy * ln / 2
            hx, hy = -ux * 3.6, -uy * 3.6
            arrows.append(f"M {x0_:.1f} {y0_:.1f} L {x1_:.1f} {y1_:.1f} "
                          f"M {x1_ + hx - uy * 2.2:.1f} {y1_ + hy + ux * 2.2:.1f} L {x1_:.1f} {y1_:.1f} "
                          f"L {x1_ + hx + uy * 2.2:.1f} {y1_ + hy - ux * 2.2:.1f}")
    L.append(("world", f.add("path", {"d": " ".join(arrows), "stroke": 0.85, "color": "#08080a80", "cap": "round"},
                             name="w3_field")))
    # Measured rings round the eye, set at their own scale.
    for n in range(1, 9):
        rr = r3 * 1.5 ** n
        L.append(("world", scale_ring(P, rr, 72, "#08080a50", max(0.5, rr * 0.0026), length=0.02, major_every=6,
                                      name="w3_ring")))
        L.append(("world", ring_text(P, rr * 1.025, f"r {rr / r3:.2f}", rr * 0.03, "#08080aa0",
                                     offset=2 * math.pi * rr * 1.025 * (0.71 + 0.05 * n), name="w3_ring_label")))
    L.append(("portal", circle(P, r3, name="w3_eye")))
    def make_ink():
        em = []
        for i, (col, rad, orb, ph) in enumerate([(INK, 30, 300, 0.0), ("#1d1d22", 22, 410, 2.1), (COBALT, 13, 250, 4.2),
                                                  (INK, 18, 520, 1.0), ("#45454c", 11, 360, 3.3), (INK, 12, 610, 5.1),
                                                  (VERM, 6, 470, 0.6), (INK, 8, 700, 2.7)]):
            w = 1.05 if i % 2 == 0 else 0.8
            em.append(f.add("shape", {"kind": "circle", "size": [2 * rad, 2 * rad], "color": col,
                                      "center": {"expr": [f"{P[0]} + {orb} * cos({w} * t + {ph})",
                                                          f"{P[1]} + {orb * 0.82} * sin({w} * t + {ph})"]}},
                            name="w3_emitter"))
        stir = circle(P, r3 * 1.3, name="w3_stir")
        fluid = f.add("fluid", {"start": 1.0, "emit": 7.0, "dissipation": 0.02, "velocity_dissipation": 0.3,
                                "vorticity": 11.0, "turbulence": 700, "turbulence_scale": 170, "swirl": 2400,
                                "stir": 2600, "resolution": 0.5, "dye_resolution": 1.0, "seed": 3},
                      {"dye": stack(em), "stir": stir}, timing={"in": T[1], "out": T[4]}, name="w3_fluid")
        keep = f.add("shape", {"kind": "circle", "center": list(P), "size": [r3 * 2.5, r3 * 2.5], "color": "#ffffff",
                               "softness": r3 * 0.45}, name="w3_calm")
        calm = f.add("mask", {"mode": "alpha_inverted"}, {"source": fluid, "matte": keep}, name="w3_calm_ink")
        frame = f.add("shape", {"kind": "rect", "center": list(C0), "size": [W - 160, H - 160], "color": "#ffffff",
                                "softness": 150}, name="w3_frame")
        ink = f.add("mask", {"mode": "alpha"}, {"source": calm, "matte": frame}, name="w3_ink")
        return ink

    ink = once("ink", make_ink)
    # Marks drawn on the paper round the eye: a dashed guide, a fine scale,
    # registration ticks, a hand note.
    marks = []
    rg = r3 * 1.55
    marks.append(f.add("path", {"d": ring_path(P, rg), "stroke": 0.9, "color": "#08080a40",
                                "dash": [6, 7], "dash_offset": {"expr": f"-13 * {BEAT_CLOCK}"}}, name="w3_guide"))
    marks.append(scale_ring(P, r3 * 2.4, 120, "#08080a2a", 0.8, length=0.03, major_every=10, name="w3_scale"))
    marks.append(f.add("path", {"d": ticks_d(P, r3 * 1.12, r3 * 1.32, r3 * 1.32, 4, 1, a0=45), "stroke": 1.0,
                                "color": "#08080a70"}, name="w3_reg"))
    marks.append(ring_text(P, r3 * 2.62, "the eye of the vortex  ·  stable fluids, vorticity confinement  ·  ",
                           13, "#08080a80", font=SANS, tracking=1.5, offset=2 * math.pi * r3 * 2.62 * 0.62,
                           name="w3_note"))
    label = caption(3, "ink, *simulated*", 13.9, 1230, 930, color=INK, name="w3_label", em={"em_color": INK})
    cascade(label, T[3] + 0.2)
    for mk in marks:
        L.append(("world", mk))
    L.append(("world", ink))
    L.append(("world", label))
    return L


# ================================================================ 4 · SHADERS
CONTOURS = """
// Contour lines of a height field: log-radius rings round the portal (the
// same at every zoom) warped by noise further out. param 0..3 = camera
// (zoom, center x, y, roll in degrees), 4..6 = portal (x, y, radius).
float height(float2 w) {
  float2 P = float2(param(4), param(5));
  float r = length(w - P) / param(6);
  float far = smoothstep(1.6, 5.0, r);
  float n = snoise(float3(w / 520.0, time() * 0.07)) + 0.5 * snoise(float3(w / 230.0, 4.0 + time() * 0.11));
  return log(max(r, 1e-4)) * 10.0 + n * 8.0 * far;
}
float2 world(float2 pos) {
  float a = -radians(param(3));
  float2 d = (pos - size() * 0.5) / param(0);
  return float2(param(1), param(2)) + float2(cos(a) * d.x - sin(a) * d.y, sin(a) * d.x + cos(a) * d.y);
}
float4 shade(float2 pos, float px) {
  float v = height(world(pos));
  float vx = height(world(pos + float2(1.0, 0.0)));
  float vy = height(world(pos + float2(0.0, 1.0)));
  float g = max(length(float2(vx - v, vy - v)), 1e-4);
  float k = round(v);
  float dist = abs(v - k) / g;
  bool major = fmod(abs(k), 5.0) < 0.5;
  float w = major ? 1.35 : 0.85;
  float a = saturate(w + 0.5 - dist);
  float2 P = float2(param(4), param(5));
  float r = length(world(pos) - P) / param(6);
  // Bands of light run down the rings into the portal.
  float b = frac(k / 9.0 + param(9) * 0.25);
  float pulse = exp(-b * 9.0) * (1.0 - smoothstep(3.0, 9.0, r));
  float3 bone = float3(0.89, 0.87, 0.83);
  float3 acid = float3(0.66, 1.0, 0.04);
  float lvl = major ? 0.42 : 0.17;
  float3 cobalt = float3(0.03, 0.09, 1.0);
  float3 col = lerp(bone * lvl, cobalt * 1.6, pulse * 0.9);
  float3 ink = float3(0.0024, 0.0024, 0.003);
  float2 wp = world(pos);
  // Every other minor line is dotted (dots a fixed size on screen).
  float ang = atan2(wp.y - P.y, wp.x - P.x);
  float dotted = (!major && fmod(abs(k), 2.0) > 0.5) ? step(0.5, frac(ang * length(wp - P) * param(0) / 7.0)) : 1.0;
  a *= dotted;
  // Survey marks: a cross on a grid, fixed size on screen; a finer grid
  // takes over as the camera closes in.
  float3 markc = float3(0.0);
  float marka = 0.0;
  for (int lvl = 0; lvl < 2; lvl++) {
    float sp = lvl == 0 ? 120.0 : 24.0;
    float vis = lvl == 0 ? 1.0 : smoothstep(2.5, 5.0, param(0));
    float2 q = (wp - round(wp / sp) * sp) * param(0);
    float cr = (abs(q.x) < 0.6 && abs(q.y) < 4.5) || (abs(q.y) < 0.6 && abs(q.x) < 4.5) ? 1.0 : 0.0;
    marka = max(marka, cr * vis * 0.33);
  }
  float edge = smoothstep(0.0, 260.0, min(min(wp.x, 1920.0 - wp.x), min(wp.y, 1080.0 - wp.y)));
  a *= lerp(1.0, edge, param(7));
  marka *= lerp(1.0, edge, param(7)) * (r > 1.0 ? 1.0 : 0.0);
  // Relief: the height field lit from the upper left, with a faint step
  // between contour bands, so the field reads as terrain round a well.
  float2 gw = float2(vx - v, vy - v) * param(0);
  float3 nrm = normalize(float3(-gw * 7.0, 1.0));
  float lit = saturate(dot(nrm, normalize(float3(-0.55, -0.65, 0.6))));
  float shadeamt = (0.05 + 0.075 * (lit - 0.5) + 0.01 * step(0.5, frac(v / 10.0))) * (r > 1.0 ? 1.0 : 0.0)
                   * lerp(1.0, edge, param(7));
  ink = ink + bone * max(shadeamt, 0.0) * 0.6;
  float3 base = lerp(ink, col, a * (r > 1.0 ? 1.0 : 0.0));
  return float4(lerp(base, bone * 0.6, marka * (1.0 - a)), 1.0);
}
"""


def world4(sh):
    L = []
    P = (1150.0, 500.0)
    r4 = 112.0
    portals[4] = Portal(P, r4, 35.0)
    base = {"code": CONTOURS, "u4": P[0], "u5": P[1], "u6": r4, "u9": {"expr": BEAT_CLOCK}}
    L.append(("world", ground(INK, name="w4_bg")))
    L.append(("tex", CONTOURS, {k_: v_ for k_, v_ in base.items() if k_ != "code"}, None, 1.0))
    L.append(("portal", circle(P, r4, name="w4_well")))
    L.append(("world", f.add("path", {"d": ring_path(P, r4), "stroke": 1.4, "color": "#f2f0eab0"}, name="w4_lip")))
    # Sonar: a ring leaves the well on every beat, stronger on the bar; each
    # one is heard as a ping.
    for b in PINGS:
        bar = abs(((b - BEAT0) / 2) - round((b - BEAT0) / 2)) < 0.01
        grow = 5.5 if bar else 3.6
        L.append(("world", f.add("path", {"d": ring_path(P, r4), "color": COBALT,
                                          "stroke": keys((b, 1.8), (b + 1.4, 1.8 / grow, OUT)),
                                          "scale": keys((b, 1.0, OUT), (b + 1.4, grow)),
                                          "position": list(P), "anchor": list(P),
                                          "opacity": keys((b, 0.9 if bar else 0.45), (b + 1.4, 0.0, OUT))},
                                 timing={"in": b, "out": b + 1.4}, name="w4_ping")))
    # Depth soundings on the rings round the well, set at each ring's scale.
    for j in range(1, 22, 2):
        rr = r4 * math.exp(j / 10.0)
        L.append(("world", ring_text(P, rr, f"−{j * 10}", rr * 0.07, "#f2f0eab0" if j % 10 == 1 else "#f2f0ea80",
                                     offset=2 * math.pi * rr * (0.08 + 0.37 * ((j * 0.618) % 1)), name="w4_depth")))
    # Spot heights scattered over the field: a cross and a figure each.
    import random
    rnd = random.Random(4)
    spots, crosses = [], []
    while len(spots) < 70:
        x, y = rnd.uniform(-300, 2220), rnd.uniform(-250, 1330)
        r = math.hypot(x - P[0], y - P[1])
        if r < r4 * 1.6:
            continue
        sz = 8.5 * (0.6 + 0.8 * rnd.random())
        crosses.append(f"M {x - sz * 0.5:.1f} {y:.1f} L {x + sz * 0.5:.1f} {y:.1f} M {x:.1f} {y - sz * 0.5:.1f} L {x:.1f} {y + sz * 0.5:.1f}")
        spots.append(text(f"{rnd.randint(120, 980)}.{rnd.randint(0, 9)}", x + sz * 0.6, y - sz * 0.4, sz,
                          "#f2f0ea70", name="w4_spot"))
    L.append(("world", f.add("path", {"d": " ".join(crosses), "stroke": 0.6, "color": "#f2f0ea80"}, name="w4_cross")))
    for sp_ in spots:
        L.append(("world", sp_))
    label = caption(4, "shaders, *written live*", 17.9, 150, 900, name="w4_label")
    cascade(label, T[4] + 0.2)
    L.append(("world", label))
    return L


# ================================================================ 5 · TYPE RINGS
WORDS = ["type", "vectors", "masks", "3D", "glass", "chrome", "ink", "smoke", "shaders", "particles", "footage",
         "light", "grain", "paths", "glyphs", "fluids", "depth", "color"]


def world5(sh):
    L = []
    P = (930.0, 560.0)
    r5 = 104.0
    portals[5] = Portal(P, r5, -30.0)
    L.append(("world", ground(INK, name="w5_bg")))
    L.append(("portal", circle(P, r5, name="w5_core")))
    L.append(("world", f.add("path", {"d": ring_path(P, r5), "stroke": 1.3, "color": "#f2f0eab0"}, name="w5_lip")))
    L.append(("world", f.add("path", {"d": ticks_d(P, r5 * 1.08, r5 * 1.14, r5 * 1.2, 60, 5), "stroke": 0.8,
                                      "color": "#f2f0ea66"}, name="w5_lip_ticks")))
    L.append(("world", f.add("path", {"d": ticks_d(P, r5 * 1.3, 156 * 1.43 ** 9, 156 * 1.43 ** 9, 36, 1),
                                      "stroke": 0.5, "color": "#f2f0ea0c"}, name="w5_spokes")))
    styles = [(SANS, 400, 0.135, "#f2f0eab8", "normal"), (SERIF, 400, 0.19, BONE, "italic"),
              (WIDE, 300, 0.12, "#8b8b90", "normal")]
    for n in range(9):
        rho = 156 * 1.43 ** n
        font, weight, rel, col, style = styles[n % 3]
        if n == 4:
            col = VERM
        size = rho * rel
        circ = 2 * math.pi * rho
        words = WORDS[n * 5 % len(WORDS):] + WORDS[:n * 5 % len(WORDS)]
        s, i = "", 0
        probe = {"font": font, "size": size, "weight": weight, "style": style}
        while True:
            cand = s + words[i % len(words)] + "  ·  "
            if measure({**probe, "text": cand})["advance"] > circ * 0.97:
                break
            s, i = cand, i + 1
        adv = measure({**probe, "text": s})["advance"]
        tracking = (circ - adv) / max(1, len(s))
        omega = (0.16 + 0.02 * n) * (1 if n % 2 else -1)
        L.append(("world", f.add("text", {**probe, "text": s, "color": col, "tracking": tracking,
                                          "markup": False, "align": "left", "valign": "middle",
                                          "on_path": ring_path(P, rho),
                                          "path_offset": {"expr": f"{rho * omega:.3f} * (0.55 * t + 0.32 * {BEAT_CLOCK})"}},
                                 name="w5_ring")))
        hr = rho * 1.19
        L.append(("world", f.add("path", {"d": ring_path(P, hr) + " " + ticks_d(P, hr, hr * 0.985, hr * 0.97, 180, 5),
                                          "stroke": max(0.4, rho * 0.0025), "color": "#f2f0ea1c",
                                          "rotation": {"expr": f"{(-1 if n % 2 else 1) * 1.5} * {BEAT_CLOCK}"},
                                          "position": list(P), "anchor": list(P)}, name="w5_hair")))
        # A thread of fine print between the rings.
        mr = rho * 1.1
        msz = rho * 0.017
        mtxt = "one document  ·  seven worlds  ·  every pixel live  ·  "
        madv = measure({"text": mtxt, "font": SANS, "size": msz})["advance"]
        reps = max(1, int(2 * math.pi * mr / madv))
        mfull = mtxt * reps
        L.append(("world", ring_text(P, mr, mfull, msz, "#f2f0ea48",
                                     tracking=(2 * math.pi * mr - madv * reps) / len(mfull),
                                     offset={"expr": f"{mr * 0.05 * (1 if n % 2 else -1):.3f} * t"},
                                     name="w5_fine")))
        # Degree marks round each hairline, at its scale.
        dr = hr * 1.035
        dsz = rho * 0.024
        degs = "".join(f"{d}°" + " " * 9 for d in range(0, 360, 15))
        dadv = measure({"text": degs, "font": SANS, "size": dsz})["advance"]
        L.append(("world", ring_text(P, dr, degs, dsz, "#f2f0ea55", tracking=(2 * math.pi * dr - dadv) / len(degs),
                                     offset={"expr": f"{dr * math.radians(2.0) * (-1 if n % 2 else 1):.3f} * t"},
                                     name="w5_degrees")))
    L.insert(1, texture(5, "w5_spun", SPUN, {"u4": P[0], "u5": P[1], "u6": r5 * 1.25, "u8": 6000.0,
                                             "u9": 0.075, "u10": -0.25}))
    return L


# ================================================================ 6 · ULTRAFAST
def world6(sh):
    L = []
    L.append(("world", ground(INK, name="w6_bg")))
    mark = {"text": "ultrafast", "font": WIDE, "size": 150, "weight": 400}
    m = measure(mark)
    r6 = 19.0
    gap = 10.0
    total = m["advance"] + gap + 2 * r6
    x0 = C0[0] - total / 2
    base = 572.0
    P = (x0 + m["advance"] + gap + r6, base - r6)
    portals[6] = Portal(P, r6, 0.0)
    L.append(("world", circle(P, r6, VERM, name="w6_dot")))
    L.append(("portal", circle(P, r6 * 0.985, name="w6_portal")))
    L.append(("world", f.add("text", {**mark, "color": BONE, "valign": "baseline", "position": [x0, base]},
                             name="w6_mark")))
    # Print marks: crop and registration marks round the lockup, a hairline
    # baseline, a dashed orbit round the dot.
    deco = []
    bx0, bx1, by0, by1 = x0 - 120, x0 + total + 120, base - 230, base + 150
    cm = []
    for cx, cy, sx, sy in ((bx0, by0, -1, -1), (bx1, by0, 1, -1), (bx0, by1, -1, 1), (bx1, by1, 1, 1)):
        cm.append(f"M {cx:.1f} {cy + sy * 14:.1f} L {cx:.1f} {cy + sy * 44:.1f} M {cx + sx * 14:.1f} {cy:.1f} L {cx + sx * 44:.1f} {cy:.1f}")
    deco.append(f.add("path", {"d": " ".join(cm), "stroke": 0.9, "color": "#f2f0ea40"}, name="w6_crop"))
    deco.append(f.add("path", {"d": f"M {bx0 - 300:.1f} {base:.1f} L {bx1 + 300:.1f} {base:.1f}", "stroke": 0.6,
                               "color": "#f2f0ea18"}, name="w6_baseline"))
    deco.append(f.add("path", {"d": ring_path(P, r6 * 2.4), "stroke": 0.7, "color": "#f2f0ea50", "dash": [3, 4],
                               "dash_offset": {"expr": f"-3.5 * {BEAT_CLOCK}"}}, name="w6_orbit"))
    deco.append(f.add("path", {"d": ticks_d(P, r6 * 3.0, r6 * 3.25, r6 * 3.5, 48, 4), "stroke": 0.6,
                               "color": "#f2f0ea40"}, name="w6_dot_scale"))
    # The proof's slug: what this is, a running frame counter, swatches.
    deco.append(text("dive   ·   one document, seven worlds   ·   1920 × 1080   ·   30 fps", bx0, by1 + 34, 13,
                     "#f2f0ea70", name="w6_slug"))
    counter = text("", bx1, by1 + 34, 13, "#f2f0ea90", name="w6_counter", align="right",
                   number=keys((T[6], T[6] * FPS, "linear"), (T[7], T[7] * FPS)), prefix="frame ", decimals=0)
    deco.append(counter)
    for j, col in enumerate((INK, "#3a3a40", GREY, BONE, VERM, COBALT)):
        cx = bx1 - 200 + j * 22
        deco.append(f.add("shape", {"kind": "rect", "center": [cx, by1 + 62], "size": [16, 16], "color": col},
                          name="w6_swatch"))
        deco.append(f.add("path", {"d": f"M {cx - 8} {by1 + 54} L {cx + 8} {by1 + 54} L {cx + 8} {by1 + 70} L {cx - 8} {by1 + 70} Z",
                                   "stroke": 0.6, "color": "#f2f0ea50"}, name="w6_swatch_rim"))
    # A ruler under the proof and registration targets at its sides.
    rl = []
    for i in range(0, 241):
        x = bx0 + i * (bx1 - bx0) / 240
        h = 14 if i % 20 == 0 else (8 if i % 5 == 0 else 4)
        rl.append(f"M {x:.1f} {by1 + 96:.1f} L {x:.1f} {by1 + 96 + h:.1f}")
        if i % 20 == 0:
            deco.append(text(str(i // 2), x + 2, by1 + 122, 8, "#f2f0ea60", name="w6_ruler_n"))
    rl.append(f"M {bx0:.1f} {by1 + 96:.1f} L {bx1:.1f} {by1 + 96:.1f}")
    deco.append(f.add("path", {"d": " ".join(rl), "stroke": 0.6, "color": "#f2f0ea50"}, name="w6_ruler"))
    for cx, cy in ((bx0 - 70, (by0 + by1) / 2), (bx1 + 70, (by0 + by1) / 2), ((bx0 + bx1) / 2, by0 - 60)):
        deco.append(f.add("path", {"d": ring_path((cx, cy), 11) + " " + ring_path((cx, cy), 5) +
                                   f" M {cx - 18:.1f} {cy:.1f} L {cx + 18:.1f} {cy:.1f} M {cx:.1f} {cy - 18:.1f} L {cx:.1f} {cy + 18:.1f}",
                                   "stroke": 0.6, "color": "#f2f0ea60"}, name="w6_target"))
    # Small print round the dot, readable only as the camera dives in.
    deco.append(text("the dot holds the first frame of the film", P[0] + r6 * 1.3, P[1] + r6 * 1.9, 3.2,
                     "#f2f0ea80", name="w6_fine"))
    deco.append(text("and back to the start.", P[0] + r6 * 1.08, P[1] - r6 * 1.12, 1.1, "#f2f0eacc",
                     name="w6_finer"))
    for d_ in deco:
        L.insert(1, ("world", d_))
    L.insert(1, texture(6, "w6_grid", GRAPH, {"u4": 600.0, "u5": 0.065}))
    tag = text("one live document, *all the way down*", x0 + 8, 652, 30, "#f2f0eacc", name="w6_tag",
               em_size=37, **EM)
    cascade(tag, T[6] + 0.3, stagger=0.012, rise=16)
    L.append(("world", tag))
    return L


BUILDERS = [world0, world1, world2, world3, world4, world5, world6]


# ---------------------------------------------------------------- nesting
memo = {}
shared = {}


def once(key, make):
    if key not in shared:
        shared[key] = make()
    return shared[key]


def world_layers(k, sh, inst=None):
    """A world's layers. Each nesting builds its own copies of the vector
    layers (a node used twice is kept as a frame-sized texture, which
    would end the world at its frame edge); rasters are shared (once)."""
    key = (k, sh, inst)
    if key not in worlds:
        worlds[key] = BUILDERS[k](sh)
    return worlds[key]


def inner_of(k, sh):
    """The world inside world k's portal, and its time shift (the first
    world comes round again, 28 s later)."""
    return ((k + 1) % NW, sh + (DUR if k == NW - 1 else 0.0))


def inner_opacity(k, sh):
    """Dot portals start as a solid lime dot; what is inside shows through
    as the dot grows."""
    if k not in (0, 6):
        return None
    if sh:
        return 0.0
    p = portals[k]
    ks = []
    for t, Z, C, phi in cams[k]:
        rpx = p.r * Z
        ks.append((t, smooth((rpx - 16) / 55)))
    return K(ks)


def compose(comp, k):
    """The camera that shows world k's inner world, given the camera comp
    that shows world k: the inner world is placed in k's portal."""
    p = portals[k]
    sigma = p.r / RSTAR
    out = []
    for t, Z, C, phi in comp:
        d = rot((p.P[0] - C[0], p.P[1] - C[1]), -p.theta)
        out.append((t, Z * sigma, (C0[0] - d[0] / sigma, C0[1] - d[1] / sigma), phi + p.theta))
    return out


def comp_transform(src, comp, name):
    return f.add("transform", {"position": list(C0),
                               "anchor": K([(t, [C[0], C[1]]) for t, Z, C, phi in comp]),
                               "scale": K([(t, Z) for t, Z, C, phi in comp]),
                               "rotation": K([(t, phi) for t, Z, C, phi in comp])},
                 {"source": src}, name=name)


def put(fg, bg, mode=None, opacity=1.0):
    if bg is None:
        return fg
    if mode is None:
        return f.over(fg, bg, **({"opacity": opacity} if opacity != 1.0 else {}))
    return f.blend(fg, bg, mode, opacity=opacity)


def scene(k, depth, sh, comp, shot, window):
    """World k as it appears on screen in a shot, seen through the camera
    comp, with depth levels of worlds inside its portal. Everything is built
    in screen space: vector layers under the camera (drawn from their
    outlines at any zoom), textures as shaders that take the camera
    themselves, so a world never ends at a frame edge."""
    main = k == shot and depth == 2
    layers = world_layers(k, sh, (shot, depth))
    res, run = None, []

    def flush():
        nonlocal res, run
        if run:
            res = put(comp_transform(stack(run), comp, f"cam{k}"), res)
            run = []

    for layer in layers:
        kind = layer[0]
        if kind == "world":
            run.append(layer[1])
        elif kind == "tex":
            _, code, params, mode, opacity = layer
            flush()
            node = f.add("shader", {"code": code, "u0": K([(t, Z) for t, Z, C, phi in comp]),
                                    "u1": K([(t, C[0]) for t, Z, C, phi in comp]),
                                    "u2": K([(t, C[1]) for t, Z, C, phi in comp]),
                                    "u3": K([(t, phi) for t, Z, C, phi in comp]), "u7": 0.0, **params},
                         timing=window, name=f"w{k}_tex")
            res = put(node, res, mode, opacity)
        elif kind == "raster":
            if depth == 0 and k == 2:
                # Too small to need the real torus: a ring stands in.
                rr = FPX * TOR_R / (FPX * HOLE / portals[2].r)
                run.append(f.add("path", {"d": ring_path(portals[2].P, rr), "stroke": rr * 2 * TOR_r / TOR_R,
                                          "color": "#9a9aa0"}, name="w2_standin"))
                continue
            flush()
            node = layer[2] if main else comp_transform(layer[1], comp, f"w{k}_placed")
            res = put(node, res)
        elif kind == "portal":
            if depth == 0:
                continue
            flush()
            kk, ssh = inner_of(k, sh)
            inner = scene(kk, depth - 1, ssh, compose(comp, k), shot, window)
            p = portals[k]
            matte = layer[1] if len(layer) > 1 else circle(p.P, p.r, name=f"w{k}_portal")
            node = f.add("mask", {"mode": "alpha"}, {"source": inner, "matte": comp_transform(matte, comp, f"w{k}_matte")},
                         name=f"w{k}_window")
            op = inner_opacity(k, sh)
            if op is not None:
                node = f.add("opacity", {"opacity": op}, {"source": node}, name=f"w{k}_reveal")
            res = put(node, res)
    flush()
    return res


# Portals first (cameras depend on every portal), then the film.
for k in range(NW):
    world_layers(k, 0.0)
shared.clear()  # rasters built in the first pass saw no camera yet
for k in range(NW):
    if k not in cams:
        camera(k)

RIPPLE = """
// A water ripple: one ring wave running out from param 1,2 from time param 3,
// amplitude param 0 px, fading as it spreads.
float4 shade(float2 pos, float px) {
  float2 c = float2(param(1), param(2));
  float2 d = pos - c;
  float r = length(d);
  float age = max(time() - param(3), 0.0);
  float front = age * 950.0;
  float w = exp(-pow((r - front) / 110.0, 2.0)) * param(0) * exp(-age * 2.2);
  float off = sin((r - front) / 15.0) * w;
  return input(0, pos + d / max(r, 1.0) * off);
}
"""

out = f.add("solid", {"color": INK}, name="black")
for k in range(NW):
    # The first shot also covers the shutter before 0, so frame 0 is not
    # half black under motion blur.
    window = {"in": HO[k] if k else -1.0, "out": HO[k + 1]}
    sc = scene(k, 2, 0.0, cams[k], k, window)
    if k == 3:
        # Breaking into the ink world: the picture ripples out from where
        # the camera hit the water.
        sc = f.add("shader", {"code": RIPPLE, "u0": 11.0, "u1": C0[0], "u2": C0[1], "u3": T[3]}, {"in0": sc},
                   timing={"in": T[3], "out": T[3] + 1.1}, name="w3_ripple")
    out = f.over(sc, out, timing=window, name=f"shot{k}")

# ---------------------------------------------------------------- finish
CRT = """
// A subtle CRT: a hair of glass curvature, radial chromatic fringes (param 0
// px at the corners: a little always, more while the camera rushes), soft
// scan lines (param 1), an aperture grille (param 2), a slow roll band,
// flicker and a vignette.
float4 shade(float2 pos, float px) {
  float2 sz = size();
  float2 uv = pos / sz * 2.0 - 1.0;
  uv *= 0.988 * (1.0 + float2(0.016, 0.026) * float2(uv.y * uv.y, uv.x * uv.x));
  float2 p = (uv * 0.5 + 0.5) * sz;
  float2 off = uv * param(0) * 0.7;
  float4 c = input(0, p);
  float r = input(0, p + off).r;
  float b = input(0, p - off).b;
  float3 col = float3(r, c.g, b);
  float sl = 0.5 + 0.5 * cos(pos.y * 6.2831853 / 4.0);
  col *= 1.0 - param(1) * sl;
  float m = fmod(floor(pos.x), 3.0);
  float k = param(2);
  col *= m < 1.0 ? float3(1.0, 1.0 - k, 1.0 - k) : (m < 2.0 ? float3(1.0 - k, 1.0, 1.0 - k) : float3(1.0 - k, 1.0 - k, 1.0));
  col *= 1.0 / (1.0 - 0.5 * param(1) - 0.66 * k);
  float roll = frac(time() * 0.11) * (sz.y + 500.0) - 250.0;
  col *= 1.0 + 0.03 * smoothstep(180.0, 0.0, abs(pos.y - roll));
  col *= 1.0 + 0.012 * sin(time() * 113.0) + 0.006 * sin(time() * 7.3);
  float vig = 1.0 - 0.32 * pow(dot(uv * float2(0.85, 1.0), uv * float2(0.85, 1.0)) * 0.5, 1.4);
  float2 e = abs(uv);
  float corner = smoothstep(1.0, 0.985, max(e.x, e.y));
  return float4(col * vig * corner, 1.0);
}
"""

speed.sort()
fr = []
seen = set()
for t, v in speed:
    if round(t, 4) in seen:
        continue
    seen.add(round(t, 4))
    bp = math.exp(-(((t - BEAT0) / BEAT) % 1) * BEAT / 0.12)
    fr.append((t, 3.2 + 2.6 * bp + min(13.0, 1.8 * max(0.0, v - 0.4))))
out = f.add("bloom", {"threshold": 0.82, "radius": 28, "intensity": 0.32, "wide": 0.2}, {"source": out}, name="bloom")
out = f.add("shader", {"code": CRT, "u0": K(fr), "u1": 0.075, "u2": 0.05}, {"in0": out}, name="crt")
out = f.add("grain", {"amount": 0.055}, {"source": out}, name="grain")
f.output = out
# The sound: an ambient score and a sound design cued to the picture, mixed
# by dive_sound.py from the cue sheet this script writes.
f.add("audio", {"src": "audio/dive_mix.wav", "gain": 1.0}, timing={"in": 0.0, "out": DUR}, name="sound")


def write_cues(path):
    """Everything the sound design needs from the picture: world times,
    portals, every camera frame, zoom speed and the timed events."""
    cues = {"T": T, "DUR": DUR, "FPS": FPS, "BEAT0": BEAT0, "BEAT": BEAT, "RSTAR": RSTAR, "C0": C0,
            "portals": {k: [p.P[0], p.P[1], p.r, p.theta] for k, p in portals.items()},
            "cams": {k: [[t, Z, C[0], C[1], phi] for t, Z, C, phi in c] for k, c in cams.items()},
            "speed": sorted(speed),
            "iris": {"t_a": 4.25, "t_b": 6.1, "R": 372.0, "N": 9, "ticks_turn": -40.0, "knurl_turn": 25.0,
                     "turn_end": 6.5},
            "shock": T[2], "ripple": T[3], "pings": PINGS,
            "cascades": {"w1": 4.4, "w2": T[2] + 0.12, "w3": T[3] + 0.2, "w4": T[4] + 0.2, "w6": T[6] + 0.3},
            "w3_emitters": [[300, 0.0, 1.05], [410, 2.1, 0.8], [250, 4.2, 1.05], [520, 1.0, 0.8], [360, 3.3, 1.05],
                            [610, 5.1, 0.8], [470, 0.6, 1.05], [700, 2.7, 0.8]]}
    with open(path, "w") as fh:
        json.dump(cues, fh)

if __name__ == "__main__":
    os.makedirs(D, exist_ok=True)
    path = f.save(os.path.join(D, "dive.json"))
    write_cues(os.path.join(D, "audio", "dive_cues.json"))
    print(path, len(f.nodes), "nodes")

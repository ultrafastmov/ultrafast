"""DIVE: the sound. An ambient score (ElevenLabs Music) under a sound
design cued frame by frame to the picture from the cue sheet dive.py writes
(world times, portals, every camera frame, zoom speed and timed events).
The effects are Mixkit sounds (Mixkit Sound Effects Free License).

What is heard is what is on screen:
- every world has a bed of its own, heard first through the portal that
  holds it (quiet, muffled, narrow) and opening up as the portal grows;
  the world you leave falls away behind you;
- every pass through a portal is a rush of air, every hand-over its own
  layered moment: the drop on the chrome ring, the plunge into ink, the
  sonar well, the vault of type, the final hit on the wordmark;
- things that move make their sound in time and in place: the iris motor
  and every tick of its barrel passing the index, the lock, the gimbal
  hoops stepping, the ink's bubbles from each orbiting emitter, sonar
  pings from the well on the beat, the dial clicks of the type rings, the
  slug printing and the frame counter;
- tonal sounds are tuned to the score's key (D); the screen's own hum and
  static follow its chromatic fringes;
- the mix is circular: tails and beds wrap round the loop seam, so the
  sound loops as seamlessly as the picture.

    python3 dive.py && python3 dive_sound.py
"""

import json
import math
import os
import random
import subprocess
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "tools", "sound"))
from audiolib import SR, load, mono, rms_db, save, spectrogram_png  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
A = os.path.join(ROOT, "dive", "audio")
MK = os.path.join(A, "mixkit")
SCORE = os.path.join(A, "score2", "take-2.mp3")

cues = json.load(open(os.path.join(A, "dive_cues.json")))
T, DUR, FPS = cues["T"], cues["DUR"], cues["FPS"]
BEAT0, BEAT = cues["BEAT0"], cues["BEAT"]
C0 = cues["C0"]
N = int(round(DUR * SR))
portals = {int(k): v for k, v in cues["portals"].items()}
cams = {int(k): np.array(v) for k, v in cues["cams"].items()}
speed = np.array(cues["speed"])
man = json.load(open(os.path.join(MK, "manifest.json")))
rng = random.Random(7)


def log(m):
    print(m, flush=True)


# ------------------------------------------------------------ DSP (numpy)
def db(x):
    return 10.0 ** (x / 20.0)


def stereo(x):
    return np.stack([x, x], axis=1) if x.ndim == 1 else x


def spectral(x, resp):
    """Zero-phase filter by a magnitude response resp(freqs)."""
    x = stereo(x)
    n = len(x)
    nf = 1 << int(math.ceil(math.log2(max(n, 2))))
    f = np.fft.rfftfreq(nf, 1 / SR)
    H = resp(f)
    out = np.empty_like(x)
    for c in range(2):
        out[:, c] = np.fft.irfft(np.fft.rfft(x[:, c], nf) * H, nf)[:n]
    return out


def lowpass(x, fc, order=2):
    return spectral(x, lambda f: 1 / np.sqrt(1 + (f / fc) ** (2 * order)))


def highpass(x, fc, order=2):
    return spectral(x, lambda f: 1 / np.sqrt(1 + (fc / np.maximum(f, 1e-3)) ** (2 * order)))


def pitch(x, semis):
    """Resample: pitch and length change together (a tape-speed shift)."""
    if abs(semis) < 1e-3:
        return x
    r = 2 ** (semis / 12)
    x = stereo(x)
    n = int(len(x) / r)
    src = np.arange(n) * r
    return np.stack([np.interp(src, np.arange(len(x)), x[:, c]) for c in range(2)], axis=1)


def fade(x, fin=0.005, fout=0.02):
    x = stereo(x).copy()
    a, b = int(fin * SR), int(fout * SR)
    if a:
        x[:a] *= np.linspace(0, 1, a)[:, None] ** 2
    if b:
        x[-b:] *= np.linspace(1, 0, b)[:, None] ** 2
    return x


def trim(x, start, length=None):
    x = stereo(x)
    s = int(start * SR)
    e = len(x) if length is None else s + int(length * SR)
    return x[max(0, s):e]


def pan(x, p, width=1.0):
    """Equal-power pan of a source (-1 left .. 1 right); its own stereo
    width scaled by width (0 = a point)."""
    x = stereo(x)
    m = (x[:, 0] + x[:, 1]) / 2
    s = (x[:, 0] - x[:, 1]) / 2 * width
    a = (p + 1) * math.pi / 4
    return np.stack([m * math.cos(a) * math.sqrt(2) + s, m * math.sin(a) * math.sqrt(2) - s], axis=1)


def normalize(x, target_db):
    """Scale so the loudest 50 ms sits at target_db (RMS)."""
    m = mono(stereo(x))
    n = int(0.05 * SR)
    k = max(1, len(m) // n)
    r = np.sqrt((m[: k * n].reshape(k, n) ** 2).mean(axis=1)).max() if len(m) >= n else np.sqrt((m ** 2).mean())
    return stereo(x) * (db(target_db) / max(r, 1e-9))


def reverse(x):
    return stereo(x)[::-1].copy()


def loopify(x, n, xf=1.0):
    """A bed exactly n samples long that loops without a seam."""
    x = stereo(x)
    k = int(xf * SR)
    need = n + k
    reps = int(math.ceil(need / len(x)))
    y = np.concatenate([x] * reps)[:need].copy()
    w = np.linspace(0, 1, k)[:, None]
    y[:k] = y[:k] * w + y[n:n + k] * (1 - w)
    return y[:n]


def tv_filter(x, fc_of_t, width_of_t=None, nfft=2048, hop=512):
    """Time-varying low-pass (cutoff fc_of_t(film seconds)) and stereo width
    by STFT overlap-add, for sounds heard through a portal."""
    x = stereo(x)
    n = len(x)
    win = np.hanning(nfft)
    pad = np.pad(x, ((nfft, nfft), (0, 0)), mode="wrap")
    frames = (len(pad) - nfft) // hop
    f = np.fft.rfftfreq(nfft, 1 / SR)
    out = np.zeros_like(pad)
    norm = np.zeros(len(pad))
    for i in range(frames):
        s = i * hop
        t = (s + nfft / 2 - nfft) / SR
        fc = fc_of_t(t)
        H = 1 / np.sqrt(1 + (f / fc) ** 4)
        seg = pad[s:s + nfft] * win[:, None]
        if width_of_t is not None:
            w = width_of_t(t)
            m = (seg[:, 0] + seg[:, 1]) / 2
            sd = (seg[:, 0] - seg[:, 1]) / 2 * w
            seg = np.stack([m + sd, m - sd], axis=1)
        for c in range(2):
            out[s:s + nfft, c] += np.fft.irfft(np.fft.rfft(seg[:, c]) * H, nfft) * win
        norm[s:s + nfft] += win ** 2
    norm[norm < 1e-6] = 1
    return (out / norm[:, None])[nfft:nfft + n]


def ir(seconds, decay, brightness, predelay=0.012, seed=1):
    """A diffuse stereo reverb impulse response: decorrelated noise with an
    exponential decay that darkens as it goes (high bands die first)."""
    r = np.random.default_rng(seed)
    n = int(seconds * SR)
    t = np.arange(n) / SR
    out = np.zeros((n, 2))
    edges = [(20, 300, 1.25), (300, 2000, 1.0), (2000, 6000, 0.6), (6000, 18000, 0.32 * brightness)]
    for lo, hi, k in edges:
        for c in range(2):
            noise = r.standard_normal(n)
            band = np.fft.irfft(np.fft.rfft(noise) * ((np.fft.rfftfreq(n, 1 / SR) >= lo) &
                                                      (np.fft.rfftfreq(n, 1 / SR) < hi)), n)
            out[:, c] += band * np.exp(-t / (decay * k / 6.9))
    pd = int(predelay * SR)
    out = np.concatenate([np.zeros((pd, 2)), out])[:n]
    return out / np.sqrt((out ** 2).sum(axis=0).mean())


def circ_convolve(x, h):
    """Circular convolution over the film's length: reverb tails wrap round
    the loop seam."""
    out = np.empty_like(x)
    H = [np.fft.rfft(h[:, c], len(x)) for c in range(2)]
    for c in range(2):
        out[:, c] = np.fft.irfft(np.fft.rfft(x[:, c]) * H[c], len(x))
    return out


# ------------------------------------------------------------ buses
class Bus:
    def __init__(self, name):
        self.name = name
        self.x = np.zeros((N, 2))

    def add(self, s, t0):
        """Add s starting at film time t0, wrapping round the loop."""
        s = stereo(s)
        start = int(round(t0 * SR)) % N
        n = len(s)
        while n > 0:
            take = min(n, N - start)
            self.x[start:start + take] += s[:take]
            s = s[take:]
            n -= take
            start = 0


music, beds, sfx, room_send, hall_send = Bus("music"), Bus("beds"), Bus("sfx"), Bus("room"), Bus("hall")

_cache = {}
placed = []
# Single clicks cut from Mixkit sounds (id, start, length): a lighter wheel's
# one click, a soft typewriter key, a revolver's first chamber, a clock knob's
# first detent.
CLICKS = {"wheel": (2641, 0.10, 0.10), "key": (1125, 0.08, 0.10), "chamber": (1674, 0.0, 0.03),
          "detent": (1062, 0.09, 0.05)}


def click(kind, t, level, **kw):
    sid, st, ln = CLICKS[kind]
    return hit(sid, t, level, start=st, length=ln, fin=0.001, fout=0.01, **kw)


def S(sid):
    """A Mixkit sound by id, decoded once."""
    if sid not in _cache:
        _cache[sid] = load(os.path.join(MK, f"{sid}.wav"))
    return _cache[sid]


def meta(sid):
    return man[str(sid)]


def hit(sid, t, level, align="auto", p=0.0, width=1.0, semis=0.0, start=None, length=None, hp=None, lp=None,
        room=0.0, hall=0.0, rev=False, fin=0.004, fout=0.03, bus=None):
    """Place a sound so that its peak (a transient) or onset (a swell), or a
    given offset, lands at film time t; level is the loudest 50 ms in dBFS."""
    x = S(sid)
    if start is not None or length is not None:
        x = trim(x, start or 0.0, length)
    if rev:
        x = reverse(x)
    x = fade(x, fin, fout)
    if hp:
        x = highpass(x, hp)
    if lp:
        x = lowpass(x, lp)
    if semis:
        x = pitch(x, semis)
    x = normalize(x, level)
    x = pan(x, p, width)
    r = 2 ** (semis / 12)
    if align == "auto":
        # transients land on their peak; swells and long sounds on their onset
        align = "peak" if meta(sid)["peak"] - meta(sid)["onset"] < 0.4 else "onset"
    if isinstance(align, (int, float)):
        off = align
    elif align == "peak":
        off = (meta(sid)["peak"] - (start or 0.0)) / r
    elif align == "end":
        off = len(x) / SR
    elif align == "start":
        off = 0.0
    else:
        off = max(0.0, meta(sid)["onset"] - (start or 0.0)) / r
    if rev and align == "peak":
        off = len(x) / SR - off
    t0 = t - off
    placed.append((t % DUR, sid, meta(sid)["title"], level, round(p, 2), round(t0 % DUR, 3), round(len(x) / SR, 2)))
    (bus or sfx).add(x, t0)
    if room:
        room_send.add(x * db(room), t0)
    if hall:
        hall_send.add(x * db(hall), t0)
    return t0


# ------------------------------------------------------------ picture
def cam(k, t):
    """World k's camera (zoom, center, roll) at time t."""
    c = cams[k]
    t = min(max(t, c[0, 0]), c[-1, 0])
    i = np.searchsorted(c[:, 0], t)
    i = min(max(i, 1), len(c) - 1)
    a = (t - c[i - 1, 0]) / max(c[i, 0] - c[i - 1, 0], 1e-9)
    v = c[i - 1] * (1 - a) + c[i] * a
    return v[1], (v[2], v[3]), v[4]


def rotate(v, deg):
    a = math.radians(deg)
    return (math.cos(a) * v[0] - math.sin(a) * v[1], math.sin(a) * v[0] + math.cos(a) * v[1])


def shot_at(t):
    t = t % DUR
    return max(k for k in range(7) if t >= T[k])


def screen(k, t, pt):
    """Where world k's point pt is on screen at time t, and the world's scale
    there: composed down from the shot's own camera through the portals."""
    s = shot_at(t)
    Z, C, phi = cam(s, t)
    j = s
    while j != k:
        P = (portals[j][0], portals[j][1])
        sig = portals[j][2] / cues["RSTAR"]
        th = portals[j][3]
        d = rotate((P[0] - C[0], P[1] - C[1]), -th)
        C = (C0[0] - d[0] / sig, C0[1] - d[1] / sig)
        Z, phi = Z * sig, phi + th
        j = (j + 1) % 7
        if j == s:
            break
    d = rotate(((pt[0] - C[0]) * Z, (pt[1] - C[1]) * Z), phi)
    return (C0[0] + d[0], C0[1] + d[1]), Z


def xpan(x):
    return float(np.clip((x - C0[0]) / C0[0], -1, 1) * 0.8)


def portal_radius(k, t):
    """On-screen radius of world k's portal at time t (k must be on screen)."""
    P = (portals[k][0], portals[k][1])
    _, Z = screen(k, t, P)
    return portals[k][2] * Z


def smoothstep(a, b, x):
    u = np.clip((x - a) / (b - a), 0, 1)
    return u * u * (3 - 2 * u)


def bezier_ease(x1, y1, x2, y2):
    """CSS cubic-bezier easing as a function of progress 0..1."""
    def f(u):
        lo, hi = 0.0, 1.0
        for _ in range(40):
            m = (lo + hi) / 2
            xb = 3 * (1 - m) ** 2 * m * x1 + 3 * (1 - m) * m * m * x2 + m ** 3
            lo, hi = (m, hi) if xb < u else (lo, m)
        m = (lo + hi) / 2
        return 3 * (1 - m) ** 2 * m * y1 + 3 * (1 - m) * m * m * y2 + m ** 3
    return f


beats = [BEAT0 + BEAT * n for n in range(int(DUR / BEAT) + 1)]


def is_bar(b):
    return abs(((b - BEAT0) / 2) - round((b - BEAT0) / 2)) < 0.01


# ============================================================ the score
score = load(SCORE)[:N]
score = np.pad(score, ((0, N - len(score)), (0, 0)))
# Ducked under the two big moments; the score sits low and wide, the
# room for the sound around it.
duck = np.ones(N)
tt = np.arange(N) / SR
for tc, depth, rel in ((T[2], -5.0, 1.4), (T[6], -4.0, 1.6), (T[3], -2.5, 1.0)):
    env = np.where(tt < tc - 0.05, 0.0, np.exp(-np.maximum(tt - tc, 0) / rel))
    env = np.maximum(env, smoothstep(tc - 0.12, tc - 0.02, tt) * (tt < tc))
    duck *= db(depth * env)
# Its own fade-in is slow; the drone is lifted into the first seconds so
# the film opens on it rather than on silence.
lift = np.interp(tt, [0, 0.4, 0.9, 1.5, 2.0, 3.0, 4.0, 6.0], [4, 12, 17, 16, 14, 11, 6, 0])
music.add(score * (duck * db(lift))[:, None] * db(-3.0), 0.0)


# ============================================================ beds
def bed(sid, level, envelope, fc=None, width=None, hp=None, lp=None, start=0.0):
    """A world's ambience: looped over the film, shaped by an envelope of
    film time, optionally heard through a portal (cutoff and width)."""
    x = trim(S(sid), start)
    if hp:
        x = highpass(x, hp)
    if lp:
        x = lowpass(x, lp)
    x = loopify(normalize(x, level), N)
    g = np.array([envelope(t) for t in np.arange(0, DUR, 0.01)])
    g = np.interp(tt, np.arange(len(g)) * 0.01, g)
    x = x * g[:, None]
    if fc is not None:
        x = tv_filter(x, fc, width)
    beds.add(x, 0.0)


def world_presence(k, through_from=0.6):
    """How present world k is at time t: heard through the previous world's
    portal before its hand-over (opening with the portal), fully while it
    is the shot, falling away just after the next hand-over."""
    prev = (k - 1) % 7
    tk, tn = T[k], T[k + 1]

    def g(t):
        t = t % DUR
        if tk <= t < tn:
            return 1.0
        if tn <= t < tn + 0.5:
            return float(1 - smoothstep(tn, tn + 0.5, t))
        lead = (tk - t) % DUR
        if 0 < lead < 4.0 and shot_at(t) == prev:
            r = portal_radius(prev, t)
            return float(smoothstep(30, 1100, r)) * through_from + 0.0
        return 0.0

    def fc(t):
        t = t % DUR
        lead = (tk - t) % DUR
        if 0 < lead < 4.0 and shot_at(t) == prev:
            r = portal_radius(prev, t)
            return float(250 * (18000 / 250) ** smoothstep(40, 1100, r))
        return 18000.0

    def width(t):
        t = t % DUR
        lead = (tk - t) % DUR
        if 0 < lead < 4.0 and shot_at(t) == prev:
            return float(0.15 + 0.85 * smoothstep(80, 1100, portal_radius(prev, t)))
        return 1.0

    return g, fc, width


log("beds")
g0, f0, w0 = world_presence(0)
bed(1173, -34, g0, f0, w0)                       # type: a quiet room, air
bed(3199, -44, lambda t: 1.0, hp=35)             # the room tone under everything
g1, f1, w1 = world_presence(1)
bed(1444, -43, g1, f1, w1, lp=7000)              # the lens: a fine mechanical whirr
g2, f2, w2 = world_presence(2)
bed(2136, -33, g2, f2, w2)                       # the ring: a deep metal hum in space
g3, f3, w3 = world_presence(3)
bed(1178, -32, g3, f3, w3)                       # ink: under water
bed(1877, -40, g3, f3, w3)
bed(3126, -47, g3, f3, w3, lp=3200)              # ...the flow above, muffled
g4, f4, w4 = world_presence(4)
bed(2009, -35, g4, f4, w4)                       # the well: deep air
bed(2135, -37, g4, f4, w4)                       # ...a transmitter hum
g5, f5, w5 = world_presence(5)
bed(2646, -44, lambda t: g5(t) * (0.55 + 0.45 * smoothstep(T[5], T[6], t % DUR)), f5, w5, lp=9000)  # the vortex
g6, f6, w6 = world_presence(6)
bed(3081, -40, g6, f6, w6)                       # the wordmark: a dark, still room

# The screen itself: a faint high hum, and static that follows the fringes.
_, ui = np.unique(np.round(speed[:, 0], 4), return_index=True)
fringe = np.array([[t, 3.2 + min(13.0, 1.8 * max(0.0, v - 0.4))] for t, v in speed[ui]])
fr_env = np.interp(tt, fringe[:, 0], fringe[:, 1])
crt = loopify(normalize(highpass(S(2132), 3000), -52), N)
beds.add(crt, 0.0)
stat = loopify(normalize(S(1450), -50), N) * db(np.clip((fr_env - 4.0) / 9.0, 0, 1) * 14 - 10)[:, None]
beds.add(stat, 0.0)

# ============================================================ events
log("events")
# --- every pass through a portal: air, with a character per world
for k, (sid, lvl, extra) in {1: (2604, -21, (1485, -27, 0.0)), 3: (1486, -20, None), 4: (2626, -24, (2604, -26, 0.0)),
                             5: (2608, -24, (2604, -25, 0.0))}.items():
    hit(sid, T[k] - 0.03, lvl, align="peak", room=-12)
    if extra:
        hit(extra[0], T[k] + extra[2], extra[1], align="peak")
# the loop seam (29 s = 0 s): the dot swallows the frame, a long vacuum pull
hit(1465, DUR - 0.02, -21, align="peak", hall=-14)
hit(2607, DUR + 0.02, -24, align="peak")

# --- 0 · TYPE: the dashed rings step on every beat, tiny ticks at the full stop
for i, b in enumerate(beats):
    if b < T[1] - 0.15:
        (x, _), Z = screen(0, b, (portals[0][0], portals[0][1]))
        click("wheel" if i % 2 else "key", b, -42 if is_bar(b) else -46, p=xpan(x),
              semis=(-1.0 if i % 2 else 0.5), hp=2500, room=-8)

# --- rings rushing past the frame's edge: a breath of air for each, at the
# moment its circle leaves the frame (type rings flutter like paper)
def ring_pass(k, center, radii, sids, level, hp=500, semis=0.0):
    c = cams[k]
    for n, rr in enumerate(radii):
        for i in range(1, len(c)):
            if rr * c[i - 1, 1] < 1000 <= rr * c[i, 1]:
                t = c[i, 0]
                sid = sids[n % len(sids)]
                hit(sid, t, level + rng.uniform(-2, 1), align="peak", p=(0.3 if n % 2 else -0.3), width=1.0, hp=hp,
                    semis=semis + rng.uniform(-0.5, 0.5))
                break


ring_pass(0, (portals[0][0], portals[0][1]), [13 * 1.62 ** n for n in range(2, 12)], [1468, 2632, 1471], -37)
ring_pass(5, (portals[5][0], portals[5][1]), [156 * 1.43 ** n for n in range(0, 9)], [2380, 1530], -39, hp=1500)
ring_pass(3, (portals[3][0], portals[3][1]), [122 * 1.5 ** n for n in range(1, 9)], [1468, 2632], -42, hp=300)

# --- 1 · LENS: motor, blades, the barrel ratcheting past its index, the lock
ir_ = cues["iris"]
t_a, t_b, t_end = ir_["t_a"], ir_["t_b"], ir_["turn_end"]
P1 = (portals[1][0], portals[1][1])
(x1, _), _ = screen(1, (t_a + t_b) / 2, P1)
# The blades open on an expo ease: almost all of the travel is in the first
# half second. The motor (one whirr of the autofocus) and the blades'
# slide are shaped by the blades' speed, so the sound moves exactly as they
# do and dies as they settle.
def blade_speed(t):
    u = (t - t_a) / (t_b - t_a)
    return 0.0 if u < 0 or u > 1 else 2 ** (-10 * u)


def follow_blades(sid, start, length, level, p):
    x = normalize(fade(trim(S(sid), start, length), 0.003, 0.05), level)
    ts = t_a + np.arange(len(x)) / SR
    env = np.array([blade_speed(t) for t in ts[::48]])
    env = np.interp(np.arange(len(x)), np.arange(len(env)) * 48, env) ** 0.6
    x = pan(x * env[:, None], p)
    sfx.add(x, t_a)
    room_send.add(x * db(-10), t_a)
    placed.append((t_a, sid, meta(sid)["title"], level, round(p, 2), t_a, round(len(x) / SR, 2)))


follow_blades(1436, 0.08, 0.95, -25, xpan(x1))          # the autofocus motor: one whirr
follow_blades(3117, 0.0, 1.0, -30, xpan(x1) * 0.6)      # blades sliding
ease = bezier_ease(0.3, 0, 0.2, 1)


def ratchet(turn, spacing, kinds, level, pspread):
    last = 0
    for i in range(1, 600):
        t = t_a + (t_end - t_a) * i / 600
        ang = abs(turn) * ease(i / 600)
        n = int(ang // spacing)
        if n > last:
            for _ in range(n - last):
                kind = kinds[rng.randrange(len(kinds))]
                click(kind, t, level + rng.uniform(-2.5, 1.0), p=float(np.clip(xpan(x1) + rng.uniform(-pspread, pspread), -1, 1)),
                      semis=rng.uniform(-0.6, 0.6), hp=1800, room=-9)
            last = n


ratchet(ir_["ticks_turn"], 3.0, ["wheel", "chamber"], -35, 0.15)   # the scale's ticks passing the index
ratchet(ir_["knurl_turn"], 1.5, ["detent", "key"], -44, 0.35)       # the knurled grip, finer
hit(2857, t_b, -23, p=xpan(x1), room=-6)                     # the blades lock open
hit(1433, t_b + 0.01, -29, p=xpan(x1), hp=2000)
hit(1842, t_b + 0.02, -36, p=xpan(x1), semis=1.16 + 12, hall=-10)   # a glint of metal, tuned to D
# the caption types itself on
hit(1376, cues["cascades"]["w1"], -40, align="start", start=1.25, length=0.55, fout=0.15, p=-0.55, hp=1500)

# --- 2 · THE RING: the drop. A reversed swell of metal into it, then sub,
# a struck bowl and a deep bell (tuned to D), the cobalt shockwave widening
# out, the chrome flaring; the ring's hum turning; the hoops stepping.
td = cues["shock"]
hit(1842, td, -28, align="end", rev=True, semis=1.16, hall=-8, fin=0.4)          # metal, backwards, into the drop
hit(619, td, -27, align="end", rev=True, semis=-4.12, start=0.0, length=1.5, fin=0.6)  # the bell's swell backwards
hit(549, td, -11, align="peak", hall=-16)                                         # the sub
hit(2299, td, -13, align="peak")
hit(1842, td + 0.005, -22, semis=1.16, hall=-6)                                   # the chrome ring struck
hit(619, td, -21, semis=-4.12, hall=-8)                                           # a deep bell under it
shock = trim(S(2609), 0.0)
shock = normalize(fade(pitch(shock, 1.86), 0.002, 0.3), -23)
w = smoothstep(0.0, 0.8, np.arange(len(shock)) / SR)                              # spreading outward with the ring
m = (shock[:, 0] + shock[:, 1]) / 2
sd = (shock[:, 0] - shock[:, 1]) / 2 + np.sin(np.arange(len(shock)) / SR * 2 * math.pi * 0.7) * m * 0.6
shock = np.stack([m + sd * w, m - sd * w], axis=1)
sfx.add(shock, td - 0.25 / 2 ** (1.86 / 12))
hall_send.add(shock * db(-10), td - 0.25 / 2 ** (1.86 / 12))
hit(2611, td + 0.06, -28, align="peak", width=1.0, hp=3000)                      # glassy air in the wave
hit(3108, td + 0.03, -30, semis=-0.85, hall=-6)                                   # the flare, tuned to A
hit(2650, td + 0.05, -31, align="start", lp=5000, room=-10, fout=1.5)             # the ring turning, settling
hit(1376, cues["cascades"]["w2"], -41, align="start", start=1.25, length=0.55, fout=0.15, p=0.5, hp=1500)
for i, b in enumerate(beats):                                                     # gimbal hoops step on the beat
    if T[2] + 0.2 < b < T[3] - 0.1:
        hit(2857 if i % 2 else 2858, b, -38 if is_bar(b) else -41, p=0.55 if i % 2 else -0.55,
            semis=rng.uniform(-0.4, 0.4), hp=1200, room=-8)

# --- 3 · INK: through the ring's hole into water; ripple, splash, bubbles
t3 = cues["ripple"]
hit(1306, t3, -20, semis=2.19, hall=-10)                                           # water and metal, tuned to D
hit(1311, t3 + 0.03, -27, align="peak", hp=600)                                     # the splash
hit(1209, t3 + 0.05, -29, align="start", fin=0.05, fout=1.2, lp=2500)               # the rush of going under
hit(1321, t3 + 0.15, -31, align="start", fout=1.0, lp=4000)                         # bubbles rising
hit(1376, cues["cascades"]["w3"], -43, align="start", start=1.25, length=0.5, fout=0.15, p=0.45, hp=1500, lp=4000)
P3 = (portals[3][0], portals[3][1])
for j, (orb, ph, w_) in enumerate(cues["w3_emitters"]):                             # each emitter blooping as it orbits
    big = orb < 450
    for n in range(40):
        t = T[3] - 1.2 + rng.uniform(0, 5.6)
        if not (T[3] - 1.0 < t < T[4] - 0.2):
            continue
        if rng.random() > 0.11:
            continue
        x_w = P3[0] + orb * math.cos(w_ * t + ph)
        y_w = P3[1] + orb * 0.82 * math.sin(w_ * t + ph)
        (sx, sy), Z = screen(3, t, (x_w, y_w))
        lvl = -40 if t < T[3] else -35
        sid = rng.choice([1317, 3000] if big else [3000, 3179, 1317])
        hit(sid, t, lvl + rng.uniform(-3, 1), p=xpan(sx), semis=rng.uniform(-5, -1) if big else rng.uniform(-1, 3),
            lp=3500 if t < T[3] else 6000, room=-6)

# --- 4 · THE WELL: a sonar ping on every beat as a ring leaves the well;
# deeper on the bar; heard through the ink's eye before the camera arrives
P4 = (portals[4][0], portals[4][1])
for b in cues["pings"]:
    bar = is_bar(b)
    (sx, _), Z = screen(4, b, P4)
    inside = b < T[4]
    semis = -26.7 if bar else -19.7                    # D6 on the bar, A6 between, from a 5.5 kHz ping
    lvl = (-24 if bar else -31) - (7 if inside else 0)
    hit(1583, b, lvl, p=xpan(sx), semis=semis, lp=2500 if inside else 9000, hall=-4 if bar else -8,
        length=0.55 if bar else 0.4, fout=0.3)
    if bar and not inside:                             # ...and its echo returning from the far wall
        hit(1583, b + 0.34, lvl - 9, p=-xpan(sx) * 0.8, semis=semis - 0.02, lp=1800, hall=-2, length=0.4, fout=0.3)
hit(902, T[4] + 0.02, -30, semis=-1.79, hall=-6, p=0.2)                            # arriving: a wet bleep
hit(1376, cues["cascades"]["w4"], -42, align="start", start=1.25, length=0.55, fout=0.15, p=-0.6, hp=1500)

# --- 5 · THE VORTEX OF TYPE: a vault. Its dial clicks on every beat, the
# rings answering left and right; a swell gathering into the final hit
hit(3186, T[5] + 0.02, -27, room=-8)                                               # the vault opens as we enter
for i, b in enumerate(beats):
    if T[5] + 0.3 < b < T[6] - 0.05:
        bar = is_bar(b)
        hit(2853 if bar else (2857 if i % 2 else 2858), b, -31 if bar else -37,
            p=(0.35 if i % 2 else -0.35), semis=rng.uniform(-0.3, 0.3), hp=900, room=-8)
        click("detent", b + 0.06, -45, p=(-0.5 if i % 2 else 0.5), hp=3000)     # the hairline scale, a beat behind
sw = trim(S(2670), 3.56, T[6] - T[5])                                               # noise swell: peak on the hit
sw = normalize(fade(sw, 1.2, 0.03), -26)
sfx.add(sw, T[5])
hit(645, T[6], -31, align="peak", semis=2.11, fin=1.5, fout=0.08, start=0.0, length=3.9)  # a riser in D under it
hit(1489, T[6] - 0.01, -27, align="peak", rev=True, fout=0.01)                     # air sucked in before the hit

# --- 6 · ULTRAFAST: the hit, the dot ringing, the slug printing, the counter
th = T[6]
hit(2902, th, -12, align="peak", hall=-14)                                         # the impact (its swell leads in)
hit(2299, th, -15, align="peak")
hit(1842, th + 0.01, -22, semis=1.16, hall=-6, p=0.2)                              # the vermilion dot rings, in D
hit(3108, th + 0.04, -31, semis=-0.85, hall=-6, p=0.25)                             # and shimmers, in A
hit(1376, cues["cascades"]["w6"], -42, align="start", start=1.25, length=0.6, fout=0.15, p=-0.1, hp=1500)
hit(1371, th + 0.42, -37, align="start", p=-0.65, hp=900, room=-6)                 # the proof's slug prints
cnt = trim(S(1053), 0.0, 3.3)
cnt = normalize(fade(highpass(cnt, 2500), 0.3, 0.6), -49)
sfx.add(pan(cnt, 0.65), th + 0.1)                                 # the frame counter running
for i, b in enumerate(beats):                                                      # the dot's orbit steps
    if th + 0.4 < b < DUR - 1.6:
        (x, _), _ = screen(6, b, (portals[6][0], portals[6][1]))
        click("wheel" if i % 2 else "key", b, -46, p=xpan(x), semis=-1.0 if i % 2 else 0.5, hp=2500, room=-8)

# ============================================================ mix
log("space")
room = circ_convolve(room_send.x, ir(0.7, 0.55, 0.8, 0.006, seed=3))
hall = circ_convolve(hall_send.x, ir(4.5, 3.6, 0.6, 0.02, seed=5))
mix = music.x + beds.x + sfx.x + room * db(-4) + hall * db(-6)

# Glue: a slow, gentle bus compression. Computed round the loop (the
# envelope is run over two laps and the second kept), so the seam is clean.
def envelope(sig, attack_blocks=1, release=0.25):
    k = int(0.005 * SR)
    e = np.abs(mono(sig))
    e = np.concatenate([e, e])
    blocks = np.maximum.reduceat(e, np.arange(0, len(e), k))
    alpha = math.exp(-1 / (release * SR / k))
    sm = blocks.copy()
    for i in range(1, len(sm)):
        sm[i] = max(blocks[i], alpha * sm[i - 1] + (1 - alpha) * blocks[i])
    sm = np.repeat(sm, k)[len(sig):2 * len(sig)]
    return sm


thr = db(-14)
sm = envelope(mix)
gr = np.where(sm > thr, (thr / np.maximum(sm, 1e-9)) ** (1 - 1 / 1.8), 1.0)
mix = mix * gr[:, None]


def lufs(sig):
    tmp = os.path.join(A, "_measure.wav")
    save(tmp, np.clip(sig, -1, 1))
    err = subprocess.run(["ffmpeg", "-hide_banner", "-i", tmp, "-af", "ebur128", "-f", "null", "-"],
                         capture_output=True, text=True).stderr
    os.remove(tmp)
    return float(err[err.rindex("I:"):].split()[1])


def limit(sig, ceiling_db=-1.5, look=0.004, release=0.08):
    """A look-ahead peak limiter on 4x-oversampled peaks, run round the
    loop like the glue."""
    up = 4
    n = len(sig)
    f = np.fft.rfft(sig, axis=0)
    fu = np.zeros((n * up // 2 + 1, 2), dtype=complex)
    fu[: f.shape[0]] = f
    over = np.fft.irfft(fu, n * up, axis=0) * up
    pk = np.abs(over).max(axis=1).reshape(n, up).max(axis=1)
    need = np.minimum(1.0, db(ceiling_db) / np.maximum(pk, 1e-9))
    w = int(look * SR)
    need2 = np.concatenate([need, need])
    # look-ahead: the gain is already down when the peak arrives
    mins = np.minimum.accumulate(need2[::-1].reshape(-1)[::-1]) if False else None
    from numpy.lib.stride_tricks import sliding_window_view
    win = sliding_window_view(np.concatenate([need2, np.ones(w)]), w + 1).min(axis=1)[: len(need2)]
    g = win.copy()
    a = math.exp(-1 / (release * SR))
    for i in range(1, len(g)):
        g[i] = min(win[i], a * g[i - 1] + (1 - a) * win[i])
    g = g[n:2 * n]
    return sig * g[:, None]


gain = -17.0 - lufs(mix)
mix = mix * db(gain)
for _ in range(2):
    mix = limit(mix)
    corr = -17.0 - lufs(mix)
    if abs(corr) < 0.2:
        break
    mix = mix * db(corr)
mix = limit(mix)
final = os.path.join(A, "dive_mix.wav")
save(final, mix)
log(f"mix: {lufs(mix):.1f} LUFS, peak {20 * np.log10(np.abs(mix).max()):.1f} dBFS -> {final}")

with open(os.path.join(A, "dive_sound_cues.txt"), "w") as fh:
    for t, sid, title, level, p, t0, ln in sorted(placed):
        fh.write(f"{t:7.3f}  {sid:5d}  {title[:36]:36s}  {level:6.1f} dB  pan {p:+.2f}  starts {t0:7.3f}  {ln:5.2f}s\n")
log(f"{len(placed)} events")

# A picture of the sound, for review: the mix and each bus, with the
# hand-overs marked.
for name, x in (("mix", load(final)), ("music", music.x), ("beds", beds.x), ("sfx", sfx.x)):
    spectrogram_png(x, os.path.join(A, f"spec_{name}.png"), width=1450, height=230, marks=T[1:7], label=name)
    r = rms_db(x, 0.5)
    log(f"{name:6s} " + " ".join(f"{v:.0f}" for v in r))

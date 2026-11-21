"""An original 120 BPM score for the hero film, synthesized with numpy:
pad chords, sub bass, kick, hats, risers into the cuts and impacts on the
big moments. Writes 48 kHz stereo WAV. Usage: score.py out.wav"""

import sys
import wave

import numpy as np

SR = 48000
BPM = 120
BEAT = 60 / BPM
DUR = 26.0
N = int(SR * DUR)
t = np.arange(N) / SR
out = np.zeros((N, 2))


def at(sec):
    return int(sec * SR)


def place(sig, start, gain=1.0, pan=0.0):
    i = at(start)
    j = min(N, i + len(sig))
    if j <= i:
        return
    l, r = np.sqrt(0.5 * (1 - pan)), np.sqrt(0.5 * (1 + pan))
    out[i:j, 0] += sig[: j - i] * gain * l * 1.4142
    out[i:j, 1] += sig[: j - i] * gain * r * 1.4142


def env(n, a, d):
    e = np.ones(n)
    na = max(1, int(a * SR))
    e[:na] = np.linspace(0, 1, na)
    e *= np.exp(-np.arange(n) / SR / max(d, 1e-4))
    return e


def lowpass(x, cutoff):
    # one-pole, cutoff may be an array (sweeps)
    a = np.exp(-2 * np.pi * np.broadcast_to(cutoff, x.shape) / SR)
    y = np.zeros_like(x)
    acc = 0.0
    for k in range(len(x)):
        acc = (1 - a[k]) * x[k] + a[k] * acc
        y[k] = acc
    return y


def saw(freq, n, detune=0.0):
    ph = np.cumsum(np.full(n, freq * (1 + detune)) / SR)
    return 2 * (ph % 1.0) - 1


def note(name):
    names = {"C": 0, "C#": 1, "D": 2, "D#": 3, "E": 4, "F": 5, "F#": 6, "G": 7, "G#": 8, "A": 9, "A#": 10, "B": 11}
    pitch, octave = name[:-1], int(name[-1])
    return 440.0 * 2 ** ((names[pitch] + 12 * (octave + 1) - 69) / 12)


# Chords, two bars (4 s) each: Am9 - Fmaj7 - Cadd9 - G6, repeated.
CHORDS = [["A2", "E3", "G3", "C4", "B4"], ["F2", "C3", "E3", "A3", "G4"], ["C3", "G3", "D4", "E4", "B4"], ["G2", "D3", "E3", "B3", "A4"]]
ROOTS = ["A1", "F1", "C2", "G1"]

# Pad: detuned saws through a slow filter that opens with the film.
for i in range(7):
    start = i * 4.0
    n = at(4.0) + at(1.0)
    chord = CHORDS[i % 4]
    s = np.zeros(n)
    for nm in chord:
        f0 = note(nm)
        s += saw(f0, n, -0.004) + saw(f0, n, 0.005)
    s /= len(chord) * 2
    open_ = 500 + 2600 * min(1.0, start / 18.0)
    s = lowpass(s, open_)
    e = np.minimum(1, np.arange(n) / (0.6 * SR)) * np.minimum(1, (n - np.arange(n)) / (0.8 * SR))
    place(s * e, start, 0.16 if start >= 4 else 0.11, pan=(-0.25 if i % 2 else 0.25))

# Kick on every beat from 4 s, with a breath before the warp.
def kick():
    n = at(0.45)
    tt = np.arange(n) / SR
    f = 45 + 110 * np.exp(-tt * 28)
    return np.sin(2 * np.pi * np.cumsum(f) / SR) * np.exp(-tt * 7.5) + 0.15 * np.random.default_rng(1).normal(size=n) * np.exp(-tt * 120)


k = kick()
b = 4.0
while b < 24.0:
    if not (19.0 <= b < 20.0):
        place(k, b, 0.85)
    b += BEAT

# Sub bass: eighth-note pulses on the chord roots.
for i in range(1, 6):
    root = note(ROOTS[i % 4])
    for e8 in range(16):
        st = i * 4.0 + e8 * BEAT / 2
        if 19.0 <= st < 20.0 or st >= 24.0:
            continue
        n = at(BEAT / 2 * 0.9)
        tt = np.arange(n) / SR
        s = np.sin(2 * np.pi * root * tt) + 0.3 * np.sin(4 * np.pi * root * tt)
        place(s * env(n, 0.005, 0.12), st, 0.32)

# Hats: offbeat sixteenths of filtered noise from 7 s.
rng = np.random.default_rng(7)
hat_n = at(0.06)
for s16 in range(int((24.0 - 7.0) / (BEAT / 4))):
    st = 7.0 + s16 * BEAT / 4
    if 19.0 <= st < 20.0:
        continue
    noise = rng.normal(size=hat_n)
    noise = np.diff(np.concatenate([[0], noise]))
    g = 0.09 if s16 % 2 else 0.05
    place(noise * env(hat_n, 0.001, 0.018), st, g, pan=0.3 if s16 % 4 == 1 else -0.2)

# Risers into 4 s and into the warp at 20 s.
def riser(length):
    n = at(length)
    tt = np.arange(n) / SR
    noise = rng.normal(size=n)
    s = lowpass(noise, 300 + 9000 * (tt / length) ** 2)
    tone = np.sin(2 * np.pi * np.cumsum(200 + 900 * (tt / length) ** 2) / SR) * 0.3
    return (s * 0.6 + tone) * (tt / length) ** 2


place(riser(2.0), 2.0, 0.35)
place(riser(3.0), 17.0, 0.45)

# Impacts: a deep boom with a noise tail at the warp punch and the drop-out.
def impact():
    n = at(2.5)
    tt = np.arange(n) / SR
    f = 32 + 60 * np.exp(-tt * 6)
    boom = np.sin(2 * np.pi * np.cumsum(f) / SR) * np.exp(-tt * 1.6)
    tail = lowpass(rng.normal(size=n), 1800) * np.exp(-tt * 2.2) * 0.5
    return boom + tail


place(impact(), 20.0, 0.9)
place(impact(), 23.7, 0.7)

# The last chord rings out.
n = at(2.6)
s = np.zeros(n)
for nm in ["A2", "E3", "B3", "C4", "E4"]:
    s += np.sin(2 * np.pi * note(nm) * np.arange(n) / SR) * 0.5 + saw(note(nm), n) * 0.12
s = lowpass(s, 1800) * np.exp(-np.arange(n) / SR * 1.1) * np.minimum(1, np.arange(n) / (0.05 * SR))
place(s / 5, 23.7, 0.5)

# Master: gentle saturation and a limiter-ish normalize.
out = np.tanh(out * 1.2)
out /= max(1e-6, np.max(np.abs(out))) / 0.89
pcm = (out * 32767).astype(np.int16)
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes(pcm.tobytes())
print("wrote", sys.argv[1], f"{DUR}s")

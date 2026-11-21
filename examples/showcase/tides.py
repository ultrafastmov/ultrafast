"""TIDES — a title sequence: generated footage (Seedance 2.5) set inside
typography, a triptych, an ink plate with editorial type, and an end card,
over an ElevenLabs score and narration timed word by word.
1920x1080, 32 s, with sound.

Assets live outside the repo (generated, not versioned):
    $ULTRAFAST_SHOWCASE/tides/clips/{waves,forest,city,dunes,ink,glacier}.mp4
    $ULTRAFAST_SHOWCASE/tides/audio/{voice.mp3, voice_words.json, score/take-1.mp3}

    python3 tides.py && ultrafast export $ULTRAFAST_SHOWCASE/tides/tides.json $ULTRAFAST_SHOWCASE/tides/tides.mp4
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "films"))
from ultrafastfilm import IN, OUT, Film, keys  # noqa: E402

ROOT = os.environ.get("ULTRAFAST_SHOWCASE", os.path.join(os.path.dirname(os.path.abspath(__file__)), "out"))
D = os.path.join(ROOT, "tides")
CLIP = lambda name: os.path.join(D, "clips", f"{name}.mp4")
WORDS = json.load(open(os.path.join(D, "audio", "voice_words.json")))["words"]

f = Film(1920, 1080, fps=30, duration=32.0, motion_blur=(8, 0.5))
W, H = f.W, f.H
CX, CY = W / 2, H / 2
INK = "#07080b"
PAPER = "#f3efe7"
SERIF, MONO, DISPLAY = "ultrafast:serif", "ultrafast:mono", "ultrafast:display"
SOFT_OUT = {"bezier": [0.22, 1.0, 0.36, 1.0]}


def clip(name, t_in, t_out, speed=0.75, start=0.0, rect=None, scale=None, name_id=None):
    """A generated clip playing from `start` (source seconds) at `t_in`,
    slowed to `speed`, filling `rect` (cover) or the frame."""
    p = {"src": CLIP(name), "fit": "cover"}
    if rect:
        p["rect"] = rect
    node = f.add("video", p, timing={"in": t_in, "out": t_out, "offset": t_in - start / speed, "speed": speed},
                 name=name_id or name)
    if scale:
        # A slow push: scale about the frame (or rect) center.
        c = [rect[0] + rect[2] / 2, rect[1] + rect[3] / 2] if rect else [CX, CY]
        node = f.add("transform", {"anchor": c, "position": c, "scale": scale}, {"source": node},
                     timing={"in": t_in, "out": t_out}, name=f"{name}_push")
    return node


def word_times(first, last):
    """Start of word index `first` and end of word index `last` in the voice."""
    return WORDS[first]["start"], WORDS[last]["end"]


def say(i0, i1, at, name):
    """Plays voice words i0..i1 starting at film time `at`."""
    a, b = word_times(i0, i1)
    a, b = max(0.0, a - 0.06), b + 0.12
    f.add("audio", {"src": os.path.join(D, "audio", "voice.mp3"), "start": a, "gain": 1.0},
          timing={"in": at, "out": at + (b - a), "offset": at}, name=name)
    return at - a  # film time of voice time 0 for this segment


def spoken(i0, i1, shift, x, y, size, color, align="left", font=SERIF, style="italic", name="line", width=None,
           t_out=None):
    """Words i0..i1 set as one line, each word appearing as it is said
    (a rise and fade per word, keyed on the voice's word times)."""
    text = " ".join(w["word"] for w in WORDS[i0:i1 + 1])
    starts = [WORDS[k]["start"] + shift for k in range(i0, i1 + 1)]
    # One node, words revealed in turn: `reveal` counts glyphs, so keys step
    # through each word's glyph count at its spoken time.
    counts, n = [], 0
    for k in range(i0, i1 + 1):
        n += len(WORDS[k]["word"]) + (1 if k > i0 else 0)
        counts.append(n)
    ks = [(starts[0] - 0.01, 0.0)]
    prev = 0
    for s, c in zip(starts, counts):
        ks.append((s, float(prev)))
        ks.append((s + 0.18, float(c), "linear"))
        prev = c
    p = {"text": text, "font": font, "size": size, "color": color, "align": align, "position": [x, y],
         "style": style, "reveal": keys(*ks), "reveal_softness": 3, "reveal_offset": [0, size * 0.18]}
    if width:
        p["width"] = width
    timing = {"in": starts[0] - 0.05}
    if t_out:
        timing["out"] = t_out
    return f.add("text", p, timing=timing, name=name)


# ------------------------------------------------------------------ sound
f.add("audio", {"src": os.path.join(D, "audio", "score", "take-1.mp3"),
                "gain": keys((0.0, 0.0), (1.2, 0.75), (29.5, 0.75), (32.0, 0.0))}, name="score")

ground = f.add("solid", {"color": INK}, name="ground")
layers = []

# ------------------------------------------------------------------ 1 title
# A hairline draws across; the kicker spells out above it; "Tides", set
# large in the serif's italic, is a window onto the sea. Then the camera
# falls through the letters into the footage.
T_ZOOM, T_CUT = 5.6, 7.4
rule = f.add("path", {"d": f"M {CX - 520} 640 L {CX + 520} 640", "stroke": 1.5, "color": "#ffffff55",
                      "trim_end": keys((0.3, 0.0, OUT), (1.6, 1.0)),
                      "opacity": keys((T_ZOOM - 0.6, 1.0), (T_ZOOM, 0.0))}, name="rule")
kicker = f.add("text", {"text": "A FILM IN SIX TIDES", "font": MONO, "size": 20, "tracking": 9, "weight": 500,
                        "color": "#ffffffb0", "align": "center", "position": [CX, 690],
                        "reveal": keys((0.8, 0.0), (1.8, 19.0, "linear")), "reveal_softness": 2,
                        "opacity": keys((T_ZOOM - 0.6, 1.0), (T_ZOOM, 0.0))}, name="kicker")
title_scale = keys((1.2, 0.94, SOFT_OUT), (T_ZOOM, 1.0, IN), (T_CUT, 14.0))
# Measured: "Tides" at 420 px is 787 px wide; the T's stem sits ~110 px in.
stem = [CX - 393 + 118, 470]
# Placement: the layout's origin (baseline, centered) sits at `position`
# less `anchor` scaled, so scaling pivots on the stem.
pivot = [stem[0] - CX, stem[1] - 610]
title = f.add("text", {"text": "Tides", "font": SERIF, "style": "italic", "size": 420, "align": "center",
                       "position": stem, "color": "#ffffff", "anchor": pivot, "scale": title_scale,
                       "glyph_opacity": keys((1.2, 0.0), (2.2, 1.0)), "stagger": 0.09,
                       "glyph_offset": keys((1.2, [0, 60], SOFT_OUT), (2.6, [0, 0]))}, name="title")
title_xf = {"anchor": stem}
sea = clip("waves", 0.0, T_CUT + 0.1, speed=0.6, name_id="sea_in_type")
sea_push = f.add("transform", {"anchor": stem, "position": stem, "scale": keys((0.0, 1.25), (T_ZOOM, 1.15, IN),
                                                                               (T_CUT, 1.0))},
                 {"source": sea}, name="sea_push")
in_type = f.add("mask", {}, {"source": sea_push, "matte": title}, timing={"out": T_CUT + 0.1}, name="in_type")
# A faint outline keeps the letters' edges crisp against the dark.
outline = f.add("text", {"text": "Tides", "font": SERIF, "style": "italic", "size": 420, "align": "center",
                         "position": stem, "color": "#ffffff22", "stroke": 1.2, "anchor": pivot,
                         "scale": title_scale, "glyph_opacity": keys((1.2, 0.0), (2.2, 1.0)), "stagger": 0.09,
                         "glyph_offset": keys((1.2, [0, 60], SOFT_OUT), (2.6, [0, 0]))},
                timing={"out": T_ZOOM + 0.6}, name="outline")
# Falling through: the full plate fades in as the letters fill the frame.
full_sea = clip("waves", T_ZOOM + 0.6, 11.0, speed=0.6, start=T_ZOOM * 0.6, name_id="sea")
full_sea = f.add("opacity", {"opacity": keys((T_ZOOM + 0.6, 0.0, IN), (T_CUT, 1.0))}, {"source": full_sea},
                 timing={"in": T_ZOOM + 0.6, "out": 11.0}, name="sea_fade")
layers += [rule, kicker, in_type, outline, full_sea]

# ------------------------------------------------------------------ 2 the sea, spoken
v1 = say(0, 1, 8.0, "voice1")  # "Everything moves."
shade = f.add("gradient", {"kind": "linear", "start": [0, 600], "end": [0, 1080],
                           "stops": "0:#00000000, 1:#000000a0"}, timing={"in": 7.4, "out": 11.0}, name="shade")
line1 = spoken(0, 1, v1, 150, 900, 96, "#fbf7ef", name="line1", t_out=11.0)
layers += [shade, line1]

# ------------------------------------------------------------------ 3 triptych
# Three panels open from their centers in turn: fog, rain, sand. Each
# plays slowed with a slow push, a numeral and a running timecode under it.
T3 = 10.6
v2 = say(2, 11, T3 + 1.0, "voice2")  # "The tide, the fog, the light on a wet street."
PW, PH, GAP = 540, 700, 36
x0 = CX - (3 * PW + 2 * GAP) / 2
panels = [("forest", "I", "FOG"), ("city", "II", "RAIN"), ("dunes", "III", "SAND")]
T3_OUT = 18.4
for i, (name, numeral, label) in enumerate(panels):
    x = x0 + i * (PW + GAP)
    y = 150
    t = T3 + 0.25 * i
    rect = [x, y, PW, PH]
    video = clip(name, t, T3_OUT + 0.6, speed=0.7, rect=rect, scale=keys((t, 1.18, SOFT_OUT), (T3_OUT, 1.04)),
                 name_id=f"panel_{name}")
    # The window opens from its middle and closes upward at the end.
    open_rect = keys((t, [x, y + PH / 2, PW, 0], SOFT_OUT), (t + 1.1, [x, y, PW, PH]),
                     (T3_OUT - 0.25 + 0.08 * i, [x, y, PW, PH], IN), (T3_OUT + 0.35 + 0.08 * i, [x, y, PW, 0]))
    win = f.add("crop", {"rect": open_rect, "softness": 0.0}, {"source": video},
                timing={"in": t, "out": T3_OUT + 0.6}, name=f"win_{name}")
    num = f.add("text", {"text": numeral, "font": SERIF, "style": "italic", "size": 64, "color": "#fbf7ef",
                         "position": [x, y + PH + 76], "opacity": keys((t + 0.5, 0.0), (t + 1.0, 1.0),
                                                                        (T3_OUT, 1.0), (T3_OUT + 0.3, 0.0))},
                timing={"in": t, "out": T3_OUT + 0.4}, name=f"num_{name}")
    lab = f.add("text", {"text": label, "font": MONO, "size": 18, "tracking": 7, "weight": 500, "color": "#fbf7ef99",
                         "position": [x + 80, y + PH + 64], "reveal": keys((t + 0.7, 0.0), (t + 1.2, float(len(label)))),
                         "opacity": keys((T3_OUT, 1.0), (T3_OUT + 0.3, 0.0))},
                timing={"in": t, "out": T3_OUT + 0.4}, name=f"lab_{name}")
    # A running timecode: seconds and frames of the panel's own clock.
    tc = f.add("text", {"text": "00:00:00", "font": MONO, "size": 16, "tracking": 2, "color": "#fbf7ef66",
                        "align": "right", "position": [x + PW, y + PH + 64],
                        "number": keys((t, 0.0), (T3_OUT, (T3_OUT - t) * 30.0, "linear")), "decimals": 0,
                        "prefix": "TC ", "opacity": keys((T3_OUT, 1.0), (T3_OUT + 0.3, 0.0))},
               timing={"in": t + 0.6, "out": T3_OUT + 0.4}, name=f"tc_{name}")
    layers += [win, num, lab, tc]
line2 = spoken(2, 11, v2, CX, 1010, 44, "#fbf7efd8", align="center", name="line2", t_out=T3_OUT)
layers += [line2]

# ------------------------------------------------------------------ 4 ink
# The ink plate, near white: the statement is set dark on it, multiplied
# into the water, the key word in the italic.
T4 = 18.4
# Pushed in past the glass's walls.
ink = clip("ink", T4, 26.4, speed=0.65, scale=keys((T4, 1.42), (26.4, 1.55)), name_id="ink")
ink_in = f.add("opacity", {"opacity": keys((T4, 0.0, OUT), (T4 + 0.6, 1.0))}, {"source": ink},
               timing={"in": T4, "out": 26.4}, name="ink_in")
v3 = say(12, 18, T4 + 0.7, "voice3")  # "A film is a record of change."
v4 = say(19, 28, T4 + 3.6, "voice4")  # "Here, every change is a choice you can make again."
state1 = spoken(12, 18, v3, 140, 250, 96, "#0e1220", font=DISPLAY, style="normal", name="state1", t_out=26.2)
state2 = spoken(19, 28, v4, 144, 350, 58, "#0e1220", name="state2", width=1100, t_out=26.2)
dark_type = f.add("over", {}, {"fg": state2, "bg": state1}, timing={"in": T4, "out": 26.4}, name="dark_type")
# A soft wash of white on the left, so the ink pales where the words sit.
wash = f.add("gradient", {"kind": "linear", "start": [0, 0], "end": [1300, 0],
                          "stops": "0:#c8c8c8, 0.5:#6a6a6a, 1:#000000", "space": "srgb"}, timing={"in": T4, "out": 26.4},
             name="wash")
washed = f.blend(wash, ink_in, "screen", name="washed")
ink_type = f.blend(dark_type, washed, "multiply", name="ink_type")
layers += [ink_type]

# ------------------------------------------------------------------ 5 end
T5 = 26.2
glacier = clip("glacier", T5, 32.0, speed=0.55, scale=keys((T5, 1.12), (32.0, 1.0)), name_id="glacier")
glacier = f.add("opacity", {"opacity": keys((T5, 0.0, OUT), (T5 + 0.8, 1.0), (30.6, 1.0), (31.8, 0.0))},
                {"source": glacier}, timing={"in": T5}, name="glacier_in")
# Letterbox to 2.39:1, closing in.
bar_h = (H - W / 2.39) / 2
bars = [f.add("shape", {"kind": "rect", "center": [CX, c], "size": keys((T5, [W, 0.0], SOFT_OUT), (T5 + 1.4, [W, bar_h * 2])),
                        "color": INK}, timing={"in": T5}, name="bar")
        for c in (0, H)]
mark = f.add("text", {"text": "ultrafast", "font": SERIF, "size": 150, "align": "center", "position": [CX, 560],
                      "color": "#ffffff", "glyph_opacity": keys((T5 + 1.2, 0.0), (T5 + 2.0, 1.0)), "stagger": 0.07,
                      "glyph_offset": keys((T5 + 1.2, [0, 30], SOFT_OUT), (T5 + 2.2, [0, 0])),
                      "opacity": keys((30.8, 1.0), (31.8, 0.0))}, timing={"in": T5}, name="mark")
tag = f.add("text", {"text": "THE VIDEO ENGINE FOR AGENTS", "font": MONO, "size": 18, "tracking": 8, "weight": 500,
                     "color": "#ffffffb8", "align": "center", "position": [CX, 640],
                     "reveal": keys((T5 + 2.0, 0.0), (T5 + 3.0, 27.0, "linear")),
                     "opacity": keys((30.8, 1.0), (31.8, 0.0))}, timing={"in": T5}, name="tag")
layers += [glacier] + bars + [mark, tag]

# ------------------------------------------------------------------ finish
comp = f.stack(ground, layers)
comp = f.add("grade", {"look": 2, "amount": 0.55}, {"source": comp}, name="grade")
comp = f.add("vignette", {"inner": "#00000000", "outer": "#00000090"}, {"source": comp}, name="vignette")
comp = f.add("grain", {"amount": 0.05}, {"source": comp}, name="grain")
f.output = comp
# Every line placed by its baseline.
for n in f.nodes.values():
    if n["op"] == "text":
        n["params"].setdefault("valign", "baseline")
out = f.save(os.path.join(D, "tides.json"))
print(out)

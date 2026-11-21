"""LAUNCH — ultrafast introduces itself: a spoken voiceover (system TTS with
exact word times), karaoke captions, SVG artwork, and one scene per idea,
each cut on the word that names it. 1080x1920, 15 s, with sound.

    swift ../../tools/say_words.swift "<script>" ../../out/films/launch_vo.wav launch_words.json Samantha 0.5
    python3 launch.py && ultrafast review launch.json && ultrafast export launch.json ../../out/films/launch.mp4
"""

import json
import wave

import numpy as np

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=15.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX = W / 2
LEAD = 0.5  # the voice starts after half a second
SCORE = "../../out/films/hero_score.wav"
SCORE_START = 8.0
VO = "../../out/films/launch_vo.wav"
WORDS = json.load(open(os.path.join(FILMS_DIR, "launch_words.json")))["words"]
SVG = lambda name: os.path.relpath(os.path.join(FILMS_DIR, "..", "assets", "svg", name), FILMS_DIR)
MONO, SANS, FR, UNB = "GeistMono-VF.ttf", "Geist-VF.ttf", "Fraunces-VF.ttf", "Unbounded-VF.ttf"
INK_BG = "#0d0b14"


def at(word, nth=1):
    """When the voice starts saying `word` (the nth time)."""
    hits = [w for w in WORDS if w["word"].strip(".,").lower() == word.lower()]
    return hits[nth - 1]["start"] + LEAD


CUT = {
    "agents": at("A"),
    "type": at("Type"),
    "3d": at("3D"),
    "sim": at("simulation"),
    "sound": at("and"),
    "graph": at("in"),
    "gpu": at("Every"),
    "change": at("and", 2),
    "end": at("Make"),
}
ORDER = ["mark", "agents", "type", "3d", "sim", "sound", "graph", "gpu", "change", "end"]
START = {"mark": 0.0, **CUT}


def window(name):
    i = ORDER.index(name)
    return {"in": START[name], **({"out": START[ORDER[i + 1]]} if i + 1 < len(ORDER) else {})}


# ---------------------------------------------------------------- sound
f.add("audio", {"src": VO, "gain": 1.0}, timing={"offset": LEAD}, name="voice")
f.add("audio", {"src": SCORE, "start": SCORE_START,
                "gain": keys((0, 0.0), (0.6, 0.24), (14.0, 0.24), (15.0, 0.0))}, name="score")

dark = f.add("gradient", {"kind": "radial", "center": [CX, 760], "radius": 1500,
                          "stops": "0:#1d1426, 0.55:#0d0b14, 1:#050408"}, name="dark")

# ---------------------------------------------------------------- 1 mark
t = 0.0
glow = f.add("gradient", {"kind": "radial", "center": [CX, 800], "radius": keys((0.0, 200.0, OUT), (1.2, 620.0)),
                          "stops": "0:#ff2e6355, 1:#ff2e6300"}, name="glow")
ring = f.add("path", {"kind": "circle", "center": [CX, 800], "radius": 330, "color": "#ffffff38", "stroke": 3,
                      "dash": [4, 16], "cap": "round", "anchor": [CX, 800], "position": [CX, 800],
                      "rotation": keys((0.0, -30.0), (1.6, 10.0))}, name="ring")
mark = f.add("svg", {"src": SVG("ultrafast-mark.svg"), "size": [400, 400], "anchor": [200, 200], "position": [CX, 800],
                     "scale": keys((0.0, 0.62, OUT), (0.7, 1.0)), "rotation": keys((0.0, -14.0, OUT), (0.8, 0.0))},
             name="mark")
wordmark = f.text("ultrafast", UNB, 150, [CX, 1250], color="#ffffff", align="center", weight=800, tracking=-4,
                  name="wordmark")
wm, wm_win = line_reveal(f, wordmark, CX, 1250, 150, at("ultrafast") - 0.05, dur=0.55, ascent=0.8, descent=0.25)
s_mark = f.stack(dark, [glow, ring, mark, (wm, None, wm_win)])

# ---------------------------------------------------------------- 2 agents
t = CUT["agents"]
card = lambda c, size, color, name: f.add("shape", {"kind": "rect", "center": c, "size": size, "radius": 36,
                                                    "color": color}, name=name)
term = f.stack(card([CX, 560], [900, 420], "#ffffff12", "term_rim"),
               [card([CX, 560], [894, 414], "#16131f", "term")])
dots = [f.add("shape", {"kind": "ellipse", "center": [150 + i * 34, 392], "size": [18, 18], "color": c}, name="dot")
        for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"])]
lines = [("agent ▸ make the title pink", "#f1eee8"),
         ('{"edit": "set_param", "node": "title",', "#9d97b4"),
         (' "param": "color", "value": "#ff2e63"}', "#9d97b4")]
typed = []
clock = t + 0.15
for i, (txt, col) in enumerate(lines):
    n = len(txt)
    dur = 0.32 if i == 0 else 0.42
    tx = f.text(txt, MONO, 30, [140, 480 + i * 64], color=col, weight=500, name="code",
                reveal=keys((clock, 0.0), (clock + dur, float(n))), reveal_softness=2)
    typed.append(tx)
    clock += dur + 0.06
hit = at("agents")
preview = f.stack(card([CX, 1080], [900, 360], "#ffffff10", "prev_rim"), [card([CX, 1080], [894, 354], "#120f19", "prev")])
title = f.text("hello", FR, 190, [CX, 1150], align="center", weight=keys((hit, 400.0, OUT), (hit + 0.5, 800.0)),
               color=keys((0.0, "#f1eee8", "hold"), (hit, "#ff2e63")), axis_SOFT=100, name="title")
pulse = f.add("shape", {"kind": "rect", "center": [CX, 1080], "size": [894, 354], "radius": 34,
                        "color": keys((hit, "#ff2e6300", OUT), (hit + 0.05, "#ff2e6340"), (hit + 0.6, "#ff2e6300"))},
              name="pulse")
label = f.text("LIVE PREVIEW", MONO, 22, [150, 950], color="#f1eee870", tracking=3, weight=500, name="tag")
s_agents = f.stack(dark, [term] + dots + typed + [preview, pulse, title, label])

# ---------------------------------------------------------------- 3 type
t = CUT["type"]
paper = f.add("solid", {"color": "#efe8dc"}, name="paper")
type_word = f.text("Type", FR, 470, [CX, 1040], color="#141824", align="center", valign="baseline",
                   weight=keys((t, 200.0, OUT), (t + 0.55, 900.0)),
                   axis_SOFT=keys((t, 0.0, OUT), (t + 0.6, 100.0)), axis_opsz=144, tracking=-14,
                   scale=keys((t, 0.92, OUT), (t + 0.6, 1.0)), anchor=[0, -160], position=[CX, 880], name="type_word")
type_cap = f.text("variable fonts", ITALIC := "InstrumentSerif-Italic.ttf", 50, [CX, 1180],
                  color="#141824e8", align="center", name="type_cap")
s_type = f.stack(paper, [type_word, type_cap])

# ---------------------------------------------------------------- 4 3D
t = CUT["3d"]
STUDIO = """
float4 shade(float2 pos, float px) {
  float2 uv = pos / size();
  float y = uv.y, u = uv.x;
  float3 sky = lerp(float3(0.5, 0.45, 0.62), float3(2.4, 2.1, 2.5), pow(saturate(y / 0.5), 2.0));
  float3 ground = lerp(float3(0.12, 0.05, 0.09), float3(0.01, 0.008, 0.012), smoothstep(0.5, 0.6, y));
  float3 c = y < 0.5 ? sky : ground;
  c += smoothstep(0.05, 0.0, length((uv - float2(0.3, 0.35)) * float2(1.0, 2.0)) - 0.03) * float3(14.0, 4.0, 7.0);
  c += smoothstep(0.05, 0.0, length((uv - float2(0.75, 0.32)) * float2(1.0, 2.0)) - 0.02) * float3(4.0, 6.0, 14.0);
  return float4(c, 1.0);
}"""
studio = f.add("shader", {"code": STUDIO}, name="studio")
mesh = f.add("mesh", {"primitive": "text", "text": "3D", "font": font(UNB), "weight": 800, "size": 1.6, "depth": 0.6,
                      "bevel": 0.06, "color": "#f4f2f8", "metallic": 1.0, "roughness": 0.12, "position": [0, 0.2, 0],
                      "rotation": keys((t, [8, -55, -6], OUT), (t + 0.8, [0, 18, 0]))}, name="mesh3d")
cam = f.add("camera", {"target": [0, 0.15, 0], "fov": 30, "orbit": [0, 6, 13]}, name="cam")
key = f.add("light", {"direction": [-0.4, 1.0, 0.7], "intensity": 0.8}, name="key")
s_3d = f.add("render3d", {"background": "#08060c", "environment_intensity": 1.0,
                          "environment_rotation": keys((t, -60.0), (t + 0.8, 40.0)), "shadows": False},
             {"camera": cam, "word": mesh, "key": key, "environment": studio}, name="scene3d")
s_3d = f.add("bloom", {"threshold": 0.8, "radius": 40, "intensity": 0.6}, {"source": s_3d}, name="bloom3d")

# ---------------------------------------------------------------- 5 simulation
t = CUT["sim"]
seed_mark = f.add("svg", {"src": SVG("ultrafast-mark.svg"), "size": [420, 420], "anchor": [210, 210],
                          "position": [CX, 760]}, name="seed_mark")
pour = f.add("opacity", {"opacity": keys((t, 1.0), (t + 0.25, 1.0), (t + 0.4, 0.0))}, {"source": seed_mark},
             name="pour")
ink = f.add("fluid", {"start": t, "emit": 30, "dissipation": 0.05, "velocity_dissipation": 0.25, "vorticity": 10,
                      "turbulence": 2400, "turbulence_scale": 210, "buoyancy": 260, "stir": 9000, "swirl": 5000,
                      "resolution": 0.25, "dye_resolution": 0.6, "seed": 11},
            {"dye": pour, "stir": pour}, name="ink")
crisp = f.add("opacity", {"opacity": keys((t, 1.0), (t + 0.3, 0.0))}, {"source": seed_mark}, name="crisp")
s_sim = f.stack(dark, [ink, crisp])

# ---------------------------------------------------------------- 6 sound
t = CUT["sound"]
t_end = CUT["graph"]
with wave.open(os.path.join(FILMS_DIR, SCORE)) as w:
    rate, n = w.getframerate(), w.getnframes()
    pcm = np.frombuffer(w.readframes(n), dtype=np.int16).astype(np.float32) / 32768.0
    pcm = pcm.reshape(-1, w.getnchannels()).mean(axis=1)
BARS = 26
rng = np.random.default_rng(4)
bias = rng.uniform(0.55, 1.0, BARS)
bars = []
fps_k = 30
for b in range(BARS):
    ks = []
    for k in range(int((t_end - t) * fps_k) + 2):
        tt = t + k / fps_k
        src = SCORE_START + tt - 0.02 * abs(b - BARS / 2)
        i0 = int(src * rate)
        seg = pcm[i0:i0 + rate // 30]
        loud = float(np.sqrt(np.mean(seg ** 2))) if len(seg) else 0.0
        h = 40 + min(1.0, loud * 9.0 * bias[b]) * 560 * (1.0 - 0.5 * abs(b - BARS / 2) / (BARS / 2))
        ks.append((round(tt, 3), [26.0, round(h, 1)]))
    x = CX + (b - (BARS - 1) / 2) * 36
    col = ["#ff7a3d", "#ff2e63", "#c13cff", "#7a3cff"][b * 4 // BARS]
    bars.append(f.add("shape", {"kind": "rect", "center": [x, 800], "size": keys(*ks), "radius": 13, "color": col},
                      name="bar"))
wave_icon = f.add("svg", {"src": SVG("icon-wave.svg"), "size": [120, 120], "anchor": [60, 60], "position": [CX, 330],
                          "opacity": 0.8}, name="wave_icon")
s_sound = f.stack(dark, bars + [wave_icon])
s_sound = f.add("bloom", {"threshold": 0.5, "radius": 30, "intensity": 0.5}, {"source": s_sound}, name="bloom_snd")

# ---------------------------------------------------------------- 7 graph
t = CUT["graph"]
DOTS = """
float4 shade(float2 pos, float px) {
  float2 c = fmod(pos, 48.0) - 24.0;
  float d = smoothstep(2.2, 1.2, length(c));
  return float4(float3(1.0), 1.0) * d * 0.07;
}"""
grid = f.add("shader", {"code": DOTS}, name="dots")
nodes_g = [("text", 250, 470, "icon-layers.svg"), ("svg", 250, 760, "icon-spark.svg"), ("fluid", 250, 1050, "icon-wave.svg"),
           ("over", 640, 760, None), ("output", 900, 760, None)]
edges = [(0, 3), (1, 3), (2, 3), (3, 4)]
layers = [grid]
for k, (a, b) in enumerate(edges):
    (_, ax, ay, _), (_, bx, by, _) = nodes_g[a], nodes_g[b]
    x0, x1 = ax + 120, bx - (100 if b == 3 else 90)
    d = f"M {x0} {ay} C {x0 + 140} {ay} {x1 - 140} {by} {x1} {by}"
    e0 = t + 0.25 + k * 0.1
    layers.append(f.add("path", {"d": d, "color": "#ff2e63", "stroke": 4, "cap": "round",
                                 "trim_end": keys((e0, 0.0, OUT), (e0 + 0.45, 1.0))}, name="edge"))
for k, (lab, x, y, icon) in enumerate(nodes_g):
    k0 = t + k * 0.08
    w_ = 230 if icon else 200
    cardn = f.add("shape", {"kind": "rect", "center": [x, y], "size": [w_, 130], "radius": 28, "color": "#1c1826",
                            "stroke": 0}, name="gcard")
    rim = f.add("shape", {"kind": "rect", "center": [x, y], "size": [w_ + 4, 134], "radius": 30,
                          "color": "#ff2e63" if lab == "output" else "#ffffff22"}, name="grim")
    parts = [rim, cardn]
    tx = f.text(lab, MONO, 34, [x + (24 if icon else 0), y + 12], color="#f1eee8", align="center", weight=600,
                name="glabel")
    parts.append(tx)
    if icon:
        parts.append(f.add("svg", {"src": SVG(icon), "size": [46, 46], "anchor": [23, 23],
                                   "position": [x - w_ / 2 + 48, y]}, name="gicon"))
    group = f.stack(parts[0], parts[1:])
    pop = f.add("transform", {"anchor": [x, y], "position": [x, y],
                              "scale": keys((k0, 0.6, {"bezier": [0.2, 1.5, 0.4, 1.0]}), (k0 + 0.35, 1.0))},
                {"source": group}, name="gpop")
    pop = f.add("opacity", {"opacity": keys((k0, 0.0), (k0 + 0.12, 1.0))}, {"source": pop}, name="gin")
    layers.append(pop)
s_graph = f.stack(dark, layers)

# ---------------------------------------------------------------- 8 gpu
t = CUT["gpu"]
big = f.text("116", UNB, 330, [CX, 900], color="#ffffff", align="center", weight=keys((t, 300.0, OUT), (t + 0.8, 800.0)),
             tracking=-12, name="fps_big")
fps = f.text("frames per second, exported", ITALIC, 60, [CX, 1010], color="#f1eee8c0", align="center", name="fps_sub")
ticks = []
for k in range(24):
    x = CX + (k - 11.5) * 34
    t0 = t + 0.15 + k * 0.05
    ticks.append(f.add("shape", {"kind": "rect", "center": [x, 1160], "size": [22, 60], "radius": 6,
                                 "color": keys((t0, "#ffffff18", OUT), (t0 + 0.06, "#ff2e63"), (t0 + 0.5, "#ff2e6390"))},
                       name="tick"))
gpu_lab = f.text("1080 × 1920  ·  0 READBACKS  ·  GPU", MONO, 24, [CX, 1260], color="#f1eee880", align="center",
                 tracking=3, weight=500, name="gpu_lab")
s_gpu = f.stack(dark, [big, fps] + ticks + [gpu_lab])
s_gpu = f.add("bloom", {"threshold": 0.7, "radius": 36, "intensity": 0.5}, {"source": s_gpu}, name="bloom_gpu")

# ---------------------------------------------------------------- 9 change
t = CUT["change"]
end = CUT["end"]
looks = [("#ff2e63", "#120f19", 900, 0, "weight 900"),
         ("#141824", "#efe8dc", 300, 100, "paper"),
         ("#ffd23f", "#1b1033", 700, 50, "#ffd23f"),
         ("#ffffff", "#ff2e63", 800, 100, "background")]
step = (end - t) / len(looks)
bg_keys, col_keys, wt_keys, soft_keys = [], [], [], []
logs = []
for k, (col, bg, wt, soft, log) in enumerate(looks):
    tk = t + k * step
    bg_keys.append((tk, bg, "hold"))
    col_keys.append((tk, col, "hold"))
    wt_keys.append((tk, float(wt), OUT))
    soft_keys.append((tk, float(soft), OUT))
    lg = f.text(log, MONO, 30, [CX, 1220], color="#f1eee8" if k != 1 else "#141824", align="center",
                weight=500, name="log")
    logs.append((lg, None, {"in": tk, "out": tk + step}))
wt_keys = [(a, v, OUT) for a, v, _ in wt_keys]
change_bg = f.add("solid", {"color": keys(*bg_keys)}, name="change_bg")
any_word = f.text("anything", FR, 240, [CX, 980], align="center", color=keys(*col_keys), weight=keys(*wt_keys),
                  axis_SOFT=keys(*soft_keys), axis_opsz=144, tracking=-6, name="anything")
s_change = f.stack(change_bg, [any_word] + logs)

# ---------------------------------------------------------------- 10 end
t = CUT["end"]
end_glow = f.add("gradient", {"kind": "radial", "center": [CX, 760], "radius": keys((t, 300.0, OUT), (t + 1.2, 700.0)),
                              "stops": "0:#7a3cff40, 1:#7a3cff00"}, name="end_glow")
end_mark = f.add("svg", {"src": SVG("ultrafast-mark.svg"), "size": [300, 300], "anchor": [150, 150], "position": [CX, 760],
                         "scale": keys((t, 0.7, OUT), (t + 0.6, 1.0))}, name="end_mark")
end_word = f.text("ultrafast", UNB, 140, [CX, 1090], color="#ffffff", align="center", weight=800, tracking=-4,
                  name="end_word")
ew, ew_win = line_reveal(f, end_word, CX, 1090, 140, t + 0.25, dur=0.55, ascent=0.8, descent=0.25)
end_tag = f.text("the video engine for agents", ITALIC, 58, [CX, 1190], color="#f1eee8c0", align="center",
                 name="end_tag")
end_tag = f.add("opacity", {"opacity": keys((t + 0.6, 0.0, SOFT), (t + 1.1, 1.0))}, {"source": end_tag}, name="tag_in")
s_end = f.stack(dark, [end_glow, end_mark, (ew, None, ew_win), end_tag])
s_end = f.add("bloom", {"threshold": 0.8, "radius": 40, "intensity": 0.4}, {"source": s_end}, name="bloom_end")

# ---------------------------------------------------------------- cut + captions
shots = {"mark": s_mark, "agents": s_agents, "type": s_type, "3d": s_3d, "sim": s_sim, "sound": s_sound,
         "graph": s_graph, "gpu": s_gpu, "change": s_change, "end": s_end}
comp = f.add("solid", {"color": "#000000"}, name="black")
for name in ORDER:
    comp = f.over(shots[name], comp, timing=window(name), name="at")
captions = f.add("captions", {"src": "launch_words.json", "font": font(SANS), "weight": 700, "size": 58,
                              "color": "#ffffffe8", "highlight": "#0d0b14", "highlight_background": "#ffffff",
                              "position": [CX, 1590], "max_lines": 2, "width": 860}, timing={"offset": LEAD},
                 name="captions")
# A tight dark edge and a soft shadow keep white captions readable on paper.
shadow = f.add("stroke_outline", {"color": "#0d0b14", "width": 2.5}, {"source": captions}, name="cap_edge")
shadow = f.add("drop_shadow", {"color": "#000000", "opacity": 0.5, "offset": [0, 8], "radius": 22},
               {"source": shadow}, name="cap_shadow")
# Captions sit above the grade (vignette and grain), as a player overlay would.
f.output = f.over(shadow, finish(f, comp, grain=0.05), name="with_captions")
f.save(os.path.join(FILMS_DIR, "launch.json"))
print("ok", len(f.nodes), "nodes", {k: round(v, 2) for k, v in START.items()})

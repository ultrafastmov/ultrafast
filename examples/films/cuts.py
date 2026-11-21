"""CUTS — every built-in transition, one per cut: crossfade, dip, wipe,
push, slide, iris, zoom, dissolve, glitch and blur, between shots of bold
type on color, each landing on a downbeat of the score (found by
`ultrafast beats`). 1080x1920, about 22 s, with sound."""

import json
import subprocess

from ultrafastfilm import *

ULTRAFAST = os.path.join(FILMS_DIR, "..", "..", "target", "release", "ultrafast")
SCORE = "../../out/films/hero_score.wav"
rhythm = json.loads(subprocess.run([ULTRAFAST, "beats", os.path.join(FILMS_DIR, SCORE)], capture_output=True, text=True,
                                   check=True).stdout)
DOWN = [t for t in rhythm["downbeats"] if t > 1.0]

f = Film(1080, 1920, fps=30, duration=17.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX = W / 2
shots = [
    ("crossfade", "#1b1033", "#ff5c7a", "fade"),
    ("dip", "#0b2b26", "#2ee6c5", "dip"),
    ("wipe", "#2a1505", "#ffb547", "wipe"),
    ("push", "#0a1a3a", "#5aa2ff", "push"),
    ("slide", "#3a0a1e", "#ff7ab6", "slide"),
    ("iris", "#14240a", "#b5ff5a", "iris"),
    ("zoom", "#1e0a3a", "#b07aff", "zoom"),
    ("dissolve", "#3a2a0a", "#ffd25a", "burn"),
    ("glitch", "#062a3a", "#5af0ff", "glitch"),
    ("blur", "#2e2e33", "#f3f1ec", "blur"),
    (None, "#0d0b14", "#ff5c7a", "cuts"),
]
nodes = []
for i, (_, bg, ink, word) in enumerate(shots):
    g = f.add("gradient", {"kind": "radial", "center": [CX, 800], "radius": 1500, "stops": f"0:{bg}, 1:#050407"},
              name="bg")
    t = f.text(word, "ultrafast:display", 330 if len(word) < 6 else 270, [CX, 1060], color=ink, align="center",
               weight=900, axis_SOFT=100, axis_opsz=144, tracking=-10, name="word")
    n = f.text(f"{i + 1:02d}", "ultrafast:mono", 34, [CX, 1220], color="#f3f1ecb0", align="center", tracking=4,
               name="num")
    nodes.append(f.stack(g, [t, n]))
f.add("audio", {"src": SCORE, "gain": keys((0, 0.0), (0.3, 0.9))}, name="score")
cur = nodes[0]
for i, (kind, _, ink, _) in enumerate(shots[:-1]):
    dur = 0.75 if kind not in ("dissolve", "iris") else 1.0
    # The change completes on a downbeat.
    at = DOWN[i] - dur
    params = {"kind": kind, "at": at, "duration": dur,
              "direction": [0, 0, 0, 0, 90, 0, 0, 0, 0, 0][i] if kind in ("push", "slide", "wipe") else 0}
    if kind == "dip":
        params["color"] = "#ffffff"
    if kind == "dissolve":
        params["color"] = "#ffb547"
    if kind == "wipe":
        params.update({"color": "#ffffff", "glow": 1.5, "direction": 30})
    cur = f.add("transition", params, {"a": cur, "b": nodes[i + 1]}, name="cut")
f.format["duration"] = round(DOWN[len(shots) - 2] + 2.0, 2)
f.nodes["score"]["params"]["gain"] = keys((0, 0.0), (0.3, 0.9), (f.format["duration"] - 1.0, 0.9), (f.format["duration"], 0.0))
f.output = finish(f, cur, grain=0.05)
f.save(os.path.join(FILMS_DIR, "cuts.json"))
print("ok", len(f.nodes), "nodes, duration", f.format["duration"])

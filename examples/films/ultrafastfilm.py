"""Tiny authoring helper for ultrafast documents: build the node graph in
Python (loops, staggers, layouts), write plain ultrafast JSON.

The JSON is the artifact; this only saves typing."""

import json
import os

FONTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "fonts")

# House easings (CSS cubic-bezier).
OUT = {"bezier": [0.16, 1.0, 0.3, 1.0]}  # expo-ish out: decisive arrivals
IN = {"bezier": [0.7, 0.0, 0.84, 0.0]}  # accelerate away
INOUT = {"bezier": [0.83, 0.0, 0.17, 1.0]}
SOFT = "in_out_cubic"


def font(name):
    """A bundled face (`ultrafast:serif`, a family name) as is; a file under
    examples/assets/fonts as a path relative to the films."""
    if not name.endswith((".ttf", ".otf", ".ttc")):
        return name
    return os.path.relpath(os.path.join(FONTS, name), FILMS_DIR)


FILMS_DIR = os.path.dirname(os.path.abspath(__file__))


def keys(*ks):
    """keys((t, v[, ease]), ...) -> animated param."""
    out = []
    for k in ks:
        t, v = k[0], k[1]
        e = {"t": round(t, 4), "v": v if isinstance(v, (list, str)) else [v]}
        if len(k) > 2 and k[2] is not None:
            e["ease"] = k[2]
        out.append(e)
    return {"keys": out}


class Film:
    def __init__(self, width, height, fps=30, duration=8.0, motion_blur=None):
        self.format = {"width": width, "height": height, "fps": fps, "duration": duration}
        if motion_blur:
            self.format["motion_blur"] = {"samples": motion_blur[0], "shutter": motion_blur[1]}
        self.nodes = {}
        self.components = {}
        self.count = {}
        self.output = None
        self.W, self.H = width, height

    def id(self, base):
        n = self.count.get(base, 0) + 1
        self.count[base] = n
        return f"{base}{n}" if n > 1 or base[-1].isdigit() else base

    def add(self, op, params=None, inputs=None, timing=None, name=None):
        nid = self.id(name or op)
        node = {"op": op}
        if params:
            node["params"] = params
        if inputs:
            node["inputs"] = inputs
        if timing:
            node["timing"] = timing
        self.nodes[nid] = node
        return nid

    def over(self, fg, bg, timing=None, name=None, **params):
        return self.add("over", params or None, {"fg": fg, "bg": bg}, timing, name or "over")

    def blend(self, fg, bg, mode, name=None, **params):
        return self.add("blend", dict(mode=mode, **params), {"fg": fg, "bg": bg}, name=name or "blend")

    def stack(self, base, layers):
        """Composite layers over base in order; each item is an id or
        (id, {over params}, timing)."""
        cur = base
        for layer in layers:
            if isinstance(layer, tuple):
                lid, params, timing = (list(layer) + [None, None])[:3]
                cur = self.over(lid, cur, timing=timing, **(params or {}))
            else:
                cur = self.over(layer, cur)
        return cur

    def text(self, text, font_file, size, pos, color="#f1eee8", name="text", **params):
        p = {"text": text, "font": font(font_file), "size": size, "position": pos, "color": color, "valign": "baseline"}
        p.update(params)
        return self.add("text", p, name=name)

    def crop(self, src, rect, softness=0.0, name="crop"):
        return self.add("crop", {"rect": rect, "softness": softness}, {"source": src}, name=name)

    def save(self, path):
        doc = {"version": 1, "format": self.format, "output": self.output, "nodes": self.nodes}
        if self.components:
            doc["components"] = self.components
        # Written whole, then moved into place: a live session watching the
        # file never reads half of it.
        tmp = f"{path}.tmp"
        with open(tmp, "w") as f:
            json.dump(doc, f, indent=1)
        os.replace(tmp, path)
        return path


# Shared looks.

GRAIN = """float4 shade(float2 pos, float px) {
  float2 p = floor(pos) + floor(time() * 24.0) * float2(37.0, 17.0);
  float h = frac(sin(dot(p, float2(12.9898, 78.233))) * 43758.5453);
  float g = frac(sin(dot(p + 3.1, float2(39.346, 11.135))) * 24634.6345);
  float n = (h + g) * 0.5;
  return float4(float3(n), 1.0);
}"""


def finish(film, src, grain=0.07, vignette=True):
    """Grain (overlay, re-rolled 24 times a second) and a soft vignette."""
    out = src
    if vignette:
        out = film.add("vignette", {"inner": "#00000000", "outer": "#000000a8"}, {"source": out}, name="vignette")
    if grain:
        g = film.add("shader", {"code": GRAIN}, name="grain")
        out = film.blend(g, out, "overlay", opacity=grain, name="grained")
    return out


def line_reveal(film, nid, x, baseline, size, t_in, t_out=None, rise=1.05, dur=0.7, exit_dx=-1400, exit_dur=0.5,
                ascent=1.0, descent=0.35):
    """Masked line reveal: the line rises out of its own baseline, and
    (optionally) leaves accelerating left. Returns the cropped layer and its
    visibility window."""
    node = film.nodes[nid]
    ks = [(t_in, [x, baseline + size * rise], OUT), (t_in + dur, [x, baseline])]
    if t_out is not None:
        ks = ks + [(t_out, [x, baseline], IN), (t_out + exit_dur, [x + exit_dx, baseline])]
    node["params"]["position"] = keys(*ks)
    top = baseline - size * ascent
    crop = film.crop(nid, [-4000, top, 9000, size * (ascent + descent)])
    window = {"in": t_in, "out": (t_out + exit_dur) if t_out is not None else None}
    if window["out"] is None:
        del window["out"]
    return crop, window

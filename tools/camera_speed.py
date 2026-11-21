#!/usr/bin/env python3
"""How fast a film's camera moves on screen, frame by frame: for a keyed
camera `transform` (anchor, scale, rotation at a fixed position), the px a
world point under the frame's centre and corners travels in one frame.
Read it before exporting: text the camera is reading should move under
~15 px a frame, and a statement should only appear once it has landed.

    python3 tools/camera_speed.py DOC.json [--node world_camera] [--step 0.25] [--from T] [--to T]
"""
import argparse
import bisect
import json
import math


def keyed(param):
    if not (isinstance(param, dict) and "keys" in param):
        v = param if isinstance(param, list) else [param]
        return lambda t: v
    ts = [k["t"] for k in param["keys"]]
    vs = [k["v"] for k in param["keys"]]

    def f(t):
        if len(ts) == 1:
            return vs[0]
        i = max(0, min(bisect.bisect_right(ts, t) - 1, len(ts) - 2))
        u = max(0.0, min(1.0, (t - ts[i]) / (ts[i + 1] - ts[i]) if ts[i + 1] > ts[i] else 0.0))
        return [a + (b - a) * u for a, b in zip(vs[i], vs[i + 1])]
    return f


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("doc")
    ap.add_argument("--node", default="world_camera")
    ap.add_argument("--step", type=float, default=0.25)
    ap.add_argument("--from", dest="t0", type=float, default=0.0)
    ap.add_argument("--to", dest="t1", type=float, default=None)
    a = ap.parse_args()
    d = json.load(open(a.doc))
    fmt = d["format"]
    w, h, fps = fmt["width"], fmt["height"], fmt["fps"]
    p = d["nodes"][a.node]["params"]
    pos = keyed(p.get("position", [w / 2, h / 2]))
    anc, sc, rot = keyed(p.get("anchor", [w / 2, h / 2])), keyed(p.get("scale", 1.0)), keyed(p.get("rotation", 0.0))

    def to_screen(t, x, y):
        (px, py), (ax, ay), s, r = pos(t), anc(t), sc(t)[0], math.radians(rot(t)[0])
        dx, dy = (x - ax) * s, (y - ay) * s
        return px + dx * math.cos(r) - dy * math.sin(r), py + dx * math.sin(r) + dy * math.cos(r)

    def to_world(t, x, y):
        (px, py), (ax, ay), s, r = pos(t), anc(t), sc(t)[0], math.radians(rot(t)[0])
        dx, dy = x - px, y - py
        return (dx * math.cos(r) + dy * math.sin(r)) / s + ax, (-dx * math.sin(r) + dy * math.cos(r)) / s + ay

    t, t1 = a.t0, a.t1 if a.t1 is not None else fmt["duration"]
    print("     t   centre   corner   (px per frame)   zoom")
    while t <= t1 + 1e-9:
        sp = []
        for (x, y) in ((w / 2, h / 2), (0, 0), (w, 0), (0, h), (w, h)):
            wx, wy = to_world(t, x, y)
            u, v = to_screen(t + 1 / fps, wx, wy)
            sp.append(math.hypot(u - x, v - y))
        print(f"{t:6.2f}  {sp[0]:7.1f}  {max(sp[1:]):7.1f}   {'#' * min(60, int(sp[0] / 4))}  {sc(t)[0]:.3f}")
        t += a.step


if __name__ == "__main__":
    main()

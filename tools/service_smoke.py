#!/usr/bin/env python3
"""Exercises a running `ultrafast serve` end to end: every v1 endpoint,
uploads with asset:// ids, errors, and the clear `501 cloud_only` answers
for what only ultrafast Cloud has (jobs, bucket output and inputs).

    ULTRAFAST_BASE_URL=http://127.0.0.1:8080 ULTRAFAST_API_KEY=uf_live_... \
        python3 tools/service_smoke.py [--https-input URL]

Renders only tiny documents (640x360, one second). Needs network access for
the https input check. Exits non-zero on a failure.
"""

import argparse
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
import zlib

BASE = os.environ.get("ULTRAFAST_BASE_URL", "http://127.0.0.1:8080").rstrip("/")
KEY = os.environ.get("ULTRAFAST_API_KEY", "")

failures = []


def check(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f": {detail}" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def call(method, path, body=None, headers=None, key=KEY, raw=False):
    h = {"Authorization": f"Bearer {key}"} if key else {}
    data = None
    if body is not None:
        if isinstance(body, (bytes, bytearray)):
            data = bytes(body)
        else:
            data = json.dumps(body).encode()
            h["Content-Type"] = "application/json"
    h.update(headers or {})
    req = urllib.request.Request(BASE + path, data=data, method=method, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            out = r.read()
            return r.status, dict(r.headers), out if raw else _json(out)
    except urllib.error.HTTPError as e:
        out = e.read()
        return e.code, dict(e.headers), out if raw else _json(out)


def _json(b):
    try:
        return json.loads(b)
    except Exception:
        return b


def doc(text="smoke", extra_nodes=None, out="o", duration=1.0):
    nodes = {
        "bg": {"op": "solid", "params": {"color": "#1d3557"}},
        "t": {"op": "text", "params": {"text": text, "font": "Inter", "size": 72, "position": [320, 180],
                                       "color": "#f1faee"}},
        "o": {"op": "over", "inputs": {"fg": "t", "bg": "bg"}},
    }
    nodes.update(extra_nodes or {})
    return {"version": 1, "format": {"width": 640, "height": 360, "fps": 30, "duration": duration},
            "nodes": nodes, "output": out}


def png_bytes(w=64, h=64, rgb=(230, 57, 70)):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def multipart(field, filename, content, ctype):
    boundary = "----ufsmoke" + hashlib.sha1(content[:64]).hexdigest()[:12]
    body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n"
            f"Content-Type: {ctype}\r\n\r\n").encode() + content + f"\r\n--{boundary}--\r\n".encode()
    return body, {"Content-Type": f"multipart/form-data; boundary={boundary}"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--https-input", default="")
    ap.add_argument("--skip-curl", action="store_true")
    a = ap.parse_args()

    s, _, h = call("GET", "/v1/health", key="")
    check("health", s == 200 and h.get("ready"), h)

    s, hd, e = call("GET", "/v1/me", key="wrong-key-wrong-key")
    check("401 for an unknown key", s == 401 and e["error"]["code"] == "unauthorized"
          and "Bearer" in hd.get("WWW-Authenticate", ""), e)

    s, _, me = call("GET", "/v1/me")
    check("me", s == 200 and me.get("caller") and me.get("auth") == "key", me)

    s, _, t = call("GET", "/v1/templates")
    check("templates", s == 200 and "vertical" in t.get("templates", []), t)
    s, _, v = call("GET", "/v1/templates/vertical")
    check("a template", s == 200 and v.get("version") == 1, str(v)[:200])

    # The site's REST sample, byte for byte (jq | curl), if both exist.
    tmp = tempfile.mkdtemp(prefix="uf-smoke-")
    film = os.path.join(tmp, "film.json")
    with open(film, "w") as f:
        json.dump(doc("curl"), f)
    if not a.skip_curl and shutil.which("jq") and shutil.which("curl"):
        out = os.path.join(tmp, "film.mp4")
        cmd = (f"jq '{{document: ., output: {{inline: true}}}}' {film} | "
               f"curl -sS {BASE}/v1/render -H \"Authorization: Bearer $ULTRAFAST_API_KEY\" "
               f"-H \"Content-Type: application/json\" --data-binary @- -o {out} -w '%{{http_code}}'")
        code = subprocess.run(cmd, shell=True, capture_output=True, text=True,
                              env={**os.environ, "ULTRAFAST_API_KEY": KEY}).stdout.strip()
        with open(out, "rb") as f:
            head = f.read(12)
        check("REST sample (jq | curl, inline)", code == "200" and head[4:8] == b"ftyp", f"{code} {head!r}")

    s, hd, body = call("POST", "/v1/render", {"document": doc("inline"), "output": {"inline": True}}, raw=True)
    check("render inline", s == 200 and body[4:8] == b"ftyp"
          and hashlib.sha256(body).hexdigest() == hd.get("X-Ultrafast-Sha256"), s)

    s, _, r = call("POST", "/v1/render", {"document": doc("stored")})
    ok = s == 200 and r.get("url") and r.get("sha256") and r.get("frames") == 30
    check("render stored (receipt with url)", ok, r)
    if ok:
        with urllib.request.urlopen(r["url"], timeout=60) as f:
            video = f.read()
        check("signed url downloads the video", hashlib.sha256(video).hexdigest() == r["sha256"])
        req = urllib.request.Request(r["url"], headers={"Range": "bytes=0-7"})
        with urllib.request.urlopen(req, timeout=60) as f:
            check("signed url serves ranges", f.status == 206 and len(f.read()) == 8)
        bad = r["url"].replace("sig=", "sig=0")
        try:
            urllib.request.urlopen(bad, timeout=30)
            check("a tampered url is refused", False)
        except urllib.error.HTTPError as e:
            check("a tampered url is refused", e.code == 401)

    s, hd, img = call("POST", "/v1/frame", {"document": doc("frame"), "at": 0.5, "format": "png"}, raw=True)
    check("frame (png)", s == 200 and img[:8] == b"\x89PNG\r\n\x1a\n", s)
    s, hd, img = call("POST", "/v1/frame", {"document": doc("frame"), "at": 0.5, "format": "jpeg"}, raw=True)
    check("frame (jpeg)", s == 200 and img[:2] == b"\xff\xd8", s)
    s, hd, img = call("POST", "/v1/sheet", {"document": doc("sheet"), "count": 4}, raw=True)
    check("sheet", s == 200 and img[:8] == b"\x89PNG\r\n\x1a\n", s)

    s, _, c = call("POST", "/v1/check", {"document": doc("ok")})
    check("check (clean)", s == 200 and c.get("ok") is True, c)
    broken = doc("broken")
    broken["nodes"]["o"]["inputs"]["fg"] = "missing"
    s, _, c = call("POST", "/v1/check", {"document": broken})
    check("check (broken)", s == 200 and c.get("ok") is False and c.get("issues"), c)
    s, _, e = call("POST", "/v1/render", {"document": broken})
    check("400 invalid_document with issues", s == 400 and e["error"]["code"] == "invalid_document"
          and e["error"].get("issues"), e)

    s, _, rv = call("POST", "/v1/review", {"document": doc("a review"), "safe": "social"})
    check("review", s == 200 and "issues" in rv and rv.get("frames", 0) > 0, rv)

    s, _, est = call("POST", "/v1/estimate", {"document": doc("estimate")})
    check("estimate", s == 200 and est.get("minutes", 0) > 0 and "gpu_seconds" in est
          and "first_byte_s" in est, est)

    # Uploads: multipart (curl -F) and raw, then a document using the asset.
    body, hdrs = multipart("file", "red.png", png_bytes(), "image/png")
    s, _, up = call("POST", "/v1/uploads", body, headers=hdrs)
    check("upload (multipart)", s == 201 and up.get("uri", "").startswith("asset://")
          and up.get("id", "").startswith("asset_") and up.get("bytes") == len(png_bytes()), up)
    s, _, up2 = call("POST", "/v1/uploads?filename=blue.png", png_bytes(rgb=(69, 123, 157)),
                     headers={"Content-Type": "image/png"})
    check("upload (raw body)", s == 201 and up2.get("filename") == "blue.png", up2)
    if up.get("uri"):
        s, _, g = call("GET", f"/v1/uploads/{up['id']}")
        check("get upload", s == 200 and g.get("sha256") == up.get("sha256"), g)
        d = doc("asset", {"img": {"op": "image", "params": {"src": up["uri"], "size": [200, 200],
                                                              "position": [320, 180]}},
                          "o2": {"op": "over", "inputs": {"fg": "img", "bg": "o"}}}, out="o2")
        s, hd, img = call("POST", "/v1/frame", {"document": d, "at": 0.2}, raw=True)
        rec = json.loads(hd.get("X-Ultrafast-Receipt", "{}"))
        check("frame reads asset://", s == 200 and any(i["uri"] == up["uri"] for i in rec.get("inputs", [])),
              (s, img[:200]))
        s, _, other = call("POST", "/v1/frame", {"document": {**d, "nodes": {**d["nodes"], "img": {
            "op": "image", "params": {"src": "asset://AAAAAAAAAAAAAAAAAAAAAA"}}}}})
        check("someone else's (or no) asset is refused", s == 403, other)
    s, _, e = call("POST", "/v1/frame", {"document": doc("x", {"i": {"op": "image", "params": {"src": "/etc/passwd"}}})})
    check("403 local_path_not_allowed", s == 403 and e["error"]["code"] == "local_path_not_allowed", e)
    driven = doc("driver")
    driven["nodes"]["t"]["params"]["size"] = {"audio": {"src": "/etc/hostname"}}
    s, _, e = call("POST", "/v1/frame", {"document": driven, "at": 0.2})
    check("403 for a local path in an audio driver", s == 403 and e["error"]["code"] == "local_path_not_allowed", e)

    if a.https_input:
        d = doc("https", {"img": {"op": "image", "params": {"src": a.https_input, "size": [200, 200],
                                                              "position": [320, 180]}},
                          "o2": {"op": "over", "inputs": {"fg": "img", "bg": "o"}}}, out="o2")
        s, hd, img = call("POST", "/v1/frame", {"document": d, "at": 0.2}, raw=True)
        check("public https input", s == 200, img[:300])

    # What only ultrafast Cloud has: a clear 501 each.
    def cloud_only(name, s, e, feature):
        err = e.get("error", {}) if isinstance(e, dict) else {}
        check(name, s == 501 and err.get("code") == "cloud_only" and err.get("feature") == feature
              and "ultrafast Cloud" in err.get("message", ""), e)

    s, _, e = call("POST", "/v1/jobs", {"document": doc("job"), "webhook": "https://example.com/hook"})
    cloud_only("jobs are Cloud's", s, e, "jobs")
    s, _, e = call("GET", "/v1/jobs/job_00000000000000000000000000")
    cloud_only("job lookups are Cloud's", s, e, "jobs")
    s, _, e = call("POST", "/v1/render", {"document": doc("x"), "output": {"uri": "s3://nobody/x.mp4"}})
    cloud_only("bucket output is Cloud's", s, e, "bucket_output")
    d = doc("bucket", {"img": {"op": "image", "params": {"src": "gs://nobody/a.png#1", "size": [200, 200],
                                                         "position": [320, 180]}},
                       "o2": {"op": "over", "inputs": {"fg": "img", "bg": "o"}}}, out="o2")
    s, _, e = call("POST", "/v1/frame", {"document": d, "at": 0.2})
    cloud_only("bucket inputs are Cloud's", s, e, "bucket_inputs")

    shutil.rmtree(tmp, ignore_errors=True)
    print(f"\n{len(failures)} failed" if failures else "\nall passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()

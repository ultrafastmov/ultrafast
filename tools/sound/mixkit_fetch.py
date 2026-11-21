#!/usr/bin/env python3
"""Download Mixkit sound effects (full-quality WAV) by id into a folder,
with a manifest of titles and the license. Skips files already there.

    python3 mixkit_fetch.py catalog.json out_dir 1489 1492 ...
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

UA = {"User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko)"}


def log(m):
    print(f"[{time.strftime('%H:%M:%S')}] {m}", flush=True)


def main():
    cat_path, out = sys.argv[1], sys.argv[2]
    ids = [int(x) for x in sys.argv[3:]]
    cat = json.load(open(cat_path))
    os.makedirs(out, exist_ok=True)
    man_path = os.path.join(out, "manifest.json")
    man = json.load(open(man_path)) if os.path.exists(man_path) else {}
    total = 0
    for i, sid in enumerate(ids):
        dst = os.path.join(out, f"{sid}.wav")
        if not os.path.exists(dst):
            url = f"https://assets.mixkit.co/active_storage/sfx/{sid}/{sid}.wav"
            req = urllib.request.Request(url, headers=UA)
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    data = r.read()
            except urllib.error.HTTPError as e:
                log(f"{sid}: no WAV ({e.code}), skipped")
                continue
            with open(dst + ".part", "wb") as fh:
                fh.write(data)
            os.replace(dst + ".part", dst)
            time.sleep(0.2)
        size = os.path.getsize(dst)
        total += size
        e = cat.get(str(sid), {})
        man[str(sid)] = {"title": e.get("title", ""), "tags": e.get("tags", []), "bytes": size,
                         "source": f"https://mixkit.co/ (sfx {sid})", "license": "Mixkit Sound Effects Free License"}
        if (i + 1) % 10 == 0 or i + 1 == len(ids):
            log(f"{i + 1}/{len(ids)} files, {total / 1e6:.1f} MB")
    json.dump(man, open(man_path, "w"), indent=1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Index Mixkit sound effects by tag: id, title, tags and length of every
sound on each tag page, merged into one catalog JSON (Mixkit License:
free for commercial use, no attribution required).

    python3 mixkit_catalog.py catalog.json whoosh metal underwater ...
"""
import html
import json
import re
import sys
import time
import urllib.request

UA = {"User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko)"}


def log(m):
    print(f"[{time.strftime('%H:%M:%S')}] {m}", flush=True)


def fetch(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode("utf-8", "replace")


def parse(page):
    items = []
    for block in page.split('data-test-id="audio-player"')[1:]:
        m = re.search(r'data-audio-player-item-id-value="(\d+)"', block)
        if not m:
            continue
        sid = int(m.group(1))
        title = re.search(r'item-grid-card__title">\s*([^<]+?)\s*</h2>', block)
        tags = re.findall(r'class="meta-links__link"[^>]*href="/free-sound-effects/([a-z0-9-]+)/"', block.split("item-grid__item")[0])
        dur = re.search(r'>\s*(\d+:\d\d)\s*<', block)
        items.append({"id": sid, "title": html.unescape(title.group(1)) if title else "", "tags": tags,
                      "length": dur.group(1) if dur else None})
    return items


def main():
    out, tags = sys.argv[1], sys.argv[2:]
    try:
        cat = json.load(open(out))
    except FileNotFoundError:
        cat = {}
    for tag in tags:
        for pg in (1, 2):
            url = f"https://mixkit.co/free-sound-effects/{tag}/" + (f"?page={pg}" if pg > 1 else "")
            try:
                items = parse(fetch(url))
            except Exception as e:  # a missing tag page is fine
                log(f"{tag} p{pg}: {e}")
                break
            log(f"{tag} p{pg}: {len(items)} sounds")
            for it in items:
                e = cat.setdefault(str(it["id"]), {**it, "found_in": []})
                if tag not in e["found_in"]:
                    e["found_in"].append(tag)
            if len(items) < 30:
                break
            time.sleep(0.4)
    json.dump(cat, open(out, "w"), indent=1)
    log(f"catalog: {len(cat)} sounds -> {out}")


if __name__ == "__main__":
    main()

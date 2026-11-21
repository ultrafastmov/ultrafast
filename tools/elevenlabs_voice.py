#!/usr/bin/env python3
"""Voiceover with ElevenLabs, timed word by word for ultrafast captions.

    ELEVENLABS_API_KEY=... python3 tools/elevenlabs_voice.py "Text to say." out.mp3 out_words.json \
        [--voice VOICE_ID] [--model eleven_multilingual_v2] [--stability 0.4] [--style 0.3]

Writes the audio (mp3) and {"words": [{"word", "start", "end"}, ...]} in the
shape the `captions` op and `ultrafast voiceover` use, from the character timings
of the text-to-speech-with-timestamps endpoint. The key is read from the
environment only and never printed. Logs each step with a timestamp.
"""

import argparse
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request

API = "https://api.elevenlabs.io/v1/text-to-speech/{voice}/with-timestamps?output_format=mp3_44100_192"
# A calm, low narrator ("George"); any voice id from the account works.
DEFAULT_VOICE = "JBFqnCBsd6RMkjVDRZzb"


def log(message: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def words_from(alignment: dict) -> list:
    """Group character timings into words (runs between spaces)."""
    chars = alignment["characters"]
    starts = alignment["character_start_times_seconds"]
    ends = alignment["character_end_times_seconds"]
    words, cur, t0, t1 = [], "", None, None
    for ch, a, b in zip(chars, starts, ends):
        if ch.isspace():
            if cur:
                words.append({"word": cur, "start": round(t0, 3), "end": round(t1, 3)})
            cur, t0, t1 = "", None, None
            continue
        if t0 is None:
            t0 = a
        cur += ch
        t1 = b
    if cur:
        words.append({"word": cur, "start": round(t0, 3), "end": round(t1, 3)})
    return words


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("text")
    p.add_argument("audio")
    p.add_argument("words")
    p.add_argument("--voice", default=DEFAULT_VOICE)
    p.add_argument("--model", default="eleven_multilingual_v2")
    p.add_argument("--stability", type=float, default=0.45)
    p.add_argument("--similarity", type=float, default=0.8)
    p.add_argument("--style", type=float, default=0.25)
    p.add_argument("--speed", type=float, default=1.0)
    a = p.parse_args()
    key = os.environ.get("ELEVENLABS_API_KEY")
    if not key:
        sys.exit("ELEVENLABS_API_KEY is not set")
    body = {
        "text": a.text,
        "model_id": a.model,
        "voice_settings": {"stability": a.stability, "similarity_boost": a.similarity, "style": a.style,
                           "use_speaker_boost": True, "speed": a.speed},
    }
    log(f"start: {len(a.text)} characters, voice {a.voice}, {a.model}")
    req = urllib.request.Request(API.format(voice=a.voice), data=json.dumps(body).encode(), method="POST",
                                 headers={"xi-api-key": key, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            res = json.loads(r.read())
    except urllib.error.HTTPError as e:
        sys.exit(f"ElevenLabs said {e.code}: {e.read().decode(errors='replace')[:400]}")
    with open(a.audio, "wb") as f:
        f.write(base64.b64decode(res["audio_base64"]))
    words = words_from(res.get("normalized_alignment") or res["alignment"])
    with open(a.words, "w") as f:
        json.dump({"words": words}, f, indent=1)
    log(f"done: {len(words)} words, {words[-1]['end'] if words else 0:.2f} s -> {a.audio}")


if __name__ == "__main__":
    main()

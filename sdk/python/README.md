# ultrafastmov

The Python client for [ultrafast](https://ultrafast.mov), the GPU video
engine. A video is a JSON document; this package sends it to your own
`ultrafast serve` and gives you back frames, contact sheets, reviews,
estimates and videos.

```bash
ultrafast serve           # on the machine with the GPU; prints its API key when it starts
pip install ultrafastmov
```

```python
from ultrafastmov import Ultrafast

uf = Ultrafast()  # the server on this machine; ULTRAFAST_API_KEY

doc = uf.load("film.json")
print(uf.estimate(doc))      # 8 s of 1080p, 2.1 GPU seconds
for issue in uf.review(doc):
    print(issue.kind, issue.node, issue.message)
uf.render(doc).save("film.mp4")
```

Python 3.9 or later. No dependencies.

## The server

`ultrafast serve` is the open-source render service for one machine with a
GPU: install `ultrafast` with `cargo install ultrafast`,
`brew install ultrafastmov/tap/ultrafast` or the
`ghcr.io/ultrafastmov/ultrafast` image
([self-hosting](https://github.com/ultrafastmov/ultrafast/blob/main/docs/service/SELF_HOST.md)).
The client talks to `http://127.0.0.1:8080` unless told otherwise:

```bash
export ULTRAFAST_BASE_URL=http://gpu-box:8080   # a server on another machine
export ULTRAFAST_API_KEY=...                    # the server's key
```

or `Ultrafast(base_url="http://gpu-box:8080", api_key="...")`.

Jobs and webhooks, `uri=` (your own bucket) and `gs://`, `s3://` or
`r2://` inputs are for ultrafast Cloud, which isn't open yet. The server
answers them with `CloudOnly` (`e.feature` says which).

## Local files

Documents name footage, images, fonts and audio by path
(`"src": "footage/a.mp4"`, relative to the document). Before each call the
client uploads every local file the document reads, once per client, and
sends a copy that points at the uploads (`asset://...`). Your document is
not changed. Image sequences (`"frames/f_{frame:06}.png"`) go up as one zip.
Family names (`"font": "Inter"`) and references (`asset://`, `https://`)
are sent as they are.

```python
asset = uf.upload("interview.mp4")     # or upload yourself
print(asset.uri)                       # asset://7KqH2...
```

## What it does

| Call | Returns |
|---|---|
| `uf.load(path)`, `uf.new("vertical")` | a `Document` (a `dict`) |
| `uf.check(doc)` | `.ok` and `.issues` (`node`, `severity`, `message`) |
| `uf.frame(doc, at=2.5)` | PNG bytes with `.save(path)` (`format="jpeg"` too) |
| `uf.sheet(doc, count=9)` | a labeled contact sheet, the same way |
| `uf.review(doc, safe="social")` | issues with `kind`, `node`, `from_`, `to`, `message` |
| `uf.estimate(doc)` | `.seconds`, `.minutes`, `.gpu_seconds`, ... (and `.usd` where the server sets a price); prints as `8 s of 1080p, 2.1 GPU seconds` |
| `uf.render(doc)` | a `Render`: `.url`, `.sha256`, `.bytes`, `.duration`, `.save(path)` |
| `uf.upload(path)` | an `Asset`: `.id`, `.uri`, `.sha256` |
| `uf.me()` | `caller`, `auth` |

`render` options: `inline=True` (the file comes back in the response, up to
32 MiB), `audio=False`, `range=[from, to]`, `name`, `deadline`,
and `attempt=True` (or your own render id), which sends an attempt id and
retries the request safely on `429`, `503` and dropped connections. `save`
streams to disk and checks the receipt's sha256; given a folder it names
the file for its format.

The export settings are `ultrafast export`'s, so one document gives every
deliverable:

```python
uf.render(doc, quality="draft", size="720p", inline=True)   # a quick preview
uf.render(doc, size="4k", quality="high")                   # rendered at 4K, not scaled up
uf.render(doc, codec="prores", alpha=True, name="overlay.mov")  # ProRes 4444 with alpha
uf.render(doc, format="webm", alpha=True)                   # VP9 with alpha, for the web
uf.render(doc, format="gif", fps=15, size="480p", loops=0)  # a GIF that plays once
uf.render(doc, format="png", alpha=True).save("frames/")    # a zip of PNG frames and audio.wav
uf.render(doc, format="wav")                                # the sound alone
print(uf.estimate(doc, size="4k", quality="master"))        # counted at the output size
```

`format` (`mp4`, `mov`, `mkv`, `webm`, `gif`, `png`, `exr`, `wav`, `mp3`,
`m4a`, `flac`, `opus`), `codec` (`h264`, `hevc`, `av1`, `vp9`, `prores`),
`quality` (`draft`, `standard`, `high`, `master`), `size` (`"720p"`,
`"1080p"`, `"4k"`, `"WxH"` or `(w, h)`) or `scale`, `fps`, `crf` or
`bitrate` (`"8M"`), `prores_profile`, `alpha`, `audio_codec`,
`audio_bitrate`, `loops`.

Reads (`frame`, `sheet`, `check`, `review`, `estimate`, `GET`s, uploads) are
retried twice on `429` and `503` when the server says so.

## Jobs and webhooks (ultrafast Cloud, not open yet)

`uf.render(doc, webhook=url)` and `uf.submit(doc)` return a `Job`
(`.status`, `.wait()`, `.save(path)`, `.cancel()`); `uf.job(id)` and
`uf.jobs.list()` read them; `uri="s3://bucket/key"` (also `r2://`,
`gs://`) writes the render to your bucket. `ultrafast serve` answers these
with `CloudOnly`.

```python
job = uf.render(doc, webhook="https://example.com/rendered")
print(job.id, job.status)              # job_01J... queued
job.save("film.mp4")                   # or wait for the webhook
```

```python
from ultrafastmov import webhooks

secret = uf.me()["webhook_secret"]
job = webhooks.verify(raw_body, headers["Ultrafast-Signature"], secret)
```

`verify` checks the HMAC-SHA256 signature over the raw body (constant
time) and refuses deliveries whose `sent_at` is more than 5 minutes off.

## Errors

Every API error raises an `UltrafastError` subclass carrying `status`,
`code`, `message`, `retry` and the full `body`; extra fields read as
attributes (`e.location`, `e.node`, `e.limit`):

`BadRequest`, `InvalidDocument` (`.issues`), `Unauthorized`,
`PaymentRequired`, `Forbidden`, `NotFound`, `Conflict`, `TooLarge`,
`InputUnavailable`, `Busy` (`RateLimited`), `ServerError`, `Unavailable`,
`DeadlineExceeded`, `CloudOnly` (`.feature`), and `APIConnectionError` when
no answer came back.

## Types

`ultrafastmov.types` has TypedDicts generated from the
[document schema](https://ultrafast.mov/schema/v1.json): `Document`,
`Format`, `Node`, `Timing`, and one `<Op>Params` per op (`TextParams`,
`VideoParams`, ...). Regenerate with `python3 tools/gen_types.py`.

## Development

```bash
python3 -m unittest discover -s tests -t .
```

The tests run against an in-process fake of the service (`tests/fake_server.py`).
`tests/test_live.py` runs the same calls against a real server (tiny
documents) when `ULTRAFAST_LIVE=1` and `ULTRAFAST_BASE_URL`/`ULTRAFAST_API_KEY` are set.

Licensed under Apache-2.0 OR MIT.

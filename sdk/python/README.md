# ultrafastmov

The Python client for [ultrafast](https://ultrafast.mov), the GPU video
engine. A video is a JSON document; this package sends it to an ultrafast
render service and gives you back frames, contact sheets, reviews, estimates
and MP4s.

```bash
pip install ultrafastmov
```

```python
from ultrafastmov import Ultrafast

uf = Ultrafast()  # ULTRAFAST_API_KEY

doc = uf.load("film.json")
print(uf.estimate(doc))      # $0.004, 8 s of 1080p
for issue in uf.review(doc):
    print(issue.kind, issue.node, issue.message)
uf.render(doc).save("film.mp4")
```

Python 3.9 or later. No dependencies.

## Your own server

`ultrafast serve` is the open-source render service for one machine with a
GPU. Point the client at it:

```bash
export ULTRAFAST_BASE_URL=http://localhost:8080
export ULTRAFAST_API_KEY=...   # the server's key (it prints one at first start)
```

or `Ultrafast(base_url="http://localhost:8080", api_key="...")`. Without
`ULTRAFAST_BASE_URL` the client talks to `https://api.ultrafast.mov`.

Everything here works against it except what only ultrafast Cloud has:
jobs and webhooks, `uri=` (your own bucket) and `gs://`, `s3://` or
`r2://` inputs. Those raise `CloudOnly` (`e.feature` says which).

## Local files

Documents name footage, images, fonts and audio by path
(`"src": "footage/a.mp4"`, relative to the document). Before each call the
client uploads every local file the document reads, once per client, and
sends a copy that points at the uploads (`asset://...`). Your document is
not changed. Image sequences (`"frames/f_{frame:06}.png"`) go up as one zip.
Family names (`"font": "Inter"`) and references (`asset://`, `https://`,
and on Cloud `gs://`, `s3://`, `r2://`) are sent as they are.

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
| `uf.estimate(doc)` | `.usd`, `.seconds`, `.minutes`, ...; prints as `$0.004, 8 s of 1080p` |
| `uf.render(doc)` | a `Render`: `.url`, `.sha256`, `.bytes`, `.duration`, `.save(path)` |
| `uf.render(doc, webhook=url)`, `uf.submit(doc)` | a `Job` (Cloud): `.status`, `.wait()`, `.save(path)`, `.cancel()` |
| `uf.job(id)`, `uf.jobs.list()` | jobs (Cloud) |
| `uf.upload(path)` | an `Asset`: `.id`, `.uri`, `.sha256` |
| `uf.me()` | `caller`, `auth` (and `webhook_secret` on Cloud) |

`render` options: `inline=True` (the MP4 comes back in the response, up to
32 MiB), `uri="s3://bucket/key"` (Cloud: create it in your bucket; also
`r2://`, `gs://`), `codec="hevc"`, `bitrate`, `audio=False`, `range=[from, to]`,
`name`, `deadline`, and `attempt=True` (or your own render id), which sends
an attempt id and retries the request safely on `429`, `503` and dropped
connections. `save` streams to disk and checks the receipt's sha256.

Reads (`frame`, `sheet`, `check`, `review`, `estimate`, `GET`s, uploads) are
retried twice on `429` and `503` when the server says so.

## Jobs and webhooks (Cloud)

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

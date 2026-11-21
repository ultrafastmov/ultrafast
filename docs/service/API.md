# ultrafast render service: v1 API

The HTTP API of the ultrafast render service. Two servers speak it:

- `ultrafast serve`, the open-source server for one machine: one API key,
  local storage, everything below except what is marked **Cloud**. How to
  run it: [SELF_HOST.md](SELF_HOST.md).
- ultrafast Cloud (`https://api.ultrafast.mov`), which adds the parts
  marked **Cloud**: jobs with signed webhooks, writing renders to your own
  bucket, inputs read from buckets, many keys and OIDC sign-in.

A client written against one works against the other by changing the base
URL and the key. On `ultrafast serve`, a Cloud feature answers `501` with
code `cloud_only` and a `feature` field (`jobs`, `bucket_output`,
`bucket_inputs`).

Changes from here on are additive unless this file says otherwise, and
every change is recorded at the end. Fixtures for the bodies below live in
[fixtures/](fixtures/); `crates/ultrafast-service/src/api.rs` holds the Rust
types and a test that parses every fixture, so the two cannot drift apart.

All bodies are JSON (UTF-8) unless stated. Times are seconds as numbers;
instants are RFC 3339 UTC strings ending in `Z`. Object generations and
versions are strings (int64 does not survive every JSON parser).

## Endpoints

| Method and path | Body | Success |
|---|---|---|
| `POST /v1/render` | render request | `200` mp4 bytes (`output.inline`), or `200` receipt |
| `POST /v1/jobs` (**Cloud**) | job request | `202` job (`queued`) |
| `GET /v1/jobs/{id}` (**Cloud**) | none | `200` job |
| `GET /v1/jobs` (**Cloud**) | none | `200` `{"jobs": [...]}`, the caller's most recent first (`?limit=`, default 20) |
| `DELETE /v1/jobs/{id}` (**Cloud**) | none | `200` job (`canceled`); `409` once it has started |
| `POST /v1/estimate` | estimate request | `200` estimate |
| `POST /v1/frame` | frame request | `200` PNG or JPEG |
| `POST /v1/sheet` | sheet request | `200` PNG or JPEG contact sheet |
| `POST /v1/check` | `{"document"}` | `200` `{"ok", "issues"}` |
| `POST /v1/review` | review request | `200` review |
| `POST /v1/uploads` | multipart `file=`, or raw bytes | `201` asset |
| `GET /v1/uploads/{id}` | none | `200` asset |
| `GET /v1/templates`, `GET /v1/templates/{name}` | none | `200` `{"templates": [names]}`; a starter document |
| `GET /v1/me` | none | `200` `{"caller", "auth"}` (and `webhook_secret` on Cloud) |
| `GET /v1/files/...` | none | signed download of a stored render (see Storage) |
| `GET /v1/health` | none | `200` health (no auth) |

## Authentication

Send a key as a bearer token on every request except `/v1/health`:

```http
Authorization: Bearer uf_live_...
Content-Type: application/json
```

`ultrafast serve` accepts one key (`ULTRAFAST_API_KEY`, or the one it
generates). **Cloud** accepts each account's keys, which start with
`uf_live_` (or `uf_test_`, which renders with a watermark and is never
billed), and OIDC ID tokens. A missing, unknown or expired credential is
`401` with `WWW-Authenticate: Bearer`.

`GET /v1/me` names the caller (on Cloud, with its webhook secret):

```json
{ "caller": "default", "auth": "key" }
```

## POST /v1/render

A document in, a video out. Short renders come back in the response; the
rest go to storage and the response is a receipt with a signed URL.

```json
{
  "document": { "version": 1, "format": {}, "nodes": {}, "output": "comp" },
  "output": {
    "inline": true,
    "codec": "h264",
    "bitrate": null,
    "audio": true,
    "range": null
  }
}
```

- `document` (required): an ultrafast document (see Documents below).
- `output` (optional, default `{}`): where the video goes.
  - `inline: true`: the mp4 is the response body (at most 32 MiB, else
    `413`; use storage, or on Cloud a job).
  - `uri` (**Cloud**): create this object in your own bucket instead
    (`s3://BUCKET/KEY`, `r2://BUCKET/KEY` or `gs://BUCKET/OBJECT`). It must
    not exist yet (`409` otherwise) and must fall under one of the caller's
    write prefixes. Create-only, so a retry never overwrites a finished
    video.
  - neither: the video is stored in the server's storage (local disk on
    `ultrafast serve`) and the receipt carries a signed `url`.
  - `name`: the stored file's name (default `film.mp4`).
  - `codec`: `h264` (default) or `hevc`. `bitrate`: bits per second, or
    null for the default for the format. `audio`: mix the document's sound
    (default true). `range`: `[from, to]` seconds of the document (default
    all of it).
- `attempt` (optional): `{"render_id", "attempt_id"}`, each 1 to 128
  characters of `A-Z a-z 0-9 . _ : -`. Stored on the object and echoed in
  the receipt; with `output.uri` it makes retries safe (see replays). The
  server makes one up when it is missing.
- `deadline` (optional): RFC 3339 UTC. The server stops when it passes,
  whatever it is doing, and never starts writing the output after it
  (`504`). Capped at 10 minutes from receipt (synchronous renders are
  capped at 10 minutes of work; Cloud jobs are not).

### Receipt

```json
{
  "receipt_version": 1,
  "id": "render_01JDX4...",
  "status": "rendered",
  "url": "https://renders.example.com/v1/files/renders/render_01JDX4.../film.mp4?expires=1764460800&sig=4be1...",
  "url_expires_at": "2026-11-28T00:00:00Z",
  "duration": 11.6,
  "frames": 348,
  "bytes": 6620112,
  "sha256": "2b7e...",
  "minutes": 0.19,
  "object": { "uri": "file:///data/renders/render_01JDX4.../film.mp4", "...": "..." },
  "video": { "frames": 348, "fps": 30, "width": 1080, "height": 1920, "duration": 11.6, "codec": "h264", "audio": true, "range": [0, 11.6] },
  "timings": { "plan_s": 0.03, "render_s": 1.71, "encode_s": 0.22, "upload_s": 0.02, "total_s": 2.2 },
  "...": "..."
}
```

- `id`: the render's id. `status`: `rendered`, or `already_rendered` on a
  `409` replay.
- `url`: where to download the video (null when the video went to an
  `output.uri` the server cannot sign for). `url_expires_at`: when it
  stops working (7 days by default).
- `duration`, `frames`, `bytes`, `sha256` (hex, of the mp4 bytes): the
  video written.
- `minutes`: rendered minutes as Cloud counts them (1080p minutes; 720p
  and below count half, above 1080p three times). Cloud adds
  `minutes_billed`.
- `attempt`: echoed. `object`: `uri`, `bucket`, `name`, `generation`
  (GCS generation, S3 version id or ETag), `size`, `crc32c` and `md5`
  (base64, where the store reports them), `sha256` (hex), `content_type`.
- `manifest`: `{uri, generation}` of `<object>.manifest.json`, the full
  receipt stored beside the video, or null.
- `video`: `frames`, `fps`, `width`, `height`, `duration`, `codec`,
  `audio`, `range`.
- `renderer`: `service`, `version`, `git`, `image_digest`.
- `document`: `sha256` (of the canonical JSON, below), `format_version`,
  and `meta` as the document declared it.
- `inputs`: every object read: `uri` (pinned, e.g. with `#generation`),
  `size`, `bytes_read`, `partial` (true when only byte ranges were read),
  and for archives `entries`.
- `fonts`: every pinned font file used: `family`, `weight`, `style`,
  `uri`, `sha256`.
- `timings`: seconds per stage: `instance_start_s` (only on an instance's
  first request, else null), `fetch_s`, `unpack_s`, `plan_s` (planning and
  kernel compiles before the first frame), `render_s` (frames rendered and
  encoded; the encoder runs alongside), `encode_s` (encoder busy time,
  informational), `upload_s`, `total_s`.
- `warnings`: strings. `created_at`.

### Inline output

`200`, `Content-Type: video/mp4`, the mp4 as the body, the receipt
(without `object`, `manifest` and `url`) as compact JSON in the
`X-Ultrafast-Receipt` header, and the mp4's sha256 (hex) in
`X-Ultrafast-Sha256`.

### Create-only output and replays

Stored renders are create-only too: a second request with the same
`attempt.render_id` gets the first one's receipt (`409`,
`already_rendered`) or `409 output_conflict`, as below. Before rendering to
an `output.uri` (**Cloud**), the server looks at it:

- Absent: it renders, then creates the object with a create-only
  precondition (GCS `ifGenerationMatch=0`, S3 and R2 `If-None-Match: *`).
- Present, and its metadata carries the same `render_id`, `attempt_id`
  and document sha256 as this request: `409` with the stored receipt
  (`"status": "already_rendered"`) and nothing is rendered. Compare the
  object identity in that receipt (generation, size, sha256) with what you
  expect before accepting it.
- Present with anything else: `409` error `output_conflict`, with the
  existing object's identity.

Create-only prevents overwriting, not duplicate work: two concurrent
requests for the same new object both render, and the second to finish
gets `409`.

The object's custom metadata is the recovery record (well under the 2 KiB
S3 and 8 KiB GCS limits; see
[fixtures/object-metadata.json](fixtures/object-metadata.json)):

| Key | Value |
|---|---|
| `ultrafast-receipt-version` | `1` |
| `ultrafast-render-id`, `ultrafast-attempt-id` | the request's `attempt` ids |
| `ultrafast-document-sha256` | sha256 of the document as received (canonical JSON) |
| `ultrafast-sha256`, `sha256` | sha256 (hex) of the mp4 bytes |
| `ultrafast-frames`, `ultrafast-duration`, `ultrafast-width`, `ultrafast-height`, `ultrafast-fps`, `ultrafast-codec` | the video written |
| `ultrafast-image-digest`, `ultrafast-version` | the renderer image digest and ultrafast version |
| `ultrafast-inputs-sha256` | sha256 of the receipt's `inputs` and `fonts` lists (canonical JSON) |
| `ultrafast-created-at` | when the upload began |

## Jobs and webhooks (Cloud)

For long or batch renders on ultrafast Cloud: create a job and poll it, or
get called back. `ultrafast serve` renders while you wait instead, and
answers `/v1/jobs` with `501 cloud_only` (`feature: "jobs"`).

```bash
curl $ULTRAFAST_URL/v1/jobs \
  -H "Authorization: Bearer $ULTRAFAST_API_KEY" \
  -d '{"document": {...}, "webhook": "https://example.com/rendered"}'
# 202 {"id": "job_01J...", "status": "queued", ...}

curl $ULTRAFAST_URL/v1/jobs/job_01J... -H "Authorization: Bearer $ULTRAFAST_API_KEY"
# {"id": "job_01J...", "status": "rendered", "receipt": {...}}
```

The job request is a render request (`document`, `output`, `attempt`)
plus `webhook`, an `https://` or `http://` URL. `output.inline` is not
allowed. Jobs have no 10-minute cap and wait behind nothing but other jobs
and synchronous requests.

A job:

```json
{
  "id": "job_01JDX4Q8...",
  "status": "rendering",
  "created_at": "2026-11-21T10:00:00.000Z",
  "started_at": "2026-11-21T10:00:01.250Z",
  "finished_at": null,
  "webhook": "https://example.com/rendered",
  "progress": { "frames": 120, "of": 348 },
  "receipt": null,
  "error": null
}
```

`status` is `queued`, `rendering`, `rendered`, `failed` or `canceled`.
A rendered job has the receipt; a failed one has the error body (the same
`{status, code, message, retry, ...}` as a synchronous request would have
answered). Job records stay in the account's storage until removed
there.

When a job with a `webhook` finishes, the server POSTs the job (the same
JSON as `GET /v1/jobs/{id}`, plus `"event"` and `"sent_at"`) to it:

| Header | Value |
|---|---|
| `Ultrafast-Event` | `job.rendered` or `job.failed` |
| `Ultrafast-Delivery` | an id, the same on every retry of one delivery |
| `Ultrafast-Signature` | `sha256=` and the hex HMAC-SHA256 of the raw body, keyed with your webhook secret (`GET /v1/me`) |

Any `2xx` answer ends the delivery. Anything else, or no answer within 10
seconds, is retried with backoff (10 s, 1 min, 5 min, 30 min, then hourly)
for 24 hours. Verify the signature over the raw bytes before parsing, and
use `sent_at` to refuse old replays.

## POST /v1/estimate

What a document will cost before it renders, from the same plan the
renderer runs: a few sampled frames are rendered on the GPU and nothing is
encoded.

```json
{ "document": {}, "range": null }
```

```json
{ "minutes": 0.2, "usd": 0.006, "gpu_seconds": 2.1, "first_byte_s": 3.0,
  "width": 1080, "height": 1920, "fps": 30, "seconds": 11.6, "frames": 348,
  "device": "NVIDIA L4", "sampled_frames": 24 }
```

`usd` is the deployment's price (`ULTRAFAST_USD_PER_MINUTE`), or null on a
server without one. Estimates are free.

## POST /v1/frame

```json
{ "document": {}, "at": 2.5, "format": "png" }
```

`200` with the frame at `at` seconds (`t` is accepted too) as `image/png`
(default) or `image/jpeg`, and a compact receipt (inputs, timings) in
`X-Ultrafast-Receipt`. Frames are free.

## POST /v1/sheet

```json
{ "document": {}, "count": 9, "at": null, "columns": null, "format": "png" }
```

A contact sheet: `count` frames spread evenly over the document (default
9), or the times in `at`, laid out `columns` wide (default 3, or 5 for a
vertical document) with each frame's time printed on it. Answers like
`/v1/frame`. Sheets are free.

## POST /v1/check

`{"document": {}}` → `200 {"ok": false, "issues": [{"node", "severity", "message"}]}`:
the same as `ultrafast check`, including plan and shader compile errors
from rendering the first frame. `ok` is false when any issue is an error.

## POST /v1/review

```json
{ "document": {}, "safe": "social", "step": 0.1 }
```

Renders the film and reports what a viewer would trip on, as
`ultrafast review` does: `200 {"issues": [{"severity", "kind", "node",
"from", "to", "message"}], "frames", "texts", "seconds"}`. `safe` is
`margin` (default, 4% of the frame) or `social` (clear of the buttons and
captions of short-video apps). `step` is the seconds between text probes.

## Uploads

Footage, images, fonts and audio go up once and are referenced by id.

```bash
curl $ULTRAFAST_URL/v1/uploads -H "Authorization: Bearer $ULTRAFAST_API_KEY" \
  -F file=@interview.mp4
# 201 {"id": "asset_7Kq...", "uri": "asset://7Kq...", "bytes": 48211002, ...}
```

The body is `multipart/form-data` with the file in a part named `file`,
or the raw bytes with their `Content-Type` and `?filename=interview.mp4`.

```json
{ "id": "asset_7KqH2...", "uri": "asset://7KqH2...", "bytes": 48211002,
  "sha256": "9c1e...", "filename": "interview.mp4",
  "content_type": "video/mp4", "created_at": "2026-11-21T10:00:00.000Z" }
```

Then use `"src": "asset://7KqH2..."` in a `video`, `image`, `audio`,
`captions` or any other file param, including `font`. A zip upload's
members are `asset://ID!/member/path` (an image sequence puts its frame
pattern in the member, e.g. `asset://ID!/frame_{frame:06}.webp`). Assets
belong to the caller that uploaded them. Uploads are capped at
`ULTRAFAST_MAX_UPLOAD_GB` (20 by default).

## Documents

An ultrafast document as [docs/AGENT_GUIDE.md](../AGENT_GUIDE.md)
describes, with these rules for the service:

- **Inputs are references.** Every file a document reads (`video.src`,
  `image.src`, `audio.src`, `svg.src`, `lottie.src`, `lut.src`,
  `mesh.src`, `captions.src`, `clip_camera.src`, `block.logo`, font
  files in `font` params, and the `src` of an audio driver,
  `{"audio": {"src": …}}`, on any param) is one of:
  - `asset://ID`, an upload of the caller's (members: `asset://ID!/NAME`);
  - a public `https://` URL (redirects are followed, every hop must
    resolve to public addresses; a deployment may restrict hosts with
    `ULTRAFAST_HTTPS_HOSTS`);
  - (**Cloud**) an object in a bucket the caller may read:
    `gs://BUCKET/OBJECT#GENERATION`, `s3://BUCKET/KEY` or `r2://BUCKET/KEY`
    (optionally `#VERSION_ID`). Archive members:
    `gs://BUCKET/ARCHIVE.zip#GENERATION!/MEMBER`. `ultrafast serve`
    answers `501 cloud_only` (`feature: "bucket_inputs"`): upload the file
    instead.

  Anything else that names a file is refused (`403
  local_path_not_allowed`); font family names (`"Inter"`) are fine and
  load from Google Fonts.
- **Face tracks inline.** `clip_camera.faces` takes face tracks as JSON
  text in the document (or a reference to a file holding them).
- **Inputs read only what they embed.** An SVG, Lottie or glTF input never
  reads another file it names (only data URLs, a GLB's own binary chunk),
  so nothing on the server outside the request's inputs is reachable.
- **Pins pin content.** An upload is immutable; on Cloud, a GCS
  generation (or S3 version id) reads exactly that version. Whole reads are
  checked against the store's CRC32C or MD5.
- **Long sources are read in part.** For MP4 and MOV video and audio in a
  bucket or upload, the server reads the file's index and only the byte
  ranges the render needs, so a chapter of a six-hour recording costs the
  chapter. `inputs[].partial` reports it.
- **`meta`** (optional object, ignored by rendering, echoed in the
  receipt). Conventions:
  - `meta.composer`: `{"name", "version", "revision"}` of whatever wrote
    the document.
  - `meta.fonts`: font pins, `[{"family", "weight", "style", "src",
    "sha256"}]`, `src` a reference. For this render only, these files
    answer for their family, weight and style ahead of every other font
    source.
  - `meta.archives`: maps an archive's reference to `{"members": {NAME:
    {"sha256", "size"}}}`; every listed member is checked after unpacking
    (`422 input_unavailable`, reason `integrity`).
- **Canonical JSON** for `document.sha256`: the request's `document`
  re-written with object keys sorted by their UTF-8 bytes, no whitespace,
  strings escaped and numbers written as serde_json writes them (integers
  in decimal; other numbers as the shortest text that reads back as the
  same double). [fixtures/canonical/](fixtures/canonical/) holds a document
  with awkward numbers and strings, its canonical text and its hash.
- **Object names are percent-decoded once.** In references (and in
  `output.uri` and archive members) a name escapes `%` as `%25`, `#` as
  `%23` and `?` as `%3F`, and may escape anything else.

## Storage

`ultrafast serve` keeps stored renders and uploads in a local directory
(`ULTRAFAST_STORAGE`, a path or `file:///srv/ultrafast`; the default is
`$ULTRAFAST_DATA/storage`) and signs download URLs itself, serving them
under `/v1/files/`. See [SELF_HOST.md](SELF_HOST.md). **Cloud** keeps them
in buckets and hands out the store's presigned URLs.

## Errors

Every error is `{"error": {"status", "code", "message", "retry", ...}}`
with extra fields per code. `retry` says whether the same request may
succeed later. Fixtures: [fixtures/errors/](fixtures/errors/).

| Status | `code` | Meaning and extra fields |
|---|---|---|
| 400 | `bad_request` | Malformed JSON or missing or invalid request fields. |
| 400 | `invalid_document` | The document fails its check. `issues`: every problem, `[{node, severity, message}]`, the same as `ultrafast check`. |
| 401 | `unauthorized` | Missing, unknown or expired key or token. |
| 402 | `payment_required` | Cloud only: out of free minutes and no card on file. |
| 403 | `input_not_allowed` | An input outside the caller's read prefixes, an asset of another caller, or an https host that is not allowed. `location`. |
| 403 | `output_not_allowed` | `output.uri` outside the caller's write prefixes. `location`. |
| 403 | `local_path_not_allowed` | A file param that is not a reference. `node`, `param`, `value`. |
| 404 | `not_found` | No such route, job, upload or template (or not the caller's). |
| 409 | `output_conflict` | `output.uri` exists and is not this request's render. `object`: its identity. |
| 409 | `job_started` | `DELETE` on a job that is no longer queued. `job_status`. |
| 413 | `over_limit` | A limit exceeded: an inline video over 32 MiB (use storage or a job), a body, a document, an input or an upload too large. `limit`, `value`, `max`. |
| 422 | `input_unavailable` | An input could not be fetched, is not at that version, or failed its integrity check. `location`, `reason`. |
| 429 | `busy` | More requests waiting than the server queues (`ULTRAFAST_QUEUE`), or on Cloud more concurrent renders than the key may run (`limit: "concurrency"`). `retry: true`. |
| 500 | `render_failed` | The render itself failed (a bug or a GPU fault; the server recovers). `retry: true`. |
| 501 | `cloud_only` | A Cloud feature asked of `ultrafast serve`: jobs, `output.uri`, bucket inputs. `feature`. |
| 503 | `unavailable` | Not ready, shutting down, or the store is unreachable. `retry: true`. |
| 504 | `deadline_exceeded` | The deadline passed before the output was written. Nothing was written. |

## Limits (defaults; a deployment can change them)

| Limit | Default | Setting |
|---|---|---|
| Request body | 8 MiB (document 5 MiB) | |
| Output duration | 3 hours | `ULTRAFAST_MAX_DURATION_S` |
| Frame size | 3840 per side, 8.3 megapixels | |
| Frame rate | 60 | |
| Input bytes read per request | 4 GiB | `ULTRAFAST_MAX_INPUT_GB` |
| Upload | 20 GiB | `ULTRAFAST_MAX_UPLOAD_GB` |
| Archive | 20,000 entries, 2 GiB unpacked, compression ratio 200 | |
| Synchronous wall time | 10 minutes | |
| Inline video | 32 MiB | |
| Requests waiting for the GPU | 16 | `ULTRAFAST_QUEUE` |
| Concurrent renders per key (Cloud) | per plan | |

One server renders one thing at a time on its GPU, in the order requests
come. Cloud runs many and puts synchronous requests ahead of jobs.

## GET /v1/health

`200` `{"ok": true, "ready": true, "busy": false, "device": "NVIDIA L4",
"backend": "vulkan", "version": "...", "git": "...", "image_digest":
"sha256:...", "uptime_s": 12.3, "queued": 0, "storage": "file"}`. `ready` is
false (with `503`) until the GPU is initialized. No authentication.

## Reproducibility

The same image digest, document, font files and input versions render
the same video. That is verified by comparing frames within a tolerance
(PSNR, see the contract tests), not promised bit for bit: encoders and
drivers may differ in the last bits.

## Change log

- 2026-10-13: first provisional version.
- 2026-10-13: `ultrafast-codec` in the object metadata; `X-Ultrafast-Sha256` on
  inline responses (additive).
- 2026-10-13: `clip_camera.faces` inline; `meta.fonts` pins enforced
  (additive).
- 2026-10-14: exact double parsing (`float_roundtrip`) wherever ultrafast reads
  documents; the canonical fixture covers two doubles the default parser
  misread.
- 2026-10-14: plain `sha256` metadata key; canonical JSON defined
  explicitly with a fixture; object names percent-decoded once;
  `meta.archives` member hints; every GCS call bounded by the deadline
  (additive).
- 2026-11-21: the self-hosted server (`ultrafast serve`). API keys and OIDC
  (`401 unauthorized`); storage on S3, R2, GCS or local disk; `output` is
  optional and a render without `inline` or `uri` is stored, with a signed
  `url` and flat `id`, `duration`, `frames`, `bytes`, `sha256` and
  `minutes` in the receipt; `attempt` and `deadline` are optional;
  `s3://` and `r2://` outputs and inputs; jobs with signed webhooks;
  `/v1/estimate`, `/v1/sheet`, `/v1/check`, `/v1/review`, `/v1/uploads`
  with `asset://` ids, `/v1/templates`, `/v1/me`, `/v1/files`; `at` for
  `t` in frame requests; public `https://` inputs without a host list;
  `404`, `409 job_started`, `429` per-caller concurrency (all additive).
- 2026-11-21: the open-source server and Cloud split. `ultrafast serve`
  takes one API key (`ULTRAFAST_API_KEY`) and stores on local disk; jobs
  and webhooks, `output.uri`, bucket inputs, several keys and OIDC are
  ultrafast Cloud's, and `ultrafast serve` answers them with
  `501 cloud_only` (new code, with `feature`). `/v1/me` has
  `webhook_secret` on Cloud only.

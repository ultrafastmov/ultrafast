# Self-hosting the render service

`ultrafast serve` runs the render service on one machine with a GPU: the v1
HTTP API ([API.md](API.md)) and the remote MCP server at `/mcp`, with one
API key and storage on local disk. It is enough for trying ultrafast, local
development, SDK tests and a small team sharing one box. Clients written
for ultrafast Cloud work against it by changing the base URL and the key;
what only Cloud has answers `501 cloud_only` (see below).

## Run it

With Docker on a machine with an NVIDIA GPU:

```bash
docker run --gpus all -p 8080:8080 -v ultrafast:/data \
  ghcr.io/ultrafastmov/ultrafast:latest serve
```

Or with the binary:

```bash
ultrafast serve            # 0.0.0.0:8080; --port and --host to change
```

With no key set, the server makes one on first start, saves it in
`$ULTRAFAST_DATA/api-key` and prints it in a box that starts with
`ultrafast serve: API key`:

```bash
docker logs <container> 2>&1 | grep -A3 'ultrafast serve: API key'
```

Then:

```bash
export ULTRAFAST_BASE_URL=http://localhost:8080 ULTRAFAST_API_KEY=uf_live_...
jq '{document: ., output: {inline: true}}' film.json |
curl $ULTRAFAST_BASE_URL/v1/render \
  -H "Authorization: Bearer $ULTRAFAST_API_KEY" \
  -H "Content-Type: application/json" \
  --data-binary @- -o film.mp4
```

The SDKs (`pip install ultrafastmov`, `npm i ultrafastmov`) read the same
two variables. `python3 tools/service_smoke.py` exercises every endpoint
against a running server.

## What it serves

| | `ultrafast serve` | ultrafast Cloud |
|---|---|---|
| `POST /v1/render`: inline, or stored with a signed download link | yes | yes |
| `/v1/frame`, `/v1/sheet`, `/v1/check`, `/v1/review`, `/v1/estimate` | yes | yes |
| `/v1/uploads` and `asset://` inputs; `https://` inputs | yes | yes |
| Remote MCP at `/mcp` with the API key as bearer token | yes | yes, and OAuth sign-in for Claude and ChatGPT connectors |
| Authentication | one API key | many keys with their own limits, OIDC sign-in |
| Storage | local disk | S3, R2 and GCS; renders written to your own bucket (`output.uri`); inputs read from buckets |
| `/v1/jobs` with signed webhooks | no | yes |
| GPUs | the machine's | many, scaled on demand |

On this server, jobs, bucket inputs, `output.uri` and Cloud-only settings
answer `501` with `{"error": {"code": "cloud_only", "feature": ...}}`, or
stop the server at start with a message naming the setting.

## Storage

`ULTRAFAST_STORAGE` is the directory stored renders (`renders/`) and
uploads (`assets/`) go in: a path or `file:///srv/ultrafast` (default:
`$ULTRAFAST_DATA/storage`).

Receipts carry a download `url` valid for `ULTRAFAST_URL_TTL_HOURS` (168):
the server signs it and serves the file at `/v1/files/` (byte ranges
included). Set `ULTRAFAST_PUBLIC_URL` when clients reach the server through
a proxy or another name; otherwise the URL is built from the request's
`Host` (and `X-Forwarded-Host` / `-Proto`).

## Authentication

| Setting | What it does |
|---|---|
| `ULTRAFAST_API_KEY` | The key callers send as `Authorization: Bearer <key>`. At least 16 characters. Without it, a generated key (above). |
| `ULTRAFAST_AUTH=none` | No authentication: every request is the caller `local`. Only on your own machine, or behind something else that authenticates. |

`ultrafast serve key` prints a new key. `GET /v1/health` and signed
`/v1/files/` URLs need no key.

## What requests may read

A request may read the uploads made with the server's key (`asset://`) and
public `https://` URLs (`ULTRAFAST_HTTPS_HOSTS` restricts them to a list of
hosts). Documents never read local paths: every file param must be a
reference (see API.md, Documents). The SDKs upload a document's local files
for you.

## The data directory

`ULTRAFAST_DATA` (default `$XDG_DATA_HOME/ultrafast`, `/data` in the image)
holds the server's own state: `secret` (signs download URLs; or set
`ULTRAFAST_SECRET`), the generated `api-key`, and, with no
`ULTRAFAST_STORAGE`, the stored files. Keep it on a volume.

## Limits and queueing

One server renders one thing at a time on its GPU. Requests (render,
frame, sheet, check, review, estimate, MCP tools) wait their turn; at most
`ULTRAFAST_QUEUE` (16) wait, then `429`. Renders stop after 10 minutes.
Other limits: `ULTRAFAST_MAX_DURATION_S` (3 hours of output),
`ULTRAFAST_MAX_INPUT_GB` (4 per request), `ULTRAFAST_MAX_UPLOAD_GB` (20),
`ULTRAFAST_INPUT_CACHE_GB` (8 of cached inputs). `ULTRAFAST_USD_PER_MINUTE`
puts a price in `/v1/estimate`.

## Remote MCP

`/mcp` serves the MCP tools over Streamable HTTP for agents that connect
by URL:

```bash
claude mcp add --transport http ultrafast http://localhost:8080/mcp \
  --header "Authorization: Bearer $ULTRAFAST_API_KEY"
```

Clients that can send a header connect this way. Connectors that only sign
in with OAuth (Claude's and ChatGPT's) use ultrafast Cloud's
`https://mcp.ultrafast.mov`; see [docs/MCP.md](../MCP.md).

## Untrusted callers

Documents can carry shaders and scripts. Scripts are bounded (no files,
network or clock, 5 s, 512 MB), inputs are only the request's own, and
https inputs must resolve to public addresses. GPU work from a shader is
bounded only by the request's deadline: give strangers their own server
(one tenant per GPU), as ultrafast Cloud does.

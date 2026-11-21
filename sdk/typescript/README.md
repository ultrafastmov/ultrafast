# ultrafastmov

TypeScript client for ultrafast, the GPU video engine. A video is a JSON
document; the render service turns it into an MP4. This package talks to
ultrafast Cloud or your own `ultrafast serve`.

```bash
npm i ultrafastmov
```

```typescript
import { Ultrafast } from "ultrafastmov";

const uf = new Ultrafast(); // ULTRAFAST_API_KEY

const doc = await uf.load("film.json");
const sheet = await uf.sheet(doc, { count: 9 });
const job = await uf.render(doc, {
  webhook: "https://example.com/rendered",
});
console.log(job.id, job.status);
```

Node 18 or later, no dependencies. ESM and CommonJS.

## Your own server

`ultrafast serve` is the open-source render service for one machine with a
GPU.

```bash
export ULTRAFAST_BASE_URL=http://localhost:8080
export ULTRAFAST_API_KEY=...   # the server's key (it prints one at first start)
```

or `new Ultrafast({ baseUrl: "http://localhost:8080", apiKey })`. Without
`ULTRAFAST_BASE_URL` the client uses ultrafast Cloud,
`https://api.ultrafast.mov`.

Everything here works against it except what only ultrafast Cloud has:
jobs and webhooks (`{ job }`, `{ webhook }`, `uf.jobs`), `{ uri }` output
to your own bucket, and `gs://`, `s3://` or `r2://` inputs. Those throw
`CloudOnlyError` (`e.feature` says which).

## Methods

```typescript
const doc = await uf.new("vertical");          // a starter document; names from uf.templates()

const frame = await uf.frame(doc, { at: 2.5 }); // Image: bytes, contentType, save(), toBase64()
await (await uf.sheet(doc, { count: 9 })).save("sheet.png");

const { ok, issues } = await uf.check(doc);
for (const issue of await uf.review(doc, { safe: "social" })) {
  console.log(issue.kind, issue.node, issue.from, issue.to, issue.message);
}
console.log(`${await uf.estimate(doc)}`);       // $0.004, 8 s of 1080p

const render = await uf.render(doc);            // stored; render.url is signed
await render.save("film.mp4");                  // streamed, sha256 checked
const short = await uf.render(doc, { inline: true, range: [0, 3] });

// One document, every deliverable: the settings are `ultrafast export`'s.
await uf.render(doc, { quality: "draft", size: "720p", inline: true });      // a quick preview
await uf.render(doc, { size: "4k", quality: "high" });                       // rendered at 4K, not scaled up
await uf.render(doc, { codec: "prores", alpha: true, name: "overlay.mov" }); // ProRes 4444 with alpha
await uf.render(doc, { format: "webm", alpha: true });                       // VP9 with alpha
await uf.render(doc, { format: "gif", fps: 15, size: "480p", loops: 0 });    // a GIF that plays once
await uf.render(doc, { format: "png", alpha: true });                        // a zip of PNG frames
console.log(`${await uf.estimate(doc, { size: "4k", quality: "master" })}`); // priced at the output size

const job = await uf.render(doc, { job: true }); // or { webhook } (Cloud)
const done = await job.wait();                  // polls; throws the job's error if it failed
await uf.jobs.get(job.id);

const asset = await uf.upload("interview.mp4"); // or a Blob, File or bytes
// "src": asset.uri
```

Every method that takes a document also takes the path of one.

## Local files

Documents can name local files: `"src": "footage/a.mp4"`, a font file, a
logo. Before sending a document the client uploads each one (once per
client, while its size and mtime stay the same) and sends a copy with
`asset://` references in their place; your object is not changed. Paths
resolve against the document's directory when it came from `uf.load`
(spread copies keep it), else the working directory. Font family names (`"Inter"`), inline face tracks
and `asset://`, `https://`, `data:` and (on Cloud) `gs://`, `s3://`, `r2://` values are
sent as they are. `uf.prepare(doc)` returns the copy without rendering.

Reading files needs Node. In edge runtimes, pass documents as objects whose
media are already references.

## Webhooks (Cloud)

```typescript
import { verifyWebhook } from "ultrafastmov";

const { webhook_secret } = await uf.me();

// in your handler, with the raw body:
const job = await verifyWebhook(rawBody, req.headers["ultrafast-signature"], webhook_secret);
if (job.status === "rendered") console.log(job.receipt?.url);
```

It checks the HMAC over the raw bytes and refuses deliveries whose
`sent_at` is more than 5 minutes off (`{ toleranceSec }`). It uses Web
Crypto, so it runs in Node and edge runtimes alike.

## Errors

Failures throw `UltrafastError` (or a subclass per API error code, such as
`InvalidDocumentError` with `issues`, `BusyError`, `OverLimitError`,
`CloudOnlyError` with `feature`), with
`status`, `code`, `message`, `retry` and the server's `body`. Requests the
server refused with `retry: true` (429, 503) are retried with backoff;
reads and other repeatable requests are also retried after a dropped
connection. A render is only resent after a dropped connection when it
writes to an `output.uri`, where the request's `attempt` ids make the
retry a replay.

A synchronous render holds the request open until the video is written,
and Node's fetch stops waiting for response headers after 5 minutes: on
Cloud, use a job for long renders.

## Types

`Document`, `Format`, `Node` (a union over `op` with each op's params),
`ParamsOf<"text">` and the animated forms (`keys`, `expr`, `wiggle`,
`oscillate`, `audio`) are generated from the document schema,
https://ultrafast.mov/schema/v1.json, by `scripts/gen-types.mjs`.

## Development

```bash
npm install
npm test          # builds, typechecks, runs the tests against an in-process fake server
npm run gen       # regenerates src/schema.ts from schema/v1.json
```

Licensed under Apache-2.0 OR MIT.

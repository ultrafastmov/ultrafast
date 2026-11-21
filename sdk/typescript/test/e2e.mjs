// End-to-end against a real `ultrafast serve` (or Cloud). Opt-in:
//
//   ULTRAFAST_BASE_URL=http://127.0.0.1:8080 ULTRAFAST_API_KEY=... npm run e2e
//
// Documents stay tiny (640x360, 1 to 2 s). Webhooks need a server that may
// call 127.0.0.1 (ULTRAFAST_WEBHOOK_PRIVATE=1 on a self-hosted one).

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { deflateSync } from "node:zlib";
import * as sdk from "../dist/esm/index.js";
import { startReceiver } from "./fake-server.mjs";

const { Ultrafast, verifyWebhook } = sdk;
const skip = !process.env.ULTRAFAST_BASE_URL || !process.env.ULTRAFAST_API_KEY ? "set ULTRAFAST_BASE_URL and ULTRAFAST_API_KEY" : false;

/** A solid-color PNG, w x h. */
function png(w, h, [r, g, b]) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const x of buf) c = crcTable[(c ^ x) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

const tiny = (extra = {}) => ({
  version: 1,
  format: { width: 640, height: 360, fps: 30, duration: 1 },
  nodes: {
    bg: { op: "solid", params: { color: "#1d3557" } },
    t: { op: "text", params: { text: "ultrafastmov", font: "Inter", size: 64, position: [320, 180], align: "center", color: "#f1faee" } },
    o: { op: "over", inputs: { fg: "t", bg: "bg" } },
    ...extra,
  },
  output: "o",
});

let uf;
let dir;
let receiver;
// Cloud gives each caller a webhook secret; a self-hosted `ultrafast serve`
// has no jobs or webhooks and must answer them with CloudOnlyError.
let cloud = false;
before(async () => {
  if (skip) return;
  uf = new Ultrafast();
  dir = await mkdtemp(join(tmpdir(), "ultrafastmov-e2e-"));
  receiver = await startReceiver();
  cloud = "webhook_secret" in (await uf.me());
});
after(() => receiver?.close());

test("health, me, templates, new", { skip }, async () => {
  const h = await uf.health();
  assert.equal(h.ok, true);
  const me = await uf.me();
  assert.ok(me.caller);
  if (cloud) assert.match(me.webhook_secret, /^whsec_/);
  const names = await uf.templates();
  assert.ok(names.includes("vertical"), names.join(","));
  const doc = await uf.new("vertical");
  assert.equal(doc.version, 1);
  assert.ok(doc.format.width > 0 && doc.nodes && doc.output);
  await assert.rejects(uf.new("no-such-template"), sdk.NotFoundError);
});

test("check, review, estimate, frame, sheet", { skip }, async () => {
  const doc = tiny();
  assert.deepEqual(await uf.check(doc), { ok: true, issues: [] });
  const bad = await uf.check({ ...doc, nodes: { ...doc.nodes, o: { op: "over", inputs: { fg: "missing", bg: "bg" } } } });
  assert.equal(bad.ok, false);
  assert.ok(bad.issues.length > 0 && bad.issues[0].message);

  const review = await uf.review(doc, { safe: "social", step: 0.25 });
  assert.equal(typeof review.seconds, "number");
  for (const i of review) assert.ok(i.kind && i.message);

  const est = await uf.estimate(doc);
  assert.equal(est.width, 640);
  assert.equal(est.frames, 30);
  assert.match(String(est), /^(\$[0-9.<]+, )?1 s of 360p$/);

  const f = await uf.frame(doc, { at: 0.5 });
  assert.equal(f.contentType, "image/png");
  assert.deepEqual([...f.bytes.subarray(0, 4)], [0x89, 0x50, 0x4e, 0x47]);
  const j = await uf.frame(doc, { at: 0.5, format: "jpeg" });
  assert.equal(j.contentType, "image/jpeg");
  const s = await uf.sheet(doc, { count: 4, columns: 2 });
  assert.equal(s.contentType, "image/png");
  await s.save(join(dir, "sheet.png"));
});

test("a document's local files upload and render", { skip }, async () => {
  await mkdir(join(dir, "art"), { recursive: true });
  await writeFile(join(dir, "art", "red.png"), png(64, 36, [230, 57, 70]));
  await writeFile(
    join(dir, "film.json"),
    JSON.stringify(tiny({ pic: { op: "image", params: { src: "art/red.png", fit: "cover" } }, o: { op: "over", inputs: { fg: "t", bg: "pic" } } })),
  );
  const doc = await uf.load(join(dir, "film.json"));
  const sent = await uf.prepare(doc);
  assert.match(sent.nodes.pic.params.src, /^asset:\/\//);
  assert.equal(doc.nodes.pic.params.src, "art/red.png");
  const frame = await uf.frame(doc, { at: 0 });
  assert.ok(frame.bytes.length > 100);

  const asset = await uf.upload(join(dir, "art", "red.png"));
  assert.equal(asset.content_type, "image/png");
  assert.equal(asset.filename, "red.png");
  assert.deepEqual((await uf.asset(asset.id)).uri, asset.uri);
  const blob = await uf.upload(new Blob([png(8, 8, [0, 0, 255])], { type: "image/png" }), { filename: "blue.png" });
  assert.equal(blob.bytes, png(8, 8, [0, 0, 255]).length);
});

test("render: stored, saved, inline", { skip }, async () => {
  const r = await uf.render(tiny());
  assert.equal(r.status, "rendered");
  assert.ok(r.url, "a stored render has a signed url");
  assert.equal(r.frames, 30);
  const path = await r.save(join(dir, "film.mp4"));
  const bytes = await readFile(path);
  assert.equal(bytes.length, r.bytes);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), r.sha256);
  assert.equal(bytes.subarray(4, 8).toString(), "ftyp");

  const inline = await uf.render(tiny(), { inline: true, range: [0, 0.5] });
  const ib = new Uint8Array(await inline.arrayBuffer());
  assert.equal(createHash("sha256").update(ib).digest("hex"), inline.sha256);
  assert.ok(inline.frames <= 16, `range [0, 0.5] gave ${inline.frames} frames`);
});

test("jobs: webhook, wait, verify, cancel, failure", { skip }, async () => {
  if (!cloud) {
    const cloudOnly = (feature) => (e) => {
      assert.ok(e instanceof sdk.CloudOnlyError, `${e.name} ${e.code}: ${e.message}`);
      assert.equal(e.status, 501);
      assert.equal(e.feature, feature);
      assert.match(e.message, /ultrafast Cloud/);
      return true;
    };
    await assert.rejects(uf.render(tiny({}), { webhook: receiver.url }), cloudOnly("jobs"));
    await assert.rejects(uf.render(tiny({}), { job: true }), cloudOnly("jobs"));
    await assert.rejects(uf.jobs.get("job_does_not_exist"), cloudOnly("jobs"));
    await assert.rejects(uf.render(tiny({}), { uri: "s3://my-bucket/x.mp4" }), cloudOnly("bucket_output"));
    await assert.rejects(
      uf.frame(tiny({ img: { op: "image", params: { src: "gs://my-bucket/a.png#1" } }, o: { op: "over", inputs: { fg: "img", bg: "bg" } } })),
      cloudOnly("bucket_inputs"),
    );
    return;
  }
  const job = await uf.render(tiny({}), { webhook: receiver.url });
  assert.match(job.id, /^job_/);
  const render = await job.wait({ pollMs: 200, timeoutMs: 60_000 });
  assert.equal(job.status, "rendered");
  await render.save(join(dir, "job.mp4"));

  const { webhook_secret } = await uf.me();
  await Promise.race([receiver.next(), new Promise((_, rej) => setTimeout(() => rej(new Error("no webhook within 20 s")), 20_000))]);
  const got = receiver.got.at(-1);
  const event = await verifyWebhook(got.raw, got.headers["ultrafast-signature"], webhook_secret);
  assert.equal(event.id, job.id);
  assert.equal(event.event, "job.rendered");
  assert.equal(got.headers["ultrafast-event"], "job.rendered");
  await assert.rejects(verifyWebhook(got.raw, got.headers["ultrafast-signature"], webhook_secret + "x"), sdk.WebhookVerificationError);

  assert.equal((await uf.jobs.get(job.id)).status, "rendered");
  assert.ok((await uf.jobs.list({ limit: 5 })).some((j) => j.id === job.id));
  await assert.rejects(job.cancel(), sdk.JobStartedError);

  const failing = await uf.render(tiny({ o: { op: "over", inputs: { fg: "missing", bg: "bg" } } }), { job: true });
  await assert.rejects(failing.wait({ pollMs: 200, timeoutMs: 60_000 }), (e) => {
    assert.ok(e instanceof sdk.UltrafastError);
    assert.equal(e.job.id, failing.id);
    return true;
  });
});

test("errors from the real server", { skip }, async () => {
  const wrong = new Ultrafast({ apiKey: "not-a-key" });
  await assert.rejects(wrong.me(), (e) => e instanceof sdk.UnauthorizedError && e.status === 401);
  await assert.rejects(uf.jobs.get("job_does_not_exist"), (e) =>
    cloud ? e instanceof sdk.NotFoundError && e.code === "not_found" : e instanceof sdk.CloudOnlyError);
  await assert.rejects(uf.render(tiny({ o: { op: "over", inputs: { fg: "missing", bg: "bg" } } })), (e) => {
    assert.ok(e instanceof sdk.InvalidDocumentError, `${e.name} ${e.code}: ${e.message}`);
    assert.ok(e.issues.length > 0);
    return true;
  });
  await assert.rejects(uf.render(tiny(), { uri: "s3://not-yours/x.mp4" }), (e) => {
    assert.ok(e instanceof sdk.UltrafastError && e.status >= 400, `${e.name}`);
    return true;
  });
});

test("the CommonJS build against the server", { skip }, async () => {
  const cjs = createRequire(import.meta.url)("../dist/cjs/index.js");
  const c = new cjs.Ultrafast();
  assert.equal((await c.check(tiny())).ok, true);
});

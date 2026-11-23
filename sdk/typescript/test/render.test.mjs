// Renders: stored with a signed URL, inline, create-only replays; and jobs
// with signed webhooks.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import {
  JobStartedError,
  Render,
  RenderFailedError,
  Ultrafast,
  UltrafastError,
  WebhookVerificationError,
  verifyWebhook,
} from "../dist/esm/index.js";
import { startFakeServer, startReceiver } from "./fake-server.mjs";

const doc = { version: 1, format: { width: 1920, height: 1080, fps: 30, duration: 2 }, nodes: { bg: { op: "solid", params: { color: "#101418" } } }, output: "bg" };

let server;
let receiver;
let uf;
let out;

before(async () => {
  server = await startFakeServer();
  receiver = await startReceiver();
  uf = new Ultrafast({ apiKey: "example-key", baseUrl: server.url });
  out = await mkdtemp(join(tmpdir(), "ultrafastmov-out-"));
});
after(async () => {
  await server.close();
  await receiver.close();
});

test("render() stores the video and save() streams it to disk", async () => {
  const r = await uf.render(doc);
  assert.ok(r instanceof Render);
  assert.equal(r.status, "rendered");
  assert.match(r.id, /^render_/);
  assert.match(r.url, /\/v1\/files\/renders\/.+\/film\.mp4\?expires=/);
  assert.equal(r.duration, 2);
  assert.equal(r.frames, 60);

  const sent = server.requestsTo("POST", "/v1/render").at(-1).json;
  assert.deepEqual(sent.output, {});
  assert.match(sent.attempt.render_id, /^render_[A-Za-z0-9]+$/);
  assert.match(sent.attempt.attempt_id, /^attempt_[A-Za-z0-9]+$/);

  const path = await r.save(join(out, "film.mp4"));
  const bytes = await readFile(path);
  assert.equal(bytes.length, r.bytes);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), r.sha256);
  assert.deepEqual(new Uint8Array(await r.arrayBuffer()), new Uint8Array(bytes));
  const dl = server.requestsTo("GET", server.state.requests.at(-1).path).at(-1);
  assert.equal(dl.headers.authorization, "Bearer example-key", "same-origin downloads carry the key");
});

test("save() refuses a download that does not match the receipt", async () => {
  const r = await uf.render(doc);
  server.state.corruptDownloads = true;
  try {
    await assert.rejects(r.save(join(out, "bad.mp4")), { code: "integrity" });
    const left = await readdir(out);
    assert.ok(!left.some((f) => f.startsWith("bad.mp4")), "no partial file left behind");
  } finally {
    server.state.corruptDownloads = false;
  }
});

test("export settings reach the output; estimates take them too", async () => {
  await uf.render(doc, { codec: "prores", alpha: true, size: "4k", fps: 60, quality: "master", prores_profile: "4444" });
  let sent = server.requestsTo("POST", "/v1/render").at(-1);
  assert.deepEqual(sent.json.output, { codec: "prores", alpha: true, size: "4k", fps: 60, quality: "master", prores_profile: "4444" });
  await uf.render(doc, { format: "webm", size: [1280, 720], bitrate: "8M" });
  sent = server.requestsTo("POST", "/v1/render").at(-1);
  assert.deepEqual(sent.json.output, { format: "webm", size: [1280, 720], bitrate: "8M" });
  await uf.estimate(doc, { size: "4k", quality: "high", range: [0, 1] });
  sent = server.requestsTo("POST", "/v1/estimate").at(-1);
  assert.deepEqual(sent.json.output, { size: "4k", quality: "high" });
  assert.deepEqual(sent.json.range, [0, 1]);
});

test("inline renders carry the mp4 and the receipt header", async () => {
  const r = await uf.render(doc, { inline: true, range: [0, 1], codec: "hevc" });
  assert.equal(r.url, null);
  assert.ok(r.sha256);
  const sent = server.requestsTo("POST", "/v1/render").at(-1);
  assert.deepEqual(sent.json.output, { inline: true, codec: "hevc", range: [0, 1] });
  assert.match(sent.headers.accept, /video\/mp4/);
  const bytes = new Uint8Array(await r.arrayBuffer());
  assert.equal(createHash("sha256").update(bytes).digest("hex"), r.sha256);
  await r.save(join(out, "inline.mp4"));
  assert.deepEqual(new Uint8Array(await readFile(join(out, "inline.mp4"))), bytes);
});

test("create-only renders retry after a dropped connection and accept the replay", async () => {
  server.state.dropAfterRender = true;
  const r = await uf.render(doc, { uri: "s3://bucket/renders/a.mp4" });
  assert.equal(r.status, "already_rendered");
  const sends = server.requestsTo("POST", "/v1/render").slice(-2);
  assert.deepEqual(sends[0].json.attempt, sends[1].json.attempt, "the retry sends the same attempt");
  await assert.rejects(r.save(join(out, "x.mp4")), /cannot sign/);
});

test("a stored render is not resent after a dropped connection", async () => {
  const before = server.requestsTo("POST", "/v1/render").length;
  server.fail("POST", "/v1/render", { drop: true });
  await assert.rejects(uf.render(doc), { name: "ConnectionError", code: "connection_error" });
  assert.equal(server.requestsTo("POST", "/v1/render").length, before + 1);
});

test("job flow: create with a webhook, wait, verify the delivery", async () => {
  const job = await uf.render(doc, { webhook: receiver.url });
  assert.equal(job.status, "queued");
  assert.equal(job.webhook, receiver.url);
  const again = await uf.jobs.get(job.id);
  assert.equal(again.id, job.id);

  const render = await job.wait({ pollMs: 5 });
  assert.equal(job.status, "rendered");
  assert.ok(render instanceof Render);
  await render.save(join(out, "job.mp4"));

  await receiver.next();
  const { headers, raw } = receiver.got.at(-1);
  assert.equal(headers["ultrafast-event"], "job.rendered");
  const { webhook_secret } = await uf.me();
  const event = await verifyWebhook(raw, headers["ultrafast-signature"], webhook_secret);
  assert.equal(event.id, job.id);
  assert.equal(event.event, "job.rendered");
  assert.equal(event.receipt.sha256, render.sha256);
  // The same bytes as a string verify too.
  await verifyWebhook(raw.toString("utf8"), headers["ultrafast-signature"], webhook_secret);

  await assert.rejects(verifyWebhook(raw, headers["ultrafast-signature"], "whsec_wrong"), (e) => e instanceof WebhookVerificationError && e.code === "bad_signature");
  const tampered = Buffer.from(raw.toString().replace("job.rendered", "job.failed!"));
  await assert.rejects(verifyWebhook(tampered, headers["ultrafast-signature"], webhook_secret), { code: "bad_signature" });
  await assert.rejects(verifyWebhook(raw, "sha256=zz", webhook_secret), { code: "bad_signature" });
  await assert.rejects(verifyWebhook(raw, undefined, webhook_secret), { code: "bad_signature" });
  const late = new Date(Date.parse(event.sent_at) + 301_000);
  await assert.rejects(verifyWebhook(raw, headers["ultrafast-signature"], webhook_secret, { now: late }), { code: "stale" });
  await verifyWebhook(raw, headers["ultrafast-signature"], webhook_secret, { now: late, toleranceSec: 600 });
});

test("job.save(), list, cancel, and a failed job", async () => {
  const job = await uf.render(doc, { job: true });
  assert.equal(job.webhook, null);
  await job.save(join(out, "job2.mp4"), { pollMs: 5 });
  assert.ok((await readFile(join(out, "job2.mp4"))).length > 0);

  const listed = await uf.jobs.list({ limit: 1 });
  assert.equal(listed[0].id, job.id);

  const queued = await uf.jobs.create(doc);
  await queued.cancel();
  assert.equal(queued.status, "canceled");
  await assert.rejects(queued.wait(), { code: "canceled" });

  const started = await uf.jobs.create(doc);
  await started.refresh();
  assert.equal(started.status, "rendering");
  await assert.rejects(started.cancel(), (e) => e instanceof JobStartedError && e.status === 409);

  const failing = await uf.jobs.create({ ...doc, meta: { fail: true } });
  await assert.rejects(failing.wait({ pollMs: 5 }), (e) => {
    assert.ok(e instanceof RenderFailedError);
    assert.equal(e.retry, true);
    assert.equal(e.job.id, failing.id);
    return true;
  });

  // A job that never finishes, so the timeout is the only way out of wait().
  const slow = await uf.jobs.create({ ...doc, meta: { hold: true } });
  await assert.rejects(slow.wait({ pollMs: 5, timeoutMs: 40 }), (e) => e instanceof UltrafastError && e.code === "timeout");
  assert.equal(slow.status, "rendering", "wait() polled before timing out");
});

test("jobs refuse inline output; job creation is not resent after a drop", async () => {
  const before = server.requestsTo("POST", "/v1/jobs").length;
  server.fail("POST", "/v1/jobs", { drop: true });
  await assert.rejects(uf.render(doc, { job: true }), { code: "connection_error" });
  assert.equal(server.requestsTo("POST", "/v1/jobs").length, before + 1);
  await assert.rejects(uf.render(doc, { job: true, inline: true }), { name: "BadRequestError", status: 0 });
  assert.equal(server.requestsTo("POST", "/v1/jobs").length, before + 1, "nothing sent");
});

// Estimates, reviews, images, uploads, the CommonJS build.

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { Estimate, OPS, Review, Ultrafast, VERSION, isOp } from "../dist/esm/index.js";
import { PNG, startFakeServer } from "./fake-server.mjs";

const doc = { version: 1, format: { width: 1920, height: 1080, fps: 30, duration: 8 }, nodes: { bg: { op: "solid" } }, output: "bg" };

let server;
let uf;
before(async () => {
  server = await startFakeServer();
  uf = new Ultrafast({ apiKey: "example-key", baseUrl: server.url });
});
after(() => server.close());

test("estimate formatting", async () => {
  const est = (o) => String(new Estimate({ width: 1920, height: 1080, seconds: 8, ...o }));
  assert.equal(est({ usd: 0.004 }), "$0.004, 8 s of 1080p");
  assert.equal(est({ usd: null }), "8 s of 1080p");
  assert.equal(est({ usd: null, gpu_seconds: 2.14 }), "8 s of 1080p, 2.1 GPU seconds");
  assert.equal(est({ usd: null, gpu_seconds: 0.043 }), "8 s of 1080p, 0.04 GPU seconds");
  assert.equal(est({ usd: null, gpu_seconds: 1.02 }), "8 s of 1080p, 1 GPU second");
  assert.equal(est({ usd: 0.004, gpu_seconds: 2.1 }), "$0.004, 8 s of 1080p");
  assert.equal(est({ usd: 0.006, seconds: 11.6, width: 1080, height: 1920 }), "$0.006, 11.6 s of 1080p");
  assert.equal(est({ usd: 1.234, seconds: 754 }), "$1.23, 12.6 min of 1080p");
  assert.equal(est({ usd: 0, width: 3840, height: 2160 }), "$0, 8 s of 2160p");
  assert.equal(est({ usd: 0.00004 }), "<$0.0001, 8 s of 1080p");

  server.state.usdPerMinute = 0.03;
  const e = await uf.estimate(doc);
  assert.equal(`${e}`, "$0.004, 8 s of 1080p");
  assert.equal(e.device, "NVIDIA L4");
  assert.equal(server.requestsTo("POST", "/v1/estimate").at(-1).json.range, null);
  server.state.usdPerMinute = null;
  assert.equal(String(await uf.estimate(doc, { range: [0, 4] })), "4 s of 1080p, 2.1 GPU seconds");
});

test("review is iterable over its issues", async () => {
  const r = await uf.review(doc, { safe: "social", step: 0.2 });
  assert.ok(r instanceof Review);
  assert.equal(r.frames, 240);
  const seen = [];
  for (const issue of r) seen.push([issue.kind, issue.node, issue.message]);
  assert.deepEqual(seen, [["safe_area", "title", "under the social safe area"]]);
  assert.deepEqual(server.requestsTo("POST", "/v1/review").at(-1).json, { document: doc, safe: "social", step: 0.2 });
});

test("images: bytes, base64, data URL, save", async () => {
  const img = await uf.sheet(doc, { at: [0, 1, 2], columns: 3, format: "png" });
  assert.deepEqual(Buffer.from(img.bytes), PNG);
  assert.equal(img.toBase64(), PNG.toString("base64"));
  assert.ok(img.toDataUrl().startsWith("data:image/png;base64,"));
  const dir = await mkdtemp(join(tmpdir(), "ultrafastmov-img-"));
  await img.save(join(dir, "sheet.png"));
  assert.deepEqual(await readFile(join(dir, "sheet.png")), PNG);
  const body = server.requestsTo("POST", "/v1/sheet").at(-1).json;
  assert.deepEqual(body, { document: doc, at: [0, 1, 2], columns: 3, format: "png" });
});

test("upload() takes paths, blobs and bytes", async () => {
  const dir = await mkdtemp(join(tmpdir(), "ultrafastmov-up-"));
  const file = join(dir, "interview.mp4");
  await import("node:fs/promises").then((fs) => fs.writeFile(file, "x".repeat(70_000)));
  const a = await uf.upload(file);
  assert.equal(a.filename, "interview.mp4");
  assert.equal(a.content_type, "video/mp4");
  assert.equal(a.bytes, 70_000);
  assert.match(a.uri, /^asset:\/\//);
  assert.deepEqual(await uf.asset(a.id), a);

  const b = await uf.upload(new Blob(["<svg/>"], { type: "image/svg+xml" }), { filename: "logo.svg" });
  assert.equal(b.content_type, "image/svg+xml");
  assert.equal(b.filename, "logo.svg");
  const c = await uf.upload(new TextEncoder().encode("WEBVTT"), { filename: "captions.vtt" });
  assert.equal(c.content_type, "text/vtt");
  const f = await uf.upload(new File(["ttf"], "Brand.ttf"));
  assert.equal(f.filename, "Brand.ttf");
  assert.equal(f.content_type, "font/ttf");
});

test("me, health, user agent", async () => {
  assert.equal((await uf.me()).caller, "ci");
  assert.equal((await uf.health()).ready, true);
  const ua = server.state.requests.at(-1).headers["user-agent"];
  assert.equal(ua, `ultrafastmov-js/${VERSION}`);
  const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(VERSION, pkg.version);
});

test("default base URL is a local ultrafast serve", () => {
  delete process.env.ULTRAFAST_BASE_URL;
  assert.equal(new Ultrafast({ apiKey: "k" }).baseUrl, "http://127.0.0.1:8080");
  assert.equal(new Ultrafast({ apiKey: "k", baseUrl: "http://localhost:8080/" }).baseUrl, "http://localhost:8080");
});

test("schema exports", () => {
  assert.ok(OPS.includes("text") && OPS.includes("engrave"));
  assert.equal(isOp({ op: "text", params: {} }, "text"), true);
  assert.equal(isOp({ op: "lower_third" }, "text"), false);
});

test("the CommonJS build works the same", async () => {
  const require = createRequire(import.meta.url);
  const cjs = require("../dist/cjs/index.js");
  const c = new cjs.Ultrafast({ apiKey: "example-key", baseUrl: server.url });
  assert.equal(String(await c.estimate(doc)), "8 s of 1080p, 2.1 GPU seconds");
  await assert.rejects(c.new("nope"), (e) => e instanceof cjs.NotFoundError);
  const sig = "sha256=" + (await import("node:crypto")).createHmac("sha256", "s").update("{}").digest("hex");
  await assert.rejects(cjs.verifyWebhook("{}", sig, "s"), { code: "bad_body" });
});

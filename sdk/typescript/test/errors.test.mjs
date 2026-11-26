// Error mapping (every fixture in docs/service/fixtures/errors) and retries.

import assert from "node:assert/strict";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { after, before, test } from "node:test";
import * as sdk from "../dist/esm/index.js";
import { FIXTURES, fixture, startFakeServer } from "./fake-server.mjs";

const { Ultrafast, UltrafastError } = sdk;
const doc = { version: 1, format: { width: 64, height: 64, fps: 30, duration: 1 }, nodes: { bg: { op: "solid" } }, output: "bg" };

const CLASS = {
  bad_request: "BadRequestError",
  invalid_document: "InvalidDocumentError",
  unauthorized: "UnauthorizedError",
  payment_required: "PaymentRequiredError",
  not_found: "NotFoundError",
  job_started: "JobStartedError",
  input_not_allowed: "InputNotAllowedError",
  local_path_not_allowed: "LocalPathNotAllowedError",
  output_not_allowed: "OutputNotAllowedError",
  output_conflict: "OutputConflictError",
  over_limit: "OverLimitError",
  input_unavailable: "InputUnavailableError",
  busy: "BusyError",
  render_failed: "RenderFailedError",
  unavailable: "UnavailableError",
  deadline_exceeded: "DeadlineExceededError",
  cloud_only: "CloudOnlyError",
};

let server;
let uf;
before(async () => {
  server = await startFakeServer();
  uf = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 0 });
});
after(() => server.close());

for (const file of readdirSync(join(FIXTURES, "errors"))) {
  test(`maps ${file}`, async () => {
    const body = fixture(join("errors", file));
    const e = body.error;
    server.fail("POST", "/v1/estimate", { status: e.status, body });
    await assert.rejects(uf.estimate(doc), (err) => {
      assert.ok(err instanceof UltrafastError);
      assert.ok(err instanceof sdk[CLASS[e.code]], `${e.code} is ${CLASS[e.code]}, got ${err.name}`);
      assert.equal(err.name, CLASS[e.code]);
      assert.equal(err.status, e.status);
      assert.equal(err.code, e.code);
      assert.equal(err.retry, e.retry);
      assert.equal(err.message, e.message);
      assert.deepEqual(err.body, e);
      for (const k of ["issues", "location", "reason", "node", "param", "value", "limit", "max", "object"]) {
        if (k in e) assert.deepEqual(err[k], e[k], k);
      }
      return true;
    });
  });
}

test("check() answers invalid_document as issues", async () => {
  const body = fixture("errors/400-invalid-document.json");
  server.fail("POST", "/v1/check", { status: 400, body });
  const r = await uf.check(doc);
  assert.equal(r.ok, false);
  assert.equal(r.issues.length, 2);
  assert.equal(r.issues[1].node, "stack");
  const fine = await uf.check({ ...doc, nodes: { bg: { op: "bogus" } } });
  assert.deepEqual(fine, { ok: false, issues: [{ node: "bg", severity: "error", message: "unknown op `bogus`" }] });
});

test("401 without a key says how to set one", async () => {
  const anon = new Ultrafast({ baseUrl: server.url, apiKey: undefined });
  delete process.env.ULTRAFAST_API_KEY;
  await assert.rejects(anon.me(), (e) => {
    assert.ok(e instanceof sdk.UnauthorizedError);
    assert.equal(e.status, 401);
    assert.match(e.message, /ULTRAFAST_API_KEY/);
    return true;
  });
  const wrong = new Ultrafast({ baseUrl: server.url, apiKey: "nope" });
  await assert.rejects(wrong.me(), { name: "UnauthorizedError", code: "unauthorized" });
  assert.equal((await anon.health()).ok, true, "health needs no key");
});

test("non-API error bodies become http_error", async () => {
  server.fail("POST", "/v1/review", { status: 502, body: undefined, headers: {} });
  server.state.faults.at(-1).body = "<html>bad gateway</html>";
  const err = await uf.review(doc).catch((e) => e);
  assert.equal(err.code, "http_error");
  assert.equal(err.status, 502);
  assert.match(err.message, /HTTP 502/);
});

test("429 and 503 with retry: true are retried, honoring Retry-After", async () => {
  const r = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 2 });
  const before = server.requestsTo("POST", "/v1/render").length;
  server.fail("POST", "/v1/render", { status: 429, body: fixture("errors/429-busy.json"), headers: { "retry-after": "0" } });
  server.fail("POST", "/v1/render", { status: 503, body: fixture("errors/503-unavailable.json"), headers: { "retry-after": "0" } });
  const render = await r.render(doc);
  assert.equal(render.status, "rendered");
  const sends = server.requestsTo("POST", "/v1/render").slice(before);
  assert.equal(sends.length, 3);
  assert.equal(new Set(sends.map((s) => JSON.stringify(s.json.attempt))).size, 1, "one attempt across retries");
});

test("retries stop at maxRetries and surface the last error", async () => {
  const r = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 1 });
  for (let i = 0; i < 2; i++) server.fail("POST", "/v1/sheet", { status: 429, body: fixture("errors/429-busy.json"), headers: { "retry-after": "0" } });
  await assert.rejects(r.sheet(doc), { name: "BusyError", retry: true });
});

test("non-retryable errors are not retried", async () => {
  const r = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 3 });
  const before = server.requestsTo("POST", "/v1/render").length;
  server.fail("POST", "/v1/render", { status: 500, body: fixture("errors/500-render-failed.json") });
  await assert.rejects(r.render(doc), { name: "RenderFailedError" });
  assert.equal(server.requestsTo("POST", "/v1/render").length, before + 1);
});

test("idempotent calls retry dropped connections", async () => {
  const r = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 1 });
  server.fail("POST", "/v1/frame", { drop: true });
  const img = await r.frame(doc, { at: 0.5, format: "jpeg" });
  assert.equal(img.contentType, "image/jpeg");
  assert.ok(img.receipt.timings);
});

test("frames ask for alpha only when told to", async () => {
  const r = new Ultrafast({ apiKey: "example-key", baseUrl: server.url, maxRetries: 0 });
  const before = server.requestsTo("POST", "/v1/frame").length;
  await r.frame(doc, { at: 0.5 });
  await r.frame(doc, { at: 0.5, alpha: true });
  const [plain, alpha] = server.requestsTo("POST", "/v1/frame").slice(before).map((r) => JSON.parse(r.raw.toString("utf8")));
  assert.equal("alpha" in plain, false);
  assert.equal(alpha.alpha, true);
});

test("a server that is not there is a ConnectionError", async () => {
  const r = new Ultrafast({ apiKey: "k", baseUrl: "http://127.0.0.1:9", maxRetries: 0 });
  await assert.rejects(r.me(), (e) => e instanceof sdk.ConnectionError && e.status === 0 && e.code === "connection_error");
});

test("abort signals cancel requests and waits", async () => {
  const ac = new AbortController();
  ac.abort(new Error("stop"));
  await assert.rejects(uf.estimate(doc, { signal: ac.signal }), /stop/);
});

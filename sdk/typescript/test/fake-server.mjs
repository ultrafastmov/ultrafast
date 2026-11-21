// An in-process stand-in for `ultrafast serve`, following docs/service/API.md
// closely enough to test the client: auth, uploads, stored and inline
// renders with signed downloads, create-only replays, jobs with signed
// webhooks, and the free endpoints. Faults can be scripted per path.

import { createHash, createHmac, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const FIXTURES = join(here, "..", "..", "..", "docs", "service", "fixtures");
export const fixture = (name) => JSON.parse(readFileSync(join(FIXTURES, name), "utf8"));

// 1x1 PNG.
export const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
  "base64",
);

const FILE_PARAMS = {
  image: ["src"], video: ["src"], audio: ["src"], svg: ["src"], lottie: ["src"], lut: ["src"],
  mesh: ["src"], captions: ["src"], clip_camera: ["src"], block: ["logo"],
};
const REF = /^(asset|https?|gs|s3|r2):\/\/|^data:/;

const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const error = (status, code, message, extra = {}) => ({ error: { status, code, message, retry: status === 429 || status === 503 || status === 500, ...extra } });

export async function startFakeServer({ keys = ["test-key"], secret = "whsec_test_secret" } = {}) {
  const state = {
    requests: [], // { method, path, headers, body }
    uploads: new Map(), // id -> { bytes, filename, content_type }
    files: new Map(), // path under /v1/files -> bytes
    objects: new Map(), // output.uri -> { receipt, attempt }
    jobs: new Map(),
    faults: [], // { method, path, status?, code?, drop?, count }
    webhooks: [], // deliveries: { url, status }
    secret,
    corruptDownloads: false,
  };
  let base = "";
  let seq = 0;

  const fault = (method, path) => {
    const f = state.faults.find((x) => x.count > 0 && x.method === method && path.startsWith(x.path));
    if (f) f.count--;
    return f;
  };

  const receiptFor = (doc, opts = {}) => {
    const id = `render_${++seq}`;
    const mp4 = Buffer.from(`fake mp4 of ${JSON.stringify(doc).length} bytes for ${id}`);
    const filePath = `renders/${id}/${opts.name ?? "film.mp4"}`;
    state.files.set(filePath, mp4);
    const fps = doc.format?.fps ?? 30;
    const duration = doc.format?.duration ?? 1;
    return {
      mp4,
      receipt: {
        receipt_version: 1,
        id,
        status: "rendered",
        url: opts.inline ? undefined : `${base}/v1/files/${filePath}?expires=1764460800&sig=${sha256(filePath).slice(0, 16)}`,
        url_expires_at: "2026-11-28T00:00:00Z",
        duration,
        frames: Math.round(duration * fps),
        bytes: mp4.length,
        sha256: sha256(mp4),
        minutes: duration / 60,
        attempt: opts.attempt,
        video: { frames: Math.round(duration * fps), fps, width: doc.format?.width, height: doc.format?.height, duration, codec: "h264", audio: true, range: [0, duration] },
        created_at: new Date().toISOString(),
      },
    };
  };

  /** 403 local_path_not_allowed for any file param that is not a reference, as the server does. */
  const localPath = (doc) => {
    for (const [id, n] of Object.entries(doc?.nodes ?? {})) {
      for (const p of FILE_PARAMS[n.op] ?? []) {
        const v = n.params?.[p];
        if (typeof v === "string" && v && !REF.test(v)) {
          return error(403, "local_path_not_allowed", `file params must be references`, { node: id, param: p, value: v });
        }
      }
    }
    return null;
  };

  const deliver = async (job) => {
    if (!job.webhook) return;
    const body = JSON.stringify({ ...job, event: job.status === "rendered" ? "job.rendered" : "job.failed", sent_at: new Date().toISOString() });
    const sig = "sha256=" + createHmac("sha256", state.secret).update(body).digest("hex");
    try {
      const res = await fetch(job.webhook, {
        method: "POST",
        headers: { "content-type": "application/json", "ultrafast-event": "job.rendered", "ultrafast-delivery": `dlv_${job.id}`, "ultrafast-signature": sig },
        body,
      });
      state.webhooks.push({ url: job.webhook, status: res.status });
    } catch (e) {
      state.webhooks.push({ url: job.webhook, status: 0, error: String(e) });
    }
  };

  const advance = async (job) => {
    if (job.status === "queued") {
      job.status = "rendering";
      job.started_at = new Date().toISOString();
      job.progress = { frames: 0, of: 30 };
    } else if (job.status === "rendering") {
      if (job.hold) return; // `meta.hold`: renders until the test ends
      if (job.fail) {
        job.status = "failed";
        job.error = error(500, "render_failed", "render failed: GPU device lost (renderer rebuilt)").error;
      } else {
        job.status = "rendered";
        job.receipt = receiptFor(job.document, { attempt: job.attempt }).receipt;
        job.progress = { frames: 30, of: 30 };
      }
      job.finished_at = new Date().toISOString();
      const { document, fail, hold, ...pub } = job;
      await deliver(pub);
    }
  };
  const publicJob = ({ document, fail, hold, ...pub }) => pub;

  const server = createServer(async (req, res) => {
    const url = new URL(req.url, base);
    const path = url.pathname;
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const raw = Buffer.concat(chunks);
    const entry = { method: req.method, path, query: Object.fromEntries(url.searchParams), headers: req.headers, raw };
    state.requests.push(entry);

    const send = (status, body, headers = {}) => {
      if (Buffer.isBuffer(body)) {
        res.writeHead(status, headers);
        return res.end(body);
      }
      res.writeHead(status, { "content-type": "application/json", ...headers });
      res.end(JSON.stringify(body));
    };

    let json;
    if ((req.headers["content-type"] ?? "").startsWith("application/json")) {
      try {
        json = JSON.parse(raw.toString("utf8"));
      } catch {
        return send(400, error(400, "bad_request", "malformed JSON"));
      }
      entry.json = json;
    }

    const f = fault(req.method, path);
    if (f?.drop) return req.socket.destroy();
    if (f) return send(f.status, f.body ?? error(f.status, f.code, f.message ?? f.code), f.headers);

    if (path === "/v1/health") return send(200, fixture("health.json"));
    if (path.startsWith("/v1/files/")) {
      const bytes = state.files.get(path.slice("/v1/files/".length));
      if (!bytes || !url.searchParams.get("sig")) return send(404, error(404, "not_found", "no such file"));
      return send(200, state.corruptDownloads ? Buffer.from("corrupted") : bytes, { "content-type": "video/mp4" });
    }
    const auth = req.headers.authorization ?? "";
    if (!keys.includes(auth.replace(/^Bearer /, ""))) {
      return send(401, error(401, "unauthorized", "missing, unknown or expired key"), { "www-authenticate": "Bearer" });
    }


    const m = (method, re) => req.method === method && re.exec(path);
    let g;
    if (m("GET", /^\/v1\/me$/)) return send(200, { caller: "ci", auth: "key", webhook_secret: state.secret });
    if (m("GET", /^\/v1\/templates$/)) return send(200, { templates: ["vertical", "title", "square", "promo", "explainer", "product"] });
    if ((g = m("GET", /^\/v1\/templates\/([^/]+)$/))) {
      if (g[1] !== "vertical") return send(404, error(404, "not_found", `no template ${g[1]}`));
      return send(200, { version: 1, format: { width: 1080, height: 1920, fps: 30, duration: 8 }, nodes: { bg: { op: "solid", params: { color: "#101418" } } }, output: "bg" });
    }
    if (m("POST", /^\/v1\/uploads$/)) {
      const id = randomBytes(6).toString("hex");
      const asset = {
        id: `asset_${id}`,
        uri: `asset://${id}`,
        bytes: raw.length,
        sha256: sha256(raw),
        filename: url.searchParams.get("filename") ?? "upload",
        content_type: req.headers["content-type"] ?? "application/octet-stream",
        created_at: new Date().toISOString(),
      };
      state.uploads.set(asset.id, { ...asset, data: raw });
      return send(201, asset);
    }
    if ((g = m("GET", /^\/v1\/uploads\/([^/]+)$/))) {
      const a = state.uploads.get(g[1]);
      if (!a) return send(404, error(404, "not_found", "no such upload"));
      const { data, ...asset } = a;
      return send(200, asset);
    }

    const doc = json?.document;
    const bad = doc && localPath(doc);
    if (bad && /^\/v1\/(render|jobs|frame|sheet|review|estimate|check)$/.test(path) && req.method === "POST") return send(403, bad);

    if (m("POST", /^\/v1\/render$/)) {
      const out = json.output ?? {};
      if (out.uri) {
        const existing = state.objects.get(out.uri);
        if (existing) {
          const same = existing.attempt?.render_id === json.attempt?.render_id && existing.attempt?.attempt_id === json.attempt?.attempt_id;
          if (same) return send(409, { ...existing.receipt, status: "already_rendered" });
          return send(409, error(409, "output_conflict", "the output object exists and is not this request's render", { object: { uri: out.uri } }));
        }
        const { receipt } = receiptFor(doc, { attempt: json.attempt, name: out.name });
        receipt.url = null;
        receipt.object = { uri: out.uri, size: receipt.bytes, sha256: receipt.sha256, content_type: "video/mp4" };
        state.objects.set(out.uri, { receipt, attempt: json.attempt });
        if (state.dropAfterRender) {
          state.dropAfterRender = false;
          return req.socket.destroy();
        }
        return send(200, receipt);
      }
      const { mp4, receipt } = receiptFor(doc, { attempt: json.attempt, inline: out.inline, name: out.name });
      if (out.inline) {
        const { url, ...compact } = receipt;
        return send(200, mp4, { "content-type": "video/mp4", "x-ultrafast-receipt": JSON.stringify(compact), "x-ultrafast-sha256": receipt.sha256 });
      }
      return send(200, receipt);
    }
    if (m("POST", /^\/v1\/jobs$/)) {
      if (json.output?.inline) return send(400, error(400, "bad_request", "`output.inline` is not allowed on jobs"));
      const job = {
        id: `job_${++seq}`,
        status: "queued",
        created_at: new Date().toISOString(),
        started_at: null,
        finished_at: null,
        webhook: json.webhook ?? null,
        progress: null,
        receipt: null,
        error: null,
        document: doc,
        attempt: json.attempt,
        fail: doc?.meta?.fail === true,
        hold: doc?.meta?.hold === true,
      };
      state.jobs.set(job.id, job);
      return send(202, publicJob(job));
    }
    if (m("GET", /^\/v1\/jobs$/)) {
      const limit = Number(url.searchParams.get("limit") ?? 20);
      return send(200, { jobs: [...state.jobs.values()].reverse().slice(0, limit).map(publicJob) });
    }
    if ((g = m("GET", /^\/v1\/jobs\/([^/]+)$/))) {
      const job = state.jobs.get(g[1]);
      if (!job) return send(404, error(404, "not_found", "no such job"));
      await advance(job);
      return send(200, publicJob(job));
    }
    if ((g = m("DELETE", /^\/v1\/jobs\/([^/]+)$/))) {
      const job = state.jobs.get(g[1]);
      if (!job) return send(404, error(404, "not_found", "no such job"));
      if (job.status !== "queued") return send(409, error(409, "job_started", "the job has started"));
      job.status = "canceled";
      job.finished_at = new Date().toISOString();
      return send(200, publicJob(job));
    }
    if (m("POST", /^\/v1\/estimate$/)) {
      const fmt = doc.format;
      const seconds = json.range ? json.range[1] - json.range[0] : fmt.duration;
      return send(200, {
        minutes: seconds / 60, usd: state.usdPerMinute === null ? null : (seconds / 60) * (state.usdPerMinute ?? 0.03),
        gpu_seconds: 2.1, first_byte_s: 3.0, width: fmt.width, height: fmt.height, fps: fmt.fps,
        seconds, frames: Math.round(seconds * fmt.fps), device: "NVIDIA L4", sampled_frames: 24,
      });
    }
    if (m("POST", /^\/v1\/(frame|sheet)$/)) {
      const jpeg = json.format === "jpeg";
      return send(200, PNG, { "content-type": jpeg ? "image/jpeg" : "image/png", "x-ultrafast-receipt": JSON.stringify({ timings: { total_s: 0.1 } }) });
    }
    if (m("POST", /^\/v1\/check$/)) {
      const issues = Object.entries(doc.nodes ?? {})
        .filter(([, n]) => n.op === "bogus")
        .map(([id]) => ({ node: id, severity: "error", message: "unknown op `bogus`" }));
      return send(200, { ok: issues.length === 0, issues });
    }
    if (m("POST", /^\/v1\/review$/)) {
      return send(200, {
        issues: [{ severity: "warning", kind: "safe_area", node: "title", from: 0.5, to: 2, message: `under the ${json.safe ?? "margin"} safe area` }],
        frames: 240, texts: 3, seconds: 8,
      });
    }
    send(404, error(404, "not_found", `no route ${req.method} ${path}`));
  });

  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  base = `http://127.0.0.1:${server.address().port}`;
  return {
    url: base,
    state,
    /** The next `count` requests to `method path...` answer `status code` (or drop the connection). */
    fail(method, path, opts) {
      state.faults.push({ method, path, count: 1, ...opts });
    },
    requestsTo(method, path) {
      return state.requests.filter((r) => r.method === method && r.path === path);
    },
    close: () => new Promise((r) => { server.closeAllConnections?.(); server.close(r); }),
  };
}

/** A webhook receiver: collects raw bodies and headers. */
export async function startReceiver() {
  const got = [];
  let wake;
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    got.push({ headers: req.headers, raw: Buffer.concat(chunks) });
    res.writeHead(204).end();
    wake?.();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return {
    url: `http://127.0.0.1:${server.address().port}/rendered`,
    got,
    next: () => (got.length ? Promise.resolve() : new Promise((r) => (wake = r))),
    close: () => new Promise((r) => { server.closeAllConnections?.(); server.close(r); }),
  };
}

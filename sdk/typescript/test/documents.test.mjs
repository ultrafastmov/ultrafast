// The site's sample, and local files in documents: found, uploaded once,
// rewritten in a copy.

import assert from "node:assert/strict";
import { mkdtemp, mkdir, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { Image, Job, LocalFileError, Ultrafast, findLocalFiles } from "../dist/esm/index.js";
import { startFakeServer } from "./fake-server.mjs";

let server;
let dir;

const film = () => ({
  version: 1,
  format: { width: 1080, height: 1920, fps: 30, duration: 8 },
  meta: { fonts: [{ family: "Brand", weight: 700, style: "normal", src: "fonts/Brand-Bold.otf" }] },
  components: {
    badge: {
      params: { logo: "art/logo.svg", label: "Title", tint: "#ff3366" },
      nodes: {
        mark: { op: "svg", params: { src: "$logo" } },
        word: { op: "text", params: { text: "$label", font: "fonts/Brand-Bold.otf" } },
        out: { op: "over", inputs: { fg: "word", bg: "mark" } },
      },
      output: "out",
    },
  },
  nodes: {
    clip: { op: "video", params: { src: "footage/a.mp4", fit: "cover" } },
    again: { op: "video", params: { src: "./footage/a.mp4" } },
    remote: { op: "image", params: { src: "https://example.com/a.png" } },
    bucket: { op: "image", params: { src: "gs://b/o.png#1760390000000002" } },
    seq: { op: "image", params: { src: "frames.zip!/frame_{frame:04}.png" } },
    title: {
      op: "text",
      params: {
        text: "Hello",
        font: "Inter",
        em_font: "ultrafast:serif",
        size: { audio: { src: "music.wav", band: "low", range: [90, 110] } },
        color: { keys: [{ t: 0, v: "#ffffff" }, { t: 1, v: "#ff3366" }] },
      },
    },
    cam: { op: "clip_camera", params: { src: "footage/a.mp4", faces: '[{"t":0,"box":[0,0,1,1]}]' } },
    mine: { op: "badge", params: { logo: "art/logo.svg", label: "Ada Lovelace" } },
    out: { op: "over", inputs: { fg: "title", bg: "clip" } },
  },
  output: "out",
});

before(async () => {
  server = await startFakeServer();
  dir = await mkdtemp(join(tmpdir(), "ultrafastmov-"));
  for (const d of ["footage", "fonts", "art"]) await mkdir(join(dir, d));
  await writeFile(join(dir, "footage", "a.mp4"), "mp4 bytes");
  await writeFile(join(dir, "fonts", "Brand-Bold.otf"), "otf bytes");
  await writeFile(join(dir, "art", "logo.svg"), "<svg/>");
  await writeFile(join(dir, "music.wav"), "wav bytes");
  await writeFile(join(dir, "frames.zip"), "zip bytes");
  await writeFile(join(dir, "film.json"), JSON.stringify(film(), null, 2));
});
after(() => server.close());

test("the site's TypeScript sample works as written", async () => {
  process.env.ULTRAFAST_API_KEY = "test-key";
  process.env.ULTRAFAST_BASE_URL = server.url;
  try {
    const uf = new Ultrafast(); // ULTRAFAST_API_KEY
    const doc = await uf.load(join(dir, "film.json"));
    const sheet = await uf.sheet(doc, { count: 9 });
    const job = await uf.render(doc, {
      webhook: "https://example.com/rendered",
    });
    assert.ok(sheet instanceof Image);
    assert.equal(sheet.contentType, "image/png");
    assert.ok(job instanceof Job);
    assert.match(job.id, /^job_/);
    assert.equal(job.status, "queued");
    const [req] = server.requestsTo("POST", "/v1/sheet");
    assert.equal(req.json.count, 9);
    assert.equal(server.requestsTo("POST", "/v1/jobs").at(-1).json.webhook, "https://example.com/rendered");
  } finally {
    delete process.env.ULTRAFAST_API_KEY;
    delete process.env.ULTRAFAST_BASE_URL;
  }
});

test("local files are uploaded once and rewritten in a copy", async () => {
  const uf = new Ultrafast({ apiKey: "test-key", baseUrl: server.url });
  const doc = await uf.load(join(dir, "film.json"));
  const before = JSON.stringify(doc);
  const uploadsBefore = server.requestsTo("POST", "/v1/uploads").length;

  const sent = await uf.prepare(doc);
  assert.equal(JSON.stringify(doc), before, "the caller's document is untouched");
  const ups = server.requestsTo("POST", "/v1/uploads").slice(uploadsBefore);
  assert.deepEqual(ups.map((u) => u.query.filename).sort(), ["Brand-Bold.otf", "a.mp4", "frames.zip", "logo.svg", "music.wav"]);
  assert.equal(ups.find((u) => u.query.filename === "a.mp4").raw.toString(), "mp4 bytes");
  assert.equal(ups.find((u) => u.query.filename === "a.mp4").headers["content-type"], "video/mp4");

  const asset = /^asset:\/\/[0-9a-f]+$/;
  const n = sent.nodes;
  assert.match(n.clip.params.src, asset);
  assert.equal(n.again.params.src, n.clip.params.src, "same file, same asset");
  assert.equal(n.cam.params.src, n.clip.params.src);
  assert.equal(n.cam.params.faces, '[{"t":0,"box":[0,0,1,1]}]', "inline face tracks stay");
  assert.equal(n.remote.params.src, "https://example.com/a.png");
  assert.equal(n.bucket.params.src, "gs://b/o.png#1760390000000002");
  assert.match(n.seq.params.src, /^asset:\/\/[0-9a-f]+!\/frame_\{frame:04\}\.png$/, "archive member kept");
  assert.equal(n.title.params.font, "Inter", "family names stay");
  assert.equal(n.title.params.em_font, "ultrafast:serif");
  assert.match(n.title.params.size.audio.src, asset, "audio driver files upload");
  assert.equal(n.title.params.color.keys[1].v, "#ff3366", "keyframe colors are not files");
  assert.match(n.mine.params.logo, asset, "component instance params that are files upload");
  assert.equal(n.mine.params.label, "Ada Lovelace");
  const c = sent.components.badge;
  assert.match(c.params.logo, asset, "component defaults that are files upload");
  assert.equal(c.params.label, "Title");
  assert.equal(c.params.tint, "#ff3366");
  assert.equal(c.nodes.mark.params.src, "$logo", "component param references stay");
  assert.match(c.nodes.word.params.font, asset, "component nodes' font files upload");
  assert.match(sent.meta.fonts[0].src, asset);

  // A second call uploads nothing; a changed file uploads again.
  const count = server.requestsTo("POST", "/v1/uploads").length;
  await uf.frame(doc, { at: 1 });
  assert.equal(server.requestsTo("POST", "/v1/uploads").length, count);
  const later = new Date(Date.now() + 5000);
  await utimes(join(dir, "music.wav"), later, later);
  await uf.frame(doc, { at: 1 });
  const fresh = server.requestsTo("POST", "/v1/uploads").slice(count);
  assert.deepEqual(fresh.map((u) => u.query.filename), ["music.wav"]);
  const frameReq = server.requestsTo("POST", "/v1/frame").at(-1).json;
  assert.equal(frameReq.at, 1);
  assert.match(frameReq.document.nodes.clip.params.src, asset);
});

test("a missing file is a LocalFileError before anything is sent", async () => {
  const uf = new Ultrafast({ apiKey: "test-key", baseUrl: server.url });
  const doc = { ...film(), nodes: { clip: { op: "video", params: { src: "nope.mp4" } } }, output: "clip" };
  delete doc.components;
  delete doc.meta;
  const renders = server.requestsTo("POST", "/v1/render").length;
  await assert.rejects(uf.render(doc), (e) => {
    assert.ok(e instanceof LocalFileError);
    assert.equal(e.code, "local_file");
    assert.match(e.message, /node clip, param src/);
    return true;
  });
  assert.equal(server.requestsTo("POST", "/v1/render").length, renders);
});

test("a spread copy of a loaded document keeps its directory", async () => {
  const uf = new Ultrafast({ apiKey: "test-key", baseUrl: server.url });
  const doc = await uf.load(join(dir, "film.json"));
  const edited = { ...doc, format: { ...doc.format, duration: 4 } };
  const sent = await uf.prepare(edited);
  assert.match(sent.nodes.clip.params.src, /^asset:\/\//);
  assert.equal(JSON.stringify(doc), JSON.stringify(JSON.parse(JSON.stringify(doc))), "JSON does not see the directory");
  assert.deepEqual(Object.keys(doc), Object.keys(film()));
});

test("documents passed as objects resolve against the working directory", async () => {
  const uf = new Ultrafast({ apiKey: "test-key", baseUrl: server.url });
  const cwd = process.cwd();
  process.chdir(dir);
  try {
    const sent = await uf.prepare({ version: 1, format: { width: 8, height: 8, fps: 1, duration: 1 }, nodes: { a: { op: "audio", params: { src: "music.wav" } } }, output: "a" });
    assert.match(sent.nodes.a.params.src, /^asset:\/\//);
  } finally {
    process.chdir(cwd);
  }
});

test("findLocalFiles lists what would upload", () => {
  const found = findLocalFiles(film()).map((s) => s.value);
  assert.ok(found.includes("footage/a.mp4"));
  assert.ok(found.includes("music.wav"));
  assert.ok(!found.includes("Inter"));
  assert.ok(!found.includes("https://example.com/a.png"));
  assert.ok(!found.includes("$logo"));
});

test("new() and templates()", async () => {
  const uf = new Ultrafast({ apiKey: "test-key", baseUrl: server.url });
  assert.deepEqual(await uf.templates(), ["vertical", "title", "square", "promo", "explainer", "product"]);
  const doc = await uf.new("vertical");
  assert.equal(doc.format.height, 1920);
  await assert.rejects(uf.new("nope"), { name: "NotFoundError", status: 404 });
});

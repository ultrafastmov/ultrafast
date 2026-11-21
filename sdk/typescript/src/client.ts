import { BadRequestError, InvalidDocumentError, UltrafastError, errorFromResponse } from "./errors.js";
import { BASE_DIR, rewriteLocalFiles } from "./files.js";
import { type FetchLike, Transport } from "./http.js";
import { Estimate, Image, Job, Render, Review } from "./results.js";
import { contentTypeFor, env, node, randomId } from "./runtime.js";
import type { Document } from "./schema.js";
import type { Asset, Attempt, CheckResult, EstimateData, Health, JobData, Me, Receipt, ReviewData } from "./types.js";

/** A local `ultrafast serve` on its default port. */
export const DEFAULT_BASE_URL = "http://127.0.0.1:8080";

export interface UltrafastOptions {
  /** Default: `ULTRAFAST_API_KEY` (the key the server prints at first start). */
  apiKey?: string;
  /** Default: `ULTRAFAST_BASE_URL`, else `http://127.0.0.1:8080`, a server on this machine. */
  baseUrl?: string;
  /** A fetch implementation (default: the global one). */
  fetch?: FetchLike;
  /** Retries of requests that are safe to repeat (default 2). */
  maxRetries?: number;
  /** Extra headers on every request. */
  headers?: Record<string, string>;
}

/** A document, or the path of a document file (Node). */
export type DocumentInput = Document | string;

/** An output size: `"720p"`, `"1080p"`, `"1440p"`, `"4k"` (the short side), `"WxH"`, or `[w, h]`. */
export type Size = "720p" | "1080p" | "1440p" | "4k" | `${number}p` | `${number}x${number}` | [number, number];

/**
 * Export settings, as `ultrafast export` and the MCP export tools spell them.
 * Settings that don't go together are refused before anything renders.
 */
export interface ExportSettings {
  /** The container (default: from `name`'s extension, else mp4). `png` and `exr` are image sequences (a zip); `wav` to `opus` are sound alone. */
  format?: "mp4" | "mov" | "mkv" | "webm" | "gif" | "png" | "exr" | "wav" | "mp3" | "m4a" | "flac" | "opus";
  /** Default h264 (vp9 for webm). */
  codec?: "h264" | "hevc" | "h265" | "av1" | "vp9" | "prores";
  /** draft: a fast preview without motion blur; standard (default); high; master: twice the motion-blur samples, the best encode. */
  quality?: "draft" | "standard" | "high" | "master";
  /** Rendered at this size, not scaled up. */
  size?: Size;
  /** A multiple of the document's size, instead of `size`. */
  scale?: number;
  /** Output frame rate (default the document's; a GIF's at most 30). */
  fps?: number;
  /** Constant quality on the codec's scale, lower is better (H.264/HEVC 0-51, AV1/VP9 0-63). */
  crf?: number;
  /** Bits per second, or text such as `"8M"`, instead of `crf`; null for the quality level's. */
  bitrate?: number | string | null;
  /** Default hq; 4444 with `alpha`. */
  prores_profile?: "proxy" | "light" | "standard" | "hq" | "4444" | "4444-xq";
  /** Keep transparency: ProRes 4444, VP9, GIF, PNG, EXR. */
  alpha?: boolean;
  /** Mix the document's sound (default true). */
  audio?: boolean;
  /** Default aac (opus for webm, pcm with ProRes). */
  audio_codec?: "aac" | "opus" | "mp3" | "flac" | "pcm" | "pcm-24";
  /** Bits per second, or text such as `"192k"`. */
  audio_bitrate?: number | string;
  /** GIF: times it plays again after the first (0: once; default forever). */
  loops?: number;
}

const SETTINGS = [
  "format",
  "codec",
  "quality",
  "size",
  "scale",
  "fps",
  "crf",
  "bitrate",
  "prores_profile",
  "alpha",
  "audio",
  "audio_codec",
  "audio_bitrate",
  "loops",
] as const;

/** The export settings among `opts`. */
function settingsOf(opts: ExportSettings): ExportSettings {
  const out: Record<string, unknown> = {};
  for (const k of SETTINGS) if (opts[k] !== undefined) out[k] = opts[k];
  return out as ExportSettings;
}

export interface RenderOptions extends ExportSettings {
  /** The file in the response (at most 32 MiB) instead of stored with a signed URL. */
  inline?: boolean;
  /** Create the video at this object in your own bucket (`s3://`, `r2://`, `gs://`); create-only. */
  uri?: string;
  /** The stored file's name (default `film.<format>`, `frames.zip` for a sequence); its extension picks the format. */
  name?: string;
  /** `[from, to]` seconds of the document (default all of it). */
  range?: [number, number] | null;
  /** Ids that make retries safe; one is made up per call when missing. */
  attempt?: Attempt;
  /** The server stops when this passes (at most 10 minutes from receipt). */
  deadline?: Date | string;
  signal?: AbortSignal;
}

export interface JobOptions extends Omit<RenderOptions, "inline"> {
  /** POSTed the finished job, signed (see `verifyWebhook`). */
  webhook?: string;
  /** A job without a webhook: poll it with `job.wait()`. */
  job?: boolean;
}

export interface FrameOptions {
  /** Seconds (default 0). */
  at?: number;
  format?: "png" | "jpeg";
  signal?: AbortSignal;
}

export interface SheetOptions {
  /** Frames spread evenly over the document (default 9). */
  count?: number;
  /** Times in seconds, instead of `count`. */
  at?: number[];
  /** Frames per row (default 3, or 5 for a vertical document). */
  columns?: number;
  format?: "png" | "jpeg";
  signal?: AbortSignal;
}

export interface ReviewOptions {
  /** `margin` (default, 4% of the frame) or `social` (clear of short-video apps' buttons and captions). */
  safe?: "margin" | "social";
  /** Seconds between text probes. */
  step?: number;
  signal?: AbortSignal;
}

export interface UploadOptions {
  filename?: string;
  contentType?: string;
  signal?: AbortSignal;
}

/** Something to upload: a file path (Node), a Blob or File, or bytes. */
export type UploadInput = string | Blob | Uint8Array | ArrayBuffer;

function compact<T extends Record<string, unknown>>(o: T): Partial<T> {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as Partial<T>;
}

function parseJson(v: string | null): Record<string, unknown> | undefined {
  if (!v) return undefined;
  try {
    return JSON.parse(v) as Record<string, unknown>;
  } catch {
    return undefined;
  }
}

/** The jobs API. */
export class Jobs {
  readonly #client: Ultrafast;
  readonly #transport: Transport;

  /** @internal */
  constructor(client: Ultrafast, transport: Transport) {
    this.#client = client;
    this.#transport = transport;
  }

  /** Starts a job (`POST /v1/jobs`). */
  async create(doc: DocumentInput, opts: JobOptions = {}): Promise<Job> {
    return this.#client.render(doc, { ...opts, job: true });
  }

  async get(id: string, opts: { signal?: AbortSignal } = {}): Promise<Job> {
    const data = await this.#transport.json<JobData>({ path: `/v1/jobs/${encodeURIComponent(id)}`, signal: opts.signal });
    return new Job(this.#transport, data);
  }

  /** The caller's jobs, most recent first. */
  async list(opts: { limit?: number; signal?: AbortSignal } = {}): Promise<Job[]> {
    const { jobs } = await this.#transport.json<{ jobs: JobData[] }>({
      path: "/v1/jobs",
      query: { limit: opts.limit },
      signal: opts.signal,
    });
    return jobs.map((j) => new Job(this.#transport, j));
  }

  /** Cancels a queued job. */
  async cancel(id: string): Promise<Job> {
    const data = await this.#transport.json<JobData>({ method: "DELETE", path: `/v1/jobs/${encodeURIComponent(id)}` });
    return new Job(this.#transport, data);
  }
}

/**
 * A client of an ultrafast render service: your own `ultrafast serve`.
 *
 * ```ts
 * const uf = new Ultrafast(); // ULTRAFAST_API_KEY
 * const doc = await uf.load("film.json");
 * await (await uf.render(doc)).save("film.mp4");
 * ```
 */
export class Ultrafast {
  readonly baseUrl: string;
  readonly jobs: Jobs;
  readonly #transport: Transport;
  readonly #uploads = new Map<string, Promise<Asset>>();
  #active = 0;
  readonly #waiting: (() => void)[] = [];

  constructor(opts: UltrafastOptions = {}) {
    const f = opts.fetch ?? globalThis.fetch?.bind(globalThis);
    if (!f) throw new UltrafastError("no fetch in this runtime: pass new Ultrafast({ fetch })", { code: "connection_error" });
    this.baseUrl = (opts.baseUrl ?? env("ULTRAFAST_BASE_URL") ?? DEFAULT_BASE_URL).replace(/\/+$/, "");
    this.#transport = new Transport({
      apiKey: opts.apiKey ?? env("ULTRAFAST_API_KEY"),
      baseUrl: this.baseUrl,
      fetch: f,
      maxRetries: opts.maxRetries ?? 2,
      headers: opts.headers,
    });
    this.jobs = new Jobs(this, this.#transport);
  }

  // --- documents ----------------------------------------------------------

  /**
   * Reads a document file (Node). Its relative paths resolve against its
   * directory, kept under a symbol key: copies made with spread or
   * `Object.assign` keep it, JSON does not see it.
   */
  async load(path: string | URL): Promise<Document> {
    const n = await node("load()");
    const file = typeof path === "string" ? n.path.resolve(path) : n.url.fileURLToPath(path);
    const doc = JSON.parse(await n.fsp.readFile(file, "utf8")) as Document;
    Object.defineProperty(doc, BASE_DIR, { value: n.path.dirname(file), enumerable: true });
    return doc;
  }

  /** A starter document: `vertical`, `title`, `square`, `promo`, `explainer`, ... (see `templates()`). */
  async new(template: string, opts: { signal?: AbortSignal } = {}): Promise<Document> {
    return this.#transport.json<Document>({ path: `/v1/templates/${encodeURIComponent(template)}`, signal: opts.signal });
  }

  /** The names `new()` takes. */
  async templates(opts: { signal?: AbortSignal } = {}): Promise<string[]> {
    const body = await this.#transport.json<unknown>({ path: "/v1/templates", signal: opts.signal });
    const list = Array.isArray(body) ? body : ((body as { templates?: unknown[] })?.templates ?? []);
    return list.map((t) => (typeof t === "string" ? t : String((t as { name?: unknown }).name)));
  }

  /**
   * The document as it is sent: local files it names uploaded (once per
   * client) and replaced by their asset:// uris, in a copy.
   */
  async prepare(doc: DocumentInput): Promise<Document> {
    const d = typeof doc === "string" ? await this.load(doc) : doc;
    return rewriteLocalFiles(d, (file) => this.#uploadCached(file));
  }

  // --- rendering ----------------------------------------------------------

  /** A video: a `Render` (stored, or `inline`), or with `webhook` or `job: true`, a `Job`. */
  render(doc: DocumentInput, opts: JobOptions & ({ webhook: string } | { job: true })): Promise<Job>;
  render(doc: DocumentInput, opts?: RenderOptions & { webhook?: undefined; job?: false }): Promise<Render>;
  render(
    doc: DocumentInput,
    opts?: (RenderOptions & { webhook?: undefined; job?: false }) | (JobOptions & { inline?: undefined }),
  ): Promise<Render | Job>;
  async render(doc: DocumentInput, opts: RenderOptions & JobOptions = {}): Promise<Render | Job> {
    const asJob = opts.webhook !== undefined || opts.job === true;
    if (asJob && opts.inline) {
      throw new BadRequestError("a job cannot be inline: drop `inline`, or render without `webhook` and `job`", { code: "bad_request" });
    }
    const document = await this.prepare(doc);
    const output = compact({
      inline: opts.inline,
      uri: opts.uri,
      name: opts.name,
      ...settingsOf(opts),
      range: opts.range,
    });
    const body = compact({
      document,
      output,
      attempt: opts.attempt ?? { render_id: randomId("render_"), attempt_id: randomId("attempt_", 12) },
      deadline: opts.deadline instanceof Date ? opts.deadline.toISOString() : opts.deadline,
      webhook: opts.webhook,
    });

    if (asJob) {
      // A resent job request would start a second job: retry only what the server refused.
      const data = await this.#transport.json<JobData>({ path: "/v1/jobs", json: body, retry: "unsent", signal: opts.signal });
      return new Job(this.#transport, data);
    }

    const res = await this.#transport.request({
      path: "/v1/render",
      json: body,
      accept: opts.inline ? "video/mp4, video/*, image/gif, audio/*, application/zip, application/json" : "application/json",
      // With `uri` a retry is a replay (409 with the stored receipt); without
      // it a resent render could render twice, so only what the server refused.
      retry: opts.uri ? "safe" : "unsent",
      pass: [409],
      signal: opts.signal,
    });
    if (res.status === 409) {
      const text = await res.text();
      const parsed = parseJson(text);
      if (parsed && "receipt_version" in parsed) return new Render(this.#transport, parsed as Receipt);
      throw errorFromResponse(409, text);
    }
    const type = res.headers.get("content-type") ?? "";
    // Inline: the file itself (an mp4, a webm, a gif, a zip of frames, sound).
    if (type !== "" && !type.startsWith("application/json")) {
      const bytes = new Uint8Array(await res.arrayBuffer());
      const receipt = (parseJson(res.headers.get("x-ultrafast-receipt")) ?? { receipt_version: 1, status: "rendered" }) as Receipt;
      receipt.sha256 ??= res.headers.get("x-ultrafast-sha256") ?? undefined;
      return new Render(this.#transport, receipt, bytes);
    }
    return new Render(this.#transport, (await res.json()) as Receipt);
  }

  async #image(path: string, body: Record<string, unknown>, signal?: AbortSignal): Promise<Image> {
    const res = await this.#transport.request({ path, json: compact(body), accept: "image/png, image/jpeg", signal });
    return new Image(
      new Uint8Array(await res.arrayBuffer()),
      res.headers.get("content-type")?.split(";")[0] ?? "image/png",
      parseJson(res.headers.get("x-ultrafast-receipt")),
    );
  }

  /** One frame, at `at` seconds. */
  async frame(doc: DocumentInput, opts: FrameOptions = {}): Promise<Image> {
    const document = await this.prepare(doc);
    return this.#image("/v1/frame", { document, at: opts.at ?? 0, format: opts.format }, opts.signal);
  }

  /** A contact sheet: `count` frames over the film, or the times in `at`, each labeled with its time. */
  async sheet(doc: DocumentInput, opts: SheetOptions = {}): Promise<Image> {
    const document = await this.prepare(doc);
    return this.#image(
      "/v1/sheet",
      { document, count: opts.count, at: opts.at, columns: opts.columns, format: opts.format },
      opts.signal,
    );
  }

  /** Static and render-time problems, as `ultrafast check` reports them. */
  async check(doc: DocumentInput, opts: { signal?: AbortSignal } = {}): Promise<CheckResult> {
    const document = await this.prepare(doc);
    try {
      return await this.#transport.json<CheckResult>({ path: "/v1/check", json: { document }, signal: opts.signal });
    } catch (e) {
      if (e instanceof InvalidDocumentError) return { ok: false, issues: e.issues ?? [] };
      throw e;
    }
  }

  /** Renders the film and reports what a viewer would trip on. */
  async review(doc: DocumentInput, opts: ReviewOptions = {}): Promise<Review> {
    const document = await this.prepare(doc);
    const data = await this.#transport.json<ReviewData>({
      path: "/v1/review",
      json: compact({ document, safe: opts.safe, step: opts.step }),
      signal: opts.signal,
    });
    return new Review(data);
  }

  /**
   * What rendering will cost (free). Pass the export settings you will render
   * with (`size`, `quality`, `fps`, ...): minutes count the output size and
   * frames are timed at that quality and frame rate.
   */
  async estimate(
    doc: DocumentInput,
    opts: ExportSettings & { range?: [number, number] | null; signal?: AbortSignal } = {},
  ): Promise<Estimate> {
    const document = await this.prepare(doc);
    const output = settingsOf(opts);
    const data = await this.#transport.json<EstimateData>({
      path: "/v1/estimate",
      json: compact({ document, range: opts.range ?? null, output: Object.keys(output).length ? output : undefined }),
      signal: opts.signal,
    });
    return new Estimate(data);
  }

  // --- assets -------------------------------------------------------------

  /** Uploads footage, an image, a font or audio; use the asset's `uri` in a document. */
  async upload(input: UploadInput, opts: UploadOptions = {}): Promise<Asset> {
    if (typeof input === "string") {
      const n = await node("upload(path)");
      const file = n.path.resolve(input);
      const stat = await n.fsp.stat(file);
      const filename = opts.filename ?? n.path.basename(file);
      return this.#send(
        () => n.stream.Readable.toWeb(n.fs.createReadStream(file)) as unknown as ReadableStream<Uint8Array>,
        filename,
        opts.contentType ?? contentTypeFor(filename),
        stat.size,
        opts.signal,
      );
    }
    const isBlob = typeof Blob !== "undefined" && input instanceof Blob;
    const filename = opts.filename ?? (isBlob && "name" in input && typeof input.name === "string" && input.name ? input.name : "upload");
    const contentType = opts.contentType ?? ((isBlob && (input as Blob).type) || contentTypeFor(filename));
    const body: BodyInit = isBlob ? (input as Blob) : input instanceof Uint8Array ? (input as Uint8Array<ArrayBuffer>) : new Uint8Array(input as ArrayBuffer);
    const size = isBlob ? (input as Blob).size : (input as Uint8Array | ArrayBuffer).byteLength;
    return this.#send(() => body, filename, contentType, size, opts.signal);
  }

  #send(body: () => BodyInit, filename: string, contentType: string, size: number, signal?: AbortSignal): Promise<Asset> {
    return this.#transport.json<Asset>({
      path: "/v1/uploads",
      query: { filename },
      body,
      headers: { "content-type": contentType, "content-length": String(size) },
      signal,
    });
  }

  /** An upload by id. */
  async asset(id: string): Promise<Asset> {
    return this.#transport.json<Asset>({ path: `/v1/uploads/${encodeURIComponent(id)}` });
  }

  /** Uploads a document's local file once per client, keyed by path, size and mtime; four at a time. */
  async #uploadCached(file: string): Promise<Asset> {
    const n = await node();
    const st = await n.fsp.stat(file);
    const key = `${file}\0${st.size}\0${st.mtimeMs}`;
    let p = this.#uploads.get(key);
    if (!p) {
      p = this.#limit(() => this.upload(file));
      this.#uploads.set(key, p);
      p.catch(() => this.#uploads.delete(key));
    }
    return p;
  }

  async #limit<T>(fn: () => Promise<T>): Promise<T> {
    while (this.#active >= 4) await new Promise<void>((r) => this.#waiting.push(r));
    this.#active++;
    try {
      return await fn();
    } finally {
      this.#active--;
      this.#waiting.shift()?.();
    }
  }

  // --- account ------------------------------------------------------------

  /** Who the key belongs to, and the webhook secret. */
  async me(): Promise<Me> {
    return this.#transport.json<Me>({ path: "/v1/me" });
  }

  /** The server's health (no key needed). */
  async health(): Promise<Health> {
    return this.#transport.json<Health>({ path: "/v1/health", pass: [503] });
  }
}

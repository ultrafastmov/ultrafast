import { UltrafastError, errorFromBody } from "./errors.js";
import type { Transport } from "./http.js";
import { node, randomId, sleep, toBase64 } from "./runtime.js";
import type { EstimateData, JobData, JobStatus, Receipt, ReviewData, ReviewIssue } from "./types.js";

/** Streams a response body to `path` (via a temporary file), checking its sha256 when given. */
async function writeResponse(res: Response, path: string, sha256?: string): Promise<string> {
  const n = await node("saving to a file");
  const tmp = `${path}.${randomId("", 8)}.part`;
  const hash = n.crypto.createHash("sha256");
  try {
    await n.streamp.pipeline(
      n.stream.Readable.fromWeb(res.body as import("node:stream/web").ReadableStream<Uint8Array>),
      async function* (source: AsyncIterable<Uint8Array>) {
        for await (const chunk of source) {
          hash.update(chunk);
          yield chunk;
        }
      },
      n.fs.createWriteStream(tmp),
    );
    const got = hash.digest("hex");
    if (sha256 && /^[0-9a-f]{64}$/i.test(sha256) && got !== sha256.toLowerCase()) {
      throw new UltrafastError(`downloaded ${got}, the receipt says ${sha256}`, { code: "integrity" });
    }
    await n.fsp.rename(tmp, path);
    return path;
  } catch (e) {
    await n.fsp.rm(tmp, { force: true });
    throw e;
  }
}

async function writeBytes(bytes: Uint8Array, path: string): Promise<string> {
  const n = await node("saving to a file");
  await n.fsp.writeFile(path, bytes);
  return path;
}

/** A finished render: its receipt, and the video to download. */
export class Render {
  readonly receipt: Receipt;
  readonly #transport: Transport;
  readonly #inline?: Uint8Array;

  /** @internal */
  constructor(transport: Transport, receipt: Receipt, inline?: Uint8Array) {
    this.#transport = transport;
    this.receipt = receipt;
    this.#inline = inline;
  }

  get id(): string {
    return this.receipt.id ?? this.receipt.attempt?.render_id ?? "";
  }
  /** `rendered`, or `already_rendered` when a retry found its own finished video. */
  get status(): Receipt["status"] {
    return this.receipt.status;
  }
  /** Signed download URL (null for an inline render, or a bucket the server cannot sign for). */
  get url(): string | null {
    return this.receipt.url ?? null;
  }
  get sha256(): string | undefined {
    return this.receipt.sha256 ?? this.receipt.object?.sha256 ?? undefined;
  }
  /** Size of the mp4 in bytes. */
  get bytes(): number | undefined {
    return this.receipt.bytes ?? this.receipt.object?.size ?? this.#inline?.byteLength;
  }
  /** Seconds of video. */
  get duration(): number | undefined {
    return this.receipt.duration ?? this.receipt.video?.duration;
  }
  get frames(): number | undefined {
    return this.receipt.frames ?? this.receipt.video?.frames;
  }
  get minutes(): number | undefined {
    return this.receipt.minutes;
  }

  #source(): string {
    if (this.url) return this.url;
    const where = this.receipt.object?.uri;
    throw new UltrafastError(
      where ? `the video went to ${where}, which the server cannot sign a URL for` : "this receipt has no download URL",
      { code: "not_found" },
    );
  }

  /** The mp4 bytes. */
  async arrayBuffer(opts: { signal?: AbortSignal } = {}): Promise<ArrayBuffer> {
    if (this.#inline) return this.#inline.slice().buffer as ArrayBuffer;
    const res = await this.#transport.download(this.#source(), opts.signal);
    return res.arrayBuffer();
  }

  /** Writes the mp4 to `path` (Node), streaming the download and checking its sha256. Resolves with `path`. */
  async save(path: string, opts: { signal?: AbortSignal } = {}): Promise<string> {
    if (this.#inline) return writeBytes(this.#inline, path);
    const res = await this.#transport.download(this.#source(), opts.signal);
    return writeResponse(res, path, this.sha256);
  }

  toJSON(): Receipt {
    return this.receipt;
  }
}

export interface WaitOptions {
  /** Milliseconds between polls (default 2000). */
  pollMs?: number;
  /** Give up after this long (default: never). The job keeps running. */
  timeoutMs?: number;
  signal?: AbortSignal;
}

/** A render job: poll it, wait for it, or get its webhook. */
export class Job {
  #data: JobData;
  readonly #transport: Transport;

  /** @internal */
  constructor(transport: Transport, data: JobData) {
    this.#transport = transport;
    this.#data = data;
  }

  get data(): JobData {
    return this.#data;
  }
  get id(): string {
    return this.#data.id;
  }
  get status(): JobStatus {
    return this.#data.status;
  }
  get progress(): JobData["progress"] {
    return this.#data.progress;
  }
  get receipt(): Receipt | null {
    return this.#data.receipt;
  }
  get error(): JobData["error"] {
    return this.#data.error;
  }
  get webhook(): string | null {
    return this.#data.webhook;
  }
  get createdAt(): string {
    return this.#data.created_at;
  }
  get startedAt(): string | null {
    return this.#data.started_at;
  }
  get finishedAt(): string | null {
    return this.#data.finished_at;
  }
  /** Rendered, failed or canceled. */
  get done(): boolean {
    return this.status === "rendered" || this.status === "failed" || this.status === "canceled";
  }

  /** Fetches the job's current state. */
  async refresh(opts: { signal?: AbortSignal } = {}): Promise<this> {
    this.#data = await this.#transport.json<JobData>({ path: `/v1/jobs/${encodeURIComponent(this.id)}`, signal: opts.signal });
    return this;
  }

  /** Cancels a queued job (`JobStartedError` once it has started). */
  async cancel(): Promise<this> {
    this.#data = await this.#transport.json<JobData>({
      method: "DELETE",
      path: `/v1/jobs/${encodeURIComponent(this.id)}`,
    });
    return this;
  }

  /** Polls until the job finishes: its Render, or the job's error thrown. */
  async wait(opts: WaitOptions = {}): Promise<Render> {
    const pollMs = opts.pollMs ?? 2000;
    const deadline = opts.timeoutMs === undefined ? Infinity : Date.now() + opts.timeoutMs;
    for (;;) {
      if (this.status === "rendered" && this.receipt) return new Render(this.#transport, this.receipt);
      if (this.status === "failed") {
        const body = this.error ?? { status: 0, code: "render_failed", message: `job ${this.id} failed`, retry: false };
        const err = errorFromBody(body);
        (err as { job?: Job }).job = this;
        throw err;
      }
      if (this.status === "canceled") throw new UltrafastError(`job ${this.id} was canceled`, { code: "canceled" });
      const left = deadline - Date.now();
      if (left <= 0) {
        throw new UltrafastError(`job ${this.id} still ${this.status} after ${opts.timeoutMs} ms`, { code: "timeout", retry: true });
      }
      await sleep(Math.min(pollMs, left), opts.signal);
      await this.refresh({ signal: opts.signal });
    }
  }

  /** Waits for the job and writes the mp4 to `path` (Node). */
  async save(path: string, opts: WaitOptions = {}): Promise<string> {
    return (await this.wait(opts)).save(path, { signal: opts.signal });
  }

  toJSON(): JobData {
    return this.#data;
  }
}

/** A frame or a contact sheet. */
export class Image {
  readonly bytes: Uint8Array;
  /** `image/png` or `image/jpeg`. */
  readonly contentType: string;
  /** The compact receipt (inputs, timings), when the server sent one. */
  readonly receipt?: Record<string, unknown>;

  /** @internal */
  constructor(bytes: Uint8Array, contentType: string, receipt?: Record<string, unknown>) {
    this.bytes = bytes;
    this.contentType = contentType;
    this.receipt = receipt;
  }

  toBase64(): string {
    return toBase64(this.bytes);
  }

  /** A `data:` URL, for an `<img>` or a model's image input. */
  toDataUrl(): string {
    return `data:${this.contentType};base64,${this.toBase64()}`;
  }

  /** Writes the image to `path` (Node). Resolves with `path`. */
  save(path: string): Promise<string> {
    return writeBytes(this.bytes, path);
  }
}

/** What a viewer would trip on. Iterates over its issues. */
export class Review implements ReviewData {
  readonly issues: ReviewIssue[];
  readonly frames: number;
  readonly texts: number;
  readonly seconds: number;
  [key: string]: unknown;

  /** @internal */
  constructor(data: ReviewData) {
    Object.assign(this, data);
    this.issues = data.issues ?? [];
    this.frames = data.frames;
    this.texts = data.texts;
    this.seconds = data.seconds;
  }

  /** Whether nothing was found. */
  get ok(): boolean {
    return this.issues.length === 0;
  }

  [Symbol.iterator](): Iterator<ReviewIssue> {
    return this.issues[Symbol.iterator]();
  }
}

/** What a document will cost. `String(estimate)` reads like "$0.004, 8 s of 1080p". */
export class Estimate implements EstimateData {
  readonly minutes!: number;
  readonly usd!: number | null;
  readonly gpu_seconds!: number;
  readonly first_byte_s!: number;
  readonly width!: number;
  readonly height!: number;
  readonly fps!: number;
  readonly seconds!: number;
  readonly frames!: number;
  readonly device!: string;
  readonly sampled_frames!: number;
  [key: string]: unknown;

  /** @internal */
  constructor(data: EstimateData) {
    Object.assign(this, data);
  }

  toString(): string {
    const parts: string[] = [];
    if (typeof this.usd === "number") parts.push(formatUsd(this.usd));
    let what = formatSeconds(this.seconds);
    if (this.width && this.height) what += ` of ${Math.min(this.width, this.height)}p`;
    parts.push(what);
    return parts.join(", ");
  }
}

export function formatUsd(usd: number): string {
  if (usd === 0) return "$0";
  if (usd >= 0.01) return `$${usd.toFixed(2)}`;
  if (usd < 0.0001) return "<$0.0001";
  return `$${Number(usd.toPrecision(2))}`;
}

export function formatSeconds(s: number): string {
  if (!Number.isFinite(s)) return "? s";
  if (s < 60) return `${Number(s.toFixed(1))} s`;
  if (s < 3600) return `${Number((s / 60).toFixed(1))} min`;
  return `${Number((s / 3600).toFixed(2))} h`;
}

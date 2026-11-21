import { ConnectionError, UltrafastError, errorFromResponse } from "./errors.js";
import { isBrowser, sleep } from "./runtime.js";
import { VERSION } from "./version.js";

type Body = NonNullable<RequestInit["body"]>;

export type FetchLike = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

/**
 * When a failed request is sent again:
 * - `safe`: on 429/503 with `retry: true`, gateway errors and network errors
 *   (reads, pure computations, uploads, create-only renders);
 * - `unsent`: only on 429/503 with `retry: true`, when the server did nothing.
 */
export type RetryMode = "safe" | "unsent";

export interface RequestOptions {
  method?: string;
  path: string;
  query?: Record<string, string | number | undefined>;
  json?: unknown;
  /** A body factory, called once per attempt (streams cannot be resent). */
  body?: () => Body | Promise<Body>;
  headers?: Record<string, string>;
  accept?: string;
  retry?: RetryMode;
  /** Statuses returned to the caller instead of thrown. */
  pass?: number[];
  signal?: AbortSignal;
}

export interface TransportOptions {
  apiKey?: string;
  baseUrl: string;
  fetch: FetchLike;
  maxRetries: number;
  headers?: Record<string, string>;
}

export class Transport {
  readonly baseUrl: string;
  readonly apiKey?: string;
  private readonly fetchFn: FetchLike;
  private readonly maxRetries: number;
  private readonly extraHeaders: Record<string, string>;

  constructor(o: TransportOptions) {
    this.baseUrl = o.baseUrl.replace(/\/+$/, "");
    this.apiKey = o.apiKey;
    this.fetchFn = o.fetch;
    this.maxRetries = o.maxRetries;
    this.extraHeaders = o.headers ?? {};
  }

  url(path: string, query?: RequestOptions["query"]): string {
    let u = /^https?:\/\//i.test(path) ? path : this.baseUrl + (path.startsWith("/") ? path : `/${path}`);
    const q = Object.entries(query ?? {}).filter(([, v]) => v !== undefined);
    if (q.length) u += (u.includes("?") ? "&" : "?") + q.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`).join("&");
    return u;
  }

  private headers(o: RequestOptions, authorize: boolean): Record<string, string> {
    const h: Record<string, string> = { accept: o.accept ?? "application/json", ...this.extraHeaders };
    if (!isBrowser) h["user-agent"] = `ultrafastmov-js/${VERSION}`;
    if (authorize && this.apiKey) h.authorization = `Bearer ${this.apiKey}`;
    if (o.json !== undefined) h["content-type"] = "application/json";
    return { ...h, ...o.headers };
  }

  /** Sends a request, retrying as `o.retry` allows; resolves with a 2xx (or `pass`) response. */
  async request(o: RequestOptions): Promise<Response> {
    const url = this.url(o.path, o.query);
    const mode = o.retry ?? "safe";
    const json = o.json === undefined ? undefined : JSON.stringify(o.json);
    for (let attempt = 0; ; attempt++) {
      const init: RequestInit & { duplex?: string } = {
        method: o.method ?? (json !== undefined || o.body ? "POST" : "GET"),
        headers: this.headers(o, true),
        signal: o.signal,
      };
      if (json !== undefined) init.body = json;
      else if (o.body) {
        init.body = await o.body();
        if (typeof ReadableStream !== "undefined" && init.body instanceof ReadableStream) init.duplex = "half";
      }
      let res: Response;
      try {
        res = await this.fetchFn(url, init);
      } catch (cause) {
        if (o.signal?.aborted) throw o.signal.reason ?? cause;
        const err = new ConnectionError(`cannot reach ${url}: ${describe(cause)}`, { code: "connection_error", retry: true, cause });
        if (mode === "safe" && attempt < this.maxRetries) {
          await sleep(backoff(attempt), o.signal);
          continue;
        }
        throw err;
      }
      if (res.ok || o.pass?.includes(res.status)) return res;
      const err = errorFromResponse(res.status, await res.text().catch(() => ""));
      if (attempt < this.maxRetries && retryable(err, mode)) {
        await sleep(retryAfter(res) ?? backoff(attempt), o.signal);
        continue;
      }
      if (err.status === 401 && !this.apiKey) {
        throw new (err.constructor as typeof UltrafastError)(
          `${err.message} (no API key: pass new Ultrafast({ apiKey }) or set ULTRAFAST_API_KEY)`,
          { status: err.status, code: err.code, retry: err.retry, body: err.body },
        );
      }
      throw err;
    }
  }

  async json<T>(o: RequestOptions): Promise<T> {
    const res = await this.request(o);
    return (await res.json()) as T;
  }

  /**
   * GETs a download URL. The API key goes only to the server's own origin:
   * presigned S3, R2 and GCS URLs refuse a second credential.
   */
  async download(url: string, signal?: AbortSignal): Promise<Response> {
    const abs = new URL(url, this.baseUrl + "/").toString();
    const same = new URL(abs).origin === new URL(this.baseUrl).origin;
    for (let attempt = 0; ; attempt++) {
      let res: Response;
      try {
        res = await this.fetchFn(abs, { headers: this.headers({ path: abs, accept: "*/*" }, same), signal });
      } catch (cause) {
        if (signal?.aborted) throw signal.reason ?? cause;
        if (attempt < this.maxRetries) {
          await sleep(backoff(attempt), signal);
          continue;
        }
        throw new ConnectionError(`cannot download ${abs}: ${describe(cause)}`, { code: "connection_error", retry: true, cause });
      }
      if (res.ok && res.body) return res;
      const err = errorFromResponse(res.status, await res.text().catch(() => ""));
      if (attempt < this.maxRetries && retryable(err, "safe")) {
        await sleep(retryAfter(res) ?? backoff(attempt), signal);
        continue;
      }
      throw err;
    }
  }
}

function retryable(err: UltrafastError, mode: RetryMode): boolean {
  if ((err.status === 429 || err.status === 503) && err.retry) return true;
  // Gateways in front of the server (no API error body).
  return mode === "safe" && err.code === "http_error" && (err.status === 502 || err.status === 503 || err.status === 504);
}

function backoff(attempt: number): number {
  const base = Math.min(8000, 500 * 2 ** attempt);
  return base * (0.75 + Math.random() * 0.5);
}

function retryAfter(res: Response): number | undefined {
  const h = res.headers.get("retry-after");
  if (!h) return undefined;
  const s = Number(h);
  const ms = Number.isFinite(s) ? s * 1000 : Date.parse(h) - Date.now();
  return Number.isFinite(ms) && ms >= 0 ? Math.min(ms, 30_000) : undefined;
}

function describe(e: unknown): string {
  const err = e as { message?: string; cause?: { code?: string; message?: string } };
  return [err?.message, err?.cause?.code ?? err?.cause?.message].filter(Boolean).join(": ") || String(e);
}

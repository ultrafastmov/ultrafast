// Request and response bodies of the v1 HTTP API (docs/service/API.md).

/** One problem `check` found. */
export interface Issue {
  node: string;
  severity: "error" | "warning" | (string & {});
  message: string;
}

/** The body of every API error, under `"error"`. */
export interface ApiErrorBody {
  status: number;
  code: string;
  message: string;
  retry: boolean;
  [key: string]: unknown;
}

/** The ids that make a render's retries safe. 1 to 128 of `A-Z a-z 0-9 . _ : -`. */
export interface Attempt {
  render_id: string;
  attempt_id: string;
}

export interface StoredObject {
  uri: string;
  bucket?: string;
  name?: string;
  /** GCS generation, S3 version id or ETag. */
  generation?: string;
  size?: number;
  crc32c?: string;
  md5?: string;
  sha256?: string | null;
  content_type?: string;
  [key: string]: unknown;
}

export interface VideoInfo {
  frames: number;
  fps: number;
  width: number;
  height: number;
  duration: number;
  /** `h264`, `hevc`, `av1`, `vp9`, `prores-<profile>`, `gif`, `png`, `exr`, or the sound's codec for sound alone. */
  codec: "h264" | "hevc" | "av1" | "vp9" | "gif" | "png" | "exr" | (string & {});
  audio: boolean;
  range: [number, number];
  /** The container: `mp4`, `mov`, `mkv`, `webm`, `gif`, `png`/`exr` (a zipped sequence), or sound alone. */
  format?: string;
  quality?: "draft" | "standard" | "high" | "master";
  /** Set when transparency was kept. */
  alpha?: boolean;
}

export interface InputRecord {
  uri: string;
  size: number;
  bytes_read: number;
  partial: boolean;
  entries?: number;
}

export interface FontRecord {
  family: string;
  weight: number;
  style: string;
  uri: string;
  sha256: string;
}

export interface Timings {
  instance_start_s?: number | null;
  fetch_s?: number;
  unpack_s?: number;
  plan_s?: number;
  render_s?: number;
  encode_s?: number;
  upload_s?: number;
  total_s?: number;
}

/** What a render wrote. Inline renders' receipts have no `object`, `manifest` or `url`. */
export interface Receipt {
  receipt_version: number;
  id?: string;
  status: "rendered" | "already_rendered" | (string & {});
  /** Where to download the video; null when it went to an `output.uri` the server cannot sign for. */
  url?: string | null;
  url_expires_at?: string | null;
  duration?: number;
  frames?: number;
  bytes?: number;
  /** Hex sha256 of the mp4 bytes. */
  sha256?: string;
  /** Rendered minutes as Cloud counts them. */
  minutes?: number;
  minutes_billed?: number;
  attempt?: Attempt;
  object?: StoredObject | null;
  manifest?: { uri: string; generation?: string } | null;
  video?: VideoInfo;
  renderer?: { service?: string; version?: string; git?: string; image_digest?: string };
  document?: { sha256: string; format_version: number; meta?: Record<string, unknown> };
  inputs?: InputRecord[];
  fonts?: FontRecord[];
  timings?: Timings;
  warnings?: string[];
  created_at?: string;
  [key: string]: unknown;
}

export type JobStatus = "queued" | "rendering" | "rendered" | "failed" | "canceled";

/** A job as `GET /v1/jobs/{id}` answers it. */
export interface JobData {
  id: string;
  status: JobStatus;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  webhook: string | null;
  progress: { frames: number; of: number } | null;
  receipt: Receipt | null;
  error: ApiErrorBody | null;
  [key: string]: unknown;
}

/** The body of a webhook delivery: the job, plus the event and when it was sent. */
export interface WebhookEvent extends JobData {
  event: "job.rendered" | "job.failed" | (string & {});
  sent_at: string;
}

/** An upload. Use `uri` in any file param. */
export interface Asset {
  id: string;
  uri: string;
  bytes: number;
  sha256: string;
  filename: string;
  content_type: string;
  created_at: string;
  [key: string]: unknown;
}

/** What a document will cost before it renders. */
export interface EstimateData {
  minutes: number;
  /** The deployment's price, or null on a server without one. */
  usd: number | null;
  gpu_seconds: number;
  first_byte_s: number;
  width: number;
  height: number;
  fps: number;
  seconds: number;
  frames: number;
  device: string;
  sampled_frames: number;
  [key: string]: unknown;
}

export interface CheckResult {
  /** False when any issue is an error. */
  ok: boolean;
  issues: Issue[];
}

/** One thing a viewer would trip on, over `from`..`to` seconds. */
export interface ReviewIssue {
  severity: "error" | "warning" | (string & {});
  kind: string;
  node: string;
  from: number;
  to: number;
  message: string;
  [key: string]: unknown;
}

export interface ReviewData {
  issues: ReviewIssue[];
  frames: number;
  texts: number;
  seconds: number;
  [key: string]: unknown;
}

export interface Me {
  caller: string;
  auth: "key" | "oidc" | (string & {});
  /** Signs webhook deliveries (see `verifyWebhook`). */
  webhook_secret: string;
  [key: string]: unknown;
}

export interface Health {
  ok: boolean;
  ready: boolean;
  busy: boolean;
  device?: string;
  backend?: string;
  version?: string;
  git?: string;
  image_digest?: string;
  uptime_s?: number;
  queued?: number;
  storage?: string;
  [key: string]: unknown;
}

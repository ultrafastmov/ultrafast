import type { ApiErrorBody, Issue } from "./types.js";

/** The API's error codes, plus the client's own (status 0). */
export type ErrorCode =
  | "bad_request"
  | "invalid_document"
  | "unauthorized"
  | "payment_required"
  | "input_not_allowed"
  | "output_not_allowed"
  | "local_path_not_allowed"
  | "not_found"
  | "output_conflict"
  | "job_started"
  | "over_limit"
  | "input_unavailable"
  | "busy"
  | "render_failed"
  | "unavailable"
  | "deadline_exceeded"
  /** A feature only ultrafast Cloud has, asked of a self-hosted `ultrafast serve` (501). */
  | "cloud_only"
  /** An HTTP error without an API error body (a proxy, say). */
  | "http_error"
  /** No response: DNS, refused, reset, or the connection dropped. */
  | "connection_error"
  /** A local file the document names is missing or cannot be read. */
  | "local_file"
  /** A download did not match the receipt's sha256. */
  | "integrity"
  /** `job.wait()` ran past its `timeoutMs`. */
  | "timeout"
  /** The job was canceled. */
  | "canceled"
  | (string & {});

export interface UltrafastErrorInit {
  status?: number;
  code?: ErrorCode;
  retry?: boolean;
  body?: unknown;
  cause?: unknown;
}

/**
 * Every error the client throws. `status` is the HTTP status (0 when there
 * was no response), `code` the API's error code, `retry` whether the same
 * request may succeed later, `body` the error object as the server sent it.
 */
export class UltrafastError extends Error {
  readonly status: number;
  readonly code: ErrorCode;
  readonly retry: boolean;
  readonly body: unknown;
  /** `invalid_document`: every problem the check found. */
  readonly issues?: Issue[];
  /** `input_not_allowed`, `output_not_allowed`, `input_unavailable`. */
  readonly location?: string;
  /** `input_unavailable`: `not_found`, `integrity`, ... */
  readonly reason?: string;
  /** `local_path_not_allowed`: the node, param and value. */
  readonly node?: string;
  readonly param?: string;
  readonly value?: unknown;
  /** `over_limit`: which limit, the value and the maximum. */
  readonly limit?: string;
  readonly max?: number;
  /** `output_conflict`: the existing object's identity. */
  readonly object?: Record<string, unknown>;
  /** `cloud_only`: `jobs`, `bucket_output` or `bucket_inputs`. */
  readonly feature?: string;

  constructor(message: string, init: UltrafastErrorInit = {}) {
    super(message, init.cause === undefined ? undefined : { cause: init.cause });
    this.name = new.target.name;
    this.status = init.status ?? 0;
    this.code = init.code ?? "http_error";
    this.retry = init.retry ?? false;
    this.body = init.body;
    const b = init.body;
    if (b && typeof b === "object") {
      const e = b as Record<string, unknown>;
      for (const k of ["issues", "location", "reason", "node", "param", "value", "limit", "max", "object", "feature"] as const) {
        if (k in e) (this as Record<string, unknown>)[k] = e[k];
      }
    }
  }
}

export class BadRequestError extends UltrafastError {}
export class InvalidDocumentError extends UltrafastError {
  declare readonly issues: Issue[];
}
export class UnauthorizedError extends UltrafastError {}
export class PaymentRequiredError extends UltrafastError {}
export class InputNotAllowedError extends UltrafastError {}
export class OutputNotAllowedError extends UltrafastError {}
export class LocalPathNotAllowedError extends UltrafastError {}
export class NotFoundError extends UltrafastError {}
export class OutputConflictError extends UltrafastError {}
export class JobStartedError extends UltrafastError {}
export class OverLimitError extends UltrafastError {}
export class InputUnavailableError extends UltrafastError {}
export class BusyError extends UltrafastError {}
export class RenderFailedError extends UltrafastError {}
export class UnavailableError extends UltrafastError {}
export class DeadlineExceededError extends UltrafastError {}
/**
 * A self-hosted `ultrafast serve` was asked for something only ultrafast
 * Cloud has: jobs and webhooks (`feature: "jobs"`), `uri` output to your
 * own bucket (`bucket_output`), or `gs://`/`s3://`/`r2://` inputs
 * (`bucket_inputs`).
 */
export class CloudOnlyError extends UltrafastError {}
export class ConnectionError extends UltrafastError {}
export class LocalFileError extends UltrafastError {}

const BY_CODE: Record<string, typeof UltrafastError> = {
  bad_request: BadRequestError,
  invalid_document: InvalidDocumentError,
  unauthorized: UnauthorizedError,
  payment_required: PaymentRequiredError,
  input_not_allowed: InputNotAllowedError,
  output_not_allowed: OutputNotAllowedError,
  local_path_not_allowed: LocalPathNotAllowedError,
  not_found: NotFoundError,
  output_conflict: OutputConflictError,
  job_started: JobStartedError,
  over_limit: OverLimitError,
  input_unavailable: InputUnavailableError,
  busy: BusyError,
  render_failed: RenderFailedError,
  unavailable: UnavailableError,
  deadline_exceeded: DeadlineExceededError,
  cloud_only: CloudOnlyError,
};

/** The error for an API error object (`{status, code, message, retry, ...}`). */
export function errorFromBody(body: ApiErrorBody, status = body.status ?? 0): UltrafastError {
  const Cls = BY_CODE[body.code] ?? UltrafastError;
  return new Cls(body.message || body.code || `HTTP ${status}`, {
    status,
    code: body.code,
    retry: body.retry === true,
    body,
  });
}

/** The error for a non-2xx response whose body is `text`. */
export function errorFromResponse(status: number, text: string): UltrafastError {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = undefined;
  }
  const e = (parsed as { error?: unknown } | undefined)?.error;
  if (e && typeof e === "object" && typeof (e as ApiErrorBody).code === "string") {
    return errorFromBody(e as ApiErrorBody, status);
  }
  const snippet = text.trim().slice(0, 200);
  return new UltrafastError(`HTTP ${status}${snippet ? `: ${snippet}` : ""}`, {
    status,
    code: status === 404 ? "not_found" : "http_error",
    retry: status === 502 || status === 503 || status === 504,
    body: parsed ?? text,
  });
}

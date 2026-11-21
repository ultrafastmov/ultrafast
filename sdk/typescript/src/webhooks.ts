// Webhook signatures, with Web Crypto so it runs in Node and edge runtimes.

import { load } from "./runtime.js";
import type { WebhookEvent } from "./types.js";

export class WebhookVerificationError extends Error {
  /** `bad_signature`, `stale` (sent_at too old or in the future) or `bad_body`. */
  readonly code: "bad_signature" | "stale" | "bad_body";
  constructor(message: string, code: WebhookVerificationError["code"]) {
    super(message);
    this.name = "WebhookVerificationError";
    this.code = code;
  }
}

export interface VerifyWebhookOptions {
  /** Refuse deliveries whose `sent_at` is further than this from now (default 300 s; 0 to skip). */
  toleranceSec?: number;
  /** The current time, for tests. */
  now?: Date;
}

async function subtle(): Promise<SubtleCrypto> {
  if (globalThis.crypto?.subtle) return globalThis.crypto.subtle;
  // Node 18 without the global.
  const c = (await load("node:crypto")) as { webcrypto: { subtle: SubtleCrypto } };
  return c.webcrypto.subtle;
}

function hexBytes(hex: string): Uint8Array | null {
  if (!/^[0-9a-fA-F]{64}$/.test(hex)) return null;
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/**
 * Checks a webhook delivery and returns the job it carries.
 *
 * `rawBody` is the request body exactly as received (verify before parsing),
 * `signatureHeader` the `Ultrafast-Signature` header (`sha256=<hex>`), and
 * `secret` the `webhook_secret` from `uf.me()`. Throws
 * `WebhookVerificationError` when the signature does not match or `sent_at`
 * is outside the tolerance.
 */
export async function verifyWebhook(
  rawBody: string | Uint8Array | ArrayBuffer,
  signatureHeader: string | null | undefined,
  secret: string,
  opts: VerifyWebhookOptions = {},
): Promise<WebhookEvent> {
  const body =
    typeof rawBody === "string"
      ? new TextEncoder().encode(rawBody)
      : rawBody instanceof Uint8Array
        ? rawBody
        : new Uint8Array(rawBody);
  // One `sha256=` signature, or several separated by commas while a secret rotates.
  const sigs = (signatureHeader ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.startsWith("sha256="))
    .map((s) => hexBytes(s.slice(7)))
    .filter((b): b is Uint8Array => b !== null);
  if (!sigs.length) throw new WebhookVerificationError("missing or malformed Ultrafast-Signature header", "bad_signature");

  const s = await subtle();
  const key = await s.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["verify"]);
  let valid = false;
  // HMAC verify compares in constant time.
  for (const sig of sigs) valid = (await s.verify("HMAC", key, sig as BufferSource, body as BufferSource)) || valid;
  if (!valid) throw new WebhookVerificationError("signature does not match the body", "bad_signature");

  let event: WebhookEvent;
  try {
    event = JSON.parse(new TextDecoder().decode(body)) as WebhookEvent;
  } catch {
    throw new WebhookVerificationError("body is not JSON", "bad_body");
  }
  const tolerance = opts.toleranceSec ?? 300;
  if (tolerance > 0) {
    const sent = Date.parse(event.sent_at);
    const now = (opts.now ?? new Date()).getTime();
    if (!Number.isFinite(sent)) throw new WebhookVerificationError("body has no sent_at", "bad_body");
    if (Math.abs(now - sent) > tolerance * 1000) {
      throw new WebhookVerificationError(`sent_at ${event.sent_at} is more than ${tolerance} s from now`, "stale");
    }
  }
  return event;
}

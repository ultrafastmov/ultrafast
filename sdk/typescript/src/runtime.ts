// What differs between Node and edge runtimes. Node modules load on first use
// through a dynamic import bundlers leave alone, so the core runs anywhere
// with fetch.

import { LocalFileError } from "./errors.js";

type FsModule = typeof import("node:fs");
type FsPromises = typeof import("node:fs/promises");
type PathModule = typeof import("node:path");
type OsModule = typeof import("node:os");
type UrlModule = typeof import("node:url");
type StreamModule = typeof import("node:stream");
type StreamPromises = typeof import("node:stream/promises");
type CryptoModule = typeof import("node:crypto");

export interface NodeModules {
  fs: FsModule;
  fsp: FsPromises;
  path: PathModule;
  os: OsModule;
  url: UrlModule;
  stream: StreamModule;
  streamp: StreamPromises;
  crypto: CryptoModule;
}

/** A dynamic import bundlers do not follow (the specifier is a variable). */
export const load = (specifier: string): Promise<any> => import(/* webpackIgnore: true */ /* @vite-ignore */ specifier);

let nodeModules: Promise<NodeModules> | undefined;

/** Node's modules, or a LocalFileError naming `what` needs them. */
export function node(what = "reading local files"): Promise<NodeModules> {
  nodeModules ??= (async () => {
    const [fs, fsp, path, os, url, stream, streamp, crypto] = await Promise.all(
      ["node:fs", "node:fs/promises", "node:path", "node:os", "node:url", "node:stream", "node:stream/promises", "node:crypto"].map(load),
    );
    return { fs, fsp, path, os, url, stream, streamp, crypto } as NodeModules;
  })();
  return nodeModules.catch((cause) => {
    nodeModules = undefined;
    throw new LocalFileError(
      `${what} needs Node (node:fs); in this runtime pass documents as objects and media as asset:// or https:// references`,
      { code: "local_file", cause },
    );
  });
}

/** An environment variable, where the runtime has them. */
export function env(name: string): string | undefined {
  const p = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process;
  const v = p?.env?.[name];
  return v === "" ? undefined : v;
}

export const isBrowser = typeof (globalThis as { document?: unknown }).document !== "undefined";

/** A random id of `A-Za-z0-9`. */
export function randomId(prefix: string, length = 20): string {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZabcdefghjkmnpqrstvwxyz";
  const bytes = new Uint8Array(length);
  globalThis.crypto?.getRandomValues
    ? globalThis.crypto.getRandomValues(bytes)
    : bytes.forEach((_, i) => (bytes[i] = Math.floor(Math.random() * 256)));
  let out = prefix;
  for (const b of bytes) out += alphabet[b % alphabet.length];
  return out;
}

export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason);
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(signal!.reason);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

export function toBase64(bytes: Uint8Array): string {
  const B = (globalThis as { Buffer?: { from(b: Uint8Array): { toString(enc: string): string } } }).Buffer;
  if (B) return B.from(bytes).toString("base64");
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

const MIME: Record<string, string> = {
  mp4: "video/mp4", m4v: "video/mp4", mov: "video/quicktime", webm: "video/webm", mkv: "video/x-matroska",
  m4a: "audio/mp4", mp3: "audio/mpeg", wav: "audio/wav", aac: "audio/aac", flac: "audio/flac", ogg: "audio/ogg", opus: "audio/opus",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", gif: "image/gif", avif: "image/avif",
  exr: "image/x-exr", hdr: "image/vnd.radiance", svg: "image/svg+xml",
  json: "application/json", gltf: "model/gltf+json", glb: "model/gltf-binary", cube: "text/plain",
  srt: "application/x-subrip", vtt: "text/vtt", txt: "text/plain",
  ttf: "font/ttf", otf: "font/otf", ttc: "font/collection", otc: "font/collection", woff: "font/woff", woff2: "font/woff2",
  zip: "application/zip",
};

export function contentTypeFor(filename: string): string {
  const ext = /\.([A-Za-z0-9]+)$/.exec(filename)?.[1]?.toLowerCase();
  return (ext && MIME[ext]) || "application/octet-stream";
}

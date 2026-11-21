// Local files a document names: found, uploaded once, and swapped for their
// asset:// references in a copy of the document.

import { LocalFileError } from "./errors.js";
import { node } from "./runtime.js";
import type { Asset } from "./types.js";

/** Where a loaded document came from: its relative paths resolve here. */
export const BASE_DIR: unique symbol = Symbol.for("ultrafastmov.baseDir");

/**
 * How a string in a param is read:
 * - `file`: a file reference;
 * - `font`: a font file, or a family name;
 * - `faces`: face tracks as inline JSON text, or a file;
 * - `maybe`: a component param: a file only if one exists at that path;
 * - `none`: not a file (only audio drivers inside it name files).
 */
type Kind = "file" | "font" | "faces" | "maybe" | "none";

const FILE_PARAMS: Record<string, Record<string, Kind>> = {
  image: { src: "file" },
  video: { src: "file" },
  audio: { src: "file" },
  svg: { src: "file" },
  lottie: { src: "file" },
  lut: { src: "file" },
  mesh: { src: "file", font: "font" },
  captions: { src: "file", font: "font" },
  clip_camera: { src: "file", faces: "faces" },
  block: { logo: "file", display_font: "font", text_font: "font", mono_font: "font" },
  text: { font: "font", em_font: "font" },
  swarm: { font: "font" },
  code: { font: "font" },
};

const REFERENCE = /^(asset|https?|gs|s3|r2):\/\//i;
const FONT_FILE = /\.(ttf|otf|ttc|otc|woff2?|dfont)$/i;

/** A string the server reads as it is: a reference, a data URL, or another scheme. */
export function isReference(v: string): boolean {
  if (REFERENCE.test(v) || /^data:/i.test(v)) return true;
  if (/^file:/i.test(v) || /^[A-Za-z]:[\\/]/.test(v)) return false;
  return /^[A-Za-z][A-Za-z0-9+.-]+:/.test(v);
}

/** A font param naming a family (`"Inter"`, `"ultrafast:serif"`) rather than a file. */
export function isFontName(v: string): boolean {
  return !v.includes("/") && !v.includes("\\") && !v.startsWith(".") && !v.startsWith("~") && !FONT_FILE.test(v);
}

interface Site {
  path: (string | number)[];
  value: string;
  kind: Kind;
  where: string;
}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

/** Every string in `doc` that may name a local file. */
export function findLocalFiles(doc: unknown): Site[] {
  const sites: Site[] = [];
  if (!isObj(doc)) return sites;
  const components = isObj(doc.components) ? doc.components : {};

  const consider = (value: string, path: Site["path"], kind: Kind, where: string, inComponent: boolean) => {
    const v = value.trim();
    if (!v || kind === "none" || isReference(v)) return;
    if (inComponent && v.startsWith("$")) return;
    if (kind === "font" && isFontName(v)) return;
    if (kind === "faces" && (v.startsWith("[") || v.startsWith("{"))) return;
    if (kind === "maybe" && (v.length > 1024 || v.includes("\n") || v.startsWith("#"))) return;
    sites.push({ path, value: v, kind, where });
  };

  const visit = (v: unknown, path: Site["path"], kind: Kind, where: string, inComponent: boolean): void => {
    if (typeof v === "string") return consider(v, path, kind, where, inComponent);
    if (Array.isArray(v)) {
      if (kind !== "none") v.forEach((x, i) => visit(x, [...path, i], kind, where, inComponent));
      return;
    }
    if (!isObj(v)) return;
    if (Array.isArray(v.keys)) {
      v.keys.forEach((k, i) => isObj(k) && visit(k.v, [...path, "keys", i, "v"], kind, where, inComponent));
    }
    if (isObj(v.audio) && typeof v.audio.src === "string") {
      consider(v.audio.src, [...path, "audio", "src"], "file", `${where} (audio driver)`, inComponent);
    }
    for (const d of ["wiggle", "oscillate"]) {
      const drv = v[d];
      if (isObj(drv) && "base" in drv) visit(drv.base, [...path, d, "base"], kind, where, inComponent);
    }
  };

  const visitNodes = (nodes: unknown, prefix: Site["path"], inComponent: boolean) => {
    if (!isObj(nodes)) return;
    for (const [id, n] of Object.entries(nodes)) {
      if (!isObj(n) || !isObj(n.params)) continue;
      const op = typeof n.op === "string" ? n.op : "";
      const instance = Object.hasOwn(components, op);
      for (const [p, v] of Object.entries(n.params)) {
        const kind: Kind = instance ? "maybe" : (FILE_PARAMS[op]?.[p] ?? "none");
        visit(v, [...prefix, id, "params", p], kind, `node ${id}, param ${p}`, inComponent);
      }
    }
  };

  visitNodes(doc.nodes, ["nodes"], false);
  for (const [name, c] of Object.entries(components)) {
    if (!isObj(c)) continue;
    if (isObj(c.params)) {
      for (const [p, v] of Object.entries(c.params)) {
        visit(v, ["components", name, "params", p], "maybe", `component ${name}, param ${p}`, true);
      }
    }
    visitNodes(c.nodes, ["components", name, "nodes"], true);
  }
  const fonts = isObj(doc.meta) ? doc.meta.fonts : undefined;
  if (Array.isArray(fonts)) {
    fonts.forEach((f, i) => {
      if (isObj(f) && typeof f.src === "string") consider(f.src, ["meta", "fonts", i, "src"], "file", `meta.fonts[${i}]`, false);
    });
  }
  return sites;
}

export type Uploader = (path: string) => Promise<Asset>;

/**
 * `doc` with every local file uploaded and replaced by its asset:// uri, as
 * a copy; `doc` itself when it names none.
 */
export async function rewriteLocalFiles<T>(doc: T, upload: Uploader): Promise<T> {
  const sites = findLocalFiles(doc);
  if (!sites.length) return doc;
  let n;
  try {
    n = await node("uploading the local files a document names");
  } catch (e) {
    if (sites.every((s) => s.kind === "maybe")) return doc;
    const s = sites.find((x) => x.kind !== "maybe")!;
    throw new LocalFileError(`${(e as Error).message} (${s.where}: ${JSON.stringify(s.value)})`, { code: "local_file", cause: e });
  }
  const { path, os, url, fsp } = n;
  const base = (doc as { [BASE_DIR]?: string })[BASE_DIR] ?? (globalThis as { process?: { cwd(): string } }).process?.cwd() ?? ".";
  const resolve = (v: string) => {
    if (/^file:/i.test(v)) return url.fileURLToPath(v);
    if (v === "~" || v.startsWith("~/") || v.startsWith("~\\")) return path.join(os.homedir(), v.slice(1));
    return path.resolve(base, v);
  };
  const isFile = async (p: string) => (await fsp.stat(p).catch(() => undefined))?.isFile() ?? false;

  const plan: { site: Site; file: string; member?: string }[] = [];
  for (const site of sites) {
    const file = resolve(site.value);
    if (await isFile(file)) {
      plan.push({ site, file });
      continue;
    }
    // An archive member: frames.zip!/frame_{frame:06}.webp
    const bang = site.value.indexOf("!/");
    if (bang > 0 && (await isFile(resolve(site.value.slice(0, bang))))) {
      plan.push({ site, file: resolve(site.value.slice(0, bang)), member: site.value.slice(bang + 2) });
      continue;
    }
    if (site.kind === "maybe") continue;
    const hint = /\{frame/.test(site.value)
      ? '; for an image sequence, zip the frames and use "frames.zip!/<pattern>"'
      : "";
    throw new LocalFileError(`${site.where}: no file at ${file} (from ${JSON.stringify(site.value)})${hint}`, {
      code: "local_file",
      body: { where: site.where, value: site.value, path: file },
    });
  }
  if (!plan.length) return doc;

  const uris = new Map<string, Promise<Asset>>();
  for (const p of plan) if (!uris.has(p.file)) uris.set(p.file, upload(p.file));
  const assets = new Map<string, Asset>();
  await Promise.all([...uris].map(async ([file, a]) => assets.set(file, await a)));

  const copy = JSON.parse(JSON.stringify(doc)) as T;
  for (const { site, file, member } of plan) {
    const uri = assets.get(file)!.uri + (member === undefined ? "" : `!/${member}`);
    let at: any = copy;
    for (const k of site.path.slice(0, -1)) at = at[k];
    at[site.path[site.path.length - 1]!] = uri;
  }
  return copy;
}

// Installs an .mcpb the way Claude Desktop does and drives the server.
//
//   node smoke.mjs <bundle.mcpb> [--render] [--launch-only]
//
// Unpacks with @anthropic-ai/mcpb (the code Claude Desktop loads bundles
// with), resolves the launch command with its getMcpConfigForManifest, and
// starts it the way hosts do: from /, with a bare PATH and no user settings.
// Then: initialize, tools/list against the manifest, ultrafast_new and
// ultrafast_check on a relative path (it must land in ~/ultrafast), and with
// --render one frame on the GPU with the bundle's own Slang (no fetching).
// With --launch-only it prints the resolved command as JSON and exits,
// for running the server elsewhere (a clean container).
// Needs @anthropic-ai/mcpb resolvable from this file's folder.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync, statSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { unpackExtension, getMcpConfigForManifest } from "@anthropic-ai/mcpb";

const [bundle, ...flags] = process.argv.slice(2);
if (!bundle) throw new Error("usage: node smoke.mjs <bundle.mcpb> [--render] [--launch-only]");
const render = flags.includes("--render");
const launchOnly = flags.includes("--launch-only");

const root = process.env.SMOKE_DIR || mkdtempSync(join(tmpdir(), "mcpb-smoke-"));
const ext = join(root, "extension");
const home = join(root, "home");
if (!(await unpackExtension({ mcpbPath: bundle, outputDir: ext, silent: true }))) throw new Error("unpack failed");
const manifest = JSON.parse(readFileSync(join(ext, "manifest.json"), "utf8"));
const exe = join(ext, manifest.server.entry_point);
if (!(statSync(exe).mode & 0o111)) throw new Error(`${exe} is not executable after unpack`);
const systemDirs = { HOME: home, DESKTOP: join(home, "Desktop"), DOCUMENTS: join(home, "Documents"), DOWNLOADS: join(home, "Downloads") };
const cfg = await getMcpConfigForManifest({ manifest, extensionPath: ext, systemDirs, userConfig: {}, pathSeparator: "/" });
if (!cfg) throw new Error("no mcp_config resolved");
if (launchOnly) {
  console.log(JSON.stringify({ root, ext, home, command: cfg.command, args: cfg.args, platforms: manifest.compatibility?.platforms }));
  process.exit(0);
}

const env = { PATH: "/usr/bin:/bin", HOME: home, ULTRAFAST_SLANG_FETCH: "0", ...(cfg.env || {}) };
for (const k of ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY", "VK_ICD_FILENAMES"]) if (process.env[k]) env[k] = process.env[k];
const child = spawn(cfg.command, cfg.args, { cwd: "/", env, stdio: ["pipe", "pipe", "pipe"] });
let stderr = "";
child.stderr.on("data", (d) => (stderr += d));
child.stdin.on("error", () => {});
child.on("exit", (code) => {
  if (pending.size) {
    console.error(`server exited (${code}) mid-call; stderr: ${stderr.slice(-800)}`);
    process.exit(1);
  }
});
const pending = new Map();
let buf = "";
child.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i);
    buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    const m = JSON.parse(line);
    if (m.id !== undefined && pending.has(m.id)) {
      pending.get(m.id)(m);
      pending.delete(m.id);
    }
  }
});
let next = 1;
const call = (method, params, ms = 120000) =>
  new Promise((resolve, reject) => {
    const id = next++;
    const t = setTimeout(() => reject(new Error(`${method} timed out; stderr: ${stderr.slice(-500)}`)), ms);
    pending.set(id, (m) => (clearTimeout(t), m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result)));
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
const tool = async (name, args, ms) => {
  const r = await call("tools/call", { name, arguments: args }, ms);
  if (r.isError) throw new Error(`${name}: ${JSON.stringify(r.content).slice(0, 400)}`);
  return r;
};

const init = await call("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "mcpb-smoke", version: "0" } });
child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
console.log(`server ${init.serverInfo.name} ${init.serverInfo.version}, protocol ${init.protocolVersion}`);
if (init.serverInfo.version !== manifest.version) throw new Error(`server is ${init.serverInfo.version}, manifest ${manifest.version}`);
const listed = (await call("tools/list", {})).tools.map((t) => t.name).sort();
const declared = manifest.tools.map((t) => t.name).sort();
if (JSON.stringify(listed) !== JSON.stringify(declared)) throw new Error(`tools differ: server ${listed} manifest ${declared}`);
console.log(`tools/list: ${listed.length} tools, same as the manifest`);
await tool("ultrafast_new", { template: "title", path: "film.json" });
const doc = join(home, "ultrafast", "film.json");
if (!existsSync(doc)) throw new Error(`relative path did not land in ${doc}`);
console.log("ultrafast_new film.json -> ~/ultrafast/film.json");
const check = await tool("ultrafast_check", { path: "film.json" });
console.log(`ultrafast_check: ${check.content[0].text.split("\n")[0].slice(0, 100)}`);
if (render) {
  const frame = await tool("ultrafast_frame", { path: "film.json", t: 1.0, max_size: 640 }, 300000);
  const img = frame.content.find((c) => c.type === "image");
  if (!img) throw new Error(`ultrafast_frame returned no image: ${JSON.stringify(frame.content).slice(0, 300)}`);
  console.log(`ultrafast_frame: ${img.mimeType}, ${Math.round((img.data.length * 3) / 4 / 1024)} KB, Slang from the bundle`);
}
child.stdin.end();
child.kill();
if (!process.env.SMOKE_DIR) rmSync(root, { recursive: true, force: true });
console.log("ok");

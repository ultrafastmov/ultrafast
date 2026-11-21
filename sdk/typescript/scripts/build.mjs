#!/usr/bin/env node
// Builds dist/esm (ES modules) and dist/cjs (CommonJS), each with .d.ts.
import { execFileSync } from "node:child_process";
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const version = /VERSION = "([^"]+)"/.exec(readFileSync(join(root, "src", "version.ts"), "utf8"))?.[1];
if (version !== pkg.version) throw new Error(`src/version.ts says ${version}, package.json ${pkg.version}`);

const tsc = join(root, "node_modules", ".bin", "tsc");
rmSync(join(root, "dist"), { recursive: true, force: true });
for (const config of ["tsconfig.json", "tsconfig.cjs.json"]) {
  execFileSync(tsc, ["-p", join(root, config)], { stdio: "inherit" });
}
writeFileSync(join(root, "dist", "esm", "package.json"), '{ "type": "module" }\n');
writeFileSync(join(root, "dist", "cjs", "package.json"), '{ "type": "commonjs" }\n');
console.log(`built ultrafastmov ${pkg.version}: dist/esm, dist/cjs`);

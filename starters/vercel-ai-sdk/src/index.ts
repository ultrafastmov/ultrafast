import { parseArgs } from "node:util";
import { connect, download, makeVideo, mcpUrl, modelId, pickModel } from "./agent.ts";

try {
  process.loadEnvFile();
} catch {} // no .env file: use the environment as it is

const { values: flags, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    out: { type: "string", default: "out/film.mp4" },
    "max-steps": { type: "string", default: "40" },
    "list-tools": { type: "boolean", default: false },
  },
});
const brief = positionals.join(" ").trim();
const maxSteps = Number(flags["max-steps"]);

if ((!flags["list-tools"] && !brief) || !(Number.isInteger(maxSteps) && maxSteps > 0)) {
  console.error('Usage: npm start -- "A 15 s launch promo for a note-taking app" [--out film.mp4] [--max-steps 40]');
  console.error("       npm run list-tools");
  process.exit(2);
}
if (!process.env.ULTRAFAST_API_KEY && !process.env.ULTRAFAST_MCP_URL) {
  console.error("Set ULTRAFAST_API_KEY (see .env.example), or ULTRAFAST_MCP_URL for a self-hosted server.");
  process.exit(2);
}

const client = await connect();
try {
  if (flags["list-tools"]) {
    const { tools } = await client.listTools();
    console.log(`${tools.length} tools at ${mcpUrl()}`);
    for (const tool of tools) console.log(`  ${tool.name.padEnd(22)} ${tool.title ?? ""}`);
  } else {
    console.log(`${modelId()} on ${mcpUrl()}`);
    const { url, text, steps } = await makeVideo(brief, {
      client,
      model: pickModel(),
      maxSteps,
      onTool: (name) => console.log(`  ${name}`),
    });
    if (text) console.log(`\n${text}`);
    if (!url) {
      console.error(`\nNo export after ${steps} steps. Try a larger --max-steps.`);
      process.exitCode = 1;
    } else {
      console.log(`\nDownload: ${url}`);
      const bytes = await download(url, flags.out);
      console.log(`Saved ${flags.out} (${(bytes / 1e6).toFixed(1)} MB)`);
    }
  }
} finally {
  await client.close();
}

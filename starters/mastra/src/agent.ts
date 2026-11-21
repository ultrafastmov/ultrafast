import { mkdir, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { Agent } from "@mastra/core/agent";
import { MCPClient } from "@mastra/mcp";

/** The MCP endpoint: `ULTRAFAST_MCP_URL`, or ultrafast's hosted server. */
export const mcpUrl = () => process.env.ULTRAFAST_MCP_URL ?? "https://mcp.ultrafast.mov";
/** `MODEL` as a Mastra model router id, Claude Sonnet by default. */
export const modelId = () => process.env.MODEL ?? "anthropic/claude-sonnet-5-5";

const INSTRUCTIONS = `You make short videos with ultrafast from a brief.
Start from the closest template and rewrite it to fit the brief. Run ultrafast_check until it is clean.
Look at a contact sheet, then at single frames where something looks off. Run ultrafast_review and fix everything it reports.
Look at a sheet once more, then call ultrafast_export once. End with one line saying what you made.`;

/** Connects to ultrafast's MCP server (Streamable HTTP, API key as bearer). */
export function connect(): MCPClient {
  const key = process.env.ULTRAFAST_API_KEY;
  return new MCPClient({
    id: "ultrafast",
    registerProcessHooks: false, // the CLI disconnects itself
    servers: {
      ultrafast: {
        url: new URL(mcpUrl()),
        requestInit: { headers: key ? { Authorization: `Bearer ${key}` } : {} },
        timeout: 600_000, // an export renders the whole film
        forwardInstructions: true, // the server's own notes on the loop go into the system prompt
        instructionsMaxLength: 4000,
      },
    },
  });
}

/**
 * The server's tools under their own names (ultrafast_new, ...), as its
 * instructions call them. Throws when the server can't be reached.
 */
export async function listTools(mcp: MCPClient) {
  const { toolsets, errors } = await mcp.listToolsetsWithErrors();
  if (errors.ultrafast) throw new Error(`Can't reach ${mcpUrl()}: ${errors.ultrafast.split("\n")[0]}`);
  const tools = toolsets.ultrafast ?? {};
  for (const tool of Object.values(tools)) tool.toModelOutput = toModelOutput;
  return tools;
}

/**
 * Runs the agent on a brief until it exports. Returns the download link of
 * the last export (if any) and the agent's closing words.
 */
export async function makeVideo(
  brief: string,
  { mcp, maxSteps, onTool }: { mcp: MCPClient; maxSteps: number; onTool?: (name: string) => void },
) {
  const agent = new Agent({
    id: "ultrafast-director",
    name: "ultrafast director",
    instructions: INSTRUCTIONS,
    model: modelId(), // e.g. "anthropic/claude-sonnet-5-5"; Mastra reads ANTHROPIC_API_KEY
  });
  const result = await agent.generate(brief, {
    toolsets: { ultrafast: await listTools(mcp) },
    maxSteps,
    onStepFinish: ({ toolCalls }) => toolCalls.forEach((call) => onTool?.(call.payload.toolName)),
  });
  const urls = result.steps
    .flatMap((step) => step.toolResults)
    .filter((r) => r.payload.toolName === "ultrafast_export")
    .map((r) => downloadUrl(r.payload.result))
    .filter((url) => url !== undefined);
  return { url: urls.at(-1), text: result.text, steps: result.steps.length };
}

/**
 * What the model reads back from a tool: MCP text as text and MCP images
 * (sheets, frames) as images. Mastra sends images as images on its own, but
 * a text-only result goes as its JSON envelope, so a returned document would
 * arrive escaped twice.
 */
function toModelOutput(result: unknown) {
  const content = (result as { content?: { type: string; text?: string; data?: string; mimeType?: string }[] } | null)?.content;
  if (!Array.isArray(content)) return undefined; // not MCP content: Mastra's default
  return {
    type: "content",
    value: content.map((part) =>
      part.type === "image"
        ? { type: "image-data", data: part.data, mediaType: part.mimeType }
        : { type: "text", text: part.text ?? JSON.stringify(part) },
    ),
  };
}

/** The link in an ultrafast_export result: its "Download:" line, or the receipt's `url`. */
export function downloadUrl(output: unknown): string | undefined {
  const content = (output as { content?: { type: string; text?: string }[] } | null)?.content ?? [];
  for (const { text } of content) {
    const line = text?.match(/Download: (\S+)/);
    if (line) return line[1];
    try {
      const receipt = JSON.parse(text ?? "");
      if (typeof receipt?.url === "string") return receipt.url;
    } catch {}
  }
  return undefined;
}

/** Saves the video at `url` (a signed link, fetched without the API key) to `out`. Returns its size in bytes. */
export async function download(url: string, out: string): Promise<number> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`Download failed: HTTP ${res.status}`);
  const bytes = Buffer.from(await res.arrayBuffer());
  await mkdir(dirname(out), { recursive: true });
  await writeFile(out, bytes);
  return bytes.length;
}

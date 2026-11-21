import { mkdir, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { createAnthropic } from "@ai-sdk/anthropic";
import { createMCPClient, type MCPClient } from "@ai-sdk/mcp";
import { createOpenAI } from "@ai-sdk/openai";
import { generateText, isStepCount, type LanguageModel } from "ai";

/** The MCP endpoint: `ULTRAFAST_MCP_URL`, or ultrafast's hosted server. */
export const mcpUrl = () => process.env.ULTRAFAST_MCP_URL ?? "https://mcp.ultrafast.mov";
/** `MODEL` as "provider/model-id", Claude Sonnet by default. */
export const modelId = () => process.env.MODEL ?? "anthropic/claude-sonnet-5-5";

const INSTRUCTIONS = `You make short videos with ultrafast from a brief.
Start from the closest template and rewrite it to fit the brief. Run ultrafast_check until it is clean.
Look at a contact sheet, then at single frames where something looks off. Run ultrafast_review and fix everything it reports.
Look at a sheet once more, then call ultrafast_export once. End with one line saying what you made.`;

/** Connects to ultrafast's MCP server (Streamable HTTP, API key as bearer). */
export function connect(): Promise<MCPClient> {
  const key = process.env.ULTRAFAST_API_KEY;
  return createMCPClient({
    clientName: "ultrafast-starter-vercel-ai-sdk",
    transport: {
      type: "http",
      url: mcpUrl(),
      headers: key ? { Authorization: `Bearer ${key}` } : undefined,
    },
  });
}

/**
 * The AI SDK model for an id like "anthropic/claude-sonnet-5-5" or
 * "openai/gpt-6.2-astra". Providers read their keys (ANTHROPIC_API_KEY,
 * OPENAI_API_KEY) and base URLs from the environment.
 */
export function pickModel(id = modelId()): LanguageModel {
  const [provider, ...rest] = id.split("/");
  const name = rest.join("/");
  if (provider === "anthropic") return createAnthropic()(name);
  if (provider === "openai") return createOpenAI()(name);
  throw new Error(`MODEL must be anthropic/<id> or openai/<id>, got "${id}"`);
}

/**
 * Runs the agent on a brief until it exports. Returns the download link of
 * the last export (if any) and the agent's closing words.
 */
export async function makeVideo(
  brief: string,
  { client, model, maxSteps, onTool }: { client: MCPClient; model: LanguageModel; maxSteps: number; onTool?: (name: string) => void },
) {
  // MCP tools come with a toModelOutput that turns image content (sheets,
  // frames) into image parts, so the model sees the pictures, not base64 text.
  const tools = await client.tools();
  const result = await generateText({
    model,
    tools,
    instructions: [INSTRUCTIONS, client.instructions].filter(Boolean).join("\n\n"),
    prompt: brief,
    stopWhen: isStepCount(maxSteps),
    onToolExecutionStart: ({ toolCall }) => onTool?.(toolCall.toolName),
  });
  const urls = result.steps
    .flatMap((step) => step.toolResults)
    .filter((r) => r.toolName === "ultrafast_export")
    .map((r) => downloadUrl(r.output))
    .filter((url) => url !== undefined);
  return { url: urls.at(-1), text: result.text, steps: result.steps.length };
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

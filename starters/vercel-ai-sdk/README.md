# ultrafast + Vercel AI SDK

An agent that takes a brief and returns an MP4. It uses ultrafast's MCP tools
through the [AI SDK](https://ai-sdk.dev): it writes the video document, checks
it, looks at a contact sheet, runs the review, fixes what the review reports and
exports. The script then downloads the file.

## Setup

Node 22 or later.

```bash
npm install
cp .env.example .env   # fill in ULTRAFAST_API_KEY and ANTHROPIC_API_KEY
```

A `uf_test_` key renders with a watermark and is never billed.

## Run

```bash
npm start -- "A 15 s launch promo for a note-taking app"
npm start -- "A 6 s square loop for a coffee brand" --out coffee.mp4 --max-steps 60
npm run list-tools   # connects and lists the tools; no model key needed
```

It prints each tool call as it runs, then the download link, and saves the
video to `out/film.mp4` (or `--out`). `--max-steps` caps the model calls
(default 40).

The default model is `anthropic/claude-sonnet-5-5`. For OpenAI, set
`MODEL=openai/gpt-6.2-astra` and `OPENAI_API_KEY`.

## Self-hosting

Against your own `ultrafast serve`:

```bash
ULTRAFAST_MCP_URL=http://localhost:8080/mcp npm start -- "..."
```

Set `ULTRAFAST_API_KEY` to one of the server's keys. A server running without
auth needs none.

## How it works

- `src/agent.ts` connects with `createMCPClient` from `@ai-sdk/mcp`, using its
  built-in Streamable HTTP transport and the key as `Authorization: Bearer`.
- `client.tools()` turns the server's tools into AI SDK tools. Their
  `toModelOutput` passes image results (sheets, frames) to the model as images.
- `generateText` runs the loop with those tools, the server's instructions and
  `stopWhen: isStepCount(maxSteps)`.
- Documents travel inline: every call carries the whole document, and
  `ultrafast_edit` returns the edited one for the next call.
- `src/index.ts` takes the link from the `ultrafast_export` result
  (`Rendered. Download: <url>`), fetches it and closes the MCP client.

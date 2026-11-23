# ultrafast + Mastra

An agent that takes a brief and returns an MP4. It is a
[Mastra](https://mastra.ai) agent with ultrafast's MCP tools: it writes the
video document, checks it, looks at a contact sheet, runs the review, fixes what
the review reports and exports. The script then downloads the file.

## Setup

Node 22 or later, and `ultrafast serve` running on this machine. It prints an
API key on first start ([docs/service/SELF_HOST.md](../../docs/service/SELF_HOST.md)).

```bash
npm install
cp .env.example .env   # fill in ULTRAFAST_API_KEY and ANTHROPIC_API_KEY
```

## Run

```bash
npm start -- "A 15 s launch promo for a note-taking app"
npm start -- "A 6 s square loop for a coffee brand" --out coffee.mp4 --max-steps 60
npm run list-tools   # connects and lists the tools; no model key needed
```

It prints each step's tool calls, then the download link, and saves the video
to `out/film.mp4` (or `--out`). `--max-steps` caps the model calls (default 40).

The model is a Mastra router id, `anthropic/claude-sonnet-5-5` by default. For
OpenAI, set `MODEL=openai/gpt-6.2-astra` and `OPENAI_API_KEY`; other providers
Mastra knows work the same way.

## Another server

The starter connects to `ultrafast serve` on this machine by default. For a
server elsewhere:

```bash
ULTRAFAST_MCP_URL=https://video.example.com/mcp npm start -- "..."
```

Set `ULTRAFAST_API_KEY` to one of that server's keys. A server running without
auth (`ULTRAFAST_AUTH=none`) needs none. ultrafast Cloud's
`https://mcp.ultrafast.mov` will work the same way once it opens.

## How it works

- `src/agent.ts` configures an `MCPClient` from `@mastra/mcp` with the server
  `url` and the key in `requestInit.headers`. It forwards the server's
  instructions into the agent's system prompt.
- `listToolsets()` gives the tools under their own names (`ultrafast_new`, ...),
  passed to `agent.generate()` as `toolsets` with `maxSteps`.
- Each tool gets a `toModelOutput` that hands MCP text to the model as text and
  images (sheets, frames) as images.
- Documents travel inline: every call carries the whole document, and
  `ultrafast_edit` returns the edited one for the next call.
- `src/index.ts` takes the link from the `ultrafast_export` result
  (`Rendered. Download: <url>`), fetches it and disconnects the MCP client.

# Starters

Agents that take a brief and return a reviewed MP4: each writes the
document, looks at a contact sheet, fixes what `review` finds, exports and
downloads the video. They use ultrafast's MCP tools over Streamable HTTP.

| Starter | Language | Framework |
|---|---|---|
| [openai-agents](openai-agents) | Python | OpenAI Agents SDK |
| [langgraph](langgraph) | Python | LangGraph |
| [vercel-ai-sdk](vercel-ai-sdk) | TypeScript | Vercel AI SDK |
| [mastra](mastra) | TypeScript | Mastra |

Each connects to `ultrafast serve` on this machine
(`http://localhost:8080/mcp`) by default, or to the server in
`ULTRAFAST_MCP_URL`, with `ULTRAFAST_API_KEY` as the bearer token. Start the
server first; it prints an API key on first start
([docs/service/SELF_HOST.md](../docs/service/SELF_HOST.md)):

```bash
ultrafast serve     # or the Docker image, with --gpus all
```

ultrafast Cloud's `https://mcp.ultrafast.mov` will work the same way once it
opens. Every starter has `--list-tools` to check the connection before
spending model tokens.

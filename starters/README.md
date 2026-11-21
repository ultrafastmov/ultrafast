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

Each reads `ULTRAFAST_API_KEY` and `ULTRAFAST_MCP_URL`: ultrafast Cloud's
`https://mcp.ultrafast.mov` by default, or your own server's
`http://localhost:8080/mcp` (`ultrafast serve`, see
[docs/service/SELF_HOST.md](../docs/service/SELF_HOST.md)). Every one has
`--list-tools` to check the connection before spending model tokens.

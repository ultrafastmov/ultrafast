# ultrafast + LangGraph

An agent that takes a brief and returns a reviewed MP4. A small LangGraph
graph drives ultrafast's MCP tools over Streamable HTTP: the model writes
the document, looks at a contact sheet, runs review, fixes what it finds and
exports; a last node downloads the video.

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env            # ULTRAFAST_API_KEY, ANTHROPIC_API_KEY
python main.py "A 15 s launch promo for a note-taking app"
```

It prints each tool call, the agent's last words, the download link, and
saves `film.mp4` (`--out` to change it).

```bash
python main.py --list-tools     # connect and list the tools; no model call
```

## The graph

```
START -> agent -> tools -> agent -> ... -> download -> END
```

- `agent`: the chat model with the MCP tools bound.
- `tools`: LangGraph's `ToolNode` running them (`langchain-mcp-adapters`,
  one MCP session for the run). Frames and sheets come back as images.
- `download`: finds the link in the `ultrafast_export` result and saves the MP4.

```python
from langchain_mcp_adapters.client import MultiServerMCPClient

client = MultiServerMCPClient({"ultrafast": {
    "transport": "streamable_http",
    "url": "https://mcp.ultrafast.mov",
    "headers": {"Authorization": f"Bearer {key}"},
}})
```

## Settings

| Variable | Default |
|---|---|
| `ULTRAFAST_API_KEY` | required; sent as `Authorization: Bearer` |
| `ULTRAFAST_MCP_URL` | `https://mcp.ultrafast.mov`; self-hosted: `http://localhost:8080/mcp` |
| `MODEL` | `anthropic/claude-sonnet-5-5`; any `provider/model-id` that `init_chat_model` knows, e.g. `openai/...` |
| `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` | the provider's key |

Claude reads images inside tool results. For other providers the graph moves
each sheet or frame into a user message after the tool results, which the
OpenAI chat API accepts.

Values in `.env` are read at start; the environment wins.

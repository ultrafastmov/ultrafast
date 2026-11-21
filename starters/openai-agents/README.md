# ultrafast + OpenAI Agents SDK

An agent that takes a brief and returns a reviewed MP4. It uses ultrafast's
MCP tools over Streamable HTTP: it writes the document, looks at a contact
sheet, runs review, fixes what it finds, exports, and this script downloads
the video.

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env            # ULTRAFAST_API_KEY, OPENAI_API_KEY
python main.py "A 15 s launch promo for a note-taking app"
```

It prints each tool call as it goes, the agent's last words, the download
link, and saves `film.mp4` (`--out` to change it).

```bash
python main.py --list-tools     # connect and list the tools; no model call
```

## The core of it

```python
from agents import Agent, Runner
from agents.mcp import MCPServerStreamableHttp

async with MCPServerStreamableHttp(params={
    "url": "https://mcp.ultrafast.mov",
    "headers": {"Authorization": f"Bearer {key}"},
}) as ultrafast:
    agent = Agent(
        name="Editor",
        mcp_servers=[ultrafast],
        instructions="Make it, review it, export it.",
    )
    result = await Runner.run(agent, "A 15 s launch promo")
```

`main.py` adds a few lines of instructions (template, check, sheet,
review, export), longer timeouts (review and export render the whole film),
and reads the link from the `ultrafast_export` result.

## Settings

| Variable | Default |
|---|---|
| `ULTRAFAST_API_KEY` | required; sent as `Authorization: Bearer` |
| `ULTRAFAST_MCP_URL` | `https://mcp.ultrafast.mov`; self-hosted: `http://localhost:8080/mcp` |
| `OPENAI_MODEL` | the Agents SDK's default model |
| `OPENAI_API_KEY` | required by the Agents SDK |

Values in `.env` are read at start; the environment wins.

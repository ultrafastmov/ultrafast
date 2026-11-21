"""A brief in, a reviewed MP4 out: a LangGraph agent with ultrafast's MCP tools.

    python main.py "A 15 s launch promo for a note-taking app"
    python main.py --list-tools          # connect and list the tools; no model call

The graph: the model calls ultrafast's tools (write, check, look at a
contact sheet, review, fix, export) until it stops, then a last node
downloads the export's MP4.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, List, Optional, Tuple
from urllib.parse import urlparse

from langchain.chat_models import init_chat_model
from langchain_core.messages import AIMessage, BaseMessage, HumanMessage, SystemMessage, ToolMessage
from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_mcp_adapters.tools import load_mcp_tools
from langgraph.graph import END, START, MessagesState, StateGraph
from langgraph.prebuilt import ToolNode

INSTRUCTIONS = """Make it, review it, export it.

You make short videos with ultrafast from a brief. Documents travel inline:
pass the whole document to every tool, and keep the one ultrafast_edit returns.
1. Start from the closest template (ultrafast_new) and rewrite it to fit the
   brief. Run ultrafast_check until it is clean.
2. Look at a contact sheet (ultrafast_sheet), then at single frames
   (ultrafast_frame) where something looks off.
3. Run ultrafast_review and fix everything it reports. Look at a sheet again.
4. Call ultrafast_export once. End with one line saying what you made."""

DEFAULT_MODEL = "anthropic/claude-sonnet-5-5"


def load_env(path: Path = Path(__file__).with_name(".env")) -> None:
    """KEY=value lines from .env into the environment (without overriding it)."""
    if not path.exists():
        return
    for line in path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, value = line.partition("=")
            value = value.strip().strip('"').strip("'")
            if value:
                os.environ.setdefault(key.strip(), value)


def model_spec(spec: Optional[str] = None) -> Tuple[Optional[str], str]:
    """``MODEL`` as provider/model-id (or provider:model-id): ``("anthropic", "claude-sonnet-5-5")``."""
    spec = spec or os.environ.get("MODEL") or DEFAULT_MODEL
    for sep in ("/", ":"):
        provider, found, name = spec.partition(sep)
        if found and provider in ("anthropic", "openai", "google_genai", "google_vertexai", "bedrock_converse", "mistralai", "ollama", "azure_openai", "groq", "xai"):
            return provider, name
    return None, spec  # let init_chat_model infer the provider


def download_url(output: Any) -> Optional[str]:
    """The link in an ultrafast_export result: its "Download:" line, a receipt's `url`, or any URL."""
    texts: List[str] = []

    def collect(x: Any) -> None:
        if isinstance(x, str):
            texts.append(x)
        elif isinstance(x, dict):
            if isinstance(x.get("url"), str):
                texts.append(json.dumps({"url": x["url"]}))
            for v in x.values():
                collect(v)
        elif isinstance(x, (list, tuple)):
            for v in x:
                collect(v)

    collect(output)
    for text in texts:
        m = re.search(r"Download:\s*(\S+)", text)
        if m:
            return m.group(1)
    for text in texts:
        try:
            data = json.loads(text)
        except ValueError:
            continue
        if isinstance(data, dict) and isinstance(data.get("url"), str):
            return data["url"]
    for text in texts:
        m = re.search(r"https?://[^\s\"'<>)\]]+", text)
        if m:
            return m.group(0)
    return None


def download(url: str, out: Path, key: str, mcp_url: str) -> Path:
    """Stream the MP4 to `out`. The link is signed; the key is only sent back to the MCP host."""
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".part")
    headers = {"User-Agent": "ultrafast-starter-langgraph"}
    for attempt in (0, 1):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=120) as r, open(tmp, "wb") as f:
                while chunk := r.read(1 << 20):
                    f.write(chunk)
            tmp.replace(out)
            return out
        except urllib.error.HTTPError as e:
            same_host = urlparse(url).hostname == urlparse(mcp_url).hostname
            if attempt == 0 and e.code in (401, 403) and same_host:
                headers["Authorization"] = f"Bearer {key}"
                continue
            raise
        finally:
            if tmp.exists():
                tmp.unlink()
    return out


def images_as_user_messages(messages: List[BaseMessage]) -> List[BaseMessage]:
    """Moves images out of tool results into a user message after them.

    Claude reads images inside tool results; OpenAI's chat API takes them
    only from the user, so for other providers the sheet goes there.
    """
    out: List[BaseMessage] = []
    pending: List[Any] = []

    def flush() -> None:
        if pending:
            out.append(HumanMessage(content=[{"type": "text", "text": "The images the tools above returned:"}, *pending]))
            pending.clear()

    for m in messages:
        if isinstance(m, ToolMessage) and isinstance(m.content, list):
            images = [b for b in m.content if isinstance(b, dict) and b.get("type") in ("image", "image_url")]
            if images:
                rest = [b for b in m.content if b not in images] or [{"type": "text", "text": "(image below)"}]
                m = m.model_copy(update={"content": rest})
                pending.extend(images)
            out.append(m)
            continue
        flush()
        out.append(m)
    flush()
    return out


class State(MessagesState):
    url: Optional[str]
    path: Optional[str]


def build_graph(model: Any, tools: List[Any], *, instructions: str, provider: Optional[str], out: Path, key: str, mcp_url: str):
    """agent -> tools -> agent ... -> download -> END."""
    bound = model.bind_tools(tools)

    async def agent(state: State) -> dict:
        messages: List[BaseMessage] = [SystemMessage(instructions), *state["messages"]]
        if provider != "anthropic":
            messages = images_as_user_messages(messages)
        return {"messages": [await bound.ainvoke(messages)]}

    def route(state: State) -> str:
        last = state["messages"][-1]
        return "tools" if isinstance(last, AIMessage) and last.tool_calls else "download"

    def fetch(state: State) -> dict:
        exports = [m for m in state["messages"] if isinstance(m, ToolMessage) and m.name == "ultrafast_export"]
        link = None
        for m in reversed(exports):
            link = download_url(m.content) or download_url(getattr(m, "artifact", None))
            if link:
                break
        if not link:
            return {"url": None, "path": None}
        return {"url": link, "path": str(download(link, out, key, mcp_url))}

    graph = StateGraph(State)
    graph.add_node("agent", agent)
    graph.add_node("tools", ToolNode(tools))
    graph.add_node("download", fetch)
    graph.add_edge(START, "agent")
    graph.add_conditional_edges("agent", route, {"tools": "tools", "download": "download"})
    graph.add_edge("tools", "agent")
    graph.add_edge("download", END)
    return graph.compile()


def explain(e: BaseException) -> str:
    """One line for a failure buried in exception groups (as MCP transports raise them)."""
    leaves, stack = [], [e]
    while stack:
        x = stack.pop()
        subs = getattr(x, "exceptions", None)
        if subs:
            stack.extend(subs)
        else:
            leaves.append(x)
    text = "; ".join(dict.fromkeys(f"{type(x).__name__}: {x}" for x in leaves if str(x)))
    if "401" in text or "nauthorized" in text:
        text += " (check ULTRAFAST_API_KEY and ULTRAFAST_MCP_URL)"
    return text or type(e).__name__


async def run(args: argparse.Namespace, key: str, url: str) -> int:
    client = MultiServerMCPClient(
        {
            "ultrafast": {
                "transport": "streamable_http",
                "url": url,
                "headers": {"Authorization": f"Bearer {key}"},
                "timeout": 30,
                # review and export render the whole film: give tool calls time
                "sse_read_timeout": 900,
            }
        }
    )
    async with client.session("ultrafast") as session:
        tools = await load_mcp_tools(session)
        if args.list_tools:
            for tool in tools:
                print(f"{tool.name}: {(tool.description or '').splitlines()[0][:100]}")
            return 0
        instructions = INSTRUCTIONS
        try:
            info = await client.get_server_info(server_name="ultrafast")
            extra = getattr(info.get("ultrafast"), "instructions", None)
            instructions = "\n\n".join(x for x in (INSTRUCTIONS, extra) if x)
        except Exception:  # noqa: BLE001 (older adapters, or a server without instructions)
            pass
        provider, name = model_spec()
        model = init_chat_model(name, model_provider=provider)
        graph = build_graph(model, tools, instructions=instructions, provider=provider or ("anthropic" if name.startswith("claude") else None),
                            out=Path(args.out), key=key, mcp_url=url)
        print(f"Making: {args.brief}", file=sys.stderr)
        state: dict = {}
        async for update in graph.astream({"messages": [HumanMessage(args.brief)]}, {"recursion_limit": args.max_steps}, stream_mode="updates"):
            for node, change in update.items():
                state.update(change or {})
                for m in (change or {}).get("messages", []):
                    for call in getattr(m, "tool_calls", None) or []:
                        print(f"  {call['name']}", file=sys.stderr, flush=True)
                    if node == "agent" and isinstance(m, AIMessage) and not m.tool_calls:
                        print(m.text if isinstance(getattr(m, "text", None), str) else m.content)
    if not state.get("url"):
        print("The agent did not export a video.", file=sys.stderr)
        return 1
    print(state["url"])
    print(f"Saved {state['path']} ({Path(state['path']).stat().st_size} bytes)", file=sys.stderr)
    return 0


async def main() -> int:
    load_env()
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("brief", nargs="?", default="A 15 s launch promo", help="what to make")
    p.add_argument("--out", default="film.mp4", help="where to save the MP4 (default film.mp4)")
    p.add_argument("--list-tools", "--dry-run", action="store_true", help="connect, list the MCP tools and stop")
    p.add_argument("--max-steps", type=int, default=100, help="graph steps before giving up (two per tool round)")
    args = p.parse_args()

    key = os.environ.get("ULTRAFAST_API_KEY")
    if not key:
        print("Set ULTRAFAST_API_KEY (see .env.example).", file=sys.stderr)
        return 2
    url = os.environ.get("ULTRAFAST_MCP_URL", "https://mcp.ultrafast.mov")
    try:
        return await run(args, key, url)
    except Exception as e:  # noqa: BLE001
        print(f"Failed ({url}): {explain(e)}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

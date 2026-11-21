"""A brief in, a reviewed MP4 out: the OpenAI Agents SDK with ultrafast's MCP tools.

    python main.py "A 15 s launch promo for a note-taking app"
    python main.py --list-tools          # connect and list the tools; no model call

The agent writes the document, looks at a contact sheet, runs review, fixes
what it finds and exports; this script then downloads the MP4.
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

from agents import Agent, RunHooks, Runner
from agents.mcp import MCPServerStreamableHttp

INSTRUCTIONS = """Make it, review it, export it.

You make short videos with ultrafast from a brief. Documents travel inline:
pass the whole document to every tool, and keep the one ultrafast_edit returns.
1. Start from the closest template (ultrafast_new) and rewrite it to fit the
   brief. Run ultrafast_check until it is clean.
2. Look at a contact sheet (ultrafast_sheet), then at single frames
   (ultrafast_frame) where something looks off.
3. Run ultrafast_review and fix everything it reports. Look at a sheet again.
4. Call ultrafast_export once. End with one line saying what you made."""


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
        elif x is not None:
            collect(getattr(x, "text", None) or getattr(x, "content", None) or getattr(x, "output", None))

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
    headers = {"User-Agent": "ultrafast-starter-openai-agents"}
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


def explain(e: BaseException) -> str:
    """One line for a failure buried in exception groups (as MCP transports raise them)."""
    leaves, stack = [], [e]
    while stack:
        x = stack.pop()
        subs = getattr(x, "exceptions", None)
        if subs:
            stack.extend(subs)
        elif x.__cause__ is not None or x.__context__ is not None:
            stack.append(x.__cause__ or x.__context__)
            leaves.append(x)
        else:
            leaves.append(x)
    text = "; ".join(dict.fromkeys(f"{type(x).__name__}: {x}" for x in leaves if str(x)))
    if "401" in text or "nauthorized" in text or "error response" in text:
        text += " (check ULTRAFAST_API_KEY and ULTRAFAST_MCP_URL)"
    return text or type(e).__name__


class Progress(RunHooks):
    """Prints each tool call and keeps the last export's link."""

    def __init__(self) -> None:
        self.url: Optional[str] = None

    async def on_tool_start(self, context, agent, tool) -> None:  # type: ignore[no-untyped-def]
        print(f"  {tool.name}", file=sys.stderr, flush=True)

    async def on_tool_end(self, context, agent, tool, result) -> None:  # type: ignore[no-untyped-def]
        if tool.name == "ultrafast_export":
            self.url = download_url(result) or self.url


def server_instructions(ultrafast: MCPServerStreamableHttp) -> str:
    """What the server said at initialize (where it is kept differs between mcp 1.x and 2.x)."""
    for holder in (getattr(ultrafast, "server_initialize_result", None), getattr(ultrafast, "session", None)):
        text = getattr(holder, "instructions", None)
        if isinstance(text, str) and text:
            return text
    return ""


def editor(ultrafast: MCPServerStreamableHttp, model: Any = None) -> Agent:
    """The agent: ultrafast's tools, these instructions and the server's own."""
    return Agent(
        name="Editor",
        mcp_servers=[ultrafast],
        instructions="\n\n".join(x for x in (INSTRUCTIONS, server_instructions(ultrafast)) if x),
        model=model,
    )


async def make_video(ultrafast: MCPServerStreamableHttp, brief: str, *, model: Any = None, max_turns: int = 40) -> Tuple[str, Optional[str]]:
    """Runs the agent on a brief; its closing words and the last export's download link."""
    hooks = Progress()
    result = await Runner.run(editor(ultrafast, model), brief, max_turns=max_turns, hooks=hooks)
    text = str(result.final_output)
    return text, hooks.url or download_url(text)


async def main() -> int:
    load_env()
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("brief", nargs="?", default="A 15 s launch promo", help="what to make")
    p.add_argument("--out", default="film.mp4", help="where to save the MP4 (default film.mp4)")
    p.add_argument("--list-tools", "--dry-run", action="store_true", help="connect, list the MCP tools and stop")
    p.add_argument("--max-turns", type=int, default=40)
    args = p.parse_args()

    key = os.environ.get("ULTRAFAST_API_KEY")
    if not key:
        print("Set ULTRAFAST_API_KEY (see .env.example).", file=sys.stderr)
        return 2
    url = os.environ.get("ULTRAFAST_MCP_URL", "https://mcp.ultrafast.mov")
    model = os.environ.get("OPENAI_MODEL") or None  # None: the Agents SDK's default model

    try:
        return await run(args, key, url, model)
    except Exception as e:  # noqa: BLE001
        print(f"Failed ({url}): {explain(e)}", file=sys.stderr)
        return 1


async def run(args: argparse.Namespace, key: str, url: str, model: Optional[str]) -> int:
    async with MCPServerStreamableHttp(
        name="ultrafast",
        params={
            "url": url,
            "headers": {"Authorization": f"Bearer {key}"},
            "timeout": 30,
            "sse_read_timeout": 900,
        },
        # review and export render the whole film: give tool calls time
        client_session_timeout_seconds=900,
        cache_tools_list=True,
    ) as ultrafast:
        if args.list_tools:
            for tool in await ultrafast.list_tools():
                print(f"{tool.name}: {(tool.description or '').splitlines()[0][:100]}")
            return 0
        print(f"Making: {args.brief}", file=sys.stderr)
        text, link = await make_video(ultrafast, args.brief, model=model, max_turns=args.max_turns)

    print(text)
    if not link:
        print("The agent did not export a video.", file=sys.stderr)
        return 1
    print(link)
    saved = download(link, Path(args.out), key, url)
    print(f"Saved {saved} ({saved.stat().st_size} bytes)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

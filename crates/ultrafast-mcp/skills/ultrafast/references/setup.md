# Setting up ultrafast for an agent

Each command here installs software or changes an agent's settings: show it
to the person, and run it only when they ask you to.

## Install the engine

One binary, `ultrafast`, with `uf` as its short name:

```bash
cargo install ultrafast --version 0.2.2 --locked   # from crates.io
brew install ultrafastmov/tap/ultrafast            # macOS, prebuilt
docker run --gpus all ghcr.io/ultrafastmov/ultrafast:0.2.2 info
```

The cargo and Docker lines pin the release this guide is for; Homebrew
installs the tap's newest. `uf --version` says which one you have.

It renders with Metal on macOS (Apple silicon) and Vulkan on Linux and
Windows (WSL2). `uf info` names the GPU it found. H.264 and HEVC exports
encode on the GPU (Metal, or NVENC on NVIDIA); everything else (ProRes,
AV1, VP9, GIF, PNG and EXR sequences, sound alone) and exports on other
GPUs go through ffmpeg, so install it (`brew install ffmpeg`,
`apt install ffmpeg`).

## Connect a local agent (stdio)

`uf mcp` serves the tools over stdio. Renders run on this machine's GPU and
documents are files on it.

Claude Code:

```bash
claude mcp add ultrafast -- uf mcp
```

Codex:

```bash
codex mcp add ultrafast -- uf mcp
```

`codex exec` has no one to ask, so it refuses the tools that write files
(`ultrafast_edit`, `ultrafast_export`) unless they are approved: add
`default_tools_approval_mode = "approve"` to the `[mcp_servers.ultrafast]`
table in `~/.codex/config.toml`.

Antigravity CLI:

```bash
agy mcp add ultrafast uf mcp
```

Gemini CLI (`--scope user` adds it for every project). Since June 2026
Gemini CLI takes only paid Gemini API keys, Vertex AI and Code Assist
Standard or Enterprise; personal Google accounts use Antigravity CLI:

```bash
gemini mcp add --scope user ultrafast uf mcp
```

Cursor (`.cursor/mcp.json` or `~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "ultrafast": { "command": "uf", "args": ["mcp"] }
  }
}
```

Claude Desktop (`claude_desktop_config.json`: on macOS in
`~/Library/Application Support/Claude/`, on Windows in `%APPDATA%\Claude\`,
on Linux in `~/.config/Claude/`): the same `mcpServers` block. Desktop apps
don't see your shell's `PATH`, so give the full path from `which uf`, e.g.
`"command": "/opt/homebrew/bin/uf"`.

Exports of long films can take longer than a client's default tool timeout.
Raise it if exports time out: in Codex, add `tool_timeout_sec = 300` to the
`[mcp_servers.ultrafast]` table that `codex mcp add` wrote to its
`config.toml`; in Claude Code, set `MCP_TOOL_TIMEOUT` (milliseconds).

## Connect to a server (Streamable HTTP)

A self-hosted server (`ultrafast serve`, or the Docker image) answers MCP at
`/mcp`, e.g. `https://video.example.com/mcp`. Clients send its API key as a
bearer token. Keep the key in the environment (`ULTRAFAST_API_KEY` here),
out of command lines and config files: every client below except
Antigravity CLI reads it from there when it connects.

Claude Code (single quotes keep `${ULTRAFAST_API_KEY}` a reference, which
Claude Code fills in from its environment when it connects):

```bash
claude mcp add --transport http ultrafast https://video.example.com/mcp \
  --header 'Authorization: Bearer ${ULTRAFAST_API_KEY}'
```

Codex:

```bash
codex mcp add ultrafast --url https://video.example.com/mcp \
  --bearer-token-env-var ULTRAFAST_API_KEY
```

Gemini CLI (single quotes keep `$ULTRAFAST_API_KEY` a reference that
Gemini CLI reads from the environment when it connects):

```bash
gemini mcp add --scope user --transport http ultrafast \
  https://video.example.com/mcp \
  --header 'Authorization: Bearer $ULTRAFAST_API_KEY'
```

Cursor:

```json
{ "mcpServers": { "ultrafast": { "url": "https://video.example.com/mcp",
  "headers": { "Authorization": "Bearer ${env:ULTRAFAST_API_KEY}" } } } }
```

Antigravity CLI sends headers exactly as written, without expanding
variables, so the key itself has to be saved in
`~/.gemini/config/mcp_config.json`, a file it leaves readable by every user
of the machine. Add the server to that file in an editor rather than on a
command line, then make the file private:

```json
{ "mcpServers": { "ultrafast": { "serverUrl": "https://video.example.com/mcp",
  "headers": { "Authorization": "Bearer <the key>" } } } }
```

```bash
chmod 600 ~/.gemini/config/mcp_config.json
```

The remote tools take documents inline instead of file paths, and exports
come back as download links. Everything a call sends, the document with its
text and media links, goes to that server.

ultrafast Cloud (`https://mcp.ultrafast.mov`, not open yet) serves the same
tools with OAuth sign-in. That is what Claude's connectors (Settings →
Connectors, on web, desktop and mobile) and the ChatGPT app use.

## Add this skill

Clients that load skills from MCP servers get this skill from the ultrafast
server itself, local or remote, with nothing to add. Everywhere else:

```bash
npx skills add ultrafastmov/ultrafast@v0.2.2
```

It installs for Claude Code, Codex, Cursor, Gemini CLI and other agents.
Claude Code can also install the ultrafast plugin, which bundles this skill
with the `uf mcp` server:

```bash
claude plugin marketplace add ultrafastmov/ultrafast@v0.2.2
claude plugin install ultrafast@ultrafast
```

In Antigravity CLI the repository itself is the plugin, with the same skill
and server:

```bash
agy plugin install https://github.com/ultrafastmov/ultrafast/tree/v0.2.2
```

These pin the release this guide is for, and a pinned install stays on it.
To follow every release instead, leave the version off:
`ultrafastmov/ultrafast`, or `https://github.com/ultrafastmov/ultrafast` for
Antigravity CLI.

# ultrafast over MCP

ultrafast serves agents as Model Context Protocol tools two ways:

- **`uf mcp`** (stdio): on the agent's machine, renders on its GPU, and
  documents are files there. 20 tools.
- **`/mcp` on the render service** (Streamable HTTP, behind the server's API
  key): for agents that can't run a local process (agent frameworks, hosted
  agents, GPU-less sandboxes). Documents travel inline and exports come back
  as download links. 16 tools.

Both speak MCP 2024-11-05 through 2025-11-25 (with `initialize`) and the
stateless 2026-07-28 (`server/discover`, the version in every request's
`_meta`), and both serve the ultrafast skill with their tools
([below](#the-skill-over-mcp)). The code is in `crates/ultrafast-mcp`; the
CLI's stdio server is `crates/ultrafast-cli/src/mcp.rs` and the service
mounts the HTTP handler in `crates/ultrafast-cli/src/serve.rs`.

## Local: `uf mcp`

```bash
claude mcp add ultrafast -- uf mcp                  # Claude Code
codex mcp add ultrafast -- uf mcp                   # Codex
agy mcp add ultrafast uf mcp                        # Antigravity CLI
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.ultrafast]
command = "uf"
args = ["mcp"]
tool_timeout_sec = 300   # exports of long films outlast the 60 s default
default_tools_approval_mode = "approve"   # or codex exec refuses edit and export
```

`codex exec` runs with no one to ask, so it refuses tools that change files
(`ultrafast_edit` and `ultrafast_export` say they do) unless the server's
tools are approved as above; `-c
mcp_servers.ultrafast.default_tools_approval_mode="approve"` does the same for
one run.

Cursor (`.cursor/mcp.json`), Antigravity CLI
(`~/.gemini/config/mcp_config.json`), Gemini CLI (`~/.gemini/settings.json`)
and Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "ultrafast": { "command": "uf", "args": ["mcp"] }
  }
}
```

Desktop apps don't inherit the shell's `PATH`: give Claude Desktop the full
path from `which uf`.

The server keeps a renderer per document (repeated frames are fast) and
reloads a file when it changes, whether the agent edits it through
`ultrafast_edit` or writes it directly. Anything else the process writes to
stdout goes to stderr, so it can't corrupt the protocol stream.

Paths in tool arguments are absolute, or relative to the project folder: the
client's first root. In the `initialize` protocols the server asks for the
roots (`roots/list`) once the session is up; in 2026-07-28 it asks during the
first call that names a relative path (an `input_required` result, which the
client answers by calling again with them). Without roots, relative paths
resolve against the folder the server runs in, unless that is its plugin's
own folder: Antigravity CLI runs a plugin's servers there (and says so in
`PLUGIN_ROOT`), so rather than leave files in the plugin the call fails and
says to use an absolute path. Results name files by absolute path.

| tool | does |
|---|---|
| `ultrafast_ops` | the op vocabulary as compact Markdown, or the full JSON of named ops (`ops: [...]`) |
| `ultrafast_new` | write a template (`vertical`, `title`, `square`, `promo`, `explainer`, `product`) to a path |
| `ultrafast_check` | problems in a document, static and render-time |
| `ultrafast_frame` | one exact frame, returned as an image as the export shows it (black where nothing is drawn; `alpha: true` shows transparency as a checkerboard) |
| `ultrafast_sheet` | a labeled contact sheet of frames, returned as an image |
| `ultrafast_review` | renders the film and reports unreadable, too-brief, clipped, overlapping or tiny text, unsafe areas and seizure-risk flashing |
| `ultrafast_inspect` | what is on screen at a time: each element's node, bounds and words |
| `ultrafast_edit` | apply an edit batch to the document file (atomic) |
| `ultrafast_export` | encode to MP4, MOV/ProRes, MKV, WebM, GIF, PNG or EXR sequences or sound alone, at any size, frame rate and quality level (`format`, `codec`, `quality`, `size`, `fps`, `alpha`, ...) |
| `ultrafast_estimate` | rendered minutes, GPU seconds and time to first frame, before rendering |
| `ultrafast_batch` | one document, a video per row of a data file (CSV, JSON, JSON lines) or inline rows |
| `ultrafast_profile` | GPU ms per render group (which nodes cost what), for slow films |
| `ultrafast_beats` | tempo, beats, downbeats and onsets of a soundtrack |
| `ultrafast_voiceover` | speak a script to WAV with every word's time (macOS voices; eSpeak NG on Linux) |
| `ultrafast_import_svg` | an SVG as an editable component (or added to a document) |
| `ultrafast_decompose` | a finished video as an editable document: plate, vector layers with their motion, gradients, and type read back as text in its matched font |
| `ultrafast_match_font` | a text node's font (family, weight, optical size, axes), size, baseline, tracking and color from a video frame |
| `ultrafast_ocr` | the text in a video frame, with word boxes, and a draft of text nodes (Apple Vision on macOS, Tesseract elsewhere) |
| `ultrafast_measure` | glyph metrics of a line of text |
| `ultrafast_outline` | a line of text as SVG path data |

Frames and sheets come back as PNG, or JPEG when the PNG would pass 1.5 MB
(sheets of busy footage); sheets are kept within 2400 px.

## Remote: Streamable HTTP

`ultrafast serve` answers MCP at `POST /mcp` (and `POST /`), next to the
`/v1` API, with the same keys. Every tool takes the whole document inline
(`document`); media inside it are `asset://` ids from `POST /v1/uploads` or
https URLs. The remote set is the local one without the tools that read or
write local files (`voiceover`, `decompose`, `match_font`, `ocr`):
`ops`, `new`, `check`, `frame`, `sheet`, `review`, `inspect`, `edit`
(returns the edited document; nothing is stored), `export` (renders, stores
and returns a signed download link), `estimate`, `batch` (up to 20 rows a
call, a link per row), `profile`, `beats` (`audio`: an asset id or URL),
`import_svg` (`svg`: the source text), `measure` and `outline`.

Clients send the server's API key as `Authorization: Bearer <key>`; a call
without one gets 401.

```bash
claude mcp add --transport http ultrafast https://video.example.com/mcp \
  --header "Authorization: Bearer $ULTRAFAST_API_KEY"
```

Codex: `url = "..."` and `bearer_token_env_var = "ULTRAFAST_API_KEY"`.
Cursor: `"url"` with `"headers"`. Antigravity CLI: `agy mcp add --header
"Authorization: Bearer $ULTRAFAST_API_KEY" ultrafast <url>` (it writes
`"serverUrl"` with `"headers"` and sends the header as written, without
expanding variables). Gemini CLI: `"httpUrl"` with `"headers"`.
Claude's and ChatGPT's connectors sign in with OAuth rather than a key, so
they use ultrafast Cloud.

ultrafast Cloud (not open yet) will run the same handler at
`https://mcp.ultrafast.mov` and add OAuth sign-in for connectors through
`http::handle_with` and the `http::SignIn` trait (the sign-in itself is
Cloud code, not in this repo).

To try the remote tools without the full service, the crate has a small
development server on this machine's GPU:

```bash
ULTRAFAST_API_KEYS=uf_test_dev cargo run --release -p ultrafastmov-mcp --example dev_server -- 8765
```

## The skill over MCP

Both servers serve the ultrafast skill (`skills/ultrafast`) next to their
tools, so an agent that adds the server has nothing else to install:

- Hosts that load skills from MCP servers (the
  [`io.modelcontextprotocol/skills`](https://github.com/modelcontextprotocol/ext-skills)
  extension: fast-agent, and Claude Code as its rollout reaches accounts,
  where it shows as `ultrafast:ultrafast`) find it with `skills/list`: its
  frontmatter, and every file's SHA-256 digest and size, which they check
  what they read against. They load it when a task calls for it.
- Every other client can read it as resources: `skill://ultrafast/SKILL.md`
  and the files beside it (`resources/list`, `resources/read`). The
  server's instructions end by pointing there, for an agent that doesn't
  have the skill loaded.

The skill is built into the binary (from the MCP crate's copy,
`crates/ultrafast-mcp/skills/ultrafast`, which `tools/sync-plugin-skill.sh`
keeps identical to `skills/ultrafast`), so it always matches the server's
version. `ULTRAFAST_MCP_SKILL=0` turns it off and serves the tools alone. The
plugins and extensions below ship the skill themselves, so their server
configs set it and their hosts don't load the skill twice; set it too if you
installed the skill with `npx skills add` and your client loads skills from
MCP servers.

## Skill and plugins

- `skills/ultrafast/`: the agent skill (the document format, the
  check-look-review-export loop, tested recipes). `npx skills add
  ultrafastmov/ultrafast` installs it for Claude Code, Codex, Cursor, Gemini
  CLI and others.
- `plugins/ultrafast/`: the Claude Code plugin (the skill and `uf mcp`),
  listed by `.claude-plugin/marketplace.json`:
  `claude plugin marketplace add ultrafastmov/ultrafast`, then
  `claude plugin install ultrafast@ultrafast`. Its copies of the skill, the
  license files and SECURITY.md (and the MCP crate's copy of the skill) are
  kept in sync by `tools/sync-plugin-skill.sh` (`--check` in CI).
- `mcp_config.json`: `agy plugin install
  https://github.com/ultrafastmov/ultrafast` adds `uf mcp` and the skill to
  Antigravity CLI. It reads a plugin's servers from this file, not from
  `mcp.json`.
- `gemini-extension.json`: `gemini extensions install
  https://github.com/ultrafastmov/ultrafast` adds `uf mcp` and the skill.
  Since June 2026 Gemini CLI serves only paid Gemini API keys, Vertex AI and
  Code Assist Standard or Enterprise; personal Google accounts moved to
  Antigravity CLI.

## Tested clients

What was run against these servers (November 2026):

| client | transport | result |
|---|---|---|
| Claude Code 2.1.325 | stdio | a model run used new, check, sheet (saw the image), inspect, edit and export |
| Claude Code 2.1.325 | HTTP, bearer key | stateless 2026-07-28 (`server/discover`, then `tools/list` and `tools/call`); new, edit, frame and export with a download link |
| Claude Code 2.1.325 | HTTP, bearer key, `ultrafast serve` | stateless 2026-07-28 through the service's router; new, estimate and export with a signed link |
| MCP Inspector (CLI) | HTTP, `ultrafast serve` | `tools/mcp-every-tool.sh`: all 16 remote tools answered, `beats` on an uploaded `asset://` |
| Claude Code 2.1.325 + skill | stdio | from a one-line request: loaded the skill, then template, ops, check, sheets, two fixes, review (0 findings), export |
| MCP TypeScript SDK 1.32 | HTTP, OAuth (Cloud's sign-in, tested before it moved out of this repo) | 401 → metadata → DCR, and separately CIMD with Claude Code's published client document → sign-in page → PKCE token → every tool |
| Antigravity CLI 1.5.17 + skill | stdio (`agy plugin install`) | installed from GitHub with the skill and the server; `/mcp` listed all 20 tools; from a one-line `agy -p` request (allow rule `mcp(ultrafast_ultrafast/*)`): ops, check, sheet, review, estimate, frame and export, then inspect, a fix and a vertical copy in a second turn |
| Gemini CLI 0.70 | stdio and HTTP (`httpUrl` + header) | connected (`gemini mcp list`); no model run: Gemini CLI refuses personal Google accounts |
| Cursor CLI 2026.11.19 | stdio (`.cursor/mcp.json`) | ready, all 20 tools listed (`agent mcp list-tools`); no model run (no Cursor sign-in) |
| Codex 0.169 + skill | stdio | from a one-line `codex exec` request (`default_tools_approval_mode = "approve"`): ops, check, sheet, review, inspect and export, then a fix and a square copy in a second turn; without the approval line Codex refused edit and export, and the model rendered with FFmpeg instead |
| fast-agent 0.10.67 | stdio and HTTP (`ultrafast serve`) | the skill over MCP: `server/discover`, `skills/list`, then `/skills add ultrafast` read all six files, matched every SHA-256 digest and installed a copy byte-identical to `skills/ultrafast`; with `ULTRAFAST_MCP_SKILL=0` in the server's `env`, no skill was offered |
| Claude Code 2.1.325 | stdio | given only the server (its skills rollout not on for the account): followed the instructions to `skill://ultrafast/SKILL.md`, read it as a resource, then template, edits, check (clean) and a sheet |
| Claude Desktop | stdio | not run |

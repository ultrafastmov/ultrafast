# ultrafast

[![ultrafast: shot on the GPU](https://ultrafast.mov/og/home.jpg)](https://ultrafast.mov)

Shaders, 3D, motion type and camera optics, written by your agent and rendered
on the GPU. Open source.

A video is a JSON document. `ultrafast` checks it, shows exact frames and
contact sheets, reviews the rendered film for what a viewer would trip over
(unreadable, clipped or overlapping text, seizure-risk flashing), and exports
it, on Metal on macOS and Vulkan on Linux, with no browser in the loop. `uf` is
the same command, short.

[ultrafast.mov](https://ultrafast.mov) ·
[Quickstart](https://ultrafast.mov/docs/) ·
[Examples](https://ultrafast.mov/examples/) ·
[Agent guide](https://github.com/ultrafastmov/ultrafast/blob/main/docs/AGENT_GUIDE.md) ·
[GitHub](https://github.com/ultrafastmov/ultrafast)

## Install

```sh
cargo install ultrafast
```

This installs `ultrafast` and `uf`. Slang, the shader compiler, is fetched once
on first use. On Linux you need your GPU's Vulkan driver, ffmpeg for exports,
and espeak-ng and tesseract for `voiceover` and `ocr` (macOS uses its own
voices and Vision).

Prebuilt, with Slang included:

```sh
curl -fsSL https://ultrafast.mov/install.sh | sh   # macOS (Apple silicon), Linux x86_64
brew install ultrafastmov/tap/ultrafast
docker run --gpus all ghcr.io/ultrafastmov/ultrafast:latest info
```

## Try it

```sh
uf new vertical film.json
uf check film.json
uf sheet film.json --count 9 --out sheet.png
uf review film.json
uf export film.json film.mp4
uf export film.json overlay.mov --codec prores --alpha --size 4k
uf export film.json preview.gif --quality draft --size 480p --fps 15
```

Exports: H.264, HEVC and AV1 MP4, ProRes (transparent 4444 included), VP9
WebM with alpha, GIF, PNG and EXR sequences, or sound alone, at any size and
frame rate from the same document.

## With an agent

`uf mcp` is a local MCP server with 20 tools: write a document, look at it,
review it, export it.

```sh
claude mcp add ultrafast -- uf mcp      # Claude Code
codex mcp add ultrafast -- uf mcp       # Codex
```

For `codex exec`, also put `default_tools_approval_mode = "approve"` under
`[mcp_servers.ultrafast]` in `~/.codex/config.toml`: with no one to ask, it
refuses the tools that write files. Cursor, Claude Desktop and other clients
take `{"command": "uf", "args": ["mcp"]}`. Every client and tool is in
[docs/MCP.md](https://github.com/ultrafastmov/ultrafast/blob/main/docs/MCP.md);
the skill is `npx skills add ultrafastmov/ultrafast`.

Listed on the [official MCP Registry](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.ultrafastmov/ultrafast)
as mcp-name: io.github.ultrafastmov/ultrafast

## Docs

- [Agent guide](https://github.com/ultrafastmov/ultrafast/blob/main/docs/AGENT_GUIDE.md): how to work in it (start here)
- [Ops](https://github.com/ultrafastmov/ultrafast/blob/main/docs/OPS.md): every op; `uf ops` prints the same as JSON
- [MCP](https://github.com/ultrafastmov/ultrafast/blob/main/docs/MCP.md): ultrafast as MCP tools, local and remote
- [Self-hosting](https://github.com/ultrafastmov/ultrafast/blob/main/docs/service/SELF_HOST.md): the render service on your GPU (`uf serve`) and its [HTTP API](https://github.com/ultrafastmov/ultrafast/blob/main/docs/service/API.md)
- [Architecture](https://github.com/ultrafastmov/ultrafast/blob/main/docs/ARCHITECTURE.md): how it works

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/ultrafastmov/ultrafast/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/ultrafastmov/ultrafast/blob/main/LICENSE-MIT),
at your option. Bundled fonts are under the SIL Open Font License.

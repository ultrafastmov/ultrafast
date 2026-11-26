# ultrafast for DeepSeek Harness

ultrafast is an open-source video engine for agents. A video is a JSON
document of nodes (footage, type, shapes, captions, charts, 3D, effects),
rendered on your GPU. This plugin registers the ultrafast MCP server
(`uf mcp`) with DeepSeek Harness, so the agent gets ultrafast's 20 tools as
`mcp__ultrafast__*`: start a document from a template, check it, look at
frames and contact sheets, fix what `review` finds, then export MP4, WebM,
ProRes, GIF, image sequences or sound.

The plugin runs `uf mcp`, so the `uf` command has to be on your PATH (or set
`UF_BIN` to its full path). Install ultrafast first, any one of these:

```sh
curl -fsSL https://ultrafast.mov/install.sh | sh   # macOS (Apple silicon), Linux x86_64
brew install ultrafastmov/tap/ultrafast            # Homebrew, prebuilt
cargo install ultrafast                            # from crates.io
```

## Install

From the plugin market, or with the CLI:

```sh
dsh plugin --profile web add "git+https://github.com/ultrafastmov/ultrafast.git#main&path:/plugins/dsh"
```

Then ask for something like: "Make a 6-second 1080p title card that says
Hello, check it, show me a contact sheet, then export an MP4."

Docs: https://ultrafast.mov/docs/ · Source: https://github.com/ultrafastmov/ultrafast

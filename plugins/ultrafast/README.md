# ultrafast for Claude Code

ultrafast is an open-source video engine for agents. A video is a JSON
document of nodes (footage, type, shapes, captions, charts, 3D, effects),
rendered on your GPU at hundreds of frames a second. This plugin gives
Claude Code two things:

- **The ultrafast skill**, which teaches the document format and the loop:
  start from a template, check, look at contact sheets and frames, fix what
  `review` finds (unreadable, clipped or too-brief text, unsafe areas,
  flashing), then export: MP4, ProRes, WebM, GIF, image sequences or sound,
  at any size.
- **The ultrafast MCP server** (`uf mcp`), 20 tools that let Claude write,
  look at, review and export videos without a shell.

The plugin starts the MCP server as `uf mcp`, so it needs the `uf` command
on your PATH. Install ultrafast first, any one of these:

```bash
curl -fsSL https://ultrafast.mov/install.sh | sh   # macOS (Apple silicon), Linux x86_64
brew install ultrafastmov/tap/ultrafast            # Homebrew, macOS and Linux
cargo install ultrafast                            # from crates.io
```

The installer downloads the release archive for your machine from
[GitHub releases](https://github.com/ultrafastmov/ultrafast/releases), checks
its SHA-256 and links `ultrafast` and `uf` into `~/.local/bin`. Check it with
`uf info`. If `uf` isn't found, open a new terminal or add
`~/.local/bin` to your PATH.

Install the plugin:

```bash
claude plugin marketplace add ultrafastmov/ultrafast
claude plugin install ultrafast@ultrafast
```

Then ask for a video, for example: "a 10-second vertical teaser for my
pottery class, Saturdays, first class free".

Documentation: https://ultrafast.mov/docs/mcp/. Licensed Apache-2.0 OR MIT.

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
- **The ultrafast MCP server** (`uf mcp`), 18 tools that let Claude write,
  look at, review and export videos without a shell.

It needs the `uf` command on your PATH:

```bash
cargo install ultrafast                    # or
brew install ultrafastmov/tap/ultrafast    # macOS
```

Install the plugin:

```bash
claude plugin marketplace add ultrafastmov/ultrafast
claude plugin install ultrafast@ultrafast
```

Then ask for a video, for example: "a 10-second vertical teaser for my
pottery class, Saturdays, first class free".

Documentation: https://ultrafast.mov/docs/mcp/. Licensed Apache-2.0 OR MIT.

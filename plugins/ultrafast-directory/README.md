# ultrafast for Claude

ultrafast is an open-source video engine for agents. A video is a JSON
document of nodes (footage, type, shapes, captions, charts, 3D, effects),
rendered on your GPU at hundreds of frames a second. This plugin holds the
ultrafast skill, which teaches Claude the document format and the loop:
start from a template, check, look at contact sheets and frames, fix what
`review` finds (unreadable, clipped or too-brief text, unsafe areas,
flashing), then export: MP4, ProRes, WebM, GIF, image sequences or sound,
at any size.

## What it runs

The skill works through the `uf` command, which you install yourself. It
runs `uf new`, `uf check`, `uf sheet`, `uf render`, `uf review` and
`uf export` in the folder you are working in, and everything renders on
your machine's GPU (Metal on Apple silicon, Vulkan on Linux), with ffmpeg
for video and audio. The plugin itself starts no server, has no hooks and
sends nothing anywhere.

`uf` reads the files your documents name. It goes online only for fonts: a
font family a document names that isn't installed is downloaded once from
Google Fonts (color emoji from Google's Noto Emoji release on GitHub) into
a local cache. `ULTRAFAST_OFFLINE=1` turns that off. A `cargo install`
build that can't find the Slang shader compiler downloads it once from
Slang's GitHub releases and checks its SHA-256; the installer and Homebrew
builds include it.

## Before you use it

Install ultrafast with the installer, Homebrew or cargo, as described at
https://ultrafast.mov/docs/#install, then check that `uf --version` prints a
version. Without `uf`, the skill explains how to install it and doesn't
pretend to render.

For Claude Code there is also a plugin with the local MCP server
(`uf mcp`, 20 tools):

```bash
claude plugin marketplace add ultrafastmov/ultrafast
claude plugin install ultrafast@ultrafast
```

Then ask for a video, for example: "a 10-second vertical teaser for my
pottery class, Saturdays, first class free".

Documentation: https://ultrafast.mov/docs/. Licensed Apache-2.0 OR MIT.

# ultrafast

**[ultrafast.mov](https://ultrafast.mov)** · the GPU video engine for agents

A GPU-native, realtime video composition engine where a video is a live,
editable document, built for agents to drive. Compositions are procedural
node graphs (JSON) that render on the GPU directly: no Chromium, no web
stack. Metal on macOS, Vulkan everywhere else (NVIDIA L4 included).

- [docs/AGENT_GUIDE.md](docs/AGENT_GUIDE.md): how to work in it (start here)
- [docs/OPS.md](docs/OPS.md): every op; `ultrafast ops` prints the same as JSON
- [docs/MCP.md](docs/MCP.md): ultrafast as MCP tools (`ultrafast mcp`)
- [docs/LIVE.md](docs/LIVE.md): the live player, edit protocol, recording
- [docs/service/SELF_HOST.md](docs/service/SELF_HOST.md): the render service on your GPU (`ultrafast serve`); its HTTP API is [docs/service/API.md](docs/service/API.md)
- [sdk/python](sdk/python), [sdk/typescript](sdk/typescript): the SDKs (`pip install ultrafastmov`, `npm i ultrafastmov`); [starters](starters): agents on the Vercel AI SDK, OpenAI Agents SDK, LangGraph and Mastra
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it works
- [docs/LINUX_NVIDIA.md](docs/LINUX_NVIDIA.md), [docs/RECONSTRUCTION.md](docs/RECONSTRUCTION.md)

## What it does

**Compose anything in one graph.** Shapes, gradients, noise, images and
video; shaped text with variable fonts, glyph animators, text on paths and
counters; vector paths with strokes, dashes, trims and morphs; SVG files as
editable vector nodes; Lottie animations as editable paths and keyframes
(closely matching lottie-web, rendered hundreds of frames a second);
agent-written Slang shaders; 3D (extruded and
bevelled type, glTF, primitives) with image-based lighting, shadows, fog,
depth of field and refractive glass; particles and swarms that form type;
fluid (ink, smoke) and feedback simulations (trails, echo tunnels,
reaction-diffusion), exact on export and seekable; masks, 19 blend modes,
warps, blurs; looks such as bloom, grain, grades, halftone, CRT, glitch,
glass panes, lens flares and light leaks; sound mixed into exports, with
params driven by a soundtrack's loudness.

**Made for agents.** Write JSON or call MCP tools. `ultrafast check` finds
mistakes before rendering; `ultrafast review` renders the film and reports text
that is unreadable, clipped, overlapping or on screen too briefly, and
seizure-risk flashing. `block` gives finished titles, lower thirds, quotes,
stats, chapter markers, end cards and charts (bars, lines, donuts that
build themselves from `values`); `code` types highlighted source into a
window; one text node mixes faces and colors
(`"Video is *live*"`, `[300 fps]{highlight=#ff4d6d}`); `stack` lays content out by its
measured size; `captions` animates a transcript; `ultrafast voiceover` speaks a
script with word times; `ultrafast new` starts from a template. Five typefaces
are bundled so documents look the same everywhere.

**Live.** `ultrafast live` plays a document in a player while agents edit it
over a socket; parameter edits apply on the next frame without recompiling,
structural edits compile in the background, and the player outlines what an
edit touched. `ultrafast record` films a document being edited.

**Fast.** The planner fuses chains of ops into one generated kernel per
group, caches what does not change, skips what cannot be seen, and renders
costly looks at reduced resolution. Motion blur samples only what moves in
each frame. Exports render straight into the hardware encoder with no
readbacks, and geometry that holds still (settled type, shapes between keys)
is not drawn again: the example films (1080x1920, motion blur, sound) render
at 130 to over 1000 fps on an M3 Max.

## Install

```bash
cargo install ultrafast                    # from crates.io
brew install ultrafastmov/tap/ultrafast    # macOS (Apple silicon) and Linux, prebuilt
docker run --gpus all ghcr.io/ultrafastmov/ultrafast:latest info
```

Prebuilt archives for macOS (Apple silicon) and Linux x86_64 (glibc 2.28 and
newer) are on the [releases page](https://github.com/ultrafastmov/ultrafast/releases);
each carries the Slang libraries it loads, and the image carries everything.
`cargo install` fetches Slang once, on first use. On Linux you need your
GPU's Vulkan driver, ffmpeg for exports, and espeak-ng and tesseract for
`voiceover` and `ocr` (macOS uses its own voices and Vision). `uf` is the
same command, short.

## Try it

```bash
uf new vertical film.json
uf check film.json
uf sheet film.json --count 9 --out sheet.png
uf live film.json
uf review film.json
uf export film.json film.mp4
```

The films in `examples/films` (each a short Python script that writes the
JSON) show the range: `ink`, `riso`, `glass`, `clay`, `kinetic`, `chrome`,
`signal_film`, `terminal`, `warp`, `pulse`, `assemble`, `flow`, `launch`,
`numbers_film`, `blocks`, `prism`, `ledger` (charts on paper), `ship` (code typing into a live preview).

## Building from source

- Rust (stable; `cargo build --release`)
- [Slang](https://github.com/shader-slang/slang/releases) 2026.19 or newer,
  found in this order: `ULTRAFAST_SLANG_DIR` (the folder with
  `lib/libslang.dylib` or `lib/libslang.so`); a `slang-*` folder beside the
  executable, as the release archives have; `SLANG_DIR` at build time; a copy
  fetched once into the cache (`ULTRAFAST_SLANG_FETCH=0` turns fetching off).
- ffmpeg for exports on the Vulkan backend, recordings, and media off macOS.

Releases: `packaging/` builds the archives, the Homebrew formula and the
image, and `.github/workflows/release.yml` builds, tests and publishes all
of them from a `v*` tag.

Build with `--features vulkan` to run the Vulkan backend on a Mac through
MoltenVK; every GPU test passes on both backends. On Windows with an NVIDIA
GPU, `tools/windows/build.sh` builds and installs ultrafast from WSL2
(docs/WINDOWS.md); Linux / NVIDIA is docs/LINUX_NVIDIA.md.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately as
described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in
ultrafast by you, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.

Bundled fonts are under the SIL Open Font License (see
`examples/assets/fonts/licenses`).

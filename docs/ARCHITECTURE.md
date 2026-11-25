# ultrafast architecture

ultrafast is a GPU-native, realtime video composition engine. A video is a **live
document**: a procedural node graph that an agent edits while it renders. There
is no browser, no DOM, no Chromium. The engine owns the GPU directly.

Primary user: an agent. Every capability is reachable through a typed,
serializable document and a small set of edit operations. A human UI, if one
ever exists, is just another client of the same edit API.

## Goals, in priority order

1. **Anything visual is composable.** 2D, 3D, text, raster, vector paths,
   shaders, masks, video, particles. If a visual can be produced, it can be a
   node or a subgraph of nodes.
2. **Live editing without frame drops.** Parameter edits never recompile
   shaders or rebuild the graph. Structural edits recompile only the affected
   region of the plan.
3. **Extremely fast mixed-media rendering.** Persistent GPU state, zero-copy
   media paths, fused passes, no work for unused nodes.
4. **Correct pixels.** Linear-light compositing, premultiplied alpha, explicit
   color spaces on every image edge.
5. **Reconstruction.** Eventually: given a finished video, an agent produces a
   ultrafast document that re-renders it, every layer editable.

## Stack

| Layer | Choice | Why |
|---|---|---|
| Language | Rust | Native speed, no GC pauses, safe concurrency for the edit/render split, good FFI to Metal/Vulkan/CUDA. |
| GPU, macOS | Metal via `objc2-metal` | Direct access to IOSurface, `CVMetalTextureCache`, VideoToolbox, memoryless attachments. |
| GPU, Linux and Windows | Vulkan via `ash` | Same kernel ABI through push descriptors; Slang emits SPIR-V. Runs on NVIDIA (Cloud Run L4, driver 580; GeForce on Linux, and on Windows in a build made from WSL2), on Mesa's lavapipe (CI runs every GPU test there) and on macOS through MoltenVK (`--features vulkan`). Memory is sub-allocated (gpu-allocator), big textures get dedicated memory, NVENC and NVDEC share buffers with CUDA. |
| Shaders | Slang → MSL + SPIR-V, compiled at runtime by libslang | One source for both backends. Kernels are generated at runtime (fusion, agent shaders), so the compiler is loaded in-process (`ultrafast-shaderc`) with a content-addressed disk cache. |
| Not used | wgpu, WebGPU, Skia-in-browser, Chromium | wgpu hides the native interop needed for zero-copy decode and explicit resource control. |

## Crates

```
crates/
  ultrafast-graph    document model: nodes, params, edges, stable ids, components, timing, edit ops, revisions
  ultrafast-geom     vector geometry and text on the CPU: paths, fonts, shaping, layout, measuring
  ultrafast-ops      op registry and param packing, built-in components, templates, JSON Schema, `check`
  ultrafast-framing  face-following clip camera: face tracks and shot boundaries to a per-frame crop
  ultrafast-gpu      thin GPU layer: Metal and Vulkan backends + copy instrumentation
  ultrafast-shaderc  runtime Slang compiler (MSL / SPIR-V) with disk cache
  ultrafast-media    decode (VideoToolbox, NVDEC, ffmpeg) and encode (VideoToolbox, NVENC); audio, beats, OCR, voices
  ultrafast-render   op library (Slang), expansion, plan compiler, codegen, kernel cache, executor; 3D, particles, review
  ultrafast-live     live sessions (preview window, JSON-lines edit server, snapshots), export, estimate, record
  ultrafast-recon    reconstruction: probe, compare, fit, OCR, plates, layer decomposition
  ultrafast-service  the v1 HTTP render service behind `ultrafast serve`
  ultrafast-mcp      MCP tools: the stdio server (`ultrafast mcp`) and Streamable HTTP at /mcp
  ultrafast-cli      `ultrafast` (and `uf`) binary: every command, from render and export to live, mcp and serve
```

Dependencies only flow downward: `cli → {service, mcp, recon} → live → render →
{ops, geom, framing, media, gpu, shaderc, graph}`. `graph`, `ops`, `geom` and `framing`
have no GPU dependency, so documents can be validated, checked, diffed, edited and laid
out anywhere.

## The document

A document is a set of nodes keyed by stable string ids, plus an output node.

```json
{
  "version": 1,
  "format": { "width": 1920, "height": 1080, "fps": 60, "duration": 5.0 },
  "output": "comp",
  "nodes": {
    "bg":   { "op": "gradient", "params": { "a": [0.05,0.05,0.1,1], "b": [0.2,0.1,0.4,1] } },
    "disc": { "op": "shape",    "params": { "kind": "circle", "center": [960,540], "size": [300,300], "color": [1,0.4,0.1,1] } },
    "comp": { "op": "over",     "inputs": { "fg": "disc", "bg": "bg" } }
  }
}
```

- **Params** are typed values: number, vec2/3/4, color, text, enum, bool or a
  list of numbers. A numeric param can instead change with time: an
  **animation curve** (keyframes with easing, optionally repeating or
  ping-ponging), `wiggle`, `oscillate`, an expression of `t`, or the
  soundtrack's loudness in a band. Any param can be animated without changing
  node type.
- **Inputs** are named image ports referencing other node ids.
- **Components** are reusable subgraphs with their own params and inputs; a
  node whose `op` names one is an instance.
- **Timing**: any node can have a window (`in` / `out`) and a local clock
  (`offset`, `speed`, `loop`). Outside its window a composite passes its
  destination through and an effect its source; a generator goes
  transparent, so a layer outside its window leaves what is under it alone.
- Ids are stable and human-meaningful so an agent can address `disc.center`
  directly.

### Edit operations

All mutation goes through `Edit`, which is serializable and batchable:

`set_param`, `remove_param`, `add_node`, `add_layer` (a node laid over the
current output), `remove_node`, `connect`, `disconnect`, `set_output`,
`set_format`, `set_timing`, `set_component`.

Each applied batch bumps a document **revision** and produces a `Change` set
classified as:

- **Param-only**: the plan is reused as is. New values land in the per-node
  uniform slot on the next frame. No shader or pipeline work.
- **Structural**: the plan is recompiled. Pipelines are cached by
  `(kernel, pixel format, variant)`, so recompiling a plan almost never compiles
  a shader; it re-sequences passes and reassigns pooled textures.

The editor side (agent, socket, file watcher) and the render side communicate
through a lock-free edit queue drained once per frame, so an edit is visible on
the next frame and never waits behind long media queues.

## From graph to frames

```
Document ──plan (on structure change)──▶ Plan: groups of fused nodes
                                           │
             ┌─────────────────────────────┴───────────────────┐
             ▼                                                 ▼
   codegen: one Slang kernel per group        executor (every frame): pack params,
   → background compile → kernel cache        skip unchanged static groups, dispatch
```

### Expansion

Before planning, each new revision of the document is expanded into a flat
graph of the 40 ops in the table below (planning still happens only when the
result's structure changes):

1. Ops that build their content from params become ordinary nodes: `block`
   (title cards, lower thirds, stats, end cards and more, laid out for the
   frame), `code` (highlighted source in a panel), `stack` (children placed
   by their measured bounds), `raymarch`, `transition`, `captions` (from
   word timings), `clip_camera` (face tracks from the footage when it has
   none), and `svg` and `lottie` (files imported once, and again when they
   change). A component holding such ops gets a copy per instance first, so
   they expand wherever it is used.
2. Components, the document's own and the 21 built-in ones (`drop_shadow`,
   `glow`, `grade`, `film_look` and the rest), are flattened into the graph
   with their params filled in; timings chain from the outermost instance to
   each node.
3. Params driven by the soundtrack become keyframes, 60 a second.
4. A `transform` over a composite is copied down onto the composite's layers
   (T(over(a, b)) = over(T(a), T(b)), and likewise for blends, masks and
   color ops), so a camera zooming over a scene keeps its type and vectors
   sharp. Blurs, shaders, footage and other images stop the descent and keep
   the transform over them.

### Ops

Image ops are functions in one Slang library (`crates/ultrafast-render/shaders/lib.slang`)
with a fixed-size param block; 3D and particles draw through graphics pipelines
(`lib3d.slang`, `particles.slang`). Each of the 40 has one of these forms:

| Form | Signature | Ops |
|---|---|---|
| generator | `float4 g_op(Ctx, o)` | solid, gradient, shape, noise, checker |
| resource generator | `float4 g_op(Ctx, o, Texture)` | image, video |
| map | `float4 f_op(Ctx, inputs..., o)` | over, blend (19 modes), composite, mask, mix, adjust, levels, color_matrix, lut, invert, tint, threshold, crop, opacity |
| warp | `Ctx w_op(Ctx, o)` | transform, corner_pin, wave, twirl, tile |
| displace | `map` at ctx, then `source` at the displaced ctx | displace |
| barrier | multi-tap kernel over textures | blur |
| raster | vector geometry → GPU coverage rasterizer | path (SVG data, presets, stroke, dash, trim, morph), text |
| custom | agent-written Slang `float4 shade(float2 pos, float px)` | shader |
| object | scene data, not an image | mesh, light, camera; audio (mixed into exports) |
| 3D | a graphics pipeline over the connected objects | render3d |
| particles | closed-form particle state, drawn as instanced sprites | particles, swarm |
| simulation | stepped once per frame on its own state textures | fluid, feedback |

`Ctx` carries the sample position and the size of one output pixel in that
space, so antialiasing stays one output pixel wide under any warp.

Particles are a closed-form function of (seed, index, time), so any frame
renders in any order. A simulation's frame F is what stepping it once per
frame from the start produces: exports are exact, seeks resume from the
nearest checkpoint and re-simulate the frames in between, and in the live
player an edit keeps the running state, so the simulation reacts instead of
restarting.

### Planning and fusion

1. Walk back from the output; unreachable nodes are never planned
   (dead-node elimination).
2. Schedule heaviest-input-first (Sethi-Ullman) to keep few textures alive.
3. Decide which nodes must be **materialized** as textures: the output;
   nodes with several consumers; blurs, 3D renders, particles, simulations
   and rasters (with the transforms they absorb); the images that blurs,
   custom shaders, meshes, particles and simulations read; and heavy static
   subtrees feeding animated consumers (so they are cached instead of
   re-evaluated every frame). Under motion blur, so is the image every
   moving layer reaches (below). A kernel binds at most 16 textures, so a
   subtree that would bring in more is split at its heaviest child.
4. Every materialized node roots a **group**: itself plus all inlinable
   upstream nodes. Each group is one generated kernel. Warps compose
   analytically: a shape under a transform is evaluated at the transformed
   position, not rasterized and resampled. A stack of N animated layers over a
   background is one dispatch with zero intermediate textures.
5. Texture slots are assigned by lifetime; static group outputs keep dedicated
   slots and are reused across frames until their params or inputs change.

The plan is keyed by a **structure signature**: ops, wiring, kernel variants
(gradient kind, shader code, image source) and which params are animated.
Param values are not part of it, so param edits never replan or recompile.

### Motion blur

`format.motion_blur` (`samples`; `shutter` in frames, 0.5 being a 180°
shutter; optionally `max_samples`) makes each output frame the average of
renders spread over the shutter, centered on the frame time. Only what moves
is sampled:

- Each frame, the renderer finds the plan nodes whose image moves over the
  shutter: params animating to other values, layers appearing or vanishing,
  particles and simulations. Changes that move nothing on screen (colors,
  opacities, grades; a transform or shape moving under 1.5 px) count as
  still, and time-driven content holds at the frame's time: footage and
  image-sequence frames (a camera already blurred them) and shader clocks,
  unless a shader sets `motion_blur: true`.
- Groups that read nothing moving run once, at the frame time. Groups
  holding a moving layer run once per sample, chained to the moving groups
  they read; a group that reads them and holds nothing moving runs once, on
  their average, so each layer blurs on its own, as in a compositor. The
  finishing looks after the image every moving layer reaches (color, blurs
  and glows, shaders, overlays) run once on the blurred result, as on a
  camera's exposure. A frame where nothing moves renders once.
- With `max_samples`, a frame takes one sample per 2 px its fastest layer
  moves (in output texels), never fewer than `samples`, so a whip pan blurs
  smoothly while slow frames stay cheap.
- Particles drawn once smear along their paths over the shutter. A 3D scene
  renders again only after about 1.5 px of motion; samples in between reuse
  its last render. Simulations step on the first sample.
- The live player turns motion blur off while it plays or scrubs (one render
  a frame) and back on when it stops, so a paused frame shows what the
  export will. Export quality levels set the sample count (see Media).

### Vector paths and text

Paths and text are flattened on the CPU (kurbo; tolerance 0.1 px) into line
segments and rasterized on the GPU with **exact area coverage** (the font-rs
accumulation algorithm): one thread per segment deposits signed area into a
fixed-point accumulator with integer atomics, then one threadgroup per row
prefix-sums it into coverage. Fill rules: nonzero, even-odd.

- Text (`ultrafast-geom`): fonts by family from the system, from the faces
  bundled into ultrafast (Geist, Geist Mono, Instrument Serif, Fraunces,
  Unbounded), from CSS's generic families mapped to faces the machine has,
  or from Google Fonts, fetched once into a per-user cache
  (`ULTRAFAST_OFFLINE=1` turns fetching off); or a font file by path.
  rustybuzz shaping (kerning, ligatures, features, variable axes), stand-in
  faces for scripts a font lacks, right-to-left text, color emoji, Unicode
  line breaking, alignment, inline markup (`*emphasis*`,
  `[words]{color=… weight=…}`), per-glyph reveal with offset (typewriter /
  fade-up). Per-glyph opacity works because the accumulator is linear in
  each contour's weight.
- Caches: shaped lines (by font, text, features), glyph outlines (by face,
  glyph and variation instance) and color glyph bitmaps are LRU caches
  bounded by entry count. Built geometry is kept per node, keyed by a hash
  of params, absorbed transforms and time; geometry that only moves, turns
  or shrinks is mapped instead of built again. Color glyphs (emoji bitmaps)
  share one atlas texture, started over when full. Static text costs
  nothing per frame.
- `transform` nodes stacked directly on a path or text are absorbed into the
  geometry, so scaled-up text stays sharp instead of being resampled
  (transforms over composites are pushed down onto their layers first; see
  Expansion).
- Raster outputs are **placed textures** sized to the geometry bounds (in
  64 px buckets), not frame-sized. Every kernel input carries a frame origin,
  so consumers sample them transparently.

### Bounds

Every group tracks where its output can be non-transparent (computed on the
CPU each frame from params: shapes, images, crops, masks, transforms,
rasters). Two things use it:

- **Guards**: `over`/`blend` in a fused kernel evaluate their fg subtree only
  inside its bounds; elsewhere bg passes through. A 120-layer animated stack
  costs per pixel only what overlaps that pixel.
- **Regions**: blurs process only their source's bounds plus 3 sigma, at
  1/2..1/8 resolution for large radii (band-limited anyway), into placed
  textures. An animated glow costs its area, not the frame.

### Kernel tiers

- **Tier 0**: one kernel per op plus utility kernels, generated from the same
  library. The live player and the render service compile them all at
  startup; a command that renders and exits (an export, a frame) compiles
  only the utility kernels up front, and an op's kernel when something
  first needs it.
- **Fused**: one kernel per group, keyed by the hash of its generated code and
  compiled on a background thread. Identical structures share kernels across
  plans, documents and runs (disk cache).
- In **live** mode, a group whose fused kernel is still compiling runs as a
  chain of tier-0 kernels for those frames, so structural edits never stall.
  In **exact** mode (export) the renderer waits, so output is deterministic.
  (`Renderer::set_wait_for_fused(false)` runs exact renders op by op
  meanwhile too: the same image to within half-float rounding; groups with
  a custom shader still wait.) `ultrafast warm` compiles and caches every
  kernel and pipeline a set of documents needs ahead of time.
- A kernel that fails to compile (for example an agent shader with a typo)
  renders transparent and surfaces a per-node diagnostic; the rest of the
  composition keeps rendering.

## Resources and lifetimes

- **Texture pool**: keyed by `(width, height, format, usage)`. Intermediates are
  leased per frame and returned; the pool is bounded and trims LRU entries.
- **Pipeline cache**: keyed by kernel id + format; built once per process.
  On Vulkan each pipeline the driver compiles is stored in a file of its own
  under `$ULTRAFAST_CACHE_DIR/pipelines/<driver UUID>` (else the user's cache
  directory), so later processes load it instead of compiling it, read only
  what they use, and never merge concurrent writes.
  `$ULTRAFAST_SHARED_CACHE_DIR` adds a directory that machines with the same
  driver share (read on a local miss, written after a compile), and
  `ULTRAFAST_PIPELINE_CACHE=0` turns caching off. Metal relies on the
  system's shader cache.
- **Warm renderers**: `Renderer::release_document` drops a document's plan,
  slots, raster textures, decoders, images, simulations and built parts and
  keeps kernels, pipelines and fonts, so a service rendering one document
  after another returns to its baseline memory between them.
- **Param arena**: one shared buffer per frame in flight (3), bump-allocated
  per dispatch, continuing in an overflow buffer when a frame uploads more
  (zooming type under motion blur). No per-node allocations; frames in flight
  never stall unless the GPU is 3 frames behind.
- **Group slots**: a plan can name thousands of frame-sized outputs (the
  flagship: 2,089, 35 GB at 1080p half float). A slot gets a texture the
  first time a live group needs it; slots and kept raster textures share a
  budget (a third of GPU memory, split between the renderers of a parallel
  export; `ULTRAFAST_SLOT_BUDGET_MB` overrides it), and the ones no frame
  needed lately are given back first, dropping their groups' caches.
  The pool's free lists are capped in bytes. Raster textures are reused
  while geometry fits and grow with headroom, so zooming type does not
  allocate per frame. `ULTRAFAST_DEBUG_MEM=N` prints where memory goes every N
  frames.
- **Text and geometry caches**: see Vector paths and text. Outlines are
  rasterized at the size they are drawn, and the raster kept while its
  geometry and paint stay the same, rather than kept as atlas pages; only
  color glyph bitmaps live in an atlas.
- **Explicit lifetimes**: GPU objects are owned by caches or pools, never by
  nodes. Removing a node releases nothing immediately; the pool trims on its own
  schedule, so edits never stall on deallocation.

## Copy instrumentation

Every CPU↔GPU transfer goes through `ultrafast-gpu`, which counts calls and
bytes per direction (uploads, readbacks). The param bytes bound with each
dispatch, a frame's expected cost, are counted apart, as are textures
created, dispatches and barriers. `ultrafast bench` prints the transfers for
its run (params per frame), and an export reports its readbacks. The target
for steady-state playback of a GPU-resident composition is **zero readbacks
and zero uploads beyond uniforms**.

## Color, alpha, blending

- Working space: **linear-light, premultiplied RGBA16F**, Rec.709/sRGB
  primaries. There is no wide-gamut (Display P3, Rec.2020) working space.
- Images and video are 8-bit and read as sRGB-encoded (video's Y'CbCr is
  converted to RGB first; see Media). Each image, and each new video frame,
  is linearized and premultiplied exactly once, by one pass (`k_prep_srgb8`,
  `k_prep_video`). HDR video (PQ, HLG) is tone mapped to SDR BT.709 as it
  decodes through ffmpeg. 3D LUTs are data, kept as RGBA16F, and say which
  encoding they expect (`space`).
- Colors in documents are **straight (unpremultiplied) sRGB-encoded** for
  ergonomics; the engine linearizes and premultiplies on ingest.
- Blend modes operate on premultiplied linear values. Porter-Duff ops are exact;
  separable/non-separable blend modes follow the W3C compositing formulas
  applied in linear space. `over` and `blend` take `space: srgb` to composite
  the encoded values instead, as browsers and design tools do; gradients
  interpolate, and scaled images and video filter, in either space too.
- The final pass encodes once: sRGB-encoded straight alpha for what is read
  back for ffmpeg (flattened over black when the output is opaque), linear
  for EXR, and for NVENC BT.709 Y'CbCr computed from the sRGB-encoded floats
  with no 8-bit RGB step. VideoToolbox takes sRGB-encoded BGRA and converts
  it itself.

## Media

- **macOS** (`ultrafast-media`): AVFoundation/VideoToolbox hardware decode into
  IOSurface-backed `CVPixelBuffer`s, mapped as Metal textures through
  `CVMetalTextureCache` (zero copies); one pass per new source frame converts
  to the working space. Export renders each frame on the GPU straight into
  the hardware encoder's pool buffers (H.264 / HEVC, tagged BT.709): zero
  readbacks, ~195 fps at 1080p on M3 Max (encoder-bound).
- Each `video` node owns a decoder thread with a bounded window (4 frames)
  around its playhead. Seeks, time-remap jumps and edits that move the
  source time discard the window rather than draining it, so edits never
  wait.
- **Linux / NVIDIA decode** (`nvdec.rs`): H.264 in MP4 (8-bit 4:2:0, SDR,
  even sizes, not fragmented) is demuxed in process and decoded by NVDEC
  into NV12 surfaces; each displayed picture is copied device to device into
  a Vulkan buffer that CUDA shares, so no frame crosses PCIe or touches the
  CPU. libnvcuvid loads at runtime. `ULTRAFAST_NVDEC=0` turns it off, and
  `ULTRAFAST_DEBUG_MEDIA=1` prints why a file decodes with ffmpeg instead.
- **Other decodes** (other codecs and containers, 10-bit, HDR; Windows, and
  macOS on Vulkan): an `ffmpeg` process per source, its frames read straight
  into mapped GPU buffers and counted as uploads: 8-bit 4:2:0 as planar
  Y'CbCr, other formats as BGRA converted by ffmpeg with the file's matrix
  and range. PQ and HLG are tone mapped to SDR BT.709 on the way (ffmpeg's
  zscale; without it ultrafast warns that colors will look flat).
- Colors follow the file's tags. On macOS VideoToolbox converts Y'CbCr;
  elsewhere `k_prep_video` does, once per new source frame: NV12 or planar
  4:2:0 with the tagged matrix (untagged: BT.709 for HD sizes, BT.601
  below), video or full range, and chroma sited as H.264's default places
  it, interpolated bilinearly.
- **Linux and Windows / NVIDIA export**: the last kernel (`k_encode_nv12`,
  BT.709 limited range from float) writes each frame into an NV12 buffer
  whose Vulkan memory is exported (an opaque fd; an NT handle on Windows)
  and imported into CUDA; NVENC registers that pointer and encodes it in
  place. A ring of these buffers, covering the frames in flight, queued and
  held by the encoder, keeps the GPU rendering ahead of the encoder; zero
  readbacks. Each frame is encoded in strips, one per encoder engine the
  GPU has (up to three: 0.7 ms for a 1080x1920 frame at P4 on an RTX 5090,
  against 1.7 ms whole). HEVC has three B-frames; H.264 has none, because
  NVENC's H.264 B-frames leave blocks stale where motion stops (a block an
  in-between frame drew wrong is never coded again until the next key
  frame), and its key frames are coded 3 QP finer than the rate control
  picks, as libx264 weighs them. Frames come out in decode order, each naming the frame it
  holds; a small MPEG-TS writer (`ts.rs`) gives each its presentation and
  decoding times, and `ffmpeg` only muxes that stream (MP4, MOV or
  Matroska, with composition offsets) and the AAC soundtrack. Runs of a
  parallel export have no B-frames, so ffmpeg's concat demuxer joins them
  as they are. Bindings come from FFmpeg's MIT
  nv-codec-headers; libcuda and libnvidia-encode load at runtime and the
  readback path remains as a fallback (`ULTRAFAST_NVENC=0`). The Docker image
  (packaging/docker) carries the Vulkan manifest NVIDIA containers need.
- **Other formats** (`ultrafast-live`: `output` resolves the settings,
  `ffmpeg` builds the command lines, `export` routes): H.264 and HEVC
  without alpha take the zero-readback paths above, NVENC at constant
  quality capped at the quality level's bits per pixel. A CRF means
  libx264's scale (libx265's for HEVC) on every encoder: NVENC's own
  constant quality is finer at the same number, and measured with VMAF on
  twelve films its H.264 at CQ 27 (no B-frames, key frames 3 QP finer)
  matches or passes libx264's CRF 18 and HEVC's CQ 28 libx265's CRF 23, so
  NVENC gets the CRF plus 9 (H.264) or 5 (HEVC).
  Quality levels pick NVENC presets P1, P4, P5 and P7; key frames come every
  250 frames, as libx264 places them. Everything else
  (ProRes, AV1, VP9, alpha, GIF, PNG and EXR sequences, and H.264 / HEVC
  where those hardware encoders don't open) is read back and piped to
  `ffmpeg` on every platform: sRGB RGBA8, or half floats for
  10-bit ProRes (sRGB) and EXR (linear, straight alpha that ffmpeg's
  `premultiply` filter premultiplies, as EXR stores it). Opaque outputs are flattened over black in the
  encode kernel, as the hardware paths do. Sound is mixed to a 32-bit float
  WAV and encoded by ffmpeg; sound alone renders no frames. Another output
  frame rate samples the document's seconds at that rate; simulations and
  the motion-blur shutter keep the document's own clock. Quality levels set
  the motion-blur samples on a fork of the document (the shutter never
  changes).
- **Output sizes** (`Renderer::set_output_size`): another size changes the
  pixel density, not the document. Positions stay in the document's frame
  pixels; slots, rasters and intermediates are sized for the view, and the
  kernel header's texel size (`P[1]`, frame px per texel) carries the
  density, so text, paths, blurs, particles and 3D are drawn for the size
  (a 4K export of a 1080p document is not an upscale). Placed textures carry
  their texel size; a layer is loaded texel for texel when its texels are
  the output's and sampled otherwise. Simulations keep the document's grids.
  Every change is the identity at the document's own size.

## Rendering roadmap

1. ✅ Workspace, Metal device, offscreen render, PNG out.
2. ✅ Animation curves, param arena, bench with copy counters.
3. ✅ Slang pipeline, codegen, operation fusion, tiered async kernels.
4. ✅ Transforms, warps, masks, 19 blend modes, blur, color ops, noise,
   images, agent-written shaders.
5. ✅ Vector paths and text: GPU exact-coverage rasterizer, shaping, layout.
6. ✅ Live: preview window (CAMetalLayer, sRGB-tagged, compute-written
   drawables; on Windows a Vulkan swapchain written the same way; Linux runs
   headless) with a timeline player and sound + JSON-lines edit server +
   file watching + contact sheets + `record` (a script of edits played and
   recorded).
7. ✅ Video: VideoToolbox decode/encode, zero-copy, frame-accurate seeking.
8. ✅ 3D: meshes (primitives, glTF, extruded text and SVG paths with
   bevels), lights, cameras as scene nodes; a `render3d` node renders them
   with a graphics pipeline (vertex pulling, depth, 4x MSAA resolved
   in-pass; memoryless attachments on Apple silicon) and GGX shading with
   hemisphere ambient, a shadow map from the first directional light,
   image-based lighting from any ultrafast image as an equirectangular
   environment, glass (refraction, dispersion), fog and depth of field. Any
   ultrafast image can texture a mesh. Metal and Vulkan renders are
   identical. `raymarch` adds signed-distance scenes marched per pixel.
9. ✅ Time: components (precomps) with params and inputs, flattened before
   planning; per-node timing (in/out windows with layer pass-through, local
   time offset/speed/loop) composed through nesting; video time remap;
   motion blur that samples only moving layers, adaptive to their speed
   (see Motion blur).
10. ✅ Vulkan backend on NVIDIA: renders on a Cloud Run L4 with
    zero-readback NVENC export and NVDEC decode; `export --workers N` runs
    render threads with a GPU queue each (flagship: 23 fps on the L4, 20 on
    an M3 Max; a 12 s talking-head short in about 1 s warm). The same
    backend runs on Windows (NVENC, ffmpeg decode).
11. ✅ Reconstruction loop: `probe` (format, shots, contact sheet),
    `extract`, `compare` (PSNR/SSIM, diff images), `fit` (Nelder-Mead over
    static params), `fit-motion` (keyframes with fitted eases), `ocr` and
    `match-font` (text layers in matched fonts), `plate` (background
    plates), `matte` (subject mattes, macOS), `decompose` (motion graphics
    into a still plate and vector layers) and `reconstruct` (the whole loop
    in one command). See RECONSTRUCTION.md. Next: plates for moving
    cameras, overlays removed from footage, motion fitted automatically for
    moving layers.

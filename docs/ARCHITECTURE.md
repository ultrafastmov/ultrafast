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
| GPU, macOS | Metal via `objc2-metal` | Direct access to IOSurface, `CVMetalTextureCache`, VideoToolbox, heaps, argument buffers. |
| GPU, Linux | Vulkan via `ash` | Same kernel ABI through push descriptors; Slang emits SPIR-V. Runs on NVIDIA (Cloud Run L4, driver 580) and on macOS through MoltenVK (`--features vulkan`). Memory is sub-allocated (gpu-allocator), big textures get dedicated memory, NVENC reads exported buffers through CUDA. |
| Shaders | Slang → MSL + SPIR-V, compiled at runtime by libslang | One source for both backends. Kernels are generated at runtime (fusion, agent shaders), so the compiler is loaded in-process (`ultrafast-shaderc`) with a content-addressed disk cache. |
| Not used | wgpu, WebGPU, Skia-in-browser, Chromium | wgpu hides the native interop needed for zero-copy decode and explicit resource control. |

## Crates

```
crates/
  ultrafast-graph   document model: nodes, params, edges, stable ids, edit ops, revisions
  ultrafast-gpu     thin GPU layer + backends (metal today, vulkan next) + copy instrumentation
  ultrafast-shaderc runtime Slang compiler (MSL / SPIR-V) with disk cache
  ultrafast-render  op library (Slang), plan compiler, codegen, kernel cache, executor
  ultrafast-live    live sessions: preview window, JSON-lines edit server, snapshots
  ultrafast-cli     `ultrafast` (and `uf`) binary: render, bench, live, send, apply edits, inspect plans
```

Dependencies only flow downward: `cli → render → {graph, gpu, shaderc}`. `graph` has no
GPU dependency, so documents can be validated, diffed and edited anywhere.

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

- **Params** are typed values: scalar, vec2/3/4, color, string, enum, or an
  **animation curve** (keyframes with easing) evaluated at time `t`. Any param
  can be animated without changing node type.
- **Inputs** are named image ports referencing other node ids.
- Ids are stable and human-meaningful so an agent can address `disc.center`
  directly.

### Edit operations

All mutation goes through `Edit`, which is serializable and batchable:

`set_param`, `add_node`, `remove_node`, `connect`, `disconnect`,
`set_output`, `set_format`.

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

### Ops

Every op is a function in one Slang library (`crates/ultrafast-render/shaders/lib.slang`)
with a fixed-size param block, in one of these forms:

| Form | Signature | Examples |
|---|---|---|
| generator | `float4 g_op(Ctx, o)` | solid, gradient, shape, noise, checker |
| resource generator | `float4 g_op(Ctx, o, Texture)` | image |
| map | `float4 f_op(Ctx, inputs..., o)` | over, blend (19 modes), composite, mask, mix, adjust, levels, color_matrix, invert, tint, threshold, crop, opacity |
| warp | `Ctx w_op(Ctx, o)` | transform, corner_pin, wave, twirl, tile, displace |
| barrier | multi-tap kernel over textures | blur |
| raster | vector geometry → GPU coverage rasterizer | path (SVG data, presets, stroke, dash, trim), text |
| custom | agent-written Slang `shade(pos, px)` | shader |

`Ctx` carries the sample position and the size of one output pixel in that
space, so antialiasing stays one output pixel wide under any warp.

### Planning and fusion

1. Walk back from the output; unreachable nodes are never planned
   (dead-node elimination).
2. Schedule heaviest-input-first (Sethi-Ullman) to keep few textures alive.
3. Decide which nodes must be **materialized** as textures: the output, nodes
   with several consumers, inputs of barriers and custom shaders, and heavy
   static subtrees feeding animated consumers (so they are cached instead of
   re-evaluated every frame).
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

### Vector paths and text

Paths and text are flattened on the CPU (kurbo; tolerance 0.1 px) into line
segments and rasterized on the GPU with **exact area coverage** (the font-rs
accumulation algorithm): one thread per segment deposits signed area into a
fixed-point accumulator with integer atomics, then one threadgroup per row
prefix-sums it into coverage. Fill rules: nonzero, even-odd.

- Text: fontdb (system fonts or files), rustybuzz shaping (kerning,
  ligatures, features), Unicode line breaking, alignment, per-glyph reveal
  with offset (typewriter / fade-up). Per-glyph opacity works because the
  accumulator is linear in each contour's weight.
- Caches: shaped lines by (font, text, features); glyph outlines by
  (font, glyph); flattened geometry by a hash of params, absorbed transforms
  and time. Static text costs nothing per frame.
- `transform` nodes stacked directly on a path or text are absorbed into the
  geometry, so scaled-up text stays sharp instead of being resampled.
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
  library and compiled once at startup.
- **Fused**: one kernel per group, keyed by the hash of its generated code and
  compiled on a background thread. Identical structures share kernels across
  plans, documents and runs (disk cache).
- In **live** mode, a group whose fused kernel is still compiling runs as a
  chain of tier-0 kernels for those frames, so structural edits never stall.
  In **exact** mode (export) the renderer waits, so output is deterministic.
- A kernel that fails to compile (for example an agent shader with a typo)
  renders transparent and surfaces a per-node diagnostic; the rest of the
  composition keeps rendering.

## Resources and lifetimes

- **Texture pool**: keyed by `(width, height, format, usage)`. Intermediates are
  leased per frame and returned; the pool is bounded and trims LRU entries.
- **Pipeline cache**: keyed by kernel id + format; built once per process.
  On Vulkan the driver's compiled pipelines persist across processes in a
  `VkPipelineCache` saved under `$ULTRAFAST_CACHE_DIR/pipelines` (per driver
  UUID) whenever new pipelines were created; Metal relies on the system's
  shader cache.
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
  budget (a third of GPU memory, `ULTRAFAST_SLOT_BUDGET_MB`), and the ones no
  frame needed lately are given back first, dropping their groups' caches.
  The pool's free lists are capped in bytes. Raster textures are reused
  while geometry fits and grow with headroom, so zooming type does not
  allocate per frame. `ULTRAFAST_DEBUG_MEM=N` prints where memory goes every N
  frames.
- **Glyph / shape / geometry caches** (planned): shaped runs keyed by
  `(font, size, text, features)`, glyph atlas pages (SDF/MSDF for scale
  independence, raster for small sizes), tessellated paths keyed by path hash.
  All bounded LRU with explicit byte budgets.
- **Explicit lifetimes**: GPU objects are owned by caches or pools, never by
  nodes. Removing a node releases nothing immediately; the pool trims on its own
  schedule, so edits never stall on deallocation.

## Copy instrumentation

Every CPU↔GPU transfer goes through `ultrafast-gpu`, which counts bytes and calls
per direction (`upload`, `readback`) and per reason tag. `ultrafast bench` prints
these per frame. The target for steady-state playback of a GPU-resident
composition is **zero readbacks and zero uploads beyond uniforms**.

## Color, alpha, blending

- Working space: **linear-light, premultiplied RGBA16F**, Rec.709/sRGB primaries
  by default. Wider gamuts (Display P3, Rec.2020) via an explicit working-space
  setting.
- Sources declare their encoding (sRGB, Rec.709, PQ, HLG, linear). Decode to
  linear happens at the source node, exactly once.
- Colors in documents are **straight (unpremultiplied) sRGB-encoded** for
  ergonomics; the engine linearizes and premultiplies on ingest.
- Blend modes operate on premultiplied linear values. Porter-Duff ops are exact;
  separable/non-separable blend modes follow the W3C compositing formulas
  applied in linear space.
- Output encodes to the target transfer function in the final pass.

## Media

- **macOS** (`ultrafast-media`): AVFoundation/VideoToolbox hardware decode into
  IOSurface-backed `CVPixelBuffer`s, mapped as Metal textures through
  `CVMetalTextureCache` (zero copies); one pass per new source frame converts
  to the working space. Export renders each frame on the GPU straight into
  the hardware encoder's pool buffers (H.264 / HEVC, tagged BT.709): zero
  readbacks, ~195 fps at 1080p on M3 Max (encoder-bound).
- Each `video` node owns a decoder thread with a bounded window (4 frames)
  around its playhead; seeks and time-remap jumps discard the window.
- Colors follow the file's tags (VideoToolbox converts Y'CbCr). Explicit NV12
  handling with selectable matrix and chroma siting is planned (needed for
  NVDEC anyway).
- **Linux / NVIDIA export**: the last kernel (`k_encode_nv12`, BT.709
  limited range from float) writes each frame into an NV12 buffer whose
  Vulkan memory is exported as an opaque fd and imported into CUDA; NVENC
  registers that pointer and encodes it in place. Three buffers keep the GPU
  rendering ahead of the encoder; zero readbacks. `ffmpeg` only muxes the
  stream and the AAC soundtrack. Bindings come from FFmpeg's MIT
  nv-codec-headers; libcuda and libnvidia-encode load at runtime and the
  readback path remains as a fallback (`ULTRAFAST_NVENC=0`). The Docker image
  (packaging/docker) carries the Vulkan manifest NVIDIA containers need.
- **Other formats** (`ultrafast-live`: `output` resolves the settings,
  `ffmpeg` builds the command lines, `export` routes): H.264 and HEVC
  without alpha take the zero-readback paths above, NVENC at constant
  quality capped at the quality level's bits per pixel. Everything else
  (ProRes, AV1, VP9, alpha, GIF, PNG and EXR sequences) is read back and
  piped to `ffmpeg` on every platform: sRGB RGBA8, or half floats for
  10-bit ProRes (sRGB) and EXR (linear, straight alpha that the EXR
  encoder premultiplies). Opaque outputs are flattened over black in the
  encode kernel, as the hardware paths do. Sound is mixed to a 32-bit float
  WAV and encoded by ffmpeg; sound alone renders no frames. Another output
  frame rate samples the document's seconds at that rate; simulations and
  the motion-blur shutter keep the document's own clock. Quality levels set
  the motion-blur samples on a fork of the document (the shutter never
  changes).
- **Linux decode**: still `ffmpeg` processes with uploads. Next: NVDEC into
  CUDA memory exported to Vulkan.
- Decoders run ahead in a **bounded** window (a few frames). Seeking or editing
  discards the window rather than draining it, so edits never wait.

## Rendering roadmap

1. ✅ Workspace, Metal device, offscreen render, PNG out.
2. ✅ Animation curves, param arena, bench with copy counters.
3. ✅ Slang pipeline, codegen, operation fusion, tiered async kernels.
4. ✅ Transforms, warps, masks, 19 blend modes, blur, color ops, noise,
   images, agent-written shaders.
5. ✅ Vector paths and text: GPU exact-coverage rasterizer, shaping, layout.
6. ✅ Live: preview window (CAMetalLayer, sRGB-tagged, compute-written
   drawables) + JSON-lines edit server + file watching + contact sheets.
7. ✅ Video: VideoToolbox decode/encode, zero-copy, frame-accurate seeking.
8. ✅ 3D: meshes (primitives, glTF), lights, cameras as scene nodes; a
   `render3d` node renders them with a graphics pipeline (vertex pulling,
   depth, 4x MSAA resolved in-pass; memoryless attachments on Apple silicon)
   and GGX shading with hemisphere ambient. Any ultrafast image can texture a
   mesh. Metal and Vulkan renders are identical. Next: shadow maps, 3D text.
9. ✅ Time: components (precomps) with params and inputs, flattened before
   planning; per-node timing (in/out windows with layer pass-through, local
   time offset/speed/loop) composed through nesting; video time remap.
   Motion blur is next.
10. ✅ Vulkan backend on NVIDIA: renders on a Cloud Run L4 with
    zero-readback NVENC export; `export --workers N` runs render threads
    with a GPU queue each (flagship: 23 fps on the L4, 20 on an M3 Max;
    a 12 s talking-head short in about 1 s warm). Next: NVDEC decode interop.
11. ✅ Reconstruction loop: `probe` (format, shots, contact sheet),
    `extract`, `compare` (PSNR/SSIM, diff images), `fit` (Nelder-Mead over
    static params). See RECONSTRUCTION.md. Next: OCR-seeded text layers,
    subject mattes, keyframe fitting.

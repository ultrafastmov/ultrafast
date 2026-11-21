# Working in ultrafast as an agent

ultrafast is built for you to drive. A video is a JSON document you can read,
diff and edit; every command answers in JSON or in files you can look at;
a running session takes edits and shows them on the next frame.

Agents with MCP can skip the shell: `ultrafast mcp` serves all of this as tools
that return frames and contact sheets as images ([MCP.md](MCP.md)).

## The loop

1. **Start**: `ultrafast new vertical film.json` (or `title`, `square`, `promo`,
   `explainer`, `product`) writes a template that already looks good (`promo`
   and `explainer` are blocks joined by transitions: change the words and go;
   `product` is a raymarched product shot with copy beside it: change the
   shape in its `scene` and the words), or copy one of `examples/films/*.json`.
2. **Learn the vocabulary**: `ultrafast ops` (JSON: every op, its ports, params,
   types, defaults; built-in looks included). Human-readable: [OPS.md](OPS.md).
3. **Check**: `ultrafast check film.json` reports unknown ops, misspelled params
   (with suggestions), wrongly shaped values, broken shaders and missing files.
4. **Look**: `ultrafast sheet film.json --count 10` renders a labeled contact
   sheet: the fastest way to review motion in one image. `ultrafast render
   film.json --t 2.5 --out f.png` for one exact frame. `ultrafast inspect
   film.json --at 2.5` lists what is on screen then (the node to edit,
   its bounds, its words): the way to find what to change at a moment.
5. **Iterate live**: `ultrafast live film.json` opens the player and listens on
   a socket; send edits (`ultrafast send '{"cmd":"edit", ...}'`, see
   [LIVE.md](LIVE.md)). Param edits apply on the next frame with no
   recompilation; responses list problems on the nodes you touched.
   `{"cmd":"player"}` returns a PNG of exactly what the player shows.
6. **Review**: `ultrafast review film.json` renders the whole film (a few
   seconds) and lists what a viewer would trip over, measured on the final
   frames: text that is never readable or dips below readable contrast,
   is on screen too briefly for its word count, runs off the frame, sits in
   the safe margin (`--safe social` for the caption and button areas of
   vertical apps), collides with other text or is too small; and flashing
   faster than 3 per second. Fix what it reports, then:
7. **Export**: `ultrafast export film.json out.mp4` (hardware encode, audio
   mixed, motion blur if the format asks for it).

## Making it look good

- **Transitions**: chain shots with `transition` nodes (crossfade, dip,
  wipe, push, slide, iris, zoom, dissolve, glitch, blur) instead of hand-made
  windows; each shot renders only while it is on screen.
- **Blocks**: for titles, lower thirds, quotes, stats, chapter markers and
  end cards, start from a `block` node (one node each, placed with a timing
  `offset`) and restyle it (`examples/films/blocks.py`). Data goes in
  `bar_chart`, `line_chart` and `donut` blocks: give `values` and
  `labels`, and the chart builds itself on screen. Code goes in a `code`
  node (`language`, `theme`, `type_speed` to type it on, `highlight` lines).
- **Layout**: put a headline, kicker, body and artwork in a `stack`
  (column or row, gap, align, anchor) instead of computing positions; it
  measures what it holds.
- **Type**: use real fonts (family names, or `.ttf`/`.otf` paths relative to
  the document); any Google Fonts family works by name (fetched once). Variable fonts animate: `weight` drives `wght`, `axis_<tag>`
  drives any axis. Lay out by hand with `ultrafast measure '<text params>'`
  (glyph boxes, advance, ascent).
- **Accent words** in one node: `"Video is now *live*"` with `em_color`,
  `em_font: "ultrafast:serif"` (a serif italic accent in a sans line is the
  classic editorial look), or a marker with `em_highlight` and an animated
  `em_draw`; `[300 fps]{color=#7cf underline=4}` for one-off spans.
- **Masked line reveal** (editorial): a `text` whose `position` rises from
  below its baseline, under a `crop` to the line's box. **Glyph cascades**:
  `stagger` + `glyph_offset` / `glyph_scale` / `glyph_rotation` /
  `glyph_opacity` keyframed once, played per glyph in `forward`, `reverse`,
  `center` or `random` order.
- **Rhythm**: `ultrafast beats score.wav` (or the `ultrafast_beats` tool) gives
  the tempo, every beat, downbeats and onsets; land cuts, transitions and
  entrances on them (`examples/films/cuts.py`).
- **Motion that feels alive**: `{"wiggle": ...}`, `{"oscillate": ...}`,
  looping keys (`"repeat": "cycle"`), `{"audio": ...}` drivers that follow
  a soundtrack band (cut and pulse on the beat), and `{"expr": "..."}` for
  anything a formula of `t` says best. Ease with
  `{"bezier": [0.16, 1, 0.3, 1]}` or `out_expo` for decisive arrivals,
  `spring` / `out_back` for things that land with weight (scale pops,
  cards settling), `in_out_sine` for slow drifts.
- **Looks** (built-in components, see `ultrafast ops`): `bloom`, `grain`,
  `vignette`, `aberration`, `glitch`, `crt`, `halftone`, `dot_matrix`,
  `glass_pane`, `marble`, `starfield`, `glow`, `drop_shadow`,
  `stroke_outline`, `grade`, `lens_flare`, `light_leak`, `film_look`, `engrave`, and `repeater`
  (copies with offset, rotation, scale and fade: trails, radial arrays).
  Finish almost everything with `grain` (0.05 to 0.09) and often `bloom`.
- **Lottie and SVG assets**: drop a Lottie animation in with a `lottie` node
  (or `ultrafast import-lottie` to get its parts as editable nodes) and SVG
  artwork with `svg`; both stay vector and every part can be restyled.
- **Type on paths**: `on_path` (SVG path data) makes a line follow a curve
  or a circle; animate `path_offset` to run it along.
- **Decoding text**: animate `scramble` 0 to 1 on a text node and its
  characters settle out of random ones (tech, terminal, reveal moments).
- **Footage**: `chroma_key` keys out a green (or any) screen with spill
  suppression; `video` nodes play hardware-decoded footage.
- **Data**: `number` on a text node (animated) makes a counter, with
  `decimals`, `prefix`, `suffix`; charts are paths with `trim_end` drawing
  them on and shapes whose `size` grows (`examples/films/numbers_film.py`).
- **Voice and captions**: `captions` animates a transcript (karaoke, pop,
  reveal) and keeps clear of the social apps' buttons on vertical frames
  (`safe`); `ultrafast voiceover` makes a voiceover with word times (macOS
  voices, or eSpeak NG elsewhere). Cut scenes on the words that name them
  (`examples/films/launch.py`). `clip_camera` frames footage on whoever is
  talking; without `faces` it tracks them from the footage itself.
  Put captions above the grade (vignette, grain) and give white captions a
  `stroke_outline` so they read on light scenes.
- **Logos and icons**: an `svg` node places an SVG file as sharp vector
  artwork; `ultrafast import-svg` turns it into named, animatable parts.
- **Simulations**: `fluid` turns any layer into ink, smoke or fire (feed
  it as `dye`, push with `stir`); `feedback` runs your Slang on its own
  last frame (trails, echo tunnels, reaction-diffusion growth). They are
  exact on export and scrub like everything else; see
  `examples/films/flow.py`.
- **Real-looking 3D: `raymarch`.** Write the scene as a distance function
  (`float2 map(float3 p)` with `sdSphere`, `sdBox`, `sdTorus`, `smin`...) and
  get soft shadows, occlusion, reflections between parts and clean edges;
  `background: transparent` composites it over 2D. Prefer it over meshes
  whenever the object should look like a product shot. Over plain mesh 3D,
  `film_look` hides the tells and `engrave` restyles it outright.
- **3D** sits in the same graph: extruded and bevelled text, glTF,
  primitives, an environment image that lights and reflects (any ultrafast image,
  a shader included), shadows, fog, depth of field (`aperture`, `focus`),
  and glass (`transmission`, `ior`, `thickness`, `dispersion`: it refracts
  whatever is behind it, so give it a colorful, moving backdrop;
  `examples/films/prism.py`).
- **Shaders**: write `float4 shade(float2 pos, float px)` in Slang. Write them
  in terms of `pos / size()` so `resolution: 0.5` (render at half size and
  upscale) works for soft, costly looks.
- Motion blur: `"motion_blur": {"samples": 6, "shutter": 0.5}` on the format.
  Each moving layer is blurred on its own, as a compositor does: what
  animated params move (positions, scales, rotations, paths, particles).
  Footage and image-sequence frames hold at the frame's time (a camera
  already blurred its footage), and so do shaders, clock and `u` values
  alike (flicker, noise and glitches stay crisp); give a shader that moves
  things `"motion_blur": true`. Color, opacity and grade animation is not
  sampled, nor is motion under 1.5 px over the shutter. Add
  `"max_samples": 32` for whips and fast pans: a frame then takes one
  sample per 2 px its fastest layer moves over the shutter (at least
  `samples`, at most `max_samples`), so fast motion blurs smooth instead
  of in steps and slow frames stay cheap.

## Gotchas

- Generators (`solid`, `gradient`, `noise`, shaders) are infinite: transform
  or crop them to make panels.
- Sequence shots with timing windows on composites: an inactive `over`
  passes its background through; an inactive effect passes its source.
  Layers outside their windows cost nothing.
- Colors are sRGB hex or 0..1 arrays with straight alpha; compositing is
  linear light (`"space": "srgb"` to match CSS/Figma blending).
- Paths and text stay vector until rasterized: put `transform` nodes
  directly on them (absorbed, sharp at any scale).
- In components, `"$name"` refers to a component param; elsewhere text
  starting with `$` is literal.

## Recreating an existing video

See [RECONSTRUCTION.md](RECONSTRUCTION.md): `ultrafast reconstruct video.mp4
--out-dir rec` gives an editable draft (shots, text layers with matched fonts,
backgrounds, fades, motion); `probe`, `ocr`, `match-font`, `plate`, `matte`,
`compare`, `fit` and `fit-motion` refine it.

## When something is slow

`ultrafast bench film.json` reports GPU and CPU time per frame, kernels,
uploads and readbacks; `ultrafast profile film.json --from 4` says which render
groups (and so which nodes) take the GPU time, and how many motion-blur
samples each runs; `ultrafast plan film.json` shows how the graph fused into
kernels. Static parts are cached and settled type is not redrawn; what
costs most is usually a large layer that moves under motion blur (every
sample redraws the kernels holding moving layers; the layers around them
render once a frame, on the moving layers' averages).

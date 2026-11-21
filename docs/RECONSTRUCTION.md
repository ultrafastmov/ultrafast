# Recreating a finished video in ultrafast

The goal: given a rendered video, produce a ultrafast document that renders the
same frames and keeps every element editable. ultrafast gives an agent four
tools for the loop; the agent supplies the judgment.

## 0. One command

```bash
ultrafast reconstruct ref.mp4 --out-dir rec
```

runs the whole loop below and writes `rec/reconstruction.json`, an
editable document, plus a report. Per shot: text lines are recognized and
become `text` layers (lines no installed font can draw are dropped as OCR
noise); fonts, sizes, baselines, tracking and colors are matched; lines
centered in the frame become `align: center` at the frame center; the
background becomes the best gradient, a still plate image, or, when no still
plate explains the shot (moving footage), the reference video itself, in
which case text that changes during the shot is dropped as part of the
footage; and each layer's presence is measured frame by frame and becomes
opacity keyframes on its `over` node (fades). Shots are stacked as
`s<k>` composites with timing windows. The report lists every decision
(fonts with their match quality, background models with PSNR, presence
keys, dropped lines and why) and the document's PSNR against the
reference.

Example: a 6 s, three-shot film (a gradient with a Georgia Bold title
fading in over a Futura Medium subtitle with +2 tracking; a cream card
with centered Avenir Next Bold and Gill Sans; color-bar footage with a
Helvetica Neue Medium caption) reconstructs in 8 s on an M3 Max: all five
lines with the right family and weight, sizes within 0.3 px, baselines
within 0.1 px, both centered lines at exactly (640, y), the gradient to
within a pixel of its endpoints and code value of its colors, the fade as
an eased opacity ramp, the footage as the video itself, and the footage's
own timecode digits dropped.

Then refine with the steps below where the report shows weak spots.

## Layers: motion graphics over a still background

```bash
ultrafast decompose ref.mp4 --out-dir layers
```

splits each shot into a background plate and vector layers, for motion
graphics where the background holds still and the content does not (it
works where `reconstruct` would fall back to the video itself). A shot
whose background changes color for good (a wipe to a new scene) gets a
plate per background, cut in the middle of the change. The plate
is the temporal median of the shot, repaired where something sits most of
the time. Foreground (the difference from the plate, averaged over 3x3 so
grain does not count) is grouped into elements per frame and linked from
frame to frame into tracks, so a word, a bar chart or a counter is one
element over its lifetime. Each element gets a few flat colors: from its
flat interiors (pixels two away from any change), plus colors its edges
need that are not one of those (or a stronger edge color) partly over the
plate, so antialiasing, chroma bleed and motion blur do not become colors
of their own. Every frame is matted
against the known plate: each pixel is one of the element's colors at
some coverage over the plate, with a pixel's color taken from the opaque
shape it borders. Each color's coverage is traced at its half level into
outlines, and becomes one `path` per element and color that morphs through
its shapes on the frames where they change (hold keys), with its opacity
keyed when it fades and a window over its lifetime. Each element color's
fullest frame is its reference outline; frames that are that outline (or
a recently traced one, for an element whose content changes and then
holds) moved to a subpixel and clipped by a rectangle (a slide, a mask
reveal, a growing bar), or the reference scaled about its center (a pop
in or out), become `position` (and `anchor`/`scale`) keys and a keyed
`crop` instead of new outlines, and unchanged frames reuse what came before. The result is
`layers/layers.json`: the plate image with those paths over it, every
element an ordinary node to recolor, move, retime or delete.

On `examples/films/ledger.py` exported to video (24 s, 1080p, grain,
motion blur), the decomposition finds 96 elements in under two minutes
and scores 36.5 dB mean PSNR (30.2 dB worst frame) against the source, in
a 2.2 MB document. Colors drawn behind others in an element (a card, a
highlight band) are filled in under them where they surround them, so the
type over them leaves no holes or halos. Type is read where it can be (macOS): OCR (with word
boxes) runs at the frames where text-like elements are whole, and the
elements inside a recognized line are grouped by motion over the window
where they move as one (glyphs that enter in turn join once they hold
still; their entrances stay outlines). A line of one color takes its font
from the whole line, then each group's run of words becomes a `text` node
in that font; a run that fits no single face is tried as two runs (an
italic phrase). Each match is against the frame with that run's own
elements filled in from around them (so neighbors, highlight bands and
the plate count as background) and the run's ink box, with installed,
bundled and variable faces (weight, optical size, other axes, size,
tracking and color fitted). A loose fit is kept only when no other family
comes within 3 dB of it. Words are owned by the colors that stand out from what is behind them as
much as the type does (a highlight band under a phrase owns nothing), an
element running across words of two styles is read in parts (each part
from its stretch of the element, the element leaving its track once all of
it is read), and a short part may take a face the line already has. When a
line's elements show before it holds still, its entrance is fitted by
rendering (with the lines already read moving through the same region): a rise and a fade per glyph,
staggered (start, duration, rise, fade and stagger, with an out-cubic or
out-expo ease), kept when it renders those frames far better than the
line held still; the ledger's closing lines come back as three text nodes
(roman, the italic phrase, the second line), rising 38 to 40 px over
0.77 s, glyphs 0.018 s apart (they were made with 40 px, 0.8 s, 0.018 s). On the ledger film 30 lines come back as text:
every Geist title at 650, labels in Geist Mono 500, the Fraunces titles at
650 with `SOFT` 50 (its italic phrase as its own run) and the closing line
at 600; lines OCR misreads and very short words stay outlines.
Motion other than moves and rectangular reveals is re-traced frame by
frame. A gradient the palette would cut into bands (three or more
neighboring colors on one smooth ramp across space, in sRGB or linear
light) becomes a `gradient` painted through the outline with a `mask`,
moving with it. An element its colors cannot paint (judged at its
largest still frame: more than 15 sRGB levels off on average), such as a
photo, an illustration or footage, is painted from pixels instead: an
image cut from that frame (unblended from the plate) placed frame by
frame to a quarter pixel where it matches (a picture sliding, held,
leaving), or, when its content keeps changing, the source video itself
seen through a window keyed to the element's box. On a test film with a
sliding photo and a playing clip this goes from 28 dB to 42 dB. Big composites whose colors do
not fit six get twice as many, and plate blocks whose color stands out
from the blocks around them (a fill that sat there most of the time) are
taken from the frames where they match their surroundings. `--threshold`
(difference that counts as foreground, 14) and `--colors` (per element,
6) tune it.

## 1. Probe the reference

```bash
ultrafast probe ref.mp4 --sheet shots.png
```

Prints format (exact frame rate), and the shots: where each starts, whether
it begins with a cut or a fade from black, and its mean color. `shots.png`
is a contact sheet of the middle frame of every shot: look at it first.

```bash
ultrafast extract ref.mp4 --at 0.5,2.0,3.25 --out-dir frames
```

writes exact frames to look at closely.

## 2. Draft the document

Build one component per shot (or per recurring element), with timing
windows matching the shot boundaries from `probe`. Guess positions, sizes,
colors and fonts from the extracted frames.

## 3. Compare

```bash
ultrafast compare draft.json ref.mp4 --at 0.5,2.0,3.25 --out-dir diffs --worst 3
```

PSNR and SSIM per frame, plus `[render | reference | difference]` images of
the worst frames. Omit `--at` to compare every frame. `--offset` shifts the
reference in time if the draft's timing is off by a constant.

## 4. Fit

```bash
ultrafast fit draft.json ref.mp4 --param card.center:20 --param card.size:20 --param card.color:0.05 \
  --at 3.5 --out fitted.json
```

Searches static numeric params (Nelder-Mead) to minimize the error against
the reference at the given times; `:step` sets the initial search step.
Rendering is fast enough that hundreds of candidates take about a second at
720p. Fit one element at a time, at frames where it is visible, and keep
params that should not move out of the search.

Example (in this repo's tests of the loop): a card whose center, size and
color were guessed wrong went from 23.9 dB to 48.1 dB PSNR in 308
evaluations (1.2 s), recovering center (700, 330), size (640, 280) and
color #264653 to within a fraction of a pixel / code value.

## 5. Fit motion

```bash
ultrafast fit-motion draft.json ref.mp4 --param dot.center:30 --from 1.5 --to 2.97 --tolerance 2 --out moved.json
```

Solves the param at every frame in the range (the first frame, and any
frame where tracking is lost, starts with a global search; the rest are
warm-started from the previous frame), then simplifies the per-frame values
into the fewest keyframes that stay within `--tolerance`, choosing an easing
per segment: linear, a named curve, or a fitted cubic bezier.

Example: a circle sweeping 300 -> 980 px with an ease in/out, starting from
a draft where it sat still at the center: 45 frames solved in 6.5 s, the
motion collapsed into 2 keyframes with a fitted bezier ease
(0.63, -0.04, 0.39, 1.02), 46.7 dB mean PSNR on the fitted frames.

## 6. Text (Apple Vision on macOS, Tesseract elsewhere)

```bash
ultrafast ocr ref.mp4 --at 3.0 --draft draft.json
```

Recognizes every text line in the frame (content, confidence, pixel box),
estimates each line's color (2-means inside its box) and the background
(median of the frame border), and writes a draft document with one `text`
node per line, sized and placed from its box. Then fix what OCR cannot see
(weight, outline, gradient backgrounds) and `fit` sizes and positions.

```bash
ultrafast match-font draft.json ref.mp4 --node all --at 3.0 --out fonted.json
```

finds the font of each text node (or `--node text1,text3`): the reference
line becomes a coverage map (each pixel's place between background and text
color, in linear light); every installed or bundled face that has the
glyphs is sized so its ink height matches and given the tracking that
makes its width match; faces needing more than 0.35 em of tracking are
dropped, the rest (up to `--screen`, 400) are rendered by ultrafast and scored
against the coverage. Variable faces are screened at their default weight,
400 and 700, measured at the optical size the line's height asks for; the
best twelve are tried at every hundred of weight, and the ones with an
optical size axis also pinned to its default and ends (type set at a fixed
instance rather than by size). The best three are refined (size, baseline,
tracking, then each other axis such as `SOFT` or `wdth` at its ends and
middle, then weight in 25s) and the text color is solved by least squares.
The node is rewritten with `font`, `weight`, `style`, `size`, `position`
(baseline, `align: left`, `valign: baseline`), `tracking`, `color` and any
pinned axes (`axis_opsz`, `axis_SOFT`). `--fonts DIR` adds font folders
(repeatable) for faces that are not installed, `--font NAME:WEIGHT[:italic]`
keeps one known face (fitting only size, place, tracking and color),
`--ink X0,Y0,X1,Y1` gives the line's ink box when neighbors touch it, and
`--background bg.png` gives what is behind the text (a frame-sized image)
when it is not a plain field.

Example: four lines set in Georgia Bold, Futura Medium (+2 tracking),
Avenir Next Demi Bold and American Typewriter, exported to H.264, then
drafted by `ocr` in Helvetica Neue: all four families and weights
recovered, sizes within 0.1 px, baselines within 0.05 px, tracking within
0.05 px, in under a second per line from 520 installed faces.

## 7. Background plates

```bash
ultrafast plate ref.mp4 --from 0 --to 3.0 --doc fonted.json --bg bg --image plate.png --out plated.json
```

The plate is the per-pixel temporal median of frames sampled across the
shot (`--samples`, 15), so anything that moves drops out. With `--doc` and
`--bg`, everything in the document except the background node marks the
pixels to fill (dilated 2 px for codec smear), which are inpainted from
their surroundings (push-pull, linear light). Solid, linear and radial
gradients in both interpolation spaces are fitted to the plate, rendered by
ultrafast and scored; the best one replaces `bg` when it reaches `--threshold`
(38 dB) against the plate, otherwise the plate image does. The report lists
every model with its PSNR and node.

Example: the four-line test above over a diagonal gradient. After
`match-font` and `plate`, the gradient came back as `#14213d -> #2a4a7f`
from (-0.4, 0.8) to (1280.4, 719.2) with linear interpolation (truth:
exactly those colors, (0, 0) to (1280, 720)), 50.7 dB against the plate.
The whole draft went from 18.2 dB to 35.4 dB against the decoded
reference; exported through the same encoder it is 40.3 dB (YUV PSNR)
against the reference, the rest being codec noise on glyph edges.

## 8. Subjects

```bash
ultrafast matte ref.mp4 subject
```

writes a foreground subject matte per frame (`subject.frames/00000.png`, ...;
white = subject). Use it as an image sequence luma matte to split footage
into subject and background layers:

```json
"matte": { "op": "image", "params": { "src": "subject.frames/{frame:05}.png", "fit": "fill" } },
"subject": { "op": "mask", "inputs": { "source": "footage", "matte": "matte" }, "params": { "mode": "luma" } }
```

## What is next

- Background plates for moving cameras (align frames before the median).
- Clean footage: remove static overlays baked into footage so the
  reconstructed overlay layers can be edited without ghosts.
- Moving layers: run `fit-motion` automatically on layers the presence
  pass flags as `moving`.
- Mattes encoded as video instead of PNG sequences.

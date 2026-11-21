# Driving ultrafast live

`ultrafast live doc.json` opens a realtime preview and listens for requests on
`127.0.0.1:7878` (`--port` to change, `--headless` for no window). Requests
and responses are single-line JSON objects. From a shell:

```bash
ultrafast send '{"cmd":"describe"}'
ultrafast send '{"cmd":"edit","edits":[{"edit":"set_param","node":"title","param":"color","value":"#ff3366"}]}'
ultrafast send '{"cmd":"snapshot","t":1.5,"path":"out/check.png"}'
```

or keep a socket open and write one request per line. The session also
reloads `doc.json` whenever the file changes, so editing the file directly
works too.

The preview window is a player: a timeline runs along the bottom (click or
drag it to scrub, scroll to step frames, click the picture to play or
pause). Keys: space play/pause, left/right 1 s (shift: one frame), `,` `.`
one frame, home restart, `i` / `o` set the loop in / out at the playhead,
`x` clear the loop, `1` `2` `3` half, normal and double speed, `h` hide the
timeline, `f` fullscreen, `m` mute, escape quit. While it plays or you scrub, the
player renders one sample a frame; a stopped frame shows the document's
full motion blur, exactly as the export will. When an agent edits a
node, the player outlines what changed for a moment (the whole picture
when the node has no vector bounds), so people watching can follow the
work.

Pointing works both ways. Option-click (alt-click) the picture to select
what is under the cursor: the player outlines it in dashed cyan and names
it in the title bar, and `{"cmd":"selection"}` tells an agent which
document node it is (`node`, `op`, the `part` inside a component or block,
and frame `bounds`), so "make this bigger" needs no ids. An agent points
for the person with `{"cmd":"select","node":"title"}` (or `"x"`, `"y"` in
frame pixels); `{"cmd":"select"}` alone clears it. `{"cmd":"player"}` returns
a PNG of exactly what the player shows (any size), and `{"cmd":"loop",
"in":..,"out":..}` sets the loop from an agent.

Comparing with a reference. `ultrafast live doc.json --compare ref.mp4` shows
the reference video against the document (the source a reconstruction was
made from, or a render it should match): the document left of a cyan
divider that follows the pointer, the reference right of it. `c` cycles
split, difference (black where they agree, brightened two stops) and off.
`{"cmd":"compare","video":"ref.mp4","mode":"split"|"difference"|"off",
"split":0.5,"t":3.2}` does the same from an agent and returns the PSNR of
the document's frame against the reference's at `t`; `player` snapshots
show the compare view.

The preview window plays the document's `audio` nodes through the default
output device. While sound plays, the audio device is the clock (picture
follows sound, compensated for output latency); edits to audio nodes rebuild
the mix in the background, with decoded sources cached, and swap it in
without a gap. Seeks and play/pause realign the two.

## Recording a live edit

`ultrafast record doc.json script.json out.mp4 [--size 1280x720] [--duration S]`
plays the document (looping) while applying a script of edit batches at
given moments, and records what the player shows: the timeline, the
outlines of what each batch touched, and a log line (`note`, or a summary
of the edits). The script is a list of steps:

```json
[{"at": 1.4, "note": "agent ▸ make the second line pink",
  "edits": [{"edit": "set_param", "node": "line2", "param": "color", "value": "#ff5c7a"}]}]
```

`examples/recordings/` has one. `--no-log` leaves the log line out.

## Edits

`edit` takes a batch that applies atomically: if any edit fails, none does.

```json
{"edit": "set_param", "node": "id", "param": "name", "value": <param>}
{"edit": "remove_param", "node": "id", "param": "name"}
{"edit": "add_node", "id": "new_id", "node": {"op": "...", "params": {...}, "inputs": {...}}}
{"edit": "add_layer", "id": "new_id", "node": {...}}      (adds it over the current output)
{"edit": "remove_node", "id": "id"}
{"edit": "connect", "node": "id", "port": "fg", "source": "other_id"}
{"edit": "disconnect", "node": "id", "port": "fg"}
{"edit": "set_output", "node": "id"}
{"edit": "set_format", "format": {"width": 1920, "height": 1080, "fps": 60, "duration": 5}}
{"edit": "set_timing", "node": "id", "timing": {"in": 1, "out": 3}}       (null clears)
{"edit": "set_component", "name": "lower_third", "component": {...}}      (null removes)
```

The response says whether the batch was `params` (instant: no replan, no
compile) or `structure` (replanned; any new kernels compile in the
background while the preview keeps playing).

## Looking at the result

- `snapshot` renders one exact frame to PNG and returns its path.
- `contact` renders a grid of frames (`count` evenly spaced, or explicit
  `times`) into one PNG: the fastest way to review motion.
- `bounds` returns the pixel bounds of a path or text node (for layout).
- `stats` returns frame timings, CPU<->GPU transfers, kernel compile state
  and per-node diagnostics (bad params, shader errors, missing files).
- `plan` shows how the document was fused into GPU kernels.

## All commands

| cmd | fields | returns |
|---|---|---|
| `ping` | | `pong` |
| `get` | | `doc`, `revision` |
| `set` | `doc` | `revision` |
| `load` | `path` | `revision` |
| `save` | `path?` | `path` |
| `edit` | `edits` | `change`, `revision` |
| `describe` | | `output`, `format`, `nodes` |
| `ops` | | op names, ports, forms |
| `plan` | | `plan` |
| `play`, `pause` | | `playing` |
| `seek` | `t` | `t` |
| `rate` | `value` | `rate` |
| `snapshot` | `t?`, `path?` | `path`, `width`, `height` |
| `contact` | `times?`, `count?`, `columns?`, `path?` | `path`, `times` |
| `bounds` | `node` | `bounds` |
| `stats` | | timings, transfers, kernels, diagnostics |
| `quit` | | |

Add `"id": <anything>` to a request to have it echoed in the response.

See [OPS.md](OPS.md) for every op and its params.

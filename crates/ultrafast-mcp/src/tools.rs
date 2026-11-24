//! Tool definitions: names, descriptions, input schemas and the safety
//! annotations clients and directories read (`readOnlyHint`,
//! `destructiveHint`, ...).
//!
//! Two sets. [`local`] is what `uf mcp` serves: documents are JSON files on
//! the agent's machine, addressed by path, and media are local files.
//! [`remote`] is what the HTTP server serves: no file paths at all;
//! documents travel inline in each call and media inside them are asset ids
//! or https URLs, and exports come back as download links.

use serde_json::{Value, json};

/// The safety hints of a tool.
#[derive(Clone, Copy)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

const READ: Hints = Hints {
    read_only: true,
    destructive: false,
    idempotent: true,
};
/// Writes something new (a file that did not exist, a new stored output).
const CREATE: Hints = Hints {
    read_only: false,
    destructive: false,
    idempotent: false,
};
/// Changes or replaces a file that already exists.
const OVERWRITE: Hints = Hints {
    read_only: false,
    destructive: true,
    idempotent: true,
};

fn tool(name: &str, title: &str, description: &str, schema: Value, h: Hints) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": schema,
        "annotations": {
            "title": title,
            "readOnlyHint": h.read_only,
            "destructiveHint": h.destructive,
            "idempotentHint": h.idempotent,
            "openWorldHint": false,
        }
    })
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// `props` with the export settings every export tool takes (the same names
/// as `ultrafast export`'s flags and the HTTP API's `output`).
fn with_export(mut props: Value) -> Value {
    let settings = json!({
        "format": { "type": "string", "enum": ["mp4", "mov", "mkv", "webm", "gif", "png", "exr", "wav", "mp3", "m4a", "flac", "opus"], "description": "Container (default: from the output's extension, else mp4). png and exr are image sequences; wav to opus are sound alone." },
        "codec": { "type": "string", "enum": ["h264", "hevc", "av1", "vp9", "prores"], "description": "Video codec (default h264; vp9 for webm)" },
        "quality": { "type": "string", "enum": ["draft", "standard", "high", "master"], "description": "draft: a fast preview without motion blur; standard (default); high: more motion-blur samples, a slower better encode; master: the most of both" },
        "crf": { "type": "number", "description": "Constant quality on the codec's scale (lower is better; H.264/HEVC 0-51, AV1/VP9 0-63), instead of the quality level's" },
        "bitrate": { "type": ["number", "string"], "description": "Average bits per second (or \"8M\") instead of constant quality" },
        "prores_profile": { "type": "string", "enum": ["proxy", "light", "standard", "hq", "4444", "4444-xq"], "description": "ProRes flavor (default hq; 4444 with alpha)" },
        "alpha": { "type": "boolean", "description": "Keep transparency: ProRes 4444 (.mov), VP9 (.webm), GIF, or a PNG or EXR sequence" },
        "size": { "type": "string", "description": "Output size, sharp at any size: 720p, 1080p, 1440p, 4k (the short side, so vertical stays vertical) or WxH of the document's shape" },
        "scale": { "type": "number", "description": "Output size as a multiple of the document's (0.5, 2)" },
        "fps": { "type": "number", "description": "Output frame rate (default the document's; a GIF's at most 30 unless asked)" },
        "audio": { "type": "boolean", "description": "Mix the document's sound in (default true)" },
        "audio_codec": { "type": "string", "enum": ["aac", "opus", "mp3", "flac", "pcm", "pcm-24"], "description": "Sound codec (default aac; opus for webm; pcm with ProRes)" },
        "audio_bitrate": { "type": ["number", "string"], "description": "Sound bits per second (or \"192k\")" },
        "loops": { "type": "integer", "description": "GIF: times it plays again after the first (0: once; default forever)" }
    });
    if let (Some(p), Some(s)) = (props.as_object_mut(), settings.as_object()) {
        for (k, v) in s {
            p.insert(k.clone(), v.clone());
        }
    }
    props
}

const OPS: &str = "The op vocabulary: every op ultrafast knows (generators, footage, type, shapes, 3D, particles, compositing, filters, captions, charts, blocks, looks), its input ports and params with types, defaults and docs. With no arguments it returns all of it as compact Markdown; `ops` returns the full JSON of just those ops. Read it before writing a document, and again when unsure of a param.";

const NEW: &str = "Templates to start from: `vertical` (1080x1920 editorial), `title` (1920x1080 title over a shader world), `square` (1080x1080 risograph), `promo` (1080x1920, 13 s: title, stat, quote and end card blocks joined by transitions), `explainer` (1920x1080 on paper: chapter, key idea with a lower third, stat), `product` (a product reveal).";

const CHECK: &str = "Problems in a document, before rendering it: unknown ops, misspelled params (with suggestions), wrongly shaped values, broken connections, shader compile errors, missing files or fonts. Run it after every change until it reports none.";

const REVIEW: &str = "Render the whole film and report what a viewer would trip over, measured on the final frames: text that never reaches readable contrast or dips below it, is on screen too briefly for its word count, runs off the frame, sits in the safe margin or under social-app buttons (`safe`: margin | social), overlaps other text, or is too small; and flashing above 3 per second (seizure risk). Each issue names a node, a time span and a fix. Run it before exporting and fix everything it finds.";

const FRAME: &str = "Render one exact frame of the document and look at it (an image, longest side `max_size`).";

const SHEET: &str = "A labeled contact sheet of frames in one image: the fastest way to see a film's motion and pacing. Give `times` in seconds, or `count` evenly spaced frames (default 9).";

const INSPECT: &str = "What is on screen at time `t`: every visible element (text, path, shape, image, video) with the document node to edit, its bounds in the frame and, for text, its words. Use it to find which node to change at a moment without reading the whole document.";

const PROFILE: &str = "Where the document's GPU time goes: renders some frames with every render group timed on its own and returns milliseconds per frame per group (named by the nodes it renders) and the motion-blur samples each runs. Use it when a film renders slowly.";

const BEATS: &str = "The rhythm of a soundtrack: tempo (`bpm`), every beat, downbeats and onsets with their strengths, in seconds. Use it to put cuts, transitions and entrances on the beat.";

const EDITS: &str = "Edits, applied in order and all or nothing. Each is an object with `edit` and its fields: set_param {node, param, value}, remove_param {node, param}, add_node {id, node}, add_layer {id, node} (adds the node over the current output: the quickest way to put something on screen), remove_node {id}, connect {node, port, source}, disconnect {node, port}, set_output {node}, set_format {format: the whole format}, set_timing {node, timing (null clears)}, set_component {name, component}. Examples: {\"edit\":\"set_param\",\"node\":\"title\",\"param\":\"color\",\"value\":\"#ff3366\"}; {\"edit\":\"add_layer\",\"id\":\"who\",\"node\":{\"op\":\"block\",\"params\":{\"kind\":\"lower_third\",\"name\":\"Mira Chen\",\"role\":\"PRODUCER\"},\"timing\":{\"offset\":1}}}; {\"edit\":\"remove_node\",\"id\":\"old_logo\"}.";

const ESTIMATE: &str = "What exporting the document will cost before it runs: rendered minutes (1080p minutes; 720p counts half, 4K three), GPU seconds measured by rendering a sample of frames on this GPU, and the time to the first frame. Pass the export settings you will export with (size, quality, fps...): minutes are counted at the output size and frames timed with the quality level's motion blur. Nothing is encoded or stored.";

const BATCH: &str = "One document, many videos: each row sets some of the document's params and becomes its own video. A key `node.param` sets that param (e.g. `title.title`), `format.width` / `format.height` / `format.fps` / `format.duration` the format, and a row may carry `edits` (an edit batch). `name` and other keys fill `{name}`-style placeholders in the output pattern; `{row}` is the row number from 1. Every row is checked before anything renders: a key naming a node or a param that isn't there is refused, with the nearest name.";

const MEASURE: &str = "Glyph-by-glyph metrics of a line of text (pen positions, advances, ink boxes, ascent, descent), for laying type out by hand. `params` are `text` op params (text, font, size, weight, tracking, ...).";

const OUTLINE: &str = "A line of text as SVG path data placed in pixels, for path morphs (`to`/`morph`) and vector work. `params` are `text` op params.";

fn times_props(doc: Value) -> Value {
    json!({
        "document": doc,
        "times": { "type": "array", "items": { "type": "number" }, "description": "Seconds" },
        "count": { "type": "integer", "minimum": 1, "maximum": 30, "description": "Evenly spaced frames when no `times` (default 9)" },
        "columns": { "type": "integer", "minimum": 1, "description": "Frames per row (default 3, or 5 for vertical formats)" }
    })
}

fn ops_tool() -> Value {
    tool(
        "ultrafast_ops",
        "Op reference",
        OPS,
        object(
            json!({ "ops": { "type": "array", "items": { "type": "string" }, "description": "Op names to return in full JSON, e.g. [\"text\", \"captions\"]" } }),
            &[],
        ),
        READ,
    )
}

fn template_enum() -> Value {
    json!({ "type": "string", "enum": ultrafast_ops::templates::NAMES })
}

fn review_props(doc: Value) -> Value {
    json!({
        "document": doc,
        "safe": { "type": "string", "enum": ["margin", "social"], "description": "margin (default): 4% from every edge; social: clear of the captions and buttons of vertical social apps" },
        "step": { "type": "number", "description": "Seconds between text probes (default 0.1)" }
    })
}

fn text_params() -> Value {
    json!({ "params": { "type": "object", "description": "text op params: text, font, size, weight, tracking, ..." } })
}

/// The tools `uf mcp` serves: documents are files on this machine.
pub fn local() -> Value {
    let path = json!({ "type": "string", "description": "Path to an ultrafast document (.json)" });
    // The local schemas name the document `path`; rename the key from the
    // shared builders.
    let with_path = |mut props: Value| {
        if let Some(o) = props.as_object_mut() {
            o.remove("document");
            o.insert("path".into(), path.clone());
        }
        props
    };
    json!([
        ops_tool(),
        tool(
            "ultrafast_new",
            "New document",
            &format!("{NEW} Writes the template to `path` (never overwrites)."),
            object(
                json!({ "template": template_enum(), "path": path }),
                &["template", "path"]
            ),
            CREATE
        ),
        tool(
            "ultrafast_check",
            "Check document",
            CHECK,
            object(json!({ "path": path }), &["path"]),
            READ
        ),
        tool(
            "ultrafast_frame",
            "Render a frame",
            FRAME,
            object(
                json!({
                    "path": path,
                    "t": { "type": "number", "description": "Time in seconds" },
                    "max_size": { "type": "integer", "description": "Longest side of the returned image in px (default 1024)" }
                }),
                &["path", "t"]
            ),
            READ
        ),
        tool(
            "ultrafast_sheet",
            "Contact sheet",
            SHEET,
            object(with_path(times_props(json!(null))), &["path"]),
            READ
        ),
        tool(
            "ultrafast_review",
            "Review film",
            REVIEW,
            object(with_path(review_props(json!(null))), &["path"]),
            READ
        ),
        tool(
            "ultrafast_inspect",
            "Inspect a moment",
            INSPECT,
            object(
                json!({ "path": path, "t": { "type": "number", "description": "Seconds" } }),
                &["path", "t"]
            ),
            READ
        ),
        tool(
            "ultrafast_edit",
            "Edit document",
            "Apply an edit batch to the document file, atomically, and get back the kind of change and any problems on the nodes it touched.",
            object(
                json!({ "path": path, "edits": { "type": "array", "items": { "type": "object" }, "description": EDITS } }),
                &["path", "edits"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_export",
            "Export video",
            "Encode the document to a file: MP4, MOV, MKV or WebM video (H.264, HEVC, AV1, VP9 or ProRes, transparent where the codec can be), a GIF, a PNG or EXR sequence (`out` a folder ending in `/` or a pattern like `frames/{frame:05}.png`), or sound alone (.wav, .mp3, .m4a, .flac, .opus). Any size from the same document, another frame rate, four quality levels. The format follows `out`'s extension. Replaces `out` if it exists.",
            object(
                with_export(json!({
                    "path": path,
                    "out": { "type": "string", "description": "Output file (its extension picks the format), or a folder for an image sequence" },
                    "from": { "type": "number", "description": "Start in seconds (default 0)" },
                    "to": { "type": "number", "description": "End in seconds (default: the whole film)" }
                })),
                &["path", "out"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_profile",
            "Profile GPU time",
            PROFILE,
            object(
                json!({
                    "path": path,
                    "frames": { "type": "integer", "description": "Frames to time (default 60)" },
                    "from": { "type": "number", "description": "Start time in seconds (default 0)" }
                }),
                &["path"]
            ),
            READ
        ),
        tool(
            "ultrafast_estimate",
            "Estimate a render",
            ESTIMATE,
            object(
                with_export(json!({
                    "path": path,
                    "from": { "type": "number", "description": "Start of the range in seconds (default 0)" },
                    "to": { "type": "number", "description": "End of the range in seconds (default: the whole film)" },
                    "samples": { "type": "integer", "description": "Frames to time (default 32)" }
                })),
                &["path"]
            ),
            READ
        ),
        tool(
            "ultrafast_batch",
            "Batch render",
            &format!("{BATCH} Writes one video per row (replacing files that exist) and returns their paths."),
            object(
                with_export(json!({
                    "path": path,
                    "data": { "type": "string", "description": "A data file: .csv (header row, then a row per video), .json (a list of objects) or .jsonl" },
                    "rows": { "type": "array", "items": { "type": "object" }, "description": "The rows inline, instead of `data`" },
                    "out": { "type": "string", "description": "Output pattern, relative to the document's folder, e.g. \"out/{name}.mp4\" (default \"<document>-{row}.mp4\"); its extension picks the format" }
                })),
                &["path"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_beats",
            "Find beats",
            BEATS,
            object(
                json!({
                    "path": { "type": "string", "description": "An audio or video file" },
                    "band": { "type": "string", "enum": ["all", "low", "mid", "high"], "description": "Frequency band to follow (low: kick, high: hats)" }
                }),
                &["path"]
            ),
            READ
        ),
        tool(
            "ultrafast_voiceover",
            "Voiceover",
            "Speak a script into a WAV file and write the start and end of every word as JSON, ready for an `audio` node and a `captions` node (cut scenes on the words too). Uses the system voices on macOS and espeak-ng on Linux. Replaces `wav` and `words` if they exist.",
            object(
                json!({
                    "text": { "type": "string" },
                    "wav": { "type": "string", "description": "Output .wav path" },
                    "words": { "type": "string", "description": "Output word-timings .json path" },
                    "voice": { "type": "string", "description": "A system voice, e.g. Samantha or Daniel on macOS, en-us or en-gb on Linux" },
                    "rate": { "type": "number", "description": "0..1, default 0.5" }
                }),
                &["text", "wav", "words"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_import_svg",
            "Import SVG",
            "An SVG file as an editable component (one path per shape, gradients, strokes, text as outlines, clips, masks), to animate part by part. Returns the component JSON, or adds it to the document at `into`. To just place artwork, use an `svg` node instead.",
            object(
                json!({
                    "path": { "type": "string", "description": "The .svg file" },
                    "size": { "type": "array", "items": { "type": "number" }, "description": "Fit inside [w, h] px" },
                    "name": { "type": "string", "description": "Component name (default: the file name)" },
                    "into": { "type": "string", "description": "A document to add the component to" }
                }),
                &["path"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_decompose",
            "Decompose a video",
            "Turn a finished motion-graphics video into an editable document: per shot a background plate and vector layers (each element's color a path keyed with its motion, opacity and reveals; gradients as gradient fills; photos as images moved frame by frame; footage as a window onto the source), with lines of type read back as text nodes in the matched font, weight and size. Returns the document path, how closely it matches the source (PSNR, worst frames with diff images) and the text it recovered. Takes several times the video's length.",
            object(
                json!({
                    "video": { "type": "string", "description": "The video to decompose" },
                    "out_dir": { "type": "string", "description": "Folder for layers.json, plates and diff images" },
                    "threshold": { "type": "number", "description": "Difference from the plate that counts as foreground (default 14)" },
                    "colors": { "type": "integer", "description": "Colors per element (default 6)" }
                }),
                &["video", "out_dir"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_match_font",
            "Match a font",
            "Find the font of a text node from a video frame: family (installed, bundled or from Google Fonts), weight, style, optical size and other variable axes, size, baseline, tracking and color, written into the node. The node should be a single untransformed line placed roughly over the text (ultrafast_ocr drafts them). `font` (\"Name:weight[:italic]\") keeps a known face; `ink` [x0,y0,x1,y1] gives the line's ink box when neighbors touch it.",
            object(
                json!({
                    "path": path,
                    "video": { "type": "string" },
                    "node": { "type": "string" },
                    "t": { "type": "number", "description": "Seconds into the video" },
                    "font": { "type": "string" },
                    "ink": { "type": "array", "items": { "type": "number" } }
                }),
                &["path", "video", "node", "t"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_ocr",
            "Read text in a frame",
            "Read the text in a video frame: each line with its box, words and their boxes, color, and a draft document with one text node per line to refine with ultrafast_match_font (Apple Vision on macOS, Tesseract on Linux).",
            object(
                json!({
                    "video": { "type": "string" },
                    "t": { "type": "number", "description": "Seconds" },
                    "draft": { "type": "string", "description": "Where to write the draft document (optional)" }
                }),
                &["video", "t"]
            ),
            OVERWRITE
        ),
        tool(
            "ultrafast_measure",
            "Measure text",
            MEASURE,
            object(
                json!({ "params": text_params()["params"], "base": { "type": "string", "description": "Directory font paths are relative to" } }),
                &["params"]
            ),
            READ
        ),
        tool(
            "ultrafast_outline",
            "Text to paths",
            OUTLINE,
            object(
                json!({ "params": text_params()["params"], "base": { "type": "string" } }),
                &["params"]
            ),
            READ
        ),
    ])
}

/// The document argument of the remote tools.
fn doc_schema() -> Value {
    json!({
        "type": "object",
        "description": "The whole ultrafast document (JSON). Media inside it are asset ids (asset://...) or https URLs, never local paths.",
        "additionalProperties": true
    })
}

/// The tools the HTTP server serves: documents inline, no file paths.
pub fn remote() -> Value {
    let doc = doc_schema();
    json!([
        ops_tool(),
        tool(
            "ultrafast_new",
            "New document",
            &format!("{NEW} Returns the template document to edit and pass to the other tools."),
            object(json!({ "template": template_enum() }), &["template"]),
            READ
        ),
        tool(
            "ultrafast_check",
            "Check document",
            CHECK,
            object(json!({ "document": doc }), &["document"]),
            READ
        ),
        tool(
            "ultrafast_frame",
            "Render a frame",
            FRAME,
            object(
                json!({
                    "document": doc,
                    "t": { "type": "number", "description": "Time in seconds" },
                    "max_size": { "type": "integer", "description": "Longest side of the returned image in px (default 1024)" }
                }),
                &["document", "t"]
            ),
            READ
        ),
        tool(
            "ultrafast_sheet",
            "Contact sheet",
            SHEET,
            object(times_props(doc.clone()), &["document"]),
            READ
        ),
        tool(
            "ultrafast_review",
            "Review film",
            REVIEW,
            object(review_props(doc.clone()), &["document"]),
            READ
        ),
        tool(
            "ultrafast_inspect",
            "Inspect a moment",
            INSPECT,
            object(
                json!({ "document": doc, "t": { "type": "number", "description": "Seconds" } }),
                &["document", "t"]
            ),
            READ
        ),
        tool(
            "ultrafast_edit",
            "Edit document",
            "Apply an edit batch to a document, atomically. Returns the edited document (pass it to the next call), the kind of change and any problems on the nodes it touched. Nothing is stored.",
            object(
                json!({ "document": doc, "edits": { "type": "array", "items": { "type": "object" }, "description": EDITS } }),
                &["document", "edits"]
            ),
            READ
        ),
        tool(
            "ultrafast_export",
            "Export video",
            "Render the document and store it: MP4, MOV, MKV or WebM video (H.264, HEVC, AV1, VP9 or ProRes, transparent where the codec can be), a GIF, a PNG or EXR sequence (as a zip), or sound alone. Any size from the same document, another frame rate, four quality levels. Returns a link to download it, the frame count and how long it took. Each call stores a new file; nothing is overwritten.",
            object(
                with_export(json!({
                    "document": doc,
                    "from": { "type": "number", "description": "Start in seconds (default 0)" },
                    "to": { "type": "number", "description": "End in seconds (default: the whole film)" }
                })),
                &["document"]
            ),
            CREATE
        ),
        tool(
            "ultrafast_profile",
            "Profile GPU time",
            PROFILE,
            object(
                json!({
                    "document": doc,
                    "frames": { "type": "integer", "description": "Frames to time (default 60)" },
                    "from": { "type": "number", "description": "Start time in seconds (default 0)" }
                }),
                &["document"]
            ),
            READ
        ),
        tool(
            "ultrafast_estimate",
            "Estimate a render",
            ESTIMATE,
            object(
                with_export(json!({
                    "document": doc,
                    "from": { "type": "number", "description": "Start of the range in seconds (default 0)" },
                    "to": { "type": "number", "description": "End of the range in seconds (default: the whole film)" },
                    "samples": { "type": "integer", "description": "Frames to time (default 32)" }
                })),
                &["document"]
            ),
            READ
        ),
        tool(
            "ultrafast_batch",
            "Batch render",
            &format!(
                "{BATCH} Renders and stores one video per row (at most 20 rows a call) and returns a download link for each; nothing is overwritten."
            ),
            object(
                with_export(json!({
                    "document": doc,
                    "rows": { "type": "array", "items": { "type": "object" }, "maxItems": 20, "description": "One object per video, e.g. {\"name\": \"ana\", \"title.title\": \"Happy birthday, Ana\"}" }
                })),
                &["document", "rows"]
            ),
            CREATE
        ),
        tool(
            "ultrafast_beats",
            "Find beats",
            BEATS,
            object(
                json!({
                    "audio": { "type": "string", "description": "An uploaded asset id (asset://...) or an https URL of an audio or video file" },
                    "band": { "type": "string", "enum": ["all", "low", "mid", "high"] }
                }),
                &["audio"]
            ),
            READ
        ),
        tool(
            "ultrafast_import_svg",
            "Import SVG",
            "An SVG as an editable component (one path per shape, gradients, strokes, text as outlines, clips, masks), to put in a document's `components` and animate part by part. Give the SVG source as `svg`. To just place artwork, use an `svg` node instead.",
            object(
                json!({
                    "svg": { "type": "string", "description": "The SVG source text" },
                    "size": { "type": "array", "items": { "type": "number" }, "description": "Fit inside [w, h] px" }
                }),
                &["svg"]
            ),
            READ
        ),
        tool(
            "ultrafast_measure",
            "Measure text",
            MEASURE,
            object(text_params(), &["params"]),
            READ
        ),
        tool(
            "ultrafast_outline",
            "Text to paths",
            OUTLINE,
            object(text_params(), &["params"]),
            READ
        ),
    ])
}

/// What the server tells the agent at `initialize`.
pub fn instructions(remote: bool) -> String {
    let doc = if remote {
        "Pass the whole document as `document` to every tool; ultrafast_edit returns the edited document for the next call. Media are asset ids or https URLs."
    } else {
        "Documents are JSON files; tools take their `path`. Edit with ultrafast_edit or by writing the file; the server reloads it."
    };
    format!(
        "ultrafast renders videos on the GPU from JSON documents: a `format` (size, fps, duration), `nodes` (each an op with params and timing) and an `output`. {doc} The loop: ultrafast_ops for the vocabulary, ultrafast_new for a starting point, ultrafast_check until clean, ultrafast_sheet and ultrafast_frame to look, ultrafast_inspect to find the node behind something, ultrafast_review and fix what it reports, then ultrafast_export (ultrafast_estimate first quotes the cost; ultrafast_batch makes one video per row of data). Always look at a sheet before exporting."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_annotated_and_named_once() {
        for set in [local(), remote()] {
            let tools = set.as_array().unwrap();
            let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
            for t in tools {
                assert!(t["annotations"]["readOnlyHint"].is_boolean(), "{}", t["name"]);
                assert!(t["annotations"]["destructiveHint"].is_boolean(), "{}", t["name"]);
                assert!(t["title"].is_string());
                assert_eq!(t["inputSchema"]["type"], "object");
                assert!(t["description"].as_str().unwrap().len() < 2000);
            }
            let n = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), n);
        }
        assert_eq!(local().as_array().unwrap().len(), 20);
        assert_eq!(remote().as_array().unwrap().len(), 16);
    }

    #[test]
    fn local_schemas_take_paths_and_remote_ones_never_do() {
        for t in local().as_array().unwrap() {
            assert!(
                t["inputSchema"]["properties"].get("document").is_none(),
                "{}",
                t["name"]
            );
        }
        for t in remote().as_array().unwrap() {
            let props = &t["inputSchema"]["properties"];
            for key in ["path", "out", "out_dir", "into", "video", "wav", "words", "base"] {
                assert!(props.get(key).is_none(), "{} takes {key}", t["name"]);
            }
        }
    }
}

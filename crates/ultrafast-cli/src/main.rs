//! `ultrafast` command line.

use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;
use ultrafast_gpu::Device;
use ultrafast_graph::{Document, Edit, Node, Param};
use ultrafast_render::{Mode, Renderer};

mod mcp;
mod serve;

const USAGE: &str = "\
ultrafast: realtime GPU video composition engine

usage:
  ultrafast info                                 show GPU device
  ultrafast mcp                                  serve ultrafast to agents over MCP (stdio): the tools (new, check,
                                             frame and sheet as images, review, edit, export, ...) and the skill
  ultrafast new    <vertical|title|square|promo|explainer|product> <out.json>
                                             start a document from a template (edit, check, sheet, export)
  ultrafast ops                                  op vocabulary (JSON): ports, params, types, defaults
  ultrafast ops --index | <op>... | --markdown   every op on one line | those ops' params | all, as Markdown
  ultrafast schema                               the document format as JSON Schema (draft 2020-12),
                                             for editors, validators and structured outputs
  ultrafast check  <doc.json>                    find problems without rendering: unknown ops,
                                             misspelled params, bad values, broken shaders, missing files,
                                             node and timing keys ultrafast ignores
  ultrafast outline '<text params JSON>' [--base DIR]
                                             a line of text as SVG path data (for path morphs and vector work)
  ultrafast measure '<text params JSON>' [--base DIR]
                                             glyph-by-glyph metrics of a line of text (for hand layout)
  ultrafast import-lottie <animation.json> [--size W,H] [--name NAME] [--into doc.json]
                                             a Lottie animation as an editable component (paths,
                                             keyframes, transforms); or use a `lottie` node directly
  ultrafast decompose <video> --out-dir DIR [--threshold 14] [--colors 4]
                                             split a motion-graphics video into a still plate and
                                             vector layers keyed frame by frame (editable paths)
  ultrafast review <doc.json> [--step S] [--safe margin|social]
                                             render the film and report what viewers would trip over:
                                             unreadable, too-brief, clipped, overlapping or tiny text;
                                             text under social UI; seizure-risk flashing (JSON)
  ultrafast import-svg <file.svg> [--size W,H] [--name NAME] [--into doc.json]
                                             an SVG as an editable component: one path per shape,
                                             gradients, strokes, text as outlines (prints it, or adds it
                                             to doc.json's components); or use an `svg` node directly
  ultrafast voiceover '<text>' <out.wav> <words.json> [--voice NAME] [--rate 0..1]
                                             speak a script (macOS voices, or espeak-ng) and write the
                                             exact time of every word, for `audio` and `captions` nodes
  ultrafast record <doc.json> <script.json> <out.mp4> [--size WxH] [--duration S] [--no-log]
                                             play the document while applying a script of edits
                                             (a list of {at, edits, note}) and record the
                                             player: timeline, outlines of what changed, an edit log
  ultrafast beats  <audio or video> [--band all|low|mid|high]
                                             tempo, beat grid, downbeats and onsets (JSON): cut on the beat
  ultrafast plan   <doc.json>                    print the fused render plan
  ultrafast render <doc.json> [--at SECS] [--out frame.png] [--size 720p|1080p|4k|WxH | --scale S] [--alpha]
                                             one frame as a PNG, drawn sharp at any size, as the export
                                             shows it (black where nothing is drawn); --alpha keeps
                                             transparency
  ultrafast sheet  <doc.json> [--at T,T,...] [--count N] [--columns C] [--out sheet.png]
                                             labeled contact sheet of frames, to review motion at a glance
  ultrafast frames <doc.json> --out-dir DIR [--every N] [--size 720p|1080p|4k|WxH | --scale S] [--alpha]
                                             every Nth frame as a PNG, like `render` (--alpha keeps
                                             transparency)
  ultrafast edit   <doc.json> <edits.json> [--out doc.json]
                                             apply an edit batch, print change kind
  ultrafast inspect <doc.json> --at T [--point X,Y]
                                             what is on screen at T: each element's document node,
                                             frame bounds and (for text) its words (JSON); with
                                             --point, the topmost element there
  ultrafast profile <doc.json> [--frames N] [--from T] [--json]
                                             GPU time per render group (which nodes cost what)
  ultrafast estimate <doc.json> [--from S] [--to S] [--samples N] [--json] [export flags: --size, --quality, --fps...]
                                             what exporting it costs: rendered minutes (at the output
                                             size), GPU seconds (timed on sampled frames on this GPU,
                                             with the quality level's motion blur) and the time to the
                                             first frame
  ultrafast bench  <doc.json> [--frames N] [--live-edits]
                                             GPU throughput, no readback
  ultrafast export <doc.json> <out> [--quality draft|standard|high|master] [--size 720p|1080p|4k|WxH | --scale S]
               [--fps N] [--format F] [--codec h264|hevc|av1|vp9|prores] [--prores-profile P] [--alpha]
               [--crf N | --bitrate 8M] [--audio-codec C] [--audio-bitrate 192k] [--no-audio] [--loops N]
               [--from S] [--to S] [--workers N]
                                             encode the composition; the format follows <out>: .mp4 .mov .mkv
                                             .webm .gif, a folder/ or frames/{frame:05}.png|.exr for a
                                             sequence, .wav .mp3 .m4a .flac .opus for sound alone. H.264 and
                                             HEVC hardware-encode with zero readback
  ultrafast serve  [--port 8080] [--host ::]
                                             the self-hosted render service on this machine: the
                                             v1 HTTP API (render, uploads, frames, sheets, check,
                                             review, estimate) and remote MCP at /mcp; one API
                                             key; storage on local disk (docs/service/SELF_HOST.md).
                                             :: is every address, IPv4 and IPv6; --host localhost
                                             this machine only
  ultrafast serve key                        print a new API key
  ultrafast batch <doc.json> --data rows.csv|rows.json|rows.jsonl [--out 'out/{name}.mp4'] [--docs DIR]
                                             one document, a video per row: columns named node.param
                                             (or format.width...) set that param, other columns fill
                                             {column} in --out ({row}: the row number); --docs writes
                                             each row's document to DIR instead of rendering, its
                                             relative file paths rewritten to resolve from there
  ultrafast batch                                exports `<doc.json> <out.mp4>` per stdin line with one
                                             warm renderer (a preview service's loop)
  ultrafast warm   <doc.json>... [--step S]      compile and cache every kernel and pipeline the documents
                                             need (frames every S seconds, default 0.5; nothing encoded):
                                             what a render image bakes so cold instances start compiled
  ultrafast faces   <video> [--from S] [--to S] [--out faces.json]
                                             face tracks, who is talking, and shots: what a
                                             `clip_camera` node follows (it tracks them itself when
                                             it has no `faces`)
  ultrafast probe   <video> [--sheet shots.png]  format, shots (cuts/fades), contact sheet
  ultrafast extract <video> --at T[,T...] --out-dir DIR
                                             reference frames as PNGs
  ultrafast compare <doc.json> <video> [--at T,...] [--out-dir DIR] [--worst N] [--offset S]
                                             PSNR/SSIM per frame, diffs of the worst
  ultrafast fit     <doc.json> <video> --param node.param[:step] ... --at T,... [--iters N] [--out fitted.json]
                                             fit static numeric params to the reference
  ultrafast fit-motion <doc.json> <video> --param node.param[:step] --from S --to S [--every N] [--tolerance X] [--out f.json]
                                             fit an animated param frame by frame, simplified to keyframes
  ultrafast ocr     <video> --at T [--draft draft.json]
                                             recognize text (Apple Vision, or Tesseract) and draft text layers
  ultrafast match-font <doc.json> <video> --node ID|all --at T [--screen N] [--fonts DIR]
                   [--font NAME:WEIGHT[:italic]] [--ink X0,Y0,X1,Y1] [--background bg.png] [--out f.json]
                                             find the font, size, baseline, tracking and color of text nodes
                                             (--font: a known face, fit only size/place/tracking/color;
                                             --ink: the line's ink box, when neighbors touch it)
  ultrafast plate   <video> [--from S] [--to S] [--doc d.json --bg NODE] [--samples N] [--image plate.png] [--out d2.json]
                                             background plate (temporal median, layers inpainted) + gradient fits
  ultrafast reconstruct <video> --out-dir DIR    whole-video reconstruction: shots, text layers with matched
                                             fonts, backgrounds, fades -> DIR/reconstruction.json
  ultrafast matte   <video> <out>               subject mattes (Apple Vision) as PNG frames
  ultrafast live   <doc.json> [--port 7878] [--headless] [--compare ref.mp4]
                                             realtime preview + JSON-lines edit server;
                                             reloads the file when it changes; --compare
                                             shows a reference video split against it
  ultrafast send   '<json request>' [--port 7878]
                                             send one request to a live session
                                             (reads requests from stdin if none given)";

/// The usage of `cmd` (each of its forms, with their notes), or all of it
/// when `cmd` is not a command.
fn usage_for(cmd: &str) -> String {
    let head = format!("ultrafast {cmd}");
    let mut lines = Vec::new();
    let mut keep = false;
    for line in USAGE.lines().skip_while(|l| !l.starts_with("usage:")).skip(1) {
        let t = line.trim_start();
        if t.starts_with("ultrafast ") {
            keep = t
                .strip_prefix(&head)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
        }
        if keep {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        USAGE.to_string()
    } else {
        format!("usage:\n{}", lines.join("\n"))
    }
}

fn main() -> Result<()> {
    quiet_on_closed_stdout();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    match cmd.as_str() {
        "--version" | "-V" | "version" => {
            println!("ultrafast {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage_for(cmd));
        return Ok(());
    }
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    // Positional arguments: everything that is not a flag or a flag's value.
    let pos: Vec<String> = {
        const BOOL_FLAGS: &[&str] = &[
            "--headless",
            "--live-edits",
            "--no-log",
            "--no-audio",
            "--alpha",
            "--json",
            "--cloud",
            "--markdown",
            "--index",
        ];
        let mut v = Vec::new();
        let mut i = 0;
        while i < args.len() {
            if args[i].starts_with("--") && args[i].len() > 2 {
                i += if BOOL_FLAGS.contains(&args[i].as_str()) { 1 } else { 2 };
            } else {
                v.push(args[i].clone());
                i += 1;
            }
        }
        v
    };
    let doc_path = || pos.get(1).map(PathBuf::from).context("missing <doc.json>");

    match cmd.as_str() {
        "mcp" => mcp::serve()?,
        "new" => {
            let name = pos
                .get(1)
                .context("missing template (vertical, title, square, promo, explainer or product)")?;
            let out = PathBuf::from(pos.get(2).context("missing <out.json>")?);
            let src = ultrafast_ops::templates::get(name).with_context(|| {
                format!(
                    "unknown template `{name}` ({})",
                    ultrafast_ops::templates::NAMES.join(", ")
                )
            })?;
            if out.exists() {
                bail!("{} exists; not overwriting", out.display());
            }
            std::fs::write(&out, src)?;
            println!("wrote {} (ultrafast check / sheet / live / export it)", out.display());
        }
        "info" => {
            let d = Device::new()?;
            println!("backend: {}\ndevice:  {}", d.backend(), d.name());
        }
        "ops" => {
            let vocab = serde_json::to_value(ultrafast_render::schema::vocabulary())?;
            if pos.len() > 1 {
                // `uf ops text captions`: just those ops, as Markdown.
                let want: Vec<&str> = pos[1..].iter().map(String::as_str).collect();
                let (md, missing) = ultrafast_mcp::core::ops_detail(&vocab, &want);
                if missing.len() < want.len() {
                    print!("{md}");
                }
                if !missing.is_empty() {
                    bail!("not ops: {}; `uf ops --index` lists every op", missing.join(", "));
                }
            } else if has("--index") {
                // Every op on one line: what an agent reads first.
                print!("{}", ultrafast_mcp::core::ops_index(&vocab, "uf ops text block"));
            } else if args.iter().any(|a| a == "--markdown") {
                // One line per op and per param: a compact reference for an
                // agent's context (docs/OPS.md).
                let mut md = String::from(
                    "# ultrafast ops\n\nEvery op, its input ports and its params (type = default: what it does). Generated by `ultrafast ops --markdown`.\n",
                );
                for op in vocab.as_array().into_iter().flatten() {
                    let ports: Vec<&str> = op["ports"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p.as_str())
                        .collect();
                    md += &format!(
                        "\n## {}{}\n{}\n",
                        op["op"].as_str().unwrap_or(""),
                        if ports.is_empty() {
                            String::new()
                        } else {
                            format!("({})", ports.join(", "))
                        },
                        op["summary"].as_str().unwrap_or("")
                    );
                    for p in op["params"].as_array().into_iter().flatten() {
                        let default = p["default"].as_str().unwrap_or("");
                        md += &format!(
                            "- `{}`: {}{} {}\n",
                            p["name"].as_str().unwrap_or(""),
                            p["type"].as_str().unwrap_or(""),
                            if default.is_empty() {
                                String::new()
                            } else {
                                format!(" = {default}")
                            },
                            match p["doc"].as_str().unwrap_or("") {
                                "" => String::new(),
                                d => format!("- {d}"),
                            }
                        );
                    }
                }
                print!("{md}");
            } else {
                println!("{}", serde_json::to_string_pretty(&vocab)?);
            }
        }
        "schema" => {
            // The document format as JSON Schema, from the op vocabulary
            // (published as https://ultrafast.mov/schema/v1.json).
            println!(
                "{}",
                serde_json::to_string_pretty(&ultrafast_ops::json_schema::json_schema())?
            );
        }
        "check" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let mut issues: Vec<serde_json::Value> = ultrafast_render::schema::check(&doc)
                .into_iter()
                .map(|i| serde_json::json!({ "node": i.node, "severity": i.severity, "message": i.message }))
                .collect();
            // A key written twice parses as its last value, the first lost.
            for p in ultrafast_graph::duplicate_keys(&std::fs::read_to_string(&path)?) {
                let (node, what) = match p.strip_prefix("nodes.").and_then(|r| r.split_once('.')) {
                    Some((id, rest)) => (
                        id.to_string(),
                        match rest.strip_prefix("params.") {
                            Some(param) => format!("param `{param}`"),
                            None => format!("`{rest}`"),
                        },
                    ),
                    None => (String::new(), format!("`{p}`")),
                };
                issues.push(serde_json::json!({
                    "node": node,
                    "severity": "warning",
                    "message": format!("{what} is given twice: only the last one counts"),
                }));
            }
            // Render one frame to surface plan errors, shader compile errors
            // and missing files as well.
            let mut r = renderer(&path)?;
            match r.render_rgba8(&doc, 0.0) {
                Ok(_) => {}
                Err(e) => issues.push(serde_json::json!({ "node": "", "severity": "error", "message": e.to_string() })),
            }
            for d in r.diagnostics() {
                issues.push(serde_json::json!({ "node": d.node, "severity": d.severity(), "message": d.message }));
            }
            let errors = issues.iter().filter(|i| i["severity"] == "error").count();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "ok": errors == 0, "issues": issues }))?
            );
            if errors > 0 {
                // A failed render can leave kernels compiling on the
                // renderer's threads; `exit` skips drops and tears the
                // process down under them (glibc aborts on a corrupted
                // heap). Dropping the renderer joins those threads first.
                drop(r);
                std::process::exit(1);
            }
        }
        "outline" => {
            let params = pos.get(1).context("missing '<text params JSON>'")?;
            let node: ultrafast_graph::Node =
                serde_json::from_str(&format!(r#"{{"op":"text","params":{params}}}"#)).context("bad params JSON")?;
            let mut r = ultrafast_render::Renderer::new(Device::new()?)?;
            r.set_base_dir(Path::new(&flag("--base").unwrap_or_else(|| ".".into())));
            println!("{}", r.text_outline(&node)?);
        }
        "measure" => {
            let params = pos.get(1).context("missing '<text params JSON>'")?;
            let node: ultrafast_graph::Node =
                serde_json::from_str(&format!(r#"{{"op":"text","params":{params}}}"#)).context("bad params JSON")?;
            let mut r = ultrafast_render::Renderer::new(Device::new()?)?;
            r.set_base_dir(Path::new(&flag("--base").unwrap_or_else(|| ".".into())));
            println!("{}", serde_json::to_string_pretty(&r.measure_text(&node)?)?);
        }
        "plan" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let mut r = renderer(&path)?;
            r.submit_frame(&doc, 0.0, false)?.0.wait()?;
            print!("{}", r.plan().unwrap().describe());
            report_diagnostics(&r);
        }
        "render" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            // `--at` like every other command; `--t` still works.
            let t: f64 = flag("--at")
                .or_else(|| flag("--t"))
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(0.0);
            let out = flag("--out").unwrap_or_else(|| "frame.png".into());
            let size = still_size(&flag, &doc)?;
            let start = Instant::now();
            let mut r = renderer(&path)?;
            r.set_output_size(size);
            let (w, h) = r.frame_size(&doc);
            let setup = start.elapsed();
            // As the export shows it, unless asked to keep transparency.
            let (px, rep) = if has("--alpha") {
                r.render_rgba8(&doc, t)?
            } else {
                r.render_opaque_rgba8(&doc, t)?
            };
            let total = start.elapsed();
            write_png(Path::new(&out), w, h, &px)?;
            println!(
                "rendered t={t} {w}x{h}: {} kernels, setup {:.1} ms, total {:.1} ms -> {out}",
                rep.kernels_run,
                setup.as_secs_f64() * 1000.0,
                total.as_secs_f64() * 1000.0,
            );
            report_diagnostics(&r);
        }
        "sheet" => {
            let path = doc_path()?;
            let mut session = ultrafast_live::Session::load(&path).map_err(anyhow::Error::msg)?;
            let times: Vec<f64> = match flag("--at") {
                Some(at) => parse_times(&at)?,
                None => {
                    let n: usize = flag("--count").map(|s| s.parse()).transpose()?.unwrap_or(9).max(1);
                    let d = session.doc.format.duration;
                    (0..n).map(|i| d * (i as f64 + 0.5) / n as f64).collect()
                }
            };
            let fmt = session.doc.format;
            let default_cols = if fmt.height > fmt.width { 5 } else { 3 };
            let cols: usize = flag("--columns")
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(default_cols);
            let out = PathBuf::from(flag("--out").unwrap_or_else(|| "sheet.png".into()));
            let (w, h) = session
                .contact_sheet(&times, cols.max(1), &out)
                .map_err(anyhow::Error::msg)?;
            println!("{} frames -> {} ({w}x{h})", times.len(), out.display());
        }
        "record" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let script = std::fs::read_to_string(pos.get(2).context("missing <script.json>")?)?;
            let steps = ultrafast_live::record::parse_script(&script).map_err(anyhow::Error::msg)?;
            let out = PathBuf::from(pos.get(3).context("missing <out.mp4>")?);
            let (w, h) = match flag("--size") {
                Some(s) => {
                    let (a, b) = s.split_once('x').context("--size WxH")?;
                    (a.parse()?, b.parse()?)
                }
                None => (1280, 720),
            };
            let opts = ultrafast_live::record::RecordOptions {
                width: w,
                height: h,
                duration: flag("--duration")
                    .map(|s| s.parse())
                    .transpose()?
                    .unwrap_or(doc.format.duration),
                log: !has("--no-log"),
            };
            let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let start = Instant::now();
            ultrafast_live::record::record(doc, &base, &steps, &out, &opts, |i, n| {
                if i % 30 == 0 || i == n {
                    eprint!("\r  {i}/{n} frames");
                }
            })
            .map_err(anyhow::Error::msg)?;
            eprintln!();
            println!(
                "recorded {:.1} s with {} edit steps in {:.1} s -> {}",
                opts.duration,
                steps.len(),
                start.elapsed().as_secs_f64(),
                out.display()
            );
        }
        "beats" => {
            let file = PathBuf::from(pos.get(1).context("missing <audio>")?);
            let pcm = ultrafast_media::audio::decode(&file)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .context("no audio in that file")?;
            let r = ultrafast_media::beats::analyze(&pcm, flag("--band").as_deref().unwrap_or("all"));
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        "voiceover" => {
            let text = pos.get(1).context("missing \"<text>\"")?;
            let wav = pos.get(2).context("missing <out.wav>")?;
            let words = pos.get(3).context("missing <words.json>")?;
            let out = voiceover(
                text,
                Path::new(wav),
                Path::new(words),
                flag("--voice").as_deref(),
                flag("--rate").as_deref(),
            )?;
            println!("{out}");
        }
        "import-svg" => {
            let file = PathBuf::from(pos.get(1).context("missing <file.svg>")?);
            let data = std::fs::read(&file).with_context(|| file.display().to_string())?;
            let fit = flag("--size")
                .map(|s| parse_times(&s))
                .transpose()?
                .map(|v| match v.len() {
                    1 => [v[0], v[0]],
                    _ => [v[0], v[1]],
                });
            let imp = ultrafast_render::svg::import(&data, file.parent(), fit).map_err(anyhow::Error::msg)?;
            for w in &imp.warnings {
                eprintln!("warning: {w}");
            }
            let name = flag("--name").unwrap_or_else(|| {
                file.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "svg".into())
            });
            match flag("--into") {
                Some(doc_file) => {
                    let mut doc = load(Path::new(&doc_file))?;
                    doc.components.insert(name.clone(), imp.component);
                    std::fs::write(&doc_file, serde_json::to_string_pretty(&doc)?)?;
                    println!(
                        "added component `{name}` ({:.0}x{:.0} px) to {doc_file}: place it with {{\"op\": \"{name}\", \"params\": {{\"position\": [x, y]}}}}",
                        imp.size[0], imp.size[1]
                    );
                }
                None => println!("{}", serde_json::to_string_pretty(&imp.component)?),
            }
        }
        "import-lottie" => {
            let file = PathBuf::from(pos.get(1).context("missing <animation.json>")?);
            let data = std::fs::read(&file).with_context(|| file.display().to_string())?;
            let fit = flag("--size")
                .map(|s| parse_times(&s))
                .transpose()?
                .map(|v| match v.len() {
                    1 => [v[0], v[0]],
                    _ => [v[0], v[1]],
                });
            let mut fonts = ultrafast_render::geom::Fonts::new();
            let imp =
                ultrafast_render::lottie::import(&data, file.parent(), fit, &mut fonts).map_err(anyhow::Error::msg)?;
            for w in &imp.warnings {
                eprintln!("warning: {w}");
            }
            let name = flag("--name").unwrap_or_else(|| {
                file.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "lottie".into())
            });
            match flag("--into") {
                Some(doc_file) => {
                    let mut doc = load(Path::new(&doc_file))?;
                    doc.components.insert(name.clone(), imp.component);
                    std::fs::write(&doc_file, serde_json::to_string_pretty(&doc)?)?;
                    println!(
                        "added component `{name}` ({:.0}x{:.0} px, {:.2} s) to {doc_file}: place it with {{\"op\": \"{name}\", \"params\": {{\"position\": [x, y]}}, \"timing\": {{\"loop\": {:.3}}}}}",
                        imp.size[0], imp.size[1], imp.duration, imp.duration
                    );
                }
                None => println!("{}", serde_json::to_string_pretty(&imp.component)?),
            }
        }
        "review" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let mut r = renderer(&path)?;
            let opts = ultrafast_render::review::ReviewOptions {
                step: flag("--step").map(|s| s.parse()).transpose()?.unwrap_or(0.1),
                safe: match flag("--safe").as_deref() {
                    None | Some("margin") => ultrafast_render::review::SafeArea::Margin(0.04),
                    Some("social") => ultrafast_render::review::SafeArea::Social,
                    Some(o) => bail!("unknown --safe `{o}` (margin, social)"),
                },
            };
            let review = ultrafast_render::review::review(&mut r, &doc, &opts)?;
            println!("{}", serde_json::to_string_pretty(&review)?);
            report_diagnostics(&r);
        }
        "frames" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let dir = PathBuf::from(flag("--out-dir").context("missing --out-dir")?);
            let every: u64 = flag("--every").map(|s| s.parse()).transpose()?.unwrap_or(1).max(1);
            std::fs::create_dir_all(&dir)?;
            let size = still_size(&flag, &doc)?;
            let mut r = renderer(&path)?;
            r.set_output_size(size);
            let (w, h) = r.frame_size(&doc);
            let n = doc.format.frame_count();
            let alpha = has("--alpha");
            for f in (0..n).step_by(every as usize) {
                let t = doc.format.time_of(f);
                let (px, _) = if alpha {
                    r.render_rgba8(&doc, t)?
                } else {
                    r.render_opaque_rgba8(&doc, t)?
                };
                write_png(&dir.join(format!("{f:05}.png")), w, h, &px)?;
            }
            println!("wrote {} frames to {}", n.div_ceil(every), dir.display());
            report_diagnostics(&r);
        }
        "edit" => {
            let mut doc = load(&doc_path()?)?;
            let edits_path = pos.get(2).context("missing <edits.json>")?;
            let edits: Vec<Edit> =
                ultrafast_graph::parse_edits(&serde_json::from_str(&std::fs::read_to_string(edits_path)?)?)
                    .map_err(anyhow::Error::msg)?;
            let change = doc.apply(&edits)?;
            println!("{change:?}");
            if let Some(out) = flag("--out") {
                std::fs::write(out, doc.to_json())?;
            }
        }
        "inspect" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let t: f64 = flag("--at").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
            let mut r = renderer(&path)?;
            r.render_rgba8(&doc, t)?;
            if let Some(pt) = flag("--point") {
                // What is drawn on top at one frame point.
                let v: Vec<f32> = pt.split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(v.len() == 2, "--point takes x,y");
                let hit = r.pick(&doc, t, v[0], v[1]);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "t": t, "point": v, "picked": hit }))?
                );
                return Ok(());
            }
            let items = r.visible(&doc, t);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "t": t, "visible": items }))?
            );
        }
        "profile" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let frames: u64 = flag("--frames").map(|s| s.parse()).transpose()?.unwrap_or(90);
            let from: f64 = flag("--from").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
            profile(&path, doc, frames, from, has("--json"))?;
        }
        "estimate" => {
            let path = doc_path()?;
            if has("--cloud") {
                bail!(
                    "ultrafast Cloud isn't open yet: `uf estimate {}` (without --cloud) measures the render on this GPU",
                    path.display()
                );
            }
            let doc = load(&path)?;
            let t0 = Instant::now();
            let mut r = renderer(&path)?;
            let setup = t0.elapsed().as_secs_f64();
            let opts = ultrafast_live::estimate::Options {
                from: flag("--from").map(|s| s.parse()).transpose()?,
                to: flag("--to").map(|s| s.parse()).transpose()?,
                samples: flag("--samples").map(|s| s.parse()).transpose()?.unwrap_or(64),
                // Priced and timed as `export` with the same flags renders.
                target: Some(
                    export_settings(&flag, &has)?
                        .resolve(None, &doc.format)
                        .map_err(anyhow::Error::msg)?,
                ),
                ..Default::default()
            };
            let mut est = ultrafast_live::estimate::estimate(&mut r, &doc, &opts).map_err(anyhow::Error::msg)?;
            // A fresh process also starts the GPU and the engine's own kernels.
            est.first_byte_s += setup;
            if has("--json") {
                println!("{}", serde_json::to_string_pretty(&est)?);
            } else {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("{}", est.text(&name));
            }
            report_diagnostics(&r);
        }
        "bench" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let frames: u64 = flag("--frames").map(|s| s.parse()).transpose()?.unwrap_or(600);
            bench(&path, doc, frames, has("--live-edits"))?;
        }
        "export" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let out = PathBuf::from(
                flag("--out")
                    .or_else(|| pos.get(2).cloned())
                    .context("missing output path")?,
            );
            let opts = ultrafast_live::export::ExportOptions {
                settings: export_settings(&flag, &has)?,
                start: flag("--from").map(|s| s.parse()).transpose()?.unwrap_or(0.0),
                end: flag("--to").map(|s| s.parse()).transpose()?,
                deadline: None,
            };
            // Every combination is checked before anything renders.
            let target = opts
                .settings
                .resolve(Some(&out), &doc.format)
                .map_err(anyhow::Error::msg)?;
            // The encoder's driver starts while the GPU does.
            ultrafast_live::export::prewarm(&target);
            if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            let workers: usize = flag("--workers").map(|s| s.parse()).transpose()?.unwrap_or(1);
            let mut last = Instant::now();
            let mut tick = |done: u64, total: u64| {
                if last.elapsed().as_secs_f64() > 1.0 || done == total {
                    last = Instant::now();
                    eprintln!("  {done}/{total} frames");
                }
            };
            if workers > 1 {
                // A queue for each worker's fork, besides this handle's own.
                let device = Device::with_lanes(workers + 1)?;
                let rep = ultrafast_live::export::export_parallel(&device, &path, &doc, &out, opts, workers, tick)
                    .map_err(anyhow::Error::msg)?;
                println!(
                    "exported {} with {workers} workers, {} readbacks -> {}",
                    export_summary(&rep),
                    rep.readbacks,
                    out.display()
                );
                println!("  {} (summed over workers)", rep.timing.summary(rep.frames));
                print_pipelines(&device);
                return Ok(());
            }
            let mut r = renderer(&path)?;
            let rep = ultrafast_live::export::export_target(&mut r, &doc, &out, target, opts, &mut tick)
                .map_err(anyhow::Error::msg)?;
            println!(
                "exported {}, {} readbacks -> {}",
                export_summary(&rep),
                rep.readbacks,
                out.display()
            );
            if rep.frames > 0 {
                println!("  {}", rep.timing.summary(rep.frames));
            }
            report_diagnostics(&r);
            print_pipelines(r.device());
        }
        "serve" if pos.get(1).is_some_and(|p| p == "key") => serve::new_key(),
        "serve" => {
            let port: Option<u16> = flag("--port").map(|s| s.parse()).transpose()?;
            serve::run(port, flag("--host"))?;
        }
        "batch" if pos.len() > 1 => {
            // One document, a video per row of a data file: each row sets
            // `node.param` columns (see ultrafast_ops::batch).
            use ultrafast_ops::batch;
            let path = doc_path()?;
            let doc = load(&path)?;
            let data = PathBuf::from(flag("--data").context("missing --data rows.csv|rows.json|rows.jsonl")?);
            let rows = batch::read_rows(&data).map_err(anyhow::Error::msg)?;
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "film".into());
            let pattern = flag("--out").unwrap_or_else(|| format!("{stem}-{{row}}.mp4"));
            let settings = export_settings(&flag, &has)?;
            // Every row's document first: a mistake in row 40 shows before
            // anything renders.
            let mut jobs = Vec::new();
            // Each warning once, with the first row that has it: a row's
            // own (a word in a number column, say), not the template's.
            let mut warned: std::collections::HashSet<(String, String)> = ultrafast_render::schema::check(&doc)
                .into_iter()
                .map(|i| (i.node, i.message))
                .collect();
            for (i, row) in rows.iter().enumerate() {
                let d = batch::apply_row(&doc, row).map_err(|e| anyhow::anyhow!("row {}: {e}", i + 1))?;
                let (errors, warnings): (Vec<_>, Vec<_>) = ultrafast_render::schema::check(&d)
                    .into_iter()
                    .filter(|i| i.severity != "info")
                    .partition(|i| i.severity == "error");
                if !errors.is_empty() {
                    let errors: Vec<String> = errors.iter().map(|e| format!("{}: {}", e.node, e.message)).collect();
                    bail!("row {}: {}", i + 1, errors.join("; "));
                }
                for w in warnings {
                    if warned.insert((w.node.clone(), w.message.clone())) {
                        eprintln!("warning: row {}: node `{}`: {}", i + 1, w.node, w.message);
                    }
                }
                let out = PathBuf::from(batch::output_name(&pattern, row, i));
                settings
                    .resolve(Some(&out), &d.format)
                    .map_err(|e| anyhow::anyhow!("row {}: {e}", i + 1))?;
                if jobs.iter().any(|(_, o): &(Document, PathBuf)| *o == out) {
                    bail!(
                        "row {}: writes {} again (put {{row}} or a column in --out)",
                        i + 1,
                        out.display()
                    );
                }
                jobs.push((d, out));
            }
            if let Some(dir) = flag("--docs") {
                // Write each row's document instead of rendering, as the
                // render would read it: relative paths rewritten to resolve
                // from DIR, and the template's `meta` (font pins, script
                // fonts), which a Document doesn't hold, carried along.
                let dir = PathBuf::from(dir);
                std::fs::create_dir_all(&dir)?;
                let from = path.parent().unwrap_or(Path::new(""));
                let mut extra = serde_json::Map::new();
                if let Ok(serde_json::Value::Object(top)) = serde_json::from_str(&std::fs::read_to_string(&path)?) {
                    extra.extend(top.into_iter().filter(|(k, _)| {
                        !["version", "format", "output", "nodes", "components"].contains(&k.as_str())
                    }));
                }
                let fonts = extra
                    .get_mut("meta")
                    .and_then(|m| m.get_mut("fonts"))
                    .and_then(|f| f.as_array_mut());
                for font in fonts.into_iter().flatten() {
                    if let Some(serde_json::Value::String(src)) = font.get_mut("src")
                        && let Some(r) = ultrafast_service::refs::rebase_path(src, from, &dir)
                    {
                        *src = r;
                    }
                }
                #[derive(serde::Serialize)]
                struct Written<'a> {
                    #[serde(flatten)]
                    doc: &'a Document,
                    #[serde(flatten)]
                    extra: &'a serde_json::Map<String, serde_json::Value>,
                }
                for (d, out) in &jobs {
                    let name = out
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut d = d.clone();
                    ultrafast_service::refs::rebase_paths(&mut d, from, &dir);
                    let p = dir.join(format!("{name}.json"));
                    std::fs::write(&p, serde_json::to_string_pretty(&Written { doc: &d, extra: &extra })?)?;
                    println!("{}", p.display());
                }
                return Ok(());
            }
            let mut r = renderer(&path)?;
            let t0 = Instant::now();
            let n = jobs.len();
            for (i, (d, out)) in jobs.iter().enumerate() {
                if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir)?;
                }
                let t1 = Instant::now();
                let opts = ultrafast_live::export::ExportOptions::with(settings);
                let rep = serve::guarded(&mut r, |r| {
                    ultrafast_live::export::export_video(r, d, out, opts, |_, _| {}).map_err(anyhow::Error::msg)
                })
                .with_context(|| format!("row {}", i + 1))?;
                println!(
                    "{}/{n} {}: {} frames in {:.2} s",
                    i + 1,
                    out.display(),
                    rep.frames,
                    t1.elapsed().as_secs_f64()
                );
                for d in r.diagnostics() {
                    if warned.insert((d.node.clone(), d.message.clone())) {
                        eprintln!("warning: row {}: node `{}`: {}", i + 1, d.node, d.message);
                    }
                }
                r.clear_diagnostics();
            }
            eprintln!("{n} videos in {:.1} s", t0.elapsed().as_secs_f64());
        }
        "batch" => {
            // A warm renderer serving export requests, one per stdin line:
            // `<doc.json> <out.mp4>`. Kernels, pipelines and fonts stay
            // loaded between requests (what a preview service runs); each
            // document's own resources are let go after it.
            use std::io::BufRead;
            let mut r = Renderer::new(Device::new()?)?;
            let ready = Instant::now();
            println!("ready: {}", r.device().name());
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                let mut it = line.split_whitespace();
                let (Some(doc_path), Some(out)) = (it.next(), it.next()) else {
                    continue;
                };
                let t0 = Instant::now();
                let doc_path = PathBuf::from(doc_path);
                let result = load(&doc_path).and_then(|doc| {
                    r.set_base_dir(doc_path.parent().unwrap_or(Path::new(".")));
                    serve::guarded(&mut r, |r| {
                        ultrafast_live::export::export_video(r, &doc, Path::new(out), Default::default(), |_, _| {})
                            .map_err(anyhow::Error::msg)
                    })
                });
                match result {
                    Ok(rep) => println!(
                        "{} -> {}: {} frames, render {:.2} s ({:.0} fps), request {:.2} s",
                        doc_path.display(),
                        out,
                        rep.frames,
                        rep.seconds,
                        rep.fps,
                        t0.elapsed().as_secs_f64()
                    ),
                    Err(e) => println!("{}: error: {e}", doc_path.display()),
                }
                r.release_document();
            }
            print_pipelines(r.device());
            eprintln!("served for {:.1} s", ready.elapsed().as_secs_f64());
        }
        "warm" => {
            let step: f64 = flag("--step").map(|v| v.parse()).transpose()?.unwrap_or(0.5);
            anyhow::ensure!(step > 0.0, "--step must be positive");
            let mut r = Renderer::new(Device::new()?)?;
            let t0 = Instant::now();
            for doc_path in pos.iter().skip(1).map(PathBuf::from) {
                let t1 = Instant::now();
                let result = load(&doc_path).and_then(|doc| {
                    r.set_base_dir(doc_path.parent().unwrap_or(Path::new(".")));
                    serve::guarded(&mut r, |r| {
                        let mut frames = 0;
                        let mut t = 0.0;
                        while t < doc.format.duration {
                            r.submit_frame(&doc, t, false)?.0.wait()?;
                            frames += 1;
                            t += step;
                        }
                        Ok(frames)
                    })
                });
                match result {
                    Ok(n) => println!(
                        "{}: {n} frames in {:.2} s",
                        doc_path.display(),
                        t1.elapsed().as_secs_f64()
                    ),
                    Err(e) => println!("{}: error: {e}", doc_path.display()),
                }
                r.release_document();
            }
            print_pipelines(r.device());
            println!("warmed in {:.1} s", t0.elapsed().as_secs_f64());
        }
        "faces" => {
            // Face tracks of a video, as `clip_camera` reads them (it runs
            // the same tracking itself when a node has no `faces`).
            let video = PathBuf::from(pos.get(1).context("missing <video>")?);
            let from: f64 = flag("--from").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
            let to: f64 = flag("--to").map(|s| s.parse()).transpose()?.unwrap_or(f64::INFINITY);
            let t0 = Instant::now();
            let v =
                ultrafast_render::autofaces::track(&Device::new()?, &video, from, to).map_err(anyhow::Error::msg)?;
            let ids: std::collections::BTreeSet<i64> = v["frames"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|f| {
                    f["faces"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|b| b["id"].as_i64())
                })
                .collect();
            let text = serde_json::to_string(&v)?;
            match flag("--out") {
                Some(out) => {
                    std::fs::write(&out, &text)?;
                    eprintln!(
                        "{out}: {} faces tracked over {} frames, {} shots ({:.1} s)",
                        ids.len(),
                        v["frames"].as_array().map_or(0, |a| a.len()),
                        v["shots"].as_array().map_or(0, |a| a.len()),
                        t0.elapsed().as_secs_f64()
                    );
                }
                None => println!("{text}"),
            }
        }
        "probe" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let sheet = flag("--sheet").map(PathBuf::from);
            let out = ultrafast_recon::probe(&Device::new()?, &video, sheet.as_deref()).map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        "extract" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let times = parse_times(&flag("--at").context("missing --at")?)?;
            let dir = PathBuf::from(flag("--out-dir").unwrap_or_else(|| "frames".into()));
            for p in ultrafast_recon::extract(&Device::new()?, &video, &times, &dir).map_err(anyhow::Error::msg)? {
                println!("{}", p.display());
            }
        }
        "compare" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let video = pos.get(2).map(PathBuf::from).context("missing <video>")?;
            let opts = ultrafast_recon::CompareOptions {
                times: flag("--at").map(|s| parse_times(&s)).transpose()?.unwrap_or_default(),
                out_dir: flag("--out-dir").map(PathBuf::from),
                worst: flag("--worst").map(|s| s.parse()).transpose()?.unwrap_or(3),
                offset: flag("--offset").map(|s| s.parse()).transpose()?.unwrap_or(0.0),
            };
            let base = path.parent().unwrap_or(Path::new("."));
            let out =
                ultrafast_recon::compare(&Device::new()?, &doc, base, &video, &opts).map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        "fit" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let video = pos.get(2).map(PathBuf::from).context("missing <video>")?;
            let params: Vec<ultrafast_recon::FitParam> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| *a == "--param")
                .filter_map(|(i, _)| args.get(i + 1))
                .map(|s| ultrafast_recon::FitParam::parse(s))
                .collect::<Result<_, _>>()
                .map_err(anyhow::Error::msg)?;
            if params.is_empty() {
                bail!("give at least one --param node.param");
            }
            let times = parse_times(&flag("--at").context("missing --at")?)?;
            let iters = flag("--iters").map(|s| s.parse()).transpose()?.unwrap_or(150);
            let base = path.parent().unwrap_or(Path::new("."));
            let (fitted, report) = ultrafast_recon::fit(&Device::new()?, &doc, base, &video, &params, &times, iters)
                .map_err(anyhow::Error::msg)?;
            if let Some(out) = flag("--out") {
                std::fs::write(&out, fitted.to_json())?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "match-font" => {
            let path = doc_path()?;
            let mut doc = load(&path)?;
            let video = pos.get(2).map(PathBuf::from).context("missing <video>")?;
            let t: f64 = flag("--at").context("missing --at")?.parse()?;
            let which = flag("--node").context("missing --node (an id, or `all`)")?;
            let mut opts = ultrafast_recon::fonts::MatchOptions::default();
            if let Some(n) = flag("--screen") {
                opts.screen = n.parse()?;
            }
            if let Some(bg) = flag("--background") {
                // What is behind the text, when known (an image the frame's
                // size): coverage is measured against it.
                let img = ultrafast_recon::metrics::Frame::read_png(Path::new(&bg)).map_err(anyhow::Error::msg)?;
                opts.background = Some(std::sync::Arc::new(img.rgba));
            }
            if let Some(b) = flag("--ink") {
                // The line's ink box, when neighbors would run into it.
                let v: Vec<f64> = b.split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(v.len() == 4, "--ink takes x0,y0,x1,y1");
                opts.ink = Some([v[0], v[1], v[2], v[3]]);
            }
            if let Some(f) = flag("--font") {
                // A known face: only its size, baseline, tracking and color.
                let mut parts = f.split(':');
                let font = parts.next().unwrap_or("").to_string();
                let weight = parts.next().map(|w| w.parse()).transpose()?.unwrap_or(400);
                let italic = parts.next().is_some_and(|s| s == "italic");
                opts.only = Some((font, weight, italic));
            }
            opts.font_dirs = args
                .iter()
                .enumerate()
                .filter(|(_, a)| *a == "--fonts")
                .filter_map(|(i, _)| args.get(i + 1).map(PathBuf::from))
                .collect();
            let nodes: Vec<String> = if which == "all" {
                let mut v: Vec<String> = doc
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.op == "text")
                    .map(|(id, _)| id.clone())
                    .collect();
                v.sort();
                v
            } else {
                which.split(',').map(str::to_string).collect()
            };
            let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let device = Device::new()?;
            let mut reports = Vec::new();
            for id in &nodes {
                match ultrafast_recon::fonts::match_font(&device, &doc, &base, &video, id, t, &opts) {
                    Ok((matched, report)) => {
                        doc = matched;
                        reports.push(report);
                    }
                    Err(e) => reports.push(serde_json::json!({ "node": id, "error": e })),
                }
            }
            if let Some(out) = flag("--out") {
                std::fs::write(&out, doc.to_json())?;
            }
            println!("{}", serde_json::to_string_pretty(&serde_json::json!(reports))?);
        }
        "plate" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let mut opts = ultrafast_recon::plate::PlateOptions::default();
            if let Some(v) = flag("--from") {
                opts.from = v.parse()?;
            }
            if let Some(v) = flag("--to") {
                opts.to = v.parse()?;
            }
            if let Some(v) = flag("--samples") {
                opts.samples = v.parse()?;
            }
            if let Some(v) = flag("--threshold") {
                opts.threshold = v.parse()?;
            }
            opts.background = flag("--bg");
            let image = PathBuf::from(flag("--image").unwrap_or_else(|| "plate.png".into()));
            let doc = flag("--doc")
                .map(|p| -> Result<_> { Ok((load(Path::new(&p))?, PathBuf::from(p))) })
                .transpose()?;
            let base = doc
                .as_ref()
                .map(|(_, p)| p.parent().unwrap_or(Path::new(".")).to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            if opts.background.is_some() && doc.is_none() {
                bail!("--bg needs --doc");
            }
            let (out_doc, report) = ultrafast_recon::plate::plate(
                &Device::new()?,
                &video,
                doc.as_ref().map(|(d, _)| (d, base.as_path())),
                &image,
                &opts,
            )
            .map_err(anyhow::Error::msg)?;
            if let (Some(out), Some(d)) = (flag("--out"), out_doc) {
                std::fs::write(&out, d.to_json())?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "decompose" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let dir = PathBuf::from(flag("--out-dir").context("missing --out-dir")?);
            let mut opts = ultrafast_recon::layers::LayerOptions::default();
            if let Some(t) = flag("--threshold") {
                opts.threshold = t.parse()?;
            }
            if let Some(c) = flag("--colors") {
                opts.max_colors = c.parse()?;
            }
            let start = Instant::now();
            let (_, mut report) = ultrafast_recon::layers::decompose(&Device::new()?, &video, &dir, &opts, |msg| {
                eprintln!("[{:7.2}s] {msg}", start.elapsed().as_secs_f64())
            })
            .map_err(anyhow::Error::msg)?;
            report["seconds"] = serde_json::json!(start.elapsed().as_secs_f64());
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "reconstruct" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let dir = PathBuf::from(flag("--out-dir").context("missing --out-dir")?);
            println!("{}", serde_json::to_string_pretty(&reconstruct_impl(&video, &dir)?)?);
        }
        "fit-motion" => {
            let path = doc_path()?;
            let doc = load(&path)?;
            let video = pos.get(2).map(PathBuf::from).context("missing <video>")?;
            let param = ultrafast_recon::FitParam::parse(&flag("--param").context("missing --param")?)
                .map_err(anyhow::Error::msg)?;
            let from: f64 = flag("--from").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
            let to: f64 = flag("--to")
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(doc.format.duration);
            let every: u64 = flag("--every").map(|s| s.parse()).transpose()?.unwrap_or(1).max(1);
            let tol: f64 = flag("--tolerance").map(|s| s.parse()).transpose()?.unwrap_or(1.0);
            let iters = flag("--iters").map(|s| s.parse()).transpose()?.unwrap_or(60);
            let f0 = (from * doc.format.fps).round() as u64;
            let f1 = (to * doc.format.fps).round() as u64;
            let times: Vec<f64> = (f0..=f1)
                .step_by(every as usize)
                .map(|f| doc.format.time_of(f))
                .collect();
            let base = path.parent().unwrap_or(Path::new("."));
            let (fitted, report) =
                ultrafast_recon::fit_motion(&Device::new()?, &doc, base, &video, &param, &times, iters, tol)
                    .map_err(anyhow::Error::msg)?;
            if let Some(out) = flag("--out") {
                std::fs::write(&out, fitted.to_json())?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "ocr" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let t: f64 = flag("--at").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
            let (report, draft) = ocr_impl(&video, t)?;
            if let Some(out) = flag("--draft") {
                std::fs::write(&out, draft)?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "matte" => {
            let video = pos.get(1).map(PathBuf::from).context("missing <video>")?;
            let out = pos.get(2).map(PathBuf::from).context("missing <out>")?;
            println!("{}", serde_json::to_string_pretty(&matte_impl(&video, &out)?)?);
        }
        "live" => {
            let path = doc_path()?;
            let port = flag("--port").unwrap_or_else(|| "7878".into());
            let addr = format!("127.0.0.1:{port}");
            let mut session = ultrafast_live::Session::load(&path).map_err(anyhow::Error::msg)?;
            if let Some(v) = flag("--compare") {
                // A reference video beside the document (`c` cycles split,
                // difference, off).
                let v = std::fs::canonicalize(&v).with_context(|| format!("reference video {v}"))?;
                session.set_compare(Some((v, ultrafast_live::CompareMode::Split(0.5))));
            }
            let rx = ultrafast_live::server::serve(&addr).with_context(|| format!("binding {addr}"))?;
            eprintln!("ultrafast live: serving {} on {addr}", path.display());
            if has("--headless") {
                ultrafast_live::run_headless(session, rx);
            } else {
                run_window(session, rx, &path)?;
            }
        }
        "send" => {
            let port = flag("--port").unwrap_or_else(|| "7878".into());
            let addr = format!("127.0.0.1:{port}");
            let send = |line: &str| -> Result<()> {
                let req: serde_json::Value = serde_json::from_str(line).context("request is not JSON")?;
                let resp =
                    ultrafast_live::server::send(&addr, &req).with_context(|| format!("connecting to {addr}"))?;
                println!("{resp}");
                Ok(())
            };
            match pos.get(1) {
                Some(line) => send(line)?,
                None => {
                    for line in std::io::stdin().lines() {
                        let line = line?;
                        if !line.trim().is_empty() {
                            send(&line)?;
                        }
                    }
                }
            }
        }
        _ => {
            println!("{USAGE}");
            bail!("unknown command `{cmd}`");
        }
    }
    Ok(())
}

#[cfg(any(all(target_os = "macos", not(feature = "vulkan")), windows))]
fn run_window(
    session: ultrafast_live::Session,
    rx: std::sync::mpsc::Receiver<ultrafast_live::server::Request>,
    path: &Path,
) -> Result<()> {
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ultrafast_live::window::run(session, Some(rx), &title).map_err(anyhow::Error::msg)
}

#[cfg(not(any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
fn run_window(
    session: ultrafast_live::Session,
    rx: std::sync::mpsc::Receiver<ultrafast_live::server::Request>,
    _path: &Path,
) -> Result<()> {
    eprintln!("ultrafast live: no preview window on this backend; running headless");
    ultrafast_live::run_headless(session, rx);
    Ok(())
}

fn ocr_impl(video: &Path, t: f64) -> Result<(serde_json::Value, String)> {
    let (report, doc) = ultrafast_recon::ocr(&Device::new()?, video, t).map_err(anyhow::Error::msg)?;
    Ok((report, doc.to_json()))
}

#[cfg(all(target_os = "macos", not(feature = "vulkan")))]
fn matte_impl(video: &Path, out: &Path) -> Result<serde_json::Value> {
    let mut last = Instant::now();
    ultrafast_recon::matte(&Device::new()?, video, out, |d, n| {
        if last.elapsed().as_secs_f64() > 1.0 || d == n {
            last = Instant::now();
            eprintln!("  {d}/{n} frames");
        }
    })
    .map_err(anyhow::Error::msg)
}

#[cfg(not(all(target_os = "macos", not(feature = "vulkan"))))]
fn matte_impl(_video: &Path, _out: &Path) -> Result<serde_json::Value> {
    bail!("matte needs Apple Vision (macOS, Metal build)")
}

fn reconstruct_impl(video: &Path, dir: &Path) -> Result<serde_json::Value> {
    let start = Instant::now();
    let (doc, mut report) =
        ultrafast_recon::reconstruct::reconstruct(&Device::new()?, video, dir, &Default::default(), |msg| {
            eprintln!("[{:7.2}s] {msg}", start.elapsed().as_secs_f64())
        })
        .map_err(anyhow::Error::msg)?;
    let out = dir.join("reconstruction.json");
    std::fs::write(&out, doc.to_json())?;
    report["document"] = serde_json::json!(out);
    report["seconds"] = serde_json::json!(start.elapsed().as_secs_f64());
    eprintln!("[{:7.2}s] done -> {}", start.elapsed().as_secs_f64(), out.display());
    Ok(report)
}

/// Speaks a script and writes every word's time: on macOS with the system
/// voices (the bundled AVSpeechSynthesizer script, with word markers),
/// elsewhere with eSpeak NG. Returns a summary line.
pub fn voiceover(text: &str, wav: &Path, words: &Path, voice: Option<&str>, rate: Option<&str>) -> Result<String> {
    if !cfg!(target_os = "macos") {
        let rate = rate
            .map(|r| r.parse::<f64>().with_context(|| format!("bad --rate `{r}` (0..1)")))
            .transpose()?;
        return ultrafast_media::voiceover::speak(text, wav, words, voice, rate).map_err(anyhow::Error::msg);
    }
    let script = std::env::temp_dir().join(format!("ultrafast-say-{}.swift", std::process::id()));
    std::fs::write(&script, include_str!("../assets/say_words.swift"))?;
    let mut cmd = std::process::Command::new("swift");
    cmd.arg(&script)
        .arg(text)
        .arg(wav)
        .arg(words)
        .arg(voice.unwrap_or("Samantha"));
    if let Some(r) = rate {
        cmd.arg(r);
    }
    let out = cmd
        .output()
        .context("`swift` is needed for voiceover (macOS with Xcode tools)")?;
    let _ = std::fs::remove_file(&script);
    if !out.status.success() {
        bail!("voiceover failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn parse_times(s: &str) -> Result<Vec<f64>> {
    s.split(',')
        .map(|x| x.trim().parse::<f64>().with_context(|| format!("bad time `{x}`")))
        .collect()
}

/// A renderer for a command that renders and exits: kernels compile as
/// its frames need them (see `Renderer::new_on_demand`).
fn renderer(doc_path: &Path) -> Result<Renderer> {
    renderer_with(doc_path, Renderer::new_on_demand(Device::new()?)?)
}

fn renderer_with(doc_path: &Path, mut r: Renderer) -> Result<Renderer> {
    if let Some(dir) = doc_path.parent() {
        r.set_base_dir(dir);
    }
    // Font pins whose files are on this machine (`meta.fonts` with a local
    // `src`, as a look lab writes them): the same faces the render service
    // pins, ahead of system and downloaded fonts.
    let pins: Vec<ultrafast_render::geom::FontPin> = std::fs::read_to_string(doc_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.pointer("/meta/fonts").and_then(|f| f.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|f| {
            let path = PathBuf::from(f.get("src")?.as_str()?);
            (path.is_absolute() && path.is_file()).then(|| ultrafast_render::geom::FontPin {
                family: f.get("family").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                weight: f.get("weight").and_then(|v| v.as_u64()).unwrap_or(400) as u16,
                italic: matches!(
                    f.get("style").and_then(|v| v.as_str()),
                    Some("italic") | Some("oblique")
                ),
                path,
            })
        })
        .collect();
    if !pins.is_empty() {
        r.pin_fonts(&pins).map_err(anyhow::Error::msg)?;
    }
    let scripts = std::fs::read_to_string(doc_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.pointer("/meta/script_fonts").cloned());
    if let Some(rows) = scripts {
        r.set_script_fonts(serde_json::from_value(rows)?);
    }
    Ok(r)
}

/// How many pipelines came from stored driver binaries and how many the
/// driver compiled (Vulkan; a cold start's main cost).
fn print_pipelines(device: &Device) {
    let (loaded, compiled, secs) = device.pipeline_stats();
    if loaded + compiled > 0 {
        println!("  pipelines: {loaded} from cache, {compiled} compiled ({secs:.1} s)");
    }
}

/// The size `--size` or `--scale` asks a still to render at (None: the
/// document's), the shape kept as exports keep it.
fn still_size(flag: &dyn Fn(&str) -> Option<String>, doc: &Document) -> Result<Option<(u32, u32)>> {
    let settings = ultrafast_live::output::Settings {
        size: flag("--size")
            .map(|s| s.parse())
            .transpose()
            .map_err(anyhow::Error::msg)?,
        scale: flag("--scale")
            .map(|s| s.parse::<f64>())
            .transpose()
            .context("--scale takes a number")?,
        ..Default::default()
    };
    if settings.size.is_none() && settings.scale.is_none() {
        return Ok(None);
    }
    let t = settings
        .resolve(Some(Path::new("still.png")), &doc.format)
        .map_err(anyhow::Error::msg)?;
    Ok(Some((t.width, t.height)))
}

/// `uf ops | head`: a reader that stops early closes stdout, and printing
/// to it panics. Exit quietly then, as other command-line tools do. Only
/// stdout: a broken pipe to ffmpeg stays an error the export reports.
fn quiet_on_closed_stdout() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let p = info.payload();
        let msg = p
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| p.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if msg.starts_with("failed printing to stdout") && msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        default(info);
    }));
}

/// The export flags (`--format`, `--codec`, `--quality`, `--size`, ...)
/// as settings.
fn export_settings(
    flag: &dyn Fn(&str) -> Option<String>,
    has: &dyn Fn(&str) -> bool,
) -> Result<ultrafast_live::output::Settings> {
    ultrafast_live::output::Settings::from_flags(flag, has).map_err(anyhow::Error::msg)
}

/// What an export wrote, in one phrase: `348 frames (1080x1920 @ 30 fps,
/// mp4 h264, standard) in 2.10 s = 166 fps`.
fn export_summary(rep: &ultrafast_live::export::ExportReport) -> String {
    let t = &rep.target;
    if t.video.is_none() {
        return format!("{} sound in {:.2} s", t.label(), rep.seconds);
    }
    format!(
        "{} frames ({}x{} @ {} fps, {}) in {:.2} s = {:.0} fps",
        rep.frames,
        t.width,
        t.height,
        (t.fps * 1000.0).round() / 1000.0,
        t.label(),
        rep.seconds,
        rep.fps
    )
}

fn report_diagnostics(r: &Renderer) {
    for d in r.diagnostics() {
        eprintln!("warning: node `{}`: {}", d.node, d.message);
    }
}

fn load(path: &Path) -> Result<Document> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Document::from_json(&s)?)
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    let (color, px) = ultrafast_live::png_pixels(rgba);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    enc.write_header()?.write_image_data(&px)?;
    Ok(())
}

/// Renders `frames` frames with up to 3 in flight and no readback. With
/// `live_edits` the renderer runs in live mode and every frame also applies a
/// param edit; every 60th frame applies a structural edit (adds or removes a
/// node), so fused kernels compile in the background mid-run.
/// Renders `frames` frames from `from` with every group timed on its own,
/// and prints where the GPU time goes.
fn profile(path: &Path, doc: Document, frames: u64, from: f64, json: bool) -> Result<()> {
    let mut r = renderer(path)?;
    let p = r.profile_frames(&doc, frames, from)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&p)?);
        return Ok(());
    }
    println!(
        "{} frames from {from}s: {:.2} GPU ms per frame (each group submitted alone, so a little slower than export)",
        p.frames, p.gpu_ms_per_frame
    );
    println!("{:>8} {:>6} {:>6}  group", "ms/frame", "share", "runs");
    for g in p.groups.iter().take(24) {
        println!(
            "{:>8.3} {:>5.1}% {:>6.1}  {}",
            g.ms_per_frame,
            g.share * 100.0,
            g.runs_per_frame,
            g.group
        );
    }
    println!(
        "{:>8.3} {:>5.1}%         the rest (motion blur averaging, output)",
        p.rest_ms_per_frame,
        p.rest_ms_per_frame / p.gpu_ms_per_frame.max(1e-9) * 100.0
    );
    Ok(())
}

fn bench(path: &Path, mut doc: Document, frames: u64, live_edits: bool) -> Result<()> {
    // Every op's kernel up front, as the live player has them.
    let mut r = renderer_with(path, Renderer::new(Device::new()?)?)?;
    // Warm up in exact mode so the starting plan's kernels are compiled.
    r.submit_frame(&doc, 0.0, false)?.0.wait()?;
    if live_edits {
        r.set_mode(Mode::Live);
    }
    let before = r.device().stats();
    let compile_before = r.kernels().stats();
    let n = doc.format.frame_count().max(1);
    let mut inflight = VecDeque::new();
    let mut gpu_ms = Vec::with_capacity(frames as usize);
    let mut cpu_ms = Vec::with_capacity(frames as usize);
    let (mut replans, mut cached, mut run, mut fallback) = (0, 0, 0, 0);
    let (mut edit_ms, mut param_edits, mut struct_edits) = (0.0, 0u64, 0u64);
    let target = doc.output.clone();
    let shape = doc
        .nodes
        .iter()
        .find(|(_, n)| n.op == "shape")
        .map(|(id, _)| id.clone());
    let start = Instant::now();
    for i in 0..frames {
        if live_edits {
            let e0 = Instant::now();
            let mut batch = Vec::new();
            if i % 60 == 30 {
                let id = "__bench_probe".to_string();
                if doc.nodes.contains_key(&id) {
                    batch.push(Edit::SetOutput { node: target.clone() });
                    batch.push(Edit::RemoveNode { id });
                } else {
                    // A new effect on top of the output: needs a new fused kernel.
                    let mut node = Node {
                        op: "adjust".into(),
                        ..Default::default()
                    };
                    node.inputs.insert("source".into(), target.clone());
                    node.params
                        .insert("saturation".into(), Param::Number(1.0 + (i as f64 / 600.0)));
                    batch.push(Edit::AddNode { id: id.clone(), node });
                    batch.push(Edit::SetOutput { node: id });
                }
                struct_edits += 1;
            } else if let Some(id) = &shape {
                let a = (i as f64 * 0.05).sin() * 0.5 + 0.5;
                batch.push(Edit::SetParam {
                    node: id.clone(),
                    param: "color".into(),
                    value: Param::Vector(vec![a, 0.5, 1.0 - a, 1.0]),
                });
                param_edits += 1;
            }
            doc.apply(&batch)?;
            edit_ms += e0.elapsed().as_secs_f64() * 1000.0;
        }
        let (sub, rep) = r.submit_frame(&doc, doc.format.time_of(i % n), false)?;
        cpu_ms.push(rep.cpu_encode_ms);
        replans += rep.replanned as u64;
        cached += rep.groups_cached;
        run += rep.kernels_run;
        fallback += rep.groups_fallback;
        inflight.push_back(sub);
        if inflight.len() >= 3 {
            gpu_ms.push(inflight.pop_front().unwrap().wait()?.gpu_ms);
        }
    }
    while let Some(s) = inflight.pop_front() {
        gpu_ms.push(s.wait()?.gpu_ms);
    }
    let wall = start.elapsed().as_secs_f64();
    let stats = r.device().stats() - before;
    let cs = r.kernels().stats();
    let sorted = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let gpu = sorted(gpu_ms);
    let cpu = sorted(cpu_ms);
    let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p) as usize];
    let f = frames as f64;
    println!("device       {} ({})", r.device().name(), r.device().backend());
    println!(
        "format       {}x{} @ {} fps",
        doc.format.width, doc.format.height, doc.format.fps
    );
    println!("frames       {frames} in {:.3} s  ->  {:.0} fps", wall, f / wall);
    println!(
        "gpu ms       p50 {:.3}  p95 {:.3}  max {:.3}",
        pct(&gpu, 0.5),
        pct(&gpu, 0.95),
        pct(&gpu, 1.0)
    );
    println!(
        "cpu ms       p50 {:.3}  p95 {:.3}  max {:.3}  (encode, incl. replans)",
        pct(&cpu, 0.5),
        pct(&cpu, 0.95),
        pct(&cpu, 1.0)
    );
    println!(
        "kernels      {:.2} run, {:.2} cached groups per frame; {} frames used tier-0 fallback",
        run as f64 / f,
        cached as f64 / f,
        fallback
    );
    println!(
        "plans        {} replans; {} kernels compiled in background ({} jobs, slang {:.0} ms, native {:.0} ms)",
        replans,
        cs.kernels - compile_before.kernels,
        cs.jobs - compile_before.jobs,
        cs.slang_ms - compile_before.slang_ms,
        cs.native_ms - compile_before.native_ms
    );
    if live_edits {
        println!(
            "edits        {param_edits} param, {struct_edits} structural, {:.4} ms/frame applying",
            edit_ms / f
        );
    }
    println!(
        "transfers    uploads {} ({} B), readbacks {} ({} B), params {} B/frame, textures created {}",
        stats.uploads,
        stats.upload_bytes,
        stats.readbacks,
        stats.readback_bytes,
        stats.uniform_bytes / frames.max(1),
        stats.textures_created
    );
    report_diagnostics(&r);
    Ok(())
}

//! The render service end to end on local storage: inputs uploaded as
//! assets (a long source, a cutout archive, a font), stored create-only
//! output with its metadata and manifest, replays, conflicts and refusals
//! (bucket inputs and output are ultrafast Cloud's). Needs a GPU and ffmpeg
//! (with an H.264 encoder) to make the test media; skips without ffmpeg.

use std::path::{Path, PathBuf};
use std::time::Instant;
use ultrafast_service::api::*;
use ultrafast_service::config::{Config, Grants};
use ultrafast_service::inputs::Cache;
use ultrafast_service::job::{Engine, Reply};
use ultrafast_service::records::{AssetRecord, asset_record_name};
use ultrafast_service::store::{Created, Store, Stores};

/// Stores `path` as an upload of `owner`'s, as POST /v1/uploads does, and
/// returns its id's random part.
fn put_asset(stores: &Stores, cfg: &Config, path: &Path, filename: &str, content_type: &str, owner: &str) -> String {
    let raw = ultrafast_service::util::random_base62(22);
    let storage = &cfg.storage;
    let object = storage.name(&format!("assets/{raw}/{filename}"));
    let hashes = ultrafast_service::util::hash_file(path).unwrap();
    let store = stores.main(storage);
    let Created::Done(obj) = store
        .create(
            &storage.bucket,
            &object,
            path,
            content_type,
            &Default::default(),
            &hashes,
        )
        .unwrap()
    else {
        panic!("asset exists")
    };
    let rec = AssetRecord {
        id: format!("asset_{raw}"),
        uri: format!("asset://{raw}"),
        bytes: obj.size,
        sha256: hashes.sha256.clone(),
        filename: filename.into(),
        content_type: content_type.into(),
        created_at: ultrafast_service::util::now_rfc3339(),
        owner: owner.into(),
        object,
        generation: obj.generation,
    };
    store
        .create_bytes(
            &storage.bucket,
            &asset_record_name(storage, &raw),
            &serde_json::to_vec(&rec).unwrap(),
            "application/json",
        )
        .unwrap();
    raw
}

fn ffmpeg(args: &[&str]) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// Frame at `t` of an mp4 as RGBA.
fn decoded_frame(mp4: &Path, t: f64, w: u32, h: u32) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-ss", &format!("{t}"), "-i"])
        .arg(mp4)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .unwrap();
    assert_eq!(
        out.stdout.len(),
        (w * h * 4) as usize,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mut se = 0.0;
    let mut n = 0.0;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = pa[c] as f64 - pb[c] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    let mse = se / n;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

struct Media {
    dir: PathBuf,
    source: Vec<u8>,
    archive: Vec<u8>,
}

fn media() -> Option<Media> {
    let dir = std::env::temp_dir().join(format!("ultrafast-service-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("frames")).unwrap();
    let src = dir.join("source.mp4");
    // A minute of a moving 720p test pattern with a tone, 2 s GOPs, moov
    // in front: tens of megabytes, of which a 2 s chapter needs a few.
    // libx264 where ffmpeg has it; else another H.264 encoder (Fedora's
    // ffmpeg ships OpenH264; NVENC on NVIDIA machines).
    let encoders: [&[&str]; 3] = [
        &["-c:v", "libx264", "-preset", "ultrafast", "-crf", "8"],
        &["-c:v", "libopenh264", "-b:v", "40M"],
        &["-c:v", "h264_nvenc", "-b:v", "40M"],
    ];
    let made = encoders.iter().any(|enc| {
        let mut args: Vec<&str> = vec![
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "60",
        ];
        args.extend_from_slice(enc);
        args.extend_from_slice(&[
            "-g",
            "60",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            src.to_str().unwrap(),
        ]);
        ffmpeg(&args)
    });
    if !made {
        return None;
    }
    // 60 cutout frames: a translucent moving square.
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red@0.6:size=320x240:rate=30,format=rgba",
        "-t",
        "2",
        "-start_number",
        "0",
        dir.join("frames/frame_%06d.png").to_str().unwrap(),
    ]));
    let zip_path = dir.join("frames.zip");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let mut names: Vec<_> = std::fs::read_dir(dir.join("frames"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    names.sort();
    for p in &names {
        z.start_file(
            p.file_name().unwrap().to_string_lossy(),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        std::io::Write::write_all(&mut z, &std::fs::read(p).unwrap()).unwrap();
    }
    z.finish().unwrap();
    Some(Media {
        source: std::fs::read(&src).unwrap(),
        archive: std::fs::read(&zip_path).unwrap(),
        dir,
    })
}

const FACE: &str = "Ultrafast Test Face";

fn font_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets/fonts/Unbounded-VF.ttf")
}

fn document(video: &str, frames: &str, font: Option<&str>) -> serde_json::Value {
    let mut meta = serde_json::json!({"composer": {"name": "test", "version": "0", "revision": "0"}});
    if let Some(src) = font {
        let sha = ultrafast_service::util::sha256_hex(&std::fs::read(font_file()).unwrap());
        meta["fonts"] =
            serde_json::json!([{"family": FACE, "weight": 400, "style": "normal", "src": src, "sha256": sha}]);
    }
    serde_json::json!({
        "version": 1,
        "format": {"width": 320, "height": 240, "fps": 30, "duration": 2},
        "meta": meta,
        "nodes": {
            "clip": {"op": "video", "params": {"src": video, "start": 40.5, "fit": "cover"}},
            "snd": {"op": "audio", "params": {"src": video, "start": 40.5}},
            "cut": {"op": "image", "params": {"src": frames, "fps": 30, "first_frame": 0, "fit": "fill"}},
            "title": {"op": "text", "params": {"text": "Pinned", "font": FACE, "size": 48, "color": "#ffffff", "position": [160, 60], "align": "center"}},
            "stack": {"op": "over", "inputs": {"fg": "cut", "bg": "clip"}},
            "out": {"op": "over", "inputs": {"fg": "title", "bg": "stack"}}
        },
        "output": "out"
    })
}

#[test]
fn renders_from_uploads_to_local_storage() {
    let Some(m) = media() else {
        eprintln!("skipped: no ffmpeg with an H.264 encoder");
        return;
    };
    let cfg = Config {
        cache_dir: m.dir.join("cache"),
        cache_bytes: 1 << 30,
        image_digest: "sha256:test".into(),
        partial_min: 0,
        part_bytes: 16 << 10,
        ..Config::for_tests(m.dir.clone())
    };
    let stores = Stores::new(&cfg.storage).unwrap();
    std::fs::write(m.dir.join("source.mp4"), &m.source).unwrap();
    std::fs::write(m.dir.join("frames.zip"), &m.archive).unwrap();
    let src_id = put_asset(
        &stores,
        &cfg,
        &m.dir.join("source.mp4"),
        "source.mp4",
        "video/mp4",
        "local",
    );
    let zip_id = put_asset(
        &stores,
        &cfg,
        &m.dir.join("frames.zip"),
        "frames.zip",
        "application/zip",
        "local",
    );
    let font_id = put_asset(&stores, &cfg, &font_file(), "face.ttf", "font/ttf", "local");
    let theirs = put_asset(
        &stores,
        &cfg,
        &m.dir.join("source.mp4"),
        "theirs.mp4",
        "video/mp4",
        "someone-else",
    );
    let grants = Grants { caller: "local".into() };
    let device = ultrafast_gpu::Device::new().unwrap();
    let mut engine = Engine {
        stores,
        cache: Cache::new(&cfg.cache_dir, cfg.cache_bytes).unwrap(),
        renderer: ultrafast_render::Renderer::new(device).unwrap(),
        start_s: Some(1.0),
        cfg,
    };
    let video = format!("asset://{src_id}");
    let frames = format!("asset://{zip_id}!/frame_{{frame:06}}.png");
    let font = format!("asset://{font_id}");
    let mut doc = document(&video, &frames, Some(&font));
    // Every cutout frame's hash, as a client's archive hints carry them.
    let mut members = serde_json::Map::new();
    for e in std::fs::read_dir(m.dir.join("frames")).unwrap().flatten() {
        let bytes = std::fs::read(e.path()).unwrap();
        members.insert(
            e.file_name().to_string_lossy().into_owned(),
            serde_json::json!({"sha256": ultrafast_service::util::sha256_hex(&bytes), "size": bytes.len()}),
        );
    }
    doc["meta"]["archives"] = serde_json::json!({format!("asset://{zip_id}"): {"members": members}});
    unsafe { std::env::set_var("ULTRAFAST_OFFLINE", "1") };
    let request = |render: &str, attempt: &str| RenderRequest {
        document: doc.clone(),
        output: None,
        attempt: Some(Attempt {
            render_id: render.into(),
            attempt_id: attempt.into(),
        }),
        deadline: Some(ultrafast_service::util::rfc3339(
            std::time::SystemTime::now() + std::time::Duration::from_secs(300),
        )),
    };

    // The render, stored.
    let Reply::Json(status, body) = engine.render(request("r1", "a1"), &grants, Instant::now()).unwrap() else {
        panic!("expected a receipt");
    };
    assert_eq!(status, 200, "{body}");
    let receipt: Receipt = serde_json::from_str(&body).unwrap();
    assert_eq!(receipt.status, "rendered");
    assert_eq!(receipt.video.frames, 60);
    assert!(receipt.inputs.iter().any(|i| i.uri == video), "{:?}", receipt.inputs);
    let zip_in = receipt.inputs.iter().find(|i| i.uri.contains(&zip_id)).unwrap();
    assert_eq!(zip_in.entries, Some(60));
    assert_eq!(receipt.fonts.len(), 1);
    assert_eq!(receipt.fonts[0].family, FACE);
    assert!(receipt.warnings.is_empty(), "{:?}", receipt.warnings);
    let obj = receipt.object.clone().unwrap();
    let manifest = receipt.manifest.clone().expect("a manifest");
    assert!(manifest.uri.ends_with(".manifest.json"), "{}", manifest.uri);
    let stored = m.dir.join("storage/renders/r1/film.mp4");
    let bytes = std::fs::read(&stored).unwrap();
    assert_eq!(ultrafast_service::util::sha256_hex(&bytes), obj.sha256);
    let meta = engine
        .stores
        .local
        .stat("", "renders/r1/film.mp4", None)
        .unwrap()
        .unwrap();
    assert_eq!(meta.metadata["ultrafast-attempt-id"], "a1");
    assert_eq!(meta.metadata["ultrafast-document-sha256"], receipt.document.sha256);
    assert_eq!(meta.content_type.as_deref(), Some("video/mp4"));

    // What the server rendered from uploads equals the local files rendered.
    let local = document(
        m.dir.join("source.mp4").to_str().unwrap(),
        m.dir.join("frames/frame_{frame:06}.png").to_str().unwrap(),
        None,
    );
    let local_doc = ultrafast_graph::Document::from_json(&local.to_string()).unwrap();
    let mut reference = ultrafast_render::Renderer::new(ultrafast_gpu::Device::new().unwrap()).unwrap();
    reference
        .pin_fonts(&[ultrafast_render::geom::FontPin {
            family: FACE.into(),
            weight: 400,
            italic: false,
            path: font_file(),
        }])
        .unwrap();
    let (want, _) = reference.render_rgba8(&local_doc, 1.0).unwrap();
    let Reply::Bytes { body: png, .. } = engine
        .frame(
            FrameRequest {
                document: doc.clone(),
                t: 1.0,
                format: ImageFormat::Png,
            },
            &grants,
        )
        .unwrap()
    else {
        panic!("expected a frame");
    };
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut got = vec![0u8; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut got).unwrap();
    assert_eq!(
        psnr(&got, &want),
        99.0,
        "the frame from uploads matches the local files exactly"
    );
    let encoded = decoded_frame(&stored, 1.0, 320, 240);
    let q = psnr(&encoded, &want);
    assert!(q > 24.0, "encoded frame PSNR {q:.1} dB");

    // A replay of the same render: 409 with its receipt, nothing rendered.
    let Reply::Json(status, body) = engine.render(request("r1", "a1"), &grants, Instant::now()).unwrap() else {
        panic!()
    };
    assert_eq!(status, 409, "{body}");
    let again: Receipt = serde_json::from_str(&body).unwrap();
    assert_eq!(again.status, "already_rendered");
    assert_eq!(again.object.unwrap().generation, obj.generation);
    // Another attempt at the same render: a conflict.
    let e = engine
        .render(request("r1", "a2"), &grants, Instant::now())
        .err()
        .unwrap();
    assert_eq!((e.status, e.code.as_str()), (409, "output_conflict"));

    // Other formats: a transparent VP9 WebM inline, a PNG sequence stored as
    // a zip, named for their format.
    if ultrafast_live::ffmpeg::encoder_opens("libvpx-vp9") {
        let mut webm = request("r8", "a1");
        webm.output = Some(Output {
            inline: true,
            format: Some(Container::Webm),
            alpha: true,
            quality: Some(Quality::Draft),
            fps: Some(15.0),
            ..Output::default()
        });
        let Reply::Bytes {
            content_type, headers, ..
        } = engine.render(webm, &grants, Instant::now()).unwrap()
        else {
            panic!("expected the video inline");
        };
        assert_eq!(content_type, "video/webm");
        let receipt: Receipt = serde_json::from_str(&headers[0].1).unwrap();
        assert_eq!(
            (receipt.video.format.as_str(), receipt.video.codec.as_str()),
            ("webm", "vp9")
        );
        assert_eq!((receipt.video.frames, receipt.video.fps), (30, 15.0));
        assert!(receipt.video.alpha);
    }
    let mut seq = request("r9", "a1");
    seq.output = Some(Output {
        format: Some(Container::Png),
        range: Some([0.0, 0.5]),
        ..Output::default()
    });
    let Reply::Json(status, body) = engine.render(seq, &grants, Instant::now()).unwrap() else {
        panic!("expected a receipt");
    };
    assert_eq!(status, 200, "{body}");
    let receipt: Receipt = serde_json::from_str(&body).unwrap();
    assert_eq!((receipt.video.format.as_str(), receipt.video.frames), ("png", 15));
    let meta = engine
        .stores
        .local
        .stat("", "renders/r9/frames.zip", None)
        .unwrap()
        .expect("the sequence stored as frames.zip");
    assert_eq!(meta.content_type.as_deref(), Some("application/zip"));
    let zip = zip::ZipArchive::new(std::fs::File::open(m.dir.join("storage/renders/r9/frames.zip")).unwrap()).unwrap();
    let names: Vec<&str> = zip.file_names().collect();
    assert!(
        names.contains(&"00000.png") && names.contains(&"00014.png"),
        "{names:?}"
    );
    assert!(names.contains(&"audio.wav"), "the sound beside the frames: {names:?}");

    // Cloud features: bucket output and bucket inputs.
    let mut to_bucket = request("r2", "a1");
    to_bucket.output = Some(Output {
        uri: Some("s3://my-bucket/renders/x.mp4".into()),
        ..Output::default()
    });
    let e = engine.render(to_bucket, &grants, Instant::now()).err().unwrap();
    assert_eq!((e.status, e.code.as_str()), (501, "cloud_only"), "{e}");
    assert_eq!(e.extra["feature"], "bucket_output");
    let mut from_bucket = request("r3", "a1");
    from_bucket.document["nodes"]["clip"]["params"]["src"] = serde_json::json!("gs://media/x.mp4#5");
    let e = engine.render(from_bucket, &grants, Instant::now()).err().unwrap();
    assert_eq!((e.status, e.code.as_str()), (501, "cloud_only"), "{e}");
    assert_eq!(e.extra["feature"], "bucket_inputs");
    // Local paths, another caller's upload, a missing upload: refused.
    let mut sneaky = request("r4", "a1");
    sneaky.document["nodes"]["clip"]["params"]["src"] = serde_json::json!("/etc/hosts");
    let e = engine.render(sneaky, &grants, Instant::now()).err().unwrap();
    assert_eq!(e.code, "local_path_not_allowed");
    let mut other = request("r5", "a1");
    other.document["nodes"]["clip"]["params"]["src"] = serde_json::json!(format!("asset://{theirs}"));
    let e = engine.render(other, &grants, Instant::now()).err().unwrap();
    assert_eq!(e.code, "input_not_allowed");
    // A member whose bytes differ from its hint: refused.
    let mut lying = request("r6", "a1");
    lying.document["meta"]["archives"][format!("asset://{zip_id}")]["members"]["frame_000003.png"]["sha256"] =
        serde_json::json!("0".repeat(64));
    let e = engine.render(lying, &grants, Instant::now()).err().unwrap();
    assert_eq!((e.status, e.code.as_str()), (422, "input_unavailable"), "{e}");
    assert_eq!(e.extra["reason"], "integrity");
    // A deadline already past: 504, nothing written.
    let mut late = request("r7", "a1");
    late.deadline = Some("2020-01-01T00:00:00Z".into());
    let e = engine.render(late, &grants, Instant::now()).err().unwrap();
    assert_eq!(e.status, 504);
    assert!(!m.dir.join("storage/renders/r7").exists());
    let _ = std::fs::remove_dir_all(&m.dir);
}

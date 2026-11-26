//! Frames look as the export shows them: `render`, `frames` and the MCP
//! frame tool flatten over black where the document draws nothing (with
//! `--alpha` / `alpha: true` they keep its transparency), and a running
//! `ultrafast mcp` renders a timing edit made to the file as the file now
//! says. (A frame shown to an agent on a white page hid near-white type;
//! a session kept a node's old offset after the edit.)

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A white box fading in over half a second of its local time, from
/// `from` seconds, with nothing behind it.
fn doc(from: f64) -> Value {
    json!({
        "version": 1,
        "format": {"width": 160, "height": 90, "fps": 30, "duration": 2},
        "output": "box",
        "nodes": {
            "box": {"op": "shape", "timing": {"in": from, "offset": from},
                "params": {"kind": "rect", "center": [80, 45], "size": [60, 30], "color": "#ffffff",
                    "opacity": {"keys": [{"t": 0, "v": 0}, {"t": 0.5, "v": 1}]}}}
        }
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ultrafast-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_doc(path: &Path, from: f64, mtime_s: u64) {
    std::fs::write(path, serde_json::to_string_pretty(&doc(from)).unwrap()).unwrap();
    // Distinct modification times, however coarse the file system's clock.
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime_s))
        .unwrap();
}

fn rgba(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).unwrap().to_rgba8()
}

fn uf(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_ultrafast"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn render_and_frames_flatten_over_black_unless_asked_for_alpha() {
    let dir = scratch("frames-shown");
    write_doc(&dir.join("doc.json"), 0.0, 1_000_000);
    uf(&dir, &["render", "doc.json", "--at", "1", "--out", "shown.png"]);
    uf(
        &dir,
        &["render", "doc.json", "--at", "1", "--out", "alpha.png", "--alpha"],
    );
    uf(&dir, &["frames", "doc.json", "--out-dir", "seq", "--every", "30"]);
    uf(
        &dir,
        &[
            "frames",
            "doc.json",
            "--out-dir",
            "seq-alpha",
            "--every",
            "30",
            "--alpha",
        ],
    );
    for f in ["shown.png", "seq/00030.png"] {
        let img = image::open(dir.join(f)).unwrap().to_rgba8();
        assert_eq!(
            img.get_pixel(2, 2).0,
            [0, 0, 0, 255],
            "{f}: black where nothing is drawn"
        );
        assert_eq!(img.get_pixel(80, 45).0, [255, 255, 255, 255], "{f}");
    }
    for f in ["alpha.png", "seq-alpha/00030.png"] {
        let img = image::open(dir.join(f)).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(2, 2).0[3], 0, "{f}: see-through where nothing is drawn");
        assert_eq!(img.get_pixel(80, 45).0, [255, 255, 255, 255], "{f}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(dir: &Path) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ultrafast"))
            .arg("mcp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            id: 0,
        };
        m.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// `ultrafast_frame`'s image, decoded, and its caption.
    fn frame(&mut self, args: Value) -> (image::RgbaImage, String) {
        let r = self.request("tools/call", json!({"name": "ultrafast_frame", "arguments": args}));
        let content = r["result"]["content"].as_array().unwrap_or_else(|| panic!("{r}"));
        let data = content[0]["data"].as_str().unwrap();
        (rgba(&base64(data)), content[1]["text"].as_str().unwrap().to_string())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').map(val).collect();
    bytes
        .chunks(4)
        .flat_map(|c| {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &v)| n | (v as u32) << (18 - 6 * i));
            [(n >> 16) as u8, (n >> 8) as u8, n as u8].into_iter().take(c.len() - 1)
        })
        .collect()
}

#[test]
fn mcp_frames_show_the_export_and_follow_timing_edits_to_the_file() {
    let dir = scratch("mcp-frames");
    let path = dir.join("doc.json");
    let p = path.to_str().unwrap();
    write_doc(&path, 1.0, 1_000_000);
    let mut mcp = Mcp::start(&dir);
    let (img, caption) = mcp.frame(json!({"path": p, "t": 1.25}));
    assert_eq!(
        img.get_pixel(2, 2).0,
        [0, 0, 0, 255],
        "black where nothing is drawn: {caption}"
    );
    let half = img.get_pixel(80, 45).0[0];
    assert!((150..230).contains(&half), "half faded in at 1.25 s: {half}");

    // The box now comes in from 0.75 s: fully in at 1.25 s.
    write_doc(&path, 0.75, 1_000_100);
    let (img, _) = mcp.frame(json!({"path": p, "t": 1.25}));
    assert_eq!(img.get_pixel(80, 45).0, [255, 255, 255, 255], "the edited timing");

    // Transparency on request, as a checkerboard a client can't hide.
    let (img, caption) = mcp.frame(json!({"path": p, "t": 1.25, "alpha": true}));
    assert!(caption.contains("checkerboard"), "{caption}");
    let corner = img.get_pixel(2, 2).0;
    assert!(corner[3] == 255 && corner[0] > 60 && corner[0] < 160, "{corner:?}");
    assert_ne!(img.get_pixel(2, 2), img.get_pixel(14, 2), "squares alternate");
    assert_eq!(img.get_pixel(80, 45).0, [255, 255, 255, 255]);
    drop(mcp);
    let _ = std::fs::remove_dir_all(&dir);
}

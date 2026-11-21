//! Frame analysis where Apple Vision isn't available: text recognition
//! with Tesseract (the `tesseract` program, English by default; set
//! `ULTRAFAST_OCR_LANGS`, e.g. `eng+deu`). Same results as the macOS
//! module: lines of text with their boxes and their words' boxes. Subject
//! masks still need Apple Vision.

use crate::MediaError;
use std::path::Path;
use std::process::Command;

/// A recognized line of text, in pixels with the origin at the top left.
#[derive(Debug, Clone)]
pub struct TextBox {
    pub text: String,
    pub confidence: f32,
    pub rect: [f64; 4],
    /// Its words (runs between spaces) and their boxes, in order.
    pub words: Vec<(String, [f64; 4])>,
}

fn run(cmd: &mut Command, what: &str, install: &str) -> Result<Vec<u8>, MediaError> {
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            MediaError::Platform(format!("{what} needs `{install}` installed (apt, dnf, pacman or brew)"))
        } else {
            MediaError::Platform(format!("{what}: {e}"))
        }
    })?;
    if !out.status.success() {
        return Err(MediaError::Platform(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// Lines and words from Tesseract's TSV output (level 5 rows are words,
/// grouped into lines by block, paragraph and line number), in frame px
/// (the image was scaled by `scale`).
fn parse_tsv(tsv: &str, scale: f64) -> Vec<TextBox> {
    let mut lines: Vec<((u32, u32, u32), TextBox, f32, usize)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 || c[0] != "5" {
            continue;
        }
        let text = c[11].trim();
        let conf: f32 = c[10].parse().unwrap_or(-1.0);
        if text.is_empty() || conf < 0.0 {
            continue;
        }
        let n = |i: usize| c[i].parse::<f64>().unwrap_or(0.0) / scale;
        let rect = [n(6), n(7), n(8), n(9)];
        let key = (
            c[2].parse().unwrap_or(0),
            c[3].parse().unwrap_or(0),
            c[4].parse().unwrap_or(0),
        );
        match lines.iter_mut().find(|l| l.0 == key) {
            Some(l) => {
                let r = &mut l.1.rect;
                let (x1, y1) = (
                    (r[0] + r[2]).max(rect[0] + rect[2]),
                    (r[1] + r[3]).max(rect[1] + rect[3]),
                );
                r[0] = r[0].min(rect[0]);
                r[1] = r[1].min(rect[1]);
                r[2] = x1 - r[0];
                r[3] = y1 - r[1];
                l.1.text.push(' ');
                l.1.text.push_str(text);
                l.1.words.push((text.to_string(), rect));
                l.2 += conf;
                l.3 += 1;
            }
            None => lines.push((
                key,
                TextBox {
                    text: text.to_string(),
                    confidence: 0.0,
                    rect,
                    words: vec![(text.to_string(), rect)],
                },
                conf,
                1,
            )),
        }
    }
    let mut out: Vec<TextBox> = lines
        .into_iter()
        .map(|(_, mut b, sum, n)| {
            b.confidence = sum / n as f32 / 100.0;
            b
        })
        // Weak single marks are usually texture read as letters.
        .filter(|b| b.confidence >= 0.5 || b.text.chars().filter(|c| c.is_alphanumeric()).count() >= 3)
        .collect();
    out.sort_by(|a, b| a.rect[1].total_cmp(&b.rect[1]).then(a.rect[0].total_cmp(&b.rect[0])));
    out
}

/// Recognizes text lines in the frame of `path` at `t` seconds.
pub fn recognize_text(path: &Path, t: f64) -> Result<Vec<TextBox>, MediaError> {
    let info = crate::video_info(path)?;
    let png = std::env::temp_dir().join(format!(
        "ultrafast-ocr-{}-{}.png",
        std::process::id(),
        (t * 1000.0) as u64
    ));
    // Tesseract reads type best with x-heights of 20-30 px: small frames
    // are enlarged first.
    let scale = if info.width.max(info.height) < 1600 { 2.0 } else { 1.0 };
    let result = (|| {
        run(
            Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
                .arg(path)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    &format!("scale=iw*{scale}:ih*{scale}:flags=lanczos"),
                ])
                .arg(&png),
            "ocr",
            "ffmpeg",
        )?;
        let langs = std::env::var("ULTRAFAST_OCR_LANGS").unwrap_or_else(|_| "eng".into());
        let tsv = run(
            Command::new("tesseract")
                .arg(&png)
                .args(["stdout", "-l", &langs, "--psm", "11", "tsv"]),
            "ocr",
            "tesseract",
        )?;
        Ok(parse_tsv(&String::from_utf8_lossy(&tsv), scale))
    })();
    let _ = std::fs::remove_file(&png);
    result
}

/// Foreground subject masks need Apple Vision.
pub fn subject_mask(_path: &Path, _t: f64) -> Result<Option<(u32, u32, Vec<u8>)>, MediaError> {
    Err(MediaError::Platform("subject masks need Apple Vision (macOS)".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_words_into_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t100\t50\t80\t40\t96.5\tSay\n\
                   5\t1\t1\t1\t1\t2\t200\t52\t60\t38\t91.0\tthe\n\
                   5\t1\t2\t1\t1\t1\t100\t150\t300\t40\t88.0\tthing.\n\
                   5\t1\t3\t1\t1\t1\t10\t10\t5\t5\t20.0\t~\n";
        let b = parse_tsv(tsv, 2.0);
        assert_eq!(b.len(), 2, "{b:?}");
        assert_eq!(b[0].text, "Say the");
        assert_eq!(b[0].words.len(), 2);
        assert_eq!(b[0].rect, [50.0, 25.0, 80.0, 20.0]);
        assert!((b[0].confidence - 0.9375).abs() < 1e-4);
        assert_eq!(b[1].text, "thing.");
    }
}

//! Voiceover where macOS's voices aren't available: speaks a script with
//! eSpeak NG (`espeak-ng`, run as a program) into a WAV file and writes the
//! time of every word as `{"words": [{"word", "start", "end"}], "duration"}`,
//! the same file the macOS voices write, for `audio` and `captions` nodes.
//!
//! The script is spoken in one take, so it keeps its phrasing. Word times
//! come from aligning each word's own spoken length (each word is also
//! spoken alone) to the take: phrase breaks (punctuation) are pinned to
//! the pauses in it, and the words of a phrase share its speech in
//! proportion to their lengths.

use std::path::Path;
use std::process::{Command, Stdio};

/// The rate a `rate` of 0..1 means, in words a minute (0.5: espeak-ng's
/// default 175).
fn wpm(rate: f64) -> u32 {
    (80.0 + rate.clamp(0.0, 1.0) * 190.0).round() as u32
}

/// Mono samples (-1..1) and the sample rate of a 16-bit PCM WAV file.
fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, f64), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut rate, mut channels, mut bits) = (0u32, 1u16, 16u16);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &bytes[i + 8..(i + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]).max(1);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            if bits != 16 {
                return Err(format!("{bits}-bit WAV (expected 16)"));
            }
            let frame = 2 * channels as usize;
            let samples = body
                .chunks_exact(frame)
                .map(|f| i16::from_le_bytes([f[0], f[1]]) as f32 / 32768.0)
                .collect();
            return Ok((samples, rate.max(1) as f64));
        }
        i += 8 + len + (len & 1);
    }
    Err("WAV without data".into())
}

/// Speech and silence in 10 ms windows: `(start, end)` seconds of each
/// stretch of speech, with pauses shorter than `bridge` seconds (the stops
/// inside words) counted as speech.
fn speech(samples: &[f32], rate: f64, bridge: f64) -> Vec<(f64, f64)> {
    let win = (rate * 0.01).round().max(1.0) as usize;
    let rms: Vec<f32> = samples
        .chunks(win)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0.0f32, f32::max);
    let floor = (peak * 0.04).max(0.002);
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (k, &v) in rms.iter().enumerate() {
        if v < floor {
            continue;
        }
        let (a, b) = (k as f64 * 0.01, (k + 1) as f64 * 0.01);
        match out.last_mut() {
            Some(last) if a - last.1 < bridge => last.1 = b,
            _ => out.push((a, b)),
        }
    }
    out
}

fn espeak(text: &str, voice: &str, wpm: u32, out: &Path, no_final_pause: bool) -> Result<(), String> {
    let mut cmd = Command::new("espeak-ng");
    cmd.args(["-v", voice, "-s", &wpm.to_string(), "-w"])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if no_final_pause {
        cmd.arg("-z");
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "voiceover needs eSpeak NG on this system: install `espeak-ng` (apt, dnf, pacman or brew)".to_string()
        } else {
            format!("espeak-ng: {e}")
        }
    })?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    let done = child.wait_with_output().map_err(|e| e.to_string())?;
    if !done.status.success() {
        return Err(format!(
            "espeak-ng failed: {}",
            String::from_utf8_lossy(&done.stderr).trim()
        ));
    }
    Ok(())
}

/// Speaks `text` into `wav` and writes the word times to `words`. `voice`
/// is an espeak-ng voice (`en-us`, `en-gb`, `fr`...; default `en-us`),
/// `rate` 0..1 (default 0.5). Returns a one-line summary.
pub fn speak(text: &str, wav: &Path, words: &Path, voice: Option<&str>, rate: Option<f64>) -> Result<String, String> {
    let voice = voice.unwrap_or("en-us");
    let wpm = wpm(rate.unwrap_or(0.5));
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Err("nothing to say".into());
    }
    let tmp = std::env::temp_dir().join(format!("ultrafast-voice-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        espeak(text, voice, wpm, wav, false)?;
        let (samples, sr) = read_wav(&std::fs::read(wav).map_err(|e| format!("{}: {e}", wav.display()))?)?;
        let total = samples.len() as f64 / sr;

        // Each word alone: how long it takes to say.
        let threads = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4)
            .clamp(1, 8);
        let mut lens = vec![0.0f64; tokens.len()];
        let idx: Vec<usize> = (0..tokens.len()).collect();
        std::thread::scope(|s| -> Result<(), String> {
            let handles: Vec<_> = idx
                .chunks(tokens.len().div_ceil(threads))
                .map(|chunk| {
                    let tmp = &tmp;
                    let tokens = &tokens;
                    s.spawn(move || -> Result<Vec<(usize, f64)>, String> {
                        chunk
                            .iter()
                            .map(|&i| {
                                let p = tmp.join(format!("w{i}.wav"));
                                espeak(tokens[i], voice, wpm, &p, true)?;
                                let (w, r) = read_wav(&std::fs::read(&p).map_err(|e| e.to_string())?)?;
                                let sp = speech(&w, r, 0.12);
                                let len = match (sp.first(), sp.last()) {
                                    (Some(a), Some(b)) => b.1 - a.0,
                                    _ => 0.05,
                                };
                                Ok((i, len.max(0.05)))
                            })
                            .collect()
                    })
                })
                .collect();
            for h in handles {
                for (i, l) in h.join().map_err(|_| "a voice thread failed".to_string())?? {
                    lens[i] = l;
                }
            }
            Ok(())
        })?;

        // The take's speech, and the pauses between its phrases.
        let parts = speech(&samples, sr, 0.12);
        if parts.is_empty() {
            return Err("espeak-ng spoke nothing".into());
        }
        // Speech time as one stream (pauses removed): position -> seconds.
        let mut cum = vec![0.0];
        for p in &parts {
            cum.push(cum.last().unwrap() + (p.1 - p.0));
        }
        let stream = *cum.last().unwrap();
        let at = |pos: f64, end: bool| -> f64 {
            for (k, p) in parts.iter().enumerate() {
                let (a, b) = (cum[k], cum[k + 1]);
                if pos < b || (end && pos <= b) || k + 1 == parts.len() {
                    return p.0 + (pos - a).clamp(0.0, p.1 - p.0);
                }
            }
            parts.last().unwrap().1
        };
        // Phrase ends (punctuation) pinned to the nearest pause.
        let sum: f64 = lens.iter().sum();
        let mut expect = vec![0.0];
        for l in &lens {
            expect.push(expect.last().unwrap() + l / sum * stream);
        }
        let mut anchors: Vec<(usize, f64)> = vec![(0, 0.0)];
        let mut used = 0usize; // pauses are cum[1..parts.len()]
        for i in 0..tokens.len() - 1 {
            if !tokens[i].ends_with([',', '.', ';', ':', '!', '?', '—']) {
                continue;
            }
            let want = expect[i + 1];
            let best = (used + 1..parts.len()).min_by(|&a, &b| (cum[a] - want).abs().total_cmp(&(cum[b] - want).abs()));
            if let Some(k) = best
                && (cum[k] - want).abs() < 0.8
            {
                anchors.push((i + 1, cum[k]));
                used = k;
            }
        }
        anchors.push((tokens.len(), stream));
        // Within a phrase, words share its speech by their lengths.
        let mut pos = vec![0.0; tokens.len() + 1];
        for w in anchors.windows(2) {
            let ((i0, p0), (i1, p1)) = (w[0], w[1]);
            let span: f64 = lens[i0..i1].iter().sum::<f64>().max(1e-9);
            let mut acc = p0;
            for i in i0..i1 {
                pos[i] = acc;
                acc += lens[i] / span * (p1 - p0);
            }
            pos[i1] = p1;
        }
        let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
        let list: Vec<serde_json::Value> = tokens
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let start = at(pos[i], false);
                let end = at(pos[i + 1], true).max(start + 0.05);
                serde_json::json!({ "word": w, "start": r3(start), "end": r3(end) })
            })
            .collect();
        let out = serde_json::json!({ "words": list, "duration": r3(total) });
        std::fs::write(words, serde_json::to_string_pretty(&out).unwrap())
            .map_err(|e| format!("{}: {e}", words.display()))?;
        Ok(format!(
            "{} words, {total:.2} s -> {}, {}",
            tokens.len(),
            wav.display(),
            words.display()
        ))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wav_and_finds_speech() {
        // 0.3 s silence, 0.4 s tone, 0.3 s silence at 8 kHz.
        let sr = 8000u32;
        let mut pcm: Vec<i16> = vec![0; 2400];
        pcm.extend((0..3200).map(|i| ((i as f32 * 0.3).sin() * 12000.0) as i16));
        pcm.extend(vec![0i16; 2400]);
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend(((36 + pcm.len() * 2) as u32).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(sr.to_le_bytes());
        b.extend((sr * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(((pcm.len() * 2) as u32).to_le_bytes());
        for s in &pcm {
            b.extend(s.to_le_bytes());
        }
        let (s, r) = read_wav(&b).unwrap();
        assert_eq!(r, 8000.0);
        let sp = speech(&s, r, 0.12);
        assert_eq!(sp.len(), 1);
        assert!((sp[0].0 - 0.3).abs() < 0.02 && (sp[0].1 - 0.7).abs() < 0.02, "{sp:?}");
        assert_eq!(wpm(0.5), 175);
    }
}

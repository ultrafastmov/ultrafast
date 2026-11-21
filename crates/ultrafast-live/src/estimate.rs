//! What a render will cost before it runs: rendered minutes from the
//! document's format alone (and their price, where a service sets one), and
//! GPU seconds measured by rendering a sample of its frames (nothing is
//! encoded) on the renderer's own GPU, scaled to the whole range.
//!
//! `uf estimate` prints it; a render service answers `POST /v1/estimate`
//! with it, on the GPU it would render on.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use ultrafast_graph::{Document, Format};
use ultrafast_render::{Mode, Renderer};

/// How many rendered minutes one minute of output counts as at this size:
/// 720p and below half, up to 1080p one, 4K three (and larger frames in
/// proportion to 4K's pixels).
pub fn minute_weight(width: u32, height: u32) -> f64 {
    let px = width as f64 * height as f64;
    const P720: f64 = 1280.0 * 720.0;
    const P1080: f64 = 1920.0 * 1080.0;
    const P4K: f64 = 3840.0 * 2160.0;
    if px <= P720 {
        0.5
    } else if px <= P1080 {
        1.0
    } else if px <= P4K {
        3.0
    } else {
        3.0 * px / P4K
    }
}

/// Rendered minutes, from the format alone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// Rendered minutes (output minutes weighted by size).
    pub minutes: f64,
}

/// What rendering seconds `[from, to]` of a document of this format counts as.
pub fn price(format: &Format, from: f64, to: f64) -> Price {
    let seconds = (to.min(format.duration) - from.max(0.0)).max(0.0);
    Price {
        minutes: seconds / 60.0 * minute_weight(format.width, format.height),
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Range in seconds (default: the whole document).
    pub from: Option<f64>,
    pub to: Option<f64>,
    /// Frames to time, spread evenly over the range (every frame of a
    /// range with fewer than twice as many).
    pub samples: u32,
    /// The price of a rendered minute, for a service that bills; `None`
    /// (the default) quotes no price.
    pub usd_per_minute: Option<f64>,
    /// What the export writes (size, frame rate, quality level): priced at
    /// its size, timed with its motion blur, its frames counted at its rate
    /// (default: the document's own).
    pub target: Option<crate::output::Target>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            samples: 64,
            usd_per_minute: None,
            target: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    /// Rendered minutes (1080p minutes; 720p counts half, 4K three).
    pub minutes: f64,
    /// At [`Options::usd_per_minute`]; `None` where nothing is billed.
    pub usd: Option<f64>,
    /// GPU time for every frame of the range, from the sampled frames.
    pub gpu_seconds: f64,
    /// Seconds from the request to the first frame: planning, kernel
    /// compiles that are not cached yet, opening media, the frame itself.
    pub first_byte_s: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Seconds of output.
    pub seconds: f64,
    pub frames: u64,
    /// The GPU the frames were timed on.
    pub device: String,
    pub sampled_frames: u32,
}

/// Estimates a render of `doc` on `renderer` (whose base directory is set
/// for the document's files). Leaves the renderer in the mode and at the
/// size it found it.
pub fn estimate(renderer: &mut Renderer, doc: &Document, opts: &Options) -> Result<Estimate, String> {
    let fmt = doc.format;
    let target = opts.target.unwrap_or_else(|| crate::output::Target::of(&fmt));
    // The quality level's motion blur, as the export renders it.
    let tuned;
    let mb = target.quality.motion_blur(fmt.motion_blur);
    let doc = if mb != fmt.motion_blur.filter(|m| m.samples > 1) {
        let mut d = doc.fork();
        d.format.motion_blur = mb;
        tuned = d;
        &tuned
    } else {
        doc
    };
    let from = opts.from.unwrap_or(0.0).clamp(0.0, fmt.duration);
    let to = opts.to.unwrap_or(fmt.duration).clamp(from, fmt.duration);
    let fps = target.fps.max(1.0);
    let first = (from * fps).round() as u64;
    let last = ((to * fps).round() as u64).max(first + 1);
    let total = last - first;
    let time_of = |f: u64| f as f64 / fps;
    // Priced at the size written; sound alone as the smallest.
    let (w, h) = match target.video {
        Some(_) => (target.width, target.height),
        None => (1, 1),
    };
    let price = price(
        &Format {
            width: w,
            height: h,
            ..fmt
        },
        from,
        to,
    );

    let prev_mode = renderer.mode();
    renderer.set_mode(Mode::Exact);
    // Timed at the size the export draws (4K costs what 4K costs).
    let own_size = renderer.output_size();
    if target.video.is_some() && !target.native_size(&fmt) {
        renderer.set_output_size(Some((target.width, target.height)));
    }
    let result = (|| -> Result<(f64, f64, u32), String> {
        // The first frame pays for planning, compiles and opening media:
        // what a caller waits before the first byte.
        let t0 = Instant::now();
        renderer
            .submit_frame(doc, time_of(first), false)
            .and_then(|(sub, _)| sub.wait().map_err(Into::into))
            .map_err(|e| e.to_string())?;
        let first_byte = t0.elapsed().as_secs_f64();
        // A GPU that was idle runs its first frames at low clocks: warm it
        // up (untimed) for a moment first, as an export's first frames do.
        let warm = Instant::now();
        let mut frame = first;
        while warm.elapsed().as_secs_f64() < 0.2 && frame < first + 30.min(total) {
            let (sub, _) = renderer
                .submit_frame(doc, time_of(frame), false)
                .map_err(|e| e.to_string())?;
            sub.wait().map_err(|e| e.to_string())?;
            frame += 1;
        }

        // The frames to time: all of them for a short range, otherwise one
        // in the middle of each of `samples` equal spans (a film's cost
        // swings frame to frame: motion blur takes more samples on fast
        // moves, layers come and go). Each timed frame follows its own
        // previous frame, untimed, so it costs what it does in an export
        // (layers that held still are reused, a simulation has caught up).
        let samples = (opts.samples.max(1) as u64).min(total);
        let plan: Vec<(u64, bool)> = if total <= samples * 2 {
            (first..last).map(|f| (f, true)).collect()
        } else {
            (0..samples)
                .flat_map(|k| {
                    let f = first + ((k as f64 + 0.5) * total as f64 / samples as f64) as u64;
                    let f = f.clamp(first + 1, last - 1);
                    [(f - 1, false), (f, true)]
                })
                .collect()
        };
        // A few frames in flight, as an export keeps them: a GPU waited on
        // frame by frame clocks down.
        const IN_FLIGHT: usize = 3;
        let mut gpu_ms = 0.0;
        let mut timed = 0u32;
        let mut inflight = std::collections::VecDeque::new();
        let mut take = |sub: ultrafast_gpu::Submitted, timed_frame: bool| -> Result<(), String> {
            let ms = sub.wait().map_err(|e| e.to_string())?.gpu_ms;
            if timed_frame {
                gpu_ms += ms;
                timed += 1;
            }
            Ok(())
        };
        for (frame, timed_frame) in plan {
            let (sub, _) = renderer
                .submit_frame(doc, time_of(frame), false)
                .map_err(|e| e.to_string())?;
            inflight.push_back((sub, timed_frame));
            while inflight.len() >= IN_FLIGHT {
                let (sub, timed_frame) = inflight.pop_front().unwrap();
                take(sub, timed_frame)?;
            }
        }
        for (sub, timed_frame) in inflight {
            take(sub, timed_frame)?;
        }
        drop(take);
        if timed == 0 {
            // A one-frame range: its only frame was the seek.
            let (sub, _) = renderer
                .submit_frame(doc, time_of(first), false)
                .map_err(|e| e.to_string())?;
            gpu_ms += sub.wait().map_err(|e| e.to_string())?.gpu_ms;
            timed = 1;
        }
        Ok((first_byte, gpu_ms / timed as f64, timed))
    })();
    renderer.set_mode(prev_mode);
    renderer.set_output_size(own_size);
    let (first_byte_s, ms_per_frame, sampled_frames) = result?;
    Ok(Estimate {
        minutes: price.minutes,
        usd: opts.usd_per_minute.map(|rate| price.minutes * rate),
        gpu_seconds: ms_per_frame * total as f64 / 1000.0,
        first_byte_s,
        width: target.width,
        height: target.height,
        fps: target.fps,
        seconds: to - from,
        frames: total,
        device: renderer.device().name().to_string(),
        sampled_frames,
    })
}

/// Rounds for display: `0.2`, `0.06`, `1.25`, `12`.
fn short(x: f64) -> String {
    let s = if x >= 10.0 {
        format!("{x:.1}")
    } else if x >= 0.1 {
        format!("{x:.2}")
    } else {
        format!("{x:.3}")
    };
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s.is_empty() || s == "0" && x > 0.0 {
        format!("{x:.1e}")
    } else {
        s
    }
}

/// Dollars for display: `$0.006`, `$0.30`, `$12.40`.
/// Rounded up, so a bill never comes to more than the quote shown.
pub fn usd_text(usd: f64) -> String {
    let up = |x: f64, scale: f64| (x * scale - 1e-9).ceil().max(0.0) / scale;
    if usd < 0.1 {
        format!("${:.3}", up(usd, 1000.0))
    } else {
        format!("${:.2}", up(usd, 100.0))
    }
}

impl Estimate {
    /// The three lines `uf estimate` prints, after the document's name.
    pub fn text(&self, name: &str) -> String {
        let price = self.usd.map(|usd| format!(" · {}", usd_text(usd))).unwrap_or_default();
        format!(
            "{name} · {}×{} · {} fps · {:.1} s\n{} rendered minutes{price}\ngpu {} s on {} · about {} s to the first byte",
            self.width,
            self.height,
            short(self.fps),
            self.seconds,
            short(self.minutes),
            short(self.gpu_seconds),
            self.device,
            short(self.first_byte_s.max(0.1)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(w: u32, h: u32, d: f64) -> Format {
        Format {
            width: w,
            height: h,
            fps: 30.0,
            duration: d,
            motion_blur: None,
        }
    }

    #[test]
    fn prices_by_size() {
        // 12 s of 1080x1920 is 0.2 minutes.
        let p = price(&fmt(1080, 1920, 12.0), 0.0, 12.0);
        assert!((p.minutes - 0.2).abs() < 1e-9);
        assert_eq!(usd_text(p.minutes * 0.03), "$0.006");
        assert_eq!(short(p.minutes), "0.2");
        assert_eq!(minute_weight(1280, 720), 0.5);
        assert_eq!(minute_weight(1080, 1080), 1.0);
        assert_eq!(minute_weight(3840, 2160), 3.0);
        assert_eq!(minute_weight(2160, 3840), 3.0);
        // A range prices only its seconds.
        let p = price(&fmt(1920, 1080, 60.0), 30.0, 90.0);
        assert!((p.minutes - 0.5).abs() < 1e-9);
        assert_eq!(usd_text(0.3), "$0.30");
        assert_eq!(usd_text(0.0075), "$0.008");
        assert_eq!(usd_text(0.0001), "$0.001");
    }
}

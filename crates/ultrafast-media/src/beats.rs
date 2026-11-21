//! Rhythm analysis for cutting to music: onsets (spectral flux), tempo
//! (autocorrelation of the onset envelope) and a beat grid aligned to the
//! strongest onsets, with every fourth beat marked as a downbeat.

use crate::audio::{CHANNELS, Pcm, RATE};

const N: usize = 1024;
const HOP: usize = 512;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Onset {
    pub t: f64,
    /// 0..1, relative to the strongest onset.
    pub strength: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Rhythm {
    pub bpm: f64,
    /// Seconds of every beat over the track.
    pub beats: Vec<f64>,
    /// Every fourth beat, from the strongest phase.
    pub downbeats: Vec<f64>,
    pub onsets: Vec<Onset>,
    pub duration: f64,
}

/// In-place radix-2 FFT of (re, im).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nc = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nc;
            }
        }
        len <<= 1;
    }
}

/// Onset strength per hop: half-wave rectified change of log magnitude,
/// summed over bins (weighted toward lows when `low` is set).
fn flux(pcm: &Pcm, band: &str) -> Vec<f64> {
    let mono: Vec<f64> = pcm
        .samples
        .chunks_exact(CHANNELS)
        .map(|c| c.iter().map(|&s| s as f64).sum::<f64>() / CHANNELS as f64)
        .collect();
    let window: Vec<f64> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N as f64).cos())
        .collect();
    let bin_hz = RATE as f64 / N as f64;
    let (lo, hi) = match band {
        "low" => (20.0, 200.0),
        "mid" => (200.0, 2000.0),
        "high" => (2000.0, 12000.0),
        _ => (20.0, 12000.0),
    };
    let (b0, b1) = ((lo / bin_hz) as usize, ((hi / bin_hz) as usize).min(N / 2));
    let mut prev = vec![0.0; N / 2];
    let mut out = Vec::new();
    let mut i = 0;
    while i + N <= mono.len() {
        let mut re: Vec<f64> = (0..N).map(|k| mono[i + k] * window[k]).collect();
        let mut im = vec![0.0; N];
        fft(&mut re, &mut im);
        let mut f = 0.0;
        for k in b0.max(1)..b1 {
            let m = (1.0 + 100.0 * (re[k] * re[k] + im[k] * im[k]).sqrt()).ln();
            f += (m - prev[k]).max(0.0);
            prev[k] = m;
        }
        out.push(f);
        i += HOP;
    }
    out
}

/// Onsets, tempo and a beat grid of `pcm` (`band`: all, low, mid, high).
pub fn analyze(pcm: &Pcm, band: &str) -> Rhythm {
    let hop_s = HOP as f64 / RATE as f64;
    let duration = pcm.duration();
    let env = flux(pcm, band);
    if env.len() < 8 {
        return Rhythm {
            bpm: 0.0,
            beats: vec![],
            downbeats: vec![],
            onsets: vec![],
            duration,
        };
    }
    // Onsets: peaks above a local mean, at least 90 ms apart.
    let w = (0.25 / hop_s) as usize;
    let max = env.iter().cloned().fold(0.0, f64::max).max(1e-9);
    let mut onsets: Vec<Onset> = Vec::new();
    for i in 1..env.len() - 1 {
        let (a, b) = (i.saturating_sub(w), (i + w).min(env.len()));
        let mean = env[a..b].iter().sum::<f64>() / (b - a) as f64;
        if env[i] > env[i - 1] && env[i] >= env[i + 1] && env[i] > mean * 1.4 + max * 0.05 {
            let t = i as f64 * hop_s + N as f64 / 2.0 / RATE as f64;
            if onsets.last().is_none_or(|o| t - o.t > 0.09) {
                onsets.push(Onset {
                    t,
                    strength: env[i] / max,
                });
            } else if let Some(o) = onsets.last_mut()
                && env[i] / max > o.strength
            {
                *o = Onset {
                    t,
                    strength: env[i] / max,
                };
            }
        }
    }
    // Tempo: the lag (60..190 BPM) where the onset envelope best matches
    // itself, preferring the middle of the range a little.
    let mean = env.iter().sum::<f64>() / env.len() as f64;
    let e: Vec<f64> = env.iter().map(|v| v - mean).collect();
    // 190 BPM down to 60 BPM.
    let (lag_min, lag_max) = ((60.0 / 190.0 / hop_s) as usize, (1.0 / hop_s) as usize);
    let mut best = (lag_min, f64::MIN);
    for lag in lag_min..=lag_max.min(e.len() - 1) {
        let s: f64 = (0..e.len() - lag).map(|i| e[i] * e[i + lag]).sum::<f64>() / (e.len() - lag) as f64;
        let bpm = 60.0 / (lag as f64 * hop_s);
        let prior = (-((bpm / 120.0).log2()).powi(2) / (2.0 * 0.6 * 0.6)).exp();
        if s * prior > best.1 {
            best = (lag, s * prior);
        }
    }
    let period = best.0 as f64 * hop_s;
    // Phase: the offset whose grid collects the most onset strength.
    let mut phase = (0.0, f64::MIN);
    for k in 0..48 {
        let off = period * k as f64 / 48.0;
        let score: f64 = onsets
            .iter()
            .map(|o| {
                let d = ((o.t - off) / period).fract().abs();
                let d = d.min(1.0 - d) * period;
                o.strength * (-(d * d) / (2.0 * 0.03 * 0.03)).exp()
            })
            .sum();
        if score > phase.1 {
            phase = (off, score);
        }
    }
    // Refine period and phase by least squares on the onsets that fall
    // on the grid (the lag is only as fine as one hop).
    let (mut off, mut period) = (phase.0, period);
    for _ in 0..3 {
        let pts: Vec<(f64, f64)> = onsets
            .iter()
            .filter_map(|o| {
                let k = ((o.t - off) / period).round();
                ((o.t - (off + k * period)).abs() < 0.07 && k >= 0.0).then_some((k, o.t))
            })
            .collect();
        if pts.len() < 4 {
            break;
        }
        let n = pts.len() as f64;
        let (sk, st) = (
            pts.iter().map(|p| p.0).sum::<f64>(),
            pts.iter().map(|p| p.1).sum::<f64>(),
        );
        let skk = pts.iter().map(|p| p.0 * p.0).sum::<f64>();
        let skt = pts.iter().map(|p| p.0 * p.1).sum::<f64>();
        let den = n * skk - sk * sk;
        if den.abs() < 1e-9 {
            break;
        }
        period = (n * skt - sk * st) / den;
        off = (st - period * sk) / n;
    }
    let off = off.rem_euclid(period);
    let bpm = 60.0 / period;
    let beats: Vec<f64> = (0..)
        .map(|i| off + period * i as f64)
        .take_while(|&t| t < duration)
        .map(|t| (t * 1000.0).round() / 1000.0)
        .collect();
    // Downbeats: the one-in-four phase with the strongest beats.
    let strength_at = |t: f64| -> f64 {
        onsets
            .iter()
            .filter(|o| (o.t - t).abs() < 0.06)
            .map(|o| o.strength)
            .fold(0.0, f64::max)
    };
    let bar = (0..4)
        .max_by(|&a, &b| {
            let s = |p: usize| beats.iter().skip(p).step_by(4).map(|&t| strength_at(t)).sum::<f64>();
            s(a).total_cmp(&s(b))
        })
        .unwrap_or(0);
    let downbeats = beats.iter().skip(bar).step_by(4).copied().collect();
    for o in &mut onsets {
        o.t = (o.t * 1000.0).round() / 1000.0;
        o.strength = (o.strength * 1000.0).round() / 1000.0;
    }
    Rhythm {
        bpm: (bpm * 10.0).round() / 10.0,
        beats,
        downbeats,
        onsets,
        duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tempo_and_beats_of_a_click_track() {
        // 128 BPM clicks with a louder click every 4 beats, starting at 0.3 s.
        let bpm = 128.0;
        let period = 60.0 / bpm;
        let dur = 12.0;
        let n = (dur * RATE as f64) as usize;
        let mut samples = vec![0.0f32; n * CHANNELS];
        let mut k = 0;
        loop {
            let t = 0.3 + k as f64 * period;
            if t >= dur - 0.1 {
                break;
            }
            let amp = if k % 4 == 0 { 0.9 } else { 0.5 };
            let at = (t * RATE as f64) as usize;
            for i in 0..2400 {
                let env = (-(i as f64) / 300.0).exp();
                let s = (amp * env * (2.0 * std::f64::consts::PI * 80.0 * i as f64 / RATE as f64).sin()) as f32;
                for c in 0..CHANNELS {
                    samples[(at + i) * CHANNELS + c] += s;
                }
            }
            k += 1;
        }
        let r = analyze(&Pcm { samples }, "all");
        assert!((r.bpm - bpm).abs() < 2.0, "bpm {}", r.bpm);
        // Beats land on the clicks.
        let near = r
            .beats
            .iter()
            .filter(|&&b| ((b - 0.3) / period - ((b - 0.3) / period).round()).abs() * period < 0.03)
            .count();
        assert!(near as f64 >= r.beats.len() as f64 * 0.9, "{near}/{}", r.beats.len());
        // Downbeats on the loud clicks.
        let d0 = r.downbeats[0];
        let idx = ((d0 - 0.3) / period).round() as i64;
        assert_eq!(idx % 4, 0, "downbeat {d0}");
    }
}

//! Rhythm analysis for cutting to music: onsets (spectral flux, every
//! octave weighted alike), tempo (autocorrelation of the onset envelope,
//! backed by its repeats two and four beats on), a beat grid aligned to the
//! strongest onsets, and downbeats: every fourth beat from the bar's first,
//! found from where the kick and the snare fall, where the harmony changes
//! and which beats are accented.

use crate::audio::{CHANNELS, Pcm, RATE};

const N: usize = 1024;
const HOP: usize = 512;
const HOP_S: f64 = HOP as f64 / RATE as f64;

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
    /// Every fourth beat: the first of each bar (where the kick, the
    /// harmony's changes and the accents fall, not the backbeat's snare).
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

/// Frequency of FFT bin `k`.
fn hz(k: usize) -> f64 {
    k as f64 * RATE as f64 / N as f64
}

/// The bins from `lo` up to (not including) `hi` Hz, without DC.
fn bins(lo: f64, hi: f64) -> std::ops::Range<usize> {
    let per = RATE as f64 / N as f64;
    let a = ((lo / per).ceil() as usize).max(1);
    let b = ((hi / per).ceil() as usize).min(N / 2);
    a..b.max(a)
}

/// A band's per-bin weights (zero outside it), averaging 1 inside: flat,
/// or every octave alike (1/f), so the many bins of a hi-hat's octaves
/// count no more than the few of a kick's.
fn weights(range: std::ops::Range<usize>, octaves: bool) -> Vec<f64> {
    let mut w = vec![0.0; N / 2];
    for k in range.clone() {
        w[k] = if octaves { 1.0 / hz(k).max(40.0) } else { 1.0 };
    }
    let mean = w.iter().sum::<f64>() / range.len().max(1) as f64;
    for v in &mut w {
        *v /= mean.max(1e-12);
    }
    w
}

/// Time of the middle of analysis frame `j` (the signal is padded by `N`
/// zeros at the start).
fn frame_time(j: usize) -> f64 {
    j as f64 * HOP_S - (N / 2) as f64 / RATE as f64
}

/// Frames whose middles fall in [a, b].
fn frames_in(a: f64, b: f64, len: usize) -> std::ops::Range<usize> {
    let c = (N / 2) as f64 / RATE as f64;
    let lo = ((a + c) / HOP_S).ceil().max(0.0) as usize;
    let hi = (((b + c) / HOP_S).floor() + 1.0).max(0.0) as usize;
    lo.min(len)..hi.min(len)
}

/// The harmony bins: 80-2000 Hz, bass notes to the top of chords.
fn tone_bins() -> std::ops::Range<usize> {
    bins(80.0, 2000.0)
}

/// What one pass over the spectrum keeps per frame (hop): onset strength
/// (half-wave rectified change of log magnitude, summed over bins) in the
/// asked band and in the bands downbeats are found from, and the harmony
/// bins' log magnitudes.
struct Frames {
    /// The asked band (all of it: every octave alike).
    env: Vec<f64>,
    /// All bands, every octave alike: accents.
    all: Vec<f64>,
    /// 30-120 Hz: kick drums and bass notes.
    kick: Vec<f64>,
    /// 200-6000 Hz, every octave alike: snares and claps.
    snare: Vec<f64>,
    /// [`tone_bins`] log magnitudes, frame after frame.
    tone: Vec<f32>,
}

fn analyse_frames(mono: &[f64], band: &str) -> Frames {
    let window: Vec<f64> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N as f64).cos())
        .collect();
    let (lo, hi) = match band {
        "low" => (20.0, 200.0),
        "mid" => (200.0, 2000.0),
        "high" => (2000.0, 12000.0),
        _ => (20.0, 12000.0),
    };
    let w_env = weights(bins(lo, hi), !matches!(band, "low" | "mid" | "high"));
    let w_all = weights(bins(20.0, 12000.0), true);
    let w_kick = weights(bins(30.0, 120.0), false);
    let w_snare = weights(bins(200.0, 6000.0), true);
    let tone = tone_bins();
    // `N` zeros either side: a hit on the first sample is an onset like any
    // other, and the last frames hold the end.
    let len = mono.len() + 2 * N;
    let at = |i: usize| if i >= N && i < N + mono.len() { mono[i - N] } else { 0.0 };
    let mut prev = vec![0.0; N / 2];
    let mut m = vec![0.0; N / 2];
    let mut f = Frames {
        env: Vec::new(),
        all: Vec::new(),
        kick: Vec::new(),
        snare: Vec::new(),
        tone: Vec::new(),
    };
    let mut s = 0;
    while s + N <= len {
        let mut re: Vec<f64> = (0..N).map(|k| at(s + k) * window[k]).collect();
        let mut im = vec![0.0; N];
        fft(&mut re, &mut im);
        let (mut e, mut a, mut kk, mut sn) = (0.0, 0.0, 0.0, 0.0);
        for k in 1..N / 2 {
            m[k] = (1.0 + 100.0 * (re[k] * re[k] + im[k] * im[k]).sqrt()).ln();
            let d = (m[k] - prev[k]).max(0.0);
            e += w_env[k] * d;
            a += w_all[k] * d;
            kk += w_kick[k] * d;
            sn += w_snare[k] * d;
        }
        f.env.push(e);
        f.all.push(a);
        f.kick.push(kk);
        f.snare.push(sn);
        f.tone.extend(tone.clone().map(|k| m[k] as f32));
        std::mem::swap(&mut prev, &mut m);
        s += HOP;
    }
    f
}

/// Onsets, tempo, beats and downbeats of `pcm` (`band`: all, low, mid,
/// high: the onsets and beats from that band).
pub fn analyze(pcm: &Pcm, band: &str) -> Rhythm {
    let duration = pcm.duration();
    let mono: Vec<f64> = pcm
        .samples
        .chunks_exact(CHANNELS)
        .map(|c| c.iter().map(|&s| s as f64).sum::<f64>() / CHANNELS as f64)
        .collect();
    if mono.len() < N + 7 * HOP {
        return Rhythm {
            bpm: 0.0,
            beats: vec![],
            downbeats: vec![],
            onsets: vec![],
            duration,
        };
    }
    let fr = analyse_frames(&mono, band);
    let env = &fr.env;
    // Onsets: peaks above a local mean, at least 90 ms apart.
    let w = (0.25 / HOP_S) as usize;
    let max = env.iter().cloned().fold(0.0, f64::max).max(1e-9);
    let mut onsets: Vec<Onset> = Vec::new();
    for i in 1..env.len() - 1 {
        let (a, b) = (i.saturating_sub(w), (i + w).min(env.len()));
        let mean = env[a..b].iter().sum::<f64>() / (b - a) as f64;
        if env[i] > env[i - 1] && env[i] >= env[i + 1] && env[i] > mean * 1.4 + max * 0.05 {
            let t = frame_time(i).max(0.0);
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
    // itself, backed by how well it matches itself two and four beats on
    // (a beat repeats in bars; a lag of one and a half beats does not),
    // preferring the middle of the range a little.
    let mean = env.iter().sum::<f64>() / env.len() as f64;
    let e: Vec<f64> = env.iter().map(|v| v - mean).collect();
    let (lag_min, lag_max) = ((60.0 / 190.0 / HOP_S) as usize, (1.0 / HOP_S) as usize);
    let acf: Vec<f64> = (0..=4 * lag_max + 1)
        .map(|lag| {
            if lag == 0 || lag >= e.len() {
                return 0.0;
            }
            (0..e.len() - lag).map(|i| e[i] * e[i + lag]).sum::<f64>() / (e.len() - lag) as f64
        })
        .collect();
    let near = |l: usize| acf[l - 1].max(acf[l]).max(acf[l + 1]);
    let mut best = (lag_min, f64::MIN);
    for lag in lag_min..=lag_max.min(e.len() - 1) {
        let s = acf[lag] + 0.5 * near(2 * lag) + 0.25 * near(4 * lag);
        let bpm = 60.0 / (lag as f64 * HOP_S);
        let prior = (-((bpm / 120.0).log2()).powi(2) / (2.0 * 0.6 * 0.6)).exp();
        if s * prior > best.1 {
            best = (lag, s * prior);
        }
    }
    let period = best.0 as f64 * HOP_S;
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
    let (mut off, mut fit) = (phase.0, period);
    for _ in 0..3 {
        let pts: Vec<(f64, f64)> = onsets
            .iter()
            .filter_map(|o| {
                let k = ((o.t - off) / fit).round();
                ((o.t - (off + k * fit)).abs() < 0.07 && k >= 0.0).then_some((k, o.t))
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
        let p = (n * skt - sk * st) / den;
        // A fit that wanders off the tempo found is no refinement.
        if !(period * 0.9..period * 1.1).contains(&p) {
            break;
        }
        fit = p;
        off = (st - fit * sk) / n;
    }
    // Beats: tracked through the onsets at about that tempo (so they follow
    // the music where a grid would drift off it), and a straight grid
    // through them where the music keeps time. Without beats to track, the
    // grid the onsets fit.
    let (times, period) =
        steady(&track(env, fit), fit, duration).unwrap_or_else(|| (straight(off, fit, duration), fit));
    let bar = bar_start(&fr, &mono, &times, period, duration, &onsets);
    let ms = |t: f64| (t * 1000.0).round() / 1000.0;
    let beats: Vec<f64> = times.iter().map(|&t| ms(t)).collect();
    let downbeats = beats.iter().skip(bar).step_by(4).copied().collect();
    for o in &mut onsets {
        o.t = ms(o.t);
        o.strength = (o.strength * 1000.0).round() / 1000.0;
    }
    Rhythm {
        bpm: (60.0 / period * 10.0).round() / 10.0,
        beats,
        downbeats,
        onsets,
        duration,
    }
}

/// A grid of `period` through `off`, over [0, `duration`). A beat it puts
/// just before the start (within 50 ms) is the first, on the first sample.
fn straight(off: f64, period: f64, duration: f64) -> Vec<f64> {
    let mut off = off.rem_euclid(period);
    if off - period >= -0.05 {
        off -= period;
    }
    (0..)
        .map(|i| (off + period * i as f64).max(0.0))
        .take_while(|&t| t < duration)
        .collect()
}

/// Beats by dynamic programming (Ellis 2007, as librosa tracks them): the
/// frames, each about a beat (`period`, seconds) after the last, that
/// collect the most onset strength, every step penalised by how far its
/// spacing strays from the beat's. Weak beats at either end are dropped.
fn track(env: &[f64], period: f64) -> Vec<f64> {
    let n = env.len();
    let per = period / HOP_S;
    if n < 4 || !(2.0..).contains(&per) {
        return Vec::new();
    }
    let mean = env.iter().sum::<f64>() / n as f64;
    let sd = (env.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / (n - 1) as f64).sqrt();
    let o: Vec<f64> = env.iter().map(|v| v / sd.max(1e-12)).collect();
    // Onset strength gathered around each frame, within about a beat.
    let p = per.round() as i64;
    let win: Vec<f64> = (-p..=p)
        .map(|d| (-0.5 * (d as f64 * 32.0 / per).powi(2)).exp())
        .collect();
    let local: Vec<f64> = (0..n as i64)
        .map(|i| {
            (-p..=p)
                .filter(|d| (0..n as i64).contains(&(i + d)))
                .map(|d| o[(i + d) as usize] * win[(d + p) as usize])
                .sum()
        })
        .collect();
    let lags: Vec<usize> = ((per / 2.0).round().max(1.0) as usize..=(2.0 * per).round() as usize).collect();
    let cost: Vec<f64> = lags.iter().map(|&l| -100.0 * (l as f64 / per).ln().powi(2)).collect();
    let top = local.iter().cloned().fold(f64::MIN, f64::max);
    let mut cum = vec![0.0; n];
    let mut back: Vec<Option<usize>> = vec![None; n];
    let mut started = false;
    for i in 0..n {
        let best = lags
            .iter()
            .zip(&cost)
            .filter(|&(&l, _)| l <= i)
            .map(|(&l, &c)| (c + cum[i - l], i - l))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        cum[i] = local[i] + best.map_or(0.0, |b| b.0);
        // The first beat is where the onsets start.
        if started || local[i] >= 0.01 * top {
            back[i] = best.map(|b| b.1);
            started = true;
        }
    }
    // The last beat: the last peak of the running score above half the
    // peaks' median; then back from it.
    let peaks: Vec<usize> = (1..n - 1)
        .filter(|&i| cum[i] > cum[i - 1] && cum[i] >= cum[i + 1])
        .collect();
    let Some(med) = median(&peaks.iter().map(|&i| cum[i]).collect::<Vec<_>>()) else {
        return Vec::new();
    };
    let Some(&last) = peaks.iter().rev().find(|&&i| 2.0 * cum[i] > med) else {
        return Vec::new();
    };
    let mut beats = vec![last];
    while let Some(j) = back[*beats.last().unwrap()] {
        beats.push(j);
    }
    beats.reverse();
    // Weak beats at the ends (before the music, after it): below half the
    // RMS of the beats' smoothed onset strength.
    let at: Vec<f64> = beats.iter().map(|&j| local[j]).collect();
    let smooth: Vec<f64> = (0..at.len())
        .map(|i| at[i] + 0.5 * (if i > 0 { at[i - 1] } else { 0.0 } + at.get(i + 1).copied().unwrap_or(0.0)))
        .collect();
    let rms = (smooth.iter().map(|v| v * v).sum::<f64>() / smooth.len().max(1) as f64).sqrt();
    if let (Some(a), Some(b)) = (
        smooth.iter().position(|&v| v > 0.5 * rms),
        smooth.iter().rposition(|&v| v > 0.5 * rms),
    ) {
        beats = beats[a..=b].to_vec();
    }
    beats.iter().map(|&j| frame_time(j).max(0.0)).collect()
}

fn median(v: &[f64]) -> Option<f64> {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    (!v.is_empty()).then(|| v[v.len() / 2])
}

/// Tracked beats, settled: those spaced unlike the rest at either end
/// (where the music stops or starts) dropped, and where most of them (four
/// in five) keep one tempo within 35 ms, a straight grid of it through them
/// over the whole track (a stray beat on a syncopated kick goes). With the
/// beat's length. None when there are too few to tell.
fn steady(beats: &[f64], period: f64, duration: f64) -> Option<(Vec<f64>, f64)> {
    let mut b = beats.to_vec();
    while b.len() > 4 {
        let gaps: Vec<f64> = b.windows(2).map(|w| w[1] - w[0]).collect();
        let med = median(&gaps)?;
        if ((b[b.len() - 1] - b[b.len() - 2]) / med - 1.0).abs() > 0.25 {
            b.pop();
        } else if ((b[1] - b[0]) / med - 1.0).abs() > 0.25 {
            b.remove(0);
        } else {
            break;
        }
    }
    if b.len() < 4 {
        return None;
    }
    // Robust fit: each beat numbered by its nearest grid point, those off
    // the grid left out, four times.
    let (mut per, mut off) = (period, b[0]);
    let mut inlier = vec![true; b.len()];
    let mut res = vec![0.0; b.len()];
    for _ in 0..4 {
        let pts: Vec<(f64, f64)> = b
            .iter()
            .zip(&inlier)
            .filter(|&(_, &ok)| ok)
            .map(|(&t, _)| (((t - off) / per).round(), t))
            .collect();
        let n = pts.len() as f64;
        let (sk, st) = (
            pts.iter().map(|p| p.0).sum::<f64>(),
            pts.iter().map(|p| p.1).sum::<f64>(),
        );
        let skk = pts.iter().map(|p| p.0 * p.0).sum::<f64>();
        let skt = pts.iter().map(|p| p.0 * p.1).sum::<f64>();
        let den = n * skk - sk * sk;
        if n < 4.0 || den.abs() < 1e-9 {
            break;
        }
        per = (n * skt - sk * st) / den;
        off = (st - per * sk) / n;
        for (i, &t) in b.iter().enumerate() {
            res[i] = t - (off + per * ((t - off) / per).round());
            inlier[i] = res[i].abs() < 0.035;
        }
    }
    let kept: Vec<f64> = res
        .iter()
        .zip(&inlier)
        .filter(|&(_, &ok)| ok)
        .map(|(&r, _)| r)
        .collect();
    let rms = (kept.iter().map(|r| r * r).sum::<f64>() / kept.len().max(1) as f64).sqrt();
    let held = per > period * 0.9 && per < period * 1.1;
    if held && kept.len() * 5 >= b.len() * 4 && rms < 0.012 {
        return Some((straight(off, per, duration), per));
    }
    // Keeping its own time: the beats as tracked, and their mean tempo.
    let per = if held {
        per
    } else {
        (b[b.len() - 1] - b[0]) / (b.len() - 1) as f64
    };
    Some((b, per))
}

/// Which of the first four beats starts a bar, from what each beat holds:
/// its kick (30-120 Hz) and snare (200-6000 Hz) onsets and all its onset
/// strength just after it, how far its harmony moves from the beat before,
/// and how loud it starts.
fn bar_start(fr: &Frames, mono: &[f64], beats: &[f64], period: f64, duration: f64, onsets: &[Onset]) -> usize {
    let len = fr.env.len();
    let sum_near = |v: &[f64], t: f64| -> f64 { v[frames_in(t - 0.03, t + 0.05, len)].iter().sum() };
    // Only beats whose moment is all inside the track count.
    let n = beats.iter().take_while(|&&t| t + 0.05 <= duration).count();
    let kick: Vec<f64> = beats[..n].iter().map(|&t| sum_near(&fr.kick, t)).collect();
    let snare: Vec<f64> = beats[..n].iter().map(|&t| sum_near(&fr.snare, t)).collect();
    let accent: Vec<f64> = beats[..n].iter().map(|&t| sum_near(&fr.all, t)).collect();
    let loud: Vec<f64> = beats[..n]
        .iter()
        .map(|&t| {
            let a = ((t - 0.01).max(0.0) * RATE as f64) as usize;
            let b = (((t + 0.06) * RATE as f64) as usize).min(mono.len());
            let s = &mono[a.min(b)..b];
            let ms = s.iter().map(|x| x * x).sum::<f64>() / s.len().max(1) as f64;
            10.0 * (ms + 1e-10).log10()
        })
        .collect();
    let change = harmony_change(fr, &beats[..n], period, duration);
    // The beat the music starts on: nearest the first clear onset.
    let first = onsets
        .iter()
        .find(|o| o.strength >= 0.2)
        .map(|o| {
            (0..n)
                .min_by(|&a, &b| (beats[a] - o.t).abs().total_cmp(&(beats[b] - o.t).abs()))
                .unwrap_or(0)
        })
        .unwrap_or(0);
    bar_phase(&kick, &snare, &change, &accent, &loud, first)
}

/// How far each beat's harmony moves from the beat before's: 1 - cosine
/// of their pitch-class profiles (log magnitudes of [`tone_bins`] over
/// the beat, less each bin's mean over the track, so what sounds on every
/// beat, a kick or a held pad, cancels; half-wave rectified). Damped where
/// either profile holds little (a beat only louder or quieter than the
/// rest, not other notes). The first beat has nothing before it: 0.
fn harmony_change(fr: &Frames, beats: &[f64], period: f64, duration: f64) -> Vec<f64> {
    let tone = tone_bins();
    let nb = tone.len();
    let frames = fr.tone.len() / nb.max(1);
    let pc: Vec<usize> = tone
        .clone()
        .map(|k| ((12.0 * (hz(k) / 440.0).log2()).round() as i64).rem_euclid(12) as usize)
        .collect();
    let spans: Vec<Vec<f64>> = beats
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            let e = beats.get(i + 1).copied().unwrap_or((b + period).min(duration));
            let r = frames_in(b, e - 1e-9, frames);
            let mut v = vec![0.0; nb];
            for j in r.clone() {
                for (x, &y) in v.iter_mut().zip(&fr.tone[j * nb..(j + 1) * nb]) {
                    *x += y as f64;
                }
            }
            let c = r.len().max(1) as f64;
            v.iter().map(|x| x / c).collect()
        })
        .collect();
    let mut mean = vec![0.0; nb];
    for s in &spans {
        for (m, x) in mean.iter_mut().zip(s) {
            *m += x / spans.len().max(1) as f64;
        }
    }
    let profiles: Vec<[f64; 12]> = spans
        .iter()
        .map(|s| {
            let mut p = [0.0; 12];
            for ((x, m), &c) in s.iter().zip(&mean).zip(&pc) {
                p[c] += (x - m).max(0.0);
            }
            p
        })
        .collect();
    let size: Vec<f64> = profiles
        .iter()
        .map(|p| p.iter().map(|x| x * x).sum::<f64>().sqrt())
        .collect();
    let typical = size.iter().sum::<f64>() / size.len().max(1) as f64 + 1e-9;
    (0..beats.len())
        .map(|i| {
            if i == 0 {
                return 0.0;
            }
            let cos = (0..12).map(|c| profiles[i][c] * profiles[i - 1][c]).sum::<f64>()
                / ((size[i] + 1e-9) * (size[i - 1] + 1e-9));
            (1.0 - cos) * (size[i].min(size[i - 1]) / typical).min(1.0)
        })
        .collect()
}

/// The bar's first beat (0..4) from per-beat cues, in two steps. First,
/// which beats are 1 and 3: those with the kick, without the snare or clap
/// of the backbeat (each band against its own mean, so a beat only louder
/// in both is not brighter), with the harmony's changes, louder. Then,
/// of those two, the 1: where the harmony changes, the kick and the
/// accents fall, a little toward the beat the music starts on (`first`).
fn bar_phase(kick: &[f64], snare: &[f64], change: &[f64], accent: &[f64], loud: &[f64], first: usize) -> usize {
    let n = kick.len();
    if n < 4 {
        return 0;
    }
    const EPS: f64 = 1e-9;
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (mk, ms) = (mean(kick) + EPS, mean(snare) + EPS);
    let bright: Vec<f64> = (0..n).map(|i| snare[i] / ms - kick[i] / mk).collect();
    let avg = |v: Vec<f64>| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    // Over beat pairs (b, b + 1) from `h`: on-beat against off-beat.
    let ratio = |v: &[f64], h: usize| {
        avg((h..n - 1)
            .step_by(2)
            .map(|b| (v[b] - v[b + 1]) / (v[b] + v[b + 1] + EPS))
            .collect())
    };
    let diff = |v: &[f64], h: usize, from: usize| {
        avg((h..n - 1)
            .step_by(2)
            .filter(|&b| b >= from)
            .map(|b| v[b] - v[b + 1])
            .collect())
    };
    let onbeat = |h: usize| ratio(kick, h) - 0.5 * diff(&bright, h, 0) + diff(change, h, 1) + 0.05 * diff(loud, h, 0);
    let h = if onbeat(0) >= onbeat(1) { 0 } else { 1 };
    // Over bars from `p`: its beat 1 against its beat 3.
    let one = |p: usize| {
        let bars = || (p..n - 2).step_by(4);
        avg(bars().filter(|&b| b >= 1).map(|b| change[b] - change[b + 2]).collect())
            + avg(bars()
                .map(|b| (kick[b] - kick[b + 2]) / (kick[b] + kick[b + 2] + EPS))
                .collect())
            + avg(bars()
                .map(|b| (accent[b] - accent[b + 2]) / (accent[b] + accent[b + 2] + EPS))
                .collect())
            + 0.05 * avg(bars().map(|b| loud[b] - loud[b + 2]).collect())
            + if first % 4 == p { 0.1 } else { 0.0 }
    };
    if one(h + 2) > one(h) { h + 2 } else { h }
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

    /// A small deterministic noise source.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        }
    }

    /// Drum sounds, mixed into a mono track.
    struct Track {
        mono: Vec<f64>,
        noise: Noise,
    }

    impl Track {
        fn new(seconds: f64) -> Self {
            Track {
                mono: vec![0.0; (seconds * RATE as f64) as usize],
                noise: Noise(0x9e37_79b9_7f4a_7c15),
            }
        }
        fn add(&mut self, t: f64, mut sound: impl FnMut(f64, &mut Noise) -> f64, secs: f64) {
            let at = (t * RATE as f64).round() as usize;
            for i in 0..(secs * RATE as f64) as usize {
                if at + i >= self.mono.len() {
                    break;
                }
                let v = sound(i as f64 / RATE as f64, &mut self.noise);
                self.mono[at + i] += v;
            }
        }
        fn kick(&mut self, t: f64) {
            // A sine falling from 140 to 50 Hz.
            let mut phase = 0.0;
            self.add(
                t,
                move |x, _| {
                    phase += 2.0 * std::f64::consts::PI * (50.0 + 90.0 * (-x * 30.0).exp()) / RATE as f64;
                    0.9 * phase.sin() * (-x * 9.0).exp()
                },
                0.35,
            );
        }
        fn snare(&mut self, t: f64) {
            self.add(
                t,
                |x, n| (0.45 * n.next() + 0.3 * (2.0 * std::f64::consts::PI * 190.0 * x).sin()) * (-x * 18.0).exp(),
                0.25,
            );
        }
        fn hat(&mut self, t: f64, open: bool) {
            let mut last = 0.0;
            self.add(
                t,
                move |x, n| {
                    let v = n.next();
                    let d = v - last;
                    last = v;
                    0.12 * d * (-x * if open { 12.0 } else { 90.0 }).exp()
                },
                if open { 0.25 } else { 0.05 },
            );
        }
        fn pcm(&self) -> Pcm {
            let peak = self.mono.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-9);
            Pcm {
                samples: self
                    .mono
                    .iter()
                    .flat_map(|v| [(v / peak * 0.8) as f32; CHANNELS])
                    .collect(),
            }
        }
    }

    /// A backbeat (kick on 1 and 3, snare on 2 and 4, hi-hats on eighths)
    /// from the first sample: a beat at 0, and downbeats on the kicks of
    /// beat 1, not on the snares, which are louder in spectral flux (the
    /// strongest beats once picked the bar).
    #[test]
    fn downbeats_land_on_the_one_of_a_backbeat() {
        let beat = 0.5;
        let mut tr = Track::new(8.0);
        for b in 0..16 {
            let t = b as f64 * beat;
            if b % 2 == 0 {
                tr.kick(t);
            } else {
                tr.snare(t);
            }
            tr.hat(t, false);
            tr.hat(t + beat / 2.0, false);
        }
        let pcm = tr.pcm();
        for band in ["all", "low"] {
            let r = analyze(&pcm, band);
            assert!((r.bpm - 120.0).abs() < 1.0, "{band}: bpm {}", r.bpm);
            assert_eq!(r.beats[0], 0.0, "{band}: {:?}", r.beats);
            assert!(r.onsets[0].t < 0.03, "{band}: {:?}", r.onsets[0]);
            assert!(r.downbeats.len() >= 4, "{band}: {:?}", r.downbeats);
            for (i, d) in r.downbeats.iter().enumerate() {
                assert!((d - 2.0 * i as f64).abs() < 0.03, "{band}: downbeats {:?}", r.downbeats);
            }
        }
    }

    /// Four on the floor with the open hat and the bass on the offbeats:
    /// the beats are the kicks, not the offbeats that fill more of the
    /// spectrum, and the bar starts where the clap (on 2 and 4) isn't.
    #[test]
    fn beats_of_four_on_the_floor_are_the_kicks() {
        let (bpm, start) = (124.0, 0.25);
        let beat = 60.0 / bpm;
        let mut tr = Track::new(start + 32.0 * beat);
        for b in 0..32 {
            let t = start + b as f64 * beat;
            tr.kick(t);
            if b % 2 == 1 {
                tr.add(
                    t,
                    |x, n| 0.5 * n.next() * ((-x * 25.0).exp() + 0.6 * f64::from(x < 0.03)),
                    0.2,
                );
            }
            tr.hat(t + beat / 2.0, true);
            tr.hat(t + beat / 4.0, false);
            tr.hat(t + 3.0 * beat / 4.0, false);
            tr.add(
                t + beat / 2.0,
                |x, _| 0.5 * (2.0 * std::f64::consts::PI * 55.0 * x).sin() * (-x * 6.0).exp(),
                0.2,
            );
        }
        let r = analyze(&tr.pcm(), "all");
        assert!((r.bpm - bpm).abs() < 1.0, "bpm {}", r.bpm);
        let off = |t: f64| {
            let k = ((t - start) / beat).round();
            (t - start - k * beat).abs()
        };
        let on = r.beats.iter().filter(|&&b| b >= start && off(b) < 0.03).count();
        assert!(on as f64 >= 0.9 * r.beats.len() as f64, "beats {:?}", r.beats);
        for d in &r.downbeats {
            let k = ((d - start) / beat).round() as i64;
            assert!(off(*d) < 0.03 && k % 2 == 0, "downbeats {:?}", r.downbeats);
        }
    }

    /// Kick and snare at `bpm(t)`, hi-hats between, from 0 to `secs`:
    /// the track and the times of its beats.
    fn drums(secs: f64, bpm: impl Fn(f64) -> f64) -> (Track, Vec<f64>) {
        let mut tr = Track::new(secs);
        let (mut t, mut k, mut beats) = (0.0, 0, Vec::new());
        while t < secs - 0.3 {
            let beat = 60.0 / bpm(t);
            if k % 2 == 0 {
                tr.kick(t);
            } else {
                tr.snare(t);
            }
            tr.hat(t + beat / 2.0, false);
            beats.push(t);
            t += beat;
            k += 1;
        }
        (tr, beats)
    }

    /// The beats found that are within `tol` of a true beat.
    fn on_beat(found: &[f64], truth: &[f64], tol: f64) -> usize {
        found
            .iter()
            .filter(|&&b| truth.iter().any(|&t| (t - b).abs() < tol))
            .count()
    }

    /// A minute at a tempo that is no whole number of analysis hops: the
    /// beats stay on the drums to the end, not drifting off them as a grid
    /// a fraction of a percent fast does.
    #[test]
    fn beats_stay_on_the_drums_for_a_minute() {
        let (tr, _) = drums(60.0, |_| 97.0);
        let r = analyze(&tr.pcm(), "all");
        // A steady tempo's grid runs to the end.
        let truth: Vec<f64> = (0..98).map(|k| k as f64 * 60.0 / 97.0).collect();
        assert!((r.bpm - 97.0).abs() < 0.3, "bpm {}", r.bpm);
        let last = r.beats.iter().filter(|&&b| b > 50.0).copied().collect::<Vec<_>>();
        assert!(
            !last.is_empty() && on_beat(&last, &truth, 0.025) == last.len(),
            "{last:?}"
        );
        assert!(on_beat(&r.beats, &truth, 0.025) as f64 >= 0.97 * r.beats.len() as f64);
    }

    /// Drums that speed up from 120 to 126 BPM halfway: the beats follow.
    #[test]
    fn beats_follow_a_change_of_tempo() {
        let (tr, truth) = drums(40.0, |t| if t < 20.0 { 120.0 } else { 126.0 });
        let r = analyze(&tr.pcm(), "all");
        assert!(
            on_beat(&r.beats, &truth, 0.035) as f64 >= 0.9 * r.beats.len() as f64,
            "{:?}",
            r.beats
        );
        assert!(r.beats.iter().filter(|&&b| b > 30.0).count() >= 15, "{:?}", r.beats);
    }
}

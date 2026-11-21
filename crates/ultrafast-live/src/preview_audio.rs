//! Sound for the preview window: the document's `audio` nodes mixed on the
//! fly into the default output device.
//!
//! While sound plays, the audio device is the master clock, so picture and
//! sound cannot drift apart. Edits rebuild the mix on a background thread
//! (decoded sources stay cached) and swap it in without a gap; seeks and
//! play/pause realign the device position to the picture.

use crate::Session;
use crate::audio_mix::AudioMix;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use ultrafast_media::audio::{CHANNELS, Pcm, RATE};

struct Shared {
    mix: Mutex<Option<Arc<AudioMix>>>,
    /// Next output frame (48 kHz) the callback will mix.
    pos: AtomicU64,
    /// Document length in 48 kHz frames; playback loops at it.
    end: AtomicU64,
    playing: AtomicBool,
    muted: AtomicBool,
    /// Output latency (callback to speaker) in microseconds.
    latency_us: AtomicU64,
}

type Built = Result<Option<AudioMix>, String>;

pub struct PreviewAudio {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
    /// Hash of the audio-relevant part of the document the current (or
    /// building) mix was made from.
    key: Option<u64>,
    revision: Option<(u64, u64)>,
    building: Option<Receiver<Built>>,
    /// Decoded sources by (path, playback rate bits).
    cache: Arc<Mutex<HashMap<(PathBuf, u64), Arc<Pcm>>>>,
    has_mix: bool,
    epoch: u64,
    was_playing: bool,
}

impl PreviewAudio {
    /// Opens the default output device. Errors if there is none usable.
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("output device wants {:?} samples", supported.sample_format()));
        }
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let device_rate = config.sample_rate as f64;
        let shared = Arc::new(Shared {
            mix: Mutex::new(None),
            pos: AtomicU64::new(0),
            end: AtomicU64::new(u64::MAX),
            playing: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            latency_us: AtomicU64::new(0),
        });
        let sh = shared.clone();
        // Source frames (48 kHz) per device frame.
        let step = RATE as f64 / device_rate;
        let mut scratch: Vec<f32> = Vec::new();
        let mut frac = 0.0f64;
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    let ts = info.timestamp();
                    let lat = ts.playback.duration_since(ts.callback).as_micros() as u64;
                    sh.latency_us.store(lat, Ordering::Relaxed);
                    let frames = out.len() / channels;
                    let mix = sh.mix.try_lock().ok().and_then(|m| m.clone());
                    let (Some(mix), true) = (mix, sh.playing.load(Ordering::Relaxed)) else {
                        out.fill(0.0);
                        return;
                    };
                    let gain = if sh.muted.load(Ordering::Relaxed) { 0.0 } else { 1.0 };
                    let end = sh.end.load(Ordering::Relaxed).max(1);
                    let raw = sh.pos.load(Ordering::Relaxed);
                    let pos = raw % end;
                    // Source frames this buffer spans (+1 for interpolation).
                    let span = (frac + frames as f64 * step).ceil() as usize + 1;
                    scratch.resize(span * CHANNELS, 0.0);
                    // Mix in pieces that wrap at the loop point.
                    let mut done = 0usize;
                    while done < span {
                        let at = (pos + done as u64) % end;
                        let n = (span - done).min((end - at) as usize);
                        mix.render_into(at as usize, &mut scratch[done * CHANNELS..(done + n) * CHANNELS]);
                        done += n;
                    }
                    let lerp = |k: usize, f: f32| scratch[k] + (scratch[k + CHANNELS] - scratch[k]) * f;
                    for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
                        let x = frac + i as f64 * step;
                        let j = x.floor() as usize;
                        let f = (x - j as f64) as f32;
                        let (l, r) = (lerp(j * CHANNELS, f), lerp(j * CHANNELS + 1, f));
                        if channels == 1 {
                            frame[0] = (l + r) * 0.5 * gain;
                        } else {
                            frame[0] = l * gain;
                            frame[1] = r * gain;
                            frame[2..].fill(0.0);
                        }
                    }
                    let advance = frac + frames as f64 * step;
                    let whole = advance.floor();
                    frac = advance - whole;
                    // Advance unless a seek moved the position meanwhile.
                    let _ =
                        sh.pos
                            .compare_exchange(raw, (pos + whole as u64) % end, Ordering::Relaxed, Ordering::Relaxed);
                },
                |e| eprintln!("ultrafast: audio: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            _stream: stream,
            key: None,
            revision: None,
            building: None,
            cache: Arc::new(Mutex::new(HashMap::new())),
            has_mix: false,
            epoch: u64::MAX,
            was_playing: false,
        })
    }

    /// Silences the output (the clock keeps running), or un-silences it.
    pub fn toggle_mute(&self) {
        self.shared.muted.fetch_xor(true, Ordering::Relaxed);
    }

    /// True while the device is playing the document's sound (and so is
    /// the clock).
    pub fn active(&self) -> bool {
        self.was_playing
    }

    /// Call once per frame before rendering: keeps the mix current with the
    /// document, follows play/pause/seek, and returns the time to render.
    pub fn sync(&mut self, session: &mut Session) -> f64 {
        self.refresh_mix(session);
        let fmt = session.doc.format;
        self.shared
            .end
            .store((fmt.duration * RATE as f64).round().max(1.0) as u64, Ordering::Relaxed);
        let playing = self.has_mix && session.is_playing() && (session.rate() - 1.0).abs() < 1e-9;
        let latency = self.shared.latency_us.load(Ordering::Relaxed) as f64 / 1e6;
        if session.epoch() != self.epoch || (playing && !self.was_playing) {
            // Seek, play, pause or a new document: the picture leads. Start
            // the device ahead by its latency so the sound lands on time.
            self.epoch = session.epoch();
            let start = ((session.time() + latency) * RATE as f64).round().max(0.0) as u64;
            self.shared.pos.store(start, Ordering::Relaxed);
        }
        self.was_playing = playing;
        self.shared.playing.store(playing, Ordering::Relaxed);
        if !playing {
            return session.time();
        }
        // The device leads: what is audible now was mixed `latency` ago.
        let end = self.shared.end.load(Ordering::Relaxed) as f64;
        let pos = self.shared.pos.load(Ordering::Relaxed) as f64;
        let t = ((pos / RATE as f64 - latency).rem_euclid(end / RATE as f64)).max(0.0);
        session.set_time(t);
        t
    }

    fn refresh_mix(&mut self, session: &Session) {
        // Swap in a finished rebuild.
        if let Some(rx) = &self.building {
            match rx.try_recv() {
                Ok(Ok(mix)) => {
                    self.has_mix = mix.is_some();
                    *self.shared.mix.lock().unwrap() = mix.map(Arc::new);
                    self.building = None;
                }
                Ok(Err(e)) => {
                    eprintln!("ultrafast: audio: {e}");
                    self.building = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.building = None,
            }
        }
        let rev = (session.doc.instance, session.doc.revision);
        if self.revision == Some(rev) {
            return;
        }
        self.revision = Some(rev);
        let key = audio_key(session);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        if key == EMPTY {
            self.has_mix = false;
            *self.shared.mix.lock().unwrap() = None;
            self.building = None;
            return;
        }
        let doc = session.doc.clone();
        let dir = session.base_dir();
        let cache = self.cache.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = AudioMix::from_doc_with(&doc, &dir, |path, rate| {
                let key = (path.to_path_buf(), rate.to_bits());
                if let Some(p) = cache.lock().unwrap().get(&key) {
                    return Ok(Some(p.clone()));
                }
                let p = ultrafast_media::audio::decode_rate(path, rate)
                    .map_err(|e| e.to_string())?
                    .map(Arc::new);
                if let Some(p) = &p {
                    cache.lock().unwrap().insert(key, p.clone());
                }
                Ok(p)
            });
            let _ = tx.send(r);
        });
        self.building = Some(rx);
    }
}

const EMPTY: u64 = 0;

/// Hash of everything the mix depends on: audio nodes (after flattening, so
/// component timing counts) and the base directory. 0 if there is no audio.
fn audio_key(session: &Session) -> u64 {
    let doc = &session.doc;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    let flat;
    let (nodes, chains) = if doc.needs_flatten() {
        match doc.flatten() {
            Ok(f) => {
                flat = f;
                (&flat.doc.nodes, Some(&flat.chains))
            }
            Err(_) => return EMPTY,
        }
    } else {
        (&doc.nodes, None)
    };
    let mut ids: Vec<&String> = nodes
        .iter()
        .filter(|(_, n)| n.op == "audio")
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    for id in ids {
        any = true;
        id.hash(&mut h);
        serde_json::to_string(&nodes[id]).unwrap_or_default().hash(&mut h);
        if let Some(c) = chains.and_then(|c| c.get(id)) {
            serde_json::to_string(c).unwrap_or_default().hash(&mut h);
        }
    }
    if !any {
        return EMPTY;
    }
    session.base_dir().hash(&mut h);
    h.finish().max(1)
}

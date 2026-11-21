//! Realtime preview window. Plays the document in a loop at display rate,
//! applies agent requests between frames, and reloads the document file
//! (and any image or video under it) when it changes on disk. Paused with
//! nothing changing, it draws nothing and sleeps, waking every 50 ms for
//! requests and files.
//!
//! Sound: the document's `audio` nodes play through the default output
//! device (feature `audio`), which then drives the playback clock.
//!
//! Player: a timeline along the bottom (click or drag to scrub, wheel to
//! step frames), and keys: space play/pause, left/right 1 s (shift: one
//! frame), `,` `.` one frame, home restart, `i` / `o` loop in / out at the
//! playhead, `x` clear the loop, `1` `2` `3` half, normal and double speed,
//! `h` hide the timeline, `f` fullscreen, `m` mute, escape quit.

use crate::Session;
use crate::server::Request;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use ultrafast_gpu::{Submitted, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Fullscreen, Window, WindowId};

/// How long an edit's outline fades for.
const EDIT_FADE: f64 = 1.4;

struct App {
    session: Session,
    requests: Option<Receiver<Request>>,
    window: Option<Window>,
    surface: Option<Surface>,
    inflight: VecDeque<(Submitted, f64)>,
    modifiers: ModifiersState,
    last_title: Instant,
    title: String,
    hud: bool,
    cursor: (f64, f64),
    scrubbing: bool,
    /// The reference video while compare is toggled off (`c`).
    parked: Option<std::path::PathBuf>,
    /// What the picture on screen was drawn from; paused, it is drawn again
    /// only when this changes.
    shown: Option<Shown>,
    /// The last picture was not final (kernels compiling, media still
    /// decoding): draw again at this time even though nothing changed.
    retry_at: Option<Instant>,
    #[cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
    audio: Option<crate::preview_audio::PreviewAudio>,
}

/// Opens the preview window and runs until it is closed.
pub fn run(session: Session, requests: Option<Receiver<Request>>, title: &str) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        session,
        requests,
        window: None,
        surface: None,
        inflight: VecDeque::new(),
        modifiers: ModifiersState::empty(),
        last_title: Instant::now(),
        title: title.to_string(),
        hud: true,
        cursor: (0.0, 0.0),
        scrubbing: false,
        parked: None,
        shown: None,
        retry_at: None,
        #[cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
        audio: crate::preview_audio::PreviewAudio::new()
            .inspect_err(|e| eprintln!("ultrafast: preview audio off: {e}"))
            .ok(),
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

/// Everything the picture on screen depends on besides the running clock.
#[derive(Clone, PartialEq)]
struct Shown {
    t: u64,
    doc: (u64, u64),
    media: u64,
    size: (u32, u32),
    hud: bool,
    emphasis: bool,
    playing: bool,
    loop_range: Option<(u64, u64)>,
    selection: Option<String>,
    compare: Option<String>,
}

impl App {
    /// What the next picture would be drawn from.
    fn shown_now(&self) -> Shown {
        let size = self
            .window
            .as_ref()
            .map(|w| w.inner_size())
            .map(|s| (s.width, s.height))
            .unwrap_or((0, 0));
        Shown {
            t: self.session.time().to_bits(),
            doc: (self.session.doc.instance, self.session.doc.revision),
            media: self.session.media_epoch(),
            size,
            hud: self.hud,
            emphasis: self.scrubbing || self.near_timeline(),
            playing: self.session.is_playing(),
            loop_range: self.session.loop_range().map(|(a, b)| (a.to_bits(), b.to_bits())),
            selection: self
                .session
                .selection
                .as_ref()
                .map(|p| format!("{} {:?}", p.part, p.bounds)),
            compare: self.session.compare().map(|c| format!("{c:?}")),
        }
    }

    /// Whether the window has to draw again: playing, scrubbing, an edit's
    /// outline still fading, or anything the picture depends on changed.
    fn needs_frame(&mut self) -> bool {
        self.session.is_playing()
            || self.scrubbing
            || !self.session.recent_edits(EDIT_FADE).is_empty()
            || self.retry_at.is_some_and(|at| Instant::now() >= at)
            || self.shown.as_ref() != Some(&self.shown_now())
    }

    /// Whether the pointer is over the timeline strip.
    fn near_timeline(&self) -> bool {
        let Some(w) = &self.window else { return false };
        let h = w.inner_size().height as f64;
        self.hud && self.cursor.1 > h - 60.0
    }

    /// The frame pixel under the pointer, if it is over the picture.
    fn frame_point(&self) -> Option<(f32, f32)> {
        let w = self.window.as_ref()?;
        let size = w.inner_size();
        let fmt = self.session.doc.format;
        let (dw, dh) = (size.width.max(1) as f64, size.height.max(1) as f64);
        let s = (dw / fmt.width as f64).min(dh / fmt.height as f64);
        let (x0, y0) = ((dw - fmt.width as f64 * s) / 2.0, (dh - fmt.height as f64 * s) / 2.0);
        let (x, y) = ((self.cursor.0 - x0) / s, (self.cursor.1 - y0) / s);
        (x >= 0.0 && y >= 0.0 && x < fmt.width as f64 && y < fmt.height as f64).then_some((x as f32, y as f32))
    }

    /// Seeks to the time under the pointer's x on the timeline.
    fn scrub_to_cursor(&mut self) {
        let Some(w) = &self.window else { return };
        let width = w.inner_size().width as f64;
        let u = ((self.cursor.0 - 24.0) / (width - 48.0).max(1.0)).clamp(0.0, 1.0);
        let d = self.session.doc.format.duration;
        self.session.seek(u * d);
    }

    fn drain_requests(&mut self) {
        if let Some(rx) = &self.requests {
            while let Ok(req) = rx.try_recv() {
                let resp = self.session.handle(&req.body);
                let _ = req.reply.send(resp);
            }
        }
        self.session.poll_file();
    }

    fn collect_timings(&mut self) {
        while let Some((s, _)) = self.inflight.front() {
            match s.try_timing() {
                Some(r) => {
                    let (_, cpu) = self.inflight.pop_front().unwrap();
                    self.session.record_frame(cpu, r.map(|t| t.gpu_ms).unwrap_or(0.0));
                }
                None => break,
            }
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &self.surface) else {
            return;
        };
        let size = window.inner_size();
        let Some(drawable) = surface.next() else { return };
        let t = self.session.advance();
        // With sound playing, the audio device is the clock.
        #[cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
        let t = match self.audio.as_mut() {
            Some(a) => a.sync(&mut self.session),
            None => t,
        };
        let fmt = self.session.doc.format;
        // Letterbox the frame into the drawable.
        let (dw, dh) = (size.width.max(1) as f32, size.height.max(1) as f32);
        let s = (dw / fmt.width as f32).min(dh / fmt.height as f32);
        let (rw, rh) = (fmt.width as f32 * s, fmt.height as f32 * s);
        let rect = [(dw - rw) / 2.0, (dh - rh) / 2.0, rw, rh];
        let dur = fmt.duration.max(1e-6);
        let (loop_in, loop_out) = self
            .session
            .loop_range()
            .map(|(a, b)| (a / dur, b / dur))
            .unwrap_or((0.0, 0.0));
        let playing = self.session.is_playing();
        let hud = self.hud;
        let emphasis = if self.scrubbing || self.near_timeline() {
            1.0f32
        } else {
            0.0
        };
        // Recent agent edits: outline what changed (the whole picture when it
        // has no vector bounds), fading out.
        let mut marks: Vec<[f32; 5]> = Vec::new();
        for (node, age) in self.session.recent_edits(EDIT_FADE).iter().rev().take(4) {
            let a = (1.0 - age / EDIT_FADE) as f32;
            let r = match self.session.renderer.content_bounds(node) {
                Some(b) => [
                    rect[0] + b[0] * s,
                    rect[1] + b[1] * s,
                    rect[0] + b[2] * s,
                    rect[1] + b[3] * s,
                ],
                None => [rect[0], rect[1], rect[0] + rw, rect[1] + rh],
            };
            marks.push([r[0] - 6.0, r[1] - 6.0, r[2] + 6.0, r[3] + 6.0, a]);
        }
        // What the person (or an agent) pointed at: a steady outline.
        if let Some(sel) = &self.session.selection {
            let b = sel.bounds;
            let r = [
                rect[0] + b[0] * s,
                rect[1] + b[1] * s,
                rect[0] + b[2] * s,
                rect[1] + b[3] * s,
            ];
            marks.truncate(3);
            marks.push([r[0] - 6.0, r[1] - 6.0, r[2] + 6.0, r[3] + 6.0, -1.0]);
        }
        let session = &mut self.session;
        let (renderer, view) = session.view();
        // Motion blur costs a render per sample: playing or scrubbing shows
        // one sample a frame, a stopped frame the full exposure.
        renderer.set_motion_blur(!playing && !self.scrubbing);
        let result = renderer.submit_frame_with(view, t, |o| {
            let mut p = o.header.to_vec();
            p.extend(o.origin);
            p.extend(rect);
            p.extend([dw, dh, 0.07, 0.07, 0.08]);
            p.extend([
                hud as u8 as f32,
                (t / dur) as f32,
                loop_in as f32,
                loop_out as f32,
                emphasis,
                playing as u8 as f32,
            ]);
            p.push(marks.len() as f32);
            for k in 0..4 {
                p.extend(marks.get(k).copied().unwrap_or([0.0; 5]));
            }
            o.list.dispatch_ex(
                &o.kernels.utility("k_present"),
                &drawable.texture,
                &[o.texture],
                &p,
                ultrafast_gpu::Bindings {
                    grid: Some((dw as u32, dh as u32)),
                    ..Default::default()
                },
            );
            o.list.present(&drawable);
        });
        match result {
            Ok((sub, report)) => {
                self.inflight.push_back((sub, report.cpu_encode_ms));
                self.shown = Some(self.shown_now());
                // Not the final picture yet: look again shortly.
                let pending = report.groups_fallback + report.media_pending > 0;
                self.retry_at = pending.then(|| Instant::now() + Duration::from_millis(60));
            }
            Err(e) => {
                // Keep running: the agent can fix the document.
                if self.last_title.elapsed().as_secs_f64() > 1.0 {
                    eprintln!("ultrafast: frame failed: {e}");
                }
            }
        }
        self.collect_timings();
        if self.last_title.elapsed().as_secs_f64() > 0.5 {
            self.last_title = Instant::now();
            let st = self.session.stats();
            let gpu = st["gpu_ms"]["mean"].as_f64().unwrap_or(0.0);
            let cpu = st["cpu_ms"]["mean"].as_f64().unwrap_or(0.0);
            let Some(window) = &self.window else { return };
            let sel = self
                .session
                .selection
                .as_ref()
                .map(|p| format!(" · selected {} ({})", p.node, p.op))
                .unwrap_or_default();
            window.set_title(&format!(
                "ultrafast · {} · t={:.2}s · gpu {:.2} ms · cpu {:.2} ms{sel}",
                self.title,
                self.session.time(),
                gpu,
                cpu
            ));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let fmt = self.session.doc.format;
        let scale = (1280.0 / fmt.width as f64).min(800.0 / fmt.height as f64).min(1.0);
        let attrs = Window::default_attributes()
            .with_title(format!("ultrafast · {}", self.title))
            .with_inner_size(LogicalSize::new(fmt.width as f64 * scale, fmt.height as f64 * scale));
        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("ultrafast: cannot open window: {e}");
                el.exit();
                return;
            }
        };
        let handle = window.window_handle().expect("window handle");
        let device = self.session.renderer.device();
        let surface = match handle.as_raw() {
            #[cfg(target_os = "macos")]
            RawWindowHandle::AppKit(h) => unsafe { Surface::from_ns_view(device, h.ns_view.as_ptr()) },
            #[cfg(windows)]
            RawWindowHandle::Win32(h) => {
                let hinstance = h
                    .hinstance
                    .map_or(std::ptr::null_mut(), |v| v.get() as *mut std::ffi::c_void);
                match unsafe { Surface::from_win32(device, hinstance, h.hwnd.get() as *mut std::ffi::c_void) } {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("ultrafast: preview window: {e}");
                        el.exit();
                        return;
                    }
                }
            }
            _ => {
                eprintln!("ultrafast: the preview window needs macOS or Windows");
                el.exit();
                return;
            }
        };
        let size = window.inner_size();
        surface.resize(size.width, size.height);
        self.surface = Some(surface);
        self.window = Some(window);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(s) = &self.surface {
                    s.resize(size.width, size.height);
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let step = if self.modifiers.shift_key() {
                    1.0 / self.session.doc.format.fps
                } else {
                    1.0
                };
                let frame = 1.0 / self.session.doc.format.fps;
                let now = self.session.time();
                if let Key::Character(c) = &logical_key {
                    match c.as_str() {
                        "," => self.session.seek(now - frame),
                        "." => self.session.seek(now + frame),
                        "i" => {
                            let end = self
                                .session
                                .loop_range()
                                .map(|r| r.1)
                                .unwrap_or(self.session.doc.format.duration);
                            self.session.set_loop(Some((now, end)));
                        }
                        "o" => {
                            let start = self.session.loop_range().map(|r| r.0).unwrap_or(0.0);
                            self.session.set_loop(Some((start, now)));
                        }
                        "x" => self.session.set_loop(None),
                        "1" => self.session.set_rate(0.5),
                        "2" => self.session.set_rate(1.0),
                        "3" => self.session.set_rate(2.0),
                        "h" => self.hud = !self.hud,
                        // With a reference video: split, difference, off.
                        "c" => {
                            use crate::CompareMode;
                            let next = match self.session.compare() {
                                Some((v, CompareMode::Split(_))) => Some((v.clone(), CompareMode::Difference)),
                                Some((v, CompareMode::Difference)) => {
                                    self.parked = Some(v.clone());
                                    None
                                }
                                None => self.parked.take().map(|v| (v, CompareMode::Split(0.5))),
                            };
                            self.session.set_compare(next);
                        }
                        "f" => {
                            if let Some(w) = &self.window {
                                let full = w.fullscreen().is_some();
                                w.set_fullscreen((!full).then_some(Fullscreen::Borderless(None)));
                            }
                        }
                        #[cfg(all(feature = "audio", any(all(target_os = "macos", not(feature = "vulkan")), windows)))]
                        "m" => {
                            if let Some(a) = &self.audio {
                                a.toggle_mute();
                            }
                        }
                        _ => {}
                    }
                }
                match logical_key {
                    Key::Named(NamedKey::Space) => self.session.toggle_play(),
                    Key::Named(NamedKey::ArrowRight) => self.session.seek(self.session.time() + step),
                    Key::Named(NamedKey::ArrowLeft) => self.session.seek(self.session.time() - step),
                    Key::Named(NamedKey::Home) => self.session.seek(0.0),
                    Key::Named(NamedKey::Escape) => el.exit(),
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if self.scrubbing {
                    self.scrub_to_cursor();
                } else if let Some((v, crate::CompareMode::Split(_))) = self.session.compare().cloned()
                    && !self.near_timeline()
                    && let Some((x, _)) = self.frame_point()
                {
                    // The divider follows the pointer over the picture.
                    let fw = self.session.doc.format.width as f32;
                    self.session
                        .set_compare(Some((v, crate::CompareMode::Split((x / fw).clamp(0.0, 1.0)))));
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed if self.near_timeline() => {
                    self.scrubbing = true;
                    self.scrub_to_cursor();
                }
                // Option-click points at what is under the cursor (for an
                // agent to read with `selection`); on empty picture it clears.
                ElementState::Pressed if self.modifiers.alt_key() => {
                    if let Some((x, y)) = self.frame_point() {
                        self.session.select_at(x, y);
                        if let Some(p) = &self.session.selection {
                            eprintln!("ultrafast: selected {} ({}) at {:.0},{:.0}", p.node, p.op, x, y);
                        }
                    }
                }
                ElementState::Pressed => self.session.toggle_play(),
                ElementState::Released => self.scrubbing = false,
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 30.0,
                };
                let frame = 1.0 / self.session.doc.format.fps;
                self.session.seek(self.session.time() - lines.round() * frame);
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.drain_requests();
        self.session.poll_media(true);
        if self.session.quit {
            el.exit();
            return;
        }
        if self.needs_frame() {
            el.set_control_flow(ControlFlow::Poll);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            // Nothing to draw: sleep, waking for requests and file changes.
            self.collect_timings();
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50)));
        }
    }
}

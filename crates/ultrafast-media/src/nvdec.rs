//! NVDEC: H.264 in MP4 decoded by the GPU's video engines straight into
//! memory the renderer reads.
//!
//! Samples are demuxed in process (no ffmpeg), the hardware decoder writes
//! NV12 surfaces, and each displayed picture is copied device to device
//! into a Vulkan buffer that CUDA shares: no frame crosses PCIe or touches
//! the CPU. Sources it cannot take (other containers or codecs, 10-bit,
//! HDR, odd sizes) decode through ffmpeg instead.
//!
//! libnvcuvid comes from the driver and is loaded at runtime.

use crate::nvenc::{self, Cuda, MEMORY_DEVICE, Memcpy2d};
use crate::nvenc_sys as sys;
use crate::{FrameFormat, Layout, MediaError, VideoInfo};
use libloading::Library;
use std::collections::VecDeque;
use std::ffi::{c_ulong, c_void};
use std::path::Path;
use ultrafast_gpu::{Buffer, Device};

// nvcuvid.h / cuviddec.h (Video Codec SDK 12): the parts used here, laid
// out as the C structs (checked against bindgen output of the headers).

type Parser = *mut c_void;
type Decoder = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect32 {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect16 {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

/// CUVIDEOFORMAT.
#[repr(C)]
struct VideoFormat {
    codec: u32,
    frame_rate: [u32; 2],
    progressive_sequence: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
    min_num_decode_surfaces: u8,
    coded_width: u32,
    coded_height: u32,
    display_area: Rect32,
    chroma_format: u32,
    bitrate: u32,
    display_aspect_ratio: [i32; 2],
    video_signal_description: [u8; 4],
    seqhdr_data_length: u32,
}

/// CUVIDSOURCEDATAPACKET.
#[repr(C)]
struct Packet {
    flags: c_ulong,
    payload_size: c_ulong,
    payload: *const u8,
    timestamp: i64,
}

const PKT_ENDOFSTREAM: c_ulong = 1;
const PKT_TIMESTAMP: c_ulong = 2;

/// CUVIDPARSERDISPINFO.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DispInfo {
    picture_index: i32,
    progressive_frame: i32,
    top_field_first: i32,
    repeat_first_field: i32,
    timestamp: i64,
}

type SequenceCb = unsafe extern "C" fn(*mut c_void, *mut VideoFormat) -> i32;
type DecodeCb = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;
type DisplayCb = unsafe extern "C" fn(*mut c_void, *mut DispInfo) -> i32;
type OtherCb = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;

/// CUVIDPARSERPARAMS.
#[repr(C)]
struct ParserParams {
    codec_type: u32,
    max_num_decode_surfaces: u32,
    clock_rate: u32,
    error_threshold: u32,
    max_display_delay: u32,
    /// bAnnexb:1, bMemoryOptimize:1, reserved:30.
    bits: u32,
    reserved1: [u32; 4],
    user_data: *mut c_void,
    sequence: Option<SequenceCb>,
    decode: Option<DecodeCb>,
    display: Option<DisplayCb>,
    operating_point: Option<OtherCb>,
    sei_message: Option<OtherCb>,
    reserved2: [*mut c_void; 5],
    ext_video_info: *mut c_void,
}

/// CUVIDDECODECREATEINFO.
#[repr(C)]
struct CreateInfo {
    width: c_ulong,
    height: c_ulong,
    num_decode_surfaces: c_ulong,
    codec_type: u32,
    chroma_format: u32,
    creation_flags: c_ulong,
    bit_depth_minus8: c_ulong,
    intra_decode_only: c_ulong,
    max_width: c_ulong,
    max_height: c_ulong,
    reserved1: c_ulong,
    display_area: Rect16,
    output_format: u32,
    deinterlace_mode: u32,
    target_width: c_ulong,
    target_height: c_ulong,
    num_output_surfaces: c_ulong,
    vid_lock: *mut c_void,
    target_rect: Rect16,
    enable_histogram: c_ulong,
    reserved2: [c_ulong; 4],
}

/// CUVIDPROCPARAMS.
#[repr(C)]
struct ProcParams {
    progressive_frame: i32,
    second_field: i32,
    top_field_first: i32,
    unpaired_field: i32,
    reserved_flags: u32,
    reserved_zero: u32,
    raw_input_dptr: u64,
    raw_input_pitch: u32,
    raw_input_format: u32,
    raw_output_dptr: u64,
    raw_output_pitch: u32,
    reserved1: u32,
    output_stream: *mut c_void,
    reserved: [u32; 46],
    histogram_dptr: *mut u64,
    reserved2: [*mut c_void; 1],
}

const CODEC_H264: u32 = 4;
const CHROMA_420: u32 = 1;
const SURFACE_NV12: u32 = 0;
const DEINTERLACE_WEAVE: u32 = 0;
const DEINTERLACE_ADAPTIVE: u32 = 2;
const CREATE_PREFER_CUVID: c_ulong = 4;

fn zeroed<T>() -> T {
    // SAFETY: only used for the C structs above, for which all-zero is the
    // documented initial state.
    unsafe { std::mem::zeroed() }
}

/// The libnvcuvid calls used here.
struct Cuvid {
    _lib: Library,
    create_parser: unsafe extern "C" fn(*mut Parser, *mut ParserParams) -> sys::CUresult,
    parse: unsafe extern "C" fn(Parser, *mut Packet) -> sys::CUresult,
    destroy_parser: unsafe extern "C" fn(Parser) -> sys::CUresult,
    create_decoder: unsafe extern "C" fn(*mut Decoder, *mut CreateInfo) -> sys::CUresult,
    destroy_decoder: unsafe extern "C" fn(Decoder) -> sys::CUresult,
    decode: unsafe extern "C" fn(Decoder, *mut c_void) -> sys::CUresult,
    map: unsafe extern "C" fn(Decoder, i32, *mut u64, *mut u32, *mut ProcParams) -> sys::CUresult,
    unmap: unsafe extern "C" fn(Decoder, u64) -> sys::CUresult,
}

// SAFETY: function pointers into the driver library.
unsafe impl Send for Cuvid {}
unsafe impl Sync for Cuvid {}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the symbol's C signature matches the field it fills.
        *unsafe { $lib.get($name) }.map_err(|e| format!("libnvcuvid: {e}"))?
    };
}

fn cuvid() -> Result<&'static Cuvid, String> {
    static API: std::sync::OnceLock<Result<Cuvid, String>> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: loading the driver library runs its initializers only.
        let lib = unsafe { Library::new("libnvcuvid.so.1") }.map_err(|e| format!("libnvcuvid: {e}"))?;
        Ok(Cuvid {
            create_parser: sym!(lib, b"cuvidCreateVideoParser\0"),
            parse: sym!(lib, b"cuvidParseVideoData\0"),
            destroy_parser: sym!(lib, b"cuvidDestroyVideoParser\0"),
            create_decoder: sym!(lib, b"cuvidCreateDecoder\0"),
            destroy_decoder: sym!(lib, b"cuvidDestroyDecoder\0"),
            decode: sym!(lib, b"cuvidDecodePicture\0"),
            map: sym!(lib, b"cuvidMapVideoFrame64\0"),
            unmap: sym!(lib, b"cuvidUnmapVideoFrame64\0"),
            _lib: lib,
        })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// What the parser's callbacks share with the reader.
struct State {
    api: &'static Cuvid,
    decoder: Decoder,
    surfaces: u32,
    /// Display size of the decoder's output.
    size: (u32, u32),
    /// Pictures ready for display, in display order.
    shown: VecDeque<DispInfo>,
    /// The parser has flushed everything after end of stream.
    flushed: bool,
    error: Option<String>,
}

unsafe extern "C" fn on_sequence(user: *mut c_void, format: *mut VideoFormat) -> i32 {
    // SAFETY: `user` is the reader's boxed State, `format` the parser's.
    let (st, f) = unsafe { (&mut *(user as *mut State), &*format) };
    if !st.decoder.is_null() {
        // The same stream again after a seek.
        return st.surfaces as i32;
    }
    if f.chroma_format != CHROMA_420 || f.bit_depth_luma_minus8 != 0 {
        st.error = Some("NVDEC here takes 8-bit 4:2:0 only".into());
        return 0;
    }
    let a = f.display_area;
    let (w, h) = ((a.right - a.left) as u32, (a.bottom - a.top) as u32);
    let surfaces = (f.min_num_decode_surfaces as u32).max(1) + 4;
    let mut info: CreateInfo = zeroed();
    info.width = f.coded_width as c_ulong;
    info.height = f.coded_height as c_ulong;
    info.max_width = f.coded_width as c_ulong;
    info.max_height = f.coded_height as c_ulong;
    info.num_decode_surfaces = surfaces as c_ulong;
    info.codec_type = f.codec;
    info.chroma_format = CHROMA_420;
    info.creation_flags = CREATE_PREFER_CUVID;
    info.display_area = Rect16 {
        left: a.left as i16,
        top: a.top as i16,
        right: a.right as i16,
        bottom: a.bottom as i16,
    };
    info.output_format = SURFACE_NV12;
    info.deinterlace_mode = if f.progressive_sequence != 0 {
        DEINTERLACE_WEAVE
    } else {
        DEINTERLACE_ADAPTIVE
    };
    info.target_width = w as c_ulong;
    info.target_height = h as c_ulong;
    info.num_output_surfaces = 2;
    let mut dec = std::ptr::null_mut();
    // SAFETY: a filled create info and an out-pointer; the caller made the
    // context current.
    let r = unsafe { (st.api.create_decoder)(&mut dec, &mut info) };
    if r != 0 {
        st.error = Some(format!("cuvidCreateDecoder: CUDA error {r}"));
        return 0;
    }
    st.decoder = dec;
    st.surfaces = surfaces;
    st.size = (w, h);
    surfaces as i32
}

unsafe extern "C" fn on_decode(user: *mut c_void, pic: *mut c_void) -> i32 {
    // SAFETY: as above; `pic` is the parser's CUVIDPICPARAMS, passed through.
    let st = unsafe { &mut *(user as *mut State) };
    if st.decoder.is_null() {
        return 0;
    }
    let r = unsafe { (st.api.decode)(st.decoder, pic) };
    if r != 0 {
        st.error = Some(format!("cuvidDecodePicture: CUDA error {r}"));
        return 0;
    }
    1
}

unsafe extern "C" fn on_display(user: *mut c_void, info: *mut DispInfo) -> i32 {
    // SAFETY: as above; a null `info` means the end of the stream.
    let st = unsafe { &mut *(user as *mut State) };
    if info.is_null() {
        st.flushed = true;
    } else {
        st.shown.push_back(unsafe { *info });
    }
    1
}

/// A buffer the decoder's pictures are copied into, as CUDA sees it.
struct Slot {
    buffer: Buffer,
    memory: sys::CUexternalMemory,
    ptr: sys::CUdeviceptr,
}

/// Frame buffers made at most (the renderer holds frames until the GPU
/// work reading them is done; the source's window holds a few more).
const MAX_SLOTS: usize = 32;

/// Sequential decoded frames of an MP4's H.264 track, from a seek point
/// on, as NV12 in shared GPU buffers.
pub struct Reader {
    api: &'static Cuvid,
    cuda: &'static Cuda,
    ctx: sys::CUcontext,
    device: Device,
    mp4: mp4::Mp4Reader<std::io::BufReader<std::fs::File>>,
    track: u32,
    timescale: f64,
    /// Per sample, in decode order: presentation time in track ticks
    /// (from the first displayed frame) and whether decoding may start there.
    samples: Vec<(i64, bool)>,
    nal_length: usize,
    /// SPS and PPS as Annex B, sent before the first sample after a seek.
    headers: Vec<u8>,
    parser: Parser,
    /// This reader's CUDA stream: picture copies wait on it alone, not on
    /// the context's other work (the encoder's).
    stream: *mut c_void,
    /// Boxed for the parser's callbacks, which reach it through a raw
    /// pointer while a parse call runs (freed on drop).
    state: *mut State,
    next: usize,
    headers_sent: bool,
    eos_sent: bool,
    /// Displayed frames before this (ticks) only led up to a seek target.
    skip_before: i64,
    slots: Vec<Slot>,
    annexb: Vec<u8>,
    pub info: VideoInfo,
    pub format: FrameFormat,
}

// SAFETY: the reader lives on its source's decode thread; CUDA calls make
// the context current around themselves.
unsafe impl Send for Reader {}

impl Reader {
    /// A hardware decoder for `path`, or why it cannot take it (the caller
    /// decodes with ffmpeg then).
    pub fn open(path: &Path, device: &Device, stream: &crate::ffmpeg::Stream) -> Result<Self, String> {
        if std::env::var("ULTRAFAST_NVDEC").is_ok_and(|v| v == "0") {
            return Err("ULTRAFAST_NVDEC=0".into());
        }
        if !matches!(stream.pix_fmt.as_str(), "yuv420p" | "yuvj420p") || stream.hdr() {
            return Err(format!("{} {}", stream.pix_fmt, stream.transfer));
        }
        let (w, h) = stream.coded;
        if w % 2 == 1 || h % 2 == 1 {
            return Err("odd frame size".into());
        }
        let api = cuvid()?;
        let (cuda, ctx) = nvenc::cuda_context(device)?;
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let mp4 = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).map_err(|e| e.to_string())?;
        let (track, avc, stbl, timescale) = mp4
            .tracks()
            .values()
            .find_map(|t| {
                let stbl = &t.trak.mdia.minf.stbl;
                stbl.stsd
                    .avc1
                    .as_ref()
                    .map(|a| (t.track_id(), a.avcc.clone(), stbl.clone(), t.timescale()))
            })
            .ok_or("no H.264 track")?;
        if !mp4.tracks()[&track].trafs.is_empty() {
            return Err("fragmented MP4".into());
        }
        // Decode-order times: stts deltas, ctts offsets; stss sync samples
        // (all samples are sync without one).
        let mut samples = Vec::new();
        let mut dts: i64 = 0;
        for e in &stbl.stts.entries {
            for _ in 0..e.sample_count {
                samples.push((dts, stbl.stss.is_none()));
                dts += e.sample_delta as i64;
            }
        }
        if let Some(ctts) = &stbl.ctts {
            let mut i = 0;
            for e in &ctts.entries {
                for _ in 0..e.sample_count {
                    if let Some(s) = samples.get_mut(i) {
                        s.0 += e.sample_offset as i64;
                    }
                    i += 1;
                }
            }
        }
        if let Some(stss) = &stbl.stss {
            for &n in &stss.entries {
                if let Some(s) = samples.get_mut(n as usize - 1) {
                    s.1 = true;
                }
            }
        }
        // The first displayed frame is time 0 (what an edit list shifting
        // out the reorder delay does).
        let first = samples.iter().map(|s| s.0).min().ok_or("no samples")?;
        for s in &mut samples {
            s.0 -= first;
        }
        let mut headers = Vec::new();
        for nal in avc.sequence_parameter_sets.iter().chain(&avc.picture_parameter_sets) {
            headers.extend([0, 0, 0, 1]);
            headers.extend(&nal.bytes);
        }
        let (kr, kb) = stream.weights();
        let mut r = Self {
            api,
            cuda,
            ctx,
            device: device.clone(),
            mp4,
            track,
            timescale: timescale as f64,
            samples,
            nal_length: avc.length_size_minus_one as usize % 4 + 1,
            headers,
            parser: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            state: Box::into_raw(Box::new(State {
                api,
                decoder: std::ptr::null_mut(),
                surfaces: 0,
                size: (0, 0),
                shown: VecDeque::new(),
                flushed: false,
                error: None,
            })),
            next: 0,
            headers_sent: false,
            eos_sent: false,
            skip_before: 0,
            slots: Vec::new(),
            annexb: Vec::new(),
            info: stream.info,
            format: FrameFormat {
                layout: Layout::Nv12,
                coded: (w, h),
                kr,
                kb,
                full_range: stream.full_range(),
            },
        };
        // CU_STREAM_NON_BLOCKING: no implicit waits on the legacy stream.
        r.stream = r.in_ctx(|| unsafe {
            let mut stream = std::ptr::null_mut();
            nvenc::cu((cuda.stream_create)(&mut stream, 1), "cuStreamCreate")?;
            Ok(stream)
        })?;
        r.seek(0.0).map_err(|e| e.to_string())?;
        Ok(r)
    }

    fn in_ctx<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        nvenc::in_context(self.cuda, self.ctx, f)
    }

    fn state(&mut self) -> &mut State {
        // SAFETY: owned by this reader; callbacks only run inside its parse
        // calls, never while this reference is alive.
        unsafe { &mut *self.state }
    }

    /// Restarts at the frame showing at `t` seconds: a new parser from the
    /// sync sample before it (the decoder stays).
    pub fn seek(&mut self, t: f64) -> Result<(), MediaError> {
        let api = self.api;
        let state = self.state;
        let old = std::mem::replace(&mut self.parser, std::ptr::null_mut());
        let parser = self
            .in_ctx(|| unsafe {
                if !old.is_null() {
                    (api.destroy_parser)(old);
                }
                let mut p: ParserParams = zeroed();
                p.codec_type = CODEC_H264;
                p.max_num_decode_surfaces = 20;
                p.clock_rate = self.timescale as u32;
                // Pictures shown two decodes late: the decoder works on the
                // next ones while one is copied out.
                p.max_display_delay = 2;
                p.user_data = state.cast();
                p.sequence = Some(on_sequence);
                p.decode = Some(on_decode);
                p.display = Some(on_display);
                let mut parser = std::ptr::null_mut();
                nvenc::cu((api.create_parser)(&mut parser, &mut p), "cuvidCreateVideoParser")?;
                Ok(parser)
            })
            .map_err(MediaError::Platform)?;
        self.parser = parser;
        self.state().shown.clear();
        self.state().flushed = false;
        self.headers_sent = false;
        self.eos_sent = false;
        // The frame showing at t: the latest presentation time at or before
        // it; decoding starts at the last sync sample before that sample.
        let target = (t * self.timescale).round() as i64;
        let shown = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.0 <= target)
            .max_by_key(|(_, s)| s.0)
            .map(|(i, s)| (i, s.0));
        let (index, pts) = shown.unwrap_or((0, 0));
        self.skip_before = pts;
        self.next = (0..=index).rev().find(|&i| self.samples[i].1).unwrap_or(0);
        Ok(())
    }

    /// The next frame in display order: its presentation time and a buffer
    /// holding it (NV12, rows `format.coded.0` bytes apart), or None at the
    /// end. A buffer is used again once every frame holding it is dropped.
    pub fn next(&mut self) -> Result<Option<(f64, Buffer)>, MediaError> {
        loop {
            while let Some(info) = self.state().shown.pop_front() {
                if info.timestamp < self.skip_before {
                    continue;
                }
                let buffer = self.copy_out(&info).map_err(MediaError::Platform)?;
                return Ok(Some((info.timestamp as f64 / self.timescale, buffer)));
            }
            if let Some(e) = self.state().error.take() {
                return Err(MediaError::Platform(e));
            }
            if self.state().flushed {
                return Ok(None);
            }
            let mut packet = Packet {
                flags: PKT_ENDOFSTREAM,
                payload_size: 0,
                payload: std::ptr::null(),
                timestamp: 0,
            };
            if self.next < self.samples.len() {
                let sample = self
                    .mp4
                    .read_sample(self.track, self.next as u32 + 1)
                    .map_err(|e| MediaError::Platform(e.to_string()))?
                    .ok_or_else(|| MediaError::Platform("missing sample".into()))?;
                // A first NAL length of zero is no H.264 at all: bytes a
                // partial fetch of the file did not bring read as zeros.
                // Fail loudly rather than decode garbage.
                let head = &sample.bytes[..self.nal_length.min(sample.bytes.len())];
                if head.len() < self.nal_length || head.iter().all(|b| *b == 0) {
                    return Err(MediaError::Platform(format!(
                        "sample {} of the H.264 track holds no data (outside the fetched part of the file?)",
                        self.next + 1
                    )));
                }
                self.annexb.clear();
                if !self.headers_sent {
                    self.annexb.extend(&self.headers);
                    self.headers_sent = true;
                }
                // Length-prefixed NAL units become start-code separated.
                let (data, n) = (&sample.bytes[..], self.nal_length);
                let mut at = 0;
                while at + n <= data.len() {
                    let len = data[at..at + n].iter().fold(0usize, |l, b| (l << 8) | *b as usize);
                    at += n;
                    let end = (at + len).min(data.len());
                    self.annexb.extend([0, 0, 0, 1]);
                    self.annexb.extend(&data[at..end]);
                    at = end;
                }
                packet = Packet {
                    flags: PKT_TIMESTAMP,
                    payload_size: self.annexb.len() as c_ulong,
                    payload: self.annexb.as_ptr(),
                    timestamp: self.samples[self.next].0,
                };
                self.next += 1;
            } else if self.eos_sent {
                return Ok(None);
            } else {
                self.eos_sent = true;
            }
            let (api, parser) = (self.api, self.parser);
            self.in_ctx(|| unsafe { nvenc::cu((api.parse)(parser, &mut packet), "cuvidParseVideoData") })
                .map_err(MediaError::Platform)?;
        }
    }

    /// Copies a displayed picture into a free buffer (device to device).
    fn copy_out(&mut self, info: &DispInfo) -> Result<Buffer, String> {
        let (w, h) = self.format.coded;
        let size = self.state().size;
        if size != (w, h) {
            return Err(format!("decoder output {size:?} is not the stream's {:?}", (w, h)));
        }
        // A buffer no frame holds any more (the renderer lets frames go
        // once the GPU work reading them is done).
        let slot = match self.slots.iter().position(|s| s.buffer.is_unique()) {
            Some(i) => i,
            None if self.slots.len() < MAX_SLOTS => {
                let len = (w as usize * h as usize * 3 / 2).div_ceil(4) * 4;
                let (cuda, device) = (self.cuda, self.device.clone());
                let (buffer, memory, ptr) = self.in_ctx(|| unsafe { nvenc::import_buffer(cuda, &device, len) })?;
                self.slots.push(Slot { buffer, memory, ptr });
                self.slots.len() - 1
            }
            None => return Err("every NVDEC frame buffer is in use".into()),
        };
        let (api, cuda, decoder, stream) = (self.api, self.cuda, self.state().decoder, self.stream);
        let dst = self.slots[slot].ptr;
        self.in_ctx(|| unsafe {
            let mut proc: ProcParams = zeroed();
            proc.progressive_frame = info.progressive_frame;
            proc.top_field_first = info.top_field_first;
            proc.output_stream = stream;
            let (mut src, mut pitch) = (0u64, 0u32);
            nvenc::cu(
                (api.map)(decoder, info.picture_index, &mut src, &mut pitch, &mut proc),
                "cuvidMapVideoFrame",
            )?;
            let copy = |from: u64, to: u64, rows: usize| {
                let m = Memcpy2d {
                    src_x_in_bytes: 0,
                    src_y: 0,
                    src_memory_type: MEMORY_DEVICE,
                    src_host: std::ptr::null(),
                    src_device: from,
                    src_array: std::ptr::null_mut(),
                    src_pitch: pitch as usize,
                    dst_x_in_bytes: 0,
                    dst_y: 0,
                    dst_memory_type: MEMORY_DEVICE,
                    dst_host: std::ptr::null_mut(),
                    dst_device: to,
                    dst_array: std::ptr::null_mut(),
                    dst_pitch: w as usize,
                    width_in_bytes: w as usize,
                    height: rows,
                };
                nvenc::cu((cuda.memcpy_2d_async)(&m, stream), "cuMemcpy2DAsync")
            };
            // Luma, then interleaved chroma (below the luma rows, rounded
            // up to even, in the decoder's surface).
            let r = copy(src, dst, h as usize).and_then(|_| {
                copy(
                    src + pitch as u64 * ((h as u64 + 1) & !1),
                    dst + w as u64 * h as u64,
                    h as usize / 2,
                )
            });
            // The copies finish before the picture goes back to the decoder
            // and before the renderer reads the buffer.
            let synced = nvenc::cu((cuda.stream_synchronize)(stream), "cuStreamSynchronize");
            (api.unmap)(decoder, src);
            r.and(synced)
        })?;
        Ok(self.slots[slot].buffer.clone())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let (api, cuda) = (self.api, self.cuda);
        let (parser, stream) = (self.parser, self.stream);
        let decoder = self.state().decoder;
        let slots = std::mem::take(&mut self.slots);
        let _ = self.in_ctx(|| unsafe {
            if !parser.is_null() {
                (api.destroy_parser)(parser);
            }
            if !stream.is_null() {
                (cuda.stream_destroy)(stream);
            }
            if !decoder.is_null() {
                (api.destroy_decoder)(decoder);
            }
            for s in &slots {
                nvenc::release_buffer(cuda, s.memory, s.ptr);
            }
            Ok(())
        });
        // SAFETY: from Box::into_raw in `open`; no parser calls back now.
        drop(unsafe { Box::from_raw(self.state) });
    }
}

//! NVENC export on NVIDIA GPUs without a trip through the CPU.
//!
//! The renderer's last kernel writes each frame as NV12 into a Vulkan
//! buffer whose memory is exported (an opaque fd on Linux, an NT handle on
//! Windows) and imported into CUDA; the
//! hardware encoder registers that CUDA pointer as its input. A frame is
//! never read back: the only bytes that reach the CPU are the compressed
//! bitstream. Several buffers are kept so the GPU renders the next frames
//! while the encoder works on an earlier one.
//!
//! libcuda and libnvidia-encode (nvcuda.dll and nvEncodeAPI64.dll on
//! Windows) come from the driver and are loaded at runtime, so a build runs
//! (and falls back) on machines without them.

use crate::nvenc_sys as sys;
use crate::{Codec, EncodeRate};

/// Frames the encoder may hold before the oldest is read back.
pub const DEPTH: usize = 4;
use libloading::Library;
use std::ffi::c_void;
use std::ptr::null_mut;
use ultrafast_gpu::{Buffer, Device};

#[cfg(not(windows))]
const CUDA_LIB: &str = "libcuda.so.1";
#[cfg(windows)]
const CUDA_LIB: &str = "nvcuda.dll";
#[cfg(not(windows))]
const NVENC_LIB: &str = "libnvidia-encode.so.1";
#[cfg(windows)]
const NVENC_LIB: &str = "nvEncodeAPI64.dll";

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> sys::GUID {
    sys::GUID {
        Data1: d1,
        Data2: d2,
        Data3: d3,
        Data4: d4,
    }
}

// From nvEncodeAPI.h (static consts there, so bindgen cannot see them).
const CODEC_H264: sys::GUID = guid(
    0x6bc82762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
const CODEC_HEVC: sys::GUID = guid(
    0x790cdc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
const H264_HIGH: sys::GUID = guid(
    0xe7cbc309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
const HEVC_MAIN: sys::GUID = guid(
    0xb514c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
/// P4: the middle of NVENC's speed/quality range (P1 fastest, P7 best).
const PRESET_P4: sys::GUID = guid(
    0x90a7b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);
const PRESETS: [sys::GUID; 7] = [
    guid(
        0xfc0a8d3e,
        0x45f8,
        0x4cf8,
        [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
    ),
    guid(
        0xf581cfb8,
        0x88d6,
        0x4381,
        [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
    ),
    guid(
        0x36850110,
        0x3a07,
        0x441f,
        [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6],
    ),
    PRESET_P4,
    guid(
        0x21c6e6b4,
        0x297a,
        0x4cba,
        [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
    ),
    guid(
        0x8e75c279,
        0x6299,
        0x4ab6,
        [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5],
    ),
    guid(
        0x84848c12,
        0x6f71,
        0x4c13,
        [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74],
    ),
];

/// Encoder speed knobs, over what the export asks: ULTRAFAST_NVENC_PRESET
/// 1..7 (the standard quality level's 2: on an L4 it encodes 1080x1920 30%
/// faster than P4 for 0.2 dB less PSNR at the same bitrate),
/// ULTRAFAST_NVENC_TUNING hq | ll | ull (default hq),
/// ULTRAFAST_NVENC_MULTIPASS 0 | 1 (quarter) | 2 (full) (default: the
/// export's, else the preset's).
fn knobs(rate: &EncodeRate) -> (sys::GUID, sys::NV_ENC_TUNING_INFO, Option<sys::NV_ENC_MULTI_PASS>) {
    let preset = std::env::var("ULTRAFAST_NVENC_PRESET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|p| (1..=7).contains(p))
        .unwrap_or(rate.preset.clamp(1, 7) as usize);
    let preset = PRESETS[preset - 1];
    let tuning = match std::env::var("ULTRAFAST_NVENC_TUNING").as_deref() {
        Ok("ll") => sys::NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_LOW_LATENCY,
        Ok("ull") => sys::NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
        _ => sys::NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_HIGH_QUALITY,
    };
    let multipass = std::env::var("ULTRAFAST_NVENC_MULTIPASS")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(rate
            .multipass
            .then_some(sys::_NV_ENC_MULTI_PASS_NV_ENC_TWO_PASS_FULL_RESOLUTION));
    (preset, tuning, multipass)
}

const fn struct_version(v: u32) -> u32 {
    sys::NVENCAPI_VERSION | (v << 16) | (0x7 << 28)
}
const OPEN_SESSION_VER: u32 = struct_version(1);
const PRESET_CONFIG_VER: u32 = struct_version(5) | (1 << 31);
const CONFIG_VER: u32 = struct_version(9) | (1 << 31);
const INITIALIZE_VER: u32 = struct_version(7) | (1 << 31);
const REGISTER_VER: u32 = struct_version(5);
const MAP_VER: u32 = struct_version(4);
const BITSTREAM_VER: u32 = struct_version(1);
const PIC_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
const LOCK_VER: u32 = struct_version(2) | (1 << 31);
const FUNCTION_LIST_VER: u32 = struct_version(2);
/// CUDA_EXTERNAL_MEMORY_DEDICATED (cuda.h).
const CUDA_DEDICATED: u32 = 1;

fn zeroed<T>() -> T {
    // SAFETY: only used for the C structs below, for which all-zero is the
    // documented initial state.
    unsafe { std::mem::zeroed() }
}

/// CUDA_MEMCPY2D (cuda.h).
#[repr(C)]
pub(crate) struct Memcpy2d {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: u32,
    pub src_host: *const c_void,
    pub src_device: sys::CUdeviceptr,
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: u32,
    pub dst_host: *mut c_void,
    pub dst_device: sys::CUdeviceptr,
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

/// CU_MEMORYTYPE_DEVICE.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) const MEMORY_DEVICE: u32 = 2;

/// The CUDA driver calls the interop needs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) struct Cuda {
    _lib: Library,
    init: unsafe extern "C" fn(u32) -> sys::CUresult,
    device_get_count: unsafe extern "C" fn(*mut i32) -> sys::CUresult,
    device_get: unsafe extern "C" fn(*mut sys::CUdevice, i32) -> sys::CUresult,
    device_get_uuid: unsafe extern "C" fn(*mut sys::CUuuid, sys::CUdevice) -> sys::CUresult,
    ctx_create: unsafe extern "C" fn(*mut sys::CUcontext, u32, sys::CUdevice) -> sys::CUresult,
    ctx_push: unsafe extern "C" fn(sys::CUcontext) -> sys::CUresult,
    ctx_pop: unsafe extern "C" fn(*mut sys::CUcontext) -> sys::CUresult,
    import_memory:
        unsafe extern "C" fn(*mut sys::CUexternalMemory, *const sys::CUDA_EXTERNAL_MEMORY_HANDLE_DESC) -> sys::CUresult,
    mapped_buffer: unsafe extern "C" fn(
        *mut sys::CUdeviceptr,
        sys::CUexternalMemory,
        *const sys::CUDA_EXTERNAL_MEMORY_BUFFER_DESC,
    ) -> sys::CUresult,
    destroy_memory: unsafe extern "C" fn(sys::CUexternalMemory) -> sys::CUresult,
    mem_free: unsafe extern "C" fn(sys::CUdeviceptr) -> sys::CUresult,
    pub(crate) memcpy_2d_async: unsafe extern "C" fn(*const Memcpy2d, *mut c_void) -> sys::CUresult,
    pub(crate) stream_create: unsafe extern "C" fn(*mut *mut c_void, u32) -> sys::CUresult,
    pub(crate) stream_synchronize: unsafe extern "C" fn(*mut c_void) -> sys::CUresult,
    pub(crate) stream_destroy: unsafe extern "C" fn(*mut c_void) -> sys::CUresult,
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the symbol's C signature matches the field it fills.
        *unsafe { $lib.get($name) }.map_err(|e| format!("libcuda: {e}"))?
    };
}

/// The CUDA and NVENC drivers, loaded once for the process and never
/// unloaded (unloading libcuda while its threads live crashes at exit), with
/// one CUDA context per GPU, made on first use and kept: an encoder for a
/// warm service's next request costs a session, not a context.
struct Driver {
    cuda: Cuda,
    create_instance: unsafe extern "C" fn(*mut sys::NV_ENCODE_API_FUNCTION_LIST) -> sys::NVENCSTATUS,
    _nvenc: Library,
    contexts: std::sync::Mutex<Vec<([u8; 16], usize)>>,
    /// Contexts encoders own one each (ULTRAFAST_NVENC_OWN_CTX=1), free to take.
    spare: std::sync::Mutex<Vec<([u8; 16], usize)>>,
}

// SAFETY: function pointers and driver handles; contexts are only made
// current around calls (push/pop), which CUDA allows from any thread.
unsafe impl Send for Driver {}
unsafe impl Sync for Driver {}

fn driver() -> Result<&'static Driver, String> {
    static DRIVER: std::sync::OnceLock<Result<Driver, String>> = std::sync::OnceLock::new();
    DRIVER
        .get_or_init(|| {
            let cuda = Cuda::load()?;
            // SAFETY: plain driver calls with valid out-pointers.
            unsafe {
                cu((cuda.init)(0), "cuInit")?;
                let nvenc = Library::new(NVENC_LIB).map_err(|e| format!("libnvidia-encode: {e}"))?;
                let create_instance = *nvenc
                    .get(b"NvEncodeAPICreateInstance\0")
                    .map_err(|e| format!("libnvidia-encode: {e}"))?;
                Ok(Driver {
                    cuda,
                    create_instance,
                    _nvenc: nvenc,
                    contexts: Default::default(),
                    spare: Default::default(),
                })
            }
        })
        .as_ref()
        .map_err(|e| e.clone())
}

impl Driver {
    /// The context for the CUDA device that is the Vulkan device `uuid`.
    fn context(&self, uuid: [u8; 16]) -> Result<sys::CUcontext, String> {
        let mut contexts = self.contexts.lock().unwrap();
        if let Some((_, c)) = contexts.iter().find(|(u, _)| *u == uuid) {
            return Ok(*c as sys::CUcontext);
        }
        let ctx = self.new_context(uuid)?;
        contexts.push((uuid, ctx as usize));
        Ok(ctx)
    }

    /// A context of its own for one encoder (kept for the next when it is
    /// done): sessions sharing a context may wait on each other's work.
    fn own_context(&self, uuid: [u8; 16]) -> Result<sys::CUcontext, String> {
        let mut spare = self.spare.lock().unwrap();
        if let Some(i) = spare.iter().position(|(u, _)| *u == uuid) {
            return Ok(spare.swap_remove(i).1 as sys::CUcontext);
        }
        drop(spare);
        self.new_context(uuid)
    }

    fn give_back(&self, uuid: [u8; 16], ctx: sys::CUcontext) {
        self.spare.lock().unwrap().push((uuid, ctx as usize));
    }

    fn new_context(&self, uuid: [u8; 16]) -> Result<sys::CUcontext, String> {
        let cuda = &self.cuda;
        // SAFETY: plain driver calls with valid out-pointers.
        unsafe {
            let mut count = 0;
            cu((cuda.device_get_count)(&mut count), "cuDeviceGetCount")?;
            let mut chosen = None;
            for i in 0..count {
                let mut d = 0;
                cu((cuda.device_get)(&mut d, i), "cuDeviceGet")?;
                let mut id: sys::CUuuid = zeroed();
                cu((cuda.device_get_uuid)(&mut id, d), "cuDeviceGetUuid")?;
                let bytes: [u8; 16] = std::mem::transmute(id.bytes);
                if bytes == uuid {
                    chosen = Some(d);
                }
            }
            let dev = chosen.ok_or("no CUDA device matches the Vulkan device")?;
            let mut ctx = null_mut();
            cu((cuda.ctx_create)(&mut ctx, 0, dev), "cuCtxCreate")?;
            let mut pop = null_mut();
            cu((cuda.ctx_pop)(&mut pop), "cuCtxPopCurrent")?;
            Ok(ctx)
        }
    }
}

impl Cuda {
    fn load() -> Result<Self, String> {
        // SAFETY: loading the driver library runs its initializers only.
        let lib = unsafe { Library::new(CUDA_LIB) }.map_err(|e| format!("libcuda: {e}"))?;
        Ok(Self {
            init: sym!(lib, b"cuInit\0"),
            device_get_count: sym!(lib, b"cuDeviceGetCount\0"),
            device_get: sym!(lib, b"cuDeviceGet\0"),
            device_get_uuid: sym!(lib, b"cuDeviceGetUuid\0"),
            ctx_create: sym!(lib, b"cuCtxCreate_v2\0"),
            ctx_push: sym!(lib, b"cuCtxPushCurrent_v2\0"),
            ctx_pop: sym!(lib, b"cuCtxPopCurrent_v2\0"),
            import_memory: sym!(lib, b"cuImportExternalMemory\0"),
            mapped_buffer: sym!(lib, b"cuExternalMemoryGetMappedBuffer\0"),
            destroy_memory: sym!(lib, b"cuDestroyExternalMemory\0"),
            mem_free: sym!(lib, b"cuMemFree_v2\0"),
            memcpy_2d_async: sym!(lib, b"cuMemcpy2DAsync_v2\0"),
            stream_create: sym!(lib, b"cuStreamCreate\0"),
            stream_synchronize: sym!(lib, b"cuStreamSynchronize\0"),
            stream_destroy: sym!(lib, b"cuStreamDestroy_v2\0"),
            _lib: lib,
        })
    }
}

pub(crate) fn cu(r: sys::CUresult, what: &str) -> Result<(), String> {
    if r == 0 {
        Ok(())
    } else {
        Err(format!("{what}: CUDA error {r}"))
    }
}

/// The process's CUDA driver and the context of `device`'s GPU (shared with
/// NVDEC).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) fn cuda_context(device: &Device) -> Result<(&'static Cuda, sys::CUcontext), String> {
    let driver = driver()?;
    Ok((&driver.cuda, driver.context(device.uuid())?))
}

/// Runs `f` with `ctx` current on this thread.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) fn in_context<T>(
    cuda: &Cuda,
    ctx: sys::CUcontext,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // SAFETY: contexts live for the rest of the process.
    unsafe { cu((cuda.ctx_push)(ctx), "cuCtxPushCurrent")? };
    let r = f();
    let mut pop = null_mut();
    unsafe { (cuda.ctx_pop)(&mut pop) };
    r
}

/// A Vulkan buffer of `len` bytes that CUDA also sees: (buffer, imported
/// memory, its device pointer). Call with the context current.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) unsafe fn import_buffer(
    cuda: &Cuda,
    device: &Device,
    len: usize,
) -> Result<(Buffer, sys::CUexternalMemory, sys::CUdeviceptr), String> {
    let (buffer, handle, size) = device.create_exported_buffer(len).map_err(|e| e.to_string())?;
    let (memory, ptr) = unsafe { import_memory(cuda, handle, size, len)? };
    Ok((buffer, memory, ptr))
}

/// Imports memory Vulkan exported and maps its first `len` bytes (context
/// current): (memory, device pointer).
unsafe fn import_memory(
    cuda: &Cuda,
    handle: ultrafast_gpu::ExportedMemory,
    size: u64,
    len: usize,
) -> Result<(sys::CUexternalMemory, sys::CUdeviceptr), String> {
    unsafe {
        let mut desc: sys::CUDA_EXTERNAL_MEMORY_HANDLE_DESC = zeroed();
        #[cfg(not(windows))]
        {
            desc.type_ = sys::CUexternalMemoryHandleType_enum_CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD;
            desc.handle.fd = handle;
        }
        #[cfg(windows)]
        {
            desc.type_ = sys::CUexternalMemoryHandleType_enum_CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32;
            desc.handle.win32.handle = handle as *mut c_void;
        }
        desc.size = size;
        desc.flags = CUDA_DEDICATED;
        let mut memory = null_mut();
        let imported = cu((cuda.import_memory)(&mut memory, &desc), "cuImportExternalMemory");
        // CUDA keeps its own reference to Windows memory: the handle stays ours.
        #[cfg(windows)]
        CloseHandle(handle);
        imported?;
        let mut bd: sys::CUDA_EXTERNAL_MEMORY_BUFFER_DESC = zeroed();
        bd.size = len as u64;
        let mut ptr = 0;
        if let Err(e) = cu(
            (cuda.mapped_buffer)(&mut ptr, memory, &bd),
            "cuExternalMemoryGetMappedBuffer",
        ) {
            (cuda.destroy_memory)(memory);
            return Err(e);
        }
        Ok((memory, ptr))
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: isize) -> i32;
}

/// Lets go of a buffer from [`import_buffer`] (context current).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // shared with NVDEC (Linux)
pub(crate) unsafe fn release_buffer(cuda: &Cuda, memory: sys::CUexternalMemory, ptr: sys::CUdeviceptr) {
    unsafe {
        (cuda.mem_free)(ptr);
        (cuda.destroy_memory)(memory);
    }
}

/// One frame's input: the Vulkan buffer kernels write, as CUDA and NVENC
/// see it.
struct Slot {
    buffer: Buffer,
    memory: sys::CUexternalMemory,
    ptr: sys::CUdeviceptr,
    registered: sys::NV_ENC_REGISTERED_PTR,
}

// SAFETY: the session and CUDA handles are used by one thread at a time
// (`&mut self`), with the context made current around every call.
unsafe impl Send for Encoder {}

/// A hardware encode session reading NV12 frames from shared GPU buffers.
pub struct Encoder {
    cuda: &'static Cuda,
    api: Box<sys::NV_ENCODE_API_FUNCTION_LIST>,
    session: *mut c_void,
    ctx: sys::CUcontext,
    /// The GPU whose context this encoder owns (given back on drop), if it
    /// owns one.
    owned: Option<[u8; 16]>,
    slots: Vec<Slot>,
    /// Output buffers: frames being encoded each write their own, read back
    /// only once DEPTH frames are queued, so the encoder works while the
    /// GPU renders the next frames.
    bitstreams: Vec<sys::NV_ENC_OUTPUT_PTR>,
    /// Frames submitted and not yet read back: (mapped input, bitstream).
    pending: std::collections::VecDeque<(sys::NV_ENC_INPUT_PTR, usize)>,
    next_bitstream: usize,
    width: u32,
    height: u32,
    pitch: u32,
    frames: u64,
}

impl Encoder {
    /// Opens an encoder for `width` x `height` at `fps` with `slots` input
    /// buffers on `device`'s GPU. Odd heights are rounded up to even (4:2:0).
    pub fn new(
        device: &Device,
        width: u32,
        height: u32,
        fps: f64,
        codec: Codec,
        rate: EncodeRate,
        slots: usize,
    ) -> Result<Self, String> {
        let driver = driver()?;
        let own = std::env::var("ULTRAFAST_NVENC_OWN_CTX").is_ok_and(|v| v == "1");
        let ctx = if own {
            driver.own_context(device.uuid())?
        } else {
            driver.context(device.uuid())?
        };
        // SAFETY: plain driver calls with valid out-pointers; the driver
        // and the context live for the rest of the process.
        unsafe {
            let create = driver.create_instance;
            let mut api: Box<sys::NV_ENCODE_API_FUNCTION_LIST> = Box::new(zeroed());
            api.version = FUNCTION_LIST_VER;
            nv(create(&mut *api), "NvEncodeAPICreateInstance")?;
            let mut enc = Self {
                cuda: &driver.cuda,
                api,
                session: null_mut(),
                ctx,
                owned: own.then(|| device.uuid()),
                slots: Vec::new(),
                bitstreams: Vec::new(),
                pending: Default::default(),
                next_bitstream: 0,
                width,
                height: height.next_multiple_of(2),
                pitch: width.next_multiple_of(256),
                frames: 0,
            };
            enc.open(codec, fps, rate)?;
            for _ in 0..slots {
                enc.add_slot(device)?;
            }
            Ok(enc)
        }
    }

    /// Bytes per row of the Y and CbCr planes in the input buffers.
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    /// Rows of the Y plane (the frame height rounded up to even).
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The input buffer of slot `i`: NV12, `pitch` bytes per row.
    pub fn buffer(&self, i: usize) -> &Buffer {
        &self.slots[i].buffer
    }

    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    fn with_ctx<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        // SAFETY: the context lives as long as the encoder.
        unsafe { cu((self.cuda.ctx_push)(self.ctx), "cuCtxPushCurrent")? };
        let r = f();
        let mut pop = null_mut();
        unsafe { (self.cuda.ctx_pop)(&mut pop) };
        r
    }

    unsafe fn open(&mut self, codec: Codec, fps: f64, rate: EncodeRate) -> Result<(), String> {
        let api = &*self.api;
        let mut open: sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = zeroed();
        open.version = OPEN_SESSION_VER;
        open.deviceType = sys::_NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_CUDA;
        open.device = self.ctx.cast();
        open.apiVersion = sys::NVENCAPI_VERSION;
        let mut session = null_mut();
        nv(
            unsafe { api.nvEncOpenEncodeSessionEx.unwrap()(&mut open, &mut session) },
            "nvEncOpenEncodeSessionEx",
        )?;
        self.session = session;
        let (codec_guid, profile) = match codec {
            Codec::H264 => (CODEC_H264, H264_HIGH),
            Codec::Hevc => (CODEC_HEVC, HEVC_MAIN),
        };
        let (preset_guid, tuning, multipass) = knobs(&rate);
        let mut preset: sys::NV_ENC_PRESET_CONFIG = zeroed();
        preset.version = PRESET_CONFIG_VER;
        preset.presetCfg.version = CONFIG_VER;
        self.call(
            unsafe { api.nvEncGetEncodePresetConfigEx.unwrap()(session, codec_guid, preset_guid, tuning, &mut preset) },
            "nvEncGetEncodePresetConfigEx",
        )?;
        let mut cfg = preset.presetCfg;
        cfg.version = CONFIG_VER;
        cfg.profileGUID = profile;
        // Two-second GOPs, no B-frames: frames come out in display order,
        // one per input, so a raw stream muxes with constant timing.
        let gop = (fps * 2.0).round().max(1.0) as u32;
        cfg.gopLength = gop;
        cfg.frameIntervalP = 1;
        cfg.rcParams.rateControlMode = sys::_NV_ENC_PARAMS_RC_MODE_NV_ENC_PARAMS_RC_VBR;
        let bits = |b: u64| b.min(u32::MAX as u64) as u32;
        match rate.quality {
            // Constant quality: VBR with no average target, capped.
            Some(q) => {
                let q = q.clamp(1.0, 51.0);
                cfg.rcParams.averageBitRate = 0;
                cfg.rcParams.maxBitRate = bits(rate.bitrate);
                cfg.rcParams.targetQuality = q.floor() as u8;
                cfg.rcParams.targetQualityLSB = ((q - q.floor()) * 256.0) as u8;
            }
            None => {
                cfg.rcParams.averageBitRate = bits(rate.bitrate);
                cfg.rcParams.maxBitRate = bits(rate.bitrate * 2);
            }
        }
        if let Some(m) = multipass {
            cfg.rcParams.multiPass = m;
        }
        // One bitstream out per frame in, as `encode` reads them: the slow
        // presets' lookahead would hold frames back.
        cfg.rcParams.set_enableLookahead(0);
        cfg.rcParams.lookaheadDepth = 0;
        let vui = sys::NV_ENC_CONFIG_H264_VUI_PARAMETERS {
            videoSignalTypePresentFlag: 1,
            videoFormat: sys::_NV_ENC_VUI_VIDEO_FORMAT_NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED,
            videoFullRangeFlag: 0,
            colourDescriptionPresentFlag: 1,
            colourPrimaries: sys::_NV_ENC_VUI_COLOR_PRIMARIES_NV_ENC_VUI_COLOR_PRIMARIES_BT709,
            transferCharacteristics: sys::_NV_ENC_VUI_TRANSFER_CHARACTERISTIC_NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709,
            colourMatrix: sys::_NV_ENC_VUI_MATRIX_COEFFS_NV_ENC_VUI_MATRIX_COEFFS_BT709,
            ..zeroed()
        };
        match codec {
            Codec::H264 => unsafe {
                let h = &mut cfg.encodeCodecConfig.h264Config;
                h.idrPeriod = gop;
                h.set_repeatSPSPPS(1);
                h.h264VUIParameters = vui;
            },
            Codec::Hevc => unsafe {
                let h = &mut cfg.encodeCodecConfig.hevcConfig;
                h.idrPeriod = gop;
                h.set_repeatSPSPPS(1);
                h.hevcVUIParameters = vui;
            },
        }
        let (num, den) = if (fps - fps.round()).abs() < 1e-3 {
            (fps.round() as u32, 1)
        } else {
            ((fps * 1001.0).round() as u32, 1001)
        };
        let mut init: sys::NV_ENC_INITIALIZE_PARAMS = zeroed();
        init.version = INITIALIZE_VER;
        init.encodeGUID = codec_guid;
        init.presetGUID = preset_guid;
        init.encodeWidth = self.width;
        init.encodeHeight = self.height;
        init.darWidth = self.width;
        init.darHeight = self.height;
        init.maxEncodeWidth = self.width;
        init.maxEncodeHeight = self.height;
        init.frameRateNum = num;
        init.frameRateDen = den;
        init.enablePTD = 1;
        init.encodeConfig = &mut cfg;
        init.tuningInfo = tuning;
        self.call(
            unsafe { api.nvEncInitializeEncoder.unwrap()(session, &mut init) },
            "nvEncInitializeEncoder",
        )?;
        for _ in 0..DEPTH {
            let mut bs: sys::NV_ENC_CREATE_BITSTREAM_BUFFER = zeroed();
            bs.version = BITSTREAM_VER;
            self.call(
                unsafe { api.nvEncCreateBitstreamBuffer.unwrap()(session, &mut bs) },
                "nvEncCreateBitstreamBuffer",
            )?;
            self.bitstreams.push(bs.bitstreamBuffer);
        }
        Ok(())
    }

    unsafe fn add_slot(&mut self, device: &Device) -> Result<(), String> {
        let len = self.pitch as usize * self.height as usize * 3 / 2;
        let (buffer, handle, size) = device.create_exported_buffer(len).map_err(|e| e.to_string())?;
        let cuda = &self.cuda;
        let (memory, ptr) = self.with_ctx(|| unsafe { import_memory(cuda, handle, size, len) })?;
        let mut reg: sys::NV_ENC_REGISTER_RESOURCE = zeroed();
        reg.version = REGISTER_VER;
        reg.resourceType = sys::_NV_ENC_INPUT_RESOURCE_TYPE_NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
        reg.width = self.width;
        reg.height = self.height;
        reg.pitch = self.pitch;
        reg.resourceToRegister = ptr as *mut c_void;
        reg.bufferFormat = sys::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_NV12;
        reg.bufferUsage = sys::_NV_ENC_BUFFER_USAGE_NV_ENC_INPUT_IMAGE;
        let api = &*self.api;
        let session = self.session;
        let r = self.with_ctx(|| {
            self.call(
                unsafe { api.nvEncRegisterResource.unwrap()(session, &mut reg) },
                "nvEncRegisterResource",
            )
        });
        let slot = Slot {
            buffer,
            memory,
            ptr,
            registered: reg.registeredResource,
        };
        if let Err(e) = r {
            self.release_slot(&slot);
            return Err(e);
        }
        self.slots.push(slot);
        Ok(())
    }

    fn release_slot(&self, s: &Slot) {
        // SAFETY: handles created by this encoder, released once.
        let _ = self.with_ctx(|| unsafe {
            if !s.registered.is_null() {
                self.api.nvEncUnregisterResource.unwrap()(self.session, s.registered);
            }
            (self.cuda.mem_free)(s.ptr);
            (self.cuda.destroy_memory)(s.memory);
            Ok(())
        });
    }

    /// Encodes the frame in slot `i` (the GPU must be done writing it) and
    /// passes its compressed bytes to `out`.
    /// Queues the frame in input buffer `i`. Encoded bytes come out through
    /// `out` in order, up to [`DEPTH`] frames later; buffer `i` stays in use
    /// until then (callers cycle through [`Encoder::new`]'s slots, which
    /// must number at least their frames in flight plus `DEPTH`).
    pub fn encode(&mut self, i: usize, out: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        let api = &*self.api;
        let session = self.session;
        let registered = self.slots[i].registered;
        let frame = self.frames;
        let (w, h, pitch) = (self.width, self.height, self.pitch);
        let b = self.next_bitstream;
        let bitstream = self.bitstreams[b];
        let mapped = self.with_ctx(|| unsafe {
            let mut map: sys::NV_ENC_MAP_INPUT_RESOURCE = zeroed();
            map.version = MAP_VER;
            map.registeredResource = registered;
            self.call(
                api.nvEncMapInputResource.unwrap()(session, &mut map),
                "nvEncMapInputResource",
            )?;
            let mut pic: sys::NV_ENC_PIC_PARAMS = zeroed();
            pic.version = PIC_PARAMS_VER;
            pic.inputWidth = w;
            pic.inputHeight = h;
            pic.inputPitch = pitch;
            pic.inputBuffer = map.mappedResource;
            pic.outputBitstream = bitstream;
            pic.bufferFmt = map.mappedBufferFmt;
            pic.pictureStruct = sys::_NV_ENC_PIC_STRUCT_NV_ENC_PIC_STRUCT_FRAME;
            pic.inputTimeStamp = frame;
            pic.frameIdx = frame as u32;
            let r = api.nvEncEncodePicture.unwrap()(session, &mut pic);
            if r != sys::_NVENCSTATUS_NV_ENC_SUCCESS {
                api.nvEncUnmapInputResource.unwrap()(session, map.mappedResource);
                // P frames only: every picture produces its own output.
                self.call(r, "nvEncEncodePicture")?;
            }
            Ok(map.mappedResource)
        })?;
        self.pending.push_back((mapped, b));
        self.next_bitstream = (b + 1) % self.bitstreams.len();
        self.frames += 1;
        while self.pending.len() >= self.bitstreams.len() {
            self.drain_one(out)?;
        }
        Ok(())
    }

    /// Reads back the oldest queued frame (blocks until the encoder is done
    /// with it) and lets go of its input buffer.
    fn drain_one(&mut self, out: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        let Some((mapped, b)) = self.pending.pop_front() else {
            return Ok(());
        };
        let api = &*self.api;
        let session = self.session;
        let bitstream = self.bitstreams[b];
        self.with_ctx(|| unsafe {
            let mut lock: sys::NV_ENC_LOCK_BITSTREAM = zeroed();
            lock.version = LOCK_VER;
            lock.outputBitstream = bitstream;
            let r = self.call(
                api.nvEncLockBitstream.unwrap()(session, &mut lock),
                "nvEncLockBitstream",
            );
            let result = r.and_then(|_| {
                let bytes = std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr as *const u8,
                    lock.bitstreamSizeInBytes as usize,
                );
                let w = out(bytes);
                api.nvEncUnlockBitstream.unwrap()(session, bitstream);
                w
            });
            api.nvEncUnmapInputResource.unwrap()(session, mapped);
            result
        })
    }

    /// Flushes the encoder (end of stream).
    pub fn finish(&mut self, out: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        while !self.pending.is_empty() {
            self.drain_one(out)?;
        }
        let api = &*self.api;
        let session = self.session;
        self.with_ctx(|| unsafe {
            let mut pic: sys::NV_ENC_PIC_PARAMS = zeroed();
            pic.version = PIC_PARAMS_VER;
            pic.encodePicFlags = sys::_NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_EOS as u32;
            // Synchronous mode without B-frames holds no frames back, so
            // end of stream produces no output.
            let _ = out;
            self.call(
                api.nvEncEncodePicture.unwrap()(session, &mut pic),
                "nvEncEncodePicture (end of stream)",
            )
        })
    }

    fn call(&self, r: sys::NVENCSTATUS, what: &str) -> Result<(), String> {
        if r == sys::_NVENCSTATUS_NV_ENC_SUCCESS {
            return Ok(());
        }
        let detail = if self.session.is_null() {
            String::new()
        } else {
            // SAFETY: returns a static string owned by the session.
            unsafe {
                let p = self.api.nvEncGetLastErrorString.unwrap()(self.session);
                if p.is_null() {
                    String::new()
                } else {
                    format!(": {}", std::ffi::CStr::from_ptr(p).to_string_lossy())
                }
            }
        };
        Err(format!("{what}: NVENC error {r}{detail}"))
    }
}

fn nv(r: sys::NVENCSTATUS, what: &str) -> Result<(), String> {
    if r == sys::_NVENCSTATUS_NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what}: NVENC error {r}"))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let slots = std::mem::take(&mut self.slots);
        for s in &slots {
            self.release_slot(s);
        }
        // SAFETY: handles created by this encoder, destroyed once, after
        // the resources registered with them.
        unsafe {
            if !self.session.is_null() {
                let pending: Vec<_> = self.pending.drain(..).collect();
                let bitstreams = std::mem::take(&mut self.bitstreams);
                let _ = self.with_ctx(|| {
                    for (mapped, _) in pending {
                        self.api.nvEncUnmapInputResource.unwrap()(self.session, mapped);
                    }
                    for b in bitstreams {
                        self.api.nvEncDestroyBitstreamBuffer.unwrap()(self.session, b);
                    }
                    self.api.nvEncDestroyEncoder.unwrap()(self.session);
                    Ok(())
                });
            }
            // The context outlives the encoder: the process's, or this
            // encoder's own, kept for the next one.
            if let (Some(uuid), Ok(driver)) = (self.owned, driver()) {
                driver.give_back(uuid, self.ctx);
            }
        }
        // The Vulkan buffers go last: CUDA no longer maps them.
        drop(slots);
    }
}

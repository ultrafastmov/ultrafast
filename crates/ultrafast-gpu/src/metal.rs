//! Metal backend (macOS, Apple silicon first).
//!
//! Kernels come from MSL produced by the Slang compiler. The binding layout is
//! fixed for every kernel ultrafast generates:
//!
//! | binding      | contents                                        |
//! |--------------|-------------------------------------------------|
//! | texture(0)   | output                                          |
//! | texture(1..) | inputs                                          |
//! | sampler(0)   | bilinear, clamp-to-zero (transparent outside)   |
//! | buffer(0)    | params (float array), from the per-frame arena  |
//! | buffer(1..)  | extra storage buffers (geometry, accumulators)  |

use crate::stats::Counters;
use crate::{GpuError, GpuTiming, PixelFormat, TextureDesc, TransferStats};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer,
    MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue, MTLCompareFunction, MTLComputeCommandEncoder,
    MTLComputePipelineState, MTLCreateSystemDefaultDevice, MTLCullMode, MTLDepthStencilDescriptor,
    MTLDepthStencilState, MTLDevice, MTLIndexType, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter,
    MTLSamplerState, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage, MTLViewport,
};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

/// Metal objects that Apple documents as thread-safe (device, queue,
/// libraries, pipeline states, sampler states, resources) are shared across
/// the render and compile threads through this wrapper.
struct Shared<T>(T);
// SAFETY: only used for Metal objects documented as safe to use from any
// thread; mutation of resource contents is ordered by command buffers.
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

type P<T> = Retained<ProtocolObject<T>>;

/// Frames that may be encoded while earlier ones still execute.
const FRAMES_IN_FLIGHT: usize = 3;
/// Param arena per in-flight frame.
const ARENA_BYTES: usize = 32 << 20;
const ARENA_ALIGN: usize = 256;

struct RingSlot {
    arena: Shared<P<dyn MTLBuffer>>,
    pending: Option<Shared<P<dyn MTLCommandBuffer>>>,
    /// A frame is being recorded into it (from `begin` until it is
    /// submitted or dropped): another thread must not take it.
    recording: bool,
}

struct Inner {
    device: Shared<P<dyn MTLDevice>>,
    queue: Shared<P<dyn MTLCommandQueue>>,
    sampler: Shared<P<dyn MTLSamplerState>>,
    ring: Mutex<(usize, Vec<RingSlot>)>,
    counters: Counters,
}

/// Handle to the GPU. Cheap to clone and safe to share across threads.
#[derive(Clone)]
pub struct Device {
    inner: Arc<Inner>,
}

/// A compiled module holding one or more kernels.
#[derive(Clone)]
pub struct Library {
    raw: Arc<Shared<P<dyn MTLLibrary>>>,
    device: Device,
}

/// A compiled compute pipeline. Cheap to clone.
#[derive(Clone)]
pub struct Kernel {
    name: Arc<str>,
    pipeline: Arc<Shared<P<dyn MTLComputePipelineState>>>,
}

impl Kernel {
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A GPU texture. Cheap to clone (reference counted).
#[derive(Clone)]
pub struct Texture {
    raw: Arc<Shared<P<dyn MTLTexture>>>,
    desc: TextureDesc,
}

impl Texture {
    pub fn desc(&self) -> TextureDesc {
        self.desc
    }

    /// Identity for caching: two handles to the same GPU texture compare equal.
    pub fn id(&self) -> usize {
        Arc::as_ptr(&self.raw) as usize
    }

    pub fn raw(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.raw
    }
}

/// A GPU storage buffer.
#[derive(Clone)]
pub struct Buffer {
    raw: Arc<Shared<P<dyn MTLBuffer>>>,
    len: usize,
}

impl Buffer {
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this handle is the buffer's only one (nothing else, a frame
    /// in flight included, still holds it).
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.raw) == 1
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The buffer's memory, for filling it from the CPU in place. Writers
    /// report the bytes with [`Device::note_upload`].
    ///
    /// # Safety
    /// No GPU work may access the buffer, and no other slice of it may be
    /// alive, while the returned slice is.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn mapped(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.raw.0.contents().as_ptr().cast::<u8>(), self.len) }
    }
}

fn mtl_format(f: PixelFormat) -> MTLPixelFormat {
    match f {
        PixelFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        PixelFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        PixelFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        PixelFormat::R32Float => MTLPixelFormat::R32Float,
        PixelFormat::R16Float => MTLPixelFormat::R16Float,
        PixelFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        PixelFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}

impl Device {
    pub fn new() -> Result<Self, GpuError> {
        let device = MTLCreateSystemDefaultDevice().ok_or(GpuError::NoDevice)?;
        let queue = device.newCommandQueue().ok_or(GpuError::NoDevice)?;
        let sd = MTLSamplerDescriptor::new();
        sd.setMinFilter(MTLSamplerMinMagFilter::Linear);
        sd.setMagFilter(MTLSamplerMinMagFilter::Linear);
        sd.setSAddressMode(MTLSamplerAddressMode::ClampToZero);
        sd.setTAddressMode(MTLSamplerAddressMode::ClampToZero);
        let sampler = device.newSamplerStateWithDescriptor(&sd).ok_or(GpuError::NoDevice)?;
        let mut slots = Vec::new();
        for _ in 0..FRAMES_IN_FLIGHT {
            let arena = device
                .newBufferWithLength_options(ARENA_BYTES, MTLResourceOptions::StorageModeShared)
                .ok_or(GpuError::NoDevice)?;
            slots.push(RingSlot {
                arena: Shared(arena),
                pending: None,
                recording: false,
            });
        }
        Ok(Self {
            inner: Arc::new(Inner {
                device: Shared(device),
                queue: Shared(queue),
                sampler: Shared(sampler),
                ring: Mutex::new((0, slots)),
                counters: Counters::default(),
            }),
        })
    }

    /// The same as [`Device::new`]: one Metal queue serves every renderer
    /// forked from it.
    pub fn with_lanes(_lanes: usize) -> Result<Self, GpuError> {
        Self::new()
    }

    pub fn name(&self) -> String {
        self.inner.device.name().to_string()
    }

    pub fn backend(&self) -> &'static str {
        "metal"
    }

    /// A handle for a renderer on another thread (Metal queues are shared
    /// safely; same handle).
    pub fn fork(&self) -> Result<Device, GpuError> {
        Ok(self.clone())
    }

    /// GPU memory this device has allocated.
    pub fn memory_in_use(&self) -> u64 {
        self.inner.device.currentAllocatedSize() as u64
    }

    /// Metal command buffers release what they used when they complete.
    pub fn reclaim(&self) {}

    /// Pipelines loaded / compiled (Metal keeps compiled pipelines in the
    /// system's own shader cache; not counted).
    pub fn pipeline_stats(&self) -> (u64, u64, f64) {
        (0, 0, 0.0)
    }

    /// The largest groups of live allocations (not tracked on Metal).
    pub fn memory_breakdown(&self, _top: usize) -> String {
        String::new()
    }

    /// (bytes in live allocations, bytes reserved for them).
    pub fn memory_allocated(&self) -> (u64, u64) {
        let n = self.memory_in_use();
        (n, n)
    }

    /// GPU memory the process can use without hurting performance.
    pub fn memory_bytes(&self) -> u64 {
        if let Some(mb) = std::env::var("ULTRAFAST_GPU_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            // Pretend the GPU is this size (budget testing).
            return mb << 20;
        }
        self.inner.device.recommendedMaxWorkingSetSize()
    }

    pub fn stats(&self) -> TransferStats {
        self.inner.counters.snapshot()
    }

    /// Counts bytes the CPU wrote into GPU-visible memory directly
    /// ([`Buffer::mapped`]) as an upload.
    pub fn note_upload(&self, bytes: usize) {
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, bytes as u64);
    }

    /// Compiles MSL source (as produced by Slang) into a library. Safe to
    /// call from any thread.
    pub fn library(&self, msl: &[u8]) -> Result<Library, GpuError> {
        let msl = std::str::from_utf8(msl).map_err(|e| GpuError::ShaderCompile(e.to_string()))?;
        let raw = self
            .inner
            .device
            .newLibraryWithSource_options_error(&NSString::from_str(msl), None)
            .map_err(|e| GpuError::ShaderCompile(e.localizedDescription().to_string()))?;
        Ok(Library {
            raw: Arc::new(Shared(raw)),
            device: self.clone(),
        })
    }

    pub fn create_texture(&self, desc: TextureDesc) -> Texture {
        let raw = unsafe {
            let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format(desc.format),
                desc.width.max(1) as usize,
                desc.height.max(1) as usize,
                false,
            );
            // Render target too, so 3D passes can resolve into any texture.
            d.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite | MTLTextureUsage::RenderTarget);
            let (storage, options) = if desc.cpu_readable {
                (MTLStorageMode::Shared, MTLResourceOptions::StorageModeShared)
            } else {
                (MTLStorageMode::Private, MTLResourceOptions::StorageModePrivate)
            };
            d.setStorageMode(storage);
            d.setResourceOptions(options);
            self.inner
                .device
                .newTextureWithDescriptor(&d)
                .expect("texture allocation")
        };
        Counters::add(&self.inner.counters.textures_created, 1);
        Texture {
            raw: Arc::new(Shared(raw)),
            desc,
        }
    }

    /// Creates a GPU-private texture filled with `bytes` (tightly packed rows).
    /// Counted as an upload.
    /// Like [`upload_texture`](Self::upload_texture) from bytes already in
    /// a buffer (rows tightly packed).
    pub fn upload_texture_from(&self, desc: TextureDesc, buffer: &Buffer) -> Texture {
        let len = desc.width as usize * desc.height as usize * desc.format.bytes_per_pixel();
        // SAFETY: the buffer was filled before this call and is only read.
        let bytes = unsafe { &buffer.mapped()[..len] };
        self.upload_texture(desc, bytes)
    }

    pub fn upload_texture(&self, desc: TextureDesc, bytes: &[u8]) -> Texture {
        let row = desc.width as usize * desc.format.bytes_per_pixel();
        assert_eq!(bytes.len(), row * desc.height as usize, "upload size mismatch");
        let staging = self.create_texture(TextureDesc {
            cpu_readable: true,
            ..desc
        });
        unsafe {
            staging.raw.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region(desc.width, desc.height),
                0,
                NonNull::new(bytes.as_ptr() as *mut _).unwrap(),
                row,
            );
        }
        let tex = self.create_texture(TextureDesc {
            cpu_readable: false,
            ..desc
        });
        let cb = self.inner.queue.commandBuffer().expect("command buffer");
        let blit = cb.blitCommandEncoder().expect("blit encoder");
        unsafe { blit.copyFromTexture_toTexture(&staging.raw, &tex.raw) };
        blit.endEncoding();
        cb.commit();
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, bytes.len() as u64);
        tex
    }

    /// Creates a shared storage buffer. With `init`, contents are copied in
    /// and the copy is counted as an upload.
    pub fn create_buffer(&self, len: usize, init: Option<&[u8]>) -> Buffer {
        let len = len.max(16);
        let raw = self
            .inner
            .device
            .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
            .expect("buffer allocation");
        if let Some(data) = init {
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), raw.contents().as_ptr().cast(), data.len().min(len));
            }
            Counters::add(&self.inner.counters.uploads, 1);
            Counters::add(&self.inner.counters.upload_bytes, data.len() as u64);
        }
        Buffer {
            raw: Arc::new(Shared(raw)),
            len,
        }
    }

    /// A buffer only the GPU reads and writes (accumulators, simulation
    /// state). `read_buffer` / `write_buffer` must not be used on it.
    pub fn create_private_buffer(&self, len: usize) -> Buffer {
        let len = len.max(16);
        let raw = self
            .inner
            .device
            .newBufferWithLength_options(len, MTLResourceOptions::StorageModePrivate)
            .expect("buffer allocation");
        Buffer {
            raw: Arc::new(Shared(raw)),
            len,
        }
    }

    /// Overwrites the start of a shared buffer. The caller must ensure the GPU
    /// is not reading it. Counted as an upload.
    /// Copies the first `len` bytes of a buffer to the CPU (the GPU must be
    /// done writing it). Counted as a readback.
    pub fn read_buffer(&self, buf: &Buffer, len: usize) -> Vec<u8> {
        let len = len.min(buf.len);
        let mut out = vec![0u8; len];
        unsafe { std::ptr::copy_nonoverlapping(buf.raw.contents().as_ptr().cast::<u8>(), out.as_mut_ptr(), len) };
        Counters::add(&self.inner.counters.readbacks, 1);
        Counters::add(&self.inner.counters.readback_bytes, len as u64);
        out
    }

    pub fn write_buffer(&self, buf: &Buffer, data: &[u8]) {
        assert!(data.len() <= buf.len, "write past end of buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.raw.contents().as_ptr().cast(), data.len());
        }
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, data.len() as u64);
    }

    /// Starts recording a frame. Blocks only if the frame that last used this
    /// ring slot (FRAMES_IN_FLIGHT frames ago) is still executing. Threads
    /// recording at once each get a slot of their own (the ring grows when
    /// more record than it has slots).
    pub fn begin(&self) -> CommandList {
        let (slot, arena) = {
            let mut ring = self.inner.ring.lock().unwrap();
            let n = ring.1.len();
            let free = (0..n).map(|k| (ring.0 + k) % n).find(|&i| !ring.1[i].recording);
            let idx = match free {
                Some(i) => i,
                None => {
                    let arena = self
                        .inner
                        .device
                        .newBufferWithLength_options(ARENA_BYTES, MTLResourceOptions::StorageModeShared)
                        .expect("param arena");
                    ring.1.push(RingSlot {
                        arena: Shared(arena),
                        pending: None,
                        recording: false,
                    });
                    n
                }
            };
            ring.0 = (idx + 1) % ring.1.len();
            let s = &mut ring.1[idx];
            s.recording = true;
            if let Some(cb) = s.pending.take() {
                cb.waitUntilCompleted();
            }
            (idx, s.arena.0.clone())
        };
        let cb = self.inner.queue.commandBuffer().expect("command buffer");
        let enc = cb.computeCommandEncoder().expect("compute encoder");
        CommandList {
            device: self.clone(),
            cb,
            enc,
            slot,
            arena,
            arena_used: 0,
            submitted: false,
        }
    }

    /// Copies a CPU-readable texture to CPU memory, tightly packed. Counted
    /// as a readback.
    pub fn readback(&self, tex: &Texture) -> Vec<u8> {
        assert!(tex.desc.cpu_readable, "readback of a GPU-private texture");
        let (w, h) = (tex.desc.width as usize, tex.desc.height as usize);
        let row = w * tex.desc.format.bytes_per_pixel();
        let mut out = vec![0u8; row * h];
        unsafe {
            tex.raw.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new(out.as_mut_ptr().cast()).unwrap(),
                row,
                region(tex.desc.width, tex.desc.height),
                0,
            );
        }
        Counters::add(&self.inner.counters.readbacks, 1);
        Counters::add(&self.inner.counters.readback_bytes, out.len() as u64);
        out
    }

    /// Raw Metal device, for platform interop (IOSurface, CoreVideo).
    pub fn raw_device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.inner.device
    }

    /// Wraps an externally created Metal texture (for example one backed by
    /// an IOSurface from VideoToolbox). No copy.
    pub fn wrap_texture(&self, raw: P<dyn MTLTexture>, format: PixelFormat) -> Texture {
        let desc = TextureDesc {
            width: raw.width() as u32,
            height: raw.height() as u32,
            format,
            cpu_readable: false,
        };
        Texture {
            raw: Arc::new(Shared(raw)),
            desc,
        }
    }
}

impl Library {
    pub fn kernel(&self, name: &str) -> Result<Kernel, GpuError> {
        let func = self
            .raw
            .newFunctionWithName(&NSString::from_str(name))
            .ok_or_else(|| GpuError::NoSuchKernel(name.to_string()))?;
        let pipeline = self
            .device
            .inner
            .device
            .newComputePipelineStateWithFunction_error(&func)
            .map_err(|e| GpuError::Pipeline(name.to_string(), e.localizedDescription().to_string()))?;
        Ok(Kernel {
            name: name.into(),
            pipeline: Arc::new(Shared(pipeline)),
        })
    }
}

/// A graphics pipeline for 3D passes: vertex pulling from a storage buffer,
/// RGBA16F color, 32-bit depth, optional MSAA. Draw ABI (both stages): params
/// buffer(0), vertices buffer(1), texture(0), sampler(0).
#[derive(Clone)]
pub struct DrawPipeline {
    state: Arc<Shared<P<dyn MTLRenderPipelineState>>>,
    depth: Option<Arc<Shared<P<dyn MTLDepthStencilState>>>>,
    pub samples: u32,
}

/// Attachments of a 3D pass. `color` is the multisampled target (None for
/// single-sample passes, which then render straight into `resolve`).
pub struct DrawTarget<'a> {
    pub resolve: &'a Texture,
    pub color: Option<&'a Texture>,
    pub depth: Option<&'a Texture>,
    pub clear: [f32; 4],
}

/// One indexed draw.
pub struct Draw<'a> {
    pub pipeline: &'a DrawPipeline,
    pub params: &'a [f32],
    pub vertices: BufferBinding<'a>,
    pub indices: &'a Buffer,
    pub index_count: u32,
    pub texture: &'a Texture,
    /// Bound at texture(1) (shadow maps); pass any texture when unused.
    pub texture2: &'a Texture,
    /// Bound at texture(2) (environment maps); pass any texture when unused.
    pub texture3: &'a Texture,
    /// Bound at texture(3) (prefiltered environment); any texture when unused.
    pub texture4: &'a Texture,
}

impl Device {
    pub fn draw_pipeline(
        &self,
        library: &Library,
        vertex: &str,
        fragment: &str,
        samples: u32,
        depth: bool,
        blend: crate::Blend,
        color: PixelFormat,
    ) -> Result<DrawPipeline, GpuError> {
        let vf = library
            .raw
            .newFunctionWithName(&NSString::from_str(vertex))
            .ok_or_else(|| GpuError::NoSuchKernel(vertex.to_string()))?;
        let ff = library
            .raw
            .newFunctionWithName(&NSString::from_str(fragment))
            .ok_or_else(|| GpuError::NoSuchKernel(fragment.to_string()))?;
        let d = MTLRenderPipelineDescriptor::new();
        d.setVertexFunction(Some(&vf));
        d.setFragmentFunction(Some(&ff));
        unsafe {
            let c = d.colorAttachments().objectAtIndexedSubscript(0);
            c.setPixelFormat(mtl_format(color));
            // Premultiplied blending: src + dst * (1 - src.a), or additive.
            if blend != crate::Blend::Opaque {
                c.setBlendingEnabled(true);
                c.setRgbBlendOperation(MTLBlendOperation::Add);
                c.setAlphaBlendOperation(MTLBlendOperation::Add);
                c.setSourceRGBBlendFactor(MTLBlendFactor::One);
                c.setSourceAlphaBlendFactor(MTLBlendFactor::One);
                // Additive light adds color but composites alpha as over, so
                // coverage never exceeds 1.
                let dst = if blend == crate::Blend::Add {
                    MTLBlendFactor::One
                } else {
                    MTLBlendFactor::OneMinusSourceAlpha
                };
                c.setDestinationRGBBlendFactor(dst);
                c.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            }
            d.setRasterSampleCount(samples as usize);
        }
        if depth {
            d.setDepthAttachmentPixelFormat(MTLPixelFormat::Depth32Float);
        }
        let state = self
            .inner
            .device
            .newRenderPipelineStateWithDescriptor_error(&d)
            .map_err(|e| GpuError::Pipeline(vertex.to_string(), e.localizedDescription().to_string()))?;
        let depth = if depth {
            let dd = MTLDepthStencilDescriptor::new();
            dd.setDepthCompareFunction(MTLCompareFunction::Less);
            dd.setDepthWriteEnabled(true);
            Some(Arc::new(Shared(
                self.inner
                    .device
                    .newDepthStencilStateWithDescriptor(&dd)
                    .ok_or(GpuError::NoDevice)?,
            )))
        } else {
            None
        };
        Ok(DrawPipeline {
            state: Arc::new(Shared(state)),
            depth,
            samples,
        })
    }

    /// A render attachment: multisampled color, or depth. GPU-only memory
    /// (memoryless on Apple silicon: it never leaves tile memory).
    pub fn create_attachment(&self, width: u32, height: u32, format: PixelFormat, samples: u32) -> Texture {
        let raw = unsafe {
            let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                mtl_format(format),
                width.max(1) as usize,
                height.max(1) as usize,
                false,
            );
            if samples > 1 {
                d.setTextureType(MTLTextureType::Type2DMultisample);
                d.setSampleCount(samples as usize);
            }
            d.setUsage(MTLTextureUsage::RenderTarget);
            d.setStorageMode(MTLStorageMode::Memoryless);
            self.inner
                .device
                .newTextureWithDescriptor(&d)
                .expect("attachment allocation")
        };
        Counters::add(&self.inner.counters.textures_created, 1);
        Texture {
            raw: Arc::new(Shared(raw)),
            desc: TextureDesc {
                width,
                height,
                format,
                cpu_readable: false,
            },
        }
    }
}

fn region(w: u32, h: u32) -> MTLRegion {
    MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width: w as usize,
            height: h as usize,
            depth: 1,
        },
    }
}

/// Records dispatches for one frame into one command buffer.
pub struct CommandList {
    device: Device,
    cb: P<dyn MTLCommandBuffer>,
    enc: P<dyn MTLComputeCommandEncoder>,
    slot: usize,
    arena: P<dyn MTLBuffer>,
    arena_used: usize,
    submitted: bool,
}

impl Drop for CommandList {
    /// A list abandoned mid-frame (an error while encoding) is ended and
    /// discarded instead of tripping Metal's encoder assertions.
    fn drop(&mut self) {
        if !self.submitted {
            self.enc.endEncoding();
            self.device.inner.ring.lock().unwrap().1[self.slot].recording = false;
        }
    }
}

/// A storage buffer binding: a persistent buffer, or bytes copied into the
/// frame's arena (per-frame data such as dynamic geometry).
pub enum BufferBinding<'a> {
    Buffer(&'a Buffer),
    Data(&'a [u8]),
}

/// Extra bindings for kernels that need more than textures and params.
#[derive(Default)]
pub struct Bindings<'a> {
    /// Bound at buffer(1..).
    pub buffers: &'a [BufferBinding<'a>],
    /// Overrides the dispatch grid in threads (default: output texture size).
    pub grid: Option<(u32, u32)>,
    /// Overrides the threadgroup size (needed by kernels using groupshared
    /// memory).
    pub threadgroup: Option<(u32, u32)>,
}

impl CommandList {
    /// Runs `kernel` over every pixel of `out`.
    pub fn dispatch(&mut self, kernel: &Kernel, out: &Texture, inputs: &[&Texture], params: &[f32]) {
        self.dispatch_ex(kernel, out, inputs, params, Bindings::default());
    }

    pub fn dispatch_ex(&mut self, kernel: &Kernel, out: &Texture, inputs: &[&Texture], params: &[f32], b: Bindings) {
        self.enc.setComputePipelineState(&kernel.pipeline);
        unsafe {
            self.enc.setTexture_atIndex(Some(&out.raw), 0);
            for (i, t) in inputs.iter().enumerate() {
                self.enc.setTexture_atIndex(Some(&t.raw), i + 1);
            }
            self.enc.setSamplerState_atIndex(Some(&self.device.inner.sampler), 0);
        }
        for (i, binding) in b.buffers.iter().enumerate() {
            match binding {
                BufferBinding::Buffer(buf) => unsafe { self.enc.setBuffer_offset_atIndex(Some(&buf.raw), 0, i + 1) },
                BufferBinding::Data(data) => {
                    let offset = self.arena_write(data);
                    unsafe { self.enc.setBuffer_offset_atIndex(Some(&self.arena), offset, i + 1) };
                    Counters::add(&self.device.inner.counters.upload_bytes, data.len() as u64);
                    Counters::add(&self.device.inner.counters.uploads, 1);
                }
            }
        }
        // Params go into the frame's arena: no allocation, one write into
        // shared memory.
        let bytes = unsafe { std::slice::from_raw_parts(params.as_ptr().cast::<u8>(), std::mem::size_of_val(params)) };
        let offset = self.arena_write(bytes);
        unsafe { self.enc.setBuffer_offset_atIndex(Some(&self.arena), offset, 0) };
        Counters::add(&self.device.inner.counters.uniform_bytes, bytes.len() as u64);

        let (gw, gh) = b.grid.unwrap_or((out.desc.width, out.desc.height));
        let (tw, th) = match b.threadgroup {
            Some((x, y)) => (x as usize, y as usize),
            None => {
                let w = kernel.pipeline.threadExecutionWidth();
                (w, (kernel.pipeline.maxTotalThreadsPerThreadgroup() / w).clamp(1, 16))
            }
        };
        self.enc.dispatchThreads_threadsPerThreadgroup(
            MTLSize {
                width: gw.max(1) as usize,
                height: gh.max(1) as usize,
                depth: 1,
            },
            MTLSize {
                width: tw,
                height: th,
                depth: 1,
            },
        );
        Counters::add(&self.device.inner.counters.dispatches, 1);
    }

    /// Copies bytes into the frame arena; returns the offset into
    /// `self.arena`. A frame that outgrows the arena (dense geometry under a
    /// deep zoom, many motion-blur samples) continues in a fresh overflow
    /// buffer, kept alive by the command buffer that binds it; read
    /// `self.arena` after each write.
    fn arena_write(&mut self, data: &[u8]) -> usize {
        let bytes = data.len().max(16);
        if self.arena_used + bytes > self.arena.length() {
            let len = ARENA_BYTES.max(bytes.next_multiple_of(ARENA_ALIGN));
            self.arena = self
                .device
                .inner
                .device
                .newBufferWithLength_options(len, MTLResourceOptions::StorageModeShared)
                .expect("overflow arena");
            self.arena_used = 0;
        }
        let offset = self.arena_used;
        unsafe {
            let dst = self.arena.contents().as_ptr().cast::<u8>().add(offset);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        self.arena_used = (offset + bytes).next_multiple_of(ARENA_ALIGN);
        offset
    }

    /// Zeroes `len` bytes of a buffer starting at `offset` (GPU side).
    pub fn clear_buffer_range(&mut self, buf: &Buffer, offset: usize, len: usize) {
        let len = len.min(buf.len.saturating_sub(offset));
        if len == 0 {
            return;
        }
        self.enc.endEncoding();
        let blit = self.cb.blitCommandEncoder().expect("blit encoder");
        blit.fillBuffer_range_value(&buf.raw, objc2_foundation::NSRange::new(offset, len), 0);
        blit.endEncoding();
        self.enc = self.cb.computeCommandEncoder().expect("compute encoder");
    }

    /// Runs a 3D pass: clears, draws, and resolves into `target.resolve`.
    pub fn draw_pass(&mut self, target: DrawTarget, draws: &[Draw]) {
        self.enc.endEncoding();
        let pass = MTLRenderPassDescriptor::new();
        unsafe {
            let c = pass.colorAttachments().objectAtIndexedSubscript(0);
            let [r, g, b, a] = target.clear;
            c.setClearColor(MTLClearColor {
                red: r as f64,
                green: g as f64,
                blue: b as f64,
                alpha: a as f64,
            });
            c.setLoadAction(MTLLoadAction::Clear);
            match target.color {
                Some(msaa) => {
                    c.setTexture(Some(&msaa.raw));
                    c.setResolveTexture(Some(&target.resolve.raw));
                    c.setStoreAction(MTLStoreAction::MultisampleResolve);
                }
                None => {
                    c.setTexture(Some(&target.resolve.raw));
                    c.setStoreAction(MTLStoreAction::Store);
                }
            }
            if let Some(depth) = target.depth {
                let d = pass.depthAttachment();
                d.setTexture(Some(&depth.raw));
                d.setClearDepth(1.0);
                d.setLoadAction(MTLLoadAction::Clear);
                d.setStoreAction(MTLStoreAction::DontCare);
            }
        }
        let enc = self
            .cb
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("render encoder");
        let (w, h) = (target.resolve.desc.width as f64, target.resolve.desc.height as f64);
        enc.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: w,
            height: h,
            znear: 0.0,
            zfar: 1.0,
        });
        enc.setCullMode(MTLCullMode::None);
        for d in draws {
            enc.setRenderPipelineState(&d.pipeline.state);
            if let Some(ds) = &d.pipeline.depth {
                enc.setDepthStencilState(Some(ds));
            }
            let bytes =
                unsafe { std::slice::from_raw_parts(d.params.as_ptr().cast::<u8>(), std::mem::size_of_val(d.params)) };
            let off = self.arena_write(bytes);
            let pbuf = self.arena.clone();
            Counters::add(&self.device.inner.counters.uniform_bytes, bytes.len() as u64);
            // Vertices: a persistent buffer, or per-frame data in the arena.
            let (vbuf, voff) = match &d.vertices {
                BufferBinding::Buffer(b) => ((*b.raw).clone(), 0),
                BufferBinding::Data(data) => {
                    let o = self.arena_write(data);
                    Counters::add(&self.device.inner.counters.uploads, 1);
                    Counters::add(&self.device.inner.counters.upload_bytes, data.len() as u64);
                    (self.arena.clone(), o)
                }
            };
            unsafe {
                enc.setVertexBuffer_offset_atIndex(Some(&pbuf), off, 0);
                enc.setFragmentBuffer_offset_atIndex(Some(&pbuf), off, 0);
                enc.setVertexBuffer_offset_atIndex(Some(&vbuf), voff, 1);
                enc.setFragmentBuffer_offset_atIndex(Some(&vbuf), voff, 1);
                enc.setFragmentTexture_atIndex(Some(&d.texture.raw), 0);
                enc.setFragmentTexture_atIndex(Some(&d.texture2.raw), 1);
                enc.setFragmentTexture_atIndex(Some(&d.texture3.raw), 2);
                enc.setFragmentTexture_atIndex(Some(&d.texture4.raw), 3);
                enc.setFragmentSamplerState_atIndex(Some(&self.device.inner.sampler), 0);
                enc.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                    MTLPrimitiveType::Triangle,
                    d.index_count as usize,
                    MTLIndexType::UInt32,
                    &d.indices.raw,
                    0,
                );
            }
            Counters::add(&self.device.inner.counters.dispatches, 1);
        }
        enc.endEncoding();
        self.enc = self.cb.computeCommandEncoder().expect("compute encoder");
    }

    /// Fills a buffer with zeros (GPU side, no transfer).
    pub fn clear_buffer(&mut self, buf: &Buffer) {
        // Compute encoders cannot fill; end, blit, and resume.
        self.enc.endEncoding();
        let blit = self.cb.blitCommandEncoder().expect("blit encoder");
        blit.fillBuffer_range_value(&buf.raw, objc2_foundation::NSRange::new(0, buf.len), 0);
        blit.endEncoding();
        self.enc = self.cb.computeCommandEncoder().expect("compute encoder");
    }

    /// Copies one texture to another of identical size and format (GPU side).
    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture) {
        self.enc.endEncoding();
        let blit = self.cb.blitCommandEncoder().expect("blit encoder");
        unsafe { blit.copyFromTexture_toTexture(&src.raw, &dst.raw) };
        blit.endEncoding();
        self.enc = self.cb.computeCommandEncoder().expect("compute encoder");
    }

    /// Schedules `drawable` to be shown when this frame completes.
    pub fn present(&mut self, drawable: &crate::surface::Drawable) {
        self.cb.presentDrawable(drawable.as_mtl());
    }

    pub fn submit(mut self) -> Submitted {
        self.enc.endEncoding();
        self.submitted = true;
        self.cb.commit();
        let mut ring = self.device.inner.ring.lock().unwrap();
        ring.1[self.slot].pending = Some(Shared(self.cb.clone()));
        ring.1[self.slot].recording = false;
        Submitted { cb: self.cb.clone() }
    }
}

pub struct Submitted {
    cb: P<dyn MTLCommandBuffer>,
}

impl Submitted {
    /// GPU timing if the frame has finished, without blocking.
    pub fn try_timing(&self) -> Option<Result<GpuTiming, GpuError>> {
        match self.cb.status() {
            MTLCommandBufferStatus::Completed => Some(Ok(GpuTiming {
                gpu_ms: (self.cb.GPUEndTime() - self.cb.GPUStartTime()) * 1000.0,
            })),
            MTLCommandBufferStatus::Error => Some(Err(GpuError::Execution(
                self.cb
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_default(),
            ))),
            _ => None,
        }
    }

    pub fn wait(self) -> Result<GpuTiming, GpuError> {
        self.cb.waitUntilCompleted();
        if self.cb.status() == MTLCommandBufferStatus::Error {
            let msg = self
                .cb
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_default();
            return Err(GpuError::Execution(msg));
        }
        let gpu_ms = (self.cb.GPUEndTime() - self.cb.GPUStartTime()) * 1000.0;
        Ok(GpuTiming { gpu_ms })
    }
}

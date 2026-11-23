//! Vulkan backend (Linux / NVIDIA L4, and macOS through MoltenVK for testing).
//!
//! Same kernel ABI as the Metal backend, mapped to one descriptor set that
//! is pushed per dispatch (VK_KHR_push_descriptor):
//!
//! | binding | contents                         | Metal equivalent |
//! |---------|----------------------------------|------------------|
//! | 0       | output storage image             | texture(0)       |
//! | 1..=16  | input sampled images (T0..T15)   | texture(1..)     |
//! | 17      | sampler (bilinear, clamp-to-zero)| sampler(0)       |
//! | 18      | params storage buffer (arena)    | buffer(0)        |
//! | 19..    | extra storage buffers (B1, B2)   | buffer(1..)      |
//!
//! Images live in GENERAL layout for their whole life. Command lists track
//! what each command reads and writes and put a memory barrier only before a
//! command that reads or overwrites something an earlier one wrote since the
//! last barrier (or writes what one read), as Metal's hazard tracking does:
//! independent dispatches (the many small raster and group kernels of a
//! frame) overlap on the GPU instead of draining it after every one.

use crate::stats::Counters;
use crate::{GpuError, GpuTiming, PixelFormat, TextureDesc, TransferStats};
use ash::vk;
use ash::vk::Handle;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc};
use std::ffi::{CStr, CString};
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

#[cfg(windows)]
#[path = "vulkan_surface.rs"]
mod surface;
#[cfg(windows)]
pub use surface::{Drawable, Surface};

/// An OS handle to GPU memory shared with CUDA: a file descriptor, or a
/// Windows NT handle.
#[cfg(not(windows))]
pub type ExportedMemory = i32;
#[cfg(windows)]
pub type ExportedMemory = vk::HANDLE;

const FRAMES_IN_FLIGHT: usize = 3;
const ARENA_BYTES: usize = 32 << 20;
const ARENA_ALIGN: usize = 256;
/// Allocations at least this large get their own device memory.
const DEDICATED_BYTES: u64 = 8 << 20;

#[derive(Clone, Copy)]
enum Dedicated {
    Image(vk::Image),
    Buffer(vk::Buffer),
}
const MAX_INPUTS: u32 = 16;
const BINDING_SAMPLER: u32 = 17;
const BINDING_PARAMS: u32 = 18;
const EXTRA_BUFFERS: u32 = 2;

fn err(e: impl std::fmt::Display) -> GpuError {
    GpuError::Execution(e.to_string())
}

/// A Vulkan list (extensions, devices), asked again while the driver says
/// it changed under the call (VK_INCOMPLETE): on Windows even the count
/// query does this now and then when several threads create devices at
/// once, and ash passes it on as an error.
fn listed<T>(what: &str, f: impl Fn() -> ash::prelude::VkResult<Vec<T>>) -> Result<Vec<T>, GpuError> {
    for wait in [1, 2, 4, 8, 16, 32] {
        match f() {
            Err(vk::Result::INCOMPLETE) => std::thread::sleep(std::time::Duration::from_millis(wait)),
            r => return r.map_err(|e| GpuError::Execution(format!("{what}: {e}"))),
        }
    }
    f().map_err(|e| GpuError::Execution(format!("{what}: {e}")))
}

/// Where compiled pipelines for a driver (by its pipeline-cache UUID) are
/// kept, one file per pipeline: `$ULTRAFAST_CACHE_DIR/pipelines/<uuid>`, else
/// under the user's cache directory. `ULTRAFAST_PIPELINE_CACHE=0` turns the
/// cache off.
fn pipeline_cache_dir(uuid: &[u8; 16]) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if std::env::var_os("ULTRAFAST_PIPELINE_CACHE").is_some_and(|v| v == "0") {
        return None;
    }
    let dir = std::env::var_os("ULTRAFAST_CACHE_DIR").map(PathBuf::from).or_else(|| {
        if cfg!(windows) {
            return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ultrafast"));
        }
        let home = PathBuf::from(std::env::var_os("HOME")?);
        Some(if cfg!(target_os = "macos") {
            home.join("Library/Caches/ultrafast")
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .map(|d| PathBuf::from(d).join("ultrafast"))
                .unwrap_or_else(|| home.join(".cache/ultrafast"))
        })
    })?;
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    Some(dir.join("pipelines").join(hex))
}

/// `$ULTRAFAST_SHARED_CACHE_DIR/pipelines/<uuid>`: compiled pipelines other
/// machines with this driver stored (a bucket mount), read on a local miss
/// and written after a compile.
fn shared_pipeline_dir(uuid: &[u8; 16]) -> Option<std::path::PathBuf> {
    if std::env::var_os("ULTRAFAST_PIPELINE_CACHE").is_some_and(|v| v == "0") {
        return None;
    }
    let dir = std::env::var_os("ULTRAFAST_SHARED_CACHE_DIR").filter(|d| !d.is_empty())?;
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    Some(std::path::PathBuf::from(dir).join("pipelines").join(hex))
}

/// Writes `data` to `path` whole (write, then rename): another process may
/// be reading it.
fn write_whole(path: &std::path::Path, data: &[u8]) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// A 128-bit content hash (two differently seeded SipHashes).
fn hash128(parts: &[&[u8]]) -> u128 {
    use std::hash::Hasher;
    let half = |seed: u64| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write_u64(seed);
        for p in parts {
            h.write_usize(p.len());
            h.write(p);
        }
        h.finish()
    };
    ((half(0x9e37_79b9) as u128) << 64) | half(0x7f4a_7c15) as u128
}

/// Creates a pipeline through its own stored driver binary: a pipeline
/// built before (by any process, for this driver) loads in place of a
/// driver compile; a new one is compiled and its binary written next to
/// the others. One file per pipeline, so a process reads only what it
/// uses and concurrent writers never merge.
fn cached_pipeline(
    inner: &Inner,
    key: u128,
    create: impl FnOnce(vk::PipelineCache) -> Result<vk::Pipeline, vk::Result>,
) -> Result<vk::Pipeline, vk::Result> {
    use std::sync::atomic::Ordering;
    let path = inner.cache_dir.as_ref().map(|d| d.join(format!("{key:032x}.bin")));
    let shared = inner.shared_dir.as_ref().map(|d| d.join(format!("{key:032x}.bin")));
    let stored = path.as_ref().and_then(|p| std::fs::read(p).ok()).or_else(|| {
        // Built before on another machine: kept locally from now on.
        let data = std::fs::read(shared.as_ref()?).ok()?;
        if let Some(p) = &path {
            write_whole(p, &data);
        }
        Some(data)
    });
    let d = &inner.device;
    let cache = match &stored {
        Some(data) => unsafe {
            d.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default().initial_data(data), None)
        },
        None => unsafe { d.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None) },
    }
    .unwrap_or(vk::PipelineCache::null());
    let t0 = std::time::Instant::now();
    let result = create(cache);
    if stored.is_some() {
        inner.cache_hits.fetch_add(1, Ordering::Relaxed);
    } else {
        inner.cache_compiled.fetch_add(1, Ordering::Relaxed);
        inner
            .cache_compile_us
            .fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    if result.is_ok()
        && stored.is_none()
        && cache != vk::PipelineCache::null()
        && (path.is_some() || shared.is_some())
        && let Ok(data) = unsafe { d.get_pipeline_cache_data(cache) }
    {
        if let Some(path) = &path {
            write_whole(path, &data);
        }
        // Shared with the other machines off this thread.
        if let Some(shared) = shared {
            std::thread::spawn(move || write_whole(&shared, &data));
        }
    }
    if cache != vk::PipelineCache::null() {
        unsafe { d.destroy_pipeline_cache(cache, None) };
    }
    result
}

/// A buffer and its memory: a sub-allocation, or memory of its own
/// (exported to another API). `ptr` is null for GPU-only memory.
struct RawBuffer {
    buffer: vk::Buffer,
    alloc: Option<Allocation>,
    own: vk::DeviceMemory,
    ptr: *mut u8,
    len: usize,
}

struct Slot {
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    fence: vk::Fence,
    queries: vk::QueryPool,
    arena: RawBuffer,
    generation: u64,
    pending: bool,
    /// A frame is being recorded into it (from `begin` until it is
    /// submitted or dropped): another thread must not take it.
    recording: bool,
    /// Resources the last submission used, kept alive until its fence is
    /// waited (Metal command buffers retain them implicitly; Vulkan doesn't).
    retained: Vec<Retained>,
}

#[allow(dead_code)]
enum Retained {
    Texture(Arc<TexInner>),
    Buffer(Arc<BufInner>),
    Kernel(Arc<KernelInner>),
}

struct Inner {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    push: ash::khr::push_descriptor::Device,
    /// The family's queues; each [`Device`] handle submits to one (a lane).
    queues: Vec<Mutex<vk::Queue>>,
    next_lane: std::sync::atomic::AtomicUsize,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    pdev: vk::PhysicalDevice,
    /// VK_EXT_memory_budget is enabled: real per-heap usage is known.
    memory_budget: bool,
    #[cfg_attr(windows, allow(dead_code))]
    external_fd: Option<ash::khr::external_memory_fd::Device>,
    /// The same as Windows handles (NVENC on Windows).
    #[cfg(windows)]
    external_win32: Option<ash::khr::external_memory_win32::Device>,
    /// The instance can make window surfaces (Windows: the preview window).
    #[cfg_attr(not(windows), allow(dead_code))]
    surface_ext: bool,
    /// Presenting to windows (VK_KHR_swapchain), when `surface_ext`.
    #[cfg_attr(not(windows), allow(dead_code))]
    swapchain: Option<ash::khr::swapchain::Device>,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    draw_set_layout: vk::DescriptorSetLayout,
    draw_layout: vk::PipelineLayout,
    sampler: vk::Sampler,
    oneshot: Mutex<(vk::CommandPool, vk::CommandBuffer, vk::Fence)>,
    counters: Counters,
    /// Sub-allocates every image and buffer from large blocks: drivers cap
    /// the number of live allocations (4096 on NVIDIA) and each costs a
    /// kernel call.
    allocator: Mutex<ManuallyDrop<Allocator>>,
    timestamp_period: f64,
    queue_family: u32,
    name: String,
    /// Compiled pipelines carried between processes ([`cached_pipeline`]).
    cache_dir: Option<std::path::PathBuf>,
    /// The same, shared between machines.
    shared_dir: Option<std::path::PathBuf>,
    /// Pipelines loaded from stored binaries, compiled by the driver, and
    /// the time those compiles took.
    cache_hits: std::sync::atomic::AtomicU64,
    cache_compiled: std::sync::atomic::AtomicU64,
    cache_compile_us: std::sync::atomic::AtomicU64,
}

// SAFETY: Vulkan handles are plain identifiers; every externally
// synchronized object (queue, command pools, ring) is behind a Mutex.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            let d = &self.device;
            // A panic elsewhere may have poisoned a lock; tear down anyway.
            let (pool, _, fence) = *self.oneshot.get_mut().unwrap_or_else(|e| e.into_inner());
            d.destroy_command_pool(pool, None);
            d.destroy_fence(fence, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_pipeline_layout(self.draw_layout, None);
            d.destroy_descriptor_set_layout(self.draw_set_layout, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            ManuallyDrop::drop(&mut self.allocator.lock().unwrap_or_else(|e| e.into_inner()));
            d.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

/// Handle to the GPU. Cheap to clone and safe to share across threads.
/// Clones submit to the same queue; [`Device::fork`] makes a handle with
/// its own queue, so renderers on several threads run side by side on the
/// GPU instead of waiting on each other's barriers.
#[derive(Clone)]
pub struct Device {
    inner: Arc<Inner>,
    lane: Arc<Lane>,
}

/// One queue and its ring of frames in flight.
struct Lane {
    queue: usize,
    ring: Mutex<(usize, Vec<Slot>)>,
    /// Work new textures need before first use (layout setup, uploads),
    /// recorded at the front of this lane's next command list instead of
    /// a submit-and-wait of its own per texture.
    pending: Mutex<Vec<Pending>>,
    inner: Arc<Inner>,
}

enum Pending {
    /// UNDEFINED -> GENERAL.
    Init(Arc<TexInner>),
    /// Copy tightly packed rows from a staging buffer (after its Init).
    Upload(Arc<TexInner>, Arc<BufInner>),
}

impl Lane {
    /// Records pending setup and uploads into `cb`, followed by a barrier;
    /// returns what the command buffer must keep alive. Nothing if none.
    fn flush(&self, cb: vk::CommandBuffer) -> Vec<Retained> {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        if pending.is_empty() {
            return Vec::new();
        }
        let d = &self.inner.device;
        let mut keep = Vec::with_capacity(pending.len());
        let inits: Vec<vk::ImageMemoryBarrier> = pending
            .iter()
            .filter_map(|p| match p {
                Pending::Init(t) => Some(
                    vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(t.image)
                        .subresource_range(color_range())
                        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE),
                ),
                _ => None,
            })
            .collect();
        unsafe {
            if !inits.is_empty() {
                d.cmd_pipeline_barrier(
                    cb,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &inits,
                );
            }
            for p in pending {
                match p {
                    Pending::Init(t) => keep.push(Retained::Texture(t)),
                    Pending::Upload(t, b) => {
                        let region = [buffer_image_region(t.desc.width, t.desc.height)];
                        d.cmd_copy_buffer_to_image(cb, b.raw.buffer, t.image, vk::ImageLayout::GENERAL, &region);
                        keep.push(Retained::Texture(t));
                        keep.push(Retained::Buffer(b));
                    }
                }
            }
        }
        self.inner.full_barrier(cb);
        keep
    }
}

// SAFETY: the ring's slots (mapped arena pointers, command pools) are only
// touched behind its Mutex.
unsafe impl Send for Lane {}
unsafe impl Sync for Lane {}

impl Drop for Lane {
    fn drop(&mut self) {
        let inner = &self.inner;
        unsafe {
            for mut s in self.ring.get_mut().unwrap_or_else(|e| e.into_inner()).1.drain(..) {
                let _ = inner.device.wait_for_fences(&[s.fence], true, u64::MAX);
                inner.device.destroy_command_pool(s.pool, None);
                inner.device.destroy_fence(s.fence, None);
                inner.device.destroy_query_pool(s.queries, None);
                inner.destroy_buffer(&mut s.arena);
            }
        }
    }
}

impl Lane {
    fn new(inner: &Arc<Inner>) -> Result<Self, GpuError> {
        let n = inner.queues.len();
        let queue = inner.next_lane.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % n;
        let mut slots = Vec::new();
        for _ in 0..FRAMES_IN_FLIGHT {
            slots.push(inner.new_slot()?);
        }
        Ok(Self {
            queue,
            ring: Mutex::new((0, slots)),
            pending: Mutex::new(Vec::new()),
            inner: inner.clone(),
        })
    }
}

struct TexInner {
    image: vk::Image,
    view: vk::ImageView,
    alloc: Option<Allocation>,
    desc: TextureDesc,
    device: Arc<Inner>,
    /// A window image: the swapchain owns it (only the view is ours).
    swapchain: bool,
}

impl Drop for TexInner {
    fn drop(&mut self) {
        unsafe {
            self.device.device.destroy_image_view(self.view, None);
            if !self.swapchain {
                self.device.device.destroy_image(self.image, None);
            }
        }
        if let Some(a) = self.alloc.take() {
            self.device.free(a);
        }
    }
}

/// A GPU texture. Cheap to clone (reference counted).
#[derive(Clone)]
pub struct Texture {
    inner: Arc<TexInner>,
}

impl Texture {
    pub fn desc(&self) -> TextureDesc {
        self.inner.desc
    }

    /// Identity for caching: two handles to the same GPU texture compare equal.
    pub fn id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }
}

struct BufInner {
    raw: RawBuffer,
    device: Arc<Inner>,
}

impl Drop for BufInner {
    fn drop(&mut self) {
        self.device.destroy_buffer(&mut self.raw);
    }
}

// SAFETY: the mapped pointer is only written by the owner between frames.
unsafe impl Send for BufInner {}
unsafe impl Sync for BufInner {}

/// A GPU storage buffer (host-visible).
#[derive(Clone)]
pub struct Buffer {
    inner: Arc<BufInner>,
}

impl Buffer {
    pub fn len(&self) -> usize {
        self.inner.raw.len
    }

    /// Whether this handle is the buffer's only one (nothing else, a frame
    /// in flight included, still holds it).
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    pub fn is_empty(&self) -> bool {
        self.inner.raw.len == 0
    }

    /// The buffer's memory, for filling it from the CPU in place (a
    /// decoder reading frames straight into GPU-visible memory). Writers
    /// report the bytes with [`Device::note_upload`].
    ///
    /// # Safety
    /// No GPU work may access the buffer, and no other slice of it may be
    /// alive, while the returned slice is.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn mapped(&self) -> &mut [u8] {
        let r = &self.inner.raw;
        assert!(!r.ptr.is_null(), "buffer is not host-visible");
        unsafe { std::slice::from_raw_parts_mut(r.ptr, r.len) }
    }
}

struct LibInner {
    module: vk::ShaderModule,
    /// Content hash of the SPIR-V (the pipeline cache key's base).
    hash: u128,
    /// Entry point name -> workgroup size, parsed from the SPIR-V.
    local_sizes: Vec<(String, [u32; 3])>,
    device: Arc<Inner>,
}

impl Drop for LibInner {
    fn drop(&mut self) {
        unsafe { self.device.device.destroy_shader_module(self.module, None) };
    }
}

/// A compiled SPIR-V module holding one or more kernels.
#[derive(Clone)]
pub struct Library {
    inner: Arc<LibInner>,
}

struct KernelInner {
    pipeline: vk::Pipeline,
    local: [u32; 3],
    name: String,
    device: Arc<Inner>,
}

impl Drop for KernelInner {
    fn drop(&mut self) {
        unsafe { self.device.device.destroy_pipeline(self.pipeline, None) };
    }
}

/// A compiled compute pipeline. Cheap to clone.
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<KernelInner>,
}

impl Kernel {
    pub fn name(&self) -> &str {
        &self.inner.name
    }
}

fn vk_format(f: PixelFormat) -> vk::Format {
    match f {
        PixelFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        PixelFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        PixelFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        PixelFormat::R32Float => vk::Format::R32_SFLOAT,
        PixelFormat::R16Float => vk::Format::R16_SFLOAT,
        PixelFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        PixelFormat::Depth32Float => vk::Format::D32_SFLOAT,
    }
}

/// A command pool with one primary command buffer and a signaled fence.
fn new_pool(
    device: &ash::Device,
    queue_family: u32,
) -> Result<(vk::CommandPool, vk::CommandBuffer, vk::Fence), GpuError> {
    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default().queue_family_index(queue_family),
            None,
        )
    }
    .map_err(err)?;
    let cb = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
    }
    .map_err(err)?[0];
    let fence = unsafe {
        device.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )
    }
    .map_err(err)?;
    Ok((pool, cb, fence))
}

impl Inner {
    /// A ring slot: its command pool and buffer, fence, timestamp queries
    /// and param arena.
    fn new_slot(&self) -> Result<Slot, GpuError> {
        let (pool, cb, fence) = new_pool(&self.device, self.queue_family)?;
        let queries = unsafe {
            self.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )
        }
        .map_err(err)?;
        let arena = self.host_buffer(ARENA_BYTES, vk::BufferUsageFlags::STORAGE_BUFFER)?;
        Ok(Slot {
            pool,
            cb,
            fence,
            queries,
            arena,
            generation: 0,
            pending: false,
            recording: false,
            retained: Vec::new(),
        })
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32, GpuError> {
        (0..self.mem_props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(flags))
            .ok_or_else(|| err("no suitable memory type"))
    }

    fn host_buffer(&self, len: usize, usage: vk::BufferUsageFlags) -> Result<RawBuffer, GpuError> {
        self.buffer_at(len, usage, MemoryLocation::CpuToGpu, "host buffer")
    }

    /// A buffer in `loc`: CpuToGpu (written by the CPU, read by the GPU:
    /// device-local and mapped when the GPU exposes such memory), GpuToCpu
    /// (cached, for readbacks) or GpuOnly (unmapped).
    fn buffer_at(
        &self,
        len: usize,
        usage: vk::BufferUsageFlags,
        loc: MemoryLocation,
        name: &str,
    ) -> Result<RawBuffer, GpuError> {
        let len = len.max(16);
        unsafe {
            let buffer = self
                .device
                .create_buffer(&vk::BufferCreateInfo::default().size(len as u64).usage(usage), None)
                .map_err(err)?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let alloc = match self.alloc(name, req, loc, Dedicated::Buffer(buffer)) {
                Ok(a) => a,
                Err(e) => {
                    self.device.destroy_buffer(buffer, None);
                    return Err(e);
                }
            };
            self.device
                .bind_buffer_memory(buffer, alloc.memory(), alloc.offset())
                .map_err(err)?;
            let ptr = alloc
                .mapped_ptr()
                .map(|p| p.as_ptr().cast::<u8>())
                .unwrap_or(std::ptr::null_mut());
            Ok(RawBuffer {
                buffer,
                alloc: Some(alloc),
                own: vk::DeviceMemory::null(),
                ptr,
                len,
            })
        }
    }

    /// Sub-allocates `requirements` in `location`, or gives `dedicated`
    /// its own memory when it is large: big textures come and go as caches
    /// evict them, and sharing blocks with them fragments memory badly.
    fn alloc(
        &self,
        name: &str,
        requirements: vk::MemoryRequirements,
        location: MemoryLocation,
        dedicated: Dedicated,
    ) -> Result<Allocation, GpuError> {
        let big = requirements.size >= DEDICATED_BYTES;
        let (linear, allocation_scheme) = match dedicated {
            Dedicated::Image(i) if big => (false, AllocationScheme::DedicatedImage(i)),
            Dedicated::Image(_) => (false, AllocationScheme::GpuAllocatorManaged),
            Dedicated::Buffer(b) if big => (true, AllocationScheme::DedicatedBuffer(b)),
            Dedicated::Buffer(_) => (true, AllocationScheme::GpuAllocatorManaged),
        };
        self.allocator
            .lock()
            .unwrap()
            .allocate(&AllocationCreateDesc {
                name,
                requirements,
                location,
                linear,
                allocation_scheme,
            })
            .map_err(|e| GpuError::Execution(format!("{name} ({} bytes): {e}", requirements.size)))
    }

    fn free(&self, a: Allocation) {
        let _ = self.allocator.lock().unwrap_or_else(|e| e.into_inner()).free(a);
    }

    fn destroy_buffer(&self, raw: &mut RawBuffer) {
        unsafe { self.device.destroy_buffer(raw.buffer, None) };
        if let Some(a) = raw.alloc.take() {
            self.free(a);
        }
        if raw.own != vk::DeviceMemory::null() {
            unsafe { self.device.free_memory(raw.own, None) };
            raw.own = vk::DeviceMemory::null();
        }
    }

    /// Runs `record` in a one-shot command buffer and waits for it.
    fn oneshot(&self, record: impl FnOnce(vk::CommandBuffer)) -> Result<(), GpuError> {
        let os = self.oneshot.lock().unwrap();
        let (pool, cb, fence) = *os;
        unsafe {
            self.device
                .reset_command_pool(pool, vk::CommandPoolResetFlags::empty())
                .map_err(err)?;
            self.device
                .begin_command_buffer(
                    cb,
                    &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(err)?;
            record(cb);
            self.device.end_command_buffer(cb).map_err(err)?;
            self.device.reset_fences(&[fence]).map_err(err)?;
            let cbs = [cb];
            let submit = vk::SubmitInfo::default().command_buffers(&cbs);
            {
                let q = self.queues[0].lock().unwrap();
                self.device.queue_submit(*q, &[submit], fence).map_err(err)?;
            }
            self.device.wait_for_fences(&[fence], true, u64::MAX).map_err(err)?;
        }
        Ok(())
    }

    fn full_barrier(&self, cb: vk::CommandBuffer) {
        let access = vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE;
        let mb = [vk::MemoryBarrier::default()
            .src_access_mask(access)
            .dst_access_mask(access | vk::AccessFlags::HOST_READ)];
        unsafe {
            self.device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS | vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &mb,
                &[],
                &[],
            );
        }
    }
}

impl Device {
    /// The GPU with one queue: what a renderer on one thread submits to.
    pub fn new() -> Result<Self, GpuError> {
        Self::with_lanes(1)
    }

    /// The GPU with up to `lanes` queues (as many as its compute family
    /// has, at most 8), for that many renderers made with [`Device::fork`]
    /// to run side by side. Each queue costs start-up and teardown time
    /// (on NVIDIA a channel, about 25 ms each), so only callers that fork
    /// ask for more than one.
    pub fn with_lanes(lanes: usize) -> Result<Self, GpuError> {
        unsafe { Self::create(lanes) }
    }

    unsafe fn create(lanes: usize) -> Result<Self, GpuError> {
        let entry = load_entry()?;
        let app = CString::new("ultrafast").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app)
            .engine_name(&app)
            .api_version(vk::make_api_version(0, 1, 3, 0));
        let mut inst_ext: Vec<*const i8> = Vec::new();
        let mut flags = vk::InstanceCreateFlags::empty();
        let available = listed("instance extensions", || unsafe {
            entry.enumerate_instance_extension_properties(None)
        })?;
        let has_ext = |n: &CStr| available.iter().any(|e| e.extension_name_as_c_str().ok() == Some(n));
        if has_ext(ash::khr::portability_enumeration::NAME) {
            inst_ext.push(ash::khr::portability_enumeration::NAME.as_ptr());
            flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }
        if has_ext(ash::khr::get_physical_device_properties2::NAME) {
            inst_ext.push(ash::khr::get_physical_device_properties2::NAME.as_ptr());
        }
        // Window surfaces for the preview window (Windows only for now).
        let surface_ext = cfg!(windows) && has_ext(ash::khr::surface::NAME) && has_ext(ash::khr::win32_surface::NAME);
        if surface_ext {
            inst_ext.push(ash::khr::surface::NAME.as_ptr());
            inst_ext.push(ash::khr::win32_surface::NAME.as_ptr());
        }
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app_info)
                    .enabled_extension_names(&inst_ext)
                    .flags(flags),
                None,
            )
        }
        .map_err(err)?;

        // Prefer a discrete GPU with a compute queue.
        let pdevs = listed("GPUs", || unsafe { instance.enumerate_physical_devices() })?;
        let mut best: Option<(vk::PhysicalDevice, u32, i32)> = None;
        for pd in pdevs {
            let props = unsafe { instance.get_physical_device_properties(pd) };
            let qf = unsafe { instance.get_physical_device_queue_family_properties(pd) };
            let Some(family) = qf.iter().position(|q| q.queue_flags.contains(vk::QueueFlags::COMPUTE)) else {
                continue;
            };
            let score = match props.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => 3,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
                vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
                _ => 0,
            };
            if best.is_none_or(|b| score > b.2) {
                best = Some((pd, family as u32, score));
            }
        }
        let (pdev, queue_family, _) = best.ok_or(GpuError::NoDevice)?;
        let props = unsafe { instance.get_physical_device_properties(pdev) };
        let name = props
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let dev_exts = listed("device extensions", || unsafe {
            instance.enumerate_device_extension_properties(pdev)
        })?;
        let dev_has = |n: &CStr| dev_exts.iter().any(|e| e.extension_name_as_c_str().ok() == Some(n));
        if !dev_has(ash::khr::push_descriptor::NAME) {
            return Err(GpuError::Execution("VK_KHR_push_descriptor is required".into()));
        }
        let mut exts = vec![ash::khr::push_descriptor::NAME.as_ptr()];
        let portability = c"VK_KHR_portability_subset";
        if dev_has(portability) {
            exts.push(portability.as_ptr());
        }
        let memory_budget = dev_has(ash::ext::memory_budget::NAME);
        if memory_budget {
            exts.push(ash::ext::memory_budget::NAME.as_ptr());
        }
        // Exporting memory to CUDA (NVENC / NVDEC interop on Linux).
        let external_fd = dev_has(ash::khr::external_memory_fd::NAME);
        if external_fd {
            exts.push(ash::khr::external_memory_fd::NAME.as_ptr());
        }
        // The same through Windows handles (NVENC on Windows).
        #[cfg(windows)]
        let external_win32 = dev_has(ash::khr::external_memory_win32::NAME);
        #[cfg(windows)]
        if external_win32 {
            exts.push(ash::khr::external_memory_win32::NAME.as_ptr());
        }
        let swapchain_ext = surface_ext && dev_has(ash::khr::swapchain::NAME);
        if swapchain_ext {
            exts.push(ash::khr::swapchain::NAME.as_ptr());
        }
        // A queue per lane asked for, as far as the family has them
        // (NVIDIA: 16).
        let family_queues = unsafe { instance.get_physical_device_queue_family_properties(pdev) }
            [queue_family as usize]
            .queue_count
            .clamp(1, lanes.clamp(1, 8) as u32);
        let priorities = vec![1.0f32; family_queues as usize];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let features = vk::PhysicalDeviceFeatures::default()
            .shader_storage_image_write_without_format(true)
            .shader_storage_image_read_without_format(true);
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        let device = unsafe {
            instance.create_device(
                pdev,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queue_info)
                    .enabled_extension_names(&exts)
                    .enabled_features(&features)
                    .push_next(&mut f12)
                    .push_next(&mut f13),
                None,
            )
        }
        .map_err(err)?;
        let queues: Vec<Mutex<vk::Queue>> = (0..family_queues)
            .map(|i| Mutex::new(unsafe { device.get_device_queue(queue_family, i) }))
            .collect();
        let push = ash::khr::push_descriptor::Device::new(&instance, &device);
        let mem_props = unsafe { instance.get_physical_device_memory_properties(pdev) };

        // The kernel ABI: one push-descriptor set.
        let mut bindings = vec![
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        for i in 1..=MAX_INPUTS {
            bindings.push(
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            );
        }
        bindings.push(
            vk::DescriptorSetLayoutBinding::default()
                .binding(BINDING_SAMPLER)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        );
        for b in BINDING_PARAMS..=BINDING_PARAMS + EXTRA_BUFFERS {
            bindings.push(
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            );
        }
        let set_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default()
                    .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
                    .bindings(&bindings),
                None,
            )
        }
        .map_err(err)?;
        let layouts = [set_layout];
        let pipeline_layout = unsafe {
            device.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts), None)
        }
        .map_err(err)?;
        // The draw ABI: params, vertices, texture, sampler (both stages).
        let gs = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        let dtypes = [
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLER,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLED_IMAGE,
            vk::DescriptorType::SAMPLED_IMAGE,
        ];
        let draw_bindings: Vec<vk::DescriptorSetLayoutBinding> = dtypes
            .iter()
            .enumerate()
            .map(|(i, t)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i as u32)
                    .descriptor_type(*t)
                    .descriptor_count(1)
                    .stage_flags(gs)
            })
            .collect();
        let draw_set_layout = unsafe {
            device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default()
                    .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
                    .bindings(&draw_bindings),
                None,
            )
        }
        .map_err(err)?;
        let draw_layouts = [draw_set_layout];
        let draw_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&draw_layouts),
                None,
            )
        }
        .map_err(err)?;
        let sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                    .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK),
                None,
            )
        }
        .map_err(err)?;

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: pdev,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(err)?;
        if std::env::var_os("ULTRAFAST_GPU_INFO").is_some() {
            print_memory(&name, &props, &mem_props);
        }
        let external_fd_ext = external_fd.then(|| ash::khr::external_memory_fd::Device::new(&instance, &device));
        #[cfg(windows)]
        let external_win32_ext =
            external_win32.then(|| ash::khr::external_memory_win32::Device::new(&instance, &device));
        let swapchain = swapchain_ext.then(|| ash::khr::swapchain::Device::new(&instance, &device));
        // Binaries of other drivers live in other directories (and a driver
        // ignores data whose header is not its own).
        let cache_dir = pipeline_cache_dir(&props.pipeline_cache_uuid);
        let shared_dir = shared_pipeline_dir(&props.pipeline_cache_uuid);
        if std::env::var_os("ULTRAFAST_GPU_INFO").is_some() {
            eprintln!(
                "  pipeline cache: {}{}",
                cache_dir.as_ref().map_or("off".into(), |p| p.display().to_string()),
                shared_dir
                    .as_ref()
                    .map_or(String::new(), |p| format!(", shared {}", p.display()))
            );
        }
        let inner = Inner {
            _entry: entry,
            instance,
            push,
            queues,
            next_lane: Default::default(),
            mem_props,
            pdev,
            memory_budget,
            external_fd: external_fd_ext,
            #[cfg(windows)]
            external_win32: external_win32_ext,
            surface_ext,
            swapchain,
            set_layout,
            pipeline_layout,
            draw_set_layout,
            draw_layout,
            sampler,
            oneshot: Mutex::new(new_pool(&device, queue_family)?),
            counters: Counters::default(),
            allocator: Mutex::new(ManuallyDrop::new(allocator)),
            timestamp_period: props.limits.timestamp_period as f64,
            queue_family,
            name,
            device,
            cache_dir,
            shared_dir,
            cache_hits: Default::default(),
            cache_compiled: Default::default(),
            cache_compile_us: Default::default(),
        };
        let inner = Arc::new(inner);
        let lane = Arc::new(Lane::new(&inner)?);
        Ok(Self { inner, lane })
    }

    /// A handle to the same GPU that submits to its own queue (the next
    /// one round-robin): for a renderer on another thread.
    pub fn fork(&self) -> Result<Device, GpuError> {
        Ok(Self {
            inner: self.inner.clone(),
            lane: Arc::new(Lane::new(&self.inner)?),
        })
    }

    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    pub fn backend(&self) -> &'static str {
        "vulkan"
    }

    /// Device-local memory this process uses (driver-reported with
    /// VK_EXT_memory_budget; otherwise the allocator's blocks).
    pub fn memory_in_use(&self) -> u64 {
        if self.inner.memory_budget {
            let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
            let mut props = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
            unsafe {
                self.inner
                    .instance
                    .get_physical_device_memory_properties2(self.inner.pdev, &mut props)
            };
            let m = &self.inner.mem_props;
            return (0..m.memory_heap_count as usize)
                .filter(|&i| m.memory_heaps[i].flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
                .map(|i| budget.heap_usage[i])
                .sum();
        }
        self.memory_allocated().1
    }

    /// The largest groups of live allocations by label, for debugging.
    pub fn memory_breakdown(&self, top: usize) -> String {
        let r = self.inner.allocator.lock().unwrap().generate_report();
        let mut by: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
        for a in &r.allocations {
            let kind = if a.name.starts_with("texture") && !a.name.contains("1920x1080") {
                "texture other sizes"
            } else {
                a.name.as_str()
            };
            let e = by.entry(kind).or_default();
            e.0 += a.size;
            e.1 += 1;
        }
        let mut v: Vec<_> = by.into_iter().collect();
        v.sort_by_key(|(_, (b, _))| std::cmp::Reverse(*b));
        v.iter()
            .take(top)
            .map(|(n, (b, c))| format!("{n} x{c} = {} MiB", b >> 20))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// (bytes in live allocations, bytes of memory blocks holding them).
    pub fn memory_allocated(&self) -> (u64, u64) {
        let r = self.inner.allocator.lock().unwrap().generate_report();
        (r.total_allocated_bytes, r.total_capacity_bytes)
    }

    /// Size of the largest device-local heap.
    pub fn memory_bytes(&self) -> u64 {
        if let Some(mb) = std::env::var("ULTRAFAST_GPU_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            // Pretend the GPU is this size (budget testing).
            return mb << 20;
        }
        let m = &self.inner.mem_props;
        (0..m.memory_heap_count as usize)
            .map(|i| m.memory_heaps[i])
            .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
            .map(|h| h.size)
            .max()
            .unwrap_or(0)
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

    /// Creates a library from a SPIR-V module (as produced by Slang).
    pub fn library(&self, spirv: &[u8]) -> Result<Library, GpuError> {
        if !spirv.len().is_multiple_of(4) {
            return Err(GpuError::ShaderCompile("SPIR-V size is not a multiple of 4".into()));
        }
        let words: Vec<u32> = spirv
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let module = unsafe {
            self.inner
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|e| GpuError::ShaderCompile(e.to_string()))?;
        Ok(Library {
            inner: Arc::new(LibInner {
                module,
                hash: hash128(&[spirv]),
                local_sizes: spirv_local_sizes(&words),
                device: self.inner.clone(),
            }),
        })
    }

    pub fn create_texture(&self, desc: TextureDesc) -> Texture {
        self.try_create_texture(desc)
            .or_else(|_| {
                // Out of device memory: finished frames may still hold what
                // they used (until their ring slots come round again). Let
                // that go and try once more.
                self.reclaim();
                self.try_create_texture(desc)
            })
            .expect("texture allocation")
    }

    /// Waits for this handle's frames in flight and lets go of the
    /// resources they kept alive. Frames normally release them only when
    /// their ring slot is reused; under memory pressure that delay is
    /// several frames' worth of replaced textures.
    pub fn reclaim(&self) {
        let mut ring = self.lane.ring.lock().unwrap();
        let d = &self.inner.device;
        for s in ring.1.iter_mut().filter(|s| !s.recording) {
            unsafe {
                let _ = d.wait_for_fences(&[s.fence], true, u64::MAX);
            }
            s.retained.clear();
        }
    }

    fn try_create_texture(&self, desc: TextureDesc) -> Result<Texture, GpuError> {
        let d = &self.inner.device;
        unsafe {
            let image = d
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(vk_format(desc.format))
                        .extent(vk::Extent3D {
                            width: desc.width.max(1),
                            height: desc.height.max(1),
                            depth: 1,
                        })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(
                            vk::ImageUsageFlags::STORAGE
                                | vk::ImageUsageFlags::SAMPLED
                                | vk::ImageUsageFlags::TRANSFER_SRC
                                | vk::ImageUsageFlags::TRANSFER_DST
                                | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                        )
                        .initial_layout(vk::ImageLayout::UNDEFINED),
                    None,
                )
                .map_err(err)?;
            let req = d.get_image_memory_requirements(image);
            let label = format!("texture {}x{} {:?}", desc.width, desc.height, desc.format);
            let alloc = match self
                .inner
                .alloc(&label, req, MemoryLocation::GpuOnly, Dedicated::Image(image))
            {
                Ok(a) => a,
                Err(e) => {
                    d.destroy_image(image, None);
                    return Err(e);
                }
            };
            d.bind_image_memory(image, alloc.memory(), alloc.offset())
                .map_err(err)?;
            let view = d
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk_format(desc.format))
                        .subresource_range(color_range()),
                    None,
                )
                .map_err(err)?;
            Counters::add(&self.inner.counters.textures_created, 1);
            let inner = Arc::new(TexInner {
                image,
                view,
                alloc: Some(alloc),
                desc,
                device: self.inner.clone(),
                swapchain: false,
            });
            // Images stay in GENERAL for their whole life; the transition is
            // recorded ahead of this lane's next command.
            self.lane.pending.lock().unwrap().push(Pending::Init(inner.clone()));
            Ok(Texture { inner })
        }
    }

    /// Creates a GPU texture filled with `bytes` (tightly packed rows).
    /// Counted as an upload.
    /// The copy is recorded at the front of this lane's next command list
    /// (no wait here).
    /// Like [`upload_texture`](Self::upload_texture) from bytes already in
    /// a host-visible buffer (filled through [`Buffer::mapped`] by a decoder
    /// thread): no copy on this thread, the GPU reads the buffer. The
    /// buffer must hold the texture's rows tightly packed.
    pub fn upload_texture_from(&self, desc: TextureDesc, buffer: &Buffer) -> Texture {
        let tex = self.create_texture(desc);
        self.lane
            .pending
            .lock()
            .unwrap()
            .push(Pending::Upload(tex.inner.clone(), buffer.inner.clone()));
        let bytes = desc.width as u64 * desc.height as u64 * desc.format.bytes_per_pixel() as u64;
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, bytes);
        tex
    }

    pub fn upload_texture(&self, desc: TextureDesc, bytes: &[u8]) -> Texture {
        let tex = self.create_texture(desc);
        let staging = self
            .inner
            .host_buffer(bytes.len(), vk::BufferUsageFlags::TRANSFER_SRC)
            .expect("staging buffer");
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), staging.ptr, bytes.len()) };
        let staging = Arc::new(BufInner {
            raw: staging,
            device: self.inner.clone(),
        });
        self.lane
            .pending
            .lock()
            .unwrap()
            .push(Pending::Upload(tex.inner.clone(), staging));
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, bytes.len() as u64);
        tex
    }

    pub fn create_buffer(&self, len: usize, init: Option<&[u8]>) -> Buffer {
        let raw = self
            .inner
            .host_buffer(
                len,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::INDEX_BUFFER,
            )
            .expect("buffer allocation");
        if let Some(data) = init {
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), raw.ptr, data.len().min(raw.len)) };
            Counters::add(&self.inner.counters.uploads, 1);
            Counters::add(&self.inner.counters.upload_bytes, data.len() as u64);
        }
        Buffer {
            inner: Arc::new(BufInner {
                raw,
                device: self.inner.clone(),
            }),
        }
    }

    /// Pipelines created so far: (loaded from stored binaries, compiled by
    /// the driver, seconds spent compiling).
    pub fn pipeline_stats(&self) -> (u64, u64, f64) {
        use std::sync::atomic::Ordering;
        let i = &self.inner;
        (
            i.cache_hits.load(Ordering::Relaxed),
            i.cache_compiled.load(Ordering::Relaxed),
            i.cache_compile_us.load(Ordering::Relaxed) as f64 / 1e6,
        )
    }

    /// The device's UUID (matches CUDA's `cuDeviceGetUuid` for the same GPU).
    pub fn uuid(&self) -> [u8; 16] {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        unsafe {
            self.inner
                .instance
                .get_physical_device_properties2(self.inner.pdev, &mut props)
        };
        id.device_uuid
    }

    /// A device-local storage buffer whose memory is exported for CUDA to
    /// import, so NVENC can read what kernels write without a copy: as an
    /// opaque file descriptor (VK_KHR_external_memory_fd; the importer takes
    /// ownership), or on Windows an opaque NT handle
    /// (VK_KHR_external_memory_win32; the importer must close it). Returns
    /// the buffer, the descriptor or handle, and the size of the memory
    /// behind it.
    pub fn create_exported_buffer(&self, len: usize) -> Result<(Buffer, ExportedMemory, u64), GpuError> {
        #[cfg(not(windows))]
        let (ext, handle) = (
            self.inner
                .external_fd
                .as_ref()
                .ok_or_else(|| err("VK_KHR_external_memory_fd is not supported"))?,
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        );
        #[cfg(windows)]
        let (ext, handle) = (
            self.inner
                .external_win32
                .as_ref()
                .ok_or_else(|| err("VK_KHR_external_memory_win32 is not supported"))?,
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32,
        );
        let d = &self.inner.device;
        unsafe {
            let mut ext_info = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
            let buffer = d
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(len as u64)
                        .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)
                        .push_next(&mut ext_info),
                    None,
                )
                .map_err(err)?;
            let req = d.get_buffer_memory_requirements(buffer);
            let ty = self
                .inner
                .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(handle);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
            let memory = match d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ty)
                    .push_next(&mut export)
                    .push_next(&mut dedicated),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    return Err(err(e));
                }
            };
            d.bind_buffer_memory(buffer, memory, 0).map_err(err)?;
            #[cfg(not(windows))]
            let fd = ext
                .get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(handle))
                .map_err(err)?;
            #[cfg(windows)]
            let fd = ext
                .get_memory_win32_handle(
                    &vk::MemoryGetWin32HandleInfoKHR::default()
                        .memory(memory)
                        .handle_type(handle),
                )
                .map_err(err)?;
            let buf = Buffer {
                inner: Arc::new(BufInner {
                    raw: RawBuffer {
                        buffer,
                        alloc: None,
                        own: memory,
                        ptr: std::ptr::null_mut(),
                        len,
                    },
                    device: self.inner.clone(),
                }),
            };
            Ok((buf, fd, req.size))
        }
    }

    /// A buffer only the GPU reads and writes (accumulators, simulation
    /// state): device-local, never mapped. `read_buffer` / `write_buffer`
    /// must not be used on it.
    pub fn create_private_buffer(&self, len: usize) -> Buffer {
        let raw = self
            .inner
            .buffer_at(
                len,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
                MemoryLocation::GpuOnly,
                "private buffer",
            )
            .expect("buffer allocation");
        Buffer {
            inner: Arc::new(BufInner {
                raw,
                device: self.inner.clone(),
            }),
        }
    }

    /// Copies the first `len` bytes of a buffer to the CPU (the GPU must be
    /// done writing it). Counted as a readback.
    pub fn read_buffer(&self, buf: &Buffer, len: usize) -> Vec<u8> {
        let len = len.min(buf.len());
        let mut out = vec![0u8; len];
        unsafe { std::ptr::copy_nonoverlapping(buf.inner.raw.ptr, out.as_mut_ptr(), len) };
        Counters::add(&self.inner.counters.readbacks, 1);
        Counters::add(&self.inner.counters.readback_bytes, len as u64);
        out
    }

    pub fn write_buffer(&self, buf: &Buffer, data: &[u8]) {
        assert!(data.len() <= buf.len(), "write past end of buffer");
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf.inner.raw.ptr, data.len()) };
        Counters::add(&self.inner.counters.uploads, 1);
        Counters::add(&self.inner.counters.upload_bytes, data.len() as u64);
    }

    /// Starts recording a frame. Blocks only if the frame that last used this
    /// ring slot is still executing. Threads recording at once each get a
    /// slot of their own (the ring grows when more record than it has).
    pub fn begin(&self) -> CommandList {
        let (slot, cb, arena_ptr, arena_buf, queries) = {
            let mut ring = self.lane.ring.lock().unwrap();
            let n = ring.1.len();
            let free = (0..n).map(|k| (ring.0 + k) % n).find(|&i| !ring.1[i].recording);
            let idx = match free {
                Some(i) => i,
                None => {
                    let slot = self.inner.new_slot().expect("ring slot");
                    ring.1.push(slot);
                    n
                }
            };
            ring.0 = (idx + 1) % ring.1.len();
            let s = &mut ring.1[idx];
            s.recording = true;
            let d = &self.inner.device;
            unsafe {
                d.wait_for_fences(&[s.fence], true, u64::MAX).expect("fence");
                s.retained.clear();
                d.reset_command_pool(s.pool, vk::CommandPoolResetFlags::empty())
                    .expect("reset pool");
                d.begin_command_buffer(
                    s.cb,
                    &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .expect("begin");
                d.cmd_reset_query_pool(s.cb, s.queries, 0, 2);
                d.cmd_write_timestamp(s.cb, vk::PipelineStageFlags::TOP_OF_PIPE, s.queries, 0);
                // Orders this frame after everything submitted before it
                // (barriers inside a list only track that list's commands).
                self.inner.full_barrier(s.cb);
            }
            s.generation += 1;
            s.pending = false;
            (idx, s.cb, s.arena.ptr, s.arena.buffer, s.queries)
        };
        CommandList {
            device: self.clone(),
            slot,
            cb,
            arena_ptr,
            arena_buf,
            arena_used: 0,
            overflow: None,
            written: Default::default(),
            read: Default::default(),
            queries,
            submitted: false,
            retained: Vec::new(),
            #[cfg(windows)]
            presents: Vec::new(),
            #[cfg(windows)]
            ready: Default::default(),
        }
    }

    /// Copies a CPU-readable texture to CPU memory, tightly packed. Counted
    /// as a readback.
    pub fn readback(&self, tex: &Texture) -> Vec<u8> {
        let desc = tex.inner.desc;
        let len = desc.width as usize * desc.height as usize * desc.format.bytes_per_pixel();
        let mut staging = self
            .inner
            .buffer_at(
                len,
                vk::BufferUsageFlags::TRANSFER_DST,
                MemoryLocation::GpuToCpu,
                "readback",
            )
            .expect("staging buffer");
        let d = &self.inner.device;
        let mut keep = Vec::new();
        self.inner
            .oneshot(|cb| unsafe {
                keep = self.lane.flush(cb);
                self.inner.full_barrier(cb);
                let region = [buffer_image_region(desc.width, desc.height)];
                d.cmd_copy_image_to_buffer(cb, tex.inner.image, vk::ImageLayout::GENERAL, staging.buffer, &region);
                self.inner.full_barrier(cb);
            })
            .expect("readback");
        let mut out = vec![0u8; len];
        unsafe {
            std::ptr::copy_nonoverlapping(staging.ptr, out.as_mut_ptr(), len);
        }
        self.inner.destroy_buffer(&mut staging);
        Counters::add(&self.inner.counters.readbacks, 1);
        Counters::add(&self.inner.counters.readback_bytes, len as u64);
        out
    }
}

/// Prints the device's memory heaps and types (`ULTRAFAST_GPU_INFO=1`).
fn print_memory(name: &str, props: &vk::PhysicalDeviceProperties, mem: &vk::PhysicalDeviceMemoryProperties) {
    eprintln!(
        "gpu: {name}, Vulkan {}.{}.{}, max allocations {}",
        vk::api_version_major(props.api_version),
        vk::api_version_minor(props.api_version),
        vk::api_version_patch(props.api_version),
        props.limits.max_memory_allocation_count
    );
    for i in 0..mem.memory_heap_count as usize {
        let h = mem.memory_heaps[i];
        eprintln!("  heap {i}: {} MiB {:?}", h.size >> 20, h.flags);
    }
    for i in 0..mem.memory_type_count as usize {
        let t = mem.memory_types[i];
        eprintln!("  type {i}: heap {} {:?}", t.heap_index, t.property_flags);
    }
}

/// Loads the Vulkan loader: `ULTRAFAST_VULKAN_LIB`, the system default, then
/// Homebrew locations on macOS (MoltenVK).
fn load_entry() -> Result<ash::Entry, GpuError> {
    if let Some(p) = std::env::var_os("ULTRAFAST_VULKAN_LIB") {
        return unsafe { ash::Entry::load_from(p) }
            .map_err(|e| GpuError::Execution(format!("cannot load Vulkan: {e}")));
    }
    let first = unsafe { ash::Entry::load() };
    if let Ok(e) = first {
        return Ok(e);
    }
    for p in [
        "/opt/homebrew/lib/libvulkan.dylib",
        "/usr/local/lib/libvulkan.dylib",
        "/opt/homebrew/lib/libvulkan.1.dylib",
    ] {
        if let Ok(e) = unsafe { ash::Entry::load_from(p) } {
            return Ok(e);
        }
    }
    Err(GpuError::Execution(format!(
        "cannot load Vulkan: {}",
        first.err().map(|e| e.to_string()).unwrap_or_default()
    )))
}

/// A graphics pipeline for 3D passes (see the Metal backend for the ABI).
#[derive(Clone)]
pub struct DrawPipeline {
    inner: Arc<KernelInner>,
    pub samples: u32,
}

pub struct DrawTarget<'a> {
    pub resolve: &'a Texture,
    pub color: Option<&'a Texture>,
    pub depth: Option<&'a Texture>,
    pub clear: [f32; 4],
}

pub struct Draw<'a> {
    pub pipeline: &'a DrawPipeline,
    pub params: &'a [f32],
    pub vertices: BufferBinding<'a>,
    pub indices: &'a Buffer,
    pub index_count: u32,
    pub texture: &'a Texture,
    /// Bound at binding 4 (shadow maps); pass any texture when unused.
    pub texture2: &'a Texture,
    /// Bound at binding 5 (environment maps); pass any texture when unused.
    pub texture3: &'a Texture,
    /// Bound at binding 6 (prefiltered environment); any texture when unused.
    pub texture4: &'a Texture,
}

fn samples_flag(n: u32) -> vk::SampleCountFlags {
    match n {
        2 => vk::SampleCountFlags::TYPE_2,
        4 => vk::SampleCountFlags::TYPE_4,
        8 => vk::SampleCountFlags::TYPE_8,
        _ => vk::SampleCountFlags::TYPE_1,
    }
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
        let inner = &self.inner;
        let vname = CString::new(vertex).unwrap();
        let fname = CString::new(fragment).unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(library.inner.module)
                .name(&vname),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(library.inner.module)
                .name(&fname),
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default();
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(samples_flag(samples));
        let ds = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(depth)
            .depth_write_enable(depth)
            .depth_compare_op(vk::CompareOp::LESS);
        let blend_kind = blend;
        // Premultiplied blending: src + dst * (1 - src.a), or additive.
        let dst = if blend == crate::Blend::Add {
            vk::BlendFactor::ONE
        } else {
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        };
        let blend = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(blend != crate::Blend::Opaque)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(dst)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            // Additive light adds color but composites alpha as over.
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let formats = [vk_format(color)];
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats)
            .depth_attachment_format(if depth {
                vk::Format::D32_SFLOAT
            } else {
                vk::Format::UNDEFINED
            });
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .depth_stencil_state(&ds)
            .color_blend_state(&cb)
            .dynamic_state(&dynamic)
            .layout(inner.draw_layout)
            .push_next(&mut rendering)];
        let key = hash128(&[
            &library.inner.hash.to_le_bytes(),
            vertex.as_bytes(),
            fragment.as_bytes(),
            &[samples as u8, depth as u8, blend_kind as u8, color as u8],
        ]);
        let pipeline = cached_pipeline(inner, key, |cache| {
            unsafe { inner.device.create_graphics_pipelines(cache, &info, None) }
                .map(|p| p[0])
                .map_err(|(_, e)| e)
        })
        .map_err(|e| GpuError::Pipeline(vertex.to_string(), e.to_string()))?;
        Ok(DrawPipeline {
            inner: Arc::new(KernelInner {
                pipeline,
                local: [1, 1, 1],
                name: vertex.to_string(),
                device: inner.clone(),
            }),
            samples,
        })
    }

    /// A render attachment: multisampled color, or depth.
    pub fn create_attachment(&self, width: u32, height: u32, format: PixelFormat, samples: u32) -> Texture {
        let depth = format == PixelFormat::Depth32Float;
        let aspect = if depth {
            vk::ImageAspectFlags::DEPTH
        } else {
            vk::ImageAspectFlags::COLOR
        };
        let usage = if depth {
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
        } else {
            vk::ImageUsageFlags::COLOR_ATTACHMENT
        } | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
        let d = &self.inner.device;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(aspect)
            .level_count(1)
            .layer_count(1);
        unsafe {
            let image = d
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(vk_format(format))
                        .extent(vk::Extent3D {
                            width: width.max(1),
                            height: height.max(1),
                            depth: 1,
                        })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(samples_flag(samples))
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(usage)
                        .initial_layout(vk::ImageLayout::UNDEFINED),
                    None,
                )
                .expect("attachment");
            let req = d.get_image_memory_requirements(image);
            let alloc = self
                .inner
                .alloc("attachment", req, MemoryLocation::GpuOnly, Dedicated::Image(image))
                .expect("attachment memory");
            d.bind_image_memory(image, alloc.memory(), alloc.offset())
                .expect("bind");
            let view = d
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk_format(format))
                        .subresource_range(range),
                    None,
                )
                .expect("attachment view");
            self.inner
                .oneshot(|cb| {
                    let barrier = [vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(range)
                        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)];
                    d.cmd_pipeline_barrier(
                        cb,
                        vk::PipelineStageFlags::TOP_OF_PIPE,
                        vk::PipelineStageFlags::ALL_COMMANDS,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &barrier,
                    );
                })
                .expect("attachment layout");
            Counters::add(&self.inner.counters.textures_created, 1);
            let desc = TextureDesc {
                width,
                height,
                format,
                cpu_readable: false,
            };
            Texture {
                inner: Arc::new(TexInner {
                    image,
                    view,
                    alloc: Some(alloc),
                    desc,
                    device: self.inner.clone(),
                    swapchain: false,
                }),
            }
        }
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

fn buffer_image_region(w: u32, h: u32) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1),
        )
        .image_extent(vk::Extent3D {
            width: w,
            height: h,
            depth: 1,
        })
}

/// Workgroup sizes of every entry point in a SPIR-V module.
fn spirv_local_sizes(words: &[u32]) -> Vec<(String, [u32; 3])> {
    let mut names: Vec<(u32, String)> = Vec::new();
    let mut sizes: Vec<(u32, [u32; 3])> = Vec::new();
    let mut i = 5;
    while i < words.len() {
        let wc = (words[i] >> 16) as usize;
        let op = words[i] & 0xffff;
        if wc == 0 || i + wc > words.len() {
            break;
        }
        let ins = &words[i..i + wc];
        match op {
            // OpEntryPoint: model, id, name...
            15 if wc > 3 => {
                let bytes: Vec<u8> = ins[3..].iter().flat_map(|w| w.to_le_bytes()).collect();
                let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                names.push((ins[2], String::from_utf8_lossy(&bytes[..end]).into_owned()));
            }
            // OpExecutionMode id LocalSize(17) x y z
            16 if wc >= 6 && ins[2] == 17 => sizes.push((ins[1], [ins[3], ins[4], ins[5]])),
            _ => {}
        }
        i += wc;
    }
    names
        .into_iter()
        .map(|(id, n)| {
            let s = sizes.iter().find(|s| s.0 == id).map(|s| s.1).unwrap_or([1, 1, 1]);
            (n, s)
        })
        .collect()
}

impl Library {
    pub fn kernel(&self, name: &str) -> Result<Kernel, GpuError> {
        // Slang names a module's only entry point `main` in SPIR-V.
        let sizes = &self.inner.local_sizes;
        let (spv_name, local) = sizes
            .iter()
            .find(|(n, _)| n == name)
            .or_else(|| (sizes.len() == 1).then(|| &sizes[0]))
            .map(|(n, s)| (n.clone(), *s))
            .ok_or_else(|| GpuError::NoSuchKernel(name.to_string()))?;
        let cname = CString::new(spv_name).unwrap();
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(self.inner.module)
            .name(&cname);
        let info = [vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(self.inner.device.pipeline_layout)];
        let dev = &self.inner.device;
        let key = hash128(&[&self.inner.hash.to_le_bytes(), cname.as_bytes()]);
        let pipeline = cached_pipeline(dev, key, |cache| {
            unsafe { dev.device.create_compute_pipelines(cache, &info, None) }
                .map(|p| p[0])
                .map_err(|(_, e)| e)
        })
        .map_err(|e| GpuError::Pipeline(name.to_string(), e.to_string()))?;
        Ok(Kernel {
            inner: Arc::new(KernelInner {
                pipeline,
                local,
                name: name.to_string(),
                device: self.inner.device.clone(),
            }),
        })
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
    pub buffers: &'a [BufferBinding<'a>],
    /// Overrides the dispatch grid in threads (default: output texture size).
    pub grid: Option<(u32, u32)>,
    /// Ignored on Vulkan: the workgroup size is fixed by the shader.
    pub threadgroup: Option<(u32, u32)>,
}

/// Records dispatches for one frame into one command buffer.
pub struct CommandList {
    device: Device,
    slot: usize,
    cb: vk::CommandBuffer,
    arena_ptr: *mut u8,
    arena_buf: vk::Buffer,
    arena_used: usize,
    /// Where per-frame data goes once the arena is full (a frame drawing
    /// a lot of geometry): a buffer of its own, kept until the frame is done.
    overflow: Option<(Arc<BufInner>, usize)>,
    /// Resources written and read since the last barrier (raw handles).
    written: std::collections::HashSet<u64>,
    read: std::collections::HashSet<u64>,
    queries: vk::QueryPool,
    submitted: bool,
    retained: Vec<Retained>,
    /// Window images to show once this list is submitted.
    #[cfg(windows)]
    presents: Vec<surface::PresentReq>,
    /// Window images already made writable in this list.
    #[cfg(windows)]
    ready: std::collections::HashSet<u64>,
}

impl CommandList {
    /// Puts a barrier before the next command if it reads or overwrites
    /// something written since the last barrier, or writes something read
    /// since then; then records its accesses.
    fn hazard(&mut self, reads: &[u64], writes: &[u64]) {
        let conflict = reads.iter().chain(writes).any(|r| self.written.contains(r))
            || writes.iter().any(|w| self.read.contains(w));
        if conflict {
            self.device.inner.full_barrier(self.cb);
            self.written.clear();
            self.read.clear();
            Counters::add(&self.device.inner.counters.barriers, 1);
        }
        self.read.extend(reads);
        self.written.extend(writes);
    }

    /// Records setup and uploads of textures created since the last
    /// command (they precede their first use).
    fn flush_pending(&mut self) {
        let keep = self.device.lane.flush(self.cb);
        if !keep.is_empty() {
            self.retained.extend(keep);
            self.written.clear();
            self.read.clear();
        }
    }

    /// Everything after this waits for everything before it.
    fn barrier_all(&mut self) {
        self.device.inner.full_barrier(self.cb);
        self.written.clear();
        self.read.clear();
        Counters::add(&self.device.inner.counters.barriers, 1);
    }

    /// Copies per-frame data (params, dynamic geometry) where kernels can
    /// read it: the frame's arena, or an overflow buffer when it is full.
    /// Returns (buffer, offset, length).
    fn arena_write(&mut self, data: &[u8]) -> (vk::Buffer, u64, u64) {
        let len = data.len().max(16);
        let offset = self.arena_used;
        if offset + len <= ARENA_BYTES {
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.arena_ptr.add(offset), data.len()) };
            self.arena_used = (offset + len).next_multiple_of(ARENA_ALIGN);
            return (self.arena_buf, offset as u64, len as u64);
        }
        let fits = self.overflow.as_ref().is_some_and(|(b, used)| used + len <= b.raw.len);
        if !fits {
            let inner = self.device.inner.clone();
            let raw = inner
                .host_buffer(len.max(ARENA_BYTES), vk::BufferUsageFlags::STORAGE_BUFFER)
                .expect("overflow arena");
            let buf = Arc::new(BufInner { raw, device: inner });
            self.retained.push(Retained::Buffer(buf.clone()));
            self.overflow = Some((buf, 0));
        }
        let (buf, used) = self.overflow.as_mut().unwrap();
        let offset = *used;
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf.raw.ptr.add(offset), data.len()) };
        *used = (offset + len).next_multiple_of(ARENA_ALIGN);
        (buf.raw.buffer, offset as u64, len as u64)
    }

    pub fn dispatch(&mut self, kernel: &Kernel, out: &Texture, inputs: &[&Texture], params: &[f32]) {
        self.dispatch_ex(kernel, out, inputs, params, Bindings::default());
    }

    pub fn dispatch_ex(&mut self, kernel: &Kernel, out: &Texture, inputs: &[&Texture], params: &[f32], b: Bindings) {
        self.flush_pending();
        #[cfg(windows)]
        self.prepare_drawable(out);
        let inner = self.device.inner.clone();
        let bytes = unsafe { std::slice::from_raw_parts(params.as_ptr().cast::<u8>(), std::mem::size_of_val(params)) };
        let (p_buf, p_off, p_len) = self.arena_write(bytes);
        Counters::add(&inner.counters.uniform_bytes, bytes.len() as u64);

        let out_info = [vk::DescriptorImageInfo::default()
            .image_view(out.inner.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let in_infos: Vec<[vk::DescriptorImageInfo; 1]> = inputs
            .iter()
            .map(|t| {
                [vk::DescriptorImageInfo::default()
                    .image_view(t.inner.view)
                    .image_layout(vk::ImageLayout::GENERAL)]
            })
            .collect();
        let sampler_info = [vk::DescriptorImageInfo::default().sampler(inner.sampler)];
        let param_info = [vk::DescriptorBufferInfo::default()
            .buffer(p_buf)
            .offset(p_off)
            .range(p_len)];
        let mut extra: Vec<[vk::DescriptorBufferInfo; 1]> = Vec::new();
        for binding in b.buffers {
            extra.push(match binding {
                BufferBinding::Buffer(buf) => [vk::DescriptorBufferInfo::default()
                    .buffer(buf.inner.raw.buffer)
                    .offset(0)
                    .range(vk::WHOLE_SIZE)],
                BufferBinding::Data(data) => {
                    let (b, o, l) = self.arena_write(data);
                    Counters::add(&inner.counters.uploads, 1);
                    Counters::add(&inner.counters.upload_bytes, data.len() as u64);
                    [vk::DescriptorBufferInfo::default().buffer(b).offset(o).range(l)]
                }
            });
        }
        let mut writes = vec![
            vk::WriteDescriptorSet::default()
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&out_info),
        ];
        for (i, info) in in_infos.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_binding(1 + i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_binding(BINDING_SAMPLER)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&sampler_info),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_binding(BINDING_PARAMS)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&param_info),
        );
        for (i, info) in extra.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_binding(BINDING_PARAMS + 1 + i as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(info),
            );
        }
        let (gw, gh) = b.grid.unwrap_or((out.inner.desc.width, out.inner.desc.height));
        let l = kernel.inner.local;
        self.retained.push(Retained::Kernel(kernel.inner.clone()));
        self.retained.push(Retained::Texture(out.inner.clone()));
        for t in inputs {
            self.retained.push(Retained::Texture(t.inner.clone()));
        }
        for binding in b.buffers {
            if let BufferBinding::Buffer(buf) = binding {
                self.retained.push(Retained::Buffer(buf.inner.clone()));
            }
        }
        unsafe {
            let d = &inner.device;
            d.cmd_bind_pipeline(self.cb, vk::PipelineBindPoint::COMPUTE, kernel.inner.pipeline);
            inner.push.cmd_push_descriptor_set(
                self.cb,
                vk::PipelineBindPoint::COMPUTE,
                inner.pipeline_layout,
                0,
                &writes,
            );
        }
        // Storage buffers may be read and written (accumulators).
        let mut reads: Vec<u64> = inputs.iter().map(|t| t.inner.image.as_raw()).collect();
        let mut writes = vec![out.inner.image.as_raw()];
        for binding in b.buffers {
            if let BufferBinding::Buffer(buf) = binding {
                reads.push(buf.inner.raw.buffer.as_raw());
                writes.push(buf.inner.raw.buffer.as_raw());
            }
        }
        self.hazard(&reads, &writes);
        unsafe {
            inner
                .device
                .cmd_dispatch(self.cb, gw.max(1).div_ceil(l[0]), gh.max(1).div_ceil(l[1]), 1);
        }
        Counters::add(&inner.counters.dispatches, 1);
    }

    /// Runs a 3D pass: clears, draws, and resolves into `target.resolve`.
    pub fn draw_pass(&mut self, target: DrawTarget, draws: &[Draw]) {
        self.flush_pending();
        self.barrier_all();
        let inner = self.device.inner.clone();
        let d = &inner.device;
        let (w, h) = (target.resolve.inner.desc.width, target.resolve.inner.desc.height);
        let [r, g, b, a] = target.clear;
        let clear = vk::ClearValue {
            color: vk::ClearColorValue { float32: [r, g, b, a] },
        };
        let mut color = vk::RenderingAttachmentInfo::default()
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(clear);
        match target.color {
            Some(msaa) => {
                color = color
                    .image_view(msaa.inner.view)
                    .resolve_mode(vk::ResolveModeFlags::AVERAGE)
                    .resolve_image_view(target.resolve.inner.view)
                    .resolve_image_layout(vk::ImageLayout::GENERAL);
                self.retained.push(Retained::Texture(msaa.inner.clone()));
            }
            None => color = color.image_view(target.resolve.inner.view),
        }
        let depth_view = target.depth.map(|d| d.inner.view).unwrap_or(vk::ImageView::null());
        let depth = vk::RenderingAttachmentInfo::default()
            .image_view(depth_view)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .clear_value(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 },
            });
        self.retained.push(Retained::Texture(target.resolve.inner.clone()));
        if let Some(d) = target.depth {
            self.retained.push(Retained::Texture(d.inner.clone()));
        }
        let colors = [color];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D { width: w, height: h },
        };
        let mut info = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&colors);
        if target.depth.is_some() {
            info = info.depth_attachment(&depth);
        }
        unsafe {
            d.cmd_begin_rendering(self.cb, &info);
            // Negative height flips y so clip space matches Metal (y up).
            let vp = vk::Viewport {
                x: 0.0,
                y: h as f32,
                width: w as f32,
                height: -(h as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            };
            d.cmd_set_viewport(self.cb, 0, &[vp]);
            d.cmd_set_scissor(self.cb, 0, &[area]);
        }
        for draw in draws {
            let bytes = unsafe {
                std::slice::from_raw_parts(draw.params.as_ptr().cast::<u8>(), std::mem::size_of_val(draw.params))
            };
            let (p_buf, off, len) = self.arena_write(bytes);
            Counters::add(&inner.counters.uniform_bytes, bytes.len() as u64);
            let p = [vk::DescriptorBufferInfo::default().buffer(p_buf).offset(off).range(len)];
            // Vertices: a persistent buffer, or per-frame data in the arena.
            let v = [match &draw.vertices {
                BufferBinding::Buffer(b) => {
                    self.retained.push(Retained::Buffer(b.inner.clone()));
                    vk::DescriptorBufferInfo::default()
                        .buffer(b.inner.raw.buffer)
                        .offset(0)
                        .range(vk::WHOLE_SIZE)
                }
                BufferBinding::Data(data) => {
                    let (b, o, l) = self.arena_write(data);
                    Counters::add(&inner.counters.uploads, 1);
                    Counters::add(&inner.counters.upload_bytes, data.len() as u64);
                    vk::DescriptorBufferInfo::default().buffer(b).offset(o).range(l)
                }
            }];
            let t = [vk::DescriptorImageInfo::default()
                .image_view(draw.texture.inner.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            let s = [vk::DescriptorImageInfo::default().sampler(inner.sampler)];
            let t2 = [vk::DescriptorImageInfo::default()
                .image_view(draw.texture2.inner.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            let t3 = [vk::DescriptorImageInfo::default()
                .image_view(draw.texture3.inner.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            let t4 = [vk::DescriptorImageInfo::default()
                .image_view(draw.texture4.inner.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&p),
                vk::WriteDescriptorSet::default()
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&v),
                vk::WriteDescriptorSet::default()
                    .dst_binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&t),
                vk::WriteDescriptorSet::default()
                    .dst_binding(3)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&s),
                vk::WriteDescriptorSet::default()
                    .dst_binding(4)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&t2),
                vk::WriteDescriptorSet::default()
                    .dst_binding(5)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&t3),
                vk::WriteDescriptorSet::default()
                    .dst_binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&t4),
            ];
            unsafe {
                d.cmd_bind_pipeline(self.cb, vk::PipelineBindPoint::GRAPHICS, draw.pipeline.inner.pipeline);
                inner.push.cmd_push_descriptor_set(
                    self.cb,
                    vk::PipelineBindPoint::GRAPHICS,
                    inner.draw_layout,
                    0,
                    &writes,
                );
                d.cmd_bind_index_buffer(self.cb, draw.indices.inner.raw.buffer, 0, vk::IndexType::UINT32);
                d.cmd_draw_indexed(self.cb, draw.index_count, 1, 0, 0, 0);
            }
            self.retained.push(Retained::Kernel(draw.pipeline.inner.clone()));
            self.retained.push(Retained::Buffer(draw.indices.inner.clone()));
            self.retained.push(Retained::Texture(draw.texture.inner.clone()));
            self.retained.push(Retained::Texture(draw.texture2.inner.clone()));
            self.retained.push(Retained::Texture(draw.texture3.inner.clone()));
            self.retained.push(Retained::Texture(draw.texture4.inner.clone()));
            Counters::add(&inner.counters.dispatches, 1);
        }
        unsafe { d.cmd_end_rendering(self.cb) };
        self.barrier_all();
    }

    pub fn clear_buffer_range(&mut self, buf: &Buffer, offset: usize, len: usize) {
        let len = len.min(buf.len().saturating_sub(offset)) & !3;
        if len == 0 {
            return;
        }
        self.flush_pending();
        self.retained.push(Retained::Buffer(buf.inner.clone()));
        self.hazard(&[], &[buf.inner.raw.buffer.as_raw()]);
        unsafe {
            self.device
                .inner
                .device
                .cmd_fill_buffer(self.cb, buf.inner.raw.buffer, offset as u64, len as u64, 0);
        }
    }

    pub fn clear_buffer(&mut self, buf: &Buffer) {
        self.clear_buffer_range(buf, 0, buf.len());
    }

    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture) {
        let d = src.inner.desc;
        let layers = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .layer_count(1);
        let region = [vk::ImageCopy::default()
            .src_subresource(layers)
            .dst_subresource(layers)
            .extent(vk::Extent3D {
                width: d.width,
                height: d.height,
                depth: 1,
            })];
        self.flush_pending();
        self.retained.push(Retained::Texture(src.inner.clone()));
        self.retained.push(Retained::Texture(dst.inner.clone()));
        self.hazard(&[src.inner.image.as_raw()], &[dst.inner.image.as_raw()]);
        unsafe {
            self.device.inner.device.cmd_copy_image(
                self.cb,
                src.inner.image,
                vk::ImageLayout::GENERAL,
                dst.inner.image,
                vk::ImageLayout::GENERAL,
                &region,
            );
        }
    }

    pub fn submit(mut self) -> Submitted {
        self.flush_pending();
        // What the CPU reads after the fence (readbacks, diff counts) is
        // complete and visible.
        self.barrier_all();
        let inner = self.device.inner.clone();
        let lane = self.device.lane.clone();
        let mut ring = lane.ring.lock().unwrap();
        let s = &mut ring.1[self.slot];
        unsafe {
            let d = &inner.device;
            d.cmd_write_timestamp(self.cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.queries, 1);
            d.end_command_buffer(self.cb).expect("end command buffer");
            d.reset_fences(&[s.fence]).expect("reset fence");
            let cbs = [self.cb];
            #[cfg(windows)]
            let signals = surface::signals(&self.presents);
            #[cfg(not(windows))]
            let signals: [vk::Semaphore; 0] = [];
            let submit = vk::SubmitInfo::default()
                .command_buffers(&cbs)
                .signal_semaphores(&signals);
            let q = inner.queues[lane.queue].lock().unwrap();
            d.queue_submit(*q, &[submit], s.fence).expect("queue submit");
            #[cfg(windows)]
            surface::present(&inner, *q, &self.presents);
        }
        s.pending = true;
        s.recording = false;
        s.retained = std::mem::take(&mut self.retained);
        self.submitted = true;
        Submitted {
            device: inner.clone(),
            lane: lane.clone(),
            slot: self.slot,
            generation: s.generation,
            fence: s.fence,
            queries: s.queries,
        }
    }
}

impl Drop for CommandList {
    /// A list abandoned mid-frame is ended and submitted empty-handed so its
    /// ring slot's fence is signaled again.
    fn drop(&mut self) {
        if self.submitted {
            return;
        }
        #[cfg(windows)]
        surface::abandon(&self.presents);
        let inner = self.device.inner.clone();
        let lane = self.device.lane.clone();
        let mut ring = lane.ring.lock().unwrap();
        let s = &mut ring.1[self.slot];
        s.recording = false;
        unsafe {
            let d = &inner.device;
            let _ = d.end_command_buffer(self.cb);
            let _ = d.reset_fences(&[s.fence]);
            let q = inner.queues[lane.queue].lock().unwrap();
            let _ = d.queue_submit(*q, &[vk::SubmitInfo::default()], s.fence);
        }
    }
}

pub struct Submitted {
    device: Arc<Inner>,
    lane: Arc<Lane>,
    slot: usize,
    generation: u64,
    fence: vk::Fence,
    queries: vk::QueryPool,
}

impl Submitted {
    fn timing(&self) -> GpuTiming {
        let ring = self.lane.ring.lock().unwrap();
        if ring.1[self.slot].generation != self.generation {
            return GpuTiming::default();
        }
        let mut ts = [0u64; 2];
        let ok = unsafe {
            self.device
                .device
                .get_query_pool_results(self.queries, 0, &mut ts, vk::QueryResultFlags::TYPE_64)
        };
        match ok {
            Ok(()) => GpuTiming {
                gpu_ms: ts[1].saturating_sub(ts[0]) as f64 * self.device.timestamp_period / 1e6,
            },
            Err(_) => GpuTiming::default(),
        }
    }

    pub fn try_timing(&self) -> Option<Result<GpuTiming, GpuError>> {
        match unsafe { self.device.device.get_fence_status(self.fence) } {
            Ok(true) => Some(Ok(self.timing())),
            Ok(false) => None,
            Err(e) => Some(Err(err(e))),
        }
    }

    pub fn wait(self) -> Result<GpuTiming, GpuError> {
        {
            let ring = self.lane.ring.lock().unwrap();
            if ring.1[self.slot].generation != self.generation {
                // The slot was reused, so this frame finished long ago.
                return Ok(GpuTiming::default());
            }
        }
        unsafe { self.device.device.wait_for_fences(&[self.fence], true, u64::MAX) }.map_err(err)?;
        Ok(self.timing())
    }
}

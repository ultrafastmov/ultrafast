//! Thin native GPU layer for ultrafast.
//!
//! This is deliberately not a general graphics abstraction. It exposes the
//! handful of concepts the renderer needs (textures, compute kernels, command
//! lists) with static dispatch to one native backend per platform, and it
//! counts every CPU<->GPU transfer so copies are always visible.
//!
//! Backends: Metal (macOS). Vulkan + CUDA interop (Linux, NVIDIA L4) is next.

// Engine code: wide kernel and pass signatures and indexed loops over
// parallel arrays are the clearest form here.
#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop)]

mod stats;
pub use stats::TransferStats;

#[cfg(backend_metal)]
pub mod metal;
#[cfg(backend_metal)]
mod surface;
pub mod vulkan;

#[cfg(backend_metal)]
pub use metal::{
    Bindings, Buffer, BufferBinding, CommandList, Device, Draw, DrawPipeline, DrawTarget, Kernel, Library, Submitted,
    Texture,
};
#[cfg(backend_metal)]
pub use surface::{Drawable, Surface};
#[cfg(backend_vulkan)]
pub use vulkan::ExportedMemory;
#[cfg(backend_vulkan)]
pub use vulkan::{
    Bindings, Buffer, BufferBinding, CommandList, Device, Draw, DrawPipeline, DrawTarget, Kernel, Library, Submitted,
    Texture,
};
#[cfg(all(backend_vulkan, windows))]
pub use vulkan::{Drawable, Surface};

/// How a draw pipeline combines fragments with the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    /// Replace (3D meshes with depth).
    Opaque,
    /// Premultiplied source-over.
    Over,
    /// Additive (sparks, light).
    Add,
}

/// The backend this build renders with: "metal" or "vulkan".
/// What to tell someone whose renders run on `device`, a driver that does
/// Vulkan's work on the CPU (llvmpipe, SwiftShader): the GPU isn't visible
/// to this process, and every frame takes many times longer.
pub fn cpu_device_warning(device: &str) -> String {
    format!(
        "rendering on {device}, a driver that runs on the CPU, so renders take many times longer: \
         no GPU is visible to this process (a sandbox, a container without the GPU, or no GPU driver)"
    )
}

#[cfg(backend_metal)]
pub const BACKEND: &str = "metal";
#[cfg(backend_vulkan)]
pub const BACKEND: &str = "vulkan";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Linear premultiplied working format.
    Rgba16Float,
    /// 8-bit export / image format.
    Rgba8Unorm,
    /// 8-bit BGRA, the native layout of video frames and display surfaces.
    Bgra8Unorm,
    /// Single channel float (coverage accumulators, depth-like data).
    R32Float,
    /// Single channel half (masks, coverage).
    R16Float,
    /// Full-precision RGBA (simulation state).
    Rgba32Float,
    /// Depth attachment for 3D passes.
    Depth32Float,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgba16Float => 8,
            PixelFormat::Rgba8Unorm | PixelFormat::Bgra8Unorm | PixelFormat::R32Float | PixelFormat::Depth32Float => 4,
            PixelFormat::R16Float => 2,
            PixelFormat::Rgba32Float => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Whether the CPU will read this texture back. Readable textures live in
    /// shared memory; everything else is GPU-private.
    pub cpu_readable: bool,
}

/// GPU execution time of one submitted command list, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuTiming {
    pub gpu_ms: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU device available")]
    NoDevice,
    #[error("shader compile failed: {0}")]
    ShaderCompile(String),
    #[error("kernel `{0}` not found")]
    NoSuchKernel(String),
    #[error("pipeline creation failed for `{0}`: {1}")]
    Pipeline(String, String),
    #[error("command buffer failed: {0}")]
    Execution(String),
}

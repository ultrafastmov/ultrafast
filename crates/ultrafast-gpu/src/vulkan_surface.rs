//! Display output on Windows: a Vulkan swapchain on a Win32 window.
//!
//! As with the CAMetalLayer on macOS, the `k_present` kernel writes straight
//! into the swapchain image (storage usage, no intermediate copy). Images are
//! acquired with a fence, so the CPU waits until the display has released
//! one; a frame then only signals a semaphore for the present after it.

use super::{CommandList, Device, GpuError, Inner, Retained, TexInner, Texture, color_range, err};
use crate::{PixelFormat, TextureDesc};
use ash::vk;
use ash::vk::Handle;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// How long to wait for the display to release an image (1 s).
const ACQUIRE_TIMEOUT_NS: u64 = 1_000_000_000;

pub struct Surface {
    shared: Arc<Shared>,
}

/// One swapchain image acquired for a frame. Dropping it without presenting
/// rebuilds the swapchain before the next frame (the image stays acquired).
pub struct Drawable {
    pub texture: Texture,
    index: u32,
    shared: Arc<Shared>,
    presented: AtomicBool,
}

struct Shared {
    device: Device,
    surface_fn: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    state: Mutex<Swap>,
    /// Rebuild the swapchain before the next frame: resized, vsync changed,
    /// out of date, or an acquired image was never presented.
    stale: AtomicBool,
}

struct Swap {
    swapchain: vk::SwapchainKHR,
    images: Vec<Texture>,
    /// Per image: signaled by the frame that wrote it, waited by its present.
    rendered: Vec<vk::Semaphore>,
    acquired: vk::Fence,
    size: (u32, u32),
    vsync: bool,
}

/// A present waiting for its command list to be submitted.
pub(super) struct PresentReq {
    shared: Arc<Shared>,
    swapchain: vk::SwapchainKHR,
    index: u32,
    rendered: vk::Semaphore,
}

impl Surface {
    /// A swapchain on a Win32 window.
    ///
    /// # Safety
    /// `hinstance` and `hwnd` must belong to a live window that outlives the
    /// surface.
    pub unsafe fn from_win32(device: &Device, hinstance: *mut c_void, hwnd: *mut c_void) -> Result<Self, GpuError> {
        let inner = &device.inner;
        if !inner.surface_ext || inner.swapchain.is_none() {
            return Err(GpuError::Execution(
                "this Vulkan driver cannot present to windows".into(),
            ));
        }
        let win32 = ash::khr::win32_surface::Instance::new(&inner._entry, &inner.instance);
        let info = vk::Win32SurfaceCreateInfoKHR::default()
            .hinstance(hinstance as vk::HINSTANCE)
            .hwnd(hwnd as vk::HWND);
        let surface = unsafe { win32.create_win32_surface(&info, None) }.map_err(err)?;
        let surface_fn = ash::khr::surface::Instance::new(&inner._entry, &inner.instance);
        let supported =
            unsafe { surface_fn.get_physical_device_surface_support(inner.pdev, inner.queue_family, surface) }
                .unwrap_or(false);
        if !supported {
            unsafe { surface_fn.destroy_surface(surface, None) };
            return Err(GpuError::Execution(
                "the GPU's queue cannot present to this window".into(),
            ));
        }
        let acquired = unsafe { inner.device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(err)?;
        Ok(Self {
            shared: Arc::new(Shared {
                device: device.clone(),
                surface_fn,
                surface,
                state: Mutex::new(Swap {
                    swapchain: vk::SwapchainKHR::null(),
                    images: Vec::new(),
                    rendered: Vec::new(),
                    acquired,
                    size: (1, 1),
                    vsync: true,
                }),
                stale: AtomicBool::new(true),
            }),
        })
    }

    /// Sets the image size in physical pixels.
    pub fn resize(&self, width: u32, height: u32) {
        let mut s = self.shared.state.lock().unwrap();
        let size = (width.max(1), height.max(1));
        if s.size != size {
            s.size = size;
            self.shared.stale.store(true, Ordering::Relaxed);
        }
    }

    pub fn set_vsync(&self, on: bool) {
        let mut s = self.shared.state.lock().unwrap();
        if s.vsync != on {
            s.vsync = on;
            self.shared.stale.store(true, Ordering::Relaxed);
        }
    }

    /// Next image, or None if the display is not ready (skip the frame).
    pub fn next(&self) -> Option<Drawable> {
        let sh = &self.shared;
        let inner = &sh.device.inner;
        let chain_fn = inner.swapchain.as_ref()?;
        let mut s = sh.state.lock().unwrap();
        if sh.stale.swap(false, Ordering::Relaxed) || s.swapchain == vk::SwapchainKHR::null() {
            if let Err(e) = unsafe { sh.rebuild(&mut s) } {
                // A minimized window has no size: try again next frame.
                if !matches!(&e, GpuError::Execution(m) if m.contains("no size")) {
                    eprintln!("ultrafast: swapchain: {e}");
                }
                sh.stale.store(true, Ordering::Relaxed);
                return None;
            }
        }
        let d = &inner.device;
        unsafe { d.reset_fences(&[s.acquired]) }.ok()?;
        let acquired =
            unsafe { chain_fn.acquire_next_image(s.swapchain, ACQUIRE_TIMEOUT_NS, vk::Semaphore::null(), s.acquired) };
        let index = match acquired {
            Ok((i, suboptimal)) => {
                if suboptimal {
                    sh.stale.store(true, Ordering::Relaxed);
                }
                i
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                sh.stale.store(true, Ordering::Relaxed);
                return None;
            }
            Err(_) => return None,
        };
        if unsafe { d.wait_for_fences(&[s.acquired], true, ACQUIRE_TIMEOUT_NS) }.is_err() {
            // Acquired but not released in time: start over with a new chain.
            sh.stale.store(true, Ordering::Relaxed);
            return None;
        }
        Some(Drawable {
            texture: s.images[index as usize].clone(),
            index,
            shared: sh.clone(),
            presented: AtomicBool::new(false),
        })
    }
}

impl Shared {
    /// (Re)creates the swapchain at the current size and present mode.
    unsafe fn rebuild(&self, s: &mut Swap) -> Result<(), GpuError> {
        let inner = &self.device.inner;
        let d = &inner.device;
        let chain_fn = inner.swapchain.as_ref().expect("swapchain extension");
        let caps = unsafe {
            self.surface_fn
                .get_physical_device_surface_capabilities(inner.pdev, self.surface)
        }
        .map_err(err)?;
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: s.size.0.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: s
                    .size
                    .1
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        if extent.width == 0 || extent.height == 0 {
            return Err(GpuError::Execution("window has no size".into()));
        }
        if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::STORAGE) {
            return Err(GpuError::Execution(
                "window images cannot be written by compute kernels".into(),
            ));
        }
        let formats = unsafe {
            self.surface_fn
                .get_physical_device_surface_formats(inner.pdev, self.surface)
        }
        .map_err(err)?;
        let (format, pixel) = [
            (vk::Format::B8G8R8A8_UNORM, PixelFormat::Bgra8Unorm),
            (vk::Format::R8G8B8A8_UNORM, PixelFormat::Rgba8Unorm),
        ]
        .into_iter()
        .find(|(f, _)| {
            let storage = unsafe { inner.instance.get_physical_device_format_properties(inner.pdev, *f) }
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::STORAGE_IMAGE);
            storage
                && formats
                    .iter()
                    .any(|sf| sf.format == *f && sf.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        })
        .ok_or_else(|| GpuError::Execution("no 8-bit sRGB window format the kernels can write".into()))?;
        let modes = unsafe {
            self.surface_fn
                .get_physical_device_surface_present_modes(inner.pdev, self.surface)
        }
        .map_err(err)?;
        let mode = if s.vsync {
            vk::PresentModeKHR::FIFO
        } else {
            [vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::IMMEDIATE]
                .into_iter()
                .find(|m| modes.contains(m))
                .unwrap_or(vk::PresentModeKHR::FIFO)
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|a| caps.supported_composite_alpha.contains(*a))
        .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);

        // Nothing in flight may still use the old images: the window's frames
        // all go through this device's queue.
        {
            let q = inner.queues[self.device.lane.queue].lock().unwrap();
            unsafe { d.queue_wait_idle(*q) }.map_err(err)?;
        }
        let old = s.swapchain;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(format)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(alpha)
            .present_mode(mode)
            .clipped(true)
            .old_swapchain(old);
        let swapchain = unsafe { chain_fn.create_swapchain(&info, None) }.map_err(err)?;
        s.images.clear();
        for sem in s.rendered.drain(..) {
            unsafe { d.destroy_semaphore(sem, None) };
        }
        if old != vk::SwapchainKHR::null() {
            unsafe { chain_fn.destroy_swapchain(old, None) };
        }
        s.swapchain = swapchain;
        let images = unsafe { chain_fn.get_swapchain_images(swapchain) }.map_err(err)?;
        for image in images {
            let view = unsafe {
                d.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(color_range()),
                    None,
                )
            }
            .map_err(err)?;
            s.images.push(Texture {
                inner: Arc::new(TexInner {
                    image,
                    view,
                    alloc: None,
                    desc: TextureDesc {
                        width: extent.width,
                        height: extent.height,
                        format: pixel,
                        cpu_readable: false,
                    },
                    device: inner.clone(),
                    swapchain: true,
                }),
            });
            s.rendered
                .push(unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(err)?);
        }
        Ok(())
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let inner = &self.device.inner;
        let d = &inner.device;
        let s = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        unsafe {
            if let Ok(q) = inner.queues[self.device.lane.queue].lock() {
                let _ = d.queue_wait_idle(*q);
            }
            s.images.clear();
            for sem in s.rendered.drain(..) {
                d.destroy_semaphore(sem, None);
            }
            if let Some(chain_fn) = inner.swapchain.as_ref()
                && s.swapchain != vk::SwapchainKHR::null()
            {
                chain_fn.destroy_swapchain(s.swapchain, None);
            }
            d.destroy_fence(s.acquired, None);
            self.surface_fn.destroy_surface(self.surface, None);
        }
    }
}

impl Drop for Drawable {
    fn drop(&mut self) {
        if !self.presented.load(Ordering::Relaxed) {
            self.shared.stale.store(true, Ordering::Relaxed);
        }
    }
}

impl CommandList {
    /// Before the first write to a window image in this list: its old
    /// contents (shown last time) are not needed.
    pub(super) fn prepare_drawable(&mut self, out: &Texture) {
        if !out.inner.swapchain || !self.ready.insert(out.inner.image.as_raw()) {
            return;
        }
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(out.inner.image)
            .subresource_range(color_range())
            .dst_access_mask(vk::AccessFlags::SHADER_WRITE | vk::AccessFlags::SHADER_READ);
        unsafe {
            self.device.inner.device.cmd_pipeline_barrier(
                self.cb,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }
    }

    /// Shows `drawable` on its window once this list's work is done.
    pub fn present(&mut self, drawable: &Drawable) {
        self.prepare_drawable(&drawable.texture);
        let s = drawable.shared.state.lock().unwrap();
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(drawable.texture.inner.image)
            .subresource_range(color_range())
            .src_access_mask(vk::AccessFlags::SHADER_WRITE | vk::AccessFlags::MEMORY_WRITE);
        unsafe {
            self.device.inner.device.cmd_pipeline_barrier(
                self.cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }
        self.presents.push(PresentReq {
            shared: drawable.shared.clone(),
            swapchain: s.swapchain,
            index: drawable.index,
            rendered: s.rendered[drawable.index as usize],
        });
        self.retained.push(Retained::Texture(drawable.texture.inner.clone()));
        drawable.presented.store(true, Ordering::Relaxed);
    }
}

/// The semaphores a submission must signal for its presents.
pub(super) fn signals(reqs: &[PresentReq]) -> Vec<vk::Semaphore> {
    reqs.iter().map(|r| r.rendered).collect()
}

/// Queues the presents of a just-submitted list (on the same queue).
pub(super) fn present(inner: &Inner, queue: vk::Queue, reqs: &[PresentReq]) {
    let Some(chain_fn) = inner.swapchain.as_ref() else {
        return;
    };
    for r in reqs {
        let waits = [r.rendered];
        let chains = [r.swapchain];
        let indices = [r.index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&chains)
            .image_indices(&indices);
        match unsafe { chain_fn.queue_present(queue, &info) } {
            Ok(false) => {}
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => r.shared.stale.store(true, Ordering::Relaxed),
            Err(e) => {
                eprintln!("ultrafast: present failed: {e}");
                r.shared.stale.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// A list dropped before submitting never presents its images: rebuild.
pub(super) fn abandon(reqs: &[PresentReq]) {
    for r in reqs {
        r.shared.stale.store(true, Ordering::Relaxed);
    }
}

//! Display output on macOS: a CAMetalLayer attached to an NSView.
//!
//! Frames are written straight into the layer's drawable by a compute
//! kernel (no intermediate copy). The layer is tagged sRGB so the system
//! color-manages it onto wide-gamut displays correctly.

use crate::PixelFormat;
use crate::metal::Device;
use crate::metal::Texture;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_metal::{MTLDrawable, MTLPixelFormat};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::ffi::c_void;

pub struct Surface {
    layer: Retained<CAMetalLayer>,
    device: Device,
}

/// One drawable acquired from the layer, valid for a single frame.
pub struct Drawable {
    raw: Retained<ProtocolObject<dyn CAMetalDrawable>>,
    pub texture: Texture,
}

impl Surface {
    /// Attaches a Metal layer to `ns_view` (an `NSView*`).
    ///
    /// # Safety
    /// `ns_view` must be a valid NSView pointer that outlives the surface,
    /// and this must be called on the main thread.
    pub unsafe fn from_ns_view(device: &Device, ns_view: *mut c_void) -> Self {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device.raw_device()));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // Compute kernels write the drawable directly.
        layer.setFramebufferOnly(false);
        if let Some(cs) = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })) {
            layer.setColorspace(Some(&cs));
        }
        let view = ns_view as *mut AnyObject;
        unsafe {
            let _: () = msg_send![view, setWantsLayer: true];
            let _: () = msg_send![view, setLayer: &*layer];
        }
        Self {
            layer,
            device: device.clone(),
        }
    }

    /// Sets the drawable size in physical pixels.
    pub fn resize(&self, width: u32, height: u32) {
        self.layer.setDrawableSize(objc2_core_foundation::CGSize {
            width: width.max(1) as f64,
            height: height.max(1) as f64,
        });
    }

    /// Next drawable, or None if the display is not ready (skip the frame).
    pub fn next(&self) -> Option<Drawable> {
        let raw = self.layer.nextDrawable()?;
        let tex = raw.texture();
        let texture = self.device.wrap_texture(tex, PixelFormat::Bgra8Unorm);
        Some(Drawable { raw, texture })
    }

    pub fn set_vsync(&self, on: bool) {
        self.layer.setDisplaySyncEnabled(on);
    }
}

impl Drawable {
    pub(crate) fn as_mtl(&self) -> &ProtocolObject<dyn MTLDrawable> {
        ProtocolObject::from_ref(&*self.raw)
    }
}

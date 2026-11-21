//! Frame analysis with Apple Vision (macOS): text recognition for seeding
//! text layers, and subject masks for separating footage into layers.

use crate::MediaError;
use crate::macos::{Reader, SendBox};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSRange};
use objc2_vision::{
    VNGenerateForegroundInstanceMaskRequest, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};
use std::path::Path;

/// A recognized line of text, in pixels with the origin at the top left.
#[derive(Debug, Clone)]
pub struct TextBox {
    pub text: String,
    pub confidence: f32,
    pub rect: [f64; 4],
    /// Its words (runs between spaces) and their boxes, in order.
    pub words: Vec<(String, [f64; 4])>,
}

/// Decodes the frame showing at `t`.
fn frame_at(path: &Path, t: f64) -> Result<(CFRetained<CVPixelBuffer>, u32, u32), MediaError> {
    let mut r = Reader::open(path)?;
    r.seek(t)?;
    let d = r.next()?.ok_or_else(|| MediaError::Open(format!("no frame at {t}s")))?;
    let pb = d.buffer.0;
    let (w, h) = (CVPixelBufferGetWidth(&pb) as u32, CVPixelBufferGetHeight(&pb) as u32);
    Ok((pb, w, h))
}

fn handler(pb: &CVPixelBuffer) -> Retained<VNImageRequestHandler> {
    unsafe {
        VNImageRequestHandler::initWithCVPixelBuffer_options(VNImageRequestHandler::alloc(), pb, &NSDictionary::new())
    }
}

/// Recognizes text lines in the frame of `path` at `t` seconds.
pub fn recognize_text(path: &Path, t: f64) -> Result<Vec<TextBox>, MediaError> {
    let (pb, w, h) = frame_at(path, t)?;
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    let handler = handler(&pb);
    let as_req: Retained<VNRequest> = Retained::into_super(Retained::into_super(request.clone()));
    let requests = NSArray::from_retained_slice(&[as_req]);
    handler
        .performRequests_error(&requests)
        .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?;
    let mut out = Vec::new();
    if let Some(results) = request.results() {
        for obs in results.iter() {
            let cands = obs.topCandidates(1);
            let Some(best) = cands.firstObject() else { continue };
            let b = unsafe { obs.boundingBox() };
            // Vision: normalized, origin bottom-left.
            let rect = [
                b.origin.x * w as f64,
                (1.0 - b.origin.y - b.size.height) * h as f64,
                b.size.width * w as f64,
                b.size.height * h as f64,
            ];
            let text = best.string().to_string();
            // Word boxes: Vision ranges count UTF-16 units.
            let mut words = Vec::new();
            let mut at = 0usize;
            for word in text.split(' ') {
                let len = word.encode_utf16().count();
                if len > 0
                    && let Ok(o) = unsafe { best.boundingBoxForRange_error(NSRange::new(at, len)) }
                {
                    let b = unsafe { o.boundingBox() };
                    words.push((
                        word.to_string(),
                        [
                            b.origin.x * w as f64,
                            (1.0 - b.origin.y - b.size.height) * h as f64,
                            b.size.width * w as f64,
                            b.size.height * h as f64,
                        ],
                    ));
                }
                at += len + 1;
            }
            out.push(TextBox {
                text,
                confidence: best.confidence(),
                rect,
                words,
            });
        }
    }
    out.sort_by(|a, b| {
        a.rect[1]
            .partial_cmp(&b.rect[1])
            .unwrap()
            .then(a.rect[0].partial_cmp(&b.rect[0]).unwrap())
    });
    Ok(out)
}

/// Foreground subject mask (all instances) of the frame at `t`, as 8-bit
/// coverage at the frame's size. None if Vision finds no subject.
pub fn subject_mask(path: &Path, t: f64) -> Result<Option<(u32, u32, Vec<u8>)>, MediaError> {
    let (pb, w, h) = frame_at(path, t)?;
    subject_mask_of(&pb, w, h)
}

pub(crate) fn subject_mask_of(pb: &CVPixelBuffer, w: u32, h: u32) -> Result<Option<(u32, u32, Vec<u8>)>, MediaError> {
    let request = unsafe { VNGenerateForegroundInstanceMaskRequest::new() };
    let handler = handler(pb);
    let as_req: Retained<VNRequest> = Retained::into_super(Retained::into_super(request.clone()));
    let requests = NSArray::from_retained_slice(&[as_req]);
    handler
        .performRequests_error(&requests)
        .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?;
    let Some(results) = (unsafe { request.results() }) else {
        return Ok(None);
    };
    let Some(obs) = results.firstObject() else {
        return Ok(None);
    };
    let mask = unsafe {
        obs.generateScaledMaskForImageForInstances_fromRequestHandler_error(&obs.allInstances(), &handler)
            .map_err(|e| MediaError::Platform(e.localizedDescription().to_string()))?
    };
    // The mask is one float per pixel at the image size.
    let mask = SendBox(mask);
    let m: &CVPixelBuffer = &mask.0;
    let (mw, mh) = (CVPixelBufferGetWidth(m), CVPixelBufferGetHeight(m));
    let mut out = vec![0u8; mw * mh];
    unsafe {
        CVPixelBufferLockBaseAddress(m, CVPixelBufferLockFlags::ReadOnly);
        let base = CVPixelBufferGetBaseAddress(m) as *const u8;
        let stride = CVPixelBufferGetBytesPerRow(m);
        for y in 0..mh {
            let row = base.add(y * stride) as *const f32;
            for x in 0..mw {
                out[y * mw + x] = ((*row.add(x)).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        CVPixelBufferUnlockBaseAddress(m, CVPixelBufferLockFlags::ReadOnly);
    }
    let _ = (w, h);
    Ok(Some((mw as u32, mh as u32, out)))
}

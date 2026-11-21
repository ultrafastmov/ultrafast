//! Color glyph bitmaps (emoji) on the GPU: one atlas texture shared by
//! every text node, filled on first use (a small upload and a copy kernel
//! per new glyph) and started over when full. Shelf packed: emoji strikes
//! are nearly all the same size.

use crate::geom::GlyphBitmap;
use std::collections::HashMap;
use ultrafast_gpu::{CommandList, Device, Kernel, PixelFormat, Texture, TextureDesc};

/// Atlas side in texels: about 240 emoji at Noto's 136x128 strike.
const SIZE: u32 = 2048;
/// Free texels kept around each bitmap, so filtering never reads a
/// neighbor.
const PAD: u32 = 1;

pub struct GlyphAtlas {
    pub tex: Texture,
    /// Shelves: top, height, width used.
    shelves: Vec<(u32, u32, u32)>,
    next_top: u32,
    rects: HashMap<u64, [u32; 4]>,
    /// Glyphs placed since the atlas last started over.
    pub generation: u64,
}

impl GlyphAtlas {
    pub fn new(device: &Device) -> Self {
        let tex = device.create_texture(TextureDesc {
            width: SIZE,
            height: SIZE,
            format: PixelFormat::Rgba16Float,
            cpu_readable: false,
        });
        Self {
            tex,
            shelves: Vec::new(),
            next_top: 0,
            rects: HashMap::new(),
            generation: 0,
        }
    }

    /// Room for a `w` x `h` bitmap, or None when the atlas is full.
    fn allocate(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        let (w, h) = (w + 2 * PAD, h + 2 * PAD);
        if w > SIZE || h > SIZE {
            return None;
        }
        // The shortest shelf it fits on, wasting the least height.
        let best = self
            .shelves
            .iter()
            .enumerate()
            .filter(|(_, s)| s.1 >= h && SIZE - s.2 >= w)
            .min_by_key(|(_, s)| s.1)
            .map(|(i, _)| i);
        let i = match best {
            Some(i) => i,
            None => {
                if self.next_top + h > SIZE {
                    return None;
                }
                self.shelves.push((self.next_top, h, 0));
                self.next_top += h;
                self.shelves.len() - 1
            }
        };
        let s = &mut self.shelves[i];
        let at = [s.2 + PAD, s.0 + PAD];
        s.2 += w;
        Some(at)
    }

    /// Where `b` is in the atlas (x, y, w, h texels), uploading it first if
    /// needed. None if the bitmap cannot fit. When the atlas is full it
    /// starts over and `generation` moves on: rects placed before are gone.
    pub fn place(
        &mut self,
        device: &Device,
        list: &mut CommandList,
        put: &Kernel,
        b: &GlyphBitmap,
    ) -> Option<[u32; 4]> {
        if let Some(r) = self.rects.get(&b.key) {
            return Some(*r);
        }
        let at = match self.allocate(b.width, b.height) {
            Some(at) => at,
            None => {
                // Full: start over. Draws recorded earlier already read
                // what they need (the copy below comes after them).
                self.shelves.clear();
                self.next_top = 0;
                self.rects.clear();
                self.generation += 1;
                self.allocate(b.width, b.height)?
            }
        };
        let bytes = unsafe { std::slice::from_raw_parts(b.rgba.as_ptr().cast::<u8>(), b.rgba.len() * 2) };
        let staging = device.upload_texture(
            TextureDesc {
                width: b.width,
                height: b.height,
                format: PixelFormat::Rgba16Float,
                cpu_readable: false,
            },
            bytes,
        );
        list.dispatch_ex(
            put,
            &self.tex,
            &[&staging],
            &[at[0] as f32, at[1] as f32, b.width as f32, b.height as f32],
            ultrafast_gpu::Bindings {
                buffers: &[],
                grid: Some((b.width, b.height)),
                threadgroup: Some((8, 8)),
            },
        );
        let r = [at[0], at[1], b.width, b.height];
        self.rects.insert(b.key, r);
        Some(r)
    }
}

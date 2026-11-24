//! Vector geometry for the GPU rasterizer: paths (SVG data, presets,
//! strokes, dashes, trims) and text (font lookup, shaping, layout).
//!
//! Everything expensive is cached: shaped lines by (font, size, text),
//! glyph outlines by (font, glyph), and final flattened segments by a hash
//! of every input (params, transform, time when animated). A static path or
//! label costs nothing per frame; an animated one re-flattens only.

use crate::{Affine, GeomError, text};
use kurbo::{BezPath, ParamCurve, ParamCurveArclen, PathEl, Point, Shape};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ultrafast_graph::{Format, Node, Param};
use unicode_script::{Script, UnicodeScript};

/// Flattened geometry ready for upload: per segment two float4s,
/// (x0, y0, x1, y1) and (weight, color group, 0, 0), in output pixels.
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub segments: Vec<f32>,
    /// Bounds in output pixels (min x, min y, max x, max y).
    pub bounds: [f32; 4],
    /// Colors of the segment groups, composited in order (each over the
    /// ones before): linear straight-alpha, or None for the node's own
    /// color. Empty means one group in the node's color.
    pub groups: Vec<Option<[f32; 4]>>,
    /// Color glyphs (emoji) drawn from their bitmaps over the groups.
    pub images: Vec<GlyphImage>,
    /// A text's glyph units laid out: what its `reveal` counts (0 for
    /// paths).
    pub units: usize,
}

/// A color glyph's bitmap (an sbix or CBDT strike): linear premultiplied
/// RGBA as half floats, row-major from the top.
pub struct GlyphBitmap {
    /// Identifies the bitmap (face, glyph, strike) for GPU caches.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u16>,
    /// Where it sits in font units: left, bottom, width, height (y up).
    pub place: [f64; 4],
}

impl std::fmt::Debug for GlyphBitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GlyphBitmap({:x}, {}x{})", self.key, self.width, self.height)
    }
}

/// A color glyph placed in the output.
#[derive(Clone, Debug)]
pub struct GlyphImage {
    pub bitmap: Arc<GlyphBitmap>,
    /// Maps the bitmap's unit square (u right, v down) to output pixels.
    pub xf: [f64; 6],
    /// Coverage weight (reveal, per-glyph opacity).
    pub weight: f32,
}

impl Geometry {
    pub fn count(&self) -> usize {
        self.segments.len() / 8
    }

    fn push(&mut self, a: Point, b: Point, w: f32, group: u32) {
        if a == b {
            return;
        }
        self.segments.extend([
            a.x as f32,
            a.y as f32,
            b.x as f32,
            b.y as f32,
            w,
            group as f32,
            0.0,
            0.0,
        ]);
        for p in [a, b] {
            self.bounds[0] = self.bounds[0].min(p.x as f32);
            self.bounds[1] = self.bounds[1].min(p.y as f32);
            self.bounds[2] = self.bounds[2].max(p.x as f32);
            self.bounds[3] = self.bounds[3].max(p.y as f32);
        }
    }

    fn empty() -> Self {
        Geometry {
            segments: Vec::new(),
            bounds: [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            groups: Vec::new(),
            images: Vec::new(),
            units: 0,
        }
    }

    /// Adds a color glyph, growing the bounds by its corners.
    fn push_image(&mut self, im: GlyphImage) {
        let m = im.xf;
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let x = (m[0] * u + m[2] * v + m[4]) as f32;
            let y = (m[1] * u + m[3] * v + m[5]) as f32;
            self.bounds[0] = self.bounds[0].min(x);
            self.bounds[1] = self.bounds[1].min(y);
            self.bounds[2] = self.bounds[2].max(x);
            self.bounds[3] = self.bounds[3].max(y);
        }
        self.images.push(im);
    }

    /// Whether there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.images.is_empty()
    }
}

/// Flattening tolerance in output pixels.
const TOLERANCE: f64 = 0.1;

/// Appends `path` (already in output space) as closed contours: fills need
/// every subpath closed, whether or not the data closes it.
fn flatten_closed(path: &BezPath, weight: f32, group: u32, out: &mut Geometry) {
    let mut start = Point::ZERO;
    let mut last = Point::ZERO;
    let mut open = false;
    kurbo::flatten(path.iter(), TOLERANCE, |el| match el {
        PathEl::MoveTo(p) => {
            if open {
                out.push(last, start, weight, group);
            }
            start = p;
            last = p;
            open = true;
        }
        PathEl::LineTo(p) => {
            out.push(last, p, weight, group);
            last = p;
        }
        PathEl::ClosePath => {
            out.push(last, start, weight, group);
            last = start;
            open = false;
        }
        _ => {}
    });
    if open {
        out.push(last, start, weight, group);
    }
}

pub struct Fonts {
    db: Option<fontdb::Database>,
    data: HashMap<fontdb::ID, (Arc<Vec<u8>>, u32)>,
    files: HashMap<PathBuf, fontdb::ID>,
    shaped: crate::lru::Lru<u64, Arc<ShapedLine>>,
    /// Outlines by face, glyph and variation-instance hash.
    glyphs: crate::lru::Lru<(fontdb::ID, u16, u64), Arc<BezPath>>,
    /// Whether a face has a glyph for a character, and which face stands in
    /// for characters the requested one lacks.
    covers: HashMap<(fontdb::ID, char), bool>,
    fallback: HashMap<char, Option<fontdb::ID>>,
    /// Families looked up by name: whether the database has them (after
    /// fetching from the font cache or Google Fonts when it did not).
    families: HashMap<String, bool>,
    /// Decoded color glyph bitmaps by face, glyph and strike.
    bitmaps: crate::lru::Lru<(fontdb::ID, u16, u16), Option<Arc<GlyphBitmap>>>,
    /// Color emoji faces installed, in preference order.
    emoji: Option<Vec<fontdb::ID>>,
    /// Whether a face has color bitmaps (sbix, CBDT).
    color: HashMap<fontdb::ID, bool>,
    /// Faces resolved by (font, weight, italic, base dir) hash: matching a
    /// family by weight scans every face, so it happens once (and again
    /// when faces are added).
    resolved: HashMap<u64, fontdb::ID>,
    /// Pinned faces by (lowercase family, weight, italic): ahead of every
    /// other source until [`Fonts::unpin`].
    pins: HashMap<(String, u16, bool), fontdb::ID>,
    /// How this document sets writing systems its fonts do not cover. Empty
    /// for almost every document, which then lays out exactly as it always
    /// has.
    script_fonts: Vec<ScriptFont>,
}

/// How a document sets one writing system: the face that stands in for each
/// requested font, and what that system's text may do. `meta.script_fonts`
/// in a document, for this render only.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptFont {
    /// ISO 15924 codes this row sets ("Deva", or "Hani", "Hira", "Kana" for
    /// Japanese).
    pub scripts: Vec<String>,
    /// Requested family (lowercase) to the family that sets this script,
    /// "Family@800" to set it at that weight whatever the text asks for.
    #[serde(default)]
    pub faces: HashMap<String, String>,
    /// The family for any other requested font.
    pub default: String,
    /// Size relative to the requested one, so a line reads at one size.
    #[serde(default = "one")]
    pub scale: f64,
    /// Line height relative to the requested one, for tall marks.
    #[serde(default = "one")]
    pub leading: f64,
    /// Whether letter spacing applies. Connected scripts and scripts whose
    /// letters hang from one line break apart with it.
    #[serde(default = "yes")]
    pub tracking: bool,
    /// Whether per-letter animation moves whole words instead of clusters
    /// (the same scripts).
    #[serde(default)]
    pub whole_words: bool,
    /// Language for the font's localized forms ("mr", "ur").
    #[serde(default)]
    pub language: Option<String>,
}

fn one() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

/// A font file that answers for a family, weight and style (see
/// [`Fonts::pin`]).
#[derive(Debug, Clone, PartialEq)]
pub struct FontPin {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
    pub path: PathBuf,
}

/// A glyph of a laid-out line: its face (the requested one, or a fallback
/// for characters it lacks), position and advance in pixels.
#[derive(Debug, Clone)]
pub struct LineGlyph {
    pub font: fontdb::ID,
    pub id: u16,
    pub x: f64,
    pub y: f64,
    pub advance: f64,
    /// Byte offset of the text it sets.
    pub cluster: usize,
    /// Pixels per font unit of its face.
    pub scale: f64,
    /// Letter spacing units before it. Each glyph's own in Latin text;
    /// a vowel sign shares its letter's, and a connected script has none.
    pub track: u32,
    /// Which animation unit it moves with, in reading order: each glyph in
    /// Latin text, a letter with its marks, or a whole word.
    pub unit: u32,
    /// Set in a stand-in face for its script: never slanted to fake italics.
    pub upright: bool,
}

/// Faces tried first for characters the requested font lacks.
const FALLBACK_FAMILIES: &[&str] = &[
    "Helvetica Neue",
    "Apple Symbols",
    "Arial Unicode MS",
    "PingFang SC",
    "Hiragino Sans",
    "Apple SD Gothic Neo",
    "Noto Sans",
    "Noto Sans Symbols 2",
    "DejaVu Sans",
    "Segoe UI Symbol",
];

/// Color emoji faces, in preference order.
const EMOJI_FAMILIES: &[&str] = &[
    "Apple Color Emoji",
    "Noto Color Emoji",
    "Segoe UI Emoji",
    "Twemoji Mozilla",
];

/// Whether `c` (followed by `next`) is shown as an emoji: characters whose
/// default presentation is emoji (Unicode's Emoji_Presentation), and any
/// character asking for it with VARIATION SELECTOR-16.
fn emoji_presentation(c: char, next: Option<char>) -> bool {
    match next {
        Some('\u{fe0f}') => return true,
        Some('\u{fe0e}') => return false,
        _ => {}
    }
    matches!(c as u32,
        0x231A..=0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD..=0x25FE | 0x2614..=0x2615
        | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA..=0x26AB | 0x26BD..=0x26BE
        | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD
        | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757
        | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
        | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F1E6..=0x1F1FF | 0x1F201 | 0x1F21A
        | 0x1F22F | 0x1F232..=0x1F236 | 0x1F238..=0x1F23A | 0x1F250..=0x1F251 | 0x1F300..=0x1F320
        | 0x1F32D..=0x1F335 | 0x1F337..=0x1F37C | 0x1F37E..=0x1F393 | 0x1F3A0..=0x1F3CA
        | 0x1F3CF..=0x1F3D3 | 0x1F3E0..=0x1F3F0 | 0x1F3F4 | 0x1F3F8..=0x1F43E | 0x1F440
        | 0x1F442..=0x1F4FC | 0x1F4FF..=0x1F53D | 0x1F54B..=0x1F54E | 0x1F550..=0x1F567 | 0x1F57A
        | 0x1F595..=0x1F596 | 0x1F5A4 | 0x1F5FB..=0x1F64F | 0x1F680..=0x1F6C5 | 0x1F6CC
        | 0x1F6D0..=0x1F6D2 | 0x1F6D5..=0x1F6D7 | 0x1F6DC..=0x1F6DF | 0x1F6EB..=0x1F6EC
        | 0x1F6F4..=0x1F6FC | 0x1F7E0..=0x1F7EB | 0x1F7F0 | 0x1F90C..=0x1F93A | 0x1F93C..=0x1F945
        | 0x1F947..=0x1F9FF | 0x1FA70..=0x1FAFF)
}

/// Characters that belong to the one before them in an emoji sequence.
fn attaches(c: char) -> bool {
    matches!(c as u32, 0x200D | 0xFE0E | 0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
}

/// Decodes a glyph's raster image to linear premultiplied RGBA (half
/// floats) and its placement in font units (left, bottom, width, height).
fn decode_glyph_image(
    img: &rustybuzz::ttf_parser::RasterGlyphImage,
    units_per_em: f64,
) -> Option<(u32, u32, Vec<u16>, [f64; 4])> {
    use rustybuzz::ttf_parser::RasterImageFormat;
    // Straight-alpha sRGB8 or premultiplied sRGB8 pixels.
    let (w, h, px, premultiplied): (u32, u32, Vec<u8>, bool) = match img.format {
        RasterImageFormat::PNG => {
            let im = image::load_from_memory_with_format(img.data, image::ImageFormat::Png).ok()?;
            let im = im.to_rgba8();
            (im.width(), im.height(), im.into_raw(), false)
        }
        RasterImageFormat::BitmapPremulBgra32 => {
            let (w, h) = (img.width as u32, img.height as u32);
            let mut px = img.data.get(..(w * h * 4) as usize)?.to_vec();
            for p in px.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            (w, h, px, true)
        }
        _ => return None,
    };
    if w == 0 || h == 0 {
        return None;
    }
    let lut: Vec<f32> = (0..256)
        .map(|v| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    let mut rgba = Vec::with_capacity(px.len());
    for p in px.chunks_exact(4) {
        let a = p[3] as f32 / 255.0;
        let un = |v: u8| {
            if premultiplied && a > 0.0 {
                lut[((v as f32 / a).round().min(255.0)) as usize]
            } else {
                lut[v as usize]
            }
        };
        for c in [un(p[0]) * a, un(p[1]) * a, un(p[2]) * a, a] {
            rgba.push(crate::f32_to_f16(c));
        }
    }
    let s = units_per_em / img.pixels_per_em.max(1) as f64;
    // The image may be larger than its metrics say: scale to what it is.
    let (mw, mh) = (img.width.max(1) as f64, img.height.max(1) as f64);
    let place = [img.x as f64 * s, img.y as f64 * s, mw * s, mh * s];
    let _ = (w, h);
    Some((w, h, rgba, place))
}

/// A variable font instance: axis tag and value pairs.
pub type Variations = Vec<rustybuzz::Variation>;

fn variations_key(vars: &[rustybuzz::Variation]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in vars {
        v.tag.to_bytes().hash(&mut h);
        v.value.to_bits().hash(&mut h);
    }
    h.finish()
}

#[derive(Debug, Clone)]
pub struct ShapedGlyph {
    pub id: u16,
    /// Pen position and offset in font units.
    pub x: f32,
    pub y: f32,
    /// Advance in font units, and the byte offset of the text it came from.
    pub advance: f32,
    pub cluster: u32,
}

#[derive(Debug)]
pub struct ShapedLine {
    pub glyphs: Vec<ShapedGlyph>,
    /// Total advance in font units.
    pub advance: f32,
}

/// Applies a face's pair-kerning `kerx` subtables (formats 0, 2 and 6),
/// which rustybuzz 0.20 reads but never applies (its skipping iterator
/// ignores base glyphs), as HarfBuzz does: half of each pair's value on
/// each glyph. Faces such as macOS's Helvetica Neue kern only through
/// `kerx`. Shaping that applied a state-machine `kerx` (formats 1, 4) or
/// GPOS is left alone.
fn kerx_pairs(face: &rustybuzz::Face, infos: &[rustybuzz::GlyphInfo], pos: &mut [(i32, i32, i32)]) {
    use rustybuzz::ttf_parser::{GlyphId, kerx::Format};
    let t = face.tables();
    let Some(kerx) = t.kerx else { return };
    if t.gpos.is_some() && t.gsub.is_some() {
        return;
    }
    for sub in kerx.subtables {
        if sub.variable || !sub.horizontal || sub.has_cross_stream {
            continue;
        }
        if !matches!(sub.format, Format::Format0(_) | Format::Format2(_) | Format::Format6(_)) {
            continue;
        }
        for i in 0..infos.len().saturating_sub(1) {
            let k = sub
                .glyphs_kerning(GlyphId(infos[i].glyph_id as u16), GlyphId(infos[i + 1].glyph_id as u16))
                .unwrap_or(0) as i32;
            if k != 0 {
                let k1 = k >> 1;
                let k2 = k - k1;
                pos[i].0 += k1;
                pos[i + 1].0 += k2;
                pos[i + 1].1 += k2;
            }
        }
    }
}

struct FontMetrics {
    units_per_em: f32,
    ascender: f32,
    descender: f32,
}

/// Faces bundled into ultrafast (SIL Open Font License; see
/// examples/assets/fonts/licenses, linked from this crate's fonts/), loadable by family name or as
/// `ultrafast:sans`, `ultrafast:mono`, `ultrafast:serif`, `ultrafast:display`, `ultrafast:wide`.
const BUNDLED: &[&[u8]] = &[
    include_bytes!("../fonts/Geist-VF.ttf"),
    include_bytes!("../fonts/GeistMono-VF.ttf"),
    include_bytes!("../fonts/InstrumentSerif-Regular.ttf"),
    include_bytes!("../fonts/InstrumentSerif-Italic.ttf"),
    include_bytes!("../fonts/Fraunces-VF.ttf"),
    include_bytes!("../fonts/Fraunces-Italic-VF.ttf"),
    include_bytes!("../fonts/Unbounded-VF.ttf"),
];

/// The font files bundled into ultrafast (for tools that search faces).
pub fn bundled_fonts() -> &'static [&'static [u8]] {
    BUNDLED
}

/// The faces text is set from: the system's fonts, ultrafast's bundled
/// faces, and CSS's generic families mapped to faces this machine has.
/// The text op and SVG import both start from it.
pub fn base_database() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // Bundled faces (SIL Open Font License): the same type on every
    // machine, servers included, by family name.
    for data in BUNDLED {
        db.load_font_data(data.to_vec());
    }
    // CSS generic families resolve to faces this machine has
    // (fontdb's defaults, Arial and friends, are missing on servers):
    // on Linux what Chrome's fontconfig picks (Liberation, metric
    // twins of Arial, Times New Roman and Courier New), on a Mac what
    // Safari and Chrome show.
    let pick = |db: &fontdb::Database, names: &[&str]| {
        names
            .iter()
            .find(|n| db.faces().any(|f| f.families.iter().any(|(fam, _)| fam == *n)))
            .map(|n| n.to_string())
    };
    let mac = cfg!(target_os = "macos");
    let sans: &[&str] = if mac {
        &["Helvetica", "Arial", "Geist"]
    } else {
        &["Liberation Sans", "Arial", "DejaVu Sans", "Noto Sans", "Geist"]
    };
    let serif: &[&str] = if mac {
        &["Times", "Times New Roman", "Instrument Serif"]
    } else {
        &[
            "Liberation Serif",
            "Times New Roman",
            "DejaVu Serif",
            "Noto Serif",
            "Instrument Serif",
        ]
    };
    let mono: &[&str] = if mac {
        &["Courier", "Courier New", "Menlo", "Geist Mono"]
    } else {
        &[
            "Liberation Mono",
            "Courier New",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
            "Geist Mono",
        ]
    };
    if let Some(f) = pick(&db, sans) {
        db.set_sans_serif_family(f.clone());
        db.set_cursive_family(f.clone());
        db.set_fantasy_family(f);
    }
    if let Some(f) = pick(&db, serif) {
        db.set_serif_family(f);
    }
    if let Some(f) = pick(&db, mono) {
        db.set_monospace_family(f);
    }
    db
}

impl Fonts {
    pub fn new() -> Self {
        Self {
            db: None,
            data: HashMap::new(),
            files: HashMap::new(),
            // Bounded: text edited live would otherwise grow these forever.
            shaped: crate::lru::Lru::new(4096),
            glyphs: crate::lru::Lru::new(32768),
            covers: HashMap::new(),
            fallback: HashMap::new(),
            families: HashMap::new(),
            bitmaps: crate::lru::Lru::new(512),
            emoji: None,
            color: HashMap::new(),
            resolved: HashMap::new(),
            pins: HashMap::new(),
            script_fonts: Vec::new(),
        }
    }

    /// How this document sets the writing systems its fonts do not cover,
    /// until [`Fonts::unpin`] or the next call.
    pub fn set_script_fonts(&mut self, rows: Vec<ScriptFont>) {
        self.script_fonts = rows;
    }

    /// The row that sets `c`, when a document has one for its script.
    fn script_row(&self, c: char) -> Option<usize> {
        if self.script_fonts.is_empty() {
            return None;
        }
        let script = c.script();
        if matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
            return None;
        }
        let name = script.short_name();
        self.script_fonts
            .iter()
            .position(|row| row.scripts.iter().any(|s| s == name))
    }

    /// Line height multiplier of `text`: the tallest of its scripts.
    pub fn script_leading(&self, text: &str) -> f64 {
        if self.script_fonts.is_empty() {
            return 1.0;
        }
        text.chars()
            .filter_map(|c| self.script_row(c))
            .map(|row| self.script_fonts[row].leading)
            .fold(1.0, f64::max)
    }

    /// Whether `text` reads right to left (for a document that sets a
    /// script: every other document lays out as it always has).
    pub fn reads_rtl(&self, text: &str) -> bool {
        !self.script_fonts.is_empty() && unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl
    }

    /// The family, weight and italic a face answers for.
    fn face_key(&mut self, id: fontdb::ID) -> (String, u16, bool) {
        if let Some(((family, weight, italic), _)) = self.pins.iter().find(|(_, v)| **v == id) {
            return (family.clone(), *weight, *italic);
        }
        self.db()
            .face(id)
            .map(|f| {
                (
                    f.families.first().map(|(n, _)| n.to_lowercase()).unwrap_or_default(),
                    f.weight.0,
                    f.style != fontdb::Style::Normal,
                )
            })
            .unwrap_or_default()
    }

    /// The face a row sets `primary`'s text in, closest to its weight.
    fn script_face(&mut self, primary: fontdb::ID, row: usize) -> Result<fontdb::ID, String> {
        let (family, weight, _) = self.face_key(primary);
        let spec = &self.script_fonts[row];
        let target = spec.faces.get(&family).unwrap_or(&spec.default).clone();
        // "Family@800": a one-weight display font asks for 400 but reads heavy.
        let (target, weight) = match target
            .rsplit_once('@')
            .and_then(|(f, w)| Some((f, w.parse::<u16>().ok()?)))
        {
            Some((f, w)) => (f.to_string(), w),
            None => (target, weight),
        };
        if let Some(id) = self.pinned(&target, weight, false) {
            return Ok(id);
        }
        let q = fontdb::Query {
            families: &[fontdb::Family::Name(&target)],
            weight: fontdb::Weight(weight),
            ..Default::default()
        };
        self.db()
            .query(&q)
            .ok_or_else(|| format!("no face for script font `{target}` (it must be pinned in meta.fonts)"))
    }

    /// The face a family and weight resolve to (for [`Fonts::missing_glyphs`]).
    pub fn lookup(&mut self, family: &str, weight: u16) -> Result<fontdb::ID, String> {
        self.resolve(family, weight, false, Path::new("."))
    }

    /// Characters of `text` no face in this document can draw, as a render
    /// would find them (a look's check, before anything renders).
    pub fn missing_glyphs(&mut self, primary: fontdb::ID, text: &str) -> Vec<char> {
        let mut out = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            // Spaces and the marks that draw nothing (joiners, direction marks).
            if c.is_whitespace()
                || c.is_control()
                || attaches(c)
                || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0xFEFF)
            {
                continue;
            }
            if emoji_presentation(c, chars.get(i + 1).copied()) {
                continue;
            }
            let found = match self.script_row(c) {
                Some(row) => {
                    self.has_glyph(primary, c)
                        || self.script_face(primary, row).is_ok_and(|face| self.has_glyph(face, c))
                }
                None => self.has_glyph(primary, c) || self.fallback_for(c).is_some(),
            };
            if !found && !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// Pins font files to their family, weight and style: until
    /// [`Fonts::unpin`], those files answer for them ahead of every other
    /// font (installed, bundled or fetched), so text is set with exactly the
    /// files it was measured with elsewhere. Replaces earlier pins.
    pub fn pin(&mut self, pins: &[FontPin]) -> Result<(), String> {
        self.unpin();
        for p in pins {
            let ids = self.db().load_font_source(fontdb::Source::File(p.path.clone()));
            let id = *ids
                .first()
                .ok_or_else(|| format!("no font in {} (pinned for {})", p.path.display(), p.family))?;
            self.pins
                .insert((p.family.trim().to_lowercase(), p.weight, p.italic), id);
        }
        self.forget_resolved();
        Ok(())
    }

    /// Drops the pins (their faces leave the database).
    pub fn unpin(&mut self) {
        self.script_fonts.clear();
        if self.pins.is_empty() {
            return;
        }
        let pinned: Vec<(String, fontdb::ID)> = self.pins.drain().map(|((f, _, _), id)| (f, id)).collect();
        for (family, id) in pinned {
            self.db().remove_face(id);
            self.data.remove(&id);
            self.color.remove(&id);
            self.families.remove(&family);
        }
        self.forget_resolved();
    }

    /// After faces come or go: lookups that may name them start over.
    fn forget_resolved(&mut self) {
        self.resolved.clear();
        self.fallback.clear();
        self.covers.clear();
        self.emoji = None;
    }

    /// The pinned face for a family closest to `weight` (same style first).
    fn pinned(&self, family: &str, weight: u16, italic: bool) -> Option<fontdb::ID> {
        if self.pins.is_empty() {
            return None;
        }
        let key = family.trim().to_lowercase();
        self.pins
            .iter()
            .filter(|((f, _, _), _)| *f == key)
            .min_by_key(|((_, w, i), _)| ((*i != italic) as u32, (*w as i32 - weight as i32).unsigned_abs()))
            .map(|(_, id)| *id)
    }

    /// Whether `family` is available, loading it from the font cache or
    /// downloading it from Google Fonts the first time it is missing.
    fn ensure_family(&mut self, family: &str) -> bool {
        let key = family.to_lowercase();
        if let Some(&ok) = self.families.get(&key) {
            return ok;
        }
        let has = |db: &fontdb::Database| {
            db.faces()
                .any(|f| f.families.iter().any(|(n, _)| n.to_lowercase() == key))
        };
        let mut ok = has(self.db());
        if !ok {
            for p in crate::fontfetch::fetch(family) {
                let _ = self.db().load_font_file(&p);
            }
            self.resolved.clear();
            ok = has(self.db());
        }
        self.families.insert(key, ok);
        ok
    }

    /// Whether `family` is a named family this machine does not have yet
    /// (generic families, ultrafast's aliases and font files never are).
    pub fn lacks(&mut self, family: &str) -> bool {
        let key = family.trim().to_lowercase();
        let generic = matches!(
            key.as_str(),
            "" | "sans-serif"
                | "serif"
                | "monospace"
                | "cursive"
                | "fantasy"
                | "system-ui"
                | "ui-sans-serif"
                | "ui-serif"
                | "ui-monospace"
                | "-apple-system"
                | "blinkmacsystemfont"
        );
        if generic || key.starts_with("ultrafast:") || [".ttf", ".otf", ".ttc"].iter().any(|e| key.ends_with(e)) {
            return false;
        }
        if self.pins.keys().any(|(f, _, _)| *f == key) {
            return false;
        }
        if let Some(&ok) = self.families.get(&key) {
            return !ok;
        }
        !self
            .db()
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n.to_lowercase() == key))
    }

    fn db(&mut self) -> &mut fontdb::Database {
        self.db.get_or_insert_with(base_database)
    }

    /// Resolves a font from a family name, a CSS family list (`"Poppins",
    /// sans-serif`: the first one this machine has or can fetch), a CSS
    /// generic family, or a font file path.
    fn resolve(&mut self, font: &str, weight: u16, italic: bool, base: &Path) -> Result<fontdb::ID, String> {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (font, weight, italic, base).hash(&mut h);
            h.finish()
        };
        if let Some(&id) = self.resolved.get(&key) {
            return Ok(id);
        }
        let id = self.resolve_uncached(font, weight, italic, base)?;
        self.resolved.insert(key, id);
        Ok(id)
    }

    fn resolve_uncached(&mut self, font: &str, weight: u16, italic: bool, base: &Path) -> Result<fontdb::ID, String> {
        let names: Vec<&str> = font
            .split(',')
            .map(|f| f.trim().trim_matches(|c| c == '"' || c == '\''))
            .filter(|f| !f.is_empty())
            .collect();
        if names.len() > 1 {
            for (i, name) in names.iter().enumerate() {
                if let Some(id) = self.resolve_one(name, weight, italic, base, i + 1 == names.len())? {
                    return Ok(id);
                }
            }
        }
        self.resolve_one(names.first().copied().unwrap_or(""), weight, italic, base, true)?
            .ok_or_else(|| format!("no font matches `{font}`"))
    }

    /// One family (or a file); with `fallback`, a missing family resolves
    /// to the sans-serif family instead of None.
    fn resolve_one(
        &mut self,
        font: &str,
        weight: u16,
        italic: bool,
        base: &Path,
        fallback: bool,
    ) -> Result<Option<fontdb::ID>, String> {
        let looks_like_file = font.ends_with(".ttf") || font.ends_with(".otf") || font.ends_with(".ttc");
        if looks_like_file {
            let path = if Path::new(font).is_absolute() {
                PathBuf::from(font)
            } else {
                base.join(font)
            };
            // A service rendering many callers' documents reads font files
            // only from the document's own inputs (its base directory), so
            // markup cannot name another path on the machine.
            if std::env::var("ULTRAFAST_NO_EXTERNAL_FILES").as_deref() == Ok("1")
                && (!path.starts_with(base) || path.components().any(|c| c == std::path::Component::ParentDir))
            {
                return Err(format!("font file {} is outside the document's inputs", path.display()));
            }
            if let Some(id) = self.files.get(&path) {
                return Ok(Some(*id));
            }
            let ids = self.db().load_font_source(fontdb::Source::File(path.clone()));
            self.resolved.clear();
            let id = *ids.first().ok_or_else(|| format!("no font in {}", path.display()))?;
            self.files.insert(path, id);
            return Ok(Some(id));
        }
        if let Some(id) = self.pinned(font, weight, italic) {
            return Ok(Some(id));
        }
        let font = match font {
            "ultrafast:sans" => "Geist",
            "ultrafast:mono" => "Geist Mono",
            "ultrafast:serif" => "Instrument Serif",
            "ultrafast:display" => "Fraunces",
            "ultrafast:wide" => "Unbounded",
            f => f,
        };
        let generic = match font.to_ascii_lowercase().as_str() {
            "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" | "blinkmacsystemfont" => {
                Some(fontdb::Family::SansSerif)
            }
            "serif" | "ui-serif" => Some(fontdb::Family::Serif),
            "monospace" | "ui-monospace" => Some(fontdb::Family::Monospace),
            "cursive" => Some(fontdb::Family::Cursive),
            "fantasy" => Some(fontdb::Family::Fantasy),
            _ => None,
        };
        if !font.is_empty() && generic.is_none() {
            self.ensure_family(font);
        }
        let families: Vec<fontdb::Family> = if let Some(g) = generic {
            vec![g, fontdb::Family::SansSerif]
        } else if font.is_empty() {
            vec![
                fontdb::Family::Name("Helvetica Neue"),
                fontdb::Family::Name("Geist"),
                fontdb::Family::SansSerif,
            ]
        } else if fallback {
            vec![fontdb::Family::Name(font), fontdb::Family::SansSerif]
        } else {
            vec![fontdb::Family::Name(font)]
        };
        let q = fontdb::Query {
            families: &families,
            weight: fontdb::Weight(weight),
            stretch: fontdb::Stretch::Normal,
            style: if italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
        };
        let Some(found) = self.db().query(&q) else {
            return Ok(None);
        };
        if font.is_empty() || generic.is_some() {
            return Ok(Some(found));
        }
        // Google's static families name each extra weight as its own family
        // ("Poppins Medium", weight class 500), which a query for "Poppins"
        // never sees, so 500 fell back to 400. Match those faces by weight.
        let key = font.to_lowercase();
        let prefix = format!("{key} ");
        let want_italic = italic;
        // A face of another width is not a weight of this family, even when
        // its file calls it a member with normal width ("Helvetica Neue
        // Condensed Black" in macOS's HelveticaNeue.ttc).
        let other_width = |f: &fontdb::FaceInfo| {
            let names = format!(
                "{} {}",
                f.post_script_name,
                f.families.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" ")
            )
            .to_lowercase();
            f.stretch != fontdb::Stretch::Normal
                || ["condensed", "compressed", "narrow", "extended", "expanded"]
                    .iter()
                    .any(|w| names.contains(w) && !key.contains(w))
        };
        let mut best: Option<(fontdb::ID, i32, bool)> = None;
        for f in self.db().faces() {
            let named = f.families.iter().any(|(n, _)| {
                let n = n.to_lowercase();
                n == key || n.starts_with(&prefix)
            });
            if !named || other_width(f) {
                continue;
            }
            let style_ok = (f.style != fontdb::Style::Normal) == want_italic;
            let d = (f.weight.0 as i32 - weight as i32).abs();
            let better = match best {
                None => true,
                Some((_, bd, bs)) => (style_ok && !bs) || (style_ok == bs && d < bd),
            };
            if better {
                best = Some((f.id, d, style_ok));
            }
        }
        let found_d = self
            .db()
            .face(found)
            .map(|f| (f.weight.0 as i32 - weight as i32).abs())
            .unwrap_or(i32::MAX);
        let found_style = self
            .db()
            .face(found)
            .is_some_and(|f| (f.style != fontdb::Style::Normal) == want_italic);
        let found_width = self.db().face(found).is_some_and(other_width);
        match best {
            Some((id, d, style_ok)) if found_width || (style_ok && (d < found_d || !found_style)) => Ok(Some(id)),
            _ => Ok(Some(found)),
        }
    }

    /// Whether a face is an italic or oblique design.
    fn is_italic(&mut self, id: fontdb::ID) -> bool {
        self.db().face(id).is_some_and(|f| f.style != fontdb::Style::Normal)
    }

    fn face_data(&mut self, id: fontdb::ID) -> Result<(Arc<Vec<u8>>, u32), String> {
        if let Some(d) = self.data.get(&id) {
            return Ok(d.clone());
        }
        let d = self
            .db()
            .with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))
            .ok_or("font data unavailable")?;
        self.data.insert(id, d.clone());
        Ok(d)
    }

    fn metrics(&mut self, id: fontdb::ID) -> Result<FontMetrics, String> {
        let (data, index) = self.face_data(id)?;
        let face = rustybuzz::Face::from_slice(&data, index).ok_or("unparseable font")?;
        Ok(FontMetrics {
            units_per_em: face.units_per_em() as f32,
            ascender: face.ascender() as f32,
            descender: face.descender() as f32,
        })
    }

    /// Variation axes of a face: (tag, min, default, max).
    fn axes(&mut self, id: fontdb::ID) -> Result<Vec<([u8; 4], f32, f32, f32)>, String> {
        let (data, index) = self.face_data(id)?;
        let face = rustybuzz::Face::from_slice(&data, index).ok_or("unparseable font")?;
        Ok(face
            .variation_axes()
            .into_iter()
            .map(|a| (a.tag.to_bytes(), a.min_value, a.def_value, a.max_value))
            .collect())
    }

    fn has_glyph(&mut self, id: fontdb::ID, c: char) -> bool {
        if let Some(&v) = self.covers.get(&(id, c)) {
            return v;
        }
        let v = self
            .db()
            .with_face_data(id, |data, index| {
                rustybuzz::Face::from_slice(data, index).is_some_and(|f| f.glyph_index(c).is_some())
            })
            .unwrap_or(false);
        self.covers.insert((id, c), v);
        v
    }

    /// A face that has `c`: preferred fallback families first, then any.
    fn fallback_for(&mut self, c: char) -> Option<fontdb::ID> {
        if let Some(v) = self.fallback.get(&c) {
            return *v;
        }
        let mut found = None;
        for fam in FALLBACK_FAMILIES {
            let q = fontdb::Query {
                families: &[fontdb::Family::Name(fam)],
                ..Default::default()
            };
            if let Some(id) = self.db().query(&q)
                && self.has_glyph(id, c)
            {
                found = Some(id);
                break;
            }
        }
        if found.is_none() {
            let ids: Vec<fontdb::ID> = self.db().faces().map(|f| f.id).collect();
            found = ids.into_iter().find(|&id| self.has_glyph(id, c));
        }
        self.fallback.insert(c, found);
        found
    }

    /// The preferred color emoji face that has `c`.
    fn emoji_face(&mut self, c: char) -> Option<fontdb::ID> {
        if self.emoji.is_none() {
            let mut ids = self.emoji_faces();
            // Color emoji are drawn from bitmap strikes. Windows has only
            // Segoe UI Emoji (COLR layers) and a server may have none: then
            // Noto Color Emoji, what the render service draws with, is
            // fetched once.
            if !ids.iter().any(|&id| self.has_color_bitmaps(id)) {
                for p in crate::fontfetch::fetch_emoji() {
                    let _ = self.db().load_font_file(&p);
                }
                ids = self.emoji_faces();
            }
            self.emoji = Some(ids);
        }
        let ids = self.emoji.clone().unwrap_or_default();
        ids.into_iter().find(|&id| self.has_glyph(id, c))
    }

    /// Installed emoji faces, in order of preference.
    fn emoji_faces(&mut self) -> Vec<fontdb::ID> {
        let mut ids = Vec::new();
        for fam in EMOJI_FAMILIES {
            let q = fontdb::Query {
                families: &[fontdb::Family::Name(fam)],
                ..Default::default()
            };
            if let Some(id) = self.db().query(&q) {
                ids.push(id);
            }
        }
        ids
    }

    /// Whether a face draws (some) glyphs from color bitmaps.
    pub fn has_color_bitmaps(&mut self, id: fontdb::ID) -> bool {
        if let Some(&c) = self.color.get(&id) {
            return c;
        }
        let c = self.face_data(id).ok().is_some_and(|(data, index)| {
            rustybuzz::ttf_parser::Face::parse(&data, index)
                .is_ok_and(|f| f.tables().sbix.is_some() || f.tables().cbdt.is_some())
        });
        self.color.insert(id, c);
        c
    }

    /// The bitmap of a color glyph at the strike nearest `ppem` (pixels per
    /// em on screen), or None for glyphs without one.
    pub fn glyph_bitmap(&mut self, id: fontdb::ID, glyph: u16, ppem: f64) -> Option<Arc<GlyphBitmap>> {
        let (data, index) = self.face_data(id).ok()?;
        let face = rustybuzz::ttf_parser::Face::parse(&data, index).ok()?;
        let want = ppem.ceil().clamp(1.0, u16::MAX as f64) as u16;
        let img = face.glyph_raster_image(rustybuzz::ttf_parser::GlyphId(glyph), want)?;
        let key = (id, glyph, img.pixels_per_em);
        if let Some(b) = self.bitmaps.get(&key) {
            return b;
        }
        let decoded = decode_glyph_image(&img, face.units_per_em() as f64).map(|(width, height, rgba, place)| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h);
            Arc::new(GlyphBitmap {
                key: h.finish(),
                width,
                height,
                rgba,
                place,
            })
        });
        self.bitmaps.insert(key, decoded.clone());
        decoded
    }

    /// Shapes a line in `primary`, setting characters it has no glyph for
    /// in a fallback face (split into runs; variations apply to `primary`).
    pub fn layout(
        &mut self,
        primary: fontdb::ID,
        text: &str,
        size: f64,
        features: &[rustybuzz::Feature],
        vars: &[rustybuzz::Variation],
    ) -> Result<(Vec<LineGlyph>, f64), String> {
        if !self.script_fonts.is_empty() {
            return self.layout_scripts(primary, text, size, features, vars);
        }
        // Runs of characters by the face that sets them. Characters shown
        // as emoji by default (or asking for it with VS16) are set in a
        // color emoji face, as browsers do, even when the font has a plain
        // glyph for them.
        let mut runs: Vec<(fontdb::ID, usize, usize)> = Vec::new();
        let mut chars = text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = chars.peek().map(|&(_, n)| n);
            let face = if c.is_whitespace() || c.is_control() {
                primary
            } else if let Some(e) = emoji_presentation(c, next).then(|| self.emoji_face(c)).flatten() {
                e
            } else if self.has_glyph(primary, c) {
                primary
            } else {
                self.fallback_for(c).unwrap_or(primary)
            };
            match runs.last_mut() {
                // Joiners, variation selectors, keycaps, skin tones and tag
                // characters stay with what they attach to, so the face
                // shapes the whole sequence (one glyph for a family or flag).
                Some(r) if r.0 == face || attaches(c) => r.2 = i + c.len_utf8(),
                _ => runs.push((face, i, i + c.len_utf8())),
            }
        }
        let mut out = Vec::new();
        let mut pen = 0.0f64;
        for (face, start, end) in runs {
            let fv: &[rustybuzz::Variation] = if face == primary { vars } else { &[] };
            let shaped = self.shape(face, &text[start..end], features, fv, None, None)?;
            let scale = size / self.metrics(face)?.units_per_em as f64;
            for g in &shaped.glyphs {
                let k = out.len() as u32;
                out.push(LineGlyph {
                    font: face,
                    id: g.id,
                    x: pen + g.x as f64 * scale,
                    y: g.y as f64 * scale,
                    advance: g.advance as f64 * scale,
                    cluster: start + g.cluster as usize,
                    scale,
                    track: k,
                    unit: k,
                    upright: false,
                });
            }
            pen += shaped.advance as f64 * scale;
        }
        Ok((out, pen))
    }

    /// [`Fonts::layout`] for a document that sets scripts: each character in
    /// the face its script's row gives (unless the requested font has all of
    /// that stretch itself), right-to-left text in visual order, spacing and
    /// animation by whole clusters or words.
    fn layout_scripts(
        &mut self,
        primary: fontdb::ID,
        text: &str,
        size: f64,
        features: &[rustybuzz::Feature],
        vars: &[rustybuzz::Variation],
    ) -> Result<(Vec<LineGlyph>, f64), String> {
        let mut out: Vec<LineGlyph> = Vec::new();
        let mut pen = 0.0f64;
        let (mut track, mut unit) = (0u32, 0u32);
        let info = unicode_bidi::BidiInfo::new(text, None);
        for para in &info.paragraphs {
            let (levels, level_runs) = info.visual_runs(para, para.range.clone());
            for level_run in level_runs {
                let rtl = levels[level_run.start].is_rtl();
                let mut pieces = self.script_pieces(primary, text, level_run.clone())?;
                if rtl {
                    pieces.reverse();
                }
                let dir = if rtl {
                    rustybuzz::Direction::RightToLeft
                } else {
                    rustybuzz::Direction::LeftToRight
                };
                for (face, row, start, end) in pieces {
                    let spec = row.map(|r| self.script_fonts[r].clone());
                    let fv: &[rustybuzz::Variation] = if face == primary { vars } else { &[] };
                    let language = spec.as_ref().and_then(|s| s.language.clone());
                    let shaped = self.shape(face, &text[start..end], features, fv, Some(dir), language.as_deref())?;
                    let scale = size * spec.as_ref().map_or(1.0, |s| s.scale) / self.metrics(face)?.units_per_em as f64;
                    let (spaced, whole) = spec.as_ref().map_or((true, false), |s| (s.tracking, s.whole_words));
                    if !out.is_empty() {
                        track += 1;
                        unit += 1;
                    }
                    let mut previous: Option<u32> = None;
                    for g in &shaped.glyphs {
                        // Latin glyphs each move alone; in a stand-in face a
                        // cluster (a letter and its marks) moves as one.
                        let cluster = start + g.cluster as usize;
                        let new_cluster = previous.is_some_and(|p| row.is_none() || p != g.cluster);
                        if new_cluster && (spaced || row.is_none()) {
                            track += 1;
                        }
                        if new_cluster {
                            let at_space = text[cluster..].chars().next().is_some_and(char::is_whitespace);
                            let after_space = out.last().is_some_and(|l: &LineGlyph| {
                                text[l.cluster..].chars().next().is_some_and(char::is_whitespace)
                            });
                            if !whole || at_space || after_space {
                                unit += 1;
                            }
                        }
                        previous = Some(g.cluster);
                        out.push(LineGlyph {
                            font: face,
                            id: g.id,
                            x: pen + g.x as f64 * scale,
                            y: g.y as f64 * scale,
                            advance: g.advance as f64 * scale,
                            cluster,
                            scale,
                            track,
                            unit,
                            upright: row.is_some(),
                        });
                    }
                    pen += shaped.advance as f64 * scale;
                }
            }
        }
        // Reading order: a right-to-left line reveals from its right.
        if unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl {
            let last = out.iter().map(|g| g.unit).max().unwrap_or(0);
            for g in &mut out {
                g.unit = last - g.unit;
            }
        }
        Ok((out, pen))
    }

    /// Stretches of `text[range]` (in logical order) by the face that sets
    /// them: (face, script row, start, end).
    fn script_pieces(
        &mut self,
        primary: fontdb::ID,
        text: &str,
        range: std::ops::Range<usize>,
    ) -> Result<Vec<(fontdb::ID, Option<usize>, usize, usize)>, String> {
        let chars: Vec<(usize, char)> = text[range.clone()]
            .char_indices()
            .map(|(i, c)| (i + range.start, c))
            .collect();
        let mut pieces: Vec<(fontdb::ID, Option<usize>, usize, usize)> = Vec::new();
        // The face for the stretch of one script being read, and where it ends.
        let mut stretch: Option<(usize, fontdb::ID)> = None;
        for (index, &(at, c)) in chars.iter().enumerate() {
            let end = at + c.len_utf8();
            let next = chars.get(index + 1).map(|&(_, n)| n);
            let row = self.script_row(c);
            let neutral = matches!(c.script(), Script::Common | Script::Inherited | Script::Unknown);
            let (face, face_row) = if let Some(e) = emoji_presentation(c, next).then(|| self.emoji_face(c)).flatten() {
                (e, None)
            } else if let Some(r) = row {
                let face = match stretch {
                    Some((until, face)) if index < until => face,
                    _ => {
                        // The stretch runs to the next letter of another script.
                        let until = chars[index..]
                            .iter()
                            .position(|&(_, n)| {
                                self.script_row(n) != Some(r)
                                    && !matches!(n.script(), Script::Common | Script::Inherited | Script::Unknown)
                            })
                            .map_or(chars.len(), |p| index + p);
                        // A font that has every letter of it sets it itself.
                        let mut own = true;
                        for &(_, n) in &chars[index..until] {
                            if self.script_row(n) == Some(r) && !self.has_glyph(primary, n) {
                                own = false;
                                break;
                            }
                        }
                        let face = if own { primary } else { self.script_face(primary, r)? };
                        stretch = Some((until, face));
                        face
                    }
                };
                // A letter even the stand-in lacks still gets a glyph.
                if self.has_glyph(face, c) {
                    (face, (face != primary).then_some(r))
                } else {
                    (self.fallback_for(c).unwrap_or(face), Some(r))
                }
            } else if neutral {
                // Spaces, digits, punctuation and marks stay with what they
                // are next to when its face can draw them.
                match pieces.last() {
                    Some(&(face, r, _, _))
                        if c.is_whitespace()
                            || c.is_control()
                            || attaches(c)
                            || c == '\u{200C}'
                            || self.has_glyph(face, c) =>
                    {
                        (face, r)
                    }
                    _ => {
                        let ahead = chars[index + 1..]
                            .iter()
                            .find(|&&(_, n)| {
                                !matches!(n.script(), Script::Common | Script::Inherited | Script::Unknown)
                            })
                            .and_then(|&(_, n)| self.script_row(n));
                        let candidate = match ahead {
                            Some(r) => Some((self.script_face(primary, r)?, r)),
                            None => None,
                        };
                        match candidate {
                            Some((face, r)) if self.has_glyph(face, c) && !self.has_glyph(primary, c) => {
                                (face, Some(r))
                            }
                            _ if c.is_whitespace() || c.is_control() || self.has_glyph(primary, c) => (primary, None),
                            _ => (self.fallback_for(c).unwrap_or(primary), None),
                        }
                    }
                }
            } else if self.has_glyph(primary, c) {
                (primary, None)
            } else {
                (self.fallback_for(c).unwrap_or(primary), None)
            };
            match pieces.last_mut() {
                Some(last) if last.0 == face && last.1 == face_row => last.3 = end,
                _ => pieces.push((face, face_row, at, end)),
            }
        }
        Ok(pieces)
    }

    fn shape(
        &mut self,
        id: fontdb::ID,
        text: &str,
        features: &[rustybuzz::Feature],
        vars: &[rustybuzz::Variation],
        direction: Option<rustybuzz::Direction>,
        language: Option<&str>,
    ) -> Result<Arc<ShapedLine>, String> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        text.hash(&mut h);
        if direction.is_some() || language.is_some() {
            format!("{direction:?}{language:?}").hash(&mut h);
        }
        for f in features {
            format!("{f:?}").hash(&mut h);
        }
        variations_key(vars).hash(&mut h);
        let key = h.finish();
        if let Some(s) = self.shaped.get(&key) {
            return Ok(s);
        }
        let (data, index) = self.face_data(id)?;
        let mut face = rustybuzz::Face::from_slice(&data, index).ok_or("unparseable font")?;
        if !vars.is_empty() {
            face.set_variations(vars);
        }
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        if let Some(direction) = direction {
            buf.set_direction(direction);
        }
        if let Some(language) = language.and_then(|l| l.parse().ok()) {
            buf.set_language(language);
        }
        let out = rustybuzz::shape(&face, features, buf);
        let mut pos: Vec<(i32, i32, i32)> = out
            .glyph_positions()
            .iter()
            .map(|p| (p.x_advance, p.x_offset, p.y_offset))
            .collect();
        let kerning = !features
            .iter()
            .any(|f| f.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"kern") && f.value == 0);
        if kerning {
            kerx_pairs(&face, out.glyph_infos(), &mut pos);
        }
        let mut pen = 0.0f32;
        let mut glyphs = Vec::with_capacity(out.len());
        for (info, &(adv, xo, yo)) in out.glyph_infos().iter().zip(&pos) {
            glyphs.push(ShapedGlyph {
                id: info.glyph_id as u16,
                x: pen + xo as f32,
                y: yo as f32,
                advance: adv as f32,
                cluster: info.cluster,
            });
            pen += adv as f32;
        }
        let line = Arc::new(ShapedLine { glyphs, advance: pen });
        self.shaped.insert(key, line.clone());
        Ok(line)
    }

    /// Glyph outline in font units, y up.
    fn glyph(&mut self, id: fontdb::ID, glyph: u16, vars: &[rustybuzz::Variation]) -> Result<Arc<BezPath>, String> {
        let vkey = variations_key(vars);
        if let Some(p) = self.glyphs.get(&(id, glyph, vkey)) {
            return Ok(p);
        }
        let (data, index) = self.face_data(id)?;
        let mut face = rustybuzz::Face::from_slice(&data, index).ok_or("unparseable font")?;
        if !vars.is_empty() {
            face.set_variations(vars);
        }
        struct B(BezPath);
        impl rustybuzz::ttf_parser::OutlineBuilder for B {
            fn move_to(&mut self, x: f32, y: f32) {
                self.0.move_to((x as f64, y as f64));
            }
            fn line_to(&mut self, x: f32, y: f32) {
                self.0.line_to((x as f64, y as f64));
            }
            fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
                self.0.quad_to((x1 as f64, y1 as f64), (x as f64, y as f64));
            }
            fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
                self.0
                    .curve_to((x1 as f64, y1 as f64), (x2 as f64, y2 as f64), (x as f64, y as f64));
            }
            fn close(&mut self) {
                self.0.close_path();
            }
        }
        let mut b = B(BezPath::new());
        face.outline_glyph(rustybuzz::ttf_parser::GlyphId(glyph), &mut b);
        let p = Arc::new(b.0);
        self.glyphs.insert((id, glyph, vkey), p.clone());
        Ok(p)
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

struct Reader<'a> {
    id: &'a str,
    node: &'a Node,
    t: f64,
}

impl Reader<'_> {
    fn vecn(&self, name: &str, default: &[f64]) -> ultrafast_graph::Vals {
        let mut v = self
            .node
            .params
            .get(name)
            .and_then(|p| p.eval_vals(self.t))
            .unwrap_or_else(|| ultrafast_graph::Vals::from_slice(default));
        // Broadcast a scalar to the expected width; pad or truncate otherwise.
        if v.len() == 1 && default.len() > 1 {
            v = ultrafast_graph::Vals::filled(v[0], default.len());
        }
        v.resize(default.len(), 0.0);
        v
    }
    fn num(&self, name: &str, default: f64) -> f64 {
        self.vecn(name, &[default])[0]
    }
    fn text(&self, name: &str) -> Option<&str> {
        text(self.node, name)
    }
    fn bad(&self, param: &str, reason: impl Into<String>) -> GeomError {
        GeomError::BadParam {
            node: self.id.to_string(),
            param: param.to_string(),
            reason: reason.into(),
        }
    }
}

/// The node's own placement: position, anchor, scale, rotation (degrees).
/// Unlike the `transform` op, geometry stays vector until rasterization.
fn placement(r: &Reader, default_anchor: [f64; 2]) -> Affine {
    let pos = r.vecn("position", &default_anchor);
    let anchor = r.vecn("anchor", &default_anchor);
    let scale = r.vecn("scale", &[1.0, 1.0]);
    let rot = r.num("rotation", 0.0).to_radians();
    Affine::translate(pos[0], pos[1])
        .mul(Affine::rotate(rot))
        .mul(Affine::scale(scale[0], scale[1]))
        .mul(Affine::translate(-anchor[0], -anchor[1]))
}

fn to_kurbo(a: Affine) -> kurbo::Affine {
    kurbo::Affine::new(a.0)
}

fn stroke_style(r: &Reader) -> Result<Option<kurbo::Stroke>, GeomError> {
    let width = r.num("stroke", 0.0);
    if width <= 0.0 {
        return Ok(None);
    }
    let join = match r.text("join").unwrap_or("miter") {
        "miter" => kurbo::Join::Miter,
        "round" => kurbo::Join::Round,
        "bevel" => kurbo::Join::Bevel,
        o => return Err(r.bad("join", format!("unknown join `{o}`"))),
    };
    let cap = match r.text("cap").unwrap_or("butt") {
        "butt" => kurbo::Cap::Butt,
        "round" => kurbo::Cap::Round,
        "square" => kurbo::Cap::Square,
        o => return Err(r.bad("cap", format!("unknown cap `{o}`"))),
    };
    let mut s = kurbo::Stroke::new(width).with_join(join).with_caps(cap);
    if let Some(dash) = r.node.params.get("dash").and_then(|p| p.eval(r.t))
        && !dash.is_empty()
        && dash.iter().any(|d| *d > 0.0)
    {
        s = s.with_dashes(r.num("dash_offset", 0.0), dash);
    }
    Ok(Some(s))
}

/// Keeps the part of `path` between fractions `a` and `b` of its length.
fn trim(path: &BezPath, a: f64, b: f64) -> BezPath {
    let (a, b) = (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    if a <= 0.0 && b >= 1.0 {
        return path.clone();
    }
    let segs: Vec<kurbo::PathSeg> = path.segments().collect();
    let lens: Vec<f64> = segs.iter().map(|s| s.arclen(1e-3)).collect();
    let total: f64 = lens.iter().sum();
    let (from, to) = (a * total, b * total);
    let mut out = BezPath::new();
    let mut acc = 0.0;
    let mut last_end: Option<Point> = None;
    for (seg, len) in segs.iter().zip(&lens) {
        let (s0, s1) = (acc, acc + len);
        acc = s1;
        if s1 <= from || s0 >= to || *len <= 0.0 {
            continue;
        }
        let t0 = if from > s0 {
            seg.inv_arclen(from - s0, 1e-4)
        } else {
            0.0
        };
        let t1 = if to < s1 { seg.inv_arclen(to - s0, 1e-4) } else { 1.0 };
        let sub = seg.subsegment(t0..t1);
        if last_end.is_none_or(|p| p.distance(sub.start()) > 1e-6) {
            out.move_to(sub.start());
        }
        match sub {
            kurbo::PathSeg::Line(l) => out.line_to(l.p1),
            kurbo::PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
            kurbo::PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
        }
        last_end = Some(sub.end());
    }
    out
}

fn preset_path(r: &Reader, kind: &str, fmt: &Format) -> Result<BezPath, GeomError> {
    let c = r.vecn("center", &[fmt.width as f64 / 2.0, fmt.height as f64 / 2.0]);
    let radius = r.num("radius", fmt.height as f64 / 4.0);
    let mut p = BezPath::new();
    match kind {
        "polygon" | "star" => {
            let sides = r.num("sides", if kind == "star" { 5.0 } else { 6.0 }).round().max(3.0) as usize;
            let inner = r.num("inner_radius", radius * 0.45);
            let n = if kind == "star" { sides * 2 } else { sides };
            for i in 0..n {
                let a = -std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::TAU / n as f64;
                let rr = if kind == "star" && i % 2 == 1 { inner } else { radius };
                let pt = (c[0] + rr * a.cos(), c[1] + rr * a.sin());
                if i == 0 {
                    p.move_to(pt);
                } else {
                    p.line_to(pt);
                }
            }
            p.close_path();
        }
        "circle" => p = kurbo::Circle::new((c[0], c[1]), radius).to_path(0.01),
        "line" => {
            let pts = r.vecn("points", &[0.0, 0.0, 100.0, 100.0]);
            p.move_to((pts[0], pts[1]));
            p.line_to((pts[2], pts[3]));
        }
        "polyline" | "polygon_points" => {
            let pts = r
                .node
                .params
                .get("points")
                .and_then(|p| p.eval(r.t))
                .unwrap_or_default();
            for (i, xy) in pts.chunks_exact(2).enumerate() {
                if i == 0 {
                    p.move_to((xy[0], xy[1]));
                } else {
                    p.line_to((xy[0], xy[1]));
                }
            }
            if kind == "polygon_points" {
                p.close_path();
            }
        }
        o => return Err(r.bad("kind", format!("unknown path kind `{o}`"))),
    }
    Ok(p)
}

/// Builds the geometry of a `path` node under `outer` (absorbed transforms).
pub fn path(id: &str, node: &Node, fmt: &Format, t: f64, outer: Affine) -> Result<Geometry, GeomError> {
    let r = Reader { id, node, t };
    let local = match (r.text("d"), r.text("kind")) {
        (Some(d), _) => BezPath::from_svg(d).map_err(|e| r.bad("d", format!("invalid SVG path data: {e}")))?,
        (None, Some(kind)) => preset_path(&r, kind, fmt)?,
        (None, None) => return Err(r.bad("d", "path needs `d` (SVG path data) or `kind`")),
    };
    // Shape morphing toward `to` (another path's data).
    let local = match r.text("to") {
        // Several targets (`|`-separated): `morph` runs 0..n through them.
        Some(to) => {
            // Only the two shapes `morph` is between are parsed.
            let n = to.matches('|').count() + 2;
            let m = r.num("morph", 0.0).clamp(0.0, (n - 1) as f64);
            let k = (m.floor() as usize).min(n - 2);
            let f = m - k as f64;
            let shape = |i: usize| -> Result<BezPath, GeomError> {
                if i == 0 {
                    return Ok(local.clone());
                }
                let part = to.split('|').nth(i - 1).unwrap_or("");
                BezPath::from_svg(part.trim()).map_err(|e| r.bad("to", format!("invalid SVG path data: {e}")))
            };
            if f <= 0.0 {
                shape(k)?
            } else {
                morph(&shape(k)?, &shape(k + 1)?, f)
            }
        }
        None => local,
    };
    let local = trim(&local, r.num("trim_start", 0.0), r.num("trim_end", 1.0));
    let xf = outer.mul(placement(&r, [0.0, 0.0]));
    let mut g = Geometry::empty();
    match stroke_style(&r)? {
        Some(style) => {
            // Stroke in local space so width scales with the transform.
            let outline = kurbo::stroke(local.iter(), &style, &kurbo::StrokeOpts::default(), TOLERANCE * 0.25);
            flatten_closed(&(to_kurbo(xf) * outline), 1.0, 0, &mut g);
        }
        None => flatten_closed(&(to_kurbo(xf) * local), 1.0, 0, &mut g),
    }
    Ok(g)
}

/// The variable-font instance for a `text` node: `weight` drives the
/// `wght` axis (so weight animates smoothly), the optical size axis follows
/// the type's size in px (as browsers do: display cuts for big type, sturdy
/// ones for small), `variations` sets axes by tag (`"wdth=80, opsz=144"`),
/// and `axis_<tag>` params set one axis each, animatable (`axis_wdth`,
/// `axis_SOFT`). Values are clamped to the font's ranges; static fonts get
/// none.
fn font_variations(
    r: &Reader,
    fonts: &mut Fonts,
    font: fontdb::ID,
    weight: f64,
    size: f64,
) -> Result<Variations, GeomError> {
    let axes = fonts.axes(font).map_err(|e| r.bad("font", e))?;
    if axes.is_empty() {
        return Ok(Vec::new());
    }
    let mut set: Vec<([u8; 4], f64)> = Vec::new();
    if axes.iter().any(|a| &a.0 == b"wght") {
        set.push((*b"wght", weight));
    }
    if axes.iter().any(|a| &a.0 == b"opsz") {
        set.push((*b"opsz", size));
    }
    let tag_of = |s: &str| -> Option<[u8; 4]> {
        let b = s.trim().as_bytes();
        (!b.is_empty() && b.len() <= 4).then(|| std::array::from_fn(|i| *b.get(i).unwrap_or(&b' ')))
    };
    if let Some(spec) = r.text("variations") {
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let (tag, value) = part
                .split_once('=')
                .and_then(|(t, v)| Some((tag_of(t)?, v.trim().parse::<f64>().ok()?)))
                .ok_or_else(|| r.bad("variations", format!("`{part}` is not tag=value")))?;
            set.push((tag, value));
        }
    }
    for (name, _) in r.node.params.iter().filter(|(k, _)| k.starts_with("axis_")) {
        let tag = tag_of(&name[5..]).ok_or_else(|| r.bad(name, "axis tags are 1-4 characters"))?;
        let def = axes.iter().find(|a| a.0 == tag).map(|a| a.2 as f64).unwrap_or(0.0);
        set.push((tag, r.num(name, def)));
    }
    let mut out: Variations = Vec::new();
    for (tag, value) in set {
        let Some(axis) = axes.iter().find(|a| a.0 == tag) else {
            continue;
        };
        let v = (value as f32).clamp(axis.1, axis.3);
        out.retain(|x| x.tag.to_bytes() != tag);
        out.push(rustybuzz::Variation {
            tag: rustybuzz::ttf_parser::Tag::from_bytes(&tag),
            value: v,
        });
    }
    Ok(out)
}

/// Text params evaluated per glyph at its staggered time.
const GLYPH_ANIMATORS: [&str; 4] = ["glyph_offset", "glyph_scale", "glyph_rotation", "glyph_opacity"];

/// Closed polylines of a path's subpaths.
fn polylines(p: &BezPath) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    kurbo::flatten(p.iter(), 0.2, |el| match el {
        PathEl::MoveTo(q) => {
            if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            cur.push(q);
        }
        PathEl::LineTo(q) => cur.push(q),
        PathEl::ClosePath => {
            if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            }
        }
        _ => {}
    });
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

fn signed_area(c: &[Point]) -> f64 {
    (0..c.len())
        .map(|i| {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            a.x * b.y - b.x * a.y
        })
        .sum::<f64>()
        / 2.0
}

/// `n` points evenly spaced along the closed polyline.
fn resample(c: &[Point], n: usize) -> Vec<Point> {
    let len: Vec<f64> = (0..c.len()).map(|i| c[i].distance(c[(i + 1) % c.len()])).collect();
    let total: f64 = len.iter().sum::<f64>().max(1e-9);
    let mut out = Vec::with_capacity(n);
    let (mut seg, mut acc) = (0usize, 0.0f64);
    for k in 0..n {
        let target = total * k as f64 / n as f64;
        while seg < c.len() - 1 && acc + len[seg] < target {
            acc += len[seg];
            seg += 1;
        }
        let u = if len[seg] > 0.0 {
            ((target - acc) / len[seg]).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(c[seg].lerp(c[(seg + 1) % c.len()], u));
    }
    out
}

/// Morphs `a` into `b` at `m` (0..1): contours are paired largest to
/// largest (missing partners grow from a point), resampled to matching
/// point counts, wound the same way and rotated to line up, then blended.
pub fn morph(a: &BezPath, b: &BezPath, m: f64) -> BezPath {
    if m <= 0.0 {
        return a.clone();
    }
    if m >= 1.0 {
        return b.clone();
    }
    // Paths built alike (same commands in the same order) interpolate point
    // for point: exact, and how Lottie and most editors morph.
    let (ea, eb) = (a.elements(), b.elements());
    if ea.len() == eb.len()
        && ea
            .iter()
            .zip(eb)
            .all(|(x, y)| std::mem::discriminant(x) == std::mem::discriminant(y))
    {
        let l = |p: Point, q: Point| p.lerp(q, m);
        let mut out = BezPath::new();
        for (x, y) in ea.iter().zip(eb) {
            match (x, y) {
                (PathEl::MoveTo(p), PathEl::MoveTo(q)) => out.move_to(l(*p, *q)),
                (PathEl::LineTo(p), PathEl::LineTo(q)) => out.line_to(l(*p, *q)),
                (PathEl::QuadTo(p1, p2), PathEl::QuadTo(q1, q2)) => out.quad_to(l(*p1, *q1), l(*p2, *q2)),
                (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => {
                    out.curve_to(l(*p1, *q1), l(*p2, *q2), l(*p3, *q3))
                }
                _ => out.close_path(),
            }
        }
        return out;
    }
    let by_size = |p: &BezPath| {
        let mut v = polylines(p);
        v.sort_by(|x, y| signed_area(y).abs().partial_cmp(&signed_area(x).abs()).unwrap());
        v
    };
    let (ca, cb) = (by_size(a), by_size(b));
    let centroid = |c: &[Point]| {
        let n = c.len().max(1) as f64;
        Point::new(
            c.iter().map(|p| p.x).sum::<f64>() / n,
            c.iter().map(|p| p.y).sum::<f64>() / n,
        )
    };
    let mut out = BezPath::new();
    for i in 0..ca.len().max(cb.len()) {
        let (pa, pb) = match (ca.get(i), cb.get(i)) {
            (Some(x), Some(y)) => (x.clone(), y.clone()),
            (Some(x), None) => (x.clone(), vec![centroid(x); 3]),
            (None, Some(y)) => (vec![centroid(y); 3], y.clone()),
            (None, None) => unreachable!(),
        };
        let perim = |c: &[Point]| (0..c.len()).map(|k| c[k].distance(c[(k + 1) % c.len()])).sum::<f64>();
        let n = ((perim(&pa).max(perim(&pb)) / 3.0) as usize).clamp(48, 600);
        let ra = resample(&pa, n);
        let mut rb = resample(&pb, n);
        if signed_area(&ra) * signed_area(&rb) < 0.0 {
            rb.reverse();
        }
        // The rotation of b that best lines up with a.
        let step = (n / 120).max(1);
        let best = (0..n)
            .step_by(step)
            .min_by(|&x, &y| {
                let cost = |o: usize| {
                    (0..n)
                        .step_by(step)
                        .map(|k| ra[k].distance_squared(rb[(k + o) % n]))
                        .sum::<f64>()
                };
                cost(x).partial_cmp(&cost(y)).unwrap()
            })
            .unwrap_or(0);
        for k in 0..n {
            let p = ra[k].lerp(rb[(k + best) % n], m);
            if k == 0 {
                out.move_to(p);
            } else {
                out.line_to(p);
            }
        }
        out.close_path();
    }
    out
}

/// Glyph outlines of a `text` node, positioned in output space (laid out,
/// revealed, stroked), each with its reveal weight.
/// Paths of a text node, each with its weight (reveal and glyph opacity)
/// and color group, and the colors of the groups (see [`Geometry::groups`]).
pub type TextPaths = (Vec<(BezPath, f32, u32)>, Vec<Option<[f32; 4]>>);

/// At most this many differently colored layers in one text node; more
/// colors share the last.
const MAX_GROUPS: usize = 16;

/// The style of a run of rich text, resolved to a face.
#[derive(Clone)]
struct RunStyle {
    font: fontdb::ID,
    size: f64,
    vars: Variations,
    /// Synthetic oblique (tangent of the slant) for an italic the face lacks.
    slant: f64,
    /// Linear straight alpha; None is the node's own color.
    color: Option<[f32; 4]>,
    highlight: Option<[f32; 4]>,
    underline: f64,
    underline_color: Option<[f32; 4]>,
    rise: f64,
    /// How much of the highlight and underline is drawn, from the left.
    draw: f64,
}

/// A style before resolution: what the node, the `em_*` params or a span
/// set (unset fields inherit).
#[derive(Clone, Default)]
struct StyleSpec {
    font: Option<String>,
    weight: Option<f64>,
    italic: Option<bool>,
    size: Option<f64>,
    color: Option<[f32; 4]>,
    highlight: Option<[f32; 4]>,
    underline: Option<f64>,
    underline_color: Option<[f32; 4]>,
    rise: Option<f64>,
    draw: Option<f64>,
}

impl StyleSpec {
    fn over(&self, o: &StyleSpec) -> StyleSpec {
        StyleSpec {
            font: o.font.clone().or_else(|| self.font.clone()),
            weight: o.weight.or(self.weight),
            italic: o.italic.or(self.italic),
            size: o.size.or(self.size),
            color: o.color.or(self.color),
            highlight: o.highlight.or(self.highlight),
            underline: o.underline.or(self.underline),
            underline_color: o.underline_color.or(self.underline_color),
            rise: o.rise.or(self.rise),
            draw: o.draw.or(self.draw),
        }
    }
}

/// Attributes a `[text]{...}` span takes.
pub const SPAN_ATTRS: &[&str] = &[
    "color",
    "font",
    "weight",
    "style",
    "size",
    "highlight",
    "underline",
    "underline_color",
    "rise",
    "draw",
];

fn linear_color(c: [f64; 4]) -> [f32; 4] {
    let l = |x: f64| crate::srgb_to_linear(x) as f32;
    [l(c[0]), l(c[1]), l(c[2]), c[3].clamp(0.0, 1.0) as f32]
}

impl Reader<'_> {
    /// A color param as linear straight alpha.
    fn color(&self, name: &str) -> Result<Option<[f32; 4]>, GeomError> {
        let c = match self.node.params.get(name) {
            None => return Ok(None),
            Some(Param::Text(s)) => crate::parse_hex(s)
                .ok_or_else(|| self.bad(name, format!("`{s}` is not a #rgb, #rrggbb or #rrggbbaa color")))?,
            Some(p) => {
                let v = p.eval(self.t).unwrap_or_default();
                match v.len() {
                    3 => [v[0], v[1], v[2], 1.0],
                    4 => [v[0], v[1], v[2], v[3]],
                    _ => return Err(self.bad(name, "color needs 3 or 4 components")),
                }
            }
        };
        Ok(Some(linear_color(c)))
    }
    fn opt(&self, name: &str) -> Option<f64> {
        self.node.params.contains_key(name).then(|| self.num(name, 0.0))
    }
}

/// The emphasis style of `*words*`: the `em_*` params, or italic when
/// none is set.
fn em_spec(r: &Reader) -> Result<StyleSpec, GeomError> {
    let set = r.node.params.keys().any(|k| k.starts_with("em_"));
    Ok(StyleSpec {
        font: r.text("em_font").map(String::from),
        weight: r.opt("em_weight"),
        italic: match r.text("em_style") {
            Some(s) => Some(s == "italic"),
            None if !set => Some(true),
            None => None,
        },
        size: r.opt("em_size"),
        color: r.color("em_color")?,
        highlight: r.color("em_highlight")?,
        underline: r.opt("em_underline"),
        underline_color: r.color("em_underline_color")?,
        rise: r.opt("em_rise"),
        draw: r.opt("em_draw"),
    })
}

fn attr_spec(r: &Reader, a: &std::collections::BTreeMap<String, String>) -> Result<StyleSpec, GeomError> {
    if let Some(k) = a.keys().find(|k| !SPAN_ATTRS.contains(&k.as_str())) {
        return Err(r.bad(
            "text",
            format!("unknown span attribute `{k}` ({})", SPAN_ATTRS.join(", ")),
        ));
    }
    let num = |k: &str| -> Result<Option<f64>, GeomError> {
        a.get(k)
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| r.bad("text", format!("span `{k}={v}` is not a number")))
            })
            .transpose()
    };
    let col = |k: &str| -> Result<Option<[f32; 4]>, GeomError> {
        a.get(k)
            .map(|v| {
                crate::parse_hex(v).map(linear_color).ok_or_else(|| {
                    r.bad(
                        "text",
                        format!("span `{k}={v}` is not a #rgb, #rrggbb or #rrggbbaa color"),
                    )
                })
            })
            .transpose()
    };
    Ok(StyleSpec {
        font: a.get("font").cloned(),
        weight: match a.get("weight").map(String::as_str) {
            Some("bold") => Some(700.0),
            Some("regular" | "normal") => Some(400.0),
            _ => num("weight")?,
        },
        italic: a.get("style").map(|s| s == "italic"),
        size: num("size")?,
        color: col("color")?,
        highlight: col("highlight")?,
        // A bare `underline` is a hairline sized to the type.
        underline: match a.get("underline").map(String::as_str) {
            Some("true") => Some(-1.0),
            _ => num("underline")?,
        },
        underline_color: col("underline_color")?,
        rise: num("rise")?,
        draw: num("draw")?,
    })
}

/// A text node's content as styled runs.
struct Rich {
    plain: String,
    /// (start, end, style) byte ranges of `plain`, in order.
    runs: Vec<(usize, usize, usize)>,
    /// Style 0 is the node's own.
    styles: Vec<RunStyle>,
    features: Vec<rustybuzz::Feature>,
    ascent: f64,
    descent: f64,
    size: f64,
}

impl Rich {
    /// Glyphs of `plain[a..b]` laid out on one line, with their styles.
    fn shape(&self, fonts: &mut Fonts, a: usize, b: usize) -> Result<(Vec<(LineGlyph, usize)>, f64), String> {
        let mut out: Vec<(LineGlyph, usize)> = Vec::new();
        let mut pen = 0.0;
        // Styled spans of a right-to-left line run from its right.
        let rtl = fonts.reads_rtl(&self.plain[a..b]);
        let order: Vec<&(usize, usize, usize)> = if rtl {
            self.runs.iter().rev().collect()
        } else {
            self.runs.iter().collect()
        };
        for &&(rs, re, si) in &order {
            let (s, e) = (rs.max(a), re.min(b));
            if s >= e {
                continue;
            }
            let st = &self.styles[si];
            let (glyphs, adv) = fonts.layout(st.font, &self.plain[s..e], st.size, &self.features, &st.vars)?;
            let (track, unit) = out.last().map_or((0, 0), |(g, _)| (g.track + 1, g.unit + 1));
            for mut g in glyphs {
                g.x += pen;
                g.cluster += s;
                g.track += track;
                g.unit += unit;
                out.push((g, si));
            }
            pen += adv;
        }
        Ok((out, pen))
    }
}

/// The text a node sets, with markup parsed into spans (`markup: false`
/// turns parsing off).
fn rich_content(r: &Reader) -> (String, Vec<crate::markup::Span>) {
    let raw = number_content(r).unwrap_or_else(|| r.text("text").unwrap_or("").to_string());
    let on = !matches!(r.node.params.get("markup"), Some(Param::Bool(false)));
    let (plain, mut spans) = if on && crate::markup::has_markup(&raw) {
        crate::markup::parse(&raw)
    } else {
        (raw, Vec::new())
    };
    let Some(p) = r
        .node
        .params
        .get("scramble")
        .and_then(|p| p.eval(r.t))
        .and_then(|v| v.first().copied())
    else {
        return (plain, spans);
    };
    // Scrambling swaps characters in place; spans follow them.
    let s = scramble(&plain, p, r);
    if !spans.is_empty() && s.len() != plain.len() {
        let bytes: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let at = |b: usize| bytes[plain[..b].chars().count()];
        for sp in &mut spans {
            (sp.start, sp.end) = (at(sp.start), at(sp.end));
        }
    }
    (s, spans)
}

/// What a `text` node shows at `t`, markup removed (for reading checks).
pub fn display_text(node: &Node, t: f64) -> String {
    rich_content(&Reader { id: "", node, t }).0
}

/// Resolves a node's styled runs: `r` reads styles, `content` the text
/// (counters read it on the frame).
fn rich(r: &Reader, content: &Reader, fonts: &mut Fonts, base: &Path) -> Result<Rich, GeomError> {
    let (plain, spans) = rich_content(content);
    let size = r.num("size", 64.0);
    let base_spec = StyleSpec {
        font: Some(r.text("font").unwrap_or("").to_string()),
        weight: Some(r.num("weight", 400.0)),
        italic: Some(r.text("style") == Some("italic")),
        size: Some(size),
        rise: Some(0.0),
        draw: Some(1.0),
        ..Default::default()
    };
    let em = if spans.iter().any(|s| s.style == crate::markup::Style::Em) {
        Some(em_spec(r)?)
    } else {
        None
    };
    let resolve = |spec: &StyleSpec, fonts: &mut Fonts| -> Result<RunStyle, GeomError> {
        let weight = spec.weight.unwrap_or(400.0).clamp(1.0, 1000.0);
        let italic = spec.italic.unwrap_or(false);
        let name = spec.font.as_deref().unwrap_or("");
        let field = if spec.font == base_spec.font { "font" } else { "text" };
        let font = fonts
            .resolve(name, weight as u16, italic, base)
            .map_err(|e| r.bad(field, e))?;
        let size = spec.size.unwrap_or(size);
        let vars = font_variations(r, fonts, font, weight, size)?;
        Ok(RunStyle {
            font,
            size,
            vars,
            slant: if italic && !fonts.is_italic(font) { 0.2 } else { 0.0 },
            color: spec.color,
            highlight: spec.highlight,
            underline: match spec.underline {
                Some(v) if v < 0.0 => (size * 0.06).max(1.0),
                Some(v) => v,
                None => 0.0,
            },
            underline_color: spec.underline_color,
            rise: spec.rise.unwrap_or(0.0),
            draw: spec.draw.unwrap_or(1.0).clamp(0.0, 1.0),
        })
    };
    let mut styles = vec![resolve(&base_spec, fonts)?];
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    if spans.is_empty() {
        runs.push((0, plain.len(), 0));
    } else {
        let mut cuts: Vec<usize> = spans.iter().flat_map(|s| [s.start, s.end]).collect();
        cuts.extend([0, plain.len()]);
        cuts.sort_unstable();
        cuts.dedup();
        let mut keys: Vec<Vec<usize>> = vec![Vec::new()];
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            // Spans covering this piece, outermost first.
            let mut cover: Vec<usize> = (0..spans.len())
                .filter(|&i| spans[i].start <= a && spans[i].end >= b)
                .collect();
            cover.sort_by_key(|&i| std::cmp::Reverse(spans[i].end - spans[i].start));
            let si = match keys.iter().position(|k| *k == cover) {
                Some(si) => si,
                None => {
                    let mut spec = base_spec.clone();
                    for &i in &cover {
                        let s = match &spans[i].style {
                            crate::markup::Style::Em => em.clone().unwrap_or_default(),
                            crate::markup::Style::Attrs(a) => attr_spec(r, a)?,
                        };
                        spec = spec.over(&s);
                    }
                    styles.push(resolve(&spec, fonts)?);
                    keys.push(cover);
                    keys.len() - 1
                }
            };
            match runs.last_mut() {
                Some(last) if last.2 == si && last.1 == a => last.1 = b,
                _ => runs.push((a, b, si)),
            }
        }
    }
    let m = fonts.metrics(styles[0].font).map_err(|e| r.bad("font", e))?;
    let scale = size / m.units_per_em as f64;
    Ok(Rich {
        plain,
        runs,
        styles,
        features: text_features(r),
        ascent: m.ascender as f64 * scale,
        descent: -m.descender as f64 * scale,
        size,
    })
}

pub fn text_paths(
    id: &str,
    node: &Node,
    fmt: &Format,
    t: f64,
    outer: Affine,
    fonts: &mut Fonts,
    base: &Path,
) -> Result<TextPaths, GeomError> {
    layout_text(id, node, fmt, t, outer, fonts, base).map(|(paths, _, _)| paths)
}

/// A text node's glyph outlines and colors ([`text_paths`]) and its color
/// glyphs (emoji bitmaps), under `outer`.
pub fn text_layout(
    id: &str,
    node: &Node,
    fmt: &Format,
    t: f64,
    outer: Affine,
    fonts: &mut Fonts,
    base: &Path,
) -> Result<(TextPaths, Vec<GlyphImage>), GeomError> {
    layout_text(id, node, fmt, t, outer, fonts, base).map(|(paths, images, _)| (paths, images))
}

/// [`text_layout`], and the glyph units laid out (what `reveal` counts).
fn layout_text(
    id: &str,
    node: &Node,
    fmt: &Format,
    t: f64,
    outer: Affine,
    fonts: &mut Fonts,
    base: &Path,
) -> Result<(TextPaths, Vec<GlyphImage>, usize), GeomError> {
    let r = Reader { id, node, t };
    // Counters change digits, they do not move: read the number on the
    // frame (motion-blur samples share it), so digits never smear.
    let on_frame = Reader {
        id,
        node,
        t: (t * fmt.fps).round() / fmt.fps,
    };
    let rich = rich(&r, &on_frame, fonts, base)?;
    let plain = rich.plain.as_str();
    let size = rich.size;
    let line_height = r.num("line_height", 1.2) * size * fonts.script_leading(plain);
    let tracking = r.num("tracking", 0.0);
    let wrap = r
        .node
        .params
        .get("width")
        .and_then(|p| p.eval(t))
        .and_then(|v| v.first().copied());

    // Break into lines (byte ranges of the plain text): explicit newlines,
    // then greedy wrapping at Unicode line-break opportunities when a width
    // is set.
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut para_start = 0;
    for para in plain.split('\n') {
        let p0 = para_start;
        para_start += para.len() + 1;
        let Some(width) = wrap else {
            lines.push((p0, p0 + para.len()));
            continue;
        };
        let mut start = 0;
        let mut last_ok: Option<usize> = None;
        for (pos, _) in unicode_linebreak::linebreaks(para) {
            let end = start + para[start..pos].trim_end().len();
            let (glyphs, adv) = rich.shape(fonts, p0 + start, p0 + end).map_err(|e| r.bad("text", e))?;
            let w = adv + tracking * glyphs.last().map_or(0, |g| g.0.track + 1) as f64;
            if w > width
                && let Some(ok) = last_ok
            {
                lines.push((p0 + start, p0 + start + para[start..ok].trim_end().len()));
                start = ok;
                last_ok = Some(pos);
                continue;
            }
            last_ok = Some(pos);
        }
        lines.push((p0 + start, p0 + start + para[start..].trim_end().len()));
    }

    let align = r.text("align").unwrap_or("left");
    let valign = r.text("valign").unwrap_or("top");
    let (ascent, descent) = (rich.ascent, rich.descent);
    let block_h = ascent + descent + line_height * (lines.len().max(1) - 1) as f64;
    let first_baseline = match valign {
        "top" => ascent,
        "baseline" => 0.0,
        "middle" => ascent - block_h / 2.0,
        "bottom" => ascent - block_h,
        o => return Err(r.bad("valign", format!("unknown valign `{o}`"))),
    };

    // Per-glyph reveal: glyph i has weight clamp((reveal - i) / softness).
    let total_glyphs: f64 = 1e9;
    let reveal = r.num("reveal", total_glyphs);
    let softness = r.num("reveal_softness", 1.0).max(1e-3);
    let reveal_offset = r.vecn("reveal_offset", &[0.0, 0.0]);

    let xf = outer.mul(placement(&r, [0.0, 0.0]));
    let style = stroke_style(&r)?;
    // Output pixels per layout pixel: color glyphs pick their bitmap strike
    // by the size they appear at.
    let screen_scale = (xf.0[0] * xf.0[3] - xf.0[1] * xf.0[2]).abs().sqrt().max(1e-6);
    let mut images: Vec<GlyphImage> = Vec::new();

    // Per-glyph animators: each glyph evaluates the glyph_* params at its
    // own time, t - stagger * (its place in stagger_order).
    let animated = GLYPH_ANIMATORS.iter().any(|p| r.node.params.contains_key(*p));
    let stagger = r.num("stagger", 0.0);
    let order_mode = r.text("stagger_order").unwrap_or("forward");
    let total = if animated {
        let mut n = 0;
        for &(a, b) in &lines {
            let shaped = rich.shape(fonts, a, b).map_err(|e| r.bad("text", e))?.0;
            n += shaped.iter().map(|g| g.0.unit as usize + 1).max().unwrap_or(0);
        }
        n
    } else {
        0
    };
    let order = |g: usize| -> f64 {
        match order_mode {
            "reverse" => (total.saturating_sub(1) - g.min(total.saturating_sub(1))) as f64,
            "center" => (g as f64 - (total as f64 - 1.0) / 2.0).abs(),
            "random" => {
                let mut x = (g as u32 ^ 0x9E37_79B9).wrapping_mul(0x85EB_CA6B);
                x ^= x >> 13;
                x = x.wrapping_mul(0xC2B2_AE35);
                x ^= x >> 16;
                (x as f64 / u32::MAX as f64 * total as f64).floor()
            }
            _ => g as f64,
        }
    };
    let at = |name: &str, tk: f64, default: &[f64]| -> Vec<f64> {
        let mut v = r
            .node
            .params
            .get(name)
            .and_then(|p| p.eval(tk))
            .unwrap_or_else(|| default.to_vec());
        if v.len() == 1 && default.len() > 1 {
            v = vec![v[0]; default.len()];
        }
        v.resize(default.len(), 0.0);
        v
    };

    // Text on a path: each glyph's center rides the path (its baseline on
    // the path for valign baseline), turned to the path's direction.
    let on_path = match r.text("on_path") {
        Some(d) => Some(ArcPath::new(
            &BezPath::from_svg(d).map_err(|e| r.bad("on_path", format!("invalid SVG path data: {e}")))?,
        )),
        None => None,
    };
    let path_offset = r.num("path_offset", 0.0);

    // Lay out every line first: marks (highlights, underlines) sweep
    // across lines in reading order.
    let mut laid: Vec<(Vec<(LineGlyph, usize)>, f64, f64)> = Vec::with_capacity(lines.len());
    for (li, &(la, lb)) in lines.iter().enumerate() {
        let (glyphs, adv) = rich.shape(fonts, la, lb).map_err(|e| r.bad("text", e))?;
        let width = adv + tracking * glyphs.last().map_or(0, |g| g.0.track) as f64;
        let x0 = match (align, &on_path) {
            ("left", None) => 0.0,
            ("center", None) => -width / 2.0,
            ("right", None) => -width,
            // Along a path, alignment is to the path's start, middle or end.
            ("left", Some(_)) => 0.0,
            ("center", Some(p)) => (p.length - width) / 2.0,
            ("right", Some(p)) => p.length - width,
            (o, _) => return Err(r.bad("align", format!("unknown align `{o}`"))),
        } + if on_path.is_some() { path_offset } else { 0.0 };
        laid.push((glyphs, x0, first_baseline + li as f64 * line_height));
    }

    // Marks: runs of consecutive glyphs whose style highlights or
    // underlines, as (style, x from, x to, baseline).
    let mut marks: Vec<(usize, f64, f64, f64)> = Vec::new();
    if on_path.is_none() {
        for (glyphs, x0, baseline) in &laid {
            let mut prev: Option<usize> = None;
            for (gl, si) in glyphs {
                let st = &rich.styles[*si];
                if st.highlight.is_none() && st.underline <= 0.0 {
                    prev = None;
                    continue;
                }
                let gx = x0 + gl.x + tracking * gl.track as f64;
                match marks.last_mut() {
                    Some(m) if prev == Some(*si) => m.2 = gx + gl.advance,
                    _ => marks.push((*si, gx, gx + gl.advance, *baseline)),
                }
                prev = Some(*si);
            }
        }
    }
    // `draw` is the share of a style's marks drawn, as one stroke.
    let mut length: HashMap<usize, (f64, f64)> = HashMap::new();
    for &(si, xa, xb, _) in &marks {
        length.entry(si).or_default().0 += xb - xa;
    }

    // Highlights go behind the type; glyphs and underlines in front.
    let mut back: Vec<(BezPath, [f32; 4])> = Vec::new();
    let mut front: Vec<(BezPath, f32, Option<[f32; 4]>)> = Vec::new();
    for &(si, xa, xb, baseline) in &marks {
        let st = &rich.styles[si];
        let (total, done) = length[&si];
        let d = ((st.draw * total - done) / (xb - xa).max(1e-9)).clamp(0.0, 1.0);
        length.get_mut(&si).unwrap().1 += xb - xa;
        if d <= 0.0 {
            continue;
        }
        let y = baseline - st.rise;
        if let Some(c) = st.highlight {
            let pad = st.size * 0.12;
            let (a, b) = (xa - pad, xb + pad);
            let rect =
                kurbo::RoundedRect::new(a, y - st.size * 0.8, a + (b - a) * d, y + st.size * 0.22, st.size * 0.1);
            back.push((to_kurbo(xf) * kurbo::Shape::to_path(&rect, 0.05), c));
        }
        if st.underline > 0.0 {
            let (th, uy) = (st.underline, y + st.size * 0.13);
            let rect = kurbo::RoundedRect::new(xa, uy - th / 2.0, xa + (xb - xa) * d, uy + th / 2.0, th / 2.0);
            front.push((
                to_kurbo(xf) * kurbo::Shape::to_path(&rect, 0.05),
                1.0,
                st.underline_color.or(st.color),
            ));
        }
    }

    // Glyphs reveal and animate by their unit: the glyph itself in Latin text.
    let mut unit_base = 0usize;
    for (glyphs, x0, baseline) in &laid {
        let (x0, baseline) = (*x0, *baseline);
        let units = glyphs.iter().map(|g| g.0.unit as usize + 1).max().unwrap_or(0);
        // The span of a unit of several glyphs: it turns about its middle.
        let mut spans: HashMap<u32, (f64, f64, u32)> = HashMap::new();
        if animated {
            for (gl, _) in glyphs {
                let x = x0 + gl.x + tracking * gl.track as f64;
                let e = spans.entry(gl.unit).or_insert((x, x, 0));
                (e.0, e.1, e.2) = (e.0.min(x), e.1.max(x + gl.advance), e.2 + 1);
            }
        }
        for (gl, si) in glyphs {
            let st = &rich.styles[*si];
            let gi = unit_base + gl.unit as usize;
            let w = ((reveal - gi as f64) / softness).clamp(0.0, 1.0);
            if w <= 0.0 {
                continue;
            }
            let gv: &[rustybuzz::Variation] = if gl.font == st.font { &st.vars } else { &[] };
            let outline = fonts.glyph(gl.font, gl.id, gv).map_err(|e| r.bad("font", e))?;
            // Color glyphs (emoji) are drawn from their bitmaps (their
            // outlines, if any, are placeholders).
            let bitmap = if fonts.has_color_bitmaps(gl.font) {
                fonts.glyph_bitmap(gl.font, gl.id, st.size * screen_scale)
            } else {
                None
            };
            if bitmap.is_none() && outline.elements().is_empty() {
                continue;
            }
            let scale = gl.scale;
            let ox = x0 + gl.x + tracking * gl.track as f64 + reveal_offset[0] * (1.0 - w);
            let oy = baseline - gl.y - st.rise + reveal_offset[1] * (1.0 - w);
            let (anim, w) = if animated {
                let tk = r.t - stagger * order(gi);
                let off = at("glyph_offset", tk, &[0.0, 0.0]);
                let sc = at("glyph_scale", tk, &[1.0, 1.0]);
                let rot = at("glyph_rotation", tk, &[0.0])[0].to_radians();
                let op = at("glyph_opacity", tk, &[1.0])[0].clamp(0.0, 1.0);
                // About the glyph's own ink center.
                let bb = match &bitmap {
                    Some(b) => {
                        kurbo::Rect::new(b.place[0], b.place[1], b.place[0] + b.place[2], b.place[1] + b.place[3])
                    }
                    None => outline.bounding_box(),
                };
                let (cx, cy) = match spans.get(&gl.unit) {
                    Some(&(a, b, n)) if n > 1 => ((a + b) / 2.0, baseline - st.rise - st.size * 0.35),
                    _ => (ox + (bb.x0 + bb.x1) / 2.0 * scale, oy - (bb.y0 + bb.y1) / 2.0 * scale),
                };
                let a = Affine::translate(cx + off[0], cy + off[1])
                    .mul(Affine::rotate(rot))
                    .mul(Affine::scale(sc[0], sc[1]))
                    .mul(Affine::translate(-cx, -cy));
                (a, w * op)
            } else {
                (Affine::IDENTITY, w)
            };
            if w <= 0.0 {
                continue;
            }
            let xf = match &on_path {
                Some(p) => {
                    let c = x0 + gl.x + tracking * gl.track as f64 + gl.advance / 2.0;
                    let ((px, py), angle) = p.at(c);
                    xf.mul(Affine::translate(px, py))
                        .mul(Affine::rotate(angle))
                        .mul(Affine::translate(-c, 0.0))
                }
                None => xf,
            };
            let xf = xf.mul(anim);
            // Font units are y-up; flip into pixel space (slanted for a
            // synthetic italic).
            let slant = if gl.upright { 0.0 } else { st.slant };
            let local = Affine([scale, 0.0, slant * scale, -scale, 0.0, 0.0]);
            if let Some(b) = bitmap {
                // The bitmap's unit square onto its place in font units.
                let p = b.place;
                let m = xf.mul(Affine::translate(ox, oy)).mul(local).mul(Affine([
                    p[2],
                    0.0,
                    0.0,
                    -p[3],
                    p[0],
                    p[1] + p[3],
                ]));
                images.push(GlyphImage {
                    bitmap: b,
                    xf: m.0,
                    weight: w as f32,
                });
                continue;
            }
            match &style {
                Some(s) => {
                    let unit = kurbo::stroke(
                        (to_kurbo(local) * (*outline).clone()).iter(),
                        s,
                        &kurbo::StrokeOpts::default(),
                        TOLERANCE * 0.25,
                    );
                    let place = xf.mul(Affine::translate(ox, oy));
                    front.push((to_kurbo(place) * unit, w as f32, st.color));
                }
                None => {
                    let glyph_xf = xf.mul(Affine::translate(ox, oy)).mul(local);
                    front.push((to_kurbo(glyph_xf) * (*outline).clone(), w as f32, st.color));
                }
            }
        }
        unit_base += units;
    }

    // Color groups: every highlight color, then every type color, each
    // composited over the ones before.
    let mut groups: Vec<Option<[f32; 4]>> = Vec::new();
    let group_of = |c: Option<[f32; 4]>, from: usize, groups: &mut Vec<Option<[f32; 4]>>| -> u32 {
        match groups[from..].iter().position(|g| *g == c) {
            Some(i) => (from + i) as u32,
            None if groups.len() < MAX_GROUPS => {
                groups.push(c);
                (groups.len() - 1) as u32
            }
            None => (MAX_GROUPS - 1) as u32,
        }
    };
    let mut out = Vec::with_capacity(back.len() + front.len());
    for (p, c) in back {
        let g = group_of(Some(c), 0, &mut groups);
        out.push((p, 1.0, g));
    }
    let first_front = groups.len();
    for (p, w, c) in front {
        let g = group_of(c, first_front, &mut groups);
        out.push((p, w, g));
    }
    if groups.len() == 1 && groups[0].is_none() {
        groups.clear();
    }
    Ok(((out, groups), images, unit_base))
}

/// Decoding text: characters settle into place in a shuffled order as
/// `scramble` goes 0 to 1; the rest cycle through `scramble_chars`, a new
/// pick every other frame. Spaces and line breaks keep their place.
fn scramble(content: &str, progress: f64, r: &Reader) -> String {
    let chars: Vec<char> = r
        .text("scramble_chars")
        .unwrap_or("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789#%&*+/<>=?")
        .chars()
        .collect();
    if chars.is_empty() || progress >= 1.0 {
        return content.to_string();
    }
    let hash = |a: u64, b: u64| {
        let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^ (x >> 32)
    };
    let n = content.chars().filter(|c| !c.is_whitespace()).count().max(1);
    let tick = (r.t * 15.0).floor() as u64;
    let mut k = 0;
    content
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if c.is_whitespace() {
                return c;
            }
            k += 1;
            // Each character settles at its own point of the progress.
            let order = (hash(i as u64, 7) % 10_000) as f64 / 10_000.0 * 0.6 + (k as f64 / n as f64) * 0.4;
            if order < progress {
                c
            } else {
                chars[(hash(i as u64, tick) % chars.len() as u64) as usize]
            }
        })
        .collect()
}

/// With `number` (animatable), the text is that value formatted with
/// `decimals`, `thousands` separator, `prefix` and `suffix`, for counters.
fn number_content(r: &Reader) -> Option<String> {
    let v = r
        .node
        .params
        .get("number")
        .and_then(|p| p.eval(r.t))
        .and_then(|v| v.first().copied())?;
    let decimals = r.num("decimals", 0.0).clamp(0.0, 8.0) as usize;
    let s = format!("{:.*}", decimals, v.abs());
    let (int, frac) = s
        .split_once('.')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((s.as_str(), None));
    let sep = r.text("thousands").unwrap_or(",");
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push_str(sep);
        }
        grouped.push(c);
    }
    let sign = if v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        "-"
    } else {
        ""
    };
    Some(format!(
        "{}{sign}{grouped}{}{}",
        r.text("prefix").unwrap_or(""),
        frac.map(|f| format!(".{f}")).unwrap_or_default(),
        r.text("suffix").unwrap_or("")
    ))
}

/// OpenType features: the node's own, plus tabular figures for counters so
/// digits do not jitter as they change.
fn text_features(r: &Reader) -> Vec<rustybuzz::Feature> {
    let mut f: Vec<rustybuzz::Feature> = match r.text("features") {
        Some(f) => f.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
        None => Vec::new(),
    };
    if (r.node.params.contains_key("number") || r.node.params.contains_key("scramble"))
        && !r.text("features").is_some_and(|s| s.contains("tnum"))
        && let Ok(t) = "tnum".parse()
    {
        f.push(t);
    }
    f
}

/// A path measured by arc length, for text that follows it.
struct ArcPath {
    pts: Vec<(f64, f64)>,
    cum: Vec<f64>,
    length: f64,
    closed: bool,
}

impl ArcPath {
    fn new(p: &BezPath) -> Self {
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let mut closed = false;
        // Text follows the first subpath only.
        let mut done = false;
        kurbo::flatten(p.iter(), 0.05, |el| match el {
            _ if done => {}
            kurbo::PathEl::MoveTo(q) => {
                if pts.is_empty() {
                    pts.push((q.x, q.y));
                } else {
                    done = true;
                }
            }
            kurbo::PathEl::LineTo(q) => pts.push((q.x, q.y)),
            kurbo::PathEl::ClosePath => {
                closed = true;
                done = true;
            }
            _ => {}
        });
        if closed && pts.len() > 1 && pts.first() != pts.last() {
            pts.push(pts[0]);
        }
        if pts.len() < 2 {
            pts = vec![(0.0, 0.0), (1.0, 0.0)];
        }
        let mut cum = vec![0.0];
        for w in pts.windows(2) {
            let d = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
            cum.push(cum.last().unwrap() + d);
        }
        let length = cum.last().copied().unwrap_or(0.0).max(1e-9);
        ArcPath {
            pts,
            cum,
            length,
            closed,
        }
    }

    /// Point and direction (radians) at arc length `s`. Closed paths wrap;
    /// open ones continue straight past their ends.
    fn at(&self, s: f64) -> ((f64, f64), f64) {
        let s = if self.closed { s.rem_euclid(self.length) } else { s };
        let i = match self.cum.binary_search_by(|c| c.total_cmp(&s)) {
            Ok(i) => i.min(self.pts.len() - 2),
            Err(i) => i.saturating_sub(1).min(self.pts.len() - 2),
        };
        let (a, b) = (self.pts[i], self.pts[i + 1]);
        let seg = (self.cum[i + 1] - self.cum[i]).max(1e-9);
        let u = (s - self.cum[i]) / seg;
        let angle = (b.1 - a.1).atan2(b.0 - a.0);
        ((a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u), angle)
    }
}

/// One glyph of a measured line, in pixels from the baseline origin (before
/// alignment and placement).
#[derive(Debug, Clone, serde::Serialize)]
pub struct GlyphBox {
    /// The text this glyph sets (a ligature covers several characters).
    pub text: String,
    /// Pen position and advance, tracking included.
    pub x: f64,
    pub advance: f64,
    /// Ink box [x0, y0, x1, y1] (y down; None for spaces).
    pub ink: Option<[f64; 4]>,
}

/// Line metrics of a single-line `text` node: glyph boxes, total advance,
/// ascent and descent (pixels). For agents laying out type by hand, such as
/// one 3D mesh per letter.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LineMetrics {
    pub glyphs: Vec<GlyphBox>,
    pub advance: f64,
    pub ascent: f64,
    pub descent: f64,
}

pub fn measure(id: &str, node: &Node, t: f64, fonts: &mut Fonts, base: &Path) -> Result<LineMetrics, GeomError> {
    let r = Reader { id, node, t };
    let rich = rich(&r, &r, fonts, base)?;
    let content = rich.plain.as_str();
    let tracking = r.num("tracking", 0.0);
    let (laid, adv) = rich.shape(fonts, 0, content.len()).map_err(|e| r.bad("text", e))?;
    let mut glyphs = Vec::new();
    for (gl, si) in laid.iter() {
        let st = &rich.styles[*si];
        let start = gl.cluster;
        let end = laid
            .iter()
            .map(|g| g.0.cluster)
            .filter(|&c| c > start)
            .min()
            .unwrap_or(content.len());
        let x = gl.x + tracking * gl.track as f64;
        let gv: &[rustybuzz::Variation] = if gl.font == st.font { &st.vars } else { &[] };
        let outline = fonts.glyph(gl.font, gl.id, gv).map_err(|e| r.bad("font", e))?;
        let ink = (!outline.elements().is_empty()).then(|| {
            let b = outline.bounding_box();
            let s = gl.scale;
            let y = gl.y + st.rise;
            [x + b.x0 * s, -b.y1 * s - y, x + b.x1 * s, -b.y0 * s - y]
        });
        glyphs.push(GlyphBox {
            text: content.get(start..end).unwrap_or("").to_string(),
            x,
            advance: gl.advance + tracking,
            ink,
        });
    }
    Ok(LineMetrics {
        advance: adv + tracking * laid.last().map_or(0, |g| g.0.track) as f64,
        ascent: rich.ascent,
        descent: rich.descent,
        glyphs,
    })
}

/// Builds the geometry of a `text` node under `outer`.
pub fn text_node(
    id: &str,
    node: &Node,
    fmt: &Format,
    t: f64,
    outer: Affine,
    fonts: &mut Fonts,
    base: &Path,
) -> Result<Geometry, GeomError> {
    let mut g = Geometry::empty();
    let ((paths, groups), images, units) = layout_text(id, node, fmt, t, outer, fonts, base)?;
    for (path, w, group) in paths {
        flatten_closed(&path, w, group, &mut g);
    }
    g.groups = groups;
    g.units = units;
    for im in images {
        g.push_image(im);
    }
    Ok(g)
}

/// The node's own placement affine at `t` (position, anchor, scale,
/// rotation), as text and path geometry apply it.
pub fn placement_affine(node: &Node, t: f64) -> Affine {
    placement(&Reader { id: "", node, t }, [0.0, 0.0])
}

/// Hash of what a geometry's shape depends on apart from where it is
/// placed: everything but `position` and the outer transform. Two
/// geometries with equal content keys and the same linear transform differ
/// only by a translation.
pub fn content_key(node: &Node, t: f64) -> u64 {
    let mut n = node.clone();
    n.params.remove("position");
    geometry_key(&n, t, &Affine::IDENTITY)
}

impl Geometry {
    /// The same geometry moved by (dx, dy).
    /// The geometry mapped through the affine `m` (output px to output px):
    /// type that turns or scales but keeps its shape.
    pub fn transformed(&self, m: [f64; 6]) -> Geometry {
        let map = |x: f32, y: f32| {
            let (x, y) = (x as f64, y as f64);
            ((m[0] * x + m[2] * y + m[4]) as f32, (m[1] * x + m[3] * y + m[5]) as f32)
        };
        let mut out = Geometry::empty();
        out.segments = self.segments.clone();
        for s in out.segments.chunks_exact_mut(8) {
            let (ax, ay) = map(s[0], s[1]);
            let (bx, by) = map(s[2], s[3]);
            s[..4].copy_from_slice(&[ax, ay, bx, by]);
            out.bounds = [
                out.bounds[0].min(ax.min(bx)),
                out.bounds[1].min(ay.min(by)),
                out.bounds[2].max(ax.max(bx)),
                out.bounds[3].max(ay.max(by)),
            ];
        }
        out.groups = self.groups.clone();
        out.units = self.units;
        for im in &self.images {
            let x = im.xf;
            let mut im = im.clone();
            im.xf = [
                m[0] * x[0] + m[2] * x[1],
                m[1] * x[0] + m[3] * x[1],
                m[0] * x[2] + m[2] * x[3],
                m[1] * x[2] + m[3] * x[3],
                m[0] * x[4] + m[2] * x[5] + m[4],
                m[1] * x[4] + m[3] * x[5] + m[5],
            ];
            out.push_image(im);
        }
        out
    }

    pub fn translated(&self, dx: f64, dy: f64) -> Geometry {
        let (dx, dy) = (dx as f32, dy as f32);
        let mut segments = self.segments.clone();
        for s in segments.chunks_exact_mut(8) {
            s[0] += dx;
            s[1] += dy;
            s[2] += dx;
            s[3] += dy;
        }
        let b = self.bounds;
        let images = self
            .images
            .iter()
            .map(|im| {
                let mut im = im.clone();
                im.xf[4] += dx as f64;
                im.xf[5] += dy as f64;
                im
            })
            .collect();
        Geometry {
            segments,
            bounds: [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy],
            groups: self.groups.clone(),
            images,
            units: self.units,
        }
    }
}

/// Stable hash of everything a geometry depends on, for caching.
pub fn geometry_key(node: &Node, t: f64, outer: &Affine) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    node.op.hash(&mut h);
    for (k, p) in &node.params {
        k.hash(&mut h);
        match p {
            Param::Text(s) => s.hash(&mut h),
            p => {
                for v in p.eval(t).unwrap_or_default() {
                    v.to_bits().hash(&mut h);
                }
            }
        }
    }
    for v in outer.0 {
        v.to_bits().hash(&mut h);
    }
    // Scrambling text changes characters on its own clock until it is done.
    if node
        .params
        .get("scramble")
        .and_then(|p| p.eval(t))
        .is_some_and(|v| v.first().is_some_and(|&p| p < 1.0))
    {
        ((t * 15.0).floor() as i64).hash(&mut h);
    }
    // Staggered glyph animators read their params at shifted times, so the
    // geometry can change while the values at `t` hold still.
    let staggered = node
        .params
        .get("stagger")
        .and_then(|p| p.eval(t))
        .is_some_and(|v| v.first() != Some(&0.0));
    if staggered
        && GLYPH_ANIMATORS
            .iter()
            .any(|p| node.params.get(*p).is_some_and(Param::is_animated))
    {
        t.to_bits().hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(json: &str) -> Node {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn square_flattens_to_four_segments() {
        let n = node(r#"{"op":"path","params":{"d":"M10 10 H 20 V 20 H 10 Z"}}"#);
        let fmt = Format {
            width: 64,
            height: 64,
            fps: 30.0,
            duration: 1.0,
            motion_blur: None,
        };
        let g = path("p", &n, &fmt, 0.0, Affine::IDENTITY).unwrap();
        assert_eq!(g.count(), 4);
        assert_eq!(g.bounds, [10.0, 10.0, 20.0, 20.0]);
    }

    #[test]
    fn trim_halves_length() {
        let p = BezPath::from_svg("M0 0 L100 0").unwrap();
        let t = trim(&p, 0.25, 0.75);
        let segs: Vec<_> = t.segments().collect();
        assert_eq!(segs.len(), 1);
        assert!((segs[0].start().x - 25.0).abs() < 1e-6 && (segs[0].end().x - 75.0).abs() < 1e-6);
    }

    #[test]
    fn text_shapes_and_lays_out() {
        let n = node(r#"{"op":"text","params":{"text":"Hi there","size":40,"position":[10,10]}}"#);
        let fmt = Format {
            width: 400,
            height: 100,
            fps: 30.0,
            duration: 1.0,
            motion_blur: None,
        };
        let mut fonts = Fonts::new();
        let g = text_node("t", &n, &fmt, 0.0, Affine::IDENTITY, &mut fonts, Path::new(".")).unwrap();
        assert!(g.count() > 20, "{}", g.count());
        // Every letter and the space: what `reveal` counts.
        assert_eq!(g.units, 8);
        assert!(
            g.bounds[0] >= 10.0 && g.bounds[1] >= 10.0 && g.bounds[2] < 400.0,
            "{:?}",
            g.bounds
        );
    }
}

#[cfg(test)]
mod emoji_tests {
    use super::*;

    #[test]
    fn color_emoji_have_bitmaps() {
        let mut f = Fonts::new();
        let Some(face) = f.emoji_face('\u{1F525}') else {
            eprintln!("no color emoji face installed");
            return;
        };
        if !f.has_color_bitmaps(face) {
            // Windows' Segoe UI Emoji draws color with COLR layers, not
            // bitmaps: its emoji come out as plain outlines for now.
            eprintln!("the installed emoji face has no color bitmaps");
            return;
        }
        let (data, index) = f.face_data(face).unwrap();
        let tf = rustybuzz::ttf_parser::Face::parse(&data, index).unwrap();
        let gid = tf.glyph_index('\u{1F525}').unwrap();
        eprintln!(
            "face {:?} glyph {:?} tables sbix {} cbdt {}",
            f.db().face(face).map(|x| x.families.clone()),
            gid,
            tf.tables().sbix.is_some(),
            tf.tables().cbdt.is_some()
        );
        let raw = tf.glyph_raster_image(gid, 80);
        eprintln!(
            "raw: {:?}",
            raw.map(|r| (r.x, r.y, r.width, r.height, r.pixels_per_em, r.format))
        );
        let b = f.glyph_bitmap(face, gid.0, 80.0);
        eprintln!("bitmap: {:?}", b.as_ref().map(|b| (b.width, b.height, b.place)));
        assert!(b.is_some());
    }
}

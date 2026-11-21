//! Which bytes of an MP4 or MOV file a render reads: the top-level box
//! headers, the `moov` index and, per track, the samples inside the time
//! windows it uses (video from the keyframe before the first frame it shows,
//! a few samples past the last for frame reordering). A file is fetched as a
//! sparse local copy holding just those bytes at their own offsets, so every
//! decoder reads it like the whole file.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

/// Seconds fetched either side of a window.
pub const MARGIN_S: f64 = 1.0;
/// Samples fetched past a video window's last (B-frame reordering, the
/// decoder's read-ahead).
const REORDER: usize = 12;
/// Ranges closer than this are fetched as one.
const MERGE_GAP: u64 = 512 * 1024;
/// The start of the file is always fetched (probing reads the first
/// samples of every track).
pub const HEAD: u64 = 8 << 20;

/// A top-level box: type, offset, size.
#[derive(Debug, Clone, PartialEq)]
pub struct TopBox {
    pub kind: [u8; 4],
    pub offset: u64,
    pub size: u64,
}

/// Walks the top-level boxes of a file of `len` bytes, reading 16-byte
/// headers through `read_at(offset, len)`.
pub fn top_boxes(
    len: u64,
    mut read_at: impl FnMut(u64, u64) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Vec<TopBox>> {
    let mut out = Vec::new();
    let mut at = 0u64;
    while at + 8 <= len {
        let h = read_at(at, 16.min(len - at))?;
        if h.len() < 8 {
            anyhow::bail!("truncated box header at {at}");
        }
        let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let kind = [h[4], h[5], h[6], h[7]];
        let size = match size32 {
            0 => len - at,
            1 => {
                if h.len() < 16 {
                    anyhow::bail!("truncated large box header at {at}");
                }
                u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]])
            }
            s => s,
        };
        if size < 8 || at + size > len {
            anyhow::bail!(
                "bad box `{}` at {at}: size {size} in a file of {len}",
                String::from_utf8_lossy(&kind)
            );
        }
        out.push(TopBox { kind, offset: at, size });
        at += size;
        if out.len() > 4096 {
            anyhow::bail!("too many top-level boxes");
        }
    }
    Ok(out)
}

/// One track's samples: byte range, time (seconds from its first
/// displayed sample) and whether decoding may start there.
pub struct Samples {
    pub video: bool,
    pub offsets: Vec<u64>,
    pub sizes: Vec<u32>,
    pub times: Vec<f64>,
    pub sync: Vec<bool>,
}

/// The sample tables of every video and audio track of a parsed file.
pub fn tracks<R: Read + Seek>(mp4: &mp4::Mp4Reader<R>) -> anyhow::Result<Vec<Samples>> {
    let mut out = Vec::new();
    for t in mp4.tracks().values() {
        let trak = &t.trak;
        let handler = trak.mdia.hdlr.handler_type.to_string();
        let video = handler == "vide";
        if !video && handler != "soun" {
            continue;
        }
        let stbl = &trak.mdia.minf.stbl;
        let n = stbl.stsz.sample_count as usize;
        if n == 0 {
            continue;
        }
        let timescale = trak.mdia.mdhd.timescale.max(1) as f64;
        // Times: stts deltas plus ctts offsets, from the first displayed.
        let mut ticks = Vec::with_capacity(n);
        let mut dts: i64 = 0;
        for e in &stbl.stts.entries {
            for _ in 0..e.sample_count {
                if ticks.len() < n {
                    ticks.push(dts);
                }
                dts += e.sample_delta as i64;
            }
        }
        ticks.resize(n, dts);
        if let Some(ctts) = &stbl.ctts {
            let mut i = 0;
            for e in &ctts.entries {
                for _ in 0..e.sample_count {
                    if let Some(x) = ticks.get_mut(i) {
                        *x += e.sample_offset as i64;
                    }
                    i += 1;
                }
            }
        }
        let first = ticks.iter().copied().min().unwrap_or(0);
        let times: Vec<f64> = ticks.iter().map(|x| (x - first) as f64 / timescale).collect();
        let sizes: Vec<u32> = if stbl.stsz.sample_size != 0 {
            vec![stbl.stsz.sample_size; n]
        } else {
            let mut s = stbl.stsz.sample_sizes.clone();
            s.resize(n, 0);
            s
        };
        let chunks: Vec<u64> = match (&stbl.stco, &stbl.co64) {
            (Some(c), _) => c.entries.iter().map(|&x| x as u64).collect(),
            (None, Some(c)) => c.entries.clone(),
            _ => anyhow::bail!("track {} has no chunk offsets", t.track_id()),
        };
        // stsc: runs of chunks with the same samples per chunk.
        let mut offsets = Vec::with_capacity(n);
        let entries = &stbl.stsc.entries;
        let mut sample = 0usize;
        for (k, e) in entries.iter().enumerate() {
            let first_chunk = e.first_chunk.max(1) as usize;
            let last_chunk = entries
                .get(k + 1)
                .map_or(chunks.len(), |next| (next.first_chunk as usize).saturating_sub(1));
            for chunk in first_chunk..=last_chunk.min(chunks.len()) {
                let mut at = chunks[chunk - 1];
                for _ in 0..e.samples_per_chunk {
                    if sample >= n {
                        break;
                    }
                    offsets.push(at);
                    at += sizes[sample] as u64;
                    sample += 1;
                }
            }
        }
        if offsets.len() != n {
            anyhow::bail!(
                "track {}: chunk table covers {} of {n} samples",
                t.track_id(),
                offsets.len()
            );
        }
        let sync = match &stbl.stss {
            Some(stss) if video => {
                let mut s = vec![false; n];
                for &k in &stss.entries {
                    if let Some(x) = s.get_mut((k as usize).wrapping_sub(1)) {
                        *x = true;
                    }
                }
                s
            }
            _ => vec![true; n],
        };
        out.push(Samples {
            video,
            offsets,
            sizes,
            times,
            sync,
        });
    }
    Ok(out)
}

/// Byte ranges `[start, end)` of every track's samples inside `windows`
/// (source seconds), merged, sorted.
pub fn sample_ranges(tracks: &[Samples], windows: &[(f64, f64)]) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    for t in tracks {
        let n = t.times.len();
        for &(a, b) in windows {
            let (a, b) = (a - MARGIN_S, b + MARGIN_S);
            let inside: Vec<usize> = (0..n).filter(|&i| t.times[i] >= a && t.times[i] <= b).collect();
            let (Some(&lo), Some(&hi)) = (inside.iter().min(), inside.iter().max()) else {
                continue;
            };
            let (start, end) = if t.video {
                let start = (0..=lo).rev().find(|&i| t.sync[i]).unwrap_or(0);
                (start, (hi + REORDER).min(n - 1))
            } else {
                (lo, hi)
            };
            for i in start..=end {
                ranges.push((t.offsets[i], t.offsets[i] + t.sizes[i] as u64));
            }
        }
    }
    merge(ranges, MERGE_GAP)
}

/// Sorts and merges ranges closer than `gap`.
pub fn merge(mut ranges: Vec<(u64, u64)>, gap: u64) -> Vec<(u64, u64)> {
    ranges.retain(|r| r.1 > r.0);
    ranges.sort();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 + gap => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// What of `want` is not in `have` (both merged and sorted).
pub fn missing(want: &[(u64, u64)], have: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for &(mut a, b) in want {
        for &(h0, h1) in have {
            if h1 <= a || h0 >= b {
                continue;
            }
            if h0 > a {
                out.push((a, h0));
            }
            a = a.max(h1);
            if a >= b {
                break;
            }
        }
        if a < b {
            out.push((a, b));
        }
    }
    out
}

/// The byte ranges present in a sparse copy, as recorded beside it.
pub fn load_ranges(path: &std::path::Path) -> Vec<(u64, u64)> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<(u64, u64)>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_ranges(path: &std::path::Path, ranges: &[(u64, u64)]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", crate::util::unique()));
    std::fs::write(&tmp, serde_json::to_string(ranges).unwrap_or_default())?;
    std::fs::rename(tmp, path)
}

/// Reads `[at, at + len)` of a local file.
pub fn read_local(f: &mut std::fs::File, at: u64, len: u64) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Whether `[a, b)` lies inside the ranges.
pub fn covered(have: &[(u64, u64)], a: u64, b: u64) -> bool {
    missing(&[(a, b)], have).is_empty()
}

/// Bytes in the ranges.
pub fn total(ranges: &[(u64, u64)]) -> u64 {
    ranges.iter().map(|r| r.1 - r.0).sum()
}

/// Per kind of top-level box, how many (for diagnostics).
pub fn census(boxes: &[TopBox]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for b in boxes {
        *m.entry(String::from_utf8_lossy(&b.kind).into_owned()).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_and_missing() {
        assert_eq!(
            merge(vec![(10, 20), (0, 5), (21, 30), (100, 110)], 1),
            vec![(0, 5), (10, 30), (100, 110)]
        );
        assert_eq!(
            missing(&[(0, 100)], &[(10, 20), (50, 60)]),
            vec![(0, 10), (20, 50), (60, 100)]
        );
        assert_eq!(missing(&[(10, 20)], &[(0, 100)]), vec![]);
        assert!(covered(&merge(vec![(0, 10), (10, 20)], 0), 5, 15));
    }

    #[test]
    fn walks_top_level_boxes() {
        let mut f = Vec::new();
        let mut boxed = |kind: &[u8; 4], body: usize| {
            f.extend(((body + 8) as u32).to_be_bytes());
            f.extend(kind);
            f.extend(vec![0u8; body]);
        };
        boxed(b"ftyp", 16);
        boxed(b"mdat", 1000);
        boxed(b"moov", 40);
        let len = f.len() as u64;
        let boxes = top_boxes(len, |at, n| Ok(f[at as usize..(at + n) as usize].to_vec())).unwrap();
        let kinds: Vec<&[u8; 4]> = boxes.iter().map(|b| &b.kind).collect();
        assert_eq!(kinds, vec![b"ftyp", b"mdat", b"moov"]);
        assert_eq!(boxes[2].offset, 24 + 1008);
    }
}

//! A small MPEG transport stream writer: one H.264 or HEVC stream, each
//! access unit with its presentation and decoding times.
//!
//! NVENC's frames reach ffmpeg (which muxes MP4, MOV and Matroska and adds
//! the sound) through a pipe. A raw elementary stream carries no
//! timestamps, so ffmpeg can only number its frames in the order they
//! arrive; with B-frames that order is decode order, not display order.
//! A transport stream says both times for every frame, and ffmpeg's
//! muxers write them as given (composition offsets and an edit list in
//! MP4).

use std::io::{self, Write};

const PACKET: usize = 188;
const PID_PAT: u16 = 0x0000;
const PID_PMT: u16 = 0x1000;
const PID_VIDEO: u16 = 0x0100;

/// The video stream's codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    H264,
    Hevc,
}

impl Stream {
    fn stream_type(self) -> u8 {
        match self {
            Stream::H264 => 0x1b,
            Stream::Hevc => 0x24,
        }
    }
}

pub struct TsWriter<W: Write> {
    out: W,
    stream: Stream,
    /// Continuity counters: PAT, PMT, video.
    cc: [u8; 3],
    /// Whether the tables went out yet.
    tables: bool,
}

impl<W: Write> TsWriter<W> {
    pub fn new(out: W, stream: Stream) -> Self {
        TsWriter {
            out,
            stream,
            cc: [0; 3],
            tables: false,
        }
    }

    /// Writes one access unit (Annex B) with its times on the 90 kHz clock
    /// (`dts <= pts`). The tables go out first, and again before every
    /// key frame, where a reader can start.
    pub fn write(&mut self, data: &[u8], pts: u64, dts: u64, key: bool) -> io::Result<()> {
        if !self.tables || key {
            self.write_tables()?;
            self.tables = true;
        }
        let mut pes = Vec::with_capacity(data.len() + 19);
        // Packet start code, video stream 0, length unbounded (allowed for
        // video in a transport stream).
        pes.extend_from_slice(&[0, 0, 1, 0xe0, 0, 0]);
        // '10', data aligned (an access unit starts here).
        pes.push(0x84);
        if pts != dts {
            pes.push(0xc0);
            pes.push(10);
            pes.extend_from_slice(&timestamp(0b0011, pts));
            pes.extend_from_slice(&timestamp(0b0001, dts));
        } else {
            pes.push(0x80);
            pes.push(5);
            pes.extend_from_slice(&timestamp(0b0010, pts));
        }
        pes.extend_from_slice(data);
        // The reference clock a little ahead of decoding.
        let pcr = dts.saturating_sub(9000);
        let mut rest = &pes[..];
        let mut first = true;
        while !rest.is_empty() {
            let mut p = [0xffu8; PACKET];
            p[0] = 0x47;
            p[1] = ((first as u8) << 6) | (PID_VIDEO >> 8) as u8;
            p[2] = PID_VIDEO as u8;
            let cc = self.next_cc(2);
            // Adaptation field: on the first packet the clock (and whether
            // decoding can start here); on the last, stuffing.
            let mut af: Vec<u8> = Vec::new();
            if first {
                af.push(if key { 0x50 } else { 0x10 });
                af.extend_from_slice(&pcr_bytes(pcr));
            }
            let room = PACKET - 4 - if af.is_empty() { 0 } else { 1 + af.len() };
            let take = if rest.len() < room {
                // Stuff the adaptation field until the payload fills the
                // packet exactly.
                // (`need` counts the field's length byte; a field of length
                // 0 is just that byte, longer ones start with the flags.)
                let need = PACKET - 4 - rest.len();
                if af.is_empty() && need >= 2 {
                    af.push(0x00);
                }
                while 1 + af.len() < need {
                    af.push(0xff);
                }
                rest.len()
            } else {
                room
            };
            let mut at = 4;
            if af.is_empty() && take == PACKET - 4 {
                p[3] = 0x10 | cc;
            } else {
                p[3] = 0x30 | cc;
                p[4] = af.len() as u8;
                p[5..5 + af.len()].copy_from_slice(&af);
                at = 5 + af.len();
            }
            p[at..at + take].copy_from_slice(&rest[..take]);
            debug_assert_eq!(at + take, PACKET);
            self.out.write_all(&p)?;
            rest = &rest[take..];
            first = false;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    fn next_cc(&mut self, i: usize) -> u8 {
        let c = self.cc[i];
        self.cc[i] = (c + 1) & 0x0f;
        c
    }

    fn write_tables(&mut self) -> io::Result<()> {
        // Program 1 at the PMT's PID.
        let pat = section(0x00, 0x0001, &[0x00, 0x01, 0xe0 | (PID_PMT >> 8) as u8, PID_PMT as u8]);
        // The clock rides on the video PID; one stream, no descriptors.
        let pmt = section(
            0x02,
            0x0001,
            &[
                0xe0 | (PID_VIDEO >> 8) as u8,
                PID_VIDEO as u8,
                0xf0,
                0x00,
                self.stream.stream_type(),
                0xe0 | (PID_VIDEO >> 8) as u8,
                PID_VIDEO as u8,
                0xf0,
                0x00,
            ],
        );
        let cc = self.next_cc(0);
        self.write_psi(PID_PAT, cc, &pat)?;
        let cc = self.next_cc(1);
        self.write_psi(PID_PMT, cc, &pmt)
    }

    fn write_psi(&mut self, pid: u16, cc: u8, section: &[u8]) -> io::Result<()> {
        let mut p = [0xffu8; PACKET];
        p[0] = 0x47;
        p[1] = 0x40 | (pid >> 8) as u8;
        p[2] = pid as u8;
        p[3] = 0x10 | cc;
        // Pointer field: the section starts right after it.
        p[4] = 0;
        p[5..5 + section.len()].copy_from_slice(section);
        self.out.write_all(&p)
    }
}

/// A PSI section with the syntax header, version 0, one section, and its
/// CRC.
fn section(table_id: u8, id: u16, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![
        table_id,
        0xb0 | ((len >> 8) as u8 & 0x0f),
        len as u8,
        (id >> 8) as u8,
        id as u8,
        0xc1,
        0x00,
        0x00,
    ];
    s.extend_from_slice(body);
    let crc = crc32_mpeg(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

/// A PES time stamp: four marker bits, then 33 bits with marker bits
/// between.
fn timestamp(prefix: u8, t: u64) -> [u8; 5] {
    let t = t & ((1 << 33) - 1);
    [
        (prefix << 4) | (((t >> 30) as u8 & 0x07) << 1) | 1,
        (t >> 22) as u8,
        (((t >> 15) as u8 & 0x7f) << 1) | 1,
        (t >> 7) as u8,
        ((t as u8 & 0x7f) << 1) | 1,
    ]
}

/// A program clock reference on the 90 kHz base (no 27 MHz extension).
fn pcr_bytes(base: u64) -> [u8; 6] {
    let b = base & ((1 << 33) - 1);
    [
        (b >> 25) as u8,
        (b >> 17) as u8,
        (b >> 9) as u8,
        (b >> 1) as u8,
        ((b as u8 & 1) << 7) | 0x7e,
        0x00,
    ]
}

/// CRC-32/MPEG-2: polynomial 0x04C11DB7, not reflected, initial all ones.
fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc() {
        // The check value of CRC-32/MPEG-2.
        assert_eq!(crc32_mpeg(b"123456789"), 0x0376_e6e7);
    }

    #[test]
    fn timestamps() {
        // 33 bits with their marker bits, as ISO/IEC 13818-1 lays them out.
        assert_eq!(timestamp(0b0010, 0), [0x21, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(timestamp(0b0011, (1 << 33) - 1), [0x3f, 0xff, 0xff, 0xff, 0xff]);
        let t = 0x1_2345_6789u64;
        let b = timestamp(0b0001, t);
        let back = (((b[0] >> 1) & 0x07) as u64) << 30
            | (b[1] as u64) << 22
            | ((b[2] >> 1) as u64) << 15
            | (b[3] as u64) << 7
            | (b[4] >> 1) as u64;
        assert_eq!(back, t);
    }

    /// Every packet is 188 bytes and starts in sync; the video PID's
    /// continuity counter steps by one; payload bytes come back in order.
    #[test]
    fn packets() {
        for len in [0usize, 1, 150, 165, 166, 167, 168, 169, 182, 183, 184, 185, 1000, 5000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let mut out = Vec::new();
            let mut w = TsWriter::new(&mut out, Stream::H264);
            w.write(&data, 9000, 6000, true).unwrap();
            w.write(&data, 3000, 3000, false).unwrap();
            assert_eq!(out.len() % PACKET, 0, "len {len}");
            let mut payload = Vec::new();
            let mut last_cc = None;
            for p in out.chunks(PACKET) {
                assert_eq!(p[0], 0x47);
                let pid = ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
                if pid != PID_VIDEO {
                    continue;
                }
                let cc = p[3] & 0x0f;
                if let Some(l) = last_cc {
                    assert_eq!(cc, (l + 1) & 0x0f);
                }
                last_cc = Some(cc);
                let mut at = 4;
                if p[3] & 0x20 != 0 {
                    at = 5 + p[4] as usize;
                }
                payload.extend_from_slice(&p[at..]);
            }
            // Two PES packets: headers of 19 and 14 bytes.
            let mut want = vec![0, 0, 1, 0xe0, 0, 0, 0x84, 0xc0, 10];
            want.extend(timestamp(0b0011, 9000));
            want.extend(timestamp(0b0001, 6000));
            want.extend(&data);
            want.extend([0, 0, 1, 0xe0, 0, 0, 0x84, 0x80, 5]);
            want.extend(timestamp(0b0010, 3000));
            want.extend(&data);
            assert_eq!(payload, want, "len {len}");
        }
    }
}

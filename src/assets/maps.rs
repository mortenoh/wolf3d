//! GAMEMAPS/MAPHEAD level loader.
//!
//! MAPHEAD holds the RLEW tag (0xABCD) and 100 offsets into GAMEMAPS. Each
//! level has a header pointing at up to 3 planes (0 = walls/doors, 1 =
//! objects/spawns, 2 = unused in Wolf), each compressed twice: RLEW
//! (run-length over 16-bit words), then Carmack (LZ-style back-references
//! over words). Expansion therefore runs Carmack first, then RLEW.

use std::path::Path;

pub const MAP_SIZE: usize = 64;
const NUM_MAPS: usize = 100;

pub struct Level {
    pub name: String,
    /// Walls/doors plane, row-major 64x64: `plane0[y * 64 + x]`.
    pub plane0: Vec<u16>,
    /// Objects/spawns plane, same layout.
    pub plane1: Vec<u16>,
}

pub struct MapSet {
    gamemaps: Vec<u8>,
    rlew_tag: u16,
    offsets: Vec<i32>,
}

fn u16at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn i32at(b: &[u8], i: usize) -> i32 {
    i32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl MapSet {
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let maphead = std::fs::read(dir.join("MAPHEAD.WL6"))?;
        let gamemaps = std::fs::read(dir.join("GAMEMAPS.WL6"))?;
        let rlew_tag = u16at(&maphead, 0);
        let offsets = (0..NUM_MAPS)
            .map(|i| i32at(&maphead, 2 + i * 4))
            .collect();
        Ok(Self { gamemaps, rlew_tag, offsets })
    }

    pub fn num_levels(&self) -> usize {
        self.offsets.iter().take_while(|&&o| o > 0).count()
    }

    pub fn level(&self, n: usize) -> Level {
        let off = self.offsets[n];
        assert!(off > 0, "level {n} not present");
        let h = &self.gamemaps[off as usize..];

        // Header: 3 x i32 plane offsets, 3 x u16 compressed lengths,
        // u16 width, u16 height, 16-byte name.
        let mut planes = Vec::new();
        for p in 0..2 {
            let plane_off = i32at(h, p * 4) as usize;
            let plane_len = u16at(h, 12 + p * 2) as usize;
            let data = &self.gamemaps[plane_off..plane_off + plane_len];
            planes.push(self.expand_plane(data));
        }
        let name_bytes = &h[22..38];
        let name = name_bytes
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect::<String>()
            .trim()
            .to_string();

        let plane1 = planes.pop().unwrap();
        let plane0 = planes.pop().unwrap();
        assert_eq!(plane0.len(), MAP_SIZE * MAP_SIZE);
        Level { name, plane0, plane1 }
    }

    /// Carmack-expand then RLEW-expand one plane blob into 64x64 words.
    fn expand_plane(&self, data: &[u8]) -> Vec<u16> {
        // First word: byte length of the Carmack-expanded output.
        let expanded_len = u16at(data, 0) as usize;
        let carmack = carmack_expand(&data[2..], expanded_len);
        // The Carmack output's first word is the RLEW-expanded byte length.
        let rlew_len = carmack[0] as usize;
        rlew_expand(&carmack[1..], rlew_len, self.rlew_tag)
    }
}

const NEARTAG: u8 = 0xA7;
const FARTAG: u8 = 0xA8;

/// Carmack compression: a word whose high byte is A7 (near) or A8 (far) is a
/// back-reference of `low byte` words; near refs read a 1-byte word-distance
/// back from the write head, far refs a 2-byte absolute word offset. A ref
/// with count 0 escapes a literal word whose low byte follows.
fn carmack_expand(mut src: &[u8], out_bytes: usize) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(out_bytes / 2);
    while out.len() < out_bytes / 2 {
        let (count, tag) = (src[0], src[1]);
        if (tag == NEARTAG || tag == FARTAG) && count == 0 {
            // Escaped literal: third byte is the real low byte.
            out.push(u16::from_le_bytes([src[2], tag]));
            src = &src[3..];
        } else if tag == NEARTAG {
            let dist = src[2] as usize;
            src = &src[3..];
            let start = out.len() - dist;
            for i in 0..count as usize {
                out.push(out[start + i]);
            }
        } else if tag == FARTAG {
            let start = u16at(src, 2) as usize;
            src = &src[4..];
            for i in 0..count as usize {
                out.push(out[start + i]);
            }
        } else {
            out.push(u16::from_le_bytes([count, tag]));
            src = &src[2..];
        }
    }
    out
}

/// RLEW: literal words, except `tag` which is followed by (count, value).
fn rlew_expand(src: &[u16], out_bytes: usize, tag: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(out_bytes / 2);
    let mut i = 0;
    while out.len() < out_bytes / 2 {
        let w = src[i];
        i += 1;
        if w == tag {
            let (count, value) = (src[i], src[i + 1]);
            i += 2;
            out.extend(std::iter::repeat_n(value, count as usize));
        } else {
            out.push(w);
        }
    }
    out
}

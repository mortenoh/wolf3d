//! VGAGRAPH loader — the Huffman-compressed graphics archive holding the menu
//! and HUD pictures (the status bar, the HUD number font, the BJ faces, keys,
//! weapon icons, etc.).
//!
//! Three files cooperate:
//! - VGADICT.WL6: 1024 bytes = 256 Huffman nodes, each `{u16 bit0, u16 bit1}`.
//!   A branch value < 256 is a leaf (that byte); >= 256 means descend to node
//!   `value - 256`. The tree root is node 254. Bits are consumed LSB-first.
//! - VGAHEAD.WL6: one 3-byte little-endian file offset per chunk (plus a final
//!   end sentinel); 0xFFFFFF marks an absent chunk.
//! - VGAGRAPH.WL6: the chunks. Each compressed chunk starts with a u32 giving
//!   its expanded byte length, then the Huffman bitstream.
//!
//! Chunk 0 (STRUCTPIC) expands to the "pictable": pairs of i16 (width, height),
//! one per picture chunk, indexed by `chunk - STARTPICS`.
//!
//! Picture chunks expand to VGA-planar bytes: four consecutive planes, where
//! `pixel(x, y) = plane[x & 3]` at index `(y * width + x) >> 2`. We deplane to
//! row-major palette indices and map through PALETTE to packed u32 RGBA.

use std::path::Path;

use super::palette::PALETTE;

// =============================================================================
// GRAPHIC CHUNK NUMBERS
//
// From id-Software's WOLFSRC/GFXV_WL6.H (v1.4 WL6 numbering). Only the chunks
// the HUD actually needs are named here. Picture chunk N's pictable entry is at
// index `N - STARTPICS`.
// =============================================================================

/// STRUCTPIC (chunk 0) is the pictable; chunks 1 and 2 are the two fonts.
pub const STARTFONT: usize = 1;

/// First picture chunk (chunks 0..3 are STRUCTPIC and the two fonts).
pub const STARTPICS: usize = 3;

// --- Menu art (WL_MENU.C / GFXV_WL6.H, v1.4 WL6 numbering) ---
/// "Options" header art at the top of the main menu.
pub const C_OPTIONSPIC: usize = 10;
/// Gun cursor, two animation frames (pointing / firing).
pub const C_CURSOR1PIC: usize = 11;
pub const C_CURSOR2PIC: usize = 12;
/// Sound-menu radio buttons: the empty and filled bullet art.
pub const C_NOTSELECTEDPIC: usize = 13;
pub const C_SELECTEDPIC: usize = 14;
/// Sound-menu section titles ("Sound Effects" / "Digitized" / "AdLib Music").
/// There is no `C_SOUNDPIC` in GFXV_WL6.H; CP_Sound draws these three title
/// pics instead. We collapse to the effects and music groups.
pub const C_FXTITLEPIC: usize = 15;
pub const C_MUSICTITLEPIC: usize = 17;
/// Load / Save game screen header art.
pub const C_LOADGAMEPIC: usize = 28;
pub const C_SAVEGAMEPIC: usize = 29;
/// Difficulty banners, one per skill: "Can I play, Daddy?" (baby), "Don't hurt
/// me." (easy), "Bring 'em on!" (normal), "I am Death incarnate!" (hard). Each
/// carries a BJ mugshot whose expression hardens with the skill.
pub const C_BABYMODEPIC: usize = 19;
/// Episode name banners, C_EPISODE1PIC..=C_EPISODE1PIC+5.
pub const C_EPISODE1PIC: usize = 30;

pub const STATUSBARPIC: usize = 86;

/// The game's title screen (shown before the menu).
pub const TITLEPIC: usize = 87;

/// The credits page (GFXV_WL6.H CREDITSPIC), shown in the attract loop between
/// the title and the high-score board.
pub const CREDITSPIC: usize = 89;

// Weapon icons on the status bar (KNIFEPIC + weapon), unused for now but kept
// for reference alongside the VSWAP weapon-overlay sprites.
pub const KNIFEPIC: usize = 91;
pub const GUNPIC: usize = 92;
pub const MACHINEGUNPIC: usize = 93;
pub const GATLINGGUNPIC: usize = 94;

pub const NOKEYPIC: usize = 95;
pub const GOLDKEYPIC: usize = 96;
pub const SILVERKEYPIC: usize = 97;

/// Blank digit cell, then the ten HUD digits N_0PIC..N_9PIC.
pub const N_BLANKPIC: usize = 98;
pub const N_0PIC: usize = 99;

/// BJ face pics: 8 health brackets, 3 look-direction frames each (A/B/C).
/// `FACE1APIC + 3 * bracket + frame`. FACE8APIC is the death face.
pub const FACE1APIC: usize = 109;
pub const FACE8APIC: usize = 130;

/// Intermission (WL_INTER.C LevelCompleted) art. The two BJ "breather" frames
/// alternate; the big level-end digits are L_NUM0PIC..L_NUM0PIC+9.
pub const L_GUYPIC: usize = 43;
pub const L_NUM0PIC: usize = 45;
pub const L_GUY2PIC: usize = 84;

/// The victorious-BJ picture on the "YOU WIN!" screen (WL_INTER.C `Victory`).
pub const L_BJWINSPIC: usize = 85;

/// The high-score board header art (WL_INTER.C `DrawHighScores`).
pub const HIGHSCORESPIC: usize = 90;

/// End-of-episode article text chunks (GFXV_WL6.H `T_ENDART1..6`), one per
/// episode; and the "Read This!" help article (`T_HELPART`). These are text
/// chunks (caret markup), decoded via [`VgaGraph::raw_chunk`], not pictures.
pub const T_HELPART: usize = 138;
pub const T_ENDART1: usize = 143;

/// The "Get Psyched!" load screen bar art (GFXV_WL6.H GETPSYCHEDPIC).
pub const GETPSYCHEDPIC: usize = 134;

const NUM_NODES: usize = 256;
const HUFF_ROOT: usize = 254;
const ABSENT: u32 = 0x00FF_FFFF;

/// A decoded picture: row-major packed RGBA (`fb::rgb` layout), fully opaque.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

pub struct VgaGraph {
    dict: [[u16; 2]; NUM_NODES],
    /// One file offset per chunk, plus a trailing end offset.
    head: Vec<u32>,
    graph: Vec<u8>,
    /// (width, height) per picture chunk, indexed by `chunk - STARTPICS`.
    pictable: Vec<(usize, usize)>,
}

impl VgaGraph {
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let dict_bytes = std::fs::read(dir.join("VGADICT.WL6"))?;
        let head_bytes = std::fs::read(dir.join("VGAHEAD.WL6"))?;
        let graph = std::fs::read(dir.join("VGAGRAPH.WL6"))?;

        let mut dict = [[0u16; 2]; NUM_NODES];
        for (i, node) in dict.iter_mut().enumerate() {
            node[0] = u16::from_le_bytes([dict_bytes[i * 4], dict_bytes[i * 4 + 1]]);
            node[1] = u16::from_le_bytes([dict_bytes[i * 4 + 2], dict_bytes[i * 4 + 3]]);
        }

        let head = (0..head_bytes.len() / 3)
            .map(|i| {
                let b = &head_bytes[i * 3..];
                u32::from_le_bytes([b[0], b[1], b[2], 0])
            })
            .collect();

        let mut vga = Self { dict, head, graph, pictable: Vec::new() };

        // Chunk 0 (STRUCTPIC) is the pictable: pairs of i16 (width, height).
        let table = vga.expand_chunk(0);
        vga.pictable = table
            .chunks_exact(4)
            .map(|c| {
                let w = i16::from_le_bytes([c[0], c[1]]) as usize;
                let h = i16::from_le_bytes([c[2], c[3]]) as usize;
                (w, h)
            })
            .collect();

        Ok(vga)
    }

    /// The compressed byte span of a chunk: from its offset to the next present
    /// chunk's offset (or end of file).
    fn chunk_span(&self, chunk: usize) -> &[u8] {
        let start = self.head[chunk] as usize;
        let end = self.head[chunk + 1..]
            .iter()
            .copied()
            .find(|&o| o != ABSENT)
            .map_or(self.graph.len(), |o| o as usize);
        &self.graph[start..end]
    }

    /// Huffman-expand a chunk. Its first u32 is the expanded byte length.
    fn expand_chunk(&self, chunk: usize) -> Vec<u8> {
        let span = self.chunk_span(chunk);
        let expanded = u32::from_le_bytes([span[0], span[1], span[2], span[3]]) as usize;
        huff_expand(&self.dict, &span[4..], expanded)
    }

    /// Huffman-expand a chunk to its raw bytes. Used by the font loader, whose
    /// chunks are `fontstruct` records rather than VGA-planar pictures.
    pub fn raw_chunk(&self, chunk: usize) -> Vec<u8> {
        self.expand_chunk(chunk)
    }

    /// Decode a picture chunk to a row-major RGBA [`Picture`].
    pub fn pic(&self, chunk: usize) -> Picture {
        let (width, height) = self.pictable[chunk - STARTPICS];
        let planar = self.expand_chunk(chunk);
        let mut pixels = vec![0u32; width * height];
        let plane_len = width * height / 4;
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) >> 2;
                let byte = planar[(x & 3) * plane_len + idx];
                pixels[y * width + x] = PALETTE[byte as usize];
            }
        }
        Picture { width, height, pixels }
    }
}

/// Expand a Huffman bitstream to `out_len` bytes, LSB-first per source byte.
fn huff_expand(dict: &[[u16; 2]; NUM_NODES], src: &[u8], out_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(out_len);
    let mut node = HUFF_ROOT;
    let mut si = 0;
    let mut byte = src[0];
    let mut mask = 1u8;
    while out.len() < out_len {
        let bit = (byte & mask != 0) as usize;
        let value = dict[node][bit];
        if mask == 0x80 {
            mask = 1;
            si += 1;
            if si < src.len() {
                byte = src[si];
            }
        } else {
            mask <<= 1;
        }
        if (value as usize) < 256 {
            out.push(value as u8);
            node = HUFF_ROOT;
        } else {
            node = value as usize - 256;
        }
    }
    out
}

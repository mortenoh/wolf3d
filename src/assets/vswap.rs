//! VSWAP loader — the "page file" holding wall textures, sprites, and
//! digitized sounds. Header: chunk count, then the indices where sprites and
//! sounds begin, then per-chunk file offsets and lengths.
//!
//! Wall textures are the chunks before `sprite_start`: raw 64x64 palette
//! indices, stored COLUMN-major (texture[x * 64 + y]) — convenient for the
//! renderer, which paints vertical strips. They come in pairs: even chunks
//! are the light variant (used on N/S faces), odd the dark one (E/W faces).

use std::path::Path;

use super::palette::PALETTE;

pub const TEX_SIZE: usize = 64;

pub struct VSwap {
    /// Wall textures as packed framebuffer colors, still column-major.
    pub walls: Vec<Box<[u32; TEX_SIZE * TEX_SIZE]>>,
    /// Sprites decoded to 64x64 column-major RGBA; 0x00000000 = transparent.
    /// Indexed by sprite number (chunk - sprite_start).
    pub sprites: Vec<Box<[u32; TEX_SIZE * TEX_SIZE]>>,
    /// Chunk index where sprites begin == number of wall textures.
    pub sprite_start: usize,
}

impl VSwap {
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let data = std::fs::read(dir.join("VSWAP.WL6"))?;
        let u16at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
        let u32at = |i: usize| {
            u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize
        };

        let _num_chunks = u16at(0);
        let sprite_start = u16at(2);
        let _sound_start = u16at(4);

        let mut walls = Vec::with_capacity(sprite_start);
        for chunk in 0..sprite_start {
            let off = u32at(6 + chunk * 4);
            let mut tex = Box::new([0u32; TEX_SIZE * TEX_SIZE]);
            for (i, texel) in tex.iter_mut().enumerate() {
                *texel = PALETTE[data[off + i] as usize];
            }
            walls.push(tex);
        }

        let sound_start = u16at(4);
        let mut sprites = Vec::with_capacity(sound_start - sprite_start);
        for chunk in sprite_start..sound_start {
            let off = u32at(6 + chunk * 4);
            let len = u16at(6 + u16at(0) * 4 + chunk * 2);
            sprites.push(decode_sprite(&data[off..off + len]));
        }

        Ok(Self { walls, sprites, sprite_start })
    }

    /// The door face texture pair starts 8 chunks before the sprites.
    pub fn door_texture(&self) -> usize {
        self.sprite_start - 8
    }
}

/// Decode one sprite chunk. Layout: u16 left/right extent columns, then one
/// u16 offset per covered column pointing at that column's draw commands.
/// Commands are (end_row*2, pixel_offset, start_row*2) triples, 0-terminated;
/// the pixel offset is pre-biased so `chunk[pixel_offset + row]` (signed
/// wrapping) is the palette index for `row`. Uncovered texels stay 0
/// (transparent — distinct from opaque palette black, which packs alpha 0xFF).
fn decode_sprite(chunk: &[u8]) -> Box<[u32; TEX_SIZE * TEX_SIZE]> {
    let u16at = |i: usize| u16::from_le_bytes([chunk[i], chunk[i + 1]]);
    let mut tex = Box::new([0u32; TEX_SIZE * TEX_SIZE]);

    let left = u16at(0) as usize;
    let right = u16at(2) as usize;
    for col in left..=right {
        let mut cmd = u16at(4 + (col - left) * 2) as usize;
        loop {
            let end = u16at(cmd) as usize / 2;
            if end == 0 {
                break;
            }
            let pix_off = u16at(cmd + 2) as i16;
            let start = u16at(cmd + 4) as usize / 2;
            for row in start..end {
                let src = (pix_off as isize + row as isize) as usize;
                tex[col * TEX_SIZE + row] = PALETTE[chunk[src] as usize];
            }
            cmd += 6;
        }
    }
    tex
}

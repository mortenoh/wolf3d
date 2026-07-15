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
        Ok(Self { walls, sprite_start })
    }

    /// The door face texture pair starts 8 chunks before the sprites.
    pub fn door_texture(&self) -> usize {
        self.sprite_start - 8
    }
}

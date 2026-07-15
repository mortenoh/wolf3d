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

/// Digitized sounds play at ~7 kHz 8-bit unsigned PCM (ID_SD.C SD_PlayDigitized
/// / SDL_SetupDigi; the Sound Blaster DMA rate Wolf3D used is commonly cited as
/// 7042 Hz).
pub const DIGI_RATE: u32 = 7042;

/// Page manager page size (ID_PM.H `PMPageSize`): every VSWAP sound chunk is one
/// 4096-byte page, and a digitized sound is a run of consecutive pages.
const PM_PAGE_SIZE: usize = 4096;

pub struct VSwap {
    /// Wall textures as packed framebuffer colors, still column-major.
    pub walls: Vec<Box<[u32; TEX_SIZE * TEX_SIZE]>>,
    /// Sprites decoded to 64x64 column-major RGBA; 0x00000000 = transparent.
    /// Indexed by sprite number (chunk - sprite_start).
    pub sprites: Vec<Box<[u32; TEX_SIZE * TEX_SIZE]>>,
    /// Chunk index where sprites begin == number of wall textures.
    pub sprite_start: usize,
    /// Digitized sound effects, 8-bit unsigned PCM at [`DIGI_RATE`], indexed by
    /// digitized-sound number (the value the DigiMap maps a sound enum to).
    pub digi: Vec<Vec<u8>>,
}

impl VSwap {
    /// Load the WL6 page file (the default variant).
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        Self::load_ext(dir, "WL6")
    }

    /// Load the VSWAP page file for a given data-file extension (`WL6` / `SOD`).
    pub fn load_ext(dir: &Path, ext: &str) -> std::io::Result<Self> {
        let data = std::fs::read(dir.join(format!("VSWAP.{ext}")))?;
        let u16at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
        let u32at = |i: usize| {
            u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize
        };

        let num_chunks = u16at(0);
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

        let digi = load_digi(&data, num_chunks, sound_start);

        Ok(Self {
            walls,
            sprites,
            sprite_start,
            digi,
        })
    }

    /// The door face texture pair starts 8 chunks before the sprites.
    pub fn door_texture(&self) -> usize {
        self.sprite_start - 8
    }
}

/// Parse the digitized sounds. The final VSWAP chunk (`num_chunks - 1`) is the
/// "sound info page" (ID_SD.C `SDL_SetupDigi`): an array of `(u16 start_page,
/// u16 length)` pairs, one per digitized sound, `start_page` counted from the
/// first sound chunk (`sound_start`). A sound's PCM is `length` bytes drawn from
/// the run of pages beginning at chunk `sound_start + start_page`; because pages
/// are stored contiguously in the file, that is just `length` bytes from that
/// chunk's file offset.
fn load_digi(data: &[u8], num_chunks: usize, sound_start: usize) -> Vec<Vec<u8>> {
    if num_chunks == 0 || sound_start >= num_chunks {
        return Vec::new();
    }
    let u32at =
        |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
    let u16at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    // Per-chunk file offset table (u32) and length table (u16) live back to back.
    let chunk_off = |c: usize| u32at(6 + c * 4);
    let chunk_len = |c: usize| u16at(6 + num_chunks * 4 + c * 2);

    let info = num_chunks - 1;
    let info_off = chunk_off(info);
    let info_len = chunk_len(info);
    let pairs = info_len / 4;

    // Count the digitized sounds exactly as SDL_SetupDigi does: walk the pairs,
    // advancing a page cursor by each sound's page span, until it reaches the
    // info page itself.
    let mut pg = sound_start;
    let mut num_digi = 0;
    for i in 0..pairs {
        if pg >= info {
            break;
        }
        let len = u16at(info_off + i * 4 + 2);
        pg += len.div_ceil(PM_PAGE_SIZE);
        num_digi = i + 1;
    }

    let mut out = Vec::with_capacity(num_digi);
    for i in 0..num_digi {
        let start_page = u16at(info_off + i * 4);
        let len = u16at(info_off + i * 4 + 2);
        let chunk = sound_start + start_page;
        if chunk >= info {
            out.push(Vec::new());
            continue;
        }
        let off = chunk_off(chunk);
        let end = (off + len).min(data.len());
        out.push(data[off..end].to_vec());
    }
    out
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

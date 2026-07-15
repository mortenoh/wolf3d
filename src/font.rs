//! VGAGRAPH bitmap-font rendering for the menu and other on-screen text.
//!
//! The font chunks (STARTFONT, STARTFONT+1) are *not* pictures. Each is an
//! ID_VH.C `fontstruct`:
//!
//! ```c
//! typedef struct { sword height; word location[256]; byte width[256]; } fontstruct;
//! ```
//!
//! i.e. an `i16` height, then 256 `u16` glyph offsets (absolute byte offsets
//! into the chunk), then 256 `u8` glyph widths. A glyph is `width * height`
//! bytes stored row-major, one byte per pixel: 0 is transparent, any nonzero
//! value is ink drawn in the caller's color. (VW_DrawPropString walks each
//! glyph column-by-column with `si += width` per row, which is exactly a
//! row-major `data[row * width + col]` layout.)

use crate::assets::VgaGraph;
use crate::assets::palette::PALETTE;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};

/// A decoded proportional font.
pub struct Font {
    height: usize,
    /// Absolute byte offset of each glyph within `data`.
    offsets: [usize; 256],
    /// Pixel width of each glyph.
    widths: [usize; 256],
    /// The whole expanded font chunk (offsets index into this).
    data: Vec<u8>,
}

impl Font {
    /// Decode font `n` (0 = STARTFONT, the larger menu font; 1 = the smaller).
    pub fn load(vga: &VgaGraph, n: usize) -> Self {
        let data = vga.raw_chunk(crate::assets::vgagraph::STARTFONT + n);
        let height = i16::from_le_bytes([data[0], data[1]]) as usize;
        let mut offsets = [0usize; 256];
        let mut widths = [0usize; 256];
        for (c, off) in offsets.iter_mut().enumerate() {
            let p = 2 + c * 2;
            *off = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
        }
        for (c, w) in widths.iter_mut().enumerate() {
            *w = data[2 + 512 + c] as usize;
        }
        Self { height, offsets, widths, data }
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Total pixel width `text` will occupy.
    pub fn text_width(&self, text: &str) -> usize {
        text.bytes().map(|b| self.widths[b as usize]).sum()
    }

    /// Draw `text` with its top-left at (`x`, `y`) in palette color `color`,
    /// clipped to the framebuffer. Returns the pen x after the last glyph.
    pub fn draw(&self, fb: &mut Framebuffer, x: i32, y: i32, text: &str, color: u8) -> i32 {
        let rgba = PALETTE[color as usize];
        let mut pen = x;
        for b in text.bytes() {
            let c = b as usize;
            let w = self.widths[c];
            let base = self.offsets[c];
            for row in 0..self.height {
                let py = y + row as i32;
                if py < 0 || py as usize >= HEIGHT {
                    continue;
                }
                let row_off = base + row * w;
                for col in 0..w {
                    let px = pen + col as i32;
                    if px < 0 || px as usize >= WIDTH {
                        continue;
                    }
                    if self.data[row_off + col] != 0 {
                        fb.pixels[py as usize * WIDTH + px as usize] = rgba;
                    }
                }
            }
            pen += w as i32;
        }
        pen
    }

    /// Draw `text` centered horizontally on the screen at row `y`.
    pub fn draw_centered(&self, fb: &mut Framebuffer, y: i32, text: &str, color: u8) {
        let x = (WIDTH as i32 - self.text_width(text) as i32) / 2;
        self.draw(fb, x, y, text, color);
    }
}

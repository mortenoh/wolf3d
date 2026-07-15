//! CPU framebuffer: packed `u32` pixels, row-major, uploaded to the GPU once
//! per frame as an RGBA8 texture. Same layout as rust_demoscene: on
//! little-endian the u32 `0xAABBGGRR` sits in memory as bytes R, G, B, A.

/// Internal render resolution — the original game's 320x200 (displayed 4:3).
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;

/// Pack an opaque RGB color into the framebuffer layout (`0xFFBBGGRR`).
#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | 0xFF00_0000
}

/// Multiply a packed color by `f` in [0,1] (cheap shading, no unpack helpers needed).
#[inline]
pub fn darken(c: u32, f: f32) -> u32 {
    let r = ((c & 0xFF) as f32 * f) as u32;
    let g = (((c >> 8) & 0xFF) as f32 * f) as u32;
    let b = (((c >> 16) & 0xFF) as f32 * f) as u32;
    r | (g << 8) | (b << 16) | 0xFF00_0000
}

pub struct Framebuffer {
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new() -> Self {
        Self {
            pixels: vec![rgb(0, 0, 0); WIDTH * HEIGHT],
        }
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.pixels)
    }
}

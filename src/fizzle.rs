//! ID_VH.C `FizzleFade`: the 17-bit LFSR dissolve used when the player dies
//! (WL_GAME.C `Died` fills the view with palette index 4, then fizzles it on).
//!
//! The generator is a maximum-length Galois LFSR over 17 bits with taps on
//! bits 17 and 14 (XOR mask `0x0001_2000`). Coordinates pack as y = (low 8
//! bits) - 1 and x = next 9 bits, which is enough for the classic 320x200
//! framebuffer.

use crate::assets::palette::PALETTE;

/// Frames argument `Died` passes to `FizzleFade` (70 tics at 70 Hz ≈ 1 s of
/// throughput budgeting; the full LFSR period is longer because out-of-range
/// samples still consume the per-frame pixel budget).
pub const FIZZLE_FRAMES: u32 = 70;

/// Full-screen pixel count the original divides by `frames` for the budget,
/// regardless of the actual fade rectangle.
pub const FIZZLE_FULL_PIXELS: u32 = 64_000;

/// Pixels of LFSR throughput per tic (`64000 / 70`).
pub const FIZZLE_PIX_PER_FRAME: u32 = FIZZLE_FULL_PIXELS / FIZZLE_FRAMES; // 914

/// XOR mask for the 17-bit death/fizzle LFSR (taps 17, 14).
const RND_MASK: u32 = 0x0001_2000;

/// Palette index 4 — the red `VW_Bar` colour `Died` fizzles onto the view.
pub const DEATH_RED_INDEX: usize = 4;

/// Packed RGBA for palette index 4 (`rgb(170, 0, 0)`).
pub fn death_red() -> u32 {
    PALETTE[DEATH_RED_INDEX]
}

/// One in-flight FizzleFade over a view rectangle of `width` x `height`.
#[derive(Clone, Debug)]
pub struct Fizzle {
    rndval: u32,
    finished: bool,
    width: u32,
    height: u32,
    /// View-local coverage: `painted[y * width + x]` is true once dissolved.
    painted: Vec<bool>,
    /// Fractional leftover from dt / TIC * pix-per-frame.
    pixel_accum: f32,
}

impl Fizzle {
    /// Start a new dissolve for a `width` x `height` rectangle (view pixels).
    pub fn new(width: usize, height: usize) -> Self {
        let w = width.max(1);
        let h = height.max(1);
        Self {
            rndval: 1,
            finished: false,
            width: w as u32,
            height: h as u32,
            painted: vec![false; w * h],
            pixel_accum: 0.0,
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn width(&self) -> usize {
        self.width as usize
    }

    pub fn height(&self) -> usize {
        self.height as usize
    }

    /// True when view-local pixel `(x, y)` has been dissolved to red.
    #[inline]
    pub fn is_painted(&self, x: usize, y: usize) -> bool {
        if x >= self.width as usize || y >= self.height as usize {
            return false;
        }
        self.painted[y * self.width as usize + x]
    }

    /// Advance the LFSR by the throughput that `dt` seconds of game time buys
    /// (914 samples per tic). Returns how many in-bounds pixels were newly
    /// marked this call.
    pub fn advance(&mut self, dt: f32, tic: f32) -> u32 {
        if self.finished {
            return 0;
        }
        let tics = if tic > 0.0 { dt / tic } else { 0.0 };
        self.pixel_accum += tics * FIZZLE_PIX_PER_FRAME as f32;
        let steps = self.pixel_accum.floor() as u32;
        self.pixel_accum -= steps as f32;
        self.step_n(steps)
    }

    /// Run exactly `count` LFSR samples (including out-of-range skips).
    pub fn step_n(&mut self, count: u32) -> u32 {
        let mut painted = 0u32;
        for _ in 0..count {
            if self.finished {
                break;
            }
            if self.step_one() {
                painted += 1;
            }
        }
        painted
    }

    /// One LFSR sample. Returns true if an in-bounds pixel was marked.
    fn step_one(&mut self) -> bool {
        // ID_VH.C: y = low 8 bits - 1; x = next 9 bits.
        let y = (self.rndval as u8).wrapping_sub(1) as u32;
        let x = (self.rndval >> 8) & 0x1FF;

        // Galois right-shift; XOR when the shifted-out bit was 1.
        let lsb = self.rndval & 1;
        self.rndval >>= 1;
        if lsb != 0 {
            self.rndval ^= RND_MASK;
        }
        if self.rndval == 1 {
            self.finished = true;
        }

        // Original skips with `x > width || y > height` (inclusive upper bound).
        // For a modern tightly-sized buffer we paint when `x < width && y < height`,
        // which the 17-bit packing still fully covers for 320x200 and 320x160.
        if x < self.width && y < self.height {
            let idx = (y * self.width + x) as usize;
            if !self.painted[idx] {
                self.painted[idx] = true;
                return true;
            }
        }
        false
    }

    /// Paint every dissolved pixel of this fade into `fb` at origin `(ox, oy)`.
    pub fn blit_red(&self, fb: &mut crate::fb::Framebuffer, ox: usize, oy: usize) {
        let red = death_red();
        let fb_w = crate::fb::WIDTH;
        let fb_h = crate::fb::HEIGHT;
        let w = self.width as usize;
        let h = self.height as usize;
        for y in 0..h {
            let dy = oy + y;
            if dy >= fb_h {
                break;
            }
            let row = y * w;
            for x in 0..w {
                if !self.painted[row + x] {
                    continue;
                }
                let dx = ox + x;
                if dx >= fb_w {
                    break;
                }
                fb.pixels[dy * fb_w + dx] = red;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfsr_covers_full_320x160_view() {
        let mut f = Fizzle::new(320, 160);
        // Full period is 2^17 - 1 steps.
        f.step_n(131_071);
        assert!(f.finished());
        for y in 0..160 {
            for x in 0..320 {
                assert!(f.is_painted(x, y), "missing pixel {x},{y}");
            }
        }
    }

    #[test]
    fn first_pixel_is_origin() {
        let mut f = Fizzle::new(320, 160);
        assert_eq!(f.step_n(1), 1);
        assert!(f.is_painted(0, 0));
        // A single step must not finish the period.
        assert!(!f.finished());
    }

    #[test]
    fn pix_per_frame_matches_original() {
        assert_eq!(FIZZLE_PIX_PER_FRAME, 914);
    }
}

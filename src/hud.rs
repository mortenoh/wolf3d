//! The classic status-bar HUD and the first-person weapon overlay.
//!
//! The 3D view is letterboxed to the top [`VIEW_H`] rows; the bottom 40 rows
//! hold the 320x40 STATUSBARPIC with live values drawn onto it. Coordinates
//! come straight from WOLFSRC/WL_AGENT.C: the status routines take an x in
//! "byte" units (8 screen pixels each) and a y relative to the bar's top, so a
//! call `StatusDrawPic(x, y, pic)` lands at pixel `(x * 8, y + VIEW_H)`. HUD
//! numbers are 8px-wide digit pics stepped one byte-cell apart.

use crate::assets::vgagraph::{
    self, FACE1APIC, FACE8APIC, GOLDKEYPIC, N_0PIC, NOKEYPIC, SILVERKEYPIC, STATUSBARPIC,
    VgaGraph,
};
use crate::assets::vswap::{TEX_SIZE, VSwap};
use crate::fb::{Framebuffer, WIDTH};

/// The 3D view occupies the top rows; the status bar fills the rest.
pub const VIEW_H: usize = 160;
const BAR_TOP: usize = VIEW_H;

/// Key bitflags in `Game::keys` (matches Wolf's `gamestate.keys`).
pub const KEY_GOLD: u8 = 1;
pub const KEY_SILVER: u8 = 2;

/// One byte-cell of horizontal HUD space = 8 screen pixels.
const CELL: usize = 8;

/// Live values the HUD paints onto the bar.
pub struct HudState {
    pub floor: i32,
    pub score: i32,
    pub lives: i32,
    pub health: i32,
    pub ammo: i32,
    pub keys: u8,
}

/// Pre-decoded HUD pictures, built once from the VGAGRAPH archive.
pub struct Hud {
    statusbar: vgagraph::Picture,
    digits: Vec<vgagraph::Picture>,
    /// FACE1APIC..=FACE8APIC, indexed by `chunk - FACE1APIC`.
    faces: Vec<vgagraph::Picture>,
    no_key: vgagraph::Picture,
    gold_key: vgagraph::Picture,
    silver_key: vgagraph::Picture,
}

impl Hud {
    pub fn new(vga: &VgaGraph) -> Self {
        Self {
            statusbar: vga.pic(STATUSBARPIC),
            digits: (0..10).map(|d| vga.pic(N_0PIC + d)).collect(),
            faces: (FACE1APIC..=FACE8APIC).map(|c| vga.pic(c)).collect(),
            no_key: vga.pic(NOKEYPIC),
            gold_key: vga.pic(GOLDKEYPIC),
            silver_key: vga.pic(SILVERKEYPIC),
        }
    }

    /// Draw the status bar and its live values into the bottom rows.
    pub fn draw(&self, fb: &mut Framebuffer, s: &HudState) {
        blit(fb, &self.statusbar, 0, BAR_TOP);

        // LatchNumber(x, y, width, value) calls from WL_AGENT.C.
        self.number(fb, 2, 16, 2, s.floor);
        self.number(fb, 6, 16, 6, s.score);
        self.number(fb, 14, 16, 1, s.lives);
        self.number(fb, 21, 16, 3, s.health);
        self.number(fb, 27, 16, 2, s.ammo);

        self.face(fb, s.health);
        self.keys(fb, s.keys);
    }

    /// Right-aligned number in a `width`-cell field starting at byte `x`, bar
    /// row `y` (DrawHealth/DrawScore/… semantics).
    fn number(&self, fb: &mut Framebuffer, x: usize, y: usize, width: usize, value: i32) {
        let text = value.max(0).to_string();
        let digits: Vec<usize> = text.bytes().map(|b| (b - b'0') as usize).collect();
        // Show the low `width` digits, right-aligned; leading cells stay blank
        // (the bar's own background already shows there).
        let shown = digits.len().min(width);
        let start_cell = width - shown;
        let first = digits.len() - shown;
        for (i, &d) in digits[first..].iter().enumerate() {
            let px = (x + start_cell + i) * CELL;
            blit(fb, &self.digits[d], px, y + BAR_TOP);
        }
    }

    /// DrawFace: pick the pic by health bracket, center look frame (B).
    fn face(&self, fb: &mut Framebuffer, health: i32) {
        let pic = if health > 0 {
            let bracket = ((100 - health.min(100)) / 16) as usize; // 0..=6
            FACE1APIC + 3 * bracket + 1
        } else {
            FACE8APIC // death face
        };
        blit(fb, &self.faces[pic - FACE1APIC], 17 * CELL, 4 + BAR_TOP);
    }

    /// DrawKeys: gold slot (top), silver slot (bottom); blank when not held.
    fn keys(&self, fb: &mut Framebuffer, keys: u8) {
        let gold = if keys & KEY_GOLD != 0 { &self.gold_key } else { &self.no_key };
        let silver = if keys & KEY_SILVER != 0 { &self.silver_key } else { &self.no_key };
        blit(fb, gold, 30 * CELL, 4 + BAR_TOP);
        blit(fb, silver, 30 * CELL, 20 + BAR_TOP);
    }
}

/// Copy an opaque picture into the framebuffer at pixel (dx, dy), clipped.
fn blit(fb: &mut Framebuffer, pic: &vgagraph::Picture, dx: usize, dy: usize) {
    for row in 0..pic.height {
        let y = dy + row;
        if y >= crate::fb::HEIGHT {
            break;
        }
        for col in 0..pic.width {
            let x = dx + col;
            if x >= WIDTH {
                break;
            }
            fb.pixels[y * WIDTH + x] = pic.pixels[row * pic.width + col];
        }
    }
}

/// Draw the current weapon's ready sprite (VSWAP, 64x64, column-major) scaled
/// 2x, centered horizontally, its base resting on the bottom of the 3D view.
/// Transparent texels (0) are skipped.
pub fn draw_weapon(fb: &mut Framebuffer, vswap: &VSwap, sprite: usize) {
    const SCALE: usize = 2;
    let dim = TEX_SIZE * SCALE; // 128
    let x0 = (WIDTH - dim) / 2; // centered
    let y0 = VIEW_H - dim; // base at row VIEW_H
    let tex = &vswap.sprites[sprite];
    for py in 0..dim {
        let sy = py / SCALE;
        let dest_y = y0 + py;
        for px in 0..dim {
            let sx = px / SCALE;
            let c = tex[sx * TEX_SIZE + sy]; // sprites are column-major
            if c != 0 {
                fb.pixels[dest_y * WIDTH + x0 + px] = c;
            }
        }
    }
}

/// VSWAP sprite index of a weapon's ready frame. From WOLFSRC/WL_DEF.H: the
/// player attack frames are the last 20 sprites — READY + 4 attack frames per
/// weapon, in knife/pistol/machinegun/chaingun order.
pub fn weapon_ready_sprite(vswap: &VSwap, weapon: usize) -> usize {
    vswap.sprites.len() - 20 + weapon * 5
}

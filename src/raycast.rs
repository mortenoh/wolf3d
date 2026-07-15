//! The raycaster: one DDA ray per screen column over a grid map.
//!
//! Step 1 uses a hardcoded map and flat-colored walls. Step 2 swaps in the
//! real GAMEMAPS levels and VSWAP textures without touching the DDA itself.
//!
//! Key choices (see rust_demoscene 0602_raycaster for the long version):
//! - Ray directions are `dir + plane * camera_x`, and the strip height divides
//!   by the *perpendicular* distance (projection onto `dir`), not the Euclidean
//!   ray length — that is the fisheye fix.
//! - N/S-facing walls render darker than E/W ones, the classic Wolf depth cue.

use crate::fb::{Framebuffer, HEIGHT, WIDTH, darken, rgb};

// =============================================================================
// MAP
// =============================================================================

pub const MAP_W: usize = 24;
pub const MAP_H: usize = 24;

/// '.' = empty, digits = wall tile kinds (different colors until real textures).
#[rustfmt::skip]
const MAP: [&str; MAP_H] = [
    "111111111111111111111111",
    "1......................1",
    "1..2222..2222..33333...1",
    "1..2........2..3...3...1",
    "1..2........2..3...3...1",
    "1..2222..2222..33.33...1",
    "1......................1",
    "1......................1",
    "1..4444444444..111111..1",
    "1..4........4..1....1..1",
    "1..4........4..1....1..1",
    "1..4...44...........1..1",
    "1..4...44...........1..1",
    "1..4........4..1....1..1",
    "1..4........4..1....1..1",
    "1..4444444444..111111..1",
    "1......................1",
    "1......................1",
    "1..33..22..44..33..22..1",
    "1......................1",
    "1..2........33.........1",
    "1..2........33.........1",
    "1......................1",
    "111111111111111111111111",
];

#[inline]
pub fn tile(x: i32, y: i32) -> u8 {
    if x < 0 || y < 0 || x >= MAP_W as i32 || y >= MAP_H as i32 {
        return 1; // out of bounds is solid — the DDA can never escape
    }
    let c = MAP[y as usize].as_bytes()[x as usize];
    if c == b'.' { 0 } else { c - b'0' }
}

fn wall_color(kind: u8) -> u32 {
    match kind {
        1 => rgb(96, 96, 112),  // gray stone
        2 => rgb(72, 96, 160),  // blue brick
        3 => rgb(140, 84, 48),  // wood
        4 => rgb(120, 120, 132),// light stone
        _ => rgb(200, 60, 200), // missing-texture magenta
    }
}

// =============================================================================
// PLAYER
// =============================================================================

pub struct Player {
    pub x: f32,
    pub y: f32,
    /// Facing angle in radians; 0 = +x, grows counterclockwise in map space.
    pub angle: f32,
}

const PLAYER_RADIUS: f32 = 0.25;

impl Player {
    pub fn new() -> Self {
        // Cell-centered in the open east-west corridor at row 17, facing east.
        Self { x: 2.5, y: 17.5, angle: 0.0 }
    }

    /// Move by (dx, dy) in map units, sliding along walls: each axis is applied
    /// independently and rejected only if it would put the player's radius
    /// inside a solid cell.
    pub fn walk(&mut self, dx: f32, dy: f32) {
        let nx = self.x + dx;
        if !occupied(nx, self.y) {
            self.x = nx;
        }
        let ny = self.y + dy;
        if !occupied(self.x, ny) {
            self.y = ny;
        }
    }
}

/// Is a player-sized circle at (x, y) overlapping any solid cell?
fn occupied(x: f32, y: f32) -> bool {
    let x0 = (x - PLAYER_RADIUS).floor() as i32;
    let x1 = (x + PLAYER_RADIUS).floor() as i32;
    let y0 = (y - PLAYER_RADIUS).floor() as i32;
    let y1 = (y + PLAYER_RADIUS).floor() as i32;
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            if tile(cx, cy) != 0 {
                return true;
            }
        }
    }
    false
}

// =============================================================================
// RENDER
// =============================================================================

/// Half the horizontal FOV as the camera-plane length: 0.66 ≈ Wolf's ~66°.
const PLANE_LEN: f32 = 0.66;

const CEILING: u32 = rgb(56, 56, 56); // Wolf's solid ceiling gray
const FLOOR: u32 = rgb(112, 112, 112);

pub fn render(fb: &mut Framebuffer, p: &Player) {
    // Ceiling / floor halves.
    fb.pixels[..WIDTH * (HEIGHT / 2)].fill(CEILING);
    fb.pixels[WIDTH * (HEIGHT / 2)..].fill(FLOOR);

    let (dir_x, dir_y) = (p.angle.cos(), p.angle.sin());
    let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);

    for col in 0..WIDTH {
        // camera_x sweeps -1 (left edge) .. +1 (right edge).
        let camera_x = 2.0 * col as f32 / WIDTH as f32 - 1.0;
        let ray_x = dir_x + plane_x * camera_x;
        let ray_y = dir_y + plane_y * camera_x;

        // --- DDA ---
        let mut map_x = p.x.floor() as i32;
        let mut map_y = p.y.floor() as i32;
        let delta_x = if ray_x == 0.0 { f32::MAX } else { (1.0 / ray_x).abs() };
        let delta_y = if ray_y == 0.0 { f32::MAX } else { (1.0 / ray_y).abs() };
        let (step_x, mut side_x) = if ray_x < 0.0 {
            (-1, (p.x - map_x as f32) * delta_x)
        } else {
            (1, (map_x as f32 + 1.0 - p.x) * delta_x)
        };
        let (step_y, mut side_y) = if ray_y < 0.0 {
            (-1, (p.y - map_y as f32) * delta_y)
        } else {
            (1, (map_y as f32 + 1.0 - p.y) * delta_y)
        };

        let mut hit = 0u8;
        let mut side_ns = false; // true = crossed a horizontal border (N/S face)
        while hit == 0 {
            if side_x < side_y {
                side_x += delta_x;
                map_x += step_x;
                side_ns = false;
            } else {
                side_y += delta_y;
                map_y += step_y;
                side_ns = true;
            }
            hit = tile(map_x, map_y);
        }

        // Perpendicular distance: total side distance minus the last step.
        let perp = if side_ns { side_y - delta_y } else { side_x - delta_x };
        let perp = perp.max(1e-4);

        let line_h = (HEIGHT as f32 / perp) as i32;
        let y0 = ((HEIGHT as i32 - line_h) / 2).max(0) as usize;
        let y1 = ((HEIGHT as i32 + line_h) / 2).min(HEIGHT as i32) as usize;

        let mut color = wall_color(hit);
        if side_ns {
            color = darken(color, 0.7);
        }

        for y in y0..y1 {
            fb.pixels[y * WIDTH + col] = color;
        }
    }
}

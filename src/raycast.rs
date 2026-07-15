//! The raycaster: one DDA ray per screen column over the level grid, now
//! textured from VSWAP.
//!
//! Key choices (see rust_demoscene 0602_raycaster for the long version):
//! - Ray directions are `dir + plane * camera_x`, and the strip height divides
//!   by the *perpendicular* distance (projection onto `dir`), not the Euclidean
//!   ray length — that is the fisheye fix.
//! - Wall texture u is the fractional hit position along the wall; v steps
//!   from the *unclamped* strip top so near walls don't crawl.
//! - VSWAP wall chunks come in light/dark pairs; N/S faces take the even
//!   (light) chunk, E/W the odd one — the original's side shading, for free.

use crate::assets::maps::{Level, MAP_SIZE};
use crate::assets::vswap::{TEX_SIZE, VSwap};
use crate::fb::{Framebuffer, HEIGHT, WIDTH, rgb};

// =============================================================================
// TILE SEMANTICS (plane 0)
// =============================================================================

const MAX_WALL: u16 = 89;
const DOOR_FIRST: u16 = 90;
const DOOR_LAST: u16 = 101;

#[inline]
fn is_wall(t: u16) -> bool {
    (1..=MAX_WALL).contains(&t)
}

#[inline]
fn is_door(t: u16) -> bool {
    (DOOR_FIRST..=DOOR_LAST).contains(&t)
}

/// Solid for both rays and movement. Doors count as solid until they get
/// open/close logic.
#[inline]
fn is_solid(t: u16) -> bool {
    is_wall(t) || is_door(t)
}

#[inline]
fn tile(level: &Level, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
        return 1; // out of bounds is solid — the DDA can never escape
    }
    level.plane0[y as usize * MAP_SIZE + x as usize]
}

// =============================================================================
// PLAYER
// =============================================================================

pub struct Player {
    pub x: f32,
    pub y: f32,
    /// Facing angle in radians; 0 = +x (east), grows toward +y (south).
    pub angle: f32,
}

const PLAYER_RADIUS: f32 = 0.25;

/// Plane-1 spawn markers 19..=22 = player start facing N, E, S, W.
pub fn find_spawn(level: &Level) -> Player {
    for y in 0..MAP_SIZE {
        for x in 0..MAP_SIZE {
            let obj = level.plane1[y * MAP_SIZE + x];
            if (19..=22).contains(&obj) {
                use std::f32::consts::{FRAC_PI_2, PI};
                let angle = match obj {
                    19 => -FRAC_PI_2, // north = -y
                    20 => 0.0,        // east = +x
                    21 => FRAC_PI_2,  // south
                    _ => PI,          // west
                };
                return Player { x: x as f32 + 0.5, y: y as f32 + 0.5, angle };
            }
        }
    }
    panic!("level {:?} has no player spawn", level.name);
}

impl Player {
    /// Move by (dx, dy) in map units, sliding along walls: each axis is applied
    /// independently and rejected only if it would put the player's radius
    /// inside a solid cell.
    pub fn walk(&mut self, level: &Level, dx: f32, dy: f32) {
        let nx = self.x + dx;
        if !occupied(level, nx, self.y) {
            self.x = nx;
        }
        let ny = self.y + dy;
        if !occupied(level, self.x, ny) {
            self.y = ny;
        }
    }
}

/// Is a player-sized circle at (x, y) overlapping any solid cell?
fn occupied(level: &Level, x: f32, y: f32) -> bool {
    let x0 = (x - PLAYER_RADIUS).floor() as i32;
    let x1 = (x + PLAYER_RADIUS).floor() as i32;
    let y0 = (y - PLAYER_RADIUS).floor() as i32;
    let y1 = (y + PLAYER_RADIUS).floor() as i32;
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            if is_solid(tile(level, cx, cy)) {
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

/// Wolf's flat ceiling/floor colors: VGA palette 0x1d and 0x19.
const CEILING: u32 = rgb(56, 56, 56);
const FLOOR: u32 = rgb(112, 112, 112);

/// Wall texture chunk for a hit: walls pair up as (tile-1)*2, +1 on E/W
/// (vertical) faces — the original's horizwall/vertwall tables. Doors use the
/// dedicated pair near the end of the wall chunks.
fn texture_for(vswap: &VSwap, t: u16, side_ns: bool) -> usize {
    let base = if is_door(t) {
        vswap.door_texture()
    } else {
        (t as usize - 1) * 2
    };
    base + !side_ns as usize
}

pub fn render(fb: &mut Framebuffer, vswap: &VSwap, level: &Level, p: &Player) {
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

        let mut hit = 0u16;
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
            let t = tile(level, map_x, map_y);
            if is_solid(t) {
                hit = t;
            }
        }

        // Perpendicular distance: total side distance minus the last step.
        let perp = if side_ns { side_y - delta_y } else { side_x - delta_x };
        let perp = perp.max(1e-4);

        // Fractional hit position along the wall = texture u.
        let wall_x = if side_ns { p.x + perp * ray_x } else { p.y + perp * ray_y };
        let wall_frac = wall_x - wall_x.floor();
        let mut tex_u = (wall_frac * TEX_SIZE as f32) as usize & (TEX_SIZE - 1);
        // Mirror so textures read left-to-right on the faces we approach.
        if (!side_ns && ray_x > 0.0) || (side_ns && ray_y < 0.0) {
            tex_u = TEX_SIZE - 1 - tex_u;
        }

        let line_h = HEIGHT as f32 / perp;
        let top = (HEIGHT as f32 - line_h) / 2.0;
        let y0 = top.max(0.0) as usize;
        let y1 = ((HEIGHT as f32 + line_h) / 2.0).min(HEIGHT as f32) as usize;

        let texture = &vswap.walls[texture_for(vswap, hit, side_ns)];
        let column = &texture[tex_u * TEX_SIZE..(tex_u + 1) * TEX_SIZE];

        // v steps from the UNCLAMPED strip top so oversize strips stay put.
        let v_step = TEX_SIZE as f32 / line_h;
        let mut v = (y0 as f32 - top) * v_step;
        for y in y0..y1 {
            fb.pixels[y * WIDTH + col] = column[(v as usize).min(TEX_SIZE - 1)];
            v += v_step;
        }
    }
}

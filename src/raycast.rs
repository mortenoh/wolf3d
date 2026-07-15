//! The raycaster and world state: one DDA ray per screen column over the
//! level grid, textured walls and doors from VSWAP, sprites z-tested against
//! the wall depth buffer.
//!
//! Key choices (see rust_demoscene 0602_raycaster for the long version):
//! - Ray directions are `dir + plane * camera_x`, and the strip height divides
//!   by the *perpendicular* distance (projection onto `dir`), not the Euclidean
//!   ray length — that is the fisheye fix. With this normalization the DDA's
//!   running side distances ARE perpendicular distances, so the cell-entry
//!   distance and the door-slab intersection need no extra correction.
//! - Wall texture u is the fractional hit position along the wall; v steps
//!   from the *unclamped* strip top so near walls don't crawl.
//! - VSWAP wall chunks come in light/dark pairs; N/S faces take the even
//!   (light) chunk, E/W the odd one — the original's side shading, for free.
//! - Doors are a slab on the tile's center line: the ray intersects the plane
//!   x+0.5 (vertical doors) or y+0.5, giving the half-tile recess for free.
//!   An open fraction shifts both the solid test and the texture, so the door
//!   visually slides into the wall.

use crate::assets::maps::{Level, MAP_SIZE};
use crate::assets::vswap::{TEX_SIZE, VSwap};
use crate::fb::{Framebuffer, WIDTH, rgb};

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

#[inline]
fn tile(level: &Level, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
        return 1; // out of bounds is solid — the DDA can never escape
    }
    level.plane0[y as usize * MAP_SIZE + x as usize]
}

// =============================================================================
// DOORS
// =============================================================================

const DOOR_OPEN_TIME: f32 = 0.9; // seconds edge-to-edge, ~64 tics at 70Hz
const DOOR_HOLD_TIME: f32 = 4.3; // ~300 tics before auto-close
/// A door must be nearly fully open before you can walk through.
const DOOR_PASSABLE: f32 = 0.9;

#[derive(PartialEq)]
enum DoorState {
    Closed,
    Opening,
    Open { hold: f32 },
    Closing,
}

pub struct Door {
    pub x: i32,
    pub y: i32,
    /// Vertical doors sit on the x+0.5 plane and are crossed moving east-west.
    pub vertical: bool,
    /// Offset into the 8 door texture chunks: 0 normal, 4 elevator, 6 locked.
    tex_base: usize,
    /// Open fraction: 0 = closed, 1 = fully slid into the wall.
    pub position: f32,
    state: DoorState,
}

impl Door {
    fn tick(&mut self, dt: f32, player_on_tile: bool) {
        match self.state {
            DoorState::Opening => {
                self.position += dt / DOOR_OPEN_TIME;
                if self.position >= 1.0 {
                    self.position = 1.0;
                    self.state = DoorState::Open { hold: DOOR_HOLD_TIME };
                }
            }
            DoorState::Open { ref mut hold } => {
                if player_on_tile {
                    *hold = DOOR_HOLD_TIME; // don't close on top of the player
                } else {
                    *hold -= dt;
                    if *hold <= 0.0 {
                        self.state = DoorState::Closing;
                    }
                }
            }
            DoorState::Closing => {
                if player_on_tile {
                    self.state = DoorState::Opening; // reopen rather than trap
                } else {
                    self.position -= dt / DOOR_OPEN_TIME;
                    if self.position <= 0.0 {
                        self.position = 0.0;
                        self.state = DoorState::Closed;
                    }
                }
            }
            DoorState::Closed => {}
        }
    }
}

// =============================================================================
// WORLD — level + doors + the static objects spawned from plane 1
// =============================================================================

/// Plane-1 codes 23..=70 spawn static objects; sprite SPR_STAT_0 is sprite 2
/// (after the demo/deathcam sprites).
const STATIC_FIRST: u16 = 23;
const STATIC_LAST: u16 = 70;
const SPR_STAT_0: usize = 2;

/// Spawn codes whose object blocks movement (`bo_block` in WL_ACT1.C's
/// statinfo table): barrels, tables, pillars, trees, wells, and so on.
const BLOCKING_STATICS: [u16; 21] = [
    24, 25, 26, 28, 30, 31, 33, 34, 35, 36, 39, 40, 41, 45, 58, 59, 60, 62, 63, 68, 69,
];

pub struct StaticSprite {
    pub x: f32,
    pub y: f32,
    /// Index into `VSwap::sprites`.
    pub sprite: usize,
}

pub struct World {
    pub level: Level,
    pub statics: Vec<StaticSprite>,
    pub doors: Vec<Door>,
    /// Per-tile door index + 1 (0 = no door).
    door_grid: Vec<u8>,
    /// Tiles blocked by a solid static object.
    blocked: Vec<bool>,
}

impl World {
    pub fn new(level: Level) -> Self {
        let mut statics = Vec::new();
        let mut blocked = vec![false; MAP_SIZE * MAP_SIZE];
        let mut doors = Vec::new();
        let mut door_grid = vec![0u8; MAP_SIZE * MAP_SIZE];

        for (i, &obj) in level.plane1.iter().enumerate() {
            if (STATIC_FIRST..=STATIC_LAST).contains(&obj) {
                statics.push(StaticSprite {
                    x: (i % MAP_SIZE) as f32 + 0.5,
                    y: (i / MAP_SIZE) as f32 + 0.5,
                    sprite: SPR_STAT_0 + (obj - STATIC_FIRST) as usize,
                });
                blocked[i] = BLOCKING_STATICS.contains(&obj);
            }
        }
        for (i, &t) in level.plane0.iter().enumerate() {
            if is_door(t) {
                door_grid[i] = (doors.len() + 1) as u8;
                doors.push(Door {
                    x: (i % MAP_SIZE) as i32,
                    y: (i / MAP_SIZE) as i32,
                    vertical: t % 2 == 0,
                    tex_base: match t {
                        90 | 91 => 0,       // normal
                        100 | 101 => 4,     // elevator
                        _ => 6,             // locked (gold/silver)
                    },
                    position: 0.0,
                    state: DoorState::Closed,
                });
            }
        }
        Self { level, statics, doors, door_grid, blocked }
    }

    fn door_at(&self, x: i32, y: i32) -> Option<&Door> {
        if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
            return None;
        }
        let idx = self.door_grid[y as usize * MAP_SIZE + x as usize];
        (idx != 0).then(|| &self.doors[idx as usize - 1])
    }

    /// Advance door animations.
    pub fn tick(&mut self, dt: f32, p: &Player) {
        let (px, py) = (p.x.floor() as i32, p.y.floor() as i32);
        for d in &mut self.doors {
            d.tick(dt, d.x == px && d.y == py);
        }
    }

    /// The use key: open/close the door in the tile the player faces.
    pub fn use_door(&mut self, p: &Player) {
        let tx = (p.x + p.angle.cos()).floor() as i32;
        let ty = (p.y + p.angle.sin()).floor() as i32;
        let on_it = tx == p.x.floor() as i32 && ty == p.y.floor() as i32;
        if let Some(idx) = self
            .door_at(tx, ty)
            .map(|d| self.door_grid[d.y as usize * MAP_SIZE + d.x as usize])
        {
            let d = &mut self.doors[idx as usize - 1];
            d.state = match d.state {
                DoorState::Closed | DoorState::Closing => DoorState::Opening,
                _ if on_it => DoorState::Opening,
                _ => DoorState::Closing,
            };
        }
    }

    /// Solid for movement: walls, closed-enough doors, blocking statics.
    fn blocks_move(&self, x: i32, y: i32) -> bool {
        let t = tile(&self.level, x, y);
        if is_door(t) {
            return self
                .door_at(x, y)
                .is_none_or(|d| d.position < DOOR_PASSABLE);
        }
        is_wall(t)
            || (x >= 0
                && y >= 0
                && (x as usize) < MAP_SIZE
                && (y as usize) < MAP_SIZE
                && self.blocked[y as usize * MAP_SIZE + x as usize])
    }
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
    pub fn walk(&mut self, world: &World, dx: f32, dy: f32) {
        let nx = self.x + dx;
        if !occupied(world, nx, self.y) {
            self.x = nx;
        }
        let ny = self.y + dy;
        if !occupied(world, self.x, ny) {
            self.y = ny;
        }
    }
}

/// Is a player-sized circle at (x, y) overlapping any solid cell?
fn occupied(world: &World, x: f32, y: f32) -> bool {
    let x0 = (x - PLAYER_RADIUS).floor() as i32;
    let x1 = (x + PLAYER_RADIUS).floor() as i32;
    let y0 = (y - PLAYER_RADIUS).floor() as i32;
    let y1 = (y + PLAYER_RADIUS).floor() as i32;
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            if world.blocks_move(cx, cy) {
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

/// Wolf's flat ceiling/floor colors.
const CEILING: u32 = rgb(56, 56, 56);
const FLOOR: u32 = rgb(112, 112, 112);

/// What a column's ray hit: perpendicular distance, texture chunk, texture u.
struct Hit {
    perp: f32,
    texture: usize,
    tex_u: usize,
}

fn cast(world: &World, vswap: &VSwap, px: f32, py: f32, ray_x: f32, ray_y: f32) -> Hit {
    let level = &world.level;
    let mut map_x = px.floor() as i32;
    let mut map_y = py.floor() as i32;
    let delta_x = if ray_x == 0.0 { f32::MAX } else { (1.0 / ray_x).abs() };
    let delta_y = if ray_y == 0.0 { f32::MAX } else { (1.0 / ray_y).abs() };
    let (step_x, mut side_x) = if ray_x < 0.0 {
        (-1, (px - map_x as f32) * delta_x)
    } else {
        (1, (map_x as f32 + 1.0 - px) * delta_x)
    };
    let (step_y, mut side_y) = if ray_y < 0.0 {
        (-1, (py - map_y as f32) * delta_y)
    } else {
        (1, (map_y as f32 + 1.0 - py) * delta_y)
    };

    let mut prev_door = false;
    loop {
        // Step into the next cell; the boundary crossed on the way in is at
        // perpendicular distance `enter`, N/S when we crossed a y-boundary.
        let (enter, side_ns) = if side_x < side_y {
            let e = side_x;
            side_x += delta_x;
            map_x += step_x;
            (e, false)
        } else {
            let e = side_y;
            side_y += delta_y;
            map_y += step_y;
            (e, true)
        };

        if let Some(door) = world.door_at(map_x, map_y) {
            // Intersect the door slab on the tile's center line.
            let slab = if door.vertical {
                (map_x as f32 + 0.5 - px) / ray_x
            } else {
                (map_y as f32 + 0.5 - py) / ray_y
            };
            let exit = side_x.min(side_y);
            if slab.is_finite() && slab >= enter - 1e-6 && slab <= exit + 1e-6 {
                let along = if door.vertical {
                    py + slab * ray_y - map_y as f32
                } else {
                    px + slab * ray_x - map_x as f32
                };
                // The door has slid `position` toward along=0; the solid part
                // covers [position, 1] and carries its texture with it.
                let u = along - door.position;
                if (0.0..1.0).contains(&along) && u > 0.0 {
                    return Hit {
                        perp: slab.max(1e-4),
                        texture: vswap.door_texture()
                            + door.tex_base
                            + door.vertical as usize,
                        tex_u: (u * TEX_SIZE as f32) as usize & (TEX_SIZE - 1),
                    };
                }
            }
            prev_door = true;
            continue;
        }

        let t = tile(level, map_x, map_y);
        if is_wall(t) {
            let perp = enter.max(1e-4);
            // Fractional hit position along the wall = texture u.
            let wall_x = if side_ns { px + perp * ray_x } else { py + perp * ray_y };
            let wall_frac = wall_x - wall_x.floor();
            let mut tex_u = (wall_frac * TEX_SIZE as f32) as usize & (TEX_SIZE - 1);
            // Mirror so textures read left-to-right on the faces we approach.
            if (!side_ns && ray_x > 0.0) || (side_ns && ray_y < 0.0) {
                tex_u = TEX_SIZE - 1 - tex_u;
            }
            // A wall face reached from inside a door tile is the doorway's
            // side; it shows the door track (jamb) texture.
            let texture = if prev_door {
                vswap.door_texture() + 2 + side_ns as usize
            } else {
                (t as usize - 1) * 2 + !side_ns as usize
            };
            return Hit { perp, texture, tex_u };
        }
        prev_door = false;
    }
}

/// Render the 3D view into the top `view_h` rows of the framebuffer, leaving
/// the rows below untouched for the HUD. Wall/sprite projection scales to
/// `view_h`, so shrinking the view letterboxes rather than crops.
pub fn render(fb: &mut Framebuffer, vswap: &VSwap, world: &World, p: &Player, view_h: usize) {
    let view_hf = view_h as f32;

    // Ceiling / floor halves of the 3D view.
    fb.pixels[..WIDTH * (view_h / 2)].fill(CEILING);
    fb.pixels[WIDTH * (view_h / 2)..WIDTH * view_h].fill(FLOOR);

    // Perpendicular wall distance per column, for sprite occlusion.
    let mut zbuf = [f32::MAX; WIDTH];

    let (dir_x, dir_y) = (p.angle.cos(), p.angle.sin());
    let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);

    for col in 0..WIDTH {
        // camera_x sweeps -1 (left edge) .. +1 (right edge).
        let camera_x = 2.0 * col as f32 / WIDTH as f32 - 1.0;
        let ray_x = dir_x + plane_x * camera_x;
        let ray_y = dir_y + plane_y * camera_x;

        let hit = cast(world, vswap, p.x, p.y, ray_x, ray_y);
        zbuf[col] = hit.perp;

        let line_h = view_hf / hit.perp;
        let top = (view_hf - line_h) / 2.0;
        let y0 = top.max(0.0) as usize;
        let y1 = ((view_hf + line_h) / 2.0).min(view_hf) as usize;

        let texture = &vswap.walls[hit.texture];
        let column = &texture[hit.tex_u * TEX_SIZE..(hit.tex_u + 1) * TEX_SIZE];

        // v steps from the UNCLAMPED strip top so oversize strips stay put.
        let v_step = TEX_SIZE as f32 / line_h;
        let mut v = (y0 as f32 - top) * v_step;
        for y in y0..y1 {
            fb.pixels[y * WIDTH + col] = column[(v as usize).min(TEX_SIZE - 1)];
            v += v_step;
        }
    }

    // --- Sprite pass: back to front, columns depth-tested against zbuf ---
    let mut order: Vec<(f32, &StaticSprite)> = world
        .statics
        .iter()
        .map(|s| ((s.x - p.x).powi(2) + (s.y - p.y).powi(2), s))
        .collect();
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    // Inverse of the [plane dir] camera matrix, for world -> camera space.
    let inv_det = 1.0 / (plane_x * dir_y - dir_x * plane_y);

    for (_, s) in order {
        let (rel_x, rel_y) = (s.x - p.x, s.y - p.y);
        let cam_x = inv_det * (dir_y * rel_x - dir_x * rel_y);
        let depth = inv_det * (-plane_y * rel_x + plane_x * rel_y);
        if depth <= 0.05 {
            continue; // behind or on the camera plane
        }

        let screen_x = WIDTH as f32 / 2.0 * (1.0 + cam_x / depth);
        let size = view_hf / depth; // sprites fill floor-to-ceiling like walls
        let left = screen_x - size / 2.0;
        let top = (view_hf - size) / 2.0;

        let x0 = left.max(0.0) as usize;
        let x1 = (left + size).min(WIDTH as f32).max(0.0) as usize;
        let y0 = top.max(0.0) as usize;
        let y1 = (top + size).min(view_hf).max(0.0) as usize;

        let texture = &vswap.sprites[s.sprite];
        let uv_step = TEX_SIZE as f32 / size;
        for x in x0..x1 {
            if depth >= zbuf[x] {
                continue;
            }
            let u = ((x as f32 - left) * uv_step) as usize & (TEX_SIZE - 1);
            let column = &texture[u * TEX_SIZE..(u + 1) * TEX_SIZE];
            let mut v = (y0 as f32 - top) * uv_step;
            for y in y0..y1 {
                let c = column[(v as usize).min(TEX_SIZE - 1)];
                if c != 0 {
                    fb.pixels[y * WIDTH + x] = c;
                }
                v += uv_step;
            }
        }
    }
}

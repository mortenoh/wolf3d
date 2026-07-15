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
use crate::hud::{KEY_GOLD, KEY_SILVER};

// =============================================================================
// TILE SEMANTICS (plane 0)
// =============================================================================

const MAX_WALL: u16 = 89;
const DOOR_FIRST: u16 = 90;
const DOOR_LAST: u16 = 101;

/// WL_DEF.H ELEVATORTILE: the plane-0 wall tile that is the level-exit switch.
/// Cmd_Use on it flips the switch (tile 21 -> 22) and completes the floor.
const ELEVATOR_TILE: u16 = 21;
const ELEVATOR_TILE_FLIPPED: u16 = 22;

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

#[derive(PartialEq, Clone, Copy)]
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
    /// Required key bitmask to open (0 = unlocked). Matches `gamestate.keys`:
    /// 1 = gold, 2 = silver (WL_ACT1.C `dr_lock1`/`dr_lock2`).
    lock: u8,
    /// Open fraction: 0 = closed, 1 = fully slid into the wall.
    pub position: f32,
    state: DoorState,
}

impl Door {
    /// Advance one door. Returns a sound-enum id when the door begins a motion
    /// that the original announced (auto-close after the hold, or reopening onto
    /// the player) — see WL_ACT1.C `DoorClose`/`DoorOpen`.
    fn tick(&mut self, dt: f32, player_on_tile: bool) -> Option<u8> {
        match self.state {
            DoorState::Opening => {
                self.position += dt / DOOR_OPEN_TIME;
                if self.position >= 1.0 {
                    self.position = 1.0;
                    self.state = DoorState::Open { hold: DOOR_HOLD_TIME };
                }
                None
            }
            DoorState::Open { ref mut hold } => {
                if player_on_tile {
                    *hold = DOOR_HOLD_TIME; // don't close on top of the player
                    None
                } else {
                    *hold -= dt;
                    if *hold <= 0.0 {
                        self.state = DoorState::Closing;
                        return Some(crate::sound::CLOSEDOORSND as u8);
                    }
                    None
                }
            }
            DoorState::Closing => {
                if player_on_tile {
                    self.state = DoorState::Opening; // reopen rather than trap
                    Some(crate::sound::OPENDOORSND as u8)
                } else {
                    self.position -= dt / DOOR_OPEN_TIME;
                    if self.position <= 0.0 {
                        self.position = 0.0;
                        self.state = DoorState::Closed;
                    }
                    None
                }
            }
            DoorState::Closed => None,
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

/// A pickup type (`bo_*` in WL_DEF.H `stat_t`), i.e. the `type` field of a
/// `statinfo` entry that isn't `dressing`/`block`. GetBonus (WL_AGENT.C) maps
/// each to an effect on the player's stats.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bonus {
    Alpo,       // +4 health
    Food,       // +10 health
    FirstAid,   // +25 health
    Gibs,       // +1 health
    Clip,       // +8 ammo
    Clip2,      // +4 ammo (enemy drop)
    MachineGun, // weapon + 6 ammo
    ChainGun,   // weapon + 6 ammo
    Key1,       // gold key
    Key2,       // silver key
    Cross,      // +100
    Chalice,    // +500
    Bible,      // +1000
    Crown,      // +5000
    FullHeal,   // 1-up: full health, +25 ammo, +1 life
}

/// Map a plane-1 static spawn code to its bonus type, if any. The code offsets
/// index the WL_ACT1.C `statinfo[]` table (index = code - STATIC_FIRST); only
/// the entries whose `type` is a `bo_*` value are pickups.
fn bonus_for(code: u16) -> Option<Bonus> {
    let idx = code.checked_sub(STATIC_FIRST)?;
    Some(match idx {
        6 => Bonus::Alpo,
        20 => Bonus::Key1,
        21 => Bonus::Key2,
        24 => Bonus::Food,
        25 => Bonus::FirstAid,
        26 => Bonus::Clip,
        27 => Bonus::MachineGun,
        28 => Bonus::ChainGun,
        29 => Bonus::Cross,
        30 => Bonus::Chalice,
        31 => Bonus::Bible,
        32 => Bonus::Crown,
        33 => Bonus::FullHeal,
        34 | 38 => Bonus::Gibs,
        _ => return None,
    })
}

/// The `statinfo[]` sprite offset (from SPR_STAT_0) for a bonus, used to draw
/// enemy drops with the correct sprite (the inverse of `bonus_for`).
fn bonus_sprite_offset(b: Bonus) -> usize {
    match b {
        Bonus::Alpo => 6,
        Bonus::Key1 => 20,
        Bonus::Key2 => 21,
        Bonus::Food => 24,
        Bonus::FirstAid => 25,
        Bonus::Clip | Bonus::Clip2 => 26,
        Bonus::MachineGun => 27,
        Bonus::ChainGun => 28,
        Bonus::Cross => 29,
        Bonus::Chalice => 30,
        Bonus::Bible => 31,
        Bonus::Crown => 32,
        Bonus::FullHeal => 33,
        Bonus::Gibs => 34,
    }
}

pub struct StaticSprite {
    pub x: f32,
    pub y: f32,
    /// Index into `VSwap::sprites`.
    pub sprite: usize,
    /// The pickup effect, if this static is a bonus item (`FL_BONUS`).
    pub bonus: Option<Bonus>,
    /// Once collected: stops rendering and no longer blocks.
    pub picked: bool,
}

pub struct World {
    pub level: Level,
    pub statics: Vec<StaticSprite>,
    pub doors: Vec<Door>,
    /// Per-tile door index + 1 (0 = no door).
    door_grid: Vec<u8>,
    /// Tiles blocked by a solid static object.
    blocked: Vec<bool>,
    /// Tiles occupied by a live actor, republished each tic by the actor
    /// system; the player collides with these.
    pub actor_blocked: Vec<bool>,
    /// Sound-enum ids emitted by door activity this tic; drained by the game.
    pub sounds: Vec<u8>,
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
                    bonus: bonus_for(obj),
                    picked: false,
                });
                blocked[i] = BLOCKING_STATICS.contains(&obj);
            }
        }
        for (i, &t) in level.plane0.iter().enumerate() {
            if is_door(t) {
                door_grid[i] = (doors.len() + 1) as u8;
                // Lock/texture from the door tile (WL_ACT1.C SpawnDoor): 92/93
                // gold, 94/95 silver, 96/97 & 98/99 the unused lock3/lock4.
                let (tex_base, lock) = match t {
                    90 | 91 => (0, 0),
                    92 | 93 => (6, KEY_GOLD),
                    94 | 95 => (6, KEY_SILVER),
                    96 | 97 => (6, 4),
                    98 | 99 => (6, 8),
                    _ => (4, 0), // 100/101 elevator
                };
                doors.push(Door {
                    x: (i % MAP_SIZE) as i32,
                    y: (i / MAP_SIZE) as i32,
                    vertical: t % 2 == 0,
                    tex_base,
                    lock,
                    position: 0.0,
                    state: DoorState::Closed,
                });
            }
        }
        let actor_blocked = vec![false; MAP_SIZE * MAP_SIZE];
        Self { level, statics, doors, door_grid, blocked, actor_blocked, sounds: Vec::new() }
    }

    /// Drain the door sounds emitted since the last call.
    pub fn take_sounds(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sounds)
    }

    // --- Queries used by the actor system (see src/actors.rs) ---

    /// Is (x,y) a solid wall (or out of bounds)?
    pub fn wall_at(&self, x: i32, y: i32) -> bool {
        is_wall(tile(&self.level, x, y))
    }

    /// Door index at (x,y), if any.
    pub fn door_lookup(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
            return None;
        }
        let idx = self.door_grid[y as usize * MAP_SIZE + x as usize];
        (idx != 0).then(|| idx as usize - 1)
    }

    /// A door's open fraction (0 closed .. 1 open).
    pub fn door_position(&self, idx: usize) -> f32 {
        self.doors[idx].position
    }

    /// Is a door open enough to walk through?
    pub fn door_open_enough(&self, idx: usize) -> bool {
        self.doors[idx].position >= DOOR_PASSABLE
    }

    /// Start opening a door (idempotent) — enemies call this to pass through.
    pub fn request_open_door(&mut self, idx: usize) {
        let d = &mut self.doors[idx];
        if d.state == DoorState::Closed {
            d.state = DoorState::Opening;
            self.sounds.push(crate::sound::OPENDOORSND as u8);
        } else if d.state == DoorState::Closing {
            d.state = DoorState::Opening;
        }
    }

    /// Raw plane-1 code at tile index (for patrol turn markers).
    pub fn plane1_at(&self, tile_idx: usize) -> u16 {
        self.level.plane1[tile_idx]
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
            if let Some(snd) = d.tick(dt, d.x == px && d.y == py) {
                self.sounds.push(snd);
            }
        }
    }

    /// The use key: open/close the door in the tile the player faces. A locked
    /// door (WL_ACT1.C OperateDoor) does nothing but play "no way" unless `keys`
    /// holds its key.
    pub fn use_door(&mut self, p: &Player, keys: u8) {
        let tx = (p.x + p.angle.cos()).floor() as i32;
        let ty = (p.y + p.angle.sin()).floor() as i32;
        let on_it = tx == p.x.floor() as i32 && ty == p.y.floor() as i32;
        if let Some(idx) = self
            .door_at(tx, ty)
            .map(|d| self.door_grid[d.y as usize * MAP_SIZE + d.x as usize])
        {
            let d = &mut self.doors[idx as usize - 1];
            if d.lock != 0 && keys & d.lock == 0 {
                self.sounds.push(crate::sound::NOWAYSND as u8); // locked, lack the key
                return;
            }
            let new = match d.state {
                DoorState::Closed | DoorState::Closing => DoorState::Opening,
                _ if on_it => DoorState::Opening,
                _ => DoorState::Closing,
            };
            let snd = match (d.state, new) {
                (DoorState::Closed | DoorState::Closing, DoorState::Opening) => Some(crate::sound::OPENDOORSND as u8),
                (_, DoorState::Closing) => Some(crate::sound::CLOSEDOORSND as u8),
                _ => None,
            };
            d.state = new;
            if let Some(s) = snd {
                self.sounds.push(s);
            }
        }
    }

    /// Cmd_Use against the tile the player faces: if it is the elevator switch
    /// (ELEVATORTILE), flip it (21 -> 22) and report the floor complete. The
    /// caller advances the level (WL_AGENT.C Cmd_Use / `ex_completed`).
    pub fn use_elevator(&mut self, p: &Player) -> bool {
        let tx = (p.x + p.angle.cos()).floor() as i32;
        let ty = (p.y + p.angle.sin()).floor() as i32;
        if tx < 0 || ty < 0 || tx >= MAP_SIZE as i32 || ty >= MAP_SIZE as i32 {
            return false;
        }
        let idx = ty as usize * MAP_SIZE + tx as usize;
        if self.level.plane0[idx] == ELEVATOR_TILE {
            self.level.plane0[idx] = ELEVATOR_TILE_FLIPPED;
            return true;
        }
        false
    }

    /// Collect a bonus static: stop it rendering and clear any block it held.
    pub fn take_static(&mut self, i: usize) {
        let s = &mut self.statics[i];
        s.picked = true;
        let idx = s.y.floor() as usize * MAP_SIZE + s.x.floor() as usize;
        self.blocked[idx] = false;
    }

    /// Spawn an enemy-death drop (WL_ACT1.C PlaceItemType) as a bonus static.
    pub fn place_drop(&mut self, tilex: i32, tiley: i32, bonus: Bonus) {
        self.statics.push(StaticSprite {
            x: tilex as f32 + 0.5,
            y: tiley as f32 + 0.5,
            sprite: SPR_STAT_0 + bonus_sprite_offset(bonus),
            bonus: Some(bonus),
            picked: false,
        });
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
            if cx >= 0
                && cy >= 0
                && (cx as usize) < MAP_SIZE
                && (cy as usize) < MAP_SIZE
                && world.actor_blocked[cy as usize * MAP_SIZE + cx as usize]
            {
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
pub fn render(
    fb: &mut Framebuffer,
    vswap: &VSwap,
    world: &World,
    actors: &mut crate::actors::Actors,
    p: &Player,
    view_h: usize,
) {
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
    // Statics and actors share the pass; each contributes (dist2, x, y, sprite).
    actors.clear_visible();
    let mut order: Vec<(f32, f32, f32, usize)> = world
        .statics
        .iter()
        .filter(|s| !s.picked)
        .map(|s| ((s.x - p.x).powi(2) + (s.y - p.y).powi(2), s.x, s.y, s.sprite))
        .collect();
    for i in 0..actors.list.len() {
        let (ax, ay) = (actors.list[i].x, actors.list[i].y);
        let sprite = actors.sprite_of(i, p.x, p.y);
        order.push(((ax - p.x).powi(2) + (ay - p.y).powi(2), ax, ay, sprite));
    }
    for pr in &actors.projectiles {
        let sprite = pr.sprite(p.x, p.y);
        order.push(((pr.x - p.x).powi(2) + (pr.y - p.y).powi(2), pr.x, pr.y, sprite));
    }
    order.sort_by(|a, b| b.0.total_cmp(&a.0));

    // Inverse of the [plane dir] camera matrix, for world -> camera space.
    let inv_det = 1.0 / (plane_x * dir_y - dir_x * plane_y);

    for (_, sx_w, sy_w, sprite) in order {
        let (rel_x, rel_y) = (sx_w - p.x, sy_w - p.y);
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

        let texture = &vswap.sprites[sprite];
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

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
use crate::savegame::{Reader, SaveError, Writer};

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
/// WL_DEF.H ALTELEVATORTILE: a floor marker the player stands on; using the
/// elevator switch from here routes to the secret floor (ex_secretlevel).
const ALT_ELEVATOR_TILE: u16 = 107;
/// WL_DEF.H PUSHABLETILE: the plane-1 code marking a wall as a secret push-wall.
const PUSHABLE_TILE: u16 = 98;

/// The result of Cmd_Use against the tile the player faces (WL_AGENT.C Cmd_Use).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElevatorUse {
    /// Not an elevator switch — try a door instead.
    None,
    /// The normal level-exit switch: complete the floor.
    Normal,
    /// The switch used while standing on ALTELEVATORTILE: the secret exit.
    Secret,
}

/// The result of Cmd_Use against a possible secret push-wall (WL_ACT1.C PushWall).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PushUse {
    /// The faced tile is not a push-wall — fall through to the elevator/door.
    NotPushwall,
    /// A push-wall began sliding (bump the secret counter).
    Activated,
    /// The faced tile is a push-wall but it is blocked or already moving.
    Blocked,
}

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
/// SOD extends the statinfo table with four more statics (codes 71..=74): a
/// marble pillar, a 25-round ammo box, a truck, and the Spear of Destiny.
const STATIC_LAST_SOD: u16 = 74;
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
    Clip25,     // SOD bonus box: +25 ammo (bo_25clip)
    Spear,      // SOD: the Spear of Destiny — warps to the final Angel floor
}

impl Bonus {
    /// A stable u8 tag for serialization (`u8::MAX` encodes "no bonus").
    const NONE: u8 = 0xff;
    fn to_tag(this: Option<Bonus>) -> u8 {
        match this {
            None => Bonus::NONE,
            Some(b) => match b {
                Bonus::Alpo => 0,
                Bonus::Food => 1,
                Bonus::FirstAid => 2,
                Bonus::Gibs => 3,
                Bonus::Clip => 4,
                Bonus::Clip2 => 5,
                Bonus::MachineGun => 6,
                Bonus::ChainGun => 7,
                Bonus::Key1 => 8,
                Bonus::Key2 => 9,
                Bonus::Cross => 10,
                Bonus::Chalice => 11,
                Bonus::Bible => 12,
                Bonus::Crown => 13,
                Bonus::FullHeal => 14,
                Bonus::Clip25 => 15,
                Bonus::Spear => 16,
            },
        }
    }
    fn from_tag(tag: u8) -> Result<Option<Bonus>, SaveError> {
        Ok(Some(match tag {
            Bonus::NONE => return Ok(None),
            0 => Bonus::Alpo,
            1 => Bonus::Food,
            2 => Bonus::FirstAid,
            3 => Bonus::Gibs,
            4 => Bonus::Clip,
            5 => Bonus::Clip2,
            6 => Bonus::MachineGun,
            7 => Bonus::ChainGun,
            8 => Bonus::Key1,
            9 => Bonus::Key2,
            10 => Bonus::Cross,
            11 => Bonus::Chalice,
            12 => Bonus::Bible,
            13 => Bonus::Crown,
            14 => Bonus::FullHeal,
            15 => Bonus::Clip25,
            16 => Bonus::Spear,
            _ => return Err(SaveError::BadEnum("bonus")),
        }))
    }
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
        // SOD-only statinfo pickups (WL_ACT1.C statinfo under SPEAR).
        49 => Bonus::Clip25,
        51 => Bonus::Spear,
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
        Bonus::Clip25 => 49,
        Bonus::Spear => 51,
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

// =============================================================================
// PUSH-WALLS (secret doors) — WL_ACT1.C PushWall / MovePWalls
// =============================================================================

/// pwallstate crosses a multiple of this per tile moved (WL_ACT1.C: `oldblock =
/// pwallstate/128`). A push-wall moves two tiles, so it finishes once
/// pwallstate > 256 (2 * 128).
const PWALL_BLOCK: f32 = 128.0;
const PWALL_END: f32 = 256.0;

/// A secret wall sliding two tiles in `dir`, rendered mid-slide and blocking
/// until it stops (WL_ACT1.C PushWall/MovePWalls). Only one can move at a time,
/// mirroring the original's single `pwallstate`.
pub struct PushWall {
    /// The tile the sliding face currently occupies (pwallx/pwally).
    pub x: i32,
    pub y: i32,
    /// Push direction as a unit cardinal (dx, dy).
    pub dx: i32,
    pub dy: i32,
    /// The wall tile value that slides (WL_ACT1.C `oldtile`).
    tex: u16,
    /// pwallstate, in tics (starts at 1 on activation; grows to > 256).
    state: f32,
}

impl PushWall {
    /// pwallpos normalized to a tile fraction: WL_ACT1.C `pwallpos =
    /// (pwallstate/2)&63`, mapped to 0.0..1.0 of a tile along the push
    /// direction. This is the visible slide offset.
    pub fn offset(&self) -> f32 {
        let pos = ((self.state / 2.0) as i32 & 63) as f32;
        pos / 64.0
    }
}

/// Snap a facing angle to the cardinal (dx, dy) the player is looking along, the
/// octant test from WL_AGENT.C Cmd_Use (0 = +x east, +y is south).
fn facing_cardinal(angle: f32) -> (i32, i32) {
    use std::f32::consts::PI;
    let a = angle.rem_euclid(2.0 * PI);
    match ((a / (PI / 2.0)).round() as i32).rem_euclid(4) {
        0 => (1, 0),  // east
        1 => (0, 1),  // south
        2 => (-1, 0), // west
        _ => (0, -1), // north
    }
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
    /// The one active secret push-wall, if any (WL_ACT1.C pwallstate).
    pub pushwall: Option<PushWall>,
    /// The pristine plane-0 map, so a save can serialize only the tiles that
    /// gameplay changed (push-walls opening, the elevator switch flipping).
    plane0_orig: Vec<u16>,
    /// Sound-enum ids emitted by door activity this tic; drained by the game.
    pub sounds: Vec<u8>,
}

impl World {
    /// Build the WL6 world (the default variant).
    pub fn new(level: Level) -> Self {
        Self::new_variant(level, false)
    }

    /// Build a world for a given variant. `sod` extends the static-object code
    /// range to include the Spear-of-Destiny statics (codes 71..=74).
    pub fn new_variant(level: Level, sod: bool) -> Self {
        let static_last = if sod { STATIC_LAST_SOD } else { STATIC_LAST };
        let mut statics = Vec::new();
        let mut blocked = vec![false; MAP_SIZE * MAP_SIZE];
        let mut doors = Vec::new();
        let mut door_grid = vec![0u8; MAP_SIZE * MAP_SIZE];

        for (i, &obj) in level.plane1.iter().enumerate() {
            if (STATIC_FIRST..=static_last).contains(&obj) {
                statics.push(StaticSprite {
                    x: (i % MAP_SIZE) as f32 + 0.5,
                    y: (i / MAP_SIZE) as f32 + 0.5,
                    sprite: SPR_STAT_0 + (obj - STATIC_FIRST) as usize,
                    bonus: bonus_for(obj),
                    picked: false,
                });
                // SOD marble pillar (71) and truck (73) also block movement.
                blocked[i] = BLOCKING_STATICS.contains(&obj) || (sod && matches!(obj, 71 | 73));
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
        let plane0_orig = level.plane0.clone();
        Self {
            level,
            statics,
            doors,
            door_grid,
            blocked,
            actor_blocked,
            pushwall: None,
            plane0_orig,
            sounds: Vec::new(),
        }
    }

    /// Drain the door sounds emitted since the last call.
    pub fn take_sounds(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sounds)
    }

    // --- Save/load (see src/savegame.rs) -----------------------------------

    /// Serialize the mutable world state: door motion, the (possibly grown)
    /// static list, and the blocking grid. The immutable map geometry — walls,
    /// door positions/locks, the door lookup grid — is rebuilt from the level on
    /// load, so it is not written here.
    pub fn save(&self, w: &mut Writer) {
        w.put_u32(self.doors.len() as u32);
        for d in &self.doors {
            w.put_f32(d.position);
            let (tag, hold) = match d.state {
                DoorState::Closed => (0u8, 0.0),
                DoorState::Opening => (1, 0.0),
                DoorState::Open { hold } => (2, hold),
                DoorState::Closing => (3, 0.0),
            };
            w.put_u8(tag);
            w.put_f32(hold);
        }
        w.put_u32(self.statics.len() as u32);
        for s in &self.statics {
            w.put_f32(s.x);
            w.put_f32(s.y);
            w.put_u32(s.sprite as u32);
            w.put_u8(Bonus::to_tag(s.bonus));
            w.put_bool(s.picked);
        }
        // The full blocking grid captures picked pickups and drops without
        // reconstructing each static's blocking type.
        for &b in &self.blocked {
            w.put_bool(b);
        }

        // A moving secret push-wall (WL_ACT1.C pwallstate), so a save mid-slide
        // resumes the animation exactly.
        match &self.pushwall {
            None => w.put_u8(0),
            Some(pw) => {
                w.put_u8(1);
                w.put_i32(pw.x);
                w.put_i32(pw.y);
                w.put_i32(pw.dx);
                w.put_i32(pw.dy);
                w.put_u16(pw.tex);
                w.put_f32(pw.state);
            }
        }

        // Plane-0 tiles that gameplay changed vs. the pristine map (push-walls
        // that have moved, the flipped elevator switch): serialize just the diff.
        let diffs: Vec<(u32, u16)> = self
            .level
            .plane0
            .iter()
            .zip(&self.plane0_orig)
            .enumerate()
            .filter_map(|(i, (&now, &orig))| (now != orig).then_some((i as u32, now)))
            .collect();
        w.put_u32(diffs.len() as u32);
        for (i, v) in diffs {
            w.put_u32(i);
            w.put_u16(v);
        }
    }

    /// Restore door motion, statics and the blocking grid onto a world freshly
    /// rebuilt from the same level (so `self.doors` already has the right count,
    /// positions, locks and textures — only the animation state is overwritten).
    pub fn load(&mut self, r: &mut Reader) -> Result<(), SaveError> {
        let ndoors = r.get_u32()? as usize;
        for i in 0..ndoors {
            let position = r.get_f32()?;
            let tag = r.get_u8()?;
            let hold = r.get_f32()?;
            let state = match tag {
                0 => DoorState::Closed,
                1 => DoorState::Opening,
                2 => DoorState::Open { hold },
                3 => DoorState::Closing,
                _ => return Err(SaveError::BadEnum("door state")),
            };
            if let Some(d) = self.doors.get_mut(i) {
                d.position = position;
                d.state = state;
            }
        }
        let nstatics = r.get_u32()? as usize;
        self.statics.clear();
        self.statics.reserve(nstatics);
        for _ in 0..nstatics {
            let x = r.get_f32()?;
            let y = r.get_f32()?;
            let sprite = r.get_u32()? as usize;
            let bonus = Bonus::from_tag(r.get_u8()?)?;
            let picked = r.get_bool()?;
            self.statics.push(StaticSprite { x, y, sprite, bonus, picked });
        }
        for b in self.blocked.iter_mut() {
            *b = r.get_bool()?;
        }

        // A moving push-wall, then the plane-0 diffs onto the freshly rebuilt
        // (pristine) map. The world was recreated from the level before load, so
        // `plane0` currently equals `plane0_orig`; replay the changes on top.
        self.pushwall = if r.get_u8()? != 0 {
            Some(PushWall {
                x: r.get_i32()?,
                y: r.get_i32()?,
                dx: r.get_i32()?,
                dy: r.get_i32()?,
                tex: r.get_u16()?,
                state: r.get_f32()?,
            })
        } else {
            None
        };
        let ndiffs = r.get_u32()? as usize;
        for _ in 0..ndiffs {
            let i = r.get_u32()? as usize;
            let v = r.get_u16()?;
            if i >= self.level.plane0.len() {
                return Err(SaveError::BadEnum("plane0 diff index"));
            }
            self.level.plane0[i] = v;
        }
        Ok(())
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

    /// Advance door and push-wall animations.
    pub fn tick(&mut self, dt: f32, p: &Player) {
        let (px, py) = (p.x.floor() as i32, p.y.floor() as i32);
        for d in &mut self.doors {
            if let Some(snd) = d.tick(dt, d.x == px && d.y == py) {
                self.sounds.push(snd);
            }
        }
        self.tick_pushwall(dt / crate::game::TIC);
    }

    /// MovePWalls (WL_ACT1.C): advance the active push-wall by `tics`. When
    /// pwallstate crosses a 128-tic block boundary the wall hops one tile in its
    /// push direction, leaving passable floor behind; after two tiles it stops.
    fn tick_pushwall(&mut self, tics: f32) {
        let Some(mut pw) = self.pushwall.take() else {
            return;
        };
        let old_block = (pw.state / PWALL_BLOCK) as i32;
        pw.state += tics;
        if (pw.state / PWALL_BLOCK) as i32 != old_block {
            // The tile the wall vacated becomes walkable floor.
            self.level.plane0[pw.y as usize * MAP_SIZE + pw.x as usize] = 0;
            // WL_ACT1.C tests `pwallstate > 256`; with our fixed one-tic step
            // pwallstate lands exactly on 256, so we terminate at `>=` to keep
            // the intended two-tile distance rather than pushing a third tile.
            if pw.state >= PWALL_END {
                return; // pushed two tiles: done (pw dropped -> inactive)
            }
            // Push one more tile, unless the tile ahead is blocked.
            let (nx, ny) = (pw.x + pw.dx, pw.y + pw.dy);
            let (ax, ay) = (nx + pw.dx, ny + pw.dy);
            if self.pushwall_dest_blocked(ax, ay) {
                return; // stop early (pw dropped -> inactive)
            }
            pw.x = nx;
            pw.y = ny;
            // The sliding face lives at (nx,ny); the tile ahead is solid so the
            // ray always meets a wall while the slide is in progress.
            self.level.plane0[ny as usize * MAP_SIZE + nx as usize] = pw.tex;
            self.level.plane0[ay as usize * MAP_SIZE + ax as usize] = pw.tex;
        }
        self.pushwall = Some(pw);
    }

    /// A push-wall cannot move into a wall, an actor, or off the map.
    fn pushwall_dest_blocked(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
            return true;
        }
        let idx = y as usize * MAP_SIZE + x as usize;
        is_wall(self.level.plane0[idx]) || self.actor_blocked[idx]
    }

    /// Cmd_Use against a secret push-wall (WL_ACT1.C PushWall): if the faced tile
    /// carries the plane-1 PUSHABLETILE marker on a wall, start it sliding two
    /// tiles in the player's facing direction. Only one push-wall moves at a time.
    pub fn use_pushwall(&mut self, p: &Player) -> PushUse {
        let (dx, dy) = facing_cardinal(p.angle);
        let tx = p.x.floor() as i32 + dx;
        let ty = p.y.floor() as i32 + dy;
        if tx < 0 || ty < 0 || tx >= MAP_SIZE as i32 || ty >= MAP_SIZE as i32 {
            return PushUse::NotPushwall;
        }
        let idx = ty as usize * MAP_SIZE + tx as usize;
        if self.level.plane1[idx] != PUSHABLE_TILE {
            return PushUse::NotPushwall;
        }
        // A push-wall marker; from here on we never fall through to a door.
        let tex = self.level.plane0[idx];
        if self.pushwall.is_some() || !is_wall(tex) {
            return PushUse::Blocked;
        }
        let (ax, ay) = (tx + dx, ty + dy);
        if self.pushwall_dest_blocked(ax, ay) {
            self.sounds.push(crate::sound::NOWAYSND as u8);
            return PushUse::Blocked;
        }
        // Activate: drop the marker (so it can't retrigger), make the tile ahead
        // solid, and start the slide with pwallstate = 1.
        self.level.plane1[idx] = 0;
        self.level.plane0[ay as usize * MAP_SIZE + ax as usize] = tex;
        self.pushwall = Some(PushWall { x: tx, y: ty, dx, dy, tex, state: 1.0 });
        self.sounds.push(crate::sound::PUSHWALLSND as u8);
        PushUse::Activated
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
    /// (ELEVATORTILE), flip it (21 -> 22) and report the floor complete. Using it
    /// while the player stands on ALTELEVATORTILE takes the secret exit
    /// (WL_AGENT.C Cmd_Use / `ex_completed` vs `ex_secretlevel`).
    pub fn use_elevator(&mut self, p: &Player) -> ElevatorUse {
        let (dx, dy) = facing_cardinal(p.angle);
        let tx = p.x.floor() as i32 + dx;
        let ty = p.y.floor() as i32 + dy;
        if tx < 0 || ty < 0 || tx >= MAP_SIZE as i32 || ty >= MAP_SIZE as i32 {
            return ElevatorUse::None;
        }
        let idx = ty as usize * MAP_SIZE + tx as usize;
        if self.level.plane0[idx] != ELEVATOR_TILE {
            return ElevatorUse::None;
        }
        self.level.plane0[idx] = ELEVATOR_TILE_FLIPPED;
        // The player's own tile decides normal vs. secret exit.
        let ptile = p.y.floor() as usize * MAP_SIZE + p.x.floor() as usize;
        if self.level.plane0[ptile] == ALT_ELEVATOR_TILE {
            ElevatorUse::Secret
        } else {
            ElevatorUse::Normal
        }
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

        // A secret push-wall's sliding face: render it offset by its slide
        // fraction along the push direction (WL_ACT1.C pwallpos). The tile is
        // solid in plane0, so if the ray misses the offset face we skip the
        // cell's solidity and march on to the (solid) tile ahead of it.
        if let Some(pw) = world.pushwall.as_ref()
            && map_x == pw.x
            && map_y == pw.y
        {
            let exit = side_x.min(side_y);
            if let Some(hit) = pushwall_slab_hit(pw, px, py, ray_x, ray_y, enter, exit) {
                return hit;
            }
            prev_door = false;
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

/// Intersect a ray with a sliding push-wall's player-facing slab. The wall has
/// slid `offset` of a tile in its push direction, so the face the player sees is
/// its origin-side face pushed that far along the push axis. Returns a wall Hit
/// when the ray crosses that face inside the cell.
fn pushwall_slab_hit(
    pw: &PushWall,
    px: f32,
    py: f32,
    ray_x: f32,
    ray_y: f32,
    enter: f32,
    exit: f32,
) -> Option<Hit> {
    let f = pw.offset();
    let (tx, ty) = (pw.x as f32, pw.y as f32);
    // The moving face is perpendicular to the push axis; `side_ns` follows the
    // wall-shading convention (N/S faces take the light chunk).
    let side_ns = pw.dy != 0;
    // Plane coordinate along the push axis and the perpendicular hit coordinate.
    let (slab_t, cross, tile_lo) = if pw.dx != 0 {
        if ray_x == 0.0 {
            return None;
        }
        let plane_x = if pw.dx > 0 { tx + f } else { tx + 1.0 - f };
        let t = (plane_x - px) / ray_x;
        (t, py + t * ray_y, ty)
    } else {
        if ray_y == 0.0 {
            return None;
        }
        let plane_y = if pw.dy > 0 { ty + f } else { ty + 1.0 - f };
        let t = (plane_y - py) / ray_y;
        (t, px + t * ray_x, tx)
    };
    if !(slab_t.is_finite() && slab_t >= enter - 1e-6 && slab_t <= exit + 1e-6) {
        return None;
    }
    if cross < tile_lo || cross > tile_lo + 1.0 {
        return None;
    }
    let along = cross - tile_lo;
    let mut tex_u = (along * TEX_SIZE as f32) as usize & (TEX_SIZE - 1);
    if (!side_ns && ray_x > 0.0) || (side_ns && ray_y < 0.0) {
        tex_u = TEX_SIZE - 1 - tex_u;
    }
    let texture = (pw.tex as usize - 1) * 2 + !side_ns as usize;
    Some(Hit { perp: slab_t.max(1e-4), texture, tex_u })
}

/// The grey palette bytes of the beveled 3D-view border (WL_MAIN.C
/// DrawPlayBorder look): a flat medium-grey field with a dark top/left edge and
/// a light bottom/right edge, framing the view in black. Exposed so the HUD and
/// tests can reference the fill color.
pub const BORDER_FILL: u8 = 0x18; // 125,125,125 medium grey
const BORDER_DARK: u8 = 0x1f; // 32,32,32 shadow (top/left)
const BORDER_LIGHT: u8 = 0x10; // 239,239,239 highlight (bottom/right)

/// Fill the play area (the top [`crate::hud::VIEW_H`] rows) with the grey border
/// and frame a black hole where the shrunken 3D view will be drawn. A no-op
/// visually at full size (the view covers the whole area), so callers can always
/// draw it before [`render`].
pub fn draw_play_border(
    fb: &mut Framebuffer,
    view_x: usize,
    view_y: usize,
    view_w: usize,
    view_h: usize,
    play_h: usize,
) {
    let fill = crate::assets::palette::PALETTE[BORDER_FILL as usize];
    for row in 0..play_h {
        let base = row * WIDTH;
        fb.pixels[base..base + WIDTH].fill(fill);
    }
    // Black backdrop for the view itself, plus a 1px beveled frame around it.
    let (x0, y0) = (view_x, view_y);
    let (x1, y1) = (view_x + view_w, view_y + view_h);
    let black = crate::assets::palette::PALETTE[0];
    for row in y0..y1 {
        let base = row * WIDTH + x0;
        fb.pixels[base..base + view_w].fill(black);
    }
    let dark = crate::assets::palette::PALETTE[BORDER_DARK as usize];
    let light = crate::assets::palette::PALETTE[BORDER_LIGHT as usize];
    // Top / left edge (dark), bottom / right edge (light).
    if y0 > 0 {
        for x in x0.saturating_sub(1)..=x1.min(WIDTH - 1) {
            fb.pixels[(y0 - 1) * WIDTH + x] = dark;
        }
    }
    if x0 > 0 {
        for y in y0.saturating_sub(1)..y1 {
            fb.pixels[y * WIDTH + x0 - 1] = dark;
        }
    }
    if y1 < play_h {
        for x in x0.saturating_sub(1)..=x1.min(WIDTH - 1) {
            fb.pixels[y1 * WIDTH + x] = light;
        }
    }
    if x1 < WIDTH {
        for y in y0..=y1.min(play_h - 1) {
            fb.pixels[y * WIDTH + x1] = light;
        }
    }
}

/// Render the 3D view into the rectangle (`view_x`, `view_y`, `view_w`,
/// `view_h`) of the framebuffer, leaving everything else untouched. Wall/sprite
/// projection scales to the rectangle, so shrinking the view letterboxes rather
/// than crops. At full size (0, 0, WIDTH, view_h) this is pixel-identical to the
/// classic full-screen path.
#[allow(clippy::too_many_arguments)]
pub fn render(
    fb: &mut Framebuffer,
    vswap: &VSwap,
    world: &World,
    actors: &mut crate::actors::Actors,
    p: &Player,
    view_x: usize,
    view_y: usize,
    view_w: usize,
    view_h: usize,
) {
    let view_wf = view_w as f32;
    let view_hf = view_h as f32;
    let view_yf = view_y as f32;

    // Ceiling / floor halves of the 3D view (only within the view rectangle).
    let ceil = CEILING;
    let floor = FLOOR;
    let half = view_h / 2;
    for row in view_y..view_y + half {
        let base = row * WIDTH + view_x;
        fb.pixels[base..base + view_w].fill(ceil);
    }
    for row in view_y + half..view_y + view_h {
        let base = row * WIDTH + view_x;
        fb.pixels[base..base + view_w].fill(floor);
    }

    // Perpendicular wall distance per view column (indexed 0..view_w), for
    // sprite occlusion.
    let mut zbuf = [f32::MAX; WIDTH];

    let (dir_x, dir_y) = (p.angle.cos(), p.angle.sin());
    let (plane_x, plane_y) = (-dir_y * PLANE_LEN, dir_x * PLANE_LEN);

    // `sx` indexes both the view column (offset by view_x) and the depth buffer.
    #[allow(clippy::needless_range_loop)]
    for sx in 0..view_w {
        let col = view_x + sx;
        // camera_x sweeps -1 (left edge) .. +1 (right edge).
        let camera_x = 2.0 * sx as f32 / view_wf - 1.0;
        let ray_x = dir_x + plane_x * camera_x;
        let ray_y = dir_y + plane_y * camera_x;

        let hit = cast(world, vswap, p.x, p.y, ray_x, ray_y);
        zbuf[sx] = hit.perp;

        let line_h = view_hf / hit.perp;
        let top = view_yf + (view_hf - line_h) / 2.0;
        let y0 = top.max(view_yf) as usize;
        let y1 = (view_yf + (view_hf + line_h) / 2.0).min(view_yf + view_hf) as usize;

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

    let view_xf = view_x as f32;
    for (_, sx_w, sy_w, sprite) in order {
        let (rel_x, rel_y) = (sx_w - p.x, sy_w - p.y);
        let cam_x = inv_det * (dir_y * rel_x - dir_x * rel_y);
        let depth = inv_det * (-plane_y * rel_x + plane_x * rel_y);
        if depth <= 0.05 {
            continue; // behind or on the camera plane
        }

        let screen_x = view_xf + view_wf / 2.0 * (1.0 + cam_x / depth);
        let size = view_hf / depth; // sprites fill floor-to-ceiling like walls
        let left = screen_x - size / 2.0;
        let top = view_yf + (view_hf - size) / 2.0;

        let x0 = left.max(view_xf) as usize;
        let x1 = (left + size).min(view_xf + view_wf).max(view_xf) as usize;
        let y0 = top.max(view_yf) as usize;
        let y1 = (top + size).min(view_yf + view_hf).max(view_yf) as usize;

        let texture = &vswap.sprites[sprite];
        let uv_step = TEX_SIZE as f32 / size;
        for x in x0..x1 {
            if depth >= zbuf[x - view_x] {
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

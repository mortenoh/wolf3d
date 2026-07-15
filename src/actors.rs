//! Enemies and the combat loop, rewritten from the original's actor system
//! (WOLFSRC/WL_ACT2.C state tables, WL_STATE.C think/movement code,
//! WL_AGENT.C player attacks). The architecture mirrors the original: every
//! actor is a little state machine. A `State` carries a sprite (a base plus an
//! optional 8-rotation set), a duration in tics, an optional think function
//! (run every frame while in the state) and an optional action (run once when
//! the state ends), plus the index of the next state. Timings are the original
//! integer tic counts; the caller feeds elapsed time as fractional tics so the
//! whole simulation stays frame-rate independent and reproduces headlessly.
//!
//! Deliberate simplifications vs. the original (documented for the bosses
//! milestone):
//! - Area connectivity (`areabyplayer`) is approximated by a BFS flood from the
//!   player's tile that passes through doors once they are >30% open, instead of
//!   the map's precomputed area/door adjacency tables.
//! - Line of sight / `CheckLine` samples the segment on a fine grid rather than
//!   the original fixed-point DDA; walls are full tiles so this is equivalent in
//!   practice. A door blocks sight until it is ~40% open.
//! - Pain frames render single-view (the original's "2 rotation" pain art is
//!   collapsed to one frame), matching this milestone's brief.
//! - Actors are not blocked by blocking static objects (tables etc.), matching
//!   an original quirk where such statics are not `FL_SHOOTABLE` and so pass the
//!   `TryWalk` actor test.
//!
//! Boss milestone simplifications (WL_ACT2.C bosses are otherwise faithful:
//! state tables, hard-tier hitpoints, chase/attack thinks, the mecha-Hitler ->
//! real Hitler morph):
//! - Pac-Man ghosts (spawn codes 224..=227, E3M10) are not implemented.
//! - `A_StartDeathCam` (Schabbs/Gift/Fat/Hitler die chains) is replaced by a
//!   death event the game maps to its `victory` flag; there is no deathcam or
//!   episode-end sequence.
//! - Rockets have no smoke trail (`A_Smoke`) or explosion (`s_boom`) frames.
//! - The fake Hitler's fireball moves continuously at its 0x1200 speed. In the
//!   original `T_Projectile` is the *action* of the 6-tic fire states, so at
//!   high frame rates fireballs crawl; at low frame rates they run full speed.
//!   Continuous movement matches the low-frame-rate (intended) behavior.
//! - Boss die2 states use the DigiMode-on 140-tic duration (the death yell).

use crate::assets::maps::{Level, MAP_SIZE};
use crate::raycast::{Bonus, World};
use crate::sound as snd;

// =============================================================================
// FIXED-POINT / TUNING CONSTANTS (from WL_DEF.H, converted to tile units)
// =============================================================================

/// The original's TILEGLOBAL: one tile == 0x10000 fixed-point units.
const TILE_GLOBAL: f32 = 65536.0;
/// SPDPATROL / SPDDOG raw speeds (global units per tic).
const SPD_PATROL: f32 = 512.0;
const SPD_DOG: f32 = 1500.0;
/// MINACTORDIST 0x10000 and MINSIGHT 0x18000, in tiles.
const MIN_ACTOR_DIST: f32 = 1.0;
const MIN_SIGHT: f32 = 1.5;
/// A door must be at least this open to see / flood through it.
const DOOR_SEE: f32 = 0.4;
const DOOR_FLOOD: f32 = 0.3;

// Direction codes match WL_DEF.H `dirtype`.
const EAST: u8 = 0;
const NORTHEAST: u8 = 1;
const NORTH: u8 = 2;
const NORTHWEST: u8 = 3;
const WEST: u8 = 4;
const SOUTHWEST: u8 = 5;
const SOUTH: u8 = 6;
const SOUTHEAST: u8 = 7;
const NODIR: u8 = 8;

/// opposite[9] from WL_STATE.C.
const OPPOSITE: [u8; 9] = [WEST, SOUTHWEST, SOUTH, SOUTHEAST, EAST, NORTHEAST, NORTH, NORTHWEST, NODIR];

/// Movement delta per direction, +x east / +y south (screen down).
fn dir_delta(dir: u8) -> (i32, i32) {
    match dir {
        EAST => (1, 0),
        NORTHEAST => (1, -1),
        NORTH => (0, -1),
        NORTHWEST => (-1, -1),
        WEST => (-1, 0),
        SOUTHWEST => (-1, 1),
        SOUTH => (0, 1),
        SOUTHEAST => (1, 1),
        _ => (0, 0),
    }
}

/// diagonal[x_dir][y_dir] from WL_STATE.C: combine a cardinal E/W with a
/// cardinal N/S into the matching diagonal.
fn diagonal(a: u8, b: u8) -> u8 {
    match (a, b) {
        (EAST, NORTH) | (NORTH, EAST) => NORTHEAST,
        (EAST, SOUTH) | (SOUTH, EAST) => SOUTHEAST,
        (WEST, NORTH) | (NORTH, WEST) => NORTHWEST,
        (WEST, SOUTH) | (SOUTH, WEST) => SOUTHWEST,
        _ => NODIR,
    }
}

// =============================================================================
// US_RndT — the original 256-entry random table (ID_US_A.ASM)
// =============================================================================

const RND_TABLE: [u8; 256] = [
    0, 8, 109, 220, 222, 241, 149, 107, 75, 248, 254, 140, 16, 66, 74, 21, 211, 47, 80, 242, 154,
    27, 205, 128, 161, 89, 77, 36, 95, 110, 85, 48, 212, 140, 211, 249, 22, 79, 200, 50, 28, 188,
    52, 140, 202, 120, 68, 145, 62, 70, 184, 190, 91, 197, 152, 224, 149, 104, 25, 178, 252, 182,
    202, 182, 141, 197, 4, 81, 181, 242, 145, 42, 39, 227, 156, 198, 225, 193, 219, 93, 122, 175,
    249, 0, 175, 143, 70, 239, 46, 246, 163, 53, 163, 109, 168, 135, 2, 235, 25, 92, 20, 145, 138,
    77, 69, 166, 78, 176, 173, 212, 166, 113, 94, 161, 41, 50, 239, 49, 111, 164, 70, 60, 2, 37,
    171, 75, 136, 156, 11, 56, 42, 146, 138, 229, 73, 146, 77, 61, 98, 196, 135, 106, 63, 197, 195,
    86, 96, 203, 113, 101, 170, 247, 181, 113, 80, 250, 108, 7, 255, 237, 129, 226, 79, 107, 112,
    166, 103, 241, 24, 223, 239, 120, 198, 58, 60, 82, 128, 3, 184, 66, 143, 224, 145, 224, 81,
    206, 163, 45, 63, 90, 168, 114, 59, 33, 159, 95, 28, 139, 123, 98, 125, 196, 15, 70, 194, 253,
    54, 14, 109, 226, 71, 17, 161, 93, 186, 87, 244, 138, 20, 52, 123, 251, 26, 36, 17, 46, 52,
    231, 232, 76, 31, 221, 84, 37, 216, 165, 212, 106, 197, 242, 98, 43, 39, 175, 254, 145, 190,
    84, 118, 222, 187, 136, 120, 163, 236, 249,
];

/// Deterministic table-based RNG; `US_InitRndT(false)` seeds the index to 0.
struct Rnd {
    index: usize,
}
impl Rnd {
    fn new() -> Self {
        Self { index: 0 }
    }
    /// US_RndT: advance the index and return the next byte (0..=255).
    fn roll(&mut self) -> u32 {
        self.index = (self.index + 1) & 0xff;
        RND_TABLE[self.index] as u32
    }
}

// =============================================================================
// STATE MACHINE TABLE
// =============================================================================

#[derive(Clone, Copy, PartialEq)]
enum Think {
    None,
    Stand,
    Path,
    Chase,
    DogChase,
    /// T_Schabb / T_Gift / T_Fat: flat attack chance, runs *through* the
    /// player (SelectRunDir) when close.
    SchabbChase,
    /// T_Fake: lower attack chance, always dodges.
    FakeChase,
}

/// Movement/attack model used by `t_chase` (which original T_* think ran).
#[derive(Clone, Copy, PartialEq)]
enum Chase {
    Normal,
    Dog,
    Schabb,
    Fake,
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    None,
    Shoot,
    Bite,
    DeathScream,
    /// T_SchabbThrow: launch a syringe at the player.
    ThrowNeedle,
    /// T_GiftThrow: launch a rocket at the player.
    ThrowRocket,
    /// T_FakeFire: launch a fireball at the player.
    FakeFire,
    /// A_HitlerMorph: the mecha suit explodes and the real Hitler steps out.
    HitlerMorph,
}

/// A single actor state — a direct analogue of the original `statetype`.
struct State {
    /// Base VSWAP sprite; for `rotate` states this is the "_1" (front) frame of
    /// an 8-view group and the view offset is added at draw time.
    sprite: u16,
    rotate: bool,
    /// Duration in tics; 0 means a permanent state (stand / dead).
    tics: u16,
    think: Think,
    action: Action,
    next: usize,
}

/// The state indices a given enemy kind spawns into and transitions through.
#[derive(Clone, Copy)]
struct KindStates {
    stand: usize,
    path1: usize,
    chase1: usize,
    /// Attack entry: shoot1 for humanoids, jump1 for the dog.
    attack1: usize,
    die1: usize,
    pain: usize,
    pain1: usize,
}

/// Regular enemies plus every WL6 boss.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Guard,
    Officer,
    Ss,
    Dog,
    Mutant,
    Hans,        // bossobj, E1M9
    Schabbs,     // schabbobj, E2M9
    FakeHitler,  // fakeobj, E3M9 / E3M10
    MechaHitler, // mechahitlerobj, E3M9 phase 1
    Hitler,      // realhitlerobj, spawned by A_HitlerMorph
    Gretel,      // gretelobj, E5M9
    Gift,        // giftobj, E4M9
    Fat,         // fatobj, E6M9
}

const NUM_KINDS: usize = 13;

impl Kind {
    fn index(self) -> usize {
        match self {
            Kind::Guard => 0,
            Kind::Officer => 1,
            Kind::Ss => 2,
            Kind::Dog => 3,
            Kind::Mutant => 4,
            Kind::Hans => 5,
            Kind::Schabbs => 6,
            Kind::FakeHitler => 7,
            Kind::MechaHitler => 8,
            Kind::Hitler => 9,
            Kind::Gretel => 10,
            Kind::Gift => 11,
            Kind::Fat => 12,
        }
    }
    /// starthitpoints[gd_hard][kind] from WL_ACT2.C ("death incarnate" tier);
    /// the real Hitler uses A_HitlerMorph's own table {500,700,800,900}.
    fn hitpoints(self) -> i32 {
        match self {
            Kind::Guard => 25,
            Kind::Officer => 50,
            Kind::Ss => 100,
            Kind::Dog => 1,
            Kind::Mutant => 65,
            Kind::Hans => 1200,
            Kind::Schabbs => 2400,
            Kind::FakeHitler => 500,
            Kind::MechaHitler => 1200,
            Kind::Hitler => 900,
            Kind::Gretel => 1200,
            Kind::Gift => 1200,
            Kind::Fat => 1200,
        }
    }
    /// GivePoints on kill (WL_STATE.C KillActor). Bosses score 5000 except the
    /// fake Hitler's 2000.
    fn points(self) -> i32 {
        match self {
            Kind::Guard => 100,
            Kind::Officer => 400,
            Kind::Ss => 500,
            Kind::Dog => 200,
            Kind::Mutant => 700,
            Kind::FakeHitler => 2000,
            _ => 5000,
        }
    }
    /// Speed multiplier applied on FirstSighting (WL_STATE.C). Bosses go x3
    /// (Hans's `speed = SPDPATROL*3` equals `*= 3` from his spawn speed); the
    /// real Hitler's x5 is applied by A_HitlerMorph instead.
    fn chase_mult(self) -> f32 {
        match self {
            Kind::Guard | Kind::Mutant => 3.0,
            Kind::Officer => 5.0,
            Kind::Ss => 4.0,
            Kind::Dog => 2.0,
            Kind::Hitler => 5.0,
            _ => 3.0,
        }
    }
    /// Bosses and dogs have no pain frames (WL_STATE.C DamageActor's switch
    /// only covers guard/officer/mutant/ss).
    fn has_pain(self) -> bool {
        matches!(self, Kind::Guard | Kind::Officer | Kind::Ss | Kind::Mutant)
    }
}

/// Built once: the flat state table plus the per-kind entry indices.
struct StateTable {
    states: Vec<State>,
    kinds: [KindStates; NUM_KINDS],
}

fn build_states() -> StateTable {
    let mut v: Vec<State> = Vec::new();

    // Push a humanoid (guard/officer/ss/mutant) state block and return its
    // entry indices. Shoot/die frame counts and tics differ per enemy, so they
    // are passed in.
    let push_humanoid = |v: &mut Vec<State>,
                             stand: u16,
                             w1: u16,
                             pain1: u16,
                             pain2: u16,
                             shoots: &[(u16, u16, bool)],
                             dies: &[(u16, u16)]|
     -> KindStates {
        let (w2, w3, w4) = (w1 + 8, w1 + 16, w1 + 24);
        let b = v.len();
        let s = shoots.len();
        let chase0 = b + 9 + s;
        let die0 = chase0 + 6;

        // stand (b+0)
        v.push(State { sprite: stand, rotate: true, tics: 0, think: Think::Stand, action: Action::None, next: b });
        // path1..path4 (b+1..=b+6)
        v.push(State { sprite: w1, rotate: true, tics: 20, think: Think::Path, action: Action::None, next: b + 2 });
        v.push(State { sprite: w1, rotate: true, tics: 5, think: Think::None, action: Action::None, next: b + 3 });
        v.push(State { sprite: w2, rotate: true, tics: 15, think: Think::Path, action: Action::None, next: b + 4 });
        v.push(State { sprite: w3, rotate: true, tics: 20, think: Think::Path, action: Action::None, next: b + 5 });
        v.push(State { sprite: w3, rotate: true, tics: 5, think: Think::None, action: Action::None, next: b + 6 });
        v.push(State { sprite: w4, rotate: true, tics: 15, think: Think::Path, action: Action::None, next: b + 1 });
        // pain (b+7), pain1 (b+8)
        v.push(State { sprite: pain1, rotate: false, tics: 10, think: Think::None, action: Action::None, next: chase0 });
        v.push(State { sprite: pain2, rotate: false, tics: 10, think: Think::None, action: Action::None, next: chase0 });
        // shoots (b+9 ..)
        for (k, &(spr, tics, fire)) in shoots.iter().enumerate() {
            let next = if k + 1 < s { b + 9 + k + 1 } else { chase0 };
            v.push(State {
                sprite: spr,
                rotate: false,
                tics,
                think: Think::None,
                action: if fire { Action::Shoot } else { Action::None },
                next,
            });
        }
        // chase (chase0 ..)
        v.push(State { sprite: w1, rotate: true, tics: 10, think: Think::Chase, action: Action::None, next: chase0 + 1 });
        v.push(State { sprite: w1, rotate: true, tics: 3, think: Think::None, action: Action::None, next: chase0 + 2 });
        v.push(State { sprite: w2, rotate: true, tics: 8, think: Think::Chase, action: Action::None, next: chase0 + 3 });
        v.push(State { sprite: w3, rotate: true, tics: 10, think: Think::Chase, action: Action::None, next: chase0 + 4 });
        v.push(State { sprite: w3, rotate: true, tics: 3, think: Think::None, action: Action::None, next: chase0 + 5 });
        v.push(State { sprite: w4, rotate: true, tics: 8, think: Think::Chase, action: Action::None, next: chase0 });
        // dies (die0 ..); last has tics 0 and loops on itself
        let d = dies.len();
        for (k, &(spr, tics)) in dies.iter().enumerate() {
            let next = if k + 1 < d { die0 + k + 1 } else { die0 + k };
            v.push(State {
                sprite: spr,
                rotate: false,
                tics,
                think: Think::None,
                action: if k == 0 { Action::DeathScream } else { Action::None },
                next,
            });
        }

        KindStates { stand: b, path1: b + 1, chase1: chase0, attack1: b + 9, die1: die0, pain: b + 7, pain1: b + 8 }
    };

    // Guard, officer, ss, mutant (sprite bases from WL_DEF.H SPR_ enum).
    let guard = push_humanoid(
        &mut v, 50, 58, 90, 94,
        &[(96, 20, false), (97, 20, true), (98, 20, false)],
        &[(91, 15), (92, 15), (93, 15), (95, 0)],
    );
    let officer = push_humanoid(
        &mut v, 238, 246, 278, 282,
        &[(285, 6, false), (286, 20, true), (287, 10, false)],
        &[(279, 11), (280, 11), (281, 11), (283, 11), (284, 0)],
    );
    let ss = push_humanoid(
        &mut v, 138, 146, 178, 182,
        &[
            (184, 20, false), (185, 20, true), (186, 10, false), (185, 10, true), (186, 10, false),
            (185, 10, true), (186, 10, false), (185, 10, true), (186, 10, false),
        ],
        &[(179, 15), (180, 15), (181, 15), (183, 0)],
    );
    let mutant = push_humanoid(
        &mut v, 187, 195, 227, 231,
        &[(234, 6, true), (235, 20, false), (236, 10, true), (237, 20, false)],
        &[(228, 7), (229, 7), (230, 7), (232, 7), (233, 0)],
    );

    // Dog — no shoot; a bite-jump attack and its own die chain (WL_ACT2.C).
    let dog = {
        let (w1, w2, w3, w4) = (99u16, 107u16, 115u16, 123u16);
        let b = v.len();
        // stand (b+0) — synthetic: SpawnStand(en_dog) is a no-op in the
        // original; a dog stand marker just yields a dog that notices and chases.
        v.push(State { sprite: w1, rotate: true, tics: 0, think: Think::Stand, action: Action::None, next: b });
        // path1..path4 (b+1..=b+6)
        v.push(State { sprite: w1, rotate: true, tics: 20, think: Think::Path, action: Action::None, next: b + 2 });
        v.push(State { sprite: w1, rotate: true, tics: 5, think: Think::None, action: Action::None, next: b + 3 });
        v.push(State { sprite: w2, rotate: true, tics: 15, think: Think::Path, action: Action::None, next: b + 4 });
        v.push(State { sprite: w3, rotate: true, tics: 20, think: Think::Path, action: Action::None, next: b + 5 });
        v.push(State { sprite: w3, rotate: true, tics: 5, think: Think::None, action: Action::None, next: b + 6 });
        v.push(State { sprite: w4, rotate: true, tics: 15, think: Think::Path, action: Action::None, next: b + 1 });
        // jump1..jump5 (b+7..=b+11)
        let chase0 = b + 12;
        v.push(State { sprite: 135, rotate: false, tics: 10, think: Think::None, action: Action::None, next: b + 8 });
        v.push(State { sprite: 136, rotate: false, tics: 10, think: Think::None, action: Action::Bite, next: b + 9 });
        v.push(State { sprite: 137, rotate: false, tics: 10, think: Think::None, action: Action::None, next: b + 10 });
        v.push(State { sprite: 135, rotate: false, tics: 10, think: Think::None, action: Action::None, next: b + 11 });
        v.push(State { sprite: w1, rotate: false, tics: 10, think: Think::None, action: Action::None, next: chase0 });
        // chase1..chase4 (chase0..)
        v.push(State { sprite: w1, rotate: true, tics: 10, think: Think::DogChase, action: Action::None, next: chase0 + 1 });
        v.push(State { sprite: w1, rotate: true, tics: 3, think: Think::None, action: Action::None, next: chase0 + 2 });
        v.push(State { sprite: w2, rotate: true, tics: 8, think: Think::DogChase, action: Action::None, next: chase0 + 3 });
        v.push(State { sprite: w3, rotate: true, tics: 10, think: Think::DogChase, action: Action::None, next: chase0 + 4 });
        v.push(State { sprite: w3, rotate: true, tics: 3, think: Think::None, action: Action::None, next: chase0 + 5 });
        v.push(State { sprite: w4, rotate: true, tics: 8, think: Think::DogChase, action: Action::None, next: chase0 });
        // die1..dead (die0..)
        let die0 = chase0 + 6;
        v.push(State { sprite: 131, rotate: false, tics: 15, think: Think::None, action: Action::DeathScream, next: die0 + 1 });
        v.push(State { sprite: 132, rotate: false, tics: 15, think: Think::None, action: Action::None, next: die0 + 2 });
        v.push(State { sprite: 133, rotate: false, tics: 15, think: Think::None, action: Action::None, next: die0 + 3 });
        v.push(State { sprite: 134, rotate: false, tics: 15, think: Think::None, action: Action::None, next: die0 + 3 });
        KindStates { stand: b, path1: b + 1, chase1: chase0, attack1: b + 7, die1: die0, pain: die0, pain1: die0 }
    };

    // Boss block (WL_ACT2.C): a stand state (unless spawned straight into
    // chase, like the morphed Hitler), a 6-state chase loop whose tic pattern
    // and think vary per boss, a shoot chain, and a die chain. No rotations
    // anywhere (bosses have single-view art), no pain states.
    let push_boss = |v: &mut Vec<State>,
                     stand: bool,
                     w1: u16,
                     chase_tics: (u16, u16, u16),
                     chase_think: Think,
                     shoots: &[(u16, u16, Action)],
                     dies: &[(u16, u16, Action)]|
     -> KindStates {
        let (w2, w3, w4) = (w1 + 1, w1 + 2, w1 + 3);
        let b = v.len();
        let chase0 = if stand { b + 1 } else { b };
        if stand {
            v.push(State { sprite: w1, rotate: false, tics: 0, think: Think::Stand, action: Action::None, next: b });
        }
        // chase1, chase1s, chase2, chase3, chase3s, chase4.
        let (t1, ts, t2) = chase_tics;
        v.push(State { sprite: w1, rotate: false, tics: t1, think: chase_think, action: Action::None, next: chase0 + 1 });
        v.push(State { sprite: w1, rotate: false, tics: ts, think: Think::None, action: Action::None, next: chase0 + 2 });
        v.push(State { sprite: w2, rotate: false, tics: t2, think: chase_think, action: Action::None, next: chase0 + 3 });
        v.push(State { sprite: w3, rotate: false, tics: t1, think: chase_think, action: Action::None, next: chase0 + 4 });
        v.push(State { sprite: w3, rotate: false, tics: ts, think: Think::None, action: Action::None, next: chase0 + 5 });
        v.push(State { sprite: w4, rotate: false, tics: t2, think: chase_think, action: Action::None, next: chase0 });
        // shoots; the last always returns to chase1.
        let shoot0 = chase0 + 6;
        let s = shoots.len();
        for (k, &(spr, tics, action)) in shoots.iter().enumerate() {
            let next = if k + 1 < s { shoot0 + k + 1 } else { chase0 };
            v.push(State { sprite: spr, rotate: false, tics, think: Think::None, action, next });
        }
        // dies; the last has tics 0 and loops on itself.
        let die0 = shoot0 + s;
        let d = dies.len();
        for (k, &(spr, tics, action)) in dies.iter().enumerate() {
            let next = if k + 1 < d { die0 + k + 1 } else { die0 + k };
            v.push(State { sprite: spr, rotate: false, tics, think: Think::None, action, next });
        }
        let stand_idx = if stand { b } else { chase0 };
        KindStates { stand: stand_idx, path1: stand_idx, chase1: chase0, attack1: shoot0, die1: die0, pain: chase0, pain1: chase0 }
    };

    use Action as A;
    // Hans Grosse (s_boss*): chaingun bursts of 6 T_Shoot volleys.
    let hans = push_boss(
        &mut v, true, 296, (10, 3, 8), Think::Chase,
        &[(300, 30, A::None), (301, 10, A::Shoot), (302, 10, A::Shoot), (301, 10, A::Shoot),
          (302, 10, A::Shoot), (301, 10, A::Shoot), (302, 10, A::Shoot), (300, 10, A::None)],
        &[(304, 15, A::DeathScream), (305, 15, A::None), (306, 15, A::None), (303, 0, A::None)],
    );
    // Dr. Schabbs (s_schabb*): throws a syringe; runs at the player up close.
    let schabbs = push_boss(
        &mut v, true, 307, (10, 3, 8), Think::SchabbChase,
        &[(311, 30, A::None), (312, 10, A::ThrowNeedle)],
        &[(307, 10, A::DeathScream), (307, 140, A::None), (313, 10, A::None),
          (314, 10, A::None), (315, 10, A::None), (316, 0, A::None)],
    );
    // Fake Hitler (s_fake*): a volley of 8 fireballs; always dodges.
    let fake = push_boss(
        &mut v, true, 321, (10, 3, 8), Think::FakeChase,
        &[(325, 8, A::FakeFire), (325, 8, A::FakeFire), (325, 8, A::FakeFire), (325, 8, A::FakeFire),
          (325, 8, A::FakeFire), (325, 8, A::FakeFire), (325, 8, A::FakeFire), (325, 8, A::FakeFire),
          (325, 8, A::None)],
        &[(328, 10, A::DeathScream), (329, 10, A::None), (330, 10, A::None),
          (331, 10, A::None), (332, 10, A::None), (333, 0, A::None)],
    );
    // Mecha-Hitler (s_mecha*): chaingun bursts; dying morphs into the real
    // Hitler at the end of die3 (A_HitlerMorph).
    let mecha = push_boss(
        &mut v, true, 334, (10, 6, 8), Think::Chase,
        &[(338, 30, A::None), (339, 10, A::Shoot), (340, 10, A::Shoot),
          (339, 10, A::Shoot), (340, 10, A::Shoot), (339, 10, A::Shoot)],
        &[(342, 10, A::DeathScream), (343, 10, A::None), (344, 10, A::HitlerMorph), (341, 0, A::None)],
    );
    // Real Hitler (s_hitler*): fastest chase cycle in the game plus chaingun
    // bursts; the long 10-frame die sequence ends on SPR_HITLER_DEAD.
    let hitler = push_boss(
        &mut v, false, 345, (6, 4, 2), Think::Chase,
        &[(349, 30, A::None), (350, 10, A::Shoot), (351, 10, A::Shoot),
          (350, 10, A::Shoot), (351, 10, A::Shoot), (350, 10, A::Shoot)],
        &[(345, 1, A::DeathScream), (345, 140, A::None), (353, 10, A::None), (354, 10, A::None),
          (355, 10, A::None), (356, 10, A::None), (357, 10, A::None), (358, 10, A::None),
          (359, 10, A::None), (352, 0, A::None)],
    );
    // Gretel Grosse (s_gretel*): Hans's sister, same behavior.
    let gretel = push_boss(
        &mut v, true, 385, (10, 3, 8), Think::Chase,
        &[(389, 30, A::None), (390, 10, A::Shoot), (391, 10, A::Shoot), (390, 10, A::Shoot),
          (391, 10, A::Shoot), (390, 10, A::Shoot), (391, 10, A::Shoot), (389, 10, A::None)],
        &[(393, 15, A::DeathScream), (394, 15, A::None), (395, 15, A::None), (392, 0, A::None)],
    );
    // Otto Giftmacher (s_gift*): throws rockets.
    let gift = push_boss(
        &mut v, true, 360, (10, 3, 8), Think::SchabbChase,
        &[(364, 30, A::None), (365, 10, A::ThrowRocket)],
        &[(360, 1, A::DeathScream), (360, 140, A::None), (366, 10, A::None),
          (367, 10, A::None), (368, 10, A::None), (369, 0, A::None)],
    );
    // General Fettgesicht (s_fat*): a rocket then a chaingun burst.
    let fat = push_boss(
        &mut v, true, 396, (10, 3, 8), Think::SchabbChase,
        &[(400, 30, A::None), (401, 10, A::ThrowRocket), (402, 10, A::Shoot),
          (403, 10, A::Shoot), (402, 10, A::Shoot), (403, 10, A::Shoot)],
        &[(396, 1, A::DeathScream), (396, 140, A::None), (404, 10, A::None),
          (405, 10, A::None), (406, 10, A::None), (407, 0, A::None)],
    );

    StateTable {
        states: v,
        kinds: [guard, officer, ss, dog, mutant, hans, schabbs, fake, mecha, hitler, gretel, gift, fat],
    }
}

// =============================================================================
// ACTOR
// =============================================================================

// Actor flags (subset of WL_DEF.H FL_*).
const FL_SHOOTABLE: u8 = 1;
const FL_ATTACKMODE: u8 = 2;
const FL_FIRSTATTACK: u8 = 4;
const FL_AMBUSH: u8 = 8;

pub struct Actor {
    pub kind: Kind,
    /// World position in tile units (tile centers sit at n + 0.5).
    pub x: f32,
    pub y: f32,
    /// Goal tile while moving (the original's tilex/tiley).
    tilex: i32,
    tiley: i32,
    dir: u8,
    state: usize,
    /// Remaining tics in the current state (fractional).
    ticcount: f32,
    health: i32,
    /// Raw speed in global units per tic (SPDPATROL etc).
    speed: f32,
    /// Tiles left to the goal tile; 1.0 per step.
    distance: f32,
    /// Set while blocked waiting for a door to open.
    waiting_door: Option<usize>,
    flags: u8,
    /// temp2 reaction countdown in tics.
    reaction: f32,
    /// True once killed — stops blocking movement and further AI.
    pub dead: bool,
    /// Set by the renderer: was this actor on screen last frame (FL_VISABLE)?
    visible: bool,
}

impl Actor {
    fn tile(&self) -> usize {
        self.tiley as usize * MAP_SIZE + self.tilex as usize
    }
}

// =============================================================================
// SYSTEM
// =============================================================================

pub struct Actors {
    pub list: Vec<Actor>,
    /// Live boss/enemy projectiles (Schabbs syringes, fake-Hitler fireballs,
    /// Gift/Fat rockets).
    pub projectiles: Vec<Projectile>,
    table: StateTable,
    rnd: Rnd,
    /// Scratch flood-fill grid: tiles reachable from the player (areabyplayer).
    reachable: Vec<bool>,
    /// Scratch actor-occupancy grid (blocking, live actors).
    occ: Vec<bool>,
    /// Damage dealt to the player this update (drained by the caller).
    damage: i32,
    /// Bonus items dropped by killed enemies this update (tile + type), drained
    /// by the caller to spawn statics (WL_ACT1.C PlaceItemType).
    drops: Vec<(i32, i32, Bonus)>,
    /// Kinds killed this update, drained by the caller (used for the victory
    /// condition in place of the original's A_StartDeathCam deathcam).
    deaths: Vec<Kind>,
    /// Sound-enum ids emitted by enemy actions this update, drained by the game.
    sounds: Vec<u8>,
    /// A dedicated RNG for cosmetic sound choices (the random guard death
    /// scream). Kept separate from `rnd` so audio never perturbs the gameplay
    /// random stream that the deterministic tests depend on.
    snd_rnd: Rnd,
}

// =============================================================================
// PROJECTILES (WL_ACT2.C T_Projectile and the throw actions)
// =============================================================================

/// PROJECTILESIZE 0xc000: player contact box, in tiles.
const PROJECTILE_SIZE: f32 = 0.75;
/// PROJSIZE 0x2000: wall collision half-extent, in tiles.
const PROJ_SIZE: f32 = 0.125;

#[derive(Clone, Copy, PartialEq)]
enum ProjKind {
    /// Schabbs's syringe: damage (rnd>>3)+20, speed 0x2000/tic.
    Needle,
    /// Fake Hitler's fireball: damage rnd>>3, speed 0x1200/tic.
    Fire,
    /// Gift/Fat rocket: damage (rnd>>3)+30, speed 0x2000/tic.
    Rocket,
}

pub struct Projectile {
    pub x: f32,
    pub y: f32,
    /// Velocity in tiles per tic.
    vx: f32,
    vy: f32,
    kind: ProjKind,
    /// Animation clock in tics.
    anim: f32,
    pub dead: bool,
}

impl Projectile {
    /// Current VSWAP sprite; needles/fires cycle their frames, rockets pick
    /// one of 8 views from the flight direction relative to the viewer.
    pub fn sprite(&self, viewer_x: f32, viewer_y: f32) -> usize {
        match self.kind {
            ProjKind::Needle => 317 + (self.anim / 6.0) as usize % 4, // SPR_HYPO1..4
            ProjKind::Fire => 326 + (self.anim / 6.0) as usize % 2,   // SPR_FIRE1..2
            ProjKind::Rocket => {
                let facing = self.vy.atan2(self.vx);
                370 + rotation_from_angle(facing, self.x, self.y, viewer_x, viewer_y) // SPR_ROCKET_1..8
            }
        }
    }
}

/// The sound an enemy makes on first sighting the player (WL_STATE.C
/// FirstSighting). The mutant is silent.
fn alert_sound(kind: Kind) -> Option<u8> {
    Some(match kind {
        Kind::Guard => snd::HALTSND as u8,
        Kind::Officer => snd::SPIONSND as u8,
        Kind::Ss => snd::SCHUTZADSND as u8,
        Kind::Dog => snd::DOGBARKSND as u8,
        Kind::Hans => snd::GUTENTAGSND as u8,
        Kind::Gretel => snd::KEINSND as u8,
        Kind::Gift => snd::EINESND as u8,
        Kind::Fat => snd::ERLAUBENSND as u8,
        Kind::Schabbs => snd::SCHABBSHASND as u8,
        Kind::FakeHitler => snd::TOT_HUNDSND as u8,
        Kind::MechaHitler | Kind::Hitler => snd::DIESND as u8,
        Kind::Mutant => return None,
    })
}

/// The sound an enemy makes when firing (WL_ACT2.C T_Shoot).
fn shoot_sound(kind: Kind) -> u8 {
    match kind {
        Kind::Ss => snd::SSFIRESND as u8,
        Kind::Gift | Kind::Fat => snd::MISSILEFIRESND as u8,
        Kind::MechaHitler | Kind::Hitler | Kind::Hans => snd::BOSSFIRESND as u8,
        Kind::Schabbs => snd::SCHABBSTHROWSND as u8,
        Kind::FakeHitler => snd::FLAMETHROWERSND as u8,
        _ => snd::NAZIFIRESND as u8,
    }
}

/// PlaceItemType drop for a killed enemy (WL_STATE.C KillActor): guards,
/// officers and mutants drop a small ammo clip; SS drop a machine gun; Hans
/// and Gretel drop the gold key; everything else drops nothing.
fn death_drop(kind: Kind) -> Option<Bonus> {
    match kind {
        Kind::Guard | Kind::Officer | Kind::Mutant => Some(Bonus::Clip2),
        Kind::Ss => Some(Bonus::MachineGun),
        Kind::Hans | Kind::Gretel => Some(Bonus::Key1),
        _ => None,
    }
}

impl Actors {
    /// Spawn the level's enemies for the given `skill` (0 baby .. 3 hard),
    /// mirroring WL_GAME.C ScanInfoPlane: base spawn codes appear on every
    /// skill, the medium codes only at skill >= medium, the hard codes only at
    /// skill == hard. Bosses and corpses ignore skill.
    pub fn spawn_from_level(level: &Level, skill: u8) -> Self {
        let table = build_states();
        let mut list = Vec::new();
        for (i, &code) in level.plane1.iter().enumerate() {
            let tx = (i % MAP_SIZE) as i32;
            let ty = (i / MAP_SIZE) as i32;
            if code == 124 {
                // Dead guard corpse.
                let kd = table.kinds[Kind::Guard.index()];
                let dead_state = last_die(&table, kd.die1);
                list.push(Actor {
                    kind: Kind::Guard,
                    x: tx as f32 + 0.5,
                    y: ty as f32 + 0.5,
                    tilex: tx,
                    tiley: ty,
                    dir: NODIR,
                    state: dead_state,
                    ticcount: 0.0,
                    health: 0,
                    speed: 0.0,
                    distance: 0.0,
                    waiting_door: None,
                    flags: 0,
                    reaction: 0.0,
                    dead: true,
                    visible: false,
                });
                continue;
            }
            if let Some((kind, dir)) = decode_boss_spawn(code) {
                // Bosses (WL_ACT2.C Spawn* functions): always standing, always
                // FL_AMBUSH — they only wake on line of sight, never on noise.
                // Pac-Man ghosts (codes 224..=227, E3M10) are not implemented.
                let kd = table.kinds[kind.index()];
                list.push(Actor {
                    kind,
                    x: tx as f32 + 0.5,
                    y: ty as f32 + 0.5,
                    tilex: tx,
                    tiley: ty,
                    dir,
                    state: kd.stand,
                    ticcount: 0.0,
                    health: kind.hitpoints(),
                    speed: SPD_PATROL,
                    distance: 0.0,
                    waiting_door: None,
                    flags: FL_SHOOTABLE | FL_AMBUSH,
                    reaction: 0.0,
                    dead: false,
                    visible: false,
                });
                continue;
            }
            if let Some((kind, patrol, dir, tier)) = decode_spawn(code) {
                if skill < tier_min_skill(tier) {
                    continue;
                }
                let kd = table.kinds[kind.index()];
                let ambush = level.plane0[i] == 106;
                let speed = if kind == Kind::Dog { SPD_DOG } else { SPD_PATROL };
                let mut a = Actor {
                    kind,
                    x: tx as f32 + 0.5,
                    y: ty as f32 + 0.5,
                    tilex: tx,
                    tiley: ty,
                    dir: dir * 2, // spawn dir 0..3 -> dirtype east/north/west/south
                    state: if patrol { kd.path1 } else { kd.stand },
                    ticcount: 0.0,
                    health: kind.hitpoints(),
                    speed,
                    distance: 0.0,
                    waiting_door: None,
                    flags: FL_SHOOTABLE | if ambush { FL_AMBUSH } else { 0 },
                    reaction: 0.0,
                    dead: false,
                    visible: false,
                };
                // Randomize the starting frame phase, as SpawnNewObj does.
                let mut rnd = Rnd { index: list.len() & 0xff };
                let tt = table.states[a.state].tics;
                if tt > 0 {
                    a.ticcount = (rnd.roll() % tt as u32) as f32;
                }
                if patrol {
                    // SpawnPatrol steps the goal tile one ahead in `dir`.
                    let (dx, dy) = dir_delta(a.dir);
                    a.tilex += dx;
                    a.tiley += dy;
                    a.distance = 1.0;
                }
                list.push(a);
            }
        }
        Self {
            list,
            projectiles: Vec::new(),
            table,
            rnd: Rnd::new(),
            reachable: vec![false; MAP_SIZE * MAP_SIZE],
            occ: vec![false; MAP_SIZE * MAP_SIZE],
            damage: 0,
            drops: Vec::new(),
            deaths: Vec::new(),
            sounds: Vec::new(),
            snd_rnd: Rnd::new(),
        }
    }

    /// Drain the enemy drops accumulated since the last call.
    pub fn take_drops(&mut self) -> Vec<(i32, i32, Bonus)> {
        std::mem::take(&mut self.drops)
    }

    /// Drain the enemy sound events accumulated since the last call.
    pub fn take_sounds(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sounds)
    }

    /// Drain the kinds killed since the last call.
    pub fn take_deaths(&mut self) -> Vec<Kind> {
        std::mem::take(&mut self.deaths)
    }

    /// Advance all actors one step. `tics` is elapsed time in tic units.
    /// `madenoise` is set when the player fired this step (WL_STATE.C). Returns
    /// total damage dealt to the player.
    pub fn update(&mut self, tics: f32, world: &mut World, px: f32, py: f32, running: bool, madenoise: bool) {
        self.damage = 0;
        let ptx = px.floor() as i32;
        let pty = py.floor() as i32;
        self.compute_reachable(world, ptx, pty);
        self.rebuild_occupancy();

        for i in 0..self.list.len() {
            // Clear our own tile so TryWalk doesn't see us as a blocker.
            let t = self.list[i].tile();
            self.occ[t] = false;
            self.do_actor(i, tics, world, px, py, ptx, pty, running, madenoise);
            if !self.list[i].dead {
                let t = self.list[i].tile();
                self.occ[t] = true;
            }
        }

        // Publish blocking tiles for the player's collision test.
        world.actor_blocked.copy_from_slice(&self.occ);

        self.update_projectiles(tics, world, px, py);
    }

    /// T_Projectile: fly, die on walls / shut doors, damage the player on
    /// contact.
    fn update_projectiles(&mut self, tics: f32, world: &World, px: f32, py: f32) {
        for i in 0..self.projectiles.len() {
            if self.projectiles[i].dead {
                continue;
            }
            {
                let p = &mut self.projectiles[i];
                p.anim += tics;
                // The original clamps per-frame movement to one tile.
                p.x += (p.vx * tics).clamp(-1.0, 1.0);
                p.y += (p.vy * tics).clamp(-1.0, 1.0);
            }
            let (x, y, kind) = (self.projectiles[i].x, self.projectiles[i].y, self.projectiles[i].kind);
            if !projectile_try_move(world, x, y) {
                self.projectiles[i].dead = true;
                continue;
            }
            if (x - px).abs() < PROJECTILE_SIZE && (y - py).abs() < PROJECTILE_SIZE {
                let damage = match kind {
                    ProjKind::Needle => (self.rnd.roll() >> 3) + 20,
                    ProjKind::Rocket => (self.rnd.roll() >> 3) + 30,
                    ProjKind::Fire => self.rnd.roll() >> 3,
                };
                self.damage += damage as i32;
                self.projectiles[i].dead = true;
            }
        }
        self.projectiles.retain(|p| !p.dead);
    }

    /// Drain the damage accumulated in the last `update`.
    pub fn take_damage(&mut self) -> i32 {
        std::mem::take(&mut self.damage)
    }

    fn rebuild_occupancy(&mut self) {
        self.occ.iter_mut().for_each(|b| *b = false);
        for a in &self.list {
            if !a.dead {
                self.occ[a.tile()] = true;
            }
        }
    }

    /// Flood fill from the player's tile through non-wall tiles, crossing doors
    /// only when they are open enough. Stands in for `areabyplayer`.
    fn compute_reachable(&mut self, world: &World, ptx: i32, pty: i32) {
        self.reachable.iter_mut().for_each(|b| *b = false);
        if ptx < 0 || pty < 0 || ptx >= MAP_SIZE as i32 || pty >= MAP_SIZE as i32 {
            return;
        }
        let mut stack = vec![(ptx, pty)];
        self.reachable[pty as usize * MAP_SIZE + ptx as usize] = true;
        while let Some((x, y)) = stack.pop() {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= MAP_SIZE as i32 || ny >= MAP_SIZE as i32 {
                    continue;
                }
                let idx = ny as usize * MAP_SIZE + nx as usize;
                if self.reachable[idx] {
                    continue;
                }
                if world.wall_at(nx, ny) {
                    continue;
                }
                if world.door_lookup(nx, ny).is_some_and(|d| world.door_position(d) < DOOR_FLOOD) {
                    continue;
                }
                self.reachable[idx] = true;
                stack.push((nx, ny));
            }
        }
    }

    fn reachable_tile(&self, tx: i32, ty: i32) -> bool {
        if tx < 0 || ty < 0 || tx >= MAP_SIZE as i32 || ty >= MAP_SIZE as i32 {
            return false;
        }
        self.reachable[ty as usize * MAP_SIZE + tx as usize]
    }

    // ---- DoActor state-machine driver (WL_PLAY.C) -------------------------

    #[allow(clippy::too_many_arguments)]
    fn do_actor(&mut self, i: usize, tics: f32, world: &mut World, px: f32, py: f32, ptx: i32, pty: i32, running: bool, madenoise: bool) {
        // Permanent (tictime 0) state: just think each frame.
        if self.list[i].ticcount == 0.0 {
            let think = self.table.states[self.list[i].state].think;
            self.run_think(i, think, tics, world, px, py, ptx, pty, madenoise);
            return;
        }
        self.list[i].ticcount -= tics;
        while self.list[i].ticcount <= 0.0 {
            let action = self.table.states[self.list[i].state].action;
            self.run_action(i, action, world, px, py, ptx, pty, running);
            let next = self.table.states[self.list[i].state].next;
            self.list[i].state = next;
            let tt = self.table.states[next].tics;
            if tt == 0 {
                self.list[i].ticcount = 0.0;
                break;
            }
            self.list[i].ticcount += tt as f32;
        }
        let think = self.table.states[self.list[i].state].think;
        self.run_think(i, think, tics, world, px, py, ptx, pty, madenoise);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_think(&mut self, i: usize, think: Think, tics: f32, world: &mut World, px: f32, py: f32, ptx: i32, pty: i32, madenoise: bool) {
        match think {
            Think::None => {}
            Think::Stand => {
                self.sight_player(i, tics, world, px, py, ptx, pty, madenoise);
            }
            Think::Path => {
                if self.sight_player(i, tics, world, px, py, ptx, pty, madenoise) {
                    return;
                }
                self.t_path(i, tics, world, px, py);
            }
            Think::Chase => self.t_chase(i, tics, world, px, py, ptx, pty, Chase::Normal),
            Think::DogChase => self.t_chase(i, tics, world, px, py, ptx, pty, Chase::Dog),
            Think::SchabbChase => self.t_chase(i, tics, world, px, py, ptx, pty, Chase::Schabb),
            Think::FakeChase => self.t_chase(i, tics, world, px, py, ptx, pty, Chase::Fake),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_action(&mut self, i: usize, action: Action, world: &World, px: f32, py: f32, ptx: i32, pty: i32, running: bool) {
        match action {
            Action::None => {}
            Action::DeathScream => {
                let s = self.death_sound(self.list[i].kind);
                self.sounds.push(s);
            }
            Action::Shoot => self.t_shoot(i, world, px, py, ptx, pty, running),
            Action::Bite => self.t_bite(i, px, py),
            Action::ThrowNeedle => self.throw(i, px, py, ProjKind::Needle),
            Action::ThrowRocket => self.throw(i, px, py, ProjKind::Rocket),
            Action::FakeFire => self.throw(i, px, py, ProjKind::Fire),
            Action::HitlerMorph => self.hitler_morph(i),
        }
    }

    // ---- Perception (WL_STATE.C) ------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn sight_player(&mut self, i: usize, tics: f32, world: &World, px: f32, py: f32, _ptx: i32, _pty: i32, madenoise: bool) -> bool {
        if self.list[i].flags & FL_ATTACKMODE != 0 {
            return false;
        }
        if self.list[i].reaction != 0.0 {
            self.list[i].reaction -= tics;
            if self.list[i].reaction > 0.0 {
                return false;
            }
            self.list[i].reaction = 0.0;
        } else {
            let a = &self.list[i];
            if !self.reachable_tile(a.tilex, a.tiley) {
                return false;
            }
            if a.flags & FL_AMBUSH != 0 {
                if !self.check_sight(i, world, px, py) {
                    return false;
                }
                self.list[i].flags &= !FL_AMBUSH;
            } else if !madenoise && !self.check_sight(i, world, px, py) {
                return false;
            }
            // Reaction delay before reacting (WL_STATE.C SightPlayer);
            // bosses react in a single tic.
            let r = &mut self.list[i];
            r.reaction = match r.kind {
                Kind::Guard => 1.0 + (self.rnd.roll() / 4) as f32,
                Kind::Officer => 2.0,
                Kind::Mutant | Kind::Ss => 1.0 + (self.rnd.roll() / 6) as f32,
                Kind::Dog => 1.0 + (self.rnd.roll() / 8) as f32,
                _ => 1.0,
            };
            return false;
        }
        self.first_sighting(i);
        true
    }

    fn check_sight(&self, i: usize, world: &World, px: f32, py: f32) -> bool {
        let a = &self.list[i];
        if !self.reachable_tile(a.tilex, a.tiley) {
            return false;
        }
        let dpx = px - a.x;
        let dpy = py - a.y;
        if dpx.abs() < MIN_SIGHT && dpy.abs() < MIN_SIGHT {
            return true;
        }
        // Facing cone (cardinal dirs only, as in the original).
        let behind = match a.dir {
            NORTH => dpy > 0.0,
            EAST => dpx < 0.0,
            SOUTH => dpy < 0.0,
            WEST => dpx > 0.0,
            _ => false,
        };
        if behind {
            return false;
        }
        check_line(world, a.x, a.y, px, py)
    }

    // ---- FirstSighting / combat entry (WL_STATE.C) ------------------------

    fn first_sighting(&mut self, i: usize) {
        if let Some(s) = alert_sound(self.list[i].kind) {
            self.sounds.push(s);
        }
        let kd = self.table.kinds[self.list[i].kind.index()];
        let mult = self.list[i].kind.chase_mult();
        let a = &mut self.list[i];
        a.state = kd.chase1;
        a.ticcount = self.table.states[kd.chase1].tics as f32;
        a.speed *= mult;
        a.flags |= FL_ATTACKMODE | FL_FIRSTATTACK;
        if a.distance < 0.0 {
            a.distance = 0.0;
        }
        a.waiting_door = None;
        a.reaction = 0.0;
    }

    fn new_state(&mut self, i: usize, state: usize) {
        self.list[i].state = state;
        self.list[i].ticcount = self.table.states[state].tics as f32;
    }

    // ---- Movement (WL_STATE.C / WL_ACT2.C) --------------------------------

    fn t_path(&mut self, i: usize, tics: f32, world: &mut World, px: f32, py: f32) {
        if self.list[i].dir == NODIR {
            self.select_path_dir(i, world);
            if self.list[i].dir == NODIR {
                return;
            }
        }
        let mut move_left = self.list[i].speed / TILE_GLOBAL * tics;
        while move_left > 0.0 {
            if !self.service_door(i, world) {
                return;
            }
            if move_left < self.list[i].distance {
                self.move_obj(i, move_left, px, py);
                break;
            }
            move_left -= self.list[i].distance;
            self.snap_to_goal(i);
            self.select_path_dir(i, world);
            if self.list[i].dir == NODIR {
                return;
            }
        }
    }

    fn select_path_dir(&mut self, i: usize, world: &mut World) {
        // Turn markers live in plane 1 as ICONARROWS(90)+dir (WL_ACT2.C).
        let t = self.list[i].tile();
        let marker = world.plane1_at(t);
        if (90..=97).contains(&marker) {
            self.list[i].dir = (marker - 90) as u8;
        }
        self.list[i].distance = 1.0;
        if !self.try_walk(i, self.list[i].dir, world) {
            self.list[i].dir = NODIR;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn t_chase(&mut self, i: usize, tics: f32, world: &mut World, px: f32, py: f32, ptx: i32, pty: i32, mode: Chase) {
        let dog = mode == Chase::Dog;
        let dx = (self.list[i].tilex - ptx).abs();
        let dy = (self.list[i].tiley - pty).abs();
        let dist = dx.max(dy);

        let mut dodge = false;
        if !dog {
            // Humanoid: try to line up a shot. The attack chance differs per
            // think: T_Chase scales by distance, T_Schabb/T_Gift/T_Fat use a
            // flat tics<<3 and T_Fake a flat tics<<1 (WL_ACT2.C).
            if check_line(world, self.list[i].x, self.list[i].y, px, py) {
                let chance = match mode {
                    Chase::Schabb => (tics * 8.0) as i32,
                    Chase::Fake => (tics * 2.0) as i32,
                    _ if dist == 0 || (dist == 1 && self.list[i].distance < 0.25) => 300,
                    _ => ((tics * 16.0) as i32) / dist,
                };
                if self.rnd.roll() < chance as u32 {
                    let attack = self.table.kinds[self.list[i].kind.index()].attack1;
                    self.new_state(i, attack);
                    return;
                }
                if mode != Chase::Fake {
                    dodge = true;
                }
            }
        }

        // Direction re-selection differs per think as well: T_Fake always
        // dodges; T_Schabb runs *at* the player when within 4 tiles.
        let pick = |s: &mut Self, world: &mut World, at_goal: bool| match mode {
            Chase::Fake => s.select_dodge_dir(i, world, ptx, pty),
            Chase::Schabb if at_goal && dist < 4 => s.select_run_dir(i, world, ptx, pty),
            _ if dodge || dog => s.select_dodge_dir(i, world, ptx, pty),
            _ => s.select_chase_dir(i, world, ptx, pty),
        };

        if self.list[i].dir == NODIR {
            pick(self, world, false);
            if self.list[i].dir == NODIR {
                return;
            }
        }

        let mut move_left = self.list[i].speed / TILE_GLOBAL * tics;
        while move_left > 0.0 {
            // Dog melee check before moving (WL_ACT2.C T_DogChase).
            if dog {
                let ddx = (px - self.list[i].x).abs() - move_left;
                if ddx <= MIN_ACTOR_DIST {
                    let ddy = (py - self.list[i].y).abs() - move_left;
                    if ddy <= MIN_ACTOR_DIST {
                        let jump = self.table.kinds[self.list[i].kind.index()].attack1;
                        self.new_state(i, jump);
                        return;
                    }
                }
            }
            if !self.service_door(i, world) {
                return;
            }
            if move_left < self.list[i].distance {
                self.move_obj(i, move_left, px, py);
                break;
            }
            move_left -= self.list[i].distance;
            self.snap_to_goal(i);
            pick(self, world, true);
            if self.list[i].dir == NODIR {
                return;
            }
        }
    }

    /// Start / poll the door the actor is waiting on. Returns true once open.
    fn service_door(&mut self, i: usize, world: &mut World) -> bool {
        if let Some(d) = self.list[i].waiting_door {
            world.request_open_door(d);
            if !world.door_open_enough(d) {
                return false;
            }
            self.list[i].waiting_door = None;
            self.list[i].distance = 1.0;
        }
        true
    }

    fn snap_to_goal(&mut self, i: usize) {
        let a = &mut self.list[i];
        a.x = a.tilex as f32 + 0.5;
        a.y = a.tiley as f32 + 0.5;
    }

    /// MoveObj: slide `move` along `dir`, backing off if it would enter the
    /// player's personal space.
    fn move_obj(&mut self, i: usize, mv: f32, px: f32, py: f32) {
        let (dx, dy) = dir_delta(self.list[i].dir);
        self.list[i].x += dx as f32 * mv;
        self.list[i].y += dy as f32 * mv;
        let (tilex, tiley, ax, ay) = {
            let a = &self.list[i];
            (a.tilex, a.tiley, a.x, a.y)
        };
        if self.reachable_tile(tilex, tiley)
            && (ax - px).abs() <= MIN_ACTOR_DIST
            && (ay - py).abs() <= MIN_ACTOR_DIST
        {
            // Back up — don't stand on the player.
            self.list[i].x -= dx as f32 * mv;
            self.list[i].y -= dy as f32 * mv;
            return;
        }
        self.list[i].distance -= mv;
    }

    fn select_chase_dir(&mut self, i: usize, world: &mut World, ptx: i32, pty: i32) {
        let olddir = self.list[i].dir;
        let turnaround = OPPOSITE[olddir as usize];
        let deltax = ptx - self.list[i].tilex;
        let deltay = pty - self.list[i].tiley;

        let mut d = [NODIR, NODIR];
        if deltax > 0 {
            d[0] = EAST;
        } else if deltax < 0 {
            d[0] = WEST;
        }
        if deltay > 0 {
            d[1] = SOUTH;
        } else if deltay < 0 {
            d[1] = NORTH;
        }
        if deltay.abs() > deltax.abs() {
            d.swap(0, 1);
        }
        if d[0] == turnaround {
            d[0] = NODIR;
        }
        if d[1] == turnaround {
            d[1] = NODIR;
        }

        for &cand in &d {
            if cand != NODIR {
                self.list[i].dir = cand;
                if self.try_walk(i, cand, world) {
                    return;
                }
            }
        }
        if olddir != NODIR {
            self.list[i].dir = olddir;
            if self.try_walk(i, olddir, world) {
                return;
            }
        }
        // Search all remaining directions (order depends on a die roll).
        if self.rnd.roll() > 128 {
            for tdir in NORTH..=WEST {
                if tdir != turnaround {
                    self.list[i].dir = tdir;
                    if self.try_walk(i, tdir, world) {
                        return;
                    }
                }
            }
        } else {
            for tdir in (NORTH..=WEST).rev() {
                if tdir != turnaround {
                    self.list[i].dir = tdir;
                    if self.try_walk(i, tdir, world) {
                        return;
                    }
                }
            }
        }
        if turnaround != NODIR {
            self.list[i].dir = turnaround;
            if self.try_walk(i, turnaround, world) {
                return;
            }
        }
        self.list[i].dir = NODIR;
    }

    fn select_dodge_dir(&mut self, i: usize, world: &mut World, ptx: i32, pty: i32) {
        let turnaround = if self.list[i].flags & FL_FIRSTATTACK != 0 {
            self.list[i].flags &= !FL_FIRSTATTACK;
            NODIR
        } else {
            OPPOSITE[self.list[i].dir as usize]
        };
        let deltax = ptx - self.list[i].tilex;
        let deltay = pty - self.list[i].tiley;

        // dirtry[1..=4] as in WL_STATE.C; index 0 filled with the diagonal.
        let mut t = [NODIR; 5];
        if deltax > 0 {
            t[1] = EAST;
            t[3] = WEST;
        } else {
            t[1] = WEST;
            t[3] = EAST;
        }
        if deltay > 0 {
            t[2] = SOUTH;
            t[4] = NORTH;
        } else {
            t[2] = NORTH;
            t[4] = SOUTH;
        }
        if deltax.unsigned_abs() > deltay.unsigned_abs() {
            t.swap(1, 2);
            t.swap(3, 4);
        }
        if self.rnd.roll() < 128 {
            t.swap(1, 2);
            t.swap(3, 4);
        }
        t[0] = diagonal(t[1], t[2]);

        for &cand in &t {
            if cand == NODIR || cand == turnaround {
                continue;
            }
            self.list[i].dir = cand;
            if self.try_walk(i, cand, world) {
                return;
            }
        }
        if turnaround != NODIR {
            self.list[i].dir = turnaround;
            if self.try_walk(i, turnaround, world) {
                return;
            }
        }
        self.list[i].dir = NODIR;
    }

    /// SelectRunDir (WL_STATE.C): head *away* from the player, no turnaround
    /// restriction. Schabbs/Gift/Fat use it to charge through the player's
    /// position when cornered close.
    fn select_run_dir(&mut self, i: usize, world: &mut World, ptx: i32, pty: i32) {
        let deltax = ptx - self.list[i].tilex;
        let deltay = pty - self.list[i].tiley;

        let mut d = [NODIR; 2];
        d[0] = if deltax < 0 { EAST } else { WEST };
        d[1] = if deltay < 0 { SOUTH } else { NORTH };
        if deltay.abs() > deltax.abs() {
            d.swap(0, 1);
        }
        for &cand in &d {
            self.list[i].dir = cand;
            if self.try_walk(i, cand, world) {
                return;
            }
        }
        // No direct path away; search all directions, order by a die roll.
        if self.rnd.roll() > 128 {
            for tdir in NORTH..=WEST {
                self.list[i].dir = tdir;
                if self.try_walk(i, tdir, world) {
                    return;
                }
            }
        } else {
            for tdir in (NORTH..=WEST).rev() {
                self.list[i].dir = tdir;
                if self.try_walk(i, tdir, world) {
                    return;
                }
            }
        }
        self.list[i].dir = NODIR;
    }

    /// TryWalk: attempt to move into the `dir` tile, updating the goal tile and
    /// distance. Returns false if a wall or actor blocks the way; a closed door
    /// is opened and the actor waits. Diagonal steps require all touched tiles
    /// clear and never cross doors (WL_STATE.C).
    fn try_walk(&mut self, i: usize, dir: u8, world: &mut World) -> bool {
        let (tx, ty) = (self.list[i].tilex, self.list[i].tiley);
        let dog = self.list[i].kind == Kind::Dog;
        let mut doornum: Option<usize> = None;

        let ok = match dir {
            NORTH | EAST | SOUTH | WEST => {
                let (dx, dy) = dir_delta(dir);
                let (nx, ny) = (tx + dx, ty + dy);
                if dog {
                    self.check_diag(nx, ny, world)
                } else {
                    self.check_side(nx, ny, world, &mut doornum)
                }
            }
            NORTHEAST => {
                self.check_diag(tx + 1, ty - 1, world)
                    && self.check_diag(tx + 1, ty, world)
                    && self.check_diag(tx, ty - 1, world)
            }
            SOUTHEAST => {
                self.check_diag(tx + 1, ty + 1, world)
                    && self.check_diag(tx + 1, ty, world)
                    && self.check_diag(tx, ty + 1, world)
            }
            SOUTHWEST => {
                self.check_diag(tx - 1, ty + 1, world)
                    && self.check_diag(tx - 1, ty, world)
                    && self.check_diag(tx, ty + 1, world)
            }
            NORTHWEST => {
                self.check_diag(tx - 1, ty - 1, world)
                    && self.check_diag(tx - 1, ty, world)
                    && self.check_diag(tx, ty - 1, world)
            }
            _ => return false,
        };
        if !ok {
            return false;
        }
        let (dx, dy) = dir_delta(dir);
        self.list[i].tilex = tx + dx;
        self.list[i].tiley = ty + dy;
        if let Some(d) = doornum {
            world.request_open_door(d);
            self.list[i].waiting_door = Some(d);
            self.list[i].distance = 1.0;
        } else {
            self.list[i].distance = 1.0;
        }
        true
    }

    /// CHECKDIAG: blocked by walls, doors, or any live actor.
    fn check_diag(&self, x: i32, y: i32, world: &World) -> bool {
        if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
            return false;
        }
        if world.wall_at(x, y) || world.door_lookup(x, y).is_some() {
            return false;
        }
        !self.occ[y as usize * MAP_SIZE + x as usize]
    }

    /// CHECKSIDE: walls/actors block; a door is passable but must be opened.
    fn check_side(&self, x: i32, y: i32, world: &World, doornum: &mut Option<usize>) -> bool {
        if x < 0 || y < 0 || x >= MAP_SIZE as i32 || y >= MAP_SIZE as i32 {
            return false;
        }
        if world.wall_at(x, y) {
            return false;
        }
        if let Some(d) = world.door_lookup(x, y) {
            *doornum = Some(d);
            return true;
        }
        !self.occ[y as usize * MAP_SIZE + x as usize]
    }

    // ---- Enemy attacks (WL_ACT2.C) ----------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn t_shoot(&mut self, i: usize, world: &World, px: f32, py: f32, ptx: i32, pty: i32, running: bool) {
        let a = &self.list[i];
        if !self.reachable_tile(a.tilex, a.tiley) {
            return;
        }
        if !check_line(world, a.x, a.y, px, py) {
            return;
        }
        self.sounds.push(shoot_sound(a.kind));
        let dx = (a.tilex - ptx).abs();
        let dy = (a.tiley - pty).abs();
        let mut dist = dx.max(dy);
        if a.kind == Kind::Ss || a.kind == Kind::Hans {
            dist = dist * 2 / 3; // "ss are better shots" (T_Shoot)
        }
        let visible = a.visible;
        let hitchance = if running {
            if visible { 160 - dist * 16 } else { 160 - dist * 8 }
        } else if visible {
            256 - dist * 16
        } else {
            256 - dist * 8
        };
        if (self.rnd.roll() as i32) < hitchance {
            let damage = if dist < 2 {
                self.rnd.roll() >> 2
            } else if dist < 4 {
                self.rnd.roll() >> 3
            } else {
                self.rnd.roll() >> 4
            };
            self.damage += damage as i32;
        }
    }

    fn t_bite(&mut self, i: usize, px: f32, py: f32) {
        self.sounds.push(snd::DOGATTACKSND as u8);
        let a = &self.list[i];
        let dx = (px - a.x).abs() - 1.0;
        if dx <= MIN_ACTOR_DIST {
            let dy = (py - a.y).abs() - 1.0;
            if dy <= MIN_ACTOR_DIST && self.rnd.roll() < 180 {
                let dmg = self.rnd.roll() >> 4;
                self.damage += dmg as i32;
            }
        }
    }

    // ---- Player attack (WL_AGENT.C GunAttack / KnifeAttack) ---------------

    /// Fire the current weapon down the view center. Returns points scored from
    /// any kill. `knife` selects the melee model (short range, no line check
    /// beyond range).
    pub fn player_fire(&mut self, world: &World, px: f32, py: f32, pangle: f32, knife: bool) -> i32 {
        // Find the nearest live, roughly-centered, unobstructed target.
        let (dir_x, dir_y) = (pangle.cos(), pangle.sin());
        let mut best: Option<usize> = None;
        let mut best_dist = f32::MAX;
        for (i, a) in self.list.iter().enumerate() {
            if a.dead || a.flags & FL_SHOOTABLE == 0 {
                continue;
            }
            let rx = a.x - px;
            let ry = a.y - py;
            let along = rx * dir_x + ry * dir_y; // distance down the aim
            if along <= 0.0 {
                continue;
            }
            let perp = (rx * -dir_y + ry * dir_x).abs(); // lateral offset
            // Target radius ~0.35 tile; must be near the center line.
            if perp > 0.35 {
                continue;
            }
            let dist = (rx * rx + ry * ry).sqrt();
            if knife && dist > 1.5 {
                continue;
            }
            if along < best_dist && check_line(world, a.x, a.y, px, py) {
                best_dist = along;
                best = Some(i);
            }
        }
        let Some(i) = best else { return 0 };

        let ptx = px.floor() as i32;
        let pty = py.floor() as i32;
        let dx = (self.list[i].tilex - ptx).abs();
        let dy = (self.list[i].tiley - pty).abs();
        let dist = dx.max(dy);

        let damage = if knife {
            self.rnd.roll() >> 4
        } else if dist < 2 {
            self.rnd.roll() / 4
        } else if dist < 4 {
            self.rnd.roll() / 6
        } else {
            if self.rnd.roll() / 12 < dist as u32 {
                return 0; // out of range miss
            }
            self.rnd.roll() / 6
        };
        self.damage_actor(i, damage as i32)
    }

    /// DamageActor (WL_STATE.C): apply damage, kill or set a pain frame.
    /// Returns points awarded (nonzero only on a kill).
    fn damage_actor(&mut self, i: usize, mut damage: i32) -> i32 {
        if self.list[i].flags & FL_ATTACKMODE == 0 {
            damage <<= 1; // double damage against an unaware actor
        }
        self.list[i].health -= damage;
        if self.list[i].health <= 0 {
            return self.kill_actor(i);
        }
        if self.list[i].flags & FL_ATTACKMODE == 0 {
            self.first_sighting(i);
        }
        // Dogs and bosses have no pain frame.
        if self.list[i].kind.has_pain() {
            let kd = self.table.kinds[self.list[i].kind.index()];
            let pain = if self.list[i].health & 1 != 0 { kd.pain } else { kd.pain1 };
            self.new_state(i, pain);
        }
        0
    }

    /// The death cry for a dying enemy (WL_ACT2.C A_DeathScream). Guards pick a
    /// random scream from a dedicated RNG so the choice never disturbs gameplay.
    fn death_sound(&mut self, kind: Kind) -> u8 {
        match kind {
            Kind::Guard => {
                let i = self.snd_rnd.roll() as usize % snd::GUARD_DEATH_SCREAMS.len();
                snd::GUARD_DEATH_SCREAMS[i]
            }
            Kind::Mutant => snd::AHHHGSND as u8,
            Kind::Officer => snd::NEINSOVASSND as u8,
            Kind::Ss => snd::LEBENSND as u8,
            Kind::Dog => snd::DOGDEATHSND as u8,
            Kind::Hans => snd::MUTTISND as u8,
            Kind::Schabbs => snd::MEINGOTTSND as u8,
            Kind::FakeHitler => snd::HITLERHASND as u8,
            Kind::MechaHitler => snd::SCHEISTSND as u8,
            Kind::Hitler => snd::EVASND as u8,
            Kind::Gretel => snd::MEINSND as u8,
            Kind::Gift => snd::DONNERSND as u8,
            Kind::Fat => snd::ROSESND as u8,
        }
    }

    fn kill_actor(&mut self, i: usize) -> i32 {
        let kd = self.table.kinds[self.list[i].kind.index()];
        let a = &mut self.list[i];
        a.tilex = a.x.floor() as i32;
        a.tiley = a.y.floor() as i32;
        a.dead = true;
        a.flags &= !FL_SHOOTABLE;
        let (tx, ty, kind) = (a.tilex, a.tiley, a.kind);
        self.new_state(i, kd.die1);
        if let Some(bonus) = death_drop(kind) {
            self.drops.push((tx, ty, bonus));
        }
        self.deaths.push(kind);
        self.list[i].kind.points()
    }

    // ---- Boss actions (WL_ACT2.C) ------------------------------------------

    /// T_SchabbThrow / T_GiftThrow / T_FakeFire: launch a projectile from the
    /// actor toward the player's current position.
    fn throw(&mut self, i: usize, px: f32, py: f32, kind: ProjKind) {
        self.sounds.push(match kind {
            ProjKind::Needle => snd::SCHABBSTHROWSND as u8,
            ProjKind::Rocket => snd::MISSILEFIRESND as u8,
            ProjKind::Fire => snd::FLAMETHROWERSND as u8,
        });
        let a = &self.list[i];
        let angle = (py - a.y).atan2(px - a.x);
        // Raw speeds in global units per tic: 0x2000 needle/rocket, 0x1200 fire.
        let speed = match kind {
            ProjKind::Fire => 0x1200 as f32 / TILE_GLOBAL,
            _ => 0x2000 as f32 / TILE_GLOBAL,
        };
        self.projectiles.push(Projectile {
            x: a.x,
            y: a.y,
            vx: angle.cos() * speed,
            vy: angle.sin() * speed,
            kind,
            anim: 0.0,
            dead: false,
        });
    }

    /// A_HitlerMorph: the mecha suit's death spawns the real Hitler in place,
    /// inheriting position, direction and combat state, at SPDPATROL*5 with
    /// the morph table's hard-tier 900 hitpoints.
    fn hitler_morph(&mut self, i: usize) {
        let kd = self.table.kinds[Kind::Hitler.index()];
        let src = &self.list[i];
        let state = kd.chase1;
        let actor = Actor {
            kind: Kind::Hitler,
            x: src.x,
            y: src.y,
            tilex: src.tilex,
            tiley: src.tiley,
            dir: src.dir,
            state,
            ticcount: self.table.states[state].tics as f32,
            health: Kind::Hitler.hitpoints(),
            speed: SPD_PATROL * 5.0,
            distance: src.distance.max(0.0),
            waiting_door: None,
            flags: (src.flags & FL_ATTACKMODE) | FL_SHOOTABLE,
            reaction: 0.0,
            dead: false,
            visible: false,
        };
        self.list.push(actor);
    }

    // ---- Rendering support ------------------------------------------------

    /// Reset per-frame visibility; the renderer flags actors it draws.
    pub fn clear_visible(&mut self) {
        for a in &mut self.list {
            a.visible = false;
        }
    }

    /// Save/restore the FL_VISABLE flags. Rendering marks visibility (as the
    /// original engine did), which feeds enemy accuracy — so a render is a
    /// simulation side effect. Headless snapshots wrap the render with these
    /// to stay observation-pure: a `snap` must never alter a scripted run.
    pub fn save_visible(&self) -> Vec<bool> {
        self.list.iter().map(|a| a.visible).collect()
    }

    pub fn restore_visible(&mut self, saved: &[bool]) {
        for (a, &v) in self.list.iter_mut().zip(saved) {
            a.visible = v;
        }
    }

    /// Current sprite for actor `idx`, seen from the given viewer position.
    /// Rotating states add the 8-view offset (the original's CalcRotate).
    pub fn sprite_of(&mut self, idx: usize, viewer_x: f32, viewer_y: f32) -> usize {
        self.list[idx].visible = true;
        let a = &self.list[idx];
        let st = &self.table.states[a.state];
        if !st.rotate {
            return st.sprite as usize;
        }
        let rot = rotation_view(a.dir, a.x, a.y, viewer_x, viewer_y);
        st.sprite as usize + rot
    }
}

/// Compute the 8-view rotation frame for an actor facing `dir` at (ax,ay) seen
/// from (vx,vy). Port of WL_DRAW.C CalcRotate into this port's coordinate frame
/// (angle 0 = +x east, +y = south / screen down).
fn rotation_view(dir: u8, ax: f32, ay: f32, vx: f32, vy: f32) -> usize {
    use std::f32::consts::FRAC_PI_4;
    let dir = if dir >= 8 { 0 } else { dir };
    // Facing angle of the actor in this port's frame.
    rotation_from_angle(-(dir as f32) * FRAC_PI_4, ax, ay, vx, vy)
}

/// Like [`rotation_view`] but from an arbitrary facing angle (used for rocket
/// projectiles, whose facing is their flight direction).
fn rotation_from_angle(facing: f32, ax: f32, ay: f32, vx: f32, vy: f32) -> usize {
    // Angle from the actor toward the viewer.
    let to_viewer = (vy - ay).atan2(vx - ax);
    let rel = to_viewer - facing;
    // The original's art winds the opposite way to our clockwise angle sign;
    // negate, bias by half a sector, then quantize into 8.
    let deg = (-rel).to_degrees() + 22.5;
    ((deg.rem_euclid(360.0)) / 45.0) as usize % 8
}

/// ProjectileTryMove (WL_ACT2.C): a projectile survives while its PROJSIZE
/// box overlaps no wall and no shut door.
fn projectile_try_move(world: &World, x: f32, y: f32) -> bool {
    let xl = (x - PROJ_SIZE).floor() as i32;
    let xh = (x + PROJ_SIZE).floor() as i32;
    let yl = (y - PROJ_SIZE).floor() as i32;
    let yh = (y + PROJ_SIZE).floor() as i32;
    for ty in yl..=yh {
        for tx in xl..=xh {
            if world.wall_at(tx, ty) {
                return false;
            }
            // A door blocks like a wall until it has slid (nearly) fully open
            // (the original keeps opening/closing doors in `actorat`).
            if world.door_lookup(tx, ty).is_some_and(|d| !world.door_open_enough(d)) {
                return false;
            }
        }
    }
    true
}

/// The last (dead) state in a die chain — used for corpses.
fn last_die(table: &StateTable, die1: usize) -> usize {
    let mut s = die1;
    loop {
        let next = table.states[s].next;
        if next == s {
            return s;
        }
        s = next;
    }
}

/// Public [`check_line`] wrapper for scripting/debug callers (the demo's
/// auto-aim uses it to skip enemies behind walls).
pub fn line_clear(world: &World, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    check_line(world, x0, y0, x1, y1)
}

/// CheckLine (WL_STATE.C): is the segment from (x0,y0) to (x1,y1) unobstructed
/// by walls or shut doors? Sampled on a fine grid (walls are full tiles).
fn check_line(world: &World, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return true;
    }
    let steps = (len * 16.0).ceil() as i32;
    let (sx, sy) = (x0.floor() as i32, y0.floor() as i32);
    let (ex, ey) = (x1.floor() as i32, y1.floor() as i32);
    for s in 1..steps {
        let t = s as f32 / steps as f32;
        let x = x0 + dx * t;
        let y = y0 + dy * t;
        let (tx, ty) = (x.floor() as i32, y.floor() as i32);
        // Ignore the endpoints' own tiles.
        if (tx == sx && ty == sy) || (tx == ex && ty == ey) {
            continue;
        }
        if world.wall_at(tx, ty) {
            return false;
        }
        if world.door_lookup(tx, ty).is_some_and(|d| world.door_position(d) < DOOR_SEE) {
            return false;
        }
    }
    true
}

/// Decode a boss spawn code into (kind, facing dir). Bosses are single tiles
/// with no difficulty or direction variants (WL_GAME.C ScanInfoPlane); the
/// facing comes from each WL_ACT2.C Spawn* function.
fn decode_boss_spawn(code: u16) -> Option<(Kind, u8)> {
    Some(match code {
        214 => (Kind::Hans, SOUTH),
        196 => (Kind::Schabbs, SOUTH),
        160 => (Kind::FakeHitler, NORTH),
        178 => (Kind::MechaHitler, SOUTH),
        197 => (Kind::Gretel, NORTH),
        215 => (Kind::Gift, NORTH),
        179 => (Kind::Fat, SOUTH),
        _ => return None,
    })
}

/// Decode a plane-1 spawn code into (kind, patrol?, dir 0..3, tier). The tier
/// is 0 for the base codes (present on every skill), 1 for the medium codes and
/// 2 for the hard codes; [`tier_min_skill`] maps it to the lowest skill that
/// spawns it (WL_GAME.C ScanInfoPlane's fall-through `if difficulty < gd_* break`).
fn decode_spawn(code: u16) -> Option<(Kind, bool, u8, u8)> {
    // Guard/officer/ss/dog use +36 per difficulty tier; mutant uses +18.
    let tiers = [
        // (kind, patrol, base of each of the 3 tiers: base / medium / hard)
        (Kind::Guard, false, [108u16, 144, 180]),
        (Kind::Guard, true, [112, 148, 184]),
        (Kind::Officer, false, [116, 152, 188]),
        (Kind::Officer, true, [120, 156, 192]),
        (Kind::Ss, false, [126, 162, 198]),
        (Kind::Ss, true, [130, 166, 202]),
        (Kind::Dog, false, [134, 170, 206]),
        (Kind::Dog, true, [138, 174, 210]),
        (Kind::Mutant, false, [216, 234, 252]),
        (Kind::Mutant, true, [220, 238, 256]),
    ];
    for &(kind, patrol, bases) in &tiers {
        for (tier, &base) in bases.iter().enumerate() {
            if code >= base && code < base + 4 {
                return Some((kind, patrol, (code - base) as u8, tier as u8));
            }
        }
    }
    None
}

/// The lowest skill (0 baby .. 3 hard) at which a spawn tier appears. Base
/// codes spawn everywhere; medium codes need skill >= medium (2); hard codes
/// need skill >= hard (3). (There is no easy-only tier: baby and easy share the
/// base set.)
fn tier_min_skill(tier: u8) -> u8 {
    match tier {
        0 => 0,
        1 => 2,
        _ => 3,
    }
}

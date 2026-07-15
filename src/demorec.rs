//! Attract-mode demo recording: a versioned little-endian binary that captures
//! the [`crate::game::Input`] stream of a play session so it can be replayed
//! bit-deterministically.
//!
//! We do **not** parse the original DEMO0-3 lumps (tic-compressed original input,
//! not worth porting byte-for-byte). Instead we record our *own* demos with the
//! same [`crate::savegame::Writer`]/[`crate::savegame::Reader`] primitives the
//! save games use.
//!
//! ## Layout (version 1)
//!
//! ```text
//! magic       "WOLF3DDM"        8 bytes
//! version     u16               format version (1)
//! level_idx   u32               overall level index (episode*10 + floor)
//! difficulty  u8                skill 0 baby .. 3 hard
//! rng_index   u32               actors' gameplay RNG index at record start
//! snd_rng     u32               actors' sound RNG index at record start
//! player      f32 x, y, angle   starting camera (spawn or a scripted setup pos)
//! health      i32
//! ammo        i32
//! weapon      u32
//! keys        u8
//! god         u8 bool           invincibility during the demo (so it can't die)
//! windowed    u8 bool           render-visibility effects embedded (see below)
//! ntics       u32               number of per-tic input records
//! -- per tic --
//! flags       u16               packed booleans (see PACK below) + presence bits
//! [weapon     u8]               present iff the has-weapon flag is set
//! [turn_delta f32]              present iff the has-turn-delta flag is set
//! ```
//!
//! ## Determinism
//!
//! The simulation is already deterministic given the level, difficulty and the
//! two RNG indices. The one render side effect that leaks into the sim is
//! `FL_VISABLE` (rendering marks actors visible, which feeds enemy accuracy —
//! see [`crate::actors::Actors::save_visible`]). Two recording environments are
//! supported, distinguished by the `windowed` header flag:
//!
//! - **Headless** (`windowed == false`, the `record:`/`endrecord` script
//!   commands): nothing renders during the run, so `visible` stays false
//!   throughout. Playback wraps every on-screen render in
//!   save/restore-visible (exactly like the script's `snap` command), so the
//!   replayed sim also sees `visible == false` every tic. Bit-identical.
//! - **Windowed** (`windowed == true`, `WOLF3D_RECORD=path`): the frontend
//!   steps the game at a fixed 70 Hz tic rate and renders after every tic, so
//!   visibility marking is embedded in the run at tic granularity. Playback
//!   reproduces it by running one visibility-marking render per replayed tic
//!   (renders recompute the flags from scratch, so the on-screen frame render
//!   is idempotent on top).
//!
//! The starting player/loadout fields go slightly beyond "level + difficulty +
//! rng": storing the camera and loadout lets a demo be authored from a scripted
//! setup (teleport / face / a weapon / god) while replay stays purely
//! input-driven from that captured starting state.

use std::path::{Path, PathBuf};

use crate::game::{Game, Input};
use crate::savegame::{Reader, SaveError, Writer};

/// File magic: identifies a Wolf3D demo recording.
pub const MAGIC: &[u8; 8] = b"WOLF3DDM";
/// Current on-disk format version.
pub const VERSION: u16 = 1;

// Per-tic flag bits (PACK).
const F_FORWARD: u16 = 1 << 0;
const F_BACK: u16 = 1 << 1;
const F_STRAFE_L: u16 = 1 << 2;
const F_STRAFE_R: u16 = 1 << 3;
const F_TURN_L: u16 = 1 << 4;
const F_TURN_R: u16 = 1 << 5;
const F_RUN: u16 = 1 << 6;
const F_USE: u16 = 1 << 7;
const F_FIRE: u16 = 1 << 8;
const F_HAS_WEAPON: u16 = 1 << 9;
const F_HAS_TURN_DELTA: u16 = 1 << 10;

/// A recorded demo: the starting game state header plus one packed [`Input`] per
/// 70 Hz tic.
#[derive(Clone, Default)]
pub struct Demo {
    pub level_idx: usize,
    pub difficulty: u8,
    pub rng_index: usize,
    pub snd_rng_index: usize,
    pub player_x: f32,
    pub player_y: f32,
    pub player_angle: f32,
    pub health: i32,
    pub ammo: i32,
    pub weapon: usize,
    pub keys: u8,
    pub god: bool,
    /// True when recorded in the windowed app (render-visibility effects are
    /// embedded per tic; playback re-marks visibility each tic). False for
    /// headless recordings (playback neutralizes visibility instead).
    pub windowed: bool,
    /// One input per tic, in playback order.
    pub tics: Vec<Input>,
}

impl Demo {
    /// Capture the header from the game's current state, ready to record tics.
    /// Call this at the exact tic recording starts (before any recorded tic has
    /// advanced the simulation) so the RNG indices and camera match the run.
    pub fn begin(game: &Game) -> Self {
        Self {
            level_idx: game.level_idx,
            difficulty: game.difficulty.skill(),
            rng_index: game.actors.rng_index(),
            snd_rng_index: game.actors.snd_rng_index(),
            player_x: game.player.x,
            player_y: game.player.y,
            player_angle: game.player.angle,
            health: game.health,
            ammo: game.ammo,
            weapon: game.weapon,
            keys: game.keys,
            god: game.god,
            windowed: false,
            tics: Vec::new(),
        }
    }

    /// Append one tic's input.
    pub fn push(&mut self, input: &Input) {
        self.tics.push(*input);
    }

    /// Serialize to a fresh byte buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.buf.extend_from_slice(MAGIC);
        w.put_u16(VERSION);
        w.put_u32(self.level_idx as u32);
        w.put_u8(self.difficulty);
        w.put_u32(self.rng_index as u32);
        w.put_u32(self.snd_rng_index as u32);
        w.put_f32(self.player_x);
        w.put_f32(self.player_y);
        w.put_f32(self.player_angle);
        w.put_i32(self.health);
        w.put_i32(self.ammo);
        w.put_u32(self.weapon as u32);
        w.put_u8(self.keys);
        w.put_bool(self.god);
        w.put_bool(self.windowed);
        w.put_u32(self.tics.len() as u32);
        for input in &self.tics {
            pack_input(&mut w, input);
        }
        w.buf
    }

    /// Parse from bytes, validating the header.
    pub fn from_bytes(data: &[u8]) -> Result<Self, SaveError> {
        let mut r = Reader::new(data);
        if r.get_bytes(8)? != MAGIC {
            return Err(SaveError::BadMagic);
        }
        let version = r.get_u16()?;
        if version != VERSION {
            return Err(SaveError::BadVersion(version));
        }
        let level_idx = r.get_u32()? as usize;
        let difficulty = r.get_u8()?;
        let rng_index = r.get_u32()? as usize;
        let snd_rng_index = r.get_u32()? as usize;
        let player_x = r.get_f32()?;
        let player_y = r.get_f32()?;
        let player_angle = r.get_f32()?;
        let health = r.get_i32()?;
        let ammo = r.get_i32()?;
        let weapon = r.get_u32()? as usize;
        let keys = r.get_u8()?;
        let god = r.get_bool()?;
        let windowed = r.get_bool()?;
        let ntics = r.get_u32()? as usize;
        let mut tics = Vec::with_capacity(ntics);
        for _ in 0..ntics {
            tics.push(unpack_input(&mut r)?);
        }
        Ok(Self {
            level_idx,
            difficulty,
            rng_index,
            snd_rng_index,
            player_x,
            player_y,
            player_angle,
            health,
            ammo,
            weapon,
            keys,
            god,
            windowed,
            tics,
        })
    }

    /// Write this demo to `path` (creating parent directories).
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_bytes())
    }
}

/// Pack one tic's gameplay input. Menu / text-entry fields are never recorded
/// (they don't affect the play simulation).
fn pack_input(w: &mut Writer, input: &Input) {
    let mut flags = 0u16;
    if input.forward {
        flags |= F_FORWARD;
    }
    if input.back {
        flags |= F_BACK;
    }
    if input.strafe_left {
        flags |= F_STRAFE_L;
    }
    if input.strafe_right {
        flags |= F_STRAFE_R;
    }
    if input.turn_left {
        flags |= F_TURN_L;
    }
    if input.turn_right {
        flags |= F_TURN_R;
    }
    if input.run {
        flags |= F_RUN;
    }
    if input.use_door {
        flags |= F_USE;
    }
    if input.fire {
        flags |= F_FIRE;
    }
    if input.select_weapon.is_some() {
        flags |= F_HAS_WEAPON;
    }
    if input.turn_delta != 0.0 {
        flags |= F_HAS_TURN_DELTA;
    }
    w.put_u16(flags);
    if let Some(weapon) = input.select_weapon {
        w.put_u8(weapon);
    }
    if input.turn_delta != 0.0 {
        w.put_f32(input.turn_delta);
    }
}

fn unpack_input(r: &mut Reader) -> Result<Input, SaveError> {
    let flags = r.get_u16()?;
    let select_weapon = if flags & F_HAS_WEAPON != 0 { Some(r.get_u8()?) } else { None };
    let turn_delta = if flags & F_HAS_TURN_DELTA != 0 { r.get_f32()? } else { 0.0 };
    Ok(Input {
        forward: flags & F_FORWARD != 0,
        back: flags & F_BACK != 0,
        strafe_left: flags & F_STRAFE_L != 0,
        strafe_right: flags & F_STRAFE_R != 0,
        turn_left: flags & F_TURN_L != 0,
        turn_right: flags & F_TURN_R != 0,
        run: flags & F_RUN != 0,
        use_door: flags & F_USE != 0,
        fire: flags & F_FIRE != 0,
        select_weapon,
        turn_delta,
        ..Default::default()
    })
}

/// The directory holding the shipped attract demos (`demos/` under the crate
/// root). `WOLF3D_DEMO_DIR` overrides it, so tests can isolate their files.
pub fn demos_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WOLF3D_DEMO_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demos")
}

/// Load every `*.dm` demo in [`demos_dir`], sorted by file name. Unreadable or
/// corrupt files are skipped; a missing directory yields an empty list (the
/// attract loop then gracefully skips the demo stage).
pub fn load_all() -> Vec<Demo> {
    let dir = demos_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "dm"))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read(p).ok().and_then(|d| Demo::from_bytes(&d).ok()))
        .collect()
}

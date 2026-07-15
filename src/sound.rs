//! The sound engine: AdLib (OPL2) sound effects, IMF music, and digitized PCM,
//! mixed to one output stream.
//!
//! ## Determinism boundary
//!
//! The simulation stays headless-deterministic by never touching audio
//! hardware. Instead the game *emits* sound events — plain sound-enum ids pushed
//! into per-subsystem queues ([`Actors`](crate::actors), [`World`](crate::raycast),
//! [`Game`](crate::game)) and drained once per tic with
//! [`Game::take_sounds`](crate::game::Game::take_sounds). The windowed frontend
//! owns a [`Backend`] that consumes those ids; the headless driver just prints
//! them. No RNG the simulation depends on is consumed here.
//!
//! ## Synthesis
//!
//! One [`Opl3Chip`] (a pure-Rust Nuked-OPL3 core, run in OPL2 mode) renders both
//! the AdLib effects and the IMF music the original played on a single OPL2:
//!
//! - **Music** is an IMF stream of `(u8 reg, u8 data, u16 delay)` events clocked
//!   at 700 Hz (ID_SD.C `SDL_ALService`, `SDL_SetTimerSpeed` -> `TickBase*10`).
//! - **Effects** are a per-tick OPL channel-0 frequency stream at 140 Hz
//!   (`SDL_ALSoundService`); the 16-byte instrument is loaded once on trigger.
//! - **Digitized** effects are 8-bit unsigned PCM at ~7 kHz, mixed in on their
//!   own channel and preferred over the AdLib version when one exists
//!   (`SD_PlaySound`).
//!
//! Priorities follow `SD_PlaySound`: a new effect plays only if its priority
//! (from the AdLib chunk header) is `>=` the one already sounding.

use std::sync::mpsc::{Receiver, Sender};

use nuked_opl3::Opl3Chip;

use crate::assets::audio::{AdLibSound, AudioData, NUM_SOUNDS};
use crate::assets::vswap::DIGI_RATE;

// =============================================================================
// SOUND ENUM (AUDIOWL6.H `soundnames`)
// =============================================================================
//
// Names/values are the v1.4 WL6 sound enum; only these are referenced by the
// game's event sites, but the full set is listed for reference.

pub const HITWALLSND: usize = 0;
pub const SELECTWPNSND: usize = 1;
pub const SELECTITEMSND: usize = 2;
pub const HEARTBEATSND: usize = 3;
pub const MOVEGUN2SND: usize = 4;
pub const MOVEGUN1SND: usize = 5;
pub const NOWAYSND: usize = 6;
pub const NAZIHITPLAYERSND: usize = 7;
pub const SCHABBSTHROWSND: usize = 8;
pub const PLAYERDEATHSND: usize = 9;
pub const DOGDEATHSND: usize = 10;
pub const ATKGATLINGSND: usize = 11;
pub const GETKEYSND: usize = 12;
pub const NOITEMSND: usize = 13;
pub const WALK1SND: usize = 14;
pub const WALK2SND: usize = 15;
pub const TAKEDAMAGESND: usize = 16;
pub const GAMEOVERSND: usize = 17;
pub const OPENDOORSND: usize = 18;
pub const CLOSEDOORSND: usize = 19;
pub const DONOTHINGSND: usize = 20;
pub const HALTSND: usize = 21;
pub const DEATHSCREAM2SND: usize = 22;
pub const ATKKNIFESND: usize = 23;
pub const ATKPISTOLSND: usize = 24;
pub const DEATHSCREAM3SND: usize = 25;
pub const ATKMACHINEGUNSND: usize = 26;
pub const HITENEMYSND: usize = 27;
pub const SHOOTDOORSND: usize = 28;
pub const DEATHSCREAM1SND: usize = 29;
pub const GETMACHINESND: usize = 30;
pub const GETAMMOSND: usize = 31;
pub const SHOOTSND: usize = 32;
pub const HEALTH1SND: usize = 33;
pub const HEALTH2SND: usize = 34;
pub const BONUS1SND: usize = 35;
pub const BONUS2SND: usize = 36;
pub const BONUS3SND: usize = 37;
pub const GETGATLINGSND: usize = 38;
pub const ESCPRESSEDSND: usize = 39;
pub const LEVELDONESND: usize = 40;
pub const DOGBARKSND: usize = 41;
pub const ENDBONUS1SND: usize = 42;
pub const ENDBONUS2SND: usize = 43;
pub const BONUS1UPSND: usize = 44;
pub const BONUS4SND: usize = 45;
pub const PUSHWALLSND: usize = 46;
pub const NOBONUSSND: usize = 47;
pub const PERCENT100SND: usize = 48;
pub const BOSSACTIVESND: usize = 49;
pub const MUTTISND: usize = 50;
pub const SCHUTZADSND: usize = 51;
pub const AHHHGSND: usize = 52;
pub const DIESND: usize = 53;
pub const EVASND: usize = 54;
pub const GUTENTAGSND: usize = 55;
pub const LEBENSND: usize = 56;
pub const SCHEISTSND: usize = 57;
pub const NAZIFIRESND: usize = 58;
pub const BOSSFIRESND: usize = 59;
pub const SSFIRESND: usize = 60;
pub const SLURPIESND: usize = 61;
pub const TOT_HUNDSND: usize = 62;
pub const MEINGOTTSND: usize = 63;
pub const SCHABBSHASND: usize = 64;
pub const HITLERHASND: usize = 65;
pub const SPIONSND: usize = 66;
pub const NEINSOVASSND: usize = 67;
pub const DOGATTACKSND: usize = 68;
pub const FLAMETHROWERSND: usize = 69;
pub const MECHSTEPSND: usize = 70;
pub const GOOBSSND: usize = 71;
pub const YEAHSND: usize = 72;
pub const DEATHSCREAM4SND: usize = 73;
pub const DEATHSCREAM5SND: usize = 74;
pub const DEATHSCREAM6SND: usize = 75;
pub const DEATHSCREAM7SND: usize = 76;
pub const DEATHSCREAM8SND: usize = 77;
pub const DEATHSCREAM9SND: usize = 78;
pub const DONNERSND: usize = 79;
pub const EINESND: usize = 80;
pub const ERLAUBENSND: usize = 81;
pub const KEINSND: usize = 82;
pub const MEINSND: usize = 83;
pub const ROSESND: usize = 84;
pub const MISSILEFIRESND: usize = 85;
pub const MISSILEHITSND: usize = 86;

/// Human-readable names for the sound enum, for headless logging.
const NAMES: [&str; NUM_SOUNDS] = [
    "HITWALL", "SELECTWPN", "SELECTITEM", "HEARTBEAT", "MOVEGUN2", "MOVEGUN1", "NOWAY",
    "NAZIHITPLAYER", "SCHABBSTHROW", "PLAYERDEATH", "DOGDEATH", "ATKGATLING", "GETKEY",
    "NOITEM", "WALK1", "WALK2", "TAKEDAMAGE", "GAMEOVER", "OPENDOOR", "CLOSEDOOR", "DONOTHING",
    "HALT", "DEATHSCREAM2", "ATKKNIFE", "ATKPISTOL", "DEATHSCREAM3", "ATKMACHINEGUN", "HITENEMY",
    "SHOOTDOOR", "DEATHSCREAM1", "GETMACHINE", "GETAMMO", "SHOOT", "HEALTH1", "HEALTH2", "BONUS1",
    "BONUS2", "BONUS3", "GETGATLING", "ESCPRESSED", "LEVELDONE", "DOGBARK", "ENDBONUS1",
    "ENDBONUS2", "BONUS1UP", "BONUS4", "PUSHWALL", "NOBONUS", "PERCENT100", "BOSSACTIVE", "MUTTI",
    "SCHUTZAD", "AHHHG", "DIE", "EVA", "GUTENTAG", "LEBEN", "SCHEIST", "NAZIFIRE", "BOSSFIRE",
    "SSFIRE", "SLURPIE", "TOT_HUND", "MEINGOTT", "SCHABBSHA", "HITLERHA", "SPION", "NEINSOVAS",
    "DOGATTACK", "FLAMETHROWER", "MECHSTEP", "GOOBS", "YEAH", "DEATHSCREAM4", "DEATHSCREAM5",
    "DEATHSCREAM6", "DEATHSCREAM7", "DEATHSCREAM8", "DEATHSCREAM9", "DONNER", "EINE", "ERLAUBEN",
    "KEIN", "MEIN", "ROSE", "MISSILEFIRE", "MISSILEHIT",
];

/// A short name for a sound-enum id, for the headless sound log.
pub fn name(id: u8) -> &'static str {
    NAMES.get(id as usize).copied().unwrap_or("?")
}

/// The eight guard death screams `A_DeathScream` chooses among at random
/// (WL_ACT2.C). Sound variety is picked from a dedicated RNG so it never
/// perturbs the gameplay random stream.
pub const GUARD_DEATH_SCREAMS: [u8; 8] = [
    DEATHSCREAM1SND as u8,
    DEATHSCREAM2SND as u8,
    DEATHSCREAM3SND as u8,
    DEATHSCREAM4SND as u8,
    DEATHSCREAM5SND as u8,
    DEATHSCREAM7SND as u8,
    DEATHSCREAM8SND as u8,
    DEATHSCREAM9SND as u8,
];

// =============================================================================
// MUSIC (AUDIOWL6.H `musicnames`)
// =============================================================================

pub const CORNER_MUS: usize = 0;
pub const DUNGEON_MUS: usize = 1;
pub const WARMARCH_MUS: usize = 2;
/// Played on the level-completed intermission (WL_INTER.C `ENDLEVEL_MUS`).
pub const ENDLEVEL_MUS: usize = 5;
pub const GETTHEM_MUS: usize = 3;
pub const HEADACHE_MUS: usize = 4;
pub const INTROCW3_MUS: usize = 6;
pub const NAZI_OMI_MUS: usize = 8;
pub const POW_MUS: usize = 9;
pub const SEARCHN_MUS: usize = 11;
pub const SUSPENSE_MUS: usize = 12;
pub const WONDERIN_MUS: usize = 14;
pub const ULTIMATE_MUS: usize = 19;
pub const NAZI_RAP_MUS: usize = 20;
pub const ZEROHOUR_MUS: usize = 21;
pub const TWELFTH_MUS: usize = 22;
pub const PREGNANT_MUS: usize = 18;
pub const GOINGAFT_MUS: usize = 17;
pub const PACMAN_MUS: usize = 26;

/// Music played on the menus (WL_MENU.H `MENUSONG`).
pub const MENU_SONG: usize = WONDERIN_MUS;

/// The per-episode song sets from WL_MAIN.C `DoJukebox` (`songs[]`), which lists
/// the six tracks each episode uses. The retail per-floor `songs[]` table lives
/// in a WL_GAME.C function that the public id-Software source drop omits, so the
/// floor within an episode indexes into its jukebox group here — E1 floor 1 is
/// "Get Them", etc.
const JUKEBOX: [[usize; 6]; 3] = [
    [GETTHEM_MUS, SEARCHN_MUS, POW_MUS, SUSPENSE_MUS, WARMARCH_MUS, CORNER_MUS],
    [NAZI_OMI_MUS, PREGNANT_MUS, GOINGAFT_MUS, HEADACHE_MUS, DUNGEON_MUS, ULTIMATE_MUS],
    [INTROCW3_MUS, NAZI_RAP_MUS, TWELFTH_MUS, ZEROHOUR_MUS, ULTIMATE_MUS, PACMAN_MUS],
];

/// The music track for an overall level index (episode*10 + floor).
pub fn song_for_level(level_idx: usize) -> usize {
    let episode = (level_idx / 10) % 3;
    let floor = (level_idx % 10) % 6;
    JUKEBOX[episode][floor]
}

// =============================================================================
// ENGINE ASSETS
// =============================================================================

/// The decoded audio data the [`Engine`] renders from. Built once at startup by
/// moving the AUDIOT contents and the VSWAP digitized PCM together.
pub struct SoundAssets {
    sfx: Vec<Option<AdLibSound>>,
    music: Vec<Vec<u8>>,
    digi: Vec<Vec<u8>>,
    digi_map: [i32; NUM_SOUNDS],
}

impl SoundAssets {
    pub fn new(audio: AudioData, digi: Vec<Vec<u8>>) -> Self {
        Self { sfx: audio.sfx, music: audio.music, digi, digi_map: audio.digi_map }
    }

    pub fn num_music(&self) -> usize {
        self.music.len()
    }

    pub fn num_digi(&self) -> usize {
        self.digi.len()
    }

    /// The decoded IMF stream for a music track (for tests / inspection).
    pub fn music_track(&self, track: usize) -> &[u8] {
        self.music.get(track).map(Vec::as_slice).unwrap_or(&[])
    }
}

// =============================================================================
// ENGINE
// =============================================================================

// OPL register bases (ID_SD.H). Effects always use channel 0.
const AL_CHAR: u16 = 0x20;
const AL_SCALE: u16 = 0x40;
const AL_ATTACK: u16 = 0x60;
const AL_SUS: u16 = 0x80;
const AL_WAVE: u16 = 0xe0;
const AL_FREQ_L: u16 = 0xa0;
const AL_FREQ_H: u16 = 0xb0;
const AL_FEED_CON: u16 = 0xc0;
/// modifiers[0] / carriers[0] operator cells for channel 0 (ID_SD.C).
const FX_MOD: u16 = 0;
const FX_CAR: u16 = 3;

const MUSIC_RATE: f64 = 700.0;
const SFX_RATE: f64 = 140.0;

/// Renders the mixed AdLib + IMF + digitized stream. Pure and self-contained:
/// the live [`Backend`] drives it from the audio callback, and tests drive it
/// with [`render_offline`].
pub struct Engine {
    opl: Opl3Chip,
    sample_rate: u32,
    assets: SoundAssets,

    // Music (IMF at 700 Hz).
    music_track: Option<usize>,
    music_ptr: usize,
    music_wait: f64,
    music_accum: f64,
    music_enabled: bool,

    // AdLib effect (140 Hz frequency stream on channel 0).
    sfx: Option<usize>,
    sfx_pos: usize,
    sfx_block: u8,
    sfx_accum: f64,
    sfx_priority: i32,

    // Digitized effect (8-bit unsigned PCM).
    digi: Option<usize>,
    digi_pos: f64,
    digi_priority: i32,
    /// When false, sounds that have a digitized version fall back to AdLib (the
    /// Sound menu's "AdLib only" effects mode).
    digi_enabled: bool,
}

impl Engine {
    pub fn new(sample_rate: u32, assets: SoundAssets) -> Self {
        let mut opl = Opl3Chip::new(sample_rate.max(1));
        // Keep the chip in OPL2 mode (Wolf3D only ever wrote OPL2 registers) and
        // clear the global control/percussion register.
        opl.write_register(0x01, 0x20); // enable waveform select
        opl.write_register(0xbd, 0x00);
        Self {
            opl,
            sample_rate: sample_rate.max(1),
            assets,
            music_track: None,
            music_ptr: 0,
            music_wait: 0.0,
            music_accum: 0.0,
            music_enabled: true,
            sfx: None,
            sfx_pos: 0,
            sfx_block: 0,
            sfx_accum: 0.0,
            sfx_priority: 0,
            digi: None,
            digi_pos: 0.0,
            digi_priority: 0,
            digi_enabled: true,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Start (or restart) a music track; `None` stops the music.
    pub fn play_music(&mut self, track: Option<usize>) {
        self.all_notes_off();
        self.music_track = track.filter(|&t| t < self.assets.music.len() && !self.assets.music[t].is_empty());
        self.music_ptr = 0;
        self.music_wait = 0.0;
        self.music_accum = 0.0;
    }

    /// Enable or mute the music without forgetting the track (the `M` toggle).
    pub fn set_music_enabled(&mut self, on: bool) {
        if on == self.music_enabled {
            return;
        }
        self.music_enabled = on;
        if !on {
            self.all_notes_off();
        }
    }

    pub fn music_enabled(&self) -> bool {
        self.music_enabled
    }

    /// Enable/disable digitized playback. When disabled, sounds that have a
    /// digitized version play their AdLib rendition instead (the Sound menu's
    /// "AdLib only" effects mode).
    pub fn set_digi_enabled(&mut self, on: bool) {
        self.digi_enabled = on;
    }

    /// `SD_PlaySound`: play a sound effect, honoring the priority of whatever is
    /// already sounding. Digitized playback is preferred when the sound has a
    /// digitized version.
    pub fn play_sound(&mut self, sound: u8) {
        let s = sound as usize;
        if s >= NUM_SOUNDS {
            return;
        }
        let priority = self.assets.sfx[s].as_ref().map_or(0, |a| a.priority as i32);
        let digi_num = self.assets.digi_map[s];

        if self.digi_enabled
            && digi_num >= 0
            && (digi_num as usize) < self.assets.digi.len()
            && !self.assets.digi[digi_num as usize].is_empty()
        {
            if self.digi.is_some() && priority < self.digi_priority {
                return;
            }
            self.digi = Some(digi_num as usize);
            self.digi_pos = 0.0;
            self.digi_priority = priority;
            return;
        }

        // AdLib effect.
        if self.assets.sfx[s].is_none() {
            return;
        }
        if self.sfx.is_some() && priority < self.sfx_priority {
            return;
        }
        self.start_adlib(s, priority);
    }

    fn start_adlib(&mut self, s: usize, priority: i32) {
        let (inst, block) = {
            let a = self.assets.sfx[s].as_ref().unwrap();
            (a.instrument, a.block)
        };
        self.set_fx_instrument(&inst);
        self.sfx = Some(s);
        self.sfx_pos = 0;
        self.sfx_block = ((block & 7) << 2) | 0x20;
        self.sfx_accum = 0.0;
        self.sfx_priority = priority;
    }

    /// SDL_AlSetFXInst: load a 16-byte instrument onto channel 0's operators.
    fn set_fx_instrument(&mut self, inst: &[u8; 16]) {
        let w = |o: &mut Opl3Chip, base: u16, cell: u16, v: u8| o.write_register(base + cell, v);
        w(&mut self.opl, AL_CHAR, FX_MOD, inst[0]);
        w(&mut self.opl, AL_SCALE, FX_MOD, inst[2]);
        w(&mut self.opl, AL_ATTACK, FX_MOD, inst[4]);
        w(&mut self.opl, AL_SUS, FX_MOD, inst[6]);
        w(&mut self.opl, AL_WAVE, FX_MOD, inst[8]);
        w(&mut self.opl, AL_CHAR, FX_CAR, inst[1]);
        w(&mut self.opl, AL_SCALE, FX_CAR, inst[3]);
        w(&mut self.opl, AL_ATTACK, FX_CAR, inst[5]);
        w(&mut self.opl, AL_SUS, FX_CAR, inst[7]);
        w(&mut self.opl, AL_WAVE, FX_CAR, inst[9]);
        // The original writes 0 here (see the MUSE-compat note in ID_SD.C).
        self.opl.write_register(AL_FEED_CON, 0);
    }

    fn all_notes_off(&mut self) {
        for ch in 0..9u16 {
            self.opl.write_register(AL_FREQ_H + ch, 0);
        }
    }

    /// One 700 Hz music service tick (SDL_ALService).
    fn music_service(&mut self) {
        let Some(track) = self.music_track else { return };
        let data = &self.assets.music[track];
        if data.len() < 4 {
            return;
        }
        self.music_wait -= 1.0;
        // Process every event whose delay has elapsed; guard against a song of
        // all-zero delays looping forever within one tick.
        let mut guard = 0;
        while self.music_wait <= 0.0 {
            if self.music_ptr + 4 > data.len() {
                // Loop the song (SDL_ALService resets to the start).
                self.music_ptr = 0;
                self.music_wait = 0.0;
            }
            let reg = data[self.music_ptr];
            let val = data[self.music_ptr + 1];
            let delay = u16::from_le_bytes([data[self.music_ptr + 2], data[self.music_ptr + 3]]);
            self.music_ptr += 4;
            self.opl.write_register(reg as u16, val);
            self.music_wait += delay as f64;
            guard += 1;
            if guard > data.len() {
                break;
            }
        }
    }

    /// One 140 Hz effect service tick (SDL_ALSoundService).
    fn sfx_service(&mut self) {
        let Some(s) = self.sfx else { return };
        let data = &self.assets.sfx[s].as_ref().unwrap().data;
        if self.sfx_pos >= data.len() {
            self.opl.write_register(AL_FREQ_H, 0);
            self.sfx = None;
            self.sfx_priority = 0;
            return;
        }
        let b = data[self.sfx_pos];
        self.sfx_pos += 1;
        if b == 0 {
            self.opl.write_register(AL_FREQ_H, 0);
        } else {
            self.opl.write_register(AL_FREQ_L, b);
            self.opl.write_register(AL_FREQ_H, self.sfx_block);
        }
        if self.sfx_pos >= data.len() {
            self.opl.write_register(AL_FREQ_H, 0);
            self.sfx = None;
            self.sfx_priority = 0;
        }
    }

    /// Render `out.len()` mono samples in [-1, 1] at [`sample_rate`].
    pub fn render(&mut self, out: &mut [f32]) {
        let music_step = MUSIC_RATE / self.sample_rate as f64;
        let sfx_step = SFX_RATE / self.sample_rate as f64;
        let digi_step = DIGI_RATE as f64 / self.sample_rate as f64;
        let mut stereo = [0i16; 2];

        for slot in out.iter_mut() {
            // Music timing.
            if self.music_enabled && self.music_track.is_some() {
                self.music_accum += music_step;
                while self.music_accum >= 1.0 {
                    self.music_accum -= 1.0;
                    self.music_service();
                }
            }
            // Effect timing.
            if self.sfx.is_some() {
                self.sfx_accum += sfx_step;
                while self.sfx_accum >= 1.0 {
                    self.sfx_accum -= 1.0;
                    self.sfx_service();
                }
            }

            // OPL sample (average the stereo pair to mono).
            let _ = self.opl.generate_resampled(&mut stereo);
            let opl = (stereo[0] as f32 + stereo[1] as f32) * 0.5 / 32768.0;

            // Digitized sample.
            let mut digi = 0.0f32;
            if let Some(d) = self.digi {
                let data = &self.assets.digi[d];
                let idx = self.digi_pos as usize;
                if idx < data.len() {
                    digi = (data[idx] as f32 - 128.0) / 128.0;
                    self.digi_pos += digi_step;
                } else {
                    self.digi = None;
                    self.digi_priority = 0;
                }
            }

            *slot = (opl * 2.5 + digi * 0.9).clamp(-1.0, 1.0);
        }
    }
}

// =============================================================================
// OFFLINE RENDER + WAV (deterministic; used by tests and verification)
// =============================================================================

const OFFLINE_BLOCK: usize = 512;

/// Render `seconds` of audio off-device into a mono `Vec<f32>`. Byte-identical
/// every run, so a music render can be checksummed.
pub fn render_offline(engine: &mut Engine, seconds: f32) -> Vec<f32> {
    let total = (seconds.max(0.0) * engine.sample_rate() as f32).round() as usize;
    let mut out = vec![0.0f32; total];
    for block in out.chunks_mut(OFFLINE_BLOCK) {
        engine.render(block);
    }
    out
}

#[inline]
fn quantize(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// FNV-1a fingerprint over the 16-bit quantized little-endian samples (the bytes
/// a WAV would store), stable across harmless float perturbations.
pub fn checksum(samples: &[f32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &s in samples {
        for byte in quantize(s).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash
}

/// Encode mono `samples` as a 16-bit PCM WAV.
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&quantize(s).to_le_bytes());
    }
    buf
}

/// Write mono `samples` to `path` as a 16-bit PCM WAV.
pub fn write_wav(path: impl AsRef<std::path::Path>, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    std::fs::write(path, encode_wav(samples, sample_rate))
}

// =============================================================================
// LIVE BACKEND (cpal)
// =============================================================================

/// A command sent from the game thread to the audio callback.
enum Cmd {
    Play(u8),
    Music(Option<usize>),
    MusicEnabled(bool),
    DigiEnabled(bool),
}

/// Why the audio device could not be opened. Never fatal — the game runs silent.
#[derive(Debug)]
pub enum AudioError {
    NoDevice,
    NoConfig(String),
    Stream(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no default audio output device"),
            AudioError::NoConfig(m) => write!(f, "no usable output config: {m}"),
            AudioError::Stream(m) => write!(f, "could not open audio stream: {m}"),
        }
    }
}
impl std::error::Error for AudioError {}

/// The live audio output. Holds the cpal stream and a lock-free command channel
/// to the callback, which owns the [`Engine`]. Dropping it stops the sound.
pub struct Backend {
    _stream: cpal::Stream,
    tx: Sender<Cmd>,
    music_enabled: bool,
}

impl Backend {
    /// Open the default output device and start rendering `assets`. The
    /// [`Engine`] is built at the device's sample rate and moved onto the audio
    /// thread. Returns an error (never panics) when there is no usable device.
    pub fn start(assets: SoundAssets) -> Result<Backend, AudioError> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::NoConfig(e.to_string()))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        let engine = Engine::new(sample_rate, assets);
        let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = std::sync::mpsc::channel();

        // One arm runs (match arms are exclusive), so `engine`/`rx` move once.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, channels, engine, rx, |m| m)?
            }
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, engine, rx, |m| {
                (m.clamp(-1.0, 1.0) * 32767.0) as i16
            })?,
            cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, engine, rx, |m| {
                ((m.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0) as u16
            })?,
            other => return Err(AudioError::NoConfig(format!("unsupported sample format {other:?}"))),
        };

        stream.play().map_err(|e| AudioError::Stream(e.to_string()))?;
        Ok(Backend { _stream: stream, tx, music_enabled: true })
    }

    // (stream construction helper is the free function `build_stream` below)

    /// Queue a sound effect for playback.
    pub fn play(&self, sound: u8) {
        let _ = self.tx.send(Cmd::Play(sound));
    }

    /// Play/stop a music track.
    pub fn set_music(&self, track: Option<usize>) {
        let _ = self.tx.send(Cmd::Music(track));
    }

    /// Toggle music on/off (returns the new state).
    pub fn toggle_music(&mut self) -> bool {
        self.music_enabled = !self.music_enabled;
        let _ = self.tx.send(Cmd::MusicEnabled(self.music_enabled));
        self.music_enabled
    }

    /// Set music on/off explicitly (kept in step with the Sound menu state).
    pub fn set_music_enabled(&mut self, on: bool) {
        self.music_enabled = on;
        let _ = self.tx.send(Cmd::MusicEnabled(on));
    }

    /// Enable/disable digitized sound effects (the "AdLib only" effects mode).
    pub fn set_digi_enabled(&self, on: bool) {
        let _ = self.tx.send(Cmd::DigiEnabled(on));
    }

    pub fn music_enabled(&self) -> bool {
        self.music_enabled
    }
}

/// Build the cpal output stream for a device sample type `T`. The callback owns
/// the [`Engine`] and the command receiver: it drains queued commands, renders
/// one mono block into a reused scratch buffer, then centre-pans it across the
/// device's channels.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut engine: Engine,
    rx: Receiver<Cmd>,
    conv: impl Fn(f32) -> T + Send + 'static,
) -> Result<cpal::Stream, AudioError>
where
    T: cpal::SizedSample + Send + 'static,
{
    use cpal::traits::DeviceTrait;
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Play(s) => engine.play_sound(s),
                        Cmd::Music(t) => engine.play_music(t),
                        Cmd::MusicEnabled(on) => engine.set_music_enabled(on),
                        Cmd::DigiEnabled(on) => engine.set_digi_enabled(on),
                    }
                }
                let frames = data.len() / channels.max(1);
                if scratch.len() < frames {
                    scratch.resize(frames, 0.0);
                }
                let mono = &mut scratch[..frames];
                engine.render(mono);
                for (frame, &m) in data.chunks_mut(channels.max(1)).zip(mono.iter()) {
                    let v = conv(m);
                    for ch in frame.iter_mut() {
                        *ch = v;
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| AudioError::Stream(e.to_string()))
}

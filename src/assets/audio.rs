//! AUDIOHED.WL6 + AUDIOT.WL6 loader — the AdLib sound effects and the IMF
//! music. (The *digitized* sound effects are not here; they live in VSWAP — see
//! [`super::vswap`].)
//!
//! AUDIOHED.WL6 is a flat array of `u32` file offsets into AUDIOT.WL6, one per
//! chunk plus a terminating offset, so chunk `n` is the bytes
//! `[offset[n]..offset[n+1])`. The chunks are laid out in four banks
//! (AUDIOWL6.H): PC-speaker effects `[0..87)`, AdLib effects `[87..174)`,
//! (unused digitized slots) `[174..261)`, then the IMF songs `[261..288)`.
//!
//! An AdLib effect chunk is an `AdLibSound` (ID_SD.H): `u32 length`,
//! `u16 priority`, a 16-byte `Instrument`, one `block` byte, then `length`
//! bytes of sample data — one OPL frequency register value per 1/140 s tick.
//! A music chunk is a `u16 length` followed by `length` bytes of IMF: a stream
//! of `(u8 reg, u8 data, u16 delay)` events clocked at 700 Hz.

use std::path::Path;

use crate::sound as snd;

/// NUMSOUNDS (AUDIOWL6.H): sound effects per bank.
pub const NUM_SOUNDS: usize = 87;
/// STARTADLIBSOUNDS: first AdLib-effect chunk.
pub const START_ADLIB_SOUNDS: usize = 87;
/// STARTMUSIC: first music chunk.
pub const START_MUSIC: usize = 261;
/// NUMMUSIC (AUDIOWL6.H musicnames): IMF songs.
pub const NUM_MUSIC: usize = 27;

/// One AdLib sound effect (ID_SD.H `AdLibSound`). `data` is the per-tick OPL
/// channel-0 frequency stream played at 140 Hz; `instrument` is the raw 16-byte
/// operator/channel register block loaded once when the effect starts.
#[derive(Clone)]
pub struct AdLibSound {
    pub priority: u16,
    pub instrument: [u8; 16],
    pub block: u8,
    pub data: Vec<u8>,
}

/// The parsed AUDIOT contents plus the sound-enum -> digitized-sound map.
pub struct AudioData {
    /// AdLib effects, indexed by sound enum value (0..NUM_SOUNDS). Missing/short
    /// chunks decode to `None`.
    pub sfx: Vec<Option<AdLibSound>>,
    /// IMF songs, indexed by music enum value (0..NUM_MUSIC).
    pub music: Vec<Vec<u8>>,
    /// DigiMap (WL_MAIN.C `wolfdigimap`): sound enum -> digitized sound number,
    /// or -1 when the sound has no digitized version.
    pub digi_map: [i32; NUM_SOUNDS],
}

impl AudioData {
    /// Load the WL6 audio archive (the default variant).
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        Self::load_ext(dir, "WL6")
    }

    /// Load AUDIOHED/AUDIOT for a given extension (`WL6` / `SOD` / `SD2` / `SD3`).
    ///
    /// Spear of Destiny (and its mission packs) reorders its sound enum and uses
    /// different bank offsets (AUDIOSOD.H: STARTADLIBSOUNDS 81, STARTMUSIC 243,
    /// 24 songs). The game engine always emits WL6 sound-enum ids, so the Spear
    /// path indexes its AdLib effects by a WL6->SOD name remap
    /// ([`sod_sfx_remap`]) — a WL6 id lands on the SOD chunk of the same name,
    /// or `None` when SOD has no such effect. Songs are indexed directly by the
    /// SOD music enum. Digitized playback is disabled for Spear (adlib-only) —
    /// the SOD DigiMap is not modeled. M2/M3 ship the same AUDIOT as M1.
    pub fn load_ext(dir: &Path, ext: &str) -> std::io::Result<Self> {
        let head = std::fs::read(dir.join(format!("AUDIOHED.{ext}")))?;
        let audiot = std::fs::read(dir.join(format!("AUDIOT.{ext}")))?;

        // AUDIOHED is a plain u32 offset table (chunks + 1 terminator).
        let offsets: Vec<usize> = head
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .collect();
        let num_chunks = offsets.len().saturating_sub(1);
        let chunk = |i: usize| -> &[u8] {
            if i + 1 >= offsets.len() {
                return &[];
            }
            let (a, b) = (offsets[i], offsets[i + 1]);
            if a >= b || b > audiot.len() {
                return &[];
            }
            &audiot[a..b]
        };

        let sod = crate::variant::is_spear_ext(ext);
        // Bank offsets differ per variant.
        let (start_adlib, start_music, num_music) = if sod {
            (81usize, 243usize, 24usize)
        } else {
            (START_ADLIB_SOUNDS, START_MUSIC, NUM_MUSIC)
        };

        // The sfx vec is always indexed by the WL6 sound-enum id the engine emits.
        let mut sfx = Vec::with_capacity(NUM_SOUNDS);
        for wl6 in 0..NUM_SOUNDS {
            let bank_id = if sod { sod_sfx_remap(wl6) } else { Some(wl6) };
            sfx.push(bank_id.and_then(|id| parse_adlib(chunk(start_adlib + id))));
        }

        let mut music = Vec::with_capacity(num_music);
        for m in 0..num_music {
            let idx = start_music + m;
            music.push(if idx < num_chunks {
                parse_imf(chunk(idx))
            } else {
                Vec::new()
            });
        }

        let digi_map = if sod {
            [-1i32; NUM_SOUNDS]
        } else {
            build_digi_map()
        };
        Ok(Self {
            sfx,
            music,
            digi_map,
        })
    }
}

/// Map a WL6 sound-enum id to the SOD AdLib-effect index of the same name
/// (AUDIOSOD.H `soundnames`), or `None` when Spear has no matching effect. Built
/// by matching enum names; WL6-only speech (Mutti, Die, Eva, …) has no SOD
/// counterpart and drops silent.
fn sod_sfx_remap(wl6: usize) -> Option<usize> {
    use crate::sound as s;
    Some(match wl6 {
        s::HITWALLSND => 0,
        s::SELECTITEMSND => 2,
        s::MOVEGUN2SND => 4,
        s::MOVEGUN1SND => 5,
        s::NOWAYSND => 6,
        s::NAZIHITPLAYERSND => 7,
        s::PLAYERDEATHSND => 9,
        s::DOGDEATHSND => 10,
        s::ATKGATLINGSND => 11,
        s::GETKEYSND => 12,
        s::NOITEMSND => 13,
        s::WALK1SND => 14,
        s::WALK2SND => 15,
        s::TAKEDAMAGESND => 16,
        s::GAMEOVERSND => 17,
        s::OPENDOORSND => 18,
        s::CLOSEDOORSND => 19,
        s::DONOTHINGSND => 20,
        s::HALTSND => 21,
        s::DEATHSCREAM2SND => 22,
        s::ATKKNIFESND => 23,
        s::ATKPISTOLSND => 24,
        s::DEATHSCREAM3SND => 25,
        s::ATKMACHINEGUNSND => 26,
        s::HITENEMYSND => 27,
        s::SHOOTDOORSND => 28,
        s::DEATHSCREAM1SND => 29,
        s::GETMACHINESND => 30,
        s::GETAMMOSND => 31,
        s::SHOOTSND => 32,
        s::HEALTH1SND => 33,
        s::HEALTH2SND => 34,
        s::BONUS1SND => 35,
        s::BONUS2SND => 36,
        s::BONUS3SND => 37,
        s::GETGATLINGSND => 38,
        s::ESCPRESSEDSND => 39,
        s::LEVELDONESND => 40,
        s::DOGBARKSND => 41,
        s::ENDBONUS1SND => 42,
        s::ENDBONUS2SND => 43,
        s::BONUS1UPSND => 44,
        s::BONUS4SND => 45,
        s::PUSHWALLSND => 46,
        s::NOBONUSSND => 47,
        s::PERCENT100SND => 48,
        s::BOSSACTIVESND => 49,
        s::SCHUTZADSND => 51,
        s::AHHHGSND => 52,
        s::LEBENSND => 56,
        s::NAZIFIRESND => 58,
        s::BOSSFIRESND => 59,
        s::SSFIRESND => 60,
        s::SLURPIESND => 61,
        s::SPIONSND => 66,
        s::NEINSOVASSND => 67,
        s::DOGATTACKSND => 68,
        s::DEATHSCREAM4SND => 50,
        s::DEATHSCREAM5SND => 53,
        s::DEATHSCREAM6SND => 57,
        s::DEATHSCREAM7SND => 54,
        s::DEATHSCREAM8SND => 55,
        s::DEATHSCREAM9SND => 63,
        s::MISSILEFIRESND => 8,
        s::MISSILEHITSND => 1,
        _ => return None,
    })
}

/// Parse an `AdLibSound`: 4-byte length, 2-byte priority, 16-byte instrument,
/// 1-byte block, then `length` bytes of sample data.
fn parse_adlib(c: &[u8]) -> Option<AdLibSound> {
    if c.len() < 23 {
        return None;
    }
    let length = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize;
    let priority = u16::from_le_bytes([c[4], c[5]]);
    let mut instrument = [0u8; 16];
    instrument.copy_from_slice(&c[6..22]);
    let block = c[22];
    let data = c[23..(23 + length).min(c.len())].to_vec();
    if data.is_empty() {
        return None;
    }
    Some(AdLibSound {
        priority,
        instrument,
        block,
        data,
    })
}

/// Parse a music chunk: a `u16` byte length, then that many bytes of IMF data.
fn parse_imf(c: &[u8]) -> Vec<u8> {
    if c.len() < 2 {
        return Vec::new();
    }
    let len = u16::from_le_bytes([c[0], c[1]]) as usize;
    // A length of 0 means "the whole chunk" (some tools store it that way).
    let len = if len == 0 { c.len() - 2 } else { len };
    let end = (2 + len).min(c.len());
    c[2..end].to_vec()
}

/// WL_MAIN.C `wolfdigimap` (WL6, all episodes): sound enum -> digitized number.
fn build_digi_map() -> [i32; NUM_SOUNDS] {
    let mut m = [-1i32; NUM_SOUNDS];
    let pairs: &[(usize, i32)] = &[
        (snd::HALTSND, 0),
        (snd::DOGBARKSND, 1),
        (snd::CLOSEDOORSND, 2),
        (snd::OPENDOORSND, 3),
        (snd::ATKMACHINEGUNSND, 4),
        (snd::ATKPISTOLSND, 5),
        (snd::ATKGATLINGSND, 6),
        (snd::SCHUTZADSND, 7),
        (snd::GUTENTAGSND, 8),
        (snd::MUTTISND, 9),
        (snd::BOSSFIRESND, 10),
        (snd::SSFIRESND, 11),
        (snd::DEATHSCREAM1SND, 12),
        (snd::DEATHSCREAM2SND, 13),
        (snd::DEATHSCREAM3SND, 13),
        (snd::TAKEDAMAGESND, 14),
        (snd::PUSHWALLSND, 15),
        (snd::LEBENSND, 20),
        (snd::NAZIFIRESND, 21),
        (snd::SLURPIESND, 22),
        (snd::YEAHSND, 32),
        (snd::DOGDEATHSND, 16),
        (snd::AHHHGSND, 17),
        (snd::DIESND, 18),
        (snd::EVASND, 19),
        (snd::TOT_HUNDSND, 23),
        (snd::MEINGOTTSND, 24),
        (snd::SCHABBSHASND, 25),
        (snd::HITLERHASND, 26),
        (snd::SPIONSND, 27),
        (snd::NEINSOVASSND, 28),
        (snd::DOGATTACKSND, 29),
        (snd::LEVELDONESND, 30),
        (snd::MECHSTEPSND, 31),
        (snd::SCHEISTSND, 33),
        (snd::DEATHSCREAM4SND, 34),
        (snd::DEATHSCREAM5SND, 35),
        (snd::DONNERSND, 36),
        (snd::EINESND, 37),
        (snd::ERLAUBENSND, 38),
        (snd::DEATHSCREAM6SND, 39),
        (snd::DEATHSCREAM7SND, 40),
        (snd::DEATHSCREAM8SND, 41),
        (snd::DEATHSCREAM9SND, 42),
        (snd::KEINSND, 43),
        (snd::MEINSND, 44),
        (snd::ROSESND, 45),
    ];
    for &(sound, digi) in pairs {
        m[sound] = digi;
    }
    m
}

//! Save-game serialization: a versioned little-endian binary format, hand-rolled
//! like the asset loaders (no serde). A save captures the full mid-level state —
//! player, stats, doors, statics, every actor and projectile, and BOTH random
//! streams — so loading resumes the simulation bit-identically, even mid-fight.
//!
//! ## Layout (version 2)
//!
//! ```text
//! magic      "WOLF3DSV"        8 bytes
//! version    u16               format version (2)
//! name       u16 len + utf8    slot label shown in the menu
//! level_idx  u32               overall level index (episode*10 + floor)
//! difficulty u8                0 baby .. 3 hard
//! -- body (written by Game::write_save) --
//! player     f32 x, y, angle
//! stats      i32 health, ammo, score, lives; u8 keys; u32 weapon
//! victory    u8 bool
//! swing      attack anim (bool + frame/count/frame)
//! levelstats kills/secrets/treasure counters + elapsed time     (LevelStats::save)
//! world      doors, statics, blocking grid, push-wall, plane-0 diffs (World::save)
//! actors     RNG indices, actor list, projectiles              (Actors::save)
//! ```
//!
//! The [`Writer`]/[`Reader`] pair are the only IO primitives; each subsystem
//! (`game`, `raycast`, `actors`) writes and reads its own slice through them, so
//! private fields never have to leak across module boundaries.

use std::path::PathBuf;

/// File magic: identifies a Wolf3D save.
pub const MAGIC: &[u8; 8] = b"WOLF3DSV";
/// Current on-disk format version. Bumped to 3 to tag the game variant (WL6 /
/// SOD) so a cross-variant load is refused cleanly; version-1/2 saves are
/// refused by [`read_header`].
pub const VERSION: u16 = 3;

/// Variant tag in the save header: 0 = WL6, 1 = Spear of Destiny.
pub const VAR_WL6: u8 = 0;
pub const VAR_SOD: u8 = 1;
/// Save slots exposed by the Load/Save menus.
pub const NUM_SLOTS: usize = 10;

/// Anything that can go wrong reading a save. Writing never fails (it only
/// appends to a `Vec`); the file write itself is a plain `io::Error`.
#[derive(Debug)]
pub enum SaveError {
    /// The reader ran off the end of the buffer.
    Truncated,
    /// The magic bytes did not match.
    BadMagic,
    /// The version is newer/unknown.
    BadVersion(u16),
    /// A tagged enum held an out-of-range discriminant.
    BadEnum(&'static str),
    /// The slot name was not valid UTF-8.
    Utf8,
    /// The save was made under a different game variant (WL6 vs. SOD).
    CrossVariant,
    /// Underlying file IO failed.
    Io(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Truncated => write!(f, "save data ended unexpectedly"),
            SaveError::BadMagic => write!(f, "not a Wolf3D save (bad magic)"),
            SaveError::BadVersion(v) => write!(f, "unsupported save version {v}"),
            SaveError::BadEnum(what) => write!(f, "invalid {what} in save"),
            SaveError::Utf8 => write!(f, "slot name is not valid UTF-8"),
            SaveError::CrossVariant => write!(f, "save is from a different game (WL6/SOD)"),
            SaveError::Io(e) => write!(f, "save IO error: {e}"),
        }
    }
}
impl std::error::Error for SaveError {}
impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        SaveError::Io(e)
    }
}

/// Append-only little-endian byte sink.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }
    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn put_bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    /// A length-prefixed (u16) UTF-8 string.
    pub fn put_str(&mut self, s: &str) {
        let b = s.as_bytes();
        self.put_u16(b.len() as u16);
        self.buf.extend_from_slice(b);
    }
}

/// Sequential little-endian byte source over a borrowed buffer.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], SaveError> {
        let end = self.pos.checked_add(n).ok_or(SaveError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(SaveError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// Borrow the next `n` bytes (advancing the cursor), erroring if truncated.
    pub fn get_bytes(&mut self, n: usize) -> Result<&'a [u8], SaveError> {
        self.take(n)
    }

    pub fn get_u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }
    pub fn get_u16(&mut self) -> Result<u16, SaveError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn get_u32(&mut self) -> Result<u32, SaveError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn get_i32(&mut self) -> Result<i32, SaveError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn get_f32(&mut self) -> Result<f32, SaveError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn get_bool(&mut self) -> Result<bool, SaveError> {
        Ok(self.get_u8()? != 0)
    }
    pub fn get_str(&mut self) -> Result<String, SaveError> {
        let len = self.get_u16()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|_| SaveError::Utf8)
    }
}

/// The fixed header at the front of every save: enough to list a slot in the
/// menu without decoding the whole body.
pub struct SlotHeader {
    pub name: String,
    pub level_idx: usize,
    pub difficulty: u8,
    /// Game variant the save was made under ([`VAR_WL6`] / [`VAR_SOD`]).
    pub variant: u8,
}

/// Write the magic + version + [`SlotHeader`] fields.
pub fn write_header(w: &mut Writer, name: &str, level_idx: usize, difficulty: u8, variant: u8) {
    w.buf.extend_from_slice(MAGIC);
    w.put_u16(VERSION);
    w.put_u8(variant);
    w.put_str(name);
    w.put_u32(level_idx as u32);
    w.put_u8(difficulty);
}

/// Read and validate the header, leaving the reader positioned at the body.
pub fn read_header(r: &mut Reader) -> Result<SlotHeader, SaveError> {
    let magic = r.take(8)?;
    if magic != MAGIC {
        return Err(SaveError::BadMagic);
    }
    let version = r.get_u16()?;
    if version != VERSION {
        return Err(SaveError::BadVersion(version));
    }
    let variant = r.get_u8()?;
    let name = r.get_str()?;
    let level_idx = r.get_u32()? as usize;
    let difficulty = r.get_u8()?;
    Ok(SlotHeader { name, level_idx, difficulty, variant })
}

/// The directory holding the save slots (`saves/` under the crate root). The
/// `WOLF3D_SAVE_DIR` env var overrides it, so tests can isolate their files.
pub fn saves_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WOLF3D_SAVE_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("saves")
}

/// Path of save slot `slot` (0-based).
pub fn slot_path(slot: usize) -> PathBuf {
    saves_dir().join(format!("savegame_{slot}.bin"))
}

/// Read just the header of slot `slot`, or `None` if the slot is empty or the
/// file is unreadable/corrupt (a corrupt slot reads as empty in the menu).
pub fn read_slot_header(slot: usize) -> Option<SlotHeader> {
    let data = std::fs::read(slot_path(slot)).ok()?;
    let mut r = Reader::new(&data);
    read_header(&mut r).ok()
}

//! Persistent player options, stored in a tiny versioned binary file next to the
//! save slots (`config.bin` under [`crate::savegame::saves_dir`]). It mirrors the
//! bits the original kept in CONFIG.WL6: the 3D view size (WL_MENU.C
//! CP_ChangeView), the sound-effects and music toggles (CP_Sound), and the mouse
//! sensitivity (CP_Control / MouseSensitivity).
//!
//! Like [`crate::savegame`] this is hand-rolled little-endian, reusing the same
//! [`Reader`]/[`Writer`] primitives. The file is optional: a missing or corrupt
//! config reads as [`Config::default`] (full-size view, full sound), so the game
//! — and every headless test, which never writes one — boots with the classic
//! defaults and the demo/snapshot render path stays pixel-identical.

use std::path::PathBuf;

use crate::savegame::{Reader, SaveError, Writer, saves_dir};

/// File magic: identifies a Wolf3D config.
pub const MAGIC: &[u8; 8] = b"WOLF3DCF";
/// Current on-disk format version.
pub const VERSION: u16 = 1;

/// The smallest and largest 3D-view widths, in pixels (multiples of 16). The
/// original's CP_ChangeView sized `viewwidth` in 16px steps; the full 320-wide
/// view is the default and the floor is 64.
pub const MIN_VIEW: usize = 64;
pub const MAX_VIEW: usize = 320;
pub const VIEW_STEP: usize = 16;

/// The mouse-sensitivity slider range (CP_Control): 0..=20.
pub const MAX_MOUSE_SENS: usize = 20;
pub const DEFAULT_MOUSE_SENS: usize = 10;

/// The persisted options.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// 3D view width in pixels (64..=320, multiple of 16); the view height is
    /// half this, centered in the play area.
    pub view_size: usize,
    /// Sound-effects mode, encoded as [`crate::game::SfxMode`]'s discriminant
    /// (0 Digital, 1 Synthesized, 2 Off).
    pub sfx_mode: u8,
    /// Whether music plays.
    pub music_on: bool,
    /// Mouse-look sensitivity slider, 0..=20.
    pub mouse_sensitivity: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            view_size: MAX_VIEW,
            sfx_mode: 0,
            music_on: true,
            mouse_sensitivity: DEFAULT_MOUSE_SENS,
        }
    }
}

impl Config {
    /// Clamp every field into range (defensive against a hand-edited or
    /// forward-version file).
    fn sanitize(mut self) -> Self {
        self.view_size = self.view_size.clamp(MIN_VIEW, MAX_VIEW) / VIEW_STEP * VIEW_STEP;
        self.view_size = self.view_size.max(MIN_VIEW);
        self.sfx_mode = self.sfx_mode.min(2);
        self.mouse_sensitivity = self.mouse_sensitivity.min(MAX_MOUSE_SENS);
        self
    }

    /// Serialize to a fresh byte buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.buf.extend_from_slice(MAGIC);
        w.put_u16(VERSION);
        w.put_u32(self.view_size as u32);
        w.put_u8(self.sfx_mode);
        w.put_bool(self.music_on);
        w.put_u32(self.mouse_sensitivity as u32);
        w.buf
    }

    /// Parse from bytes, validating the header. Fields are range-clamped.
    pub fn from_bytes(data: &[u8]) -> Result<Self, SaveError> {
        let mut r = Reader::new(data);
        if r.get_bytes(8)? != MAGIC {
            return Err(SaveError::BadMagic);
        }
        let version = r.get_u16()?;
        if version != VERSION {
            return Err(SaveError::BadVersion(version));
        }
        let view_size = r.get_u32()? as usize;
        let sfx_mode = r.get_u8()?;
        let music_on = r.get_bool()?;
        let mouse_sensitivity = r.get_u32()? as usize;
        Ok(Config {
            view_size,
            sfx_mode,
            music_on,
            mouse_sensitivity,
        }
        .sanitize())
    }
}

/// Path of the config file (`config.bin` beside the save slots).
pub fn config_path() -> PathBuf {
    saves_dir().join("config.bin")
}

/// Load the config, or `None` if it is absent or unreadable/corrupt.
pub fn load() -> Option<Config> {
    let data = std::fs::read(config_path()).ok()?;
    Config::from_bytes(&data).ok()
}

/// Write the config to disk (creating the saves directory if needed).
pub fn save(cfg: &Config) -> std::io::Result<()> {
    std::fs::create_dir_all(saves_dir())?;
    std::fs::write(config_path(), cfg.to_bytes())
}

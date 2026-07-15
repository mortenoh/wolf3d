//! Game-variant selection: Wolfenstein 3D (WL6) vs. Spear of Destiny (SOD).
//!
//! The two share an engine but diverge in their data-file extension, their
//! VGAGRAPH chunk numbering (GFXV_WL6.H vs. GFXV_SOD.H), their sprite numbering
//! (SOD adds four static sprites, shifting every actor sprite by +4, and swaps
//! the boss cast), their level layout (SOD is one 21-level campaign with no
//! episode select), and their per-floor songs and par times.
//!
//! A [`Variant`] is built once at startup from [`Variant::detect`] and threaded
//! into the asset loaders (file extension), the constant tables here, and the
//! HUD / menu / intermission (chunk numbers via [`Gfx`]).

use std::path::Path;

use crate::assets::data_dir;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameId {
    Wl6,
    Sod,
}

/// The named VGAGRAPH picture/text chunk numbers that differ between the two
/// graphics headers (GFXV_WL6.H / GFXV_SOD.H). Only the chunks the front-end
/// actually draws are modeled.
#[derive(Clone, Copy)]
pub struct Gfx {
    /// Title screen. WL6 is a single full-screen pic; SOD stacks two halves
    /// (TITLE1PIC top, TITLE2PIC bottom), so `title2` is `Some` there.
    pub title1: usize,
    pub title2: Option<usize>,
    /// Custom VGA palette chunk for the title screen (SOD's TITLEPALETTE); `None`
    /// means the title uses the standard game palette (WL6).
    pub title_palette: Option<usize>,
    pub credits: usize,
    pub options: usize,
    pub cursor1: usize,
    pub cursor2: usize,
    pub not_selected: usize,
    pub selected: usize,
    /// First of the four difficulty banners (baby/easy/normal/hard).
    pub baby_mode: usize,
    /// First of the six episode banners (WL6 only).
    pub episode1: Option<usize>,
    pub fx_title: usize,
    pub music_title: usize,
    pub load_game: usize,
    pub save_game: usize,
    pub statusbar: usize,
    pub n_0: usize,
    pub face1a: usize,
    pub face8a: usize,
    pub no_key: usize,
    pub gold_key: usize,
    pub silver_key: usize,
    pub l_guy: usize,
    pub l_guy2: usize,
    pub l_num0: usize,
    pub get_psyched: usize,
    pub l_bjwins: usize,
    pub high_scores: usize,
    /// First end-of-episode article text chunk.
    pub end_art1: usize,
    /// The "Read This!" help article (WL6 only; SOD has none).
    pub help_art: Option<usize>,
}

const GFX_WL6: Gfx = Gfx {
    title1: 87,
    title2: None,
    title_palette: None,
    credits: 89,
    options: 10,
    cursor1: 11,
    cursor2: 12,
    not_selected: 13,
    selected: 14,
    baby_mode: 19,
    episode1: Some(30),
    fx_title: 15,
    music_title: 17,
    load_game: 28,
    save_game: 29,
    statusbar: 86,
    n_0: 99,
    face1a: 109,
    face8a: 130,
    no_key: 95,
    gold_key: 96,
    silver_key: 97,
    l_guy: 43,
    l_guy2: 84,
    l_num0: 45,
    get_psyched: 134,
    l_bjwins: 85,
    high_scores: 90,
    end_art1: 143,
    help_art: Some(138),
};

const GFX_SOD: Gfx = Gfx {
    title1: 79,
    title2: Some(80),
    title_palette: Some(153), // TITLEPALETTE (GFXV_SOD.H)
    credits: 92,
    options: 16,
    cursor1: 5,
    cursor2: 6,
    not_selected: 7,
    selected: 8,
    baby_mode: 21,
    episode1: None,
    fx_title: 17,
    music_title: 19,
    load_game: 27,
    save_game: 28,
    statusbar: 90,
    n_0: 109,
    face1a: 119,
    face8a: 140,
    no_key: 105,
    gold_key: 106,
    silver_key: 107,
    l_guy: 36,
    l_guy2: 77,
    l_num0: 38,
    get_psyched: 149,
    l_bjwins: 78,
    high_scores: 29,
    end_art1: 168,
    help_art: None,
};

/// Per-floor song for Spear of Destiny (WL_PLAY.C `songs[]` under SPEAR), values
/// are SOD music-enum indices (AUDIOSOD.H). 21 floors, indexed by `mapon`.
const SOD_SONGS: [usize; 21] = [
    4, 0, 2, 22, 15, // floors 1-5  (Trans Grosse boss on 5)
    1, 5, 9, 10, 15, // floors 6-10 (Wilhelm boss on 10)
    8, 3, 12, 11, 13, // floors 11-15
    15, 21, 15, // floors 16-18 (Uber boss on 16, Death Knight on 18)
    18, 0,  // floors 19-20 (secret)
    17, // floor 21 (Angel of Death boss)
];

/// SOD par times in minutes (Wolf4SDL `parTimes[]` under SPEAR), indexed by
/// `mapon`. Boss and secret floors have no par (0). The final Angel floor (20)
/// never shows an intermission, so its entry is a filler 0.
const SOD_PAR_TIMES: [f32; 21] = [
    1.5, 3.5, 2.75, 3.5, 0.0, //
    4.5, 3.25, 2.75, 4.75, 0.0, //
    6.5, 4.5, 2.75, 4.5, 6.0, //
    0.0, 6.0, 0.0, //
    0.0, 0.0, //
    0.0,
];

pub struct Variant {
    pub id: GameId,
    /// Data-file extension: `"WL6"` or `"SOD"`.
    pub ext: &'static str,
    /// SOD has no episode select — New Game goes straight to difficulty.
    pub has_episodes: bool,
    /// VGAGRAPH chunk numbers for this variant.
    pub gfx: Gfx,
    /// Amount every actor/weapon/projectile VSWAP sprite number is shifted vs.
    /// WL6: SOD inserts four extra static sprites (SPR_STAT_48..51) ahead of the
    /// enemy art, so all shared-enemy sprites move up by 4.
    pub sprite_shift: u16,
}

impl Variant {
    pub fn wl6() -> Self {
        Self {
            id: GameId::Wl6,
            ext: "WL6",
            has_episodes: true,
            gfx: GFX_WL6,
            sprite_shift: 0,
        }
    }

    pub fn sod() -> Self {
        Self {
            id: GameId::Sod,
            ext: "SOD",
            has_episodes: false,
            gfx: GFX_SOD,
            sprite_shift: 4,
        }
    }

    pub fn is_sod(&self) -> bool {
        self.id == GameId::Sod
    }

    /// Whether the given variant's core data (VSWAP) is present in `data/`.
    pub fn present(ext: &str) -> bool {
        data_dir().join(format!("VSWAP.{ext}")).is_file()
    }

    /// Select the variant to run: `WOLF3D_GAME=sod` (or `wl6`) forces it;
    /// otherwise auto-detect — if only one data set is present use it, and if
    /// both are present default to WL6.
    pub fn detect() -> Self {
        match std::env::var("WOLF3D_GAME")
            .ok()
            .map(|s| s.to_ascii_lowercase())
        {
            Some(s) if s == "sod" || s == "spear" => return Self::sod(),
            Some(s) if s == "wl6" || s == "wolf3d" => return Self::wl6(),
            _ => {}
        }
        let wl6 = Self::present("WL6");
        let sod = Self::present("SOD");
        if sod && !wl6 {
            Self::sod()
        } else {
            Self::wl6()
        }
    }

    /// The music track index for a floor (`mapon`, 0-based). WL6 defers to the
    /// jukebox mapping in [`crate::sound::song_for_level`].
    pub fn song_for_level(&self, level_idx: usize) -> usize {
        match self.id {
            GameId::Wl6 => crate::sound::song_for_level(level_idx),
            GameId::Sod => SOD_SONGS.get(level_idx).copied().unwrap_or(0),
        }
    }

    /// The menu/attract music track index.
    pub fn menu_song(&self) -> usize {
        match self.id {
            GameId::Wl6 => crate::sound::MENU_SONG,
            GameId::Sod => 6, // URAHERO_MUS
        }
    }

    /// The level-completed intermission music track index.
    pub fn endlevel_song(&self) -> usize {
        match self.id {
            GameId::Wl6 => crate::sound::ENDLEVEL_MUS,
            GameId::Sod => 16, // ENDLEVEL_MUS (AUDIOSOD.H)
        }
    }

    /// The victory / high-score music track index.
    pub fn victory_song(&self) -> usize {
        match self.id {
            GameId::Wl6 => crate::sound::ULTIMATE_MUS,
            GameId::Sod => 7, // XTHEEND_MUS
        }
    }

    /// Par time in minutes for a floor (0 = no par).
    pub fn par_minutes(&self, level_idx: usize) -> f32 {
        match self.id {
            GameId::Wl6 => crate::inter::PAR_TIMES
                .get(level_idx)
                .copied()
                .unwrap_or(0.0),
            GameId::Sod => SOD_PAR_TIMES.get(level_idx).copied().unwrap_or(0.0),
        }
    }
}

/// True when a data file for the given extension exists under `dir`.
pub fn has_data(dir: &Path, ext: &str) -> bool {
    dir.join(format!("VSWAP.{ext}")).is_file()
}

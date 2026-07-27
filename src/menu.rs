//! The front-end menus (title / main menu / episode select / difficulty
//! select), rewritten from WOLFSRC/WL_MENU.C. Rendering only lives here; the
//! [`crate::game::Game`] state machine owns the current screen and selection
//! and drives navigation, so the whole flow is headless-verifiable.
//!
//! Layout is the WL_MENU.H constant set: the item window (`BKGDCOLOR`) sits on
//! the cleared screen (`BORDCOLOR`) with a bevelled outline, active text is grey
//! (`TEXTCOLOR`, brighter `HIGHLIGHT` under the gun), and disabled items use
//! `DEACTIVE`. Those five window colors are build-specific — WL6 is red, Spear
//! of Destiny blue (see [`Colors`]) — and Spear replaces the flat `BORDCOLOR`
//! clear with a full-screen backdrop picture (ClearMScreen).

use crate::assets::VgaGraph;
use crate::assets::vgagraph::Picture;
use crate::config::MAX_MOUSE_SENS;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;
use crate::game::SfxMode;
use crate::savegame::NUM_SLOTS;

// --- Palette color bytes (WL_MENU.H) ---
// The greys are shared by both builds; the window colors are not.
const TEXTCOLOR: u8 = 0x17;
const HIGHLIGHT: u8 = 0x13;

/// The five menu window colors WL_MENU.H redefines under `SPEAR`. Both sets are
/// the same ramp in a different hue: WL6 takes the reds at 0x2x, Spear the blues
/// at 0x9x (the game palette mirrors the two rows shade for shade).
pub struct Colors {
    /// The cleared screen behind everything (Spear draws `backdrop` instead).
    bord: u8,
    /// Bevel highlight: the bottom/right edge of a window or outline.
    bord2: u8,
    /// Greyed-out item text, and the bevel shadow (top/left edge).
    deactive: u8,
    /// Window fill under the item lists.
    bkgd: u8,
    /// The rule under the black header band (DrawStripes).
    stripe: u8,
}

const WL6_COLORS: Colors = Colors {
    bord: 0x29,
    bord2: 0x23,
    deactive: 0x2b,
    bkgd: 0x2d,
    stripe: 0x2c,
};

const SOD_COLORS: Colors = Colors {
    bord: 0x99,
    bord2: 0x93,
    deactive: 0x9b,
    bkgd: 0x9d,
    stripe: 0x9c,
};

impl Colors {
    /// The color set for a variant — WL_MENU.H's `#ifdef SPEAR` switch.
    pub const fn for_variant(sod: bool) -> Self {
        if sod { SOD_COLORS } else { WL6_COLORS }
    }

    /// The cleared-screen color. Public because the "Get Psyched" load screen
    /// (inter.rs) paints itself in the menu's border color too.
    pub const fn bord(&self) -> u8 {
        self.bord
    }
}

// --- Main-menu layout (WL_MENU.H / WL_MENU.C) ---
const MENU_X: i32 = 76;
const MENU_Y: i32 = 55;
const MENU_W: i32 = 178;
const MENU_H: i32 = 13 * 10 + 6;
const MENU_INDENT: i32 = 24;
/// Per-item vertical step and gun-cursor Y bias (DrawGun: basey = y - 2).
const ITEM_STEP: i32 = 13;

// --- Episode-select layout (NE_X / NE_Y from WL_MENU.H) ---
const NE_X: i32 = 10;
const NE_Y: i32 = 23;

// --- Sound-menu layout (SM_* from WL_MENU.H) ---
const SM_X: i32 = 48;
const SM_W: i32 = 250;
const SM_Y1: i32 = 20; // effects group top
const SM_Y2: i32 = SM_Y1 + 5 * ITEM_STEP; // music group top (85)

// --- Load/Save layout (LSM_* from WL_MENU.H) ---
const LSM_X: i32 = 85;
const LSM_Y: i32 = 55;
const LSM_W: i32 = 175;
const LSM_H: i32 = 10 * ITEM_STEP + 10;

/// The ten main-menu entries in original order. Only New Game and Quit are
/// wired up in this milestone; the rest render greyed-out (`active == false`),
/// exactly as the original draws not-yet-implemented options.
pub struct MenuItem {
    pub label: &'static str,
    pub active: bool,
}
pub const MAIN_ITEMS: [MenuItem; 10] = [
    MenuItem {
        label: "New Game",
        active: true,
    },
    MenuItem {
        label: "Sound",
        active: true,
    },
    MenuItem {
        label: "Control",
        active: true,
    },
    MenuItem {
        label: "Load Game",
        active: true,
    },
    // Save Game is only reachable while a game is in progress; the game state
    // machine greys it via `Game::main_item_active` when not started.
    MenuItem {
        label: "Save Game",
        active: true,
    },
    MenuItem {
        label: "Change View",
        active: true,
    },
    MenuItem {
        label: "Read This!",
        active: true,
    },
    // Label swaps to "End Game" while playing (WL_MENU.C EnableEndGameMenuItem).
    MenuItem {
        label: "View Scores",
        active: true,
    },
    // Label swaps to "Back to Game" while playing (WL_MENU.C DrawMainMenu).
    MenuItem {
        label: "Back to Demo",
        active: true,
    },
    MenuItem {
        label: "Quit",
        active: true,
    },
];
/// Indices of the wired main-menu entries for the game state machine.
pub const ITEM_NEW_GAME: usize = 0;
pub const ITEM_SOUND: usize = 1;
pub const ITEM_CONTROL: usize = 2;
pub const ITEM_LOAD: usize = 3;
pub const ITEM_SAVE: usize = 4;
pub const ITEM_CHANGEVIEW: usize = 5;
pub const ITEM_READ: usize = 6;
/// View Scores when idle; End Game while a game is in progress.
pub const ITEM_VIEWSCORES: usize = 7;
/// Back to Demo when idle; Back to Game while a game is in progress.
pub const ITEM_BACKTODEMO: usize = 8;
pub const ITEM_QUIT: usize = 9;

/// Dynamic main-menu label for item `i` given whether a game is in progress
/// (WL_MENU.C DrawMainMenu / EnableEndGameMenuItem).
pub fn main_item_label(i: usize, started: bool) -> &'static str {
    match i {
        ITEM_VIEWSCORES if started => "End Game",
        ITEM_BACKTODEMO if started => "Back to Game",
        i => MAIN_ITEMS[i].label,
    }
}

pub const NUM_EPISODES: usize = 6;
pub const NUM_DIFFICULTIES: usize = 4;

/// Episode titles and subtitles (WL_MENU.C EpisodeSelect strings). The
/// C_EPISODE pics are only the little scene thumbnails; the names are text.
const EPISODE_NAMES: [(&str, &str); NUM_EPISODES] = [
    ("Episode 1", "Escape from Wolfenstein"),
    ("Episode 2", "Operation: Eisenfaust"),
    ("Episode 3", "Die, Fuhrer, Die!"),
    ("Episode 4", "A Dark Secret"),
    ("Episode 5", "Trail of the Madman"),
    ("Episode 6", "Confrontation"),
];

/// WL_MENU.C `endStrings[]`: the Quit prompt picks one at random. Both builds
/// use the full set (only the Spanish translation pinned it to the first).
pub const END_STRINGS: [&str; 9] = [
    "Dost thou wish to\nleave with such hasty\nabandon?",
    "Chickening out...\nalready?",
    "Press N for more carnage.\nPress Y to be a weenie.",
    "So, you think you can\nquit this easily, huh?",
    "Press N to save the world.\nPress Y to abandon it in\nits hour of need.",
    "Press N if you are brave.\nPress Y to cower in shame.",
    "Heroes, press N.\nWimps, press Y.",
    "You are at an intersection.\nA sign says, 'Press Y to quit.'\n>",
    "For guns and glory, press N.\nFor work and worry, press Y.",
];

/// The skill phrases beneath each BJ mugshot (WL_MENU.C).
const DIFFICULTY_NAMES: [&str; NUM_DIFFICULTIES] = [
    "Can I play, Daddy?",
    "Don't hurt me.",
    "Bring 'em on!",
    "I am Death incarnate!",
];

/// Pre-decoded menu art plus the menu font and the gun-cursor blink clock.
pub struct Menu {
    font: Font,
    title: Picture,
    /// SOD's title screen is two stacked halves (TITLE1PIC top, TITLE2PIC
    /// bottom); `None` for WL6's single title pic.
    title_bottom: Option<Picture>,
    options: Picture,
    cursor: [Picture; 2],
    episodes: Vec<Picture>,
    difficulty: Vec<Picture>,
    /// Radio-button art: [not-selected, selected] (Sound menu).
    radio: [Picture; 2],
    /// Sound-menu section titles: [effects, music].
    sound_titles: [Picture; 2],
    /// Load / Save header art: [load, save].
    ls_titles: [Picture; 2],
    /// The attract-loop credits page (CREDITSPIC).
    credits: Picture,
    /// Spear's full-screen menu backdrop (C_BACKDROPPIC); `None` on WL6, which
    /// clears to a flat `Colors::bord` instead.
    backdrop: Option<Picture>,
    /// The variant's window colors (red on WL6, blue on Spear).
    colors: Colors,
    /// Cursor animation clock, in seconds.
    blink: f32,
}

impl Menu {
    pub fn new(vga: &VgaGraph, variant: &crate::variant::Variant) -> Self {
        let gfx = &variant.gfx;
        let episodes = match gfx.episode1 {
            Some(e1) => (0..NUM_EPISODES).map(|e| vga.pic(e1 + e)).collect(),
            None => Vec::new(),
        };
        // SOD's title uses a custom palette (TITLEPALETTE); WL6 uses the game one.
        let (title, title_bottom) = match gfx.title_palette {
            Some(pal_chunk) => {
                let pal = vga.load_vga_palette(pal_chunk);
                (
                    vga.pic_with_palette(gfx.title1, &pal),
                    gfx.title2.map(|c| vga.pic_with_palette(c, &pal)),
                )
            }
            None => (vga.pic(gfx.title1), gfx.title2.map(|c| vga.pic(c))),
        };
        Self {
            font: Font::load(vga, 0),
            title,
            title_bottom,
            options: vga.pic(gfx.options),
            cursor: [vga.pic(gfx.cursor1), vga.pic(gfx.cursor2)],
            episodes,
            difficulty: (0..NUM_DIFFICULTIES)
                .map(|d| vga.pic(gfx.baby_mode + d))
                .collect(),
            radio: [vga.pic(gfx.not_selected), vga.pic(gfx.selected)],
            sound_titles: [vga.pic(gfx.fx_title), vga.pic(gfx.music_title)],
            ls_titles: [vga.pic(gfx.load_game), vga.pic(gfx.save_game)],
            credits: vga.pic(gfx.credits),
            backdrop: gfx.backdrop.map(|c| vga.pic(c)),
            colors: Colors::for_variant(variant.is_sod()),
            blink: 0.0,
        }
    }

    /// Advance the cursor blink clock (call once per frame from any menu).
    pub fn tick(&mut self, dt: f32) {
        self.blink = (self.blink + dt) % 1.0;
    }

    /// The current gun-cursor frame: the original alternates C_CURSOR1PIC and
    /// C_CURSOR2PIC (the gun "twitches") on a short cycle.
    fn cursor_frame(&self) -> &Picture {
        &self.cursor[if self.blink < 0.5 { 0 } else { 1 }]
    }

    /// DrawGun (WL_MENU.C): place the gun cursor at (`x`, `y`).
    ///
    /// The cursor pics paint empty space as [`Colors::bkgd`] (the window fill),
    /// not true transparency. The original first clears a 25x16 pad with that
    /// color so the empty pixels vanish over the window; we also skip those
    /// pixels when blitting so the gun sits cleanly when it straddles the
    /// window edge onto the brighter border (Sound menu Off rows).
    fn draw_gun(&self, fb: &mut Framebuffer, x: i32, y: i32) {
        bar(fb, x - 1, y, 25, 16, self.colors.bkgd);
        blit_masked(
            fb,
            self.cursor_frame(),
            x,
            y,
            vgagraph_color(self.colors.bkgd),
        );
    }

    // --- Screens ----------------------------------------------------------

    /// The title screen: one full-screen pic (WL6), or the two stacked halves
    /// TITLE1PIC/TITLE2PIC (SOD).
    pub fn render_title(&self, fb: &mut Framebuffer) {
        blit(fb, &self.title, 0, 0);
        if let Some(bottom) = &self.title_bottom {
            blit(fb, bottom, 0, self.title.height as i32);
        }
    }

    /// CREDITSPIC, full screen (the attract-loop credits page).
    pub fn render_credits(&self, fb: &mut Framebuffer) {
        blit(fb, &self.credits, 0, 0);
    }

    /// The flashing "DEMO" tag drawn in a corner during attract playback
    /// (WL_PLAY.C PlayDemo shows "DEMO" over the recorded run). `blink` drives a
    /// slow pulse between the two grey shades.
    pub fn draw_demo_label(&self, fb: &mut Framebuffer) {
        let color = if self.blink < 0.5 {
            HIGHLIGHT
        } else {
            TEXTCOLOR
        };
        // Shadowed, near the top-left corner of the 3D view.
        self.font.draw(fb, 9, 5, "DEMO", 0);
        self.font.draw(fb, 8, 4, "DEMO", color);
    }

    /// The main menu: the cleared backdrop, Options header, the item window and
    /// the grey/greyed item list with the gun cursor on `selected`.
    ///
    /// `started` is true while a game is in progress (Esc from play). That swaps
    /// "View Scores" → "End Game" and "Back to Demo" → "Back to Game", and enables
    /// Save. `has_read_this` is false for Spear of Destiny (no help article).
    pub fn render_main(
        &self,
        fb: &mut Framebuffer,
        selected: usize,
        started: bool,
        has_read_this: bool,
    ) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 10);
        blit(fb, &self.options, 84, 0);
        self.draw_window(fb, MENU_X - 8, MENU_Y - 3, MENU_W, MENU_H, self.colors.bkgd);

        for (i, item) in MAIN_ITEMS.iter().enumerate() {
            let active =
                item.active && (i != ITEM_SAVE || started) && (i != ITEM_READ || has_read_this);
            let color = if !active {
                self.colors.deactive
            } else if i == selected {
                HIGHLIGHT
            } else {
                TEXTCOLOR
            };
            let y = MENU_Y + i as i32 * ITEM_STEP;
            let label = main_item_label(i, started);
            self.font.draw(fb, MENU_X + MENU_INDENT, y, label, color);
        }

        // DrawGun: cursor at x = MENU_X & ~7, y = (MENU_Y - 2) + which*13.
        let cx = MENU_X & !7;
        let cy = MENU_Y - 2 + selected as i32 * ITEM_STEP;
        self.draw_gun(fb, cx, cy);
    }

    /// Episode select: the six C_EPISODE banners stacked, gun on `selected`.
    pub fn render_episode(&self, fb: &mut Framebuffer, selected: usize) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 0);
        self.font
            .draw_centered(fb, 3, "Which episode to play?", HIGHLIGHT);

        for (i, pic) in self.episodes.iter().enumerate() {
            let y = NE_Y + i as i32 * 26;
            blit(fb, pic, NE_X + 32, y);
            let color = if i == selected { HIGHLIGHT } else { TEXTCOLOR };
            let tx = NE_X + 32 + pic.width as i32 + 8;
            let (title, subtitle) = EPISODE_NAMES[i];
            self.font.draw(fb, tx, y + 1, title, color);
            self.font
                .draw(fb, tx, y + 1 + self.font.height() as i32, subtitle, color);
        }
        let cy = NE_Y + selected as i32 * 26 + 4;
        self.draw_gun(fb, NE_X, cy);
    }

    /// Difficulty select: the "How tough are you?" prompt over the selected
    /// skill's BJ banner (the original cycles one banner at a time, its face
    /// hardening with the skill).
    pub fn render_difficulty(&self, fb: &mut Framebuffer, selected: usize) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 10);
        self.font
            .draw_centered(fb, 68, "How tough are you?", HIGHLIGHT);

        let pic = &self.difficulty[selected];
        let x = (WIDTH as i32 - pic.width as i32) / 2;
        blit(fb, pic, x, 90);

        // The skill phrase under the mugshot, then a navigation hint.
        let phrase_y = 90 + pic.height as i32 + 6;
        self.font
            .draw_centered(fb, phrase_y, DIFFICULTY_NAMES[selected], HIGHLIGHT);
        self.font.draw_centered(
            fb,
            phrase_y + self.font.height() as i32 + 8,
            "Up / Down to choose, Enter to start",
            TEXTCOLOR,
        );
    }

    /// The Sound options screen (WL_MENU.C CP_Sound shape, modern labels).
    /// Two radio groups: effects (digital / synthesized / off) and music
    /// (on / off). The DOS hardware names (AdLib, Sound Blaster) are not shown.
    pub fn render_sound(
        &self,
        fb: &mut Framebuffer,
        sfx: SfxMode,
        music_on: bool,
        selected: usize,
    ) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 10);

        // Effects group: 3 rows. Order matches [`SfxMode`].
        let sfx_labels = ["Digital", "Synthesized", "Off"];
        let sfx_active = sfx as usize;
        self.draw_window(
            fb,
            SM_X - 8,
            SM_Y1 - 3,
            SM_W,
            3 * ITEM_STEP + 2,
            self.colors.bkgd,
        );
        blit(fb, &self.sound_titles[0], 100, SM_Y1 - 18);
        for (i, label) in sfx_labels.iter().enumerate() {
            let row = i;
            let y = SM_Y1 + i as i32 * ITEM_STEP + 2;
            self.draw_radio_item(fb, y, label, sfx_active == i, selected == row);
        }

        // Music group: 2 rows.
        let music_labels = ["On", "Off"];
        let music_active = if music_on { 0 } else { 1 };
        self.draw_window(
            fb,
            SM_X - 8,
            SM_Y2 - 3,
            SM_W,
            2 * ITEM_STEP + 2,
            self.colors.bkgd,
        );
        blit(fb, &self.sound_titles[1], 100, SM_Y2 - 18);
        for (j, label) in music_labels.iter().enumerate() {
            let row = 3 + j;
            let y = SM_Y2 + j as i32 * ITEM_STEP + 2;
            self.draw_radio_item(fb, y, label, music_active == j, selected == row);
        }

        // Gun cursor on the selected row.
        let cy = if selected < 3 {
            SM_Y1 + selected as i32 * ITEM_STEP
        } else {
            SM_Y2 + (selected - 3) as i32 * ITEM_STEP
        };
        self.draw_gun(fb, SM_X & !7, cy);
    }

    /// WL_MENU.C `Message`: a grey box of black text centered on screen, drawn
    /// over whatever screen is already there (the Quit prompt sits on the main
    /// menu). The box is filled in TEXTCOLOR and then outlined black on top /
    /// left, TEXTCOLOR on bottom / right — so only the top-left bevel shows.
    pub fn render_message(&self, fb: &mut Framebuffer, text: &str) {
        let fh = self.font.height() as i32;
        let widths: Vec<i32> = text
            .split('\n')
            .map(|l| self.font.text_width(l) as i32)
            .collect();
        let h = fh * widths.len() as i32;

        // Message() folds the running width into `mw` at each newline but pads
        // only the final line by 10, so a trailing longest line widens the box.
        let (last, rest) = widths.split_last().expect("split always yields a line");
        let mw = rest.iter().copied().max().unwrap_or(0).max(last + 10);

        // PrintX = 160 - mw/2, PrintY = WindowH/2 - h/2 (WindowH is 200 here).
        let x = 160 - mw / 2;
        let y = HEIGHT as i32 / 2 - h / 2;
        bar(fb, x - 5, y - 5, mw + 10, h + 10, TEXTCOLOR);
        hlin(fb, x - 5, x + mw + 5, y - 5, 0);
        vlin(fb, y - 5, y + h + 5, x - 5, 0);
        hlin(fb, x - 5, x + mw + 5, y + h + 5, TEXTCOLOR);
        vlin(fb, y - 5, y + h + 5, x + mw + 5, TEXTCOLOR);

        for (i, line) in text.split('\n').enumerate() {
            self.font.draw(fb, x, y + i as i32 * fh, line, 0);
        }
    }

    /// One radio row: the (un)filled bullet, then the label, highlighted when
    /// the cursor rests on it.
    fn draw_radio_item(&self, fb: &mut Framebuffer, y: i32, label: &str, on: bool, cursor: bool) {
        let pic = &self.radio[on as usize];
        let rx = SM_X + MENU_INDENT;
        blit(fb, pic, rx, y);
        let color = if cursor { HIGHLIGHT } else { TEXTCOLOR };
        self.font
            .draw(fb, rx + pic.width as i32 + 8, y, label, color);
    }

    /// The Load Game slot list (WL_MENU.C CP_LoadGame): filled slots show their
    /// name, empty slots grey out as "- EMPTY -" and cannot be chosen.
    /// The level-select cheat grid (not in the original): 6 episode columns x
    /// 10 floor rows in the house menu style; the selected map's name shows
    /// under the grid.
    pub fn render_level_select(&self, fb: &mut Framebuffer, names: &[String], selected: usize) {
        self.clear_mscreen(fb);
        self.font
            .draw_centered(fb, 4, "Warp to which level?", HIGHLIGHT);

        const GRID_X: i32 = 34; // centers 6 x 42px columns
        const GRID_Y: i32 = 24;
        const COL_W: i32 = 42;
        const ROW_H: i32 = 14;
        self.draw_window(
            fb,
            GRID_X - 8,
            GRID_Y - 3,
            6 * COL_W + 10,
            11 * ROW_H + 2,
            self.colors.bkgd,
        );

        for ep in 0..6i32 {
            let x = GRID_X + ep * COL_W;
            self.font
                .draw(fb, x, GRID_Y, &format!("E{}", ep + 1), TEXTCOLOR);
            for floor in 0..10i32 {
                let idx = (ep * 10 + floor) as usize;
                let y = GRID_Y + (floor + 1) * ROW_H;
                let sel = idx == selected;
                if sel {
                    // A highlight bar behind the selected cell.
                    self.draw_window(fb, x - 3, y - 2, COL_W - 6, ROW_H - 2, self.colors.stripe);
                }
                let label = match floor {
                    8 => "Boss".to_string(),
                    9 => "Scrt".to_string(),
                    f => format!("F{}", f + 1),
                };
                let color = if sel { HIGHLIGHT } else { TEXTCOLOR };
                self.font.draw(fb, x, y, &label, color);
            }
        }

        let name = names.get(selected).map(String::as_str).unwrap_or("?");
        self.font
            .draw_centered(fb, GRID_Y + 11 * ROW_H + 4, name, HIGHLIGHT);
    }

    /// Change View (WL_MENU.C CP_ChangeView): a caption band drawn over the live
    /// 3D-view preview (the world is already rendered at the chosen size). Shows
    /// the size step and the arrow controls.
    pub fn render_change_view(&self, fb: &mut Framebuffer, view_w: usize) {
        bar(fb, 0, 0, WIDTH as i32, 22, 0);
        hlin(fb, 0, WIDTH as i32 - 1, 22, self.colors.stripe);
        self.font
            .draw_centered(fb, 2, "Change View - Left / Right to size", HIGHLIGHT);
        let size = view_w / 16; // 4..=20 (original's ChangeView units)
        self.font.draw_centered(
            fb,
            2 + self.font.height() as i32,
            &format!("< {size} >   (Enter / Esc accepts)"),
            TEXTCOLOR,
        );
    }

    /// Control (WL_MENU.C CP_Control, simplified): a mouse-sensitivity slider
    /// (0..=20) plus a short fixed-controls reminder. Full key rebinding is
    /// intentionally out of scope; see the README Controls section for the full
    /// modern / classic-style mapping.
    pub fn render_control(&self, fb: &mut Framebuffer, sensitivity: usize) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 10);
        self.font.draw_centered(fb, 34, "CONTROL", HIGHLIGHT);

        self.draw_window(fb, 40, 50, 240, 44, self.colors.bkgd);
        self.font.draw(fb, 52, 56, "Mouse Sensitivity", TEXTCOLOR);

        // Slider track (0..=20) with a highlight knob.
        let track_x = 52;
        let track_y = 76;
        let track_w = 190;
        bar(fb, track_x, track_y, track_w, 4, self.colors.deactive);
        let knob = track_x + sensitivity as i32 * (track_w - 6) / MAX_MOUSE_SENS as i32;
        bar(fb, knob, track_y - 4, 6, 12, HIGHLIGHT);
        self.font.draw(
            fb,
            track_x + track_w + 8,
            track_y - 4,
            &format!("{sensitivity}"),
            HIGHLIGHT,
        );

        let fy = 108;
        self.font.draw_centered(
            fb,
            fy,
            "Left / Right adjust  -  Enter / Esc accepts",
            TEXTCOLOR,
        );
        // Fixed play mapping (both schemes are always live). Keep lines short
        // enough for the 320-wide proportional menu font.
        self.font
            .draw_centered(fb, fy + 16, "WASD + mouse look, or arrows", TEXTCOLOR);
        self.font.draw_centered(
            fb,
            fy + 29,
            "Fire: click / Space / Ctrl   Open: RMB / E",
            TEXTCOLOR,
        );
        self.font.draw_centered(
            fb,
            fy + 42,
            "Shift run   1-4 weapons   wheel cycles",
            TEXTCOLOR,
        );
        self.font.draw_centered(
            fb,
            fy + 58,
            "Cheats: 6 warp  7 god  8 items  9 ammo  0 MLI",
            self.colors.deactive,
        );
    }

    pub fn render_load(&self, fb: &mut Framebuffer, slots: &[Option<String>], selected: usize) {
        self.render_slot_list(fb, &self.ls_titles[0], slots, selected, false, "");
    }

    /// The Save Game slot list (WL_MENU.C CP_SaveGame). While `entering` a name,
    /// the selected slot shows the live text buffer with a caret.
    pub fn render_save(
        &self,
        fb: &mut Framebuffer,
        slots: &[Option<String>],
        selected: usize,
        entering: bool,
        name: &str,
    ) {
        self.render_slot_list(fb, &self.ls_titles[1], slots, selected, entering, name);
    }

    /// Shared slot-list body for Load and Save. `empty_disabled` (Load only)
    /// greys empty slots; `entering`/`name` (Save only) draw the text field.
    #[allow(clippy::too_many_arguments)]
    fn render_slot_list(
        &self,
        fb: &mut Framebuffer,
        header: &Picture,
        slots: &[Option<String>],
        selected: usize,
        entering: bool,
        name: &str,
    ) {
        self.clear_mscreen(fb);
        self.draw_stripes(fb, 10);
        blit(fb, header, 84, 0);
        self.draw_window(fb, LSM_X - 8, LSM_Y - 3, LSM_W, LSM_H, self.colors.bkgd);

        let box_x = LSM_X + 16;
        let box_w = LSM_W - 16 - 15;
        for i in 0..NUM_SLOTS {
            let y = LSM_Y + i as i32 * ITEM_STEP;
            self.draw_outline(fb, box_x, y, box_w, 11);
            let filled = slots.get(i).and_then(Option::as_ref).is_some();
            let editing = entering && i == selected;
            let text = if editing {
                format!("{name}_")
            } else if let Some(Some(n)) = slots.get(i) {
                n.clone()
            } else {
                "- EMPTY -".to_string()
            };
            let color = if editing || i == selected {
                HIGHLIGHT
            } else if filled {
                TEXTCOLOR
            } else {
                self.colors.deactive
            };
            self.font.draw(fb, box_x + 3, y + 2, &text, color);
        }

        // The gun cursor sits in the window's left margin (hidden while typing).
        if !entering {
            let cy = LSM_Y - 2 + selected as i32 * ITEM_STEP;
            self.draw_gun(fb, LSM_X & !7, cy);
        }
    }
}

// --- Primitives (VWB_Bar / VWB_Hlin / VWB_Vlin / VWB_DrawPic) -------------

impl Menu {
    /// ClearMScreen: WL6 bars the screen in `bord`, Spear draws C_BACKDROPPIC
    /// over the whole 320x200 instead (its blue marbled menu background).
    fn clear_mscreen(&self, fb: &mut Framebuffer) {
        match &self.backdrop {
            Some(pic) => blit(fb, pic, 0, 0),
            None => fb.pixels.fill(vgagraph_color(self.colors.bord)),
        }
    }

    /// DrawStripes: a 320x24 black bar at `y` with a `stripe`-colored rule near
    /// its bottom (the black header band behind the menu art).
    fn draw_stripes(&self, fb: &mut Framebuffer, y: i32) {
        bar(fb, 0, y, 320, 24, 0);
        hlin(fb, 0, 319, y + 22, self.colors.stripe);
    }

    /// DrawWindow: a filled rectangle with a bevelled DrawOutline.
    fn draw_window(&self, fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, color: u8) {
        bar(fb, x, y, w, h, color);
        self.draw_outline(fb, x, y, w, h);
    }

    /// A bevelled empty box outline (DrawOutline), also used bare for the
    /// save-slot cells: top/left in the darker `deactive`, bottom/right in the
    /// brighter `bord2`.
    fn draw_outline(&self, fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32) {
        hlin(fb, x, x + w, y, self.colors.deactive);
        vlin(fb, y, y + h, x, self.colors.deactive);
        hlin(fb, x, x + w, y + h, self.colors.bord2);
        vlin(fb, y, y + h, x + w, self.colors.bord2);
    }
}

fn bar(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, color: u8) {
    let rgba = vgagraph_color(color);
    for yy in y..y + h {
        if yy < 0 || yy as usize >= HEIGHT {
            continue;
        }
        for xx in x..x + w {
            if xx < 0 || xx as usize >= WIDTH {
                continue;
            }
            fb.pixels[yy as usize * WIDTH + xx as usize] = rgba;
        }
    }
}

fn hlin(fb: &mut Framebuffer, x0: i32, x1: i32, y: i32, color: u8) {
    if y < 0 || y as usize >= HEIGHT {
        return;
    }
    let rgba = vgagraph_color(color);
    for xx in x0..=x1 {
        if xx >= 0 && (xx as usize) < WIDTH {
            fb.pixels[y as usize * WIDTH + xx as usize] = rgba;
        }
    }
}

fn vlin(fb: &mut Framebuffer, y0: i32, y1: i32, x: i32, color: u8) {
    if x < 0 || x as usize >= WIDTH {
        return;
    }
    let rgba = vgagraph_color(color);
    for yy in y0..=y1 {
        if yy >= 0 && (yy as usize) < HEIGHT {
            fb.pixels[yy as usize * WIDTH + x as usize] = rgba;
        }
    }
}

/// Copy an opaque picture into the framebuffer at pixel (dx, dy), clipped.
fn blit(fb: &mut Framebuffer, pic: &Picture, dx: i32, dy: i32) {
    for row in 0..pic.height as i32 {
        let y = dy + row;
        if y < 0 || y as usize >= HEIGHT {
            continue;
        }
        for col in 0..pic.width as i32 {
            let x = dx + col;
            if x < 0 || x as usize >= WIDTH {
                continue;
            }
            fb.pixels[y as usize * WIDTH + x as usize] =
                pic.pixels[row as usize * pic.width + col as usize];
        }
    }
}

/// Like [`blit`], but skip pixels equal to `mask` (the menu gun's empty space
/// is painted as BKGDCOLOR, not true transparency).
fn blit_masked(fb: &mut Framebuffer, pic: &Picture, dx: i32, dy: i32, mask: u32) {
    for row in 0..pic.height as i32 {
        let y = dy + row;
        if y < 0 || y as usize >= HEIGHT {
            continue;
        }
        for col in 0..pic.width as i32 {
            let x = dx + col;
            if x < 0 || x as usize >= WIDTH {
                continue;
            }
            let c = pic.pixels[row as usize * pic.width + col as usize];
            if c != mask {
                fb.pixels[y as usize * WIDTH + x as usize] = c;
            }
        }
    }
}

fn vgagraph_color(index: u8) -> u32 {
    crate::assets::palette::PALETTE[index as usize]
}

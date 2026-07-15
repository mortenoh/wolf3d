//! The front-end menus (title / main menu / episode select / difficulty
//! select), rewritten from WOLFSRC/WL_MENU.C. Rendering only lives here; the
//! [`crate::game::Game`] state machine owns the current screen and selection
//! and drives navigation, so the whole flow is headless-verifiable.
//!
//! Colors and layout are the registered WL6 (non-SPEAR) constants from
//! WL_MENU.H: the screen clears to a medium red (`BORDCOLOR`), the item window
//! is a darker red (`BKGDCOLOR`) with a bevelled outline, active text is grey
//! (`TEXTCOLOR`, brighter `HIGHLIGHT` under the gun), and disabled items are the
//! dark-red `DEACTIVE`.

use crate::assets::vgagraph::Picture;
use crate::assets::VgaGraph;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;
use crate::config::MAX_MOUSE_SENS;
use crate::game::SfxMode;
use crate::savegame::NUM_SLOTS;

// --- Palette color bytes (WL_MENU.H, non-SPEAR build) ---
const BORDCOLOR: u8 = 0x29;
const BORD2COLOR: u8 = 0x23;
const DEACTIVE: u8 = 0x2b;
const BKGDCOLOR: u8 = 0x2d;
const STRIPE: u8 = 0x2c;
const TEXTCOLOR: u8 = 0x17;
const HIGHLIGHT: u8 = 0x13;

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
    MenuItem { label: "New Game", active: true },
    MenuItem { label: "Sound", active: true },
    MenuItem { label: "Control", active: true },
    MenuItem { label: "Load Game", active: true },
    // Save Game is only reachable from the pause menu during play; the game
    // state machine greys it via `Game::main_item_active` when not started.
    MenuItem { label: "Save Game", active: true },
    MenuItem { label: "Change View", active: true },
    MenuItem { label: "Read This!", active: true },
    MenuItem { label: "View Scores", active: true },
    // Returns to the title / attract loop (WL_MENU.C "Back to Demo").
    MenuItem { label: "Back to Demo", active: true },
    MenuItem { label: "Quit", active: true },
];
/// Indices of the wired main-menu entries for the game state machine.
pub const ITEM_NEW_GAME: usize = 0;
pub const ITEM_SOUND: usize = 1;
pub const ITEM_CONTROL: usize = 2;
pub const ITEM_LOAD: usize = 3;
pub const ITEM_SAVE: usize = 4;
pub const ITEM_CHANGEVIEW: usize = 5;
pub const ITEM_READ: usize = 6;
pub const ITEM_VIEWSCORES: usize = 7;
pub const ITEM_BACKTODEMO: usize = 8;
pub const ITEM_QUIT: usize = 9;

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
                (vga.pic_with_palette(gfx.title1, &pal), gfx.title2.map(|c| vga.pic_with_palette(c, &pal)))
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
            difficulty: (0..NUM_DIFFICULTIES).map(|d| vga.pic(gfx.baby_mode + d)).collect(),
            radio: [vga.pic(gfx.not_selected), vga.pic(gfx.selected)],
            sound_titles: [vga.pic(gfx.fx_title), vga.pic(gfx.music_title)],
            ls_titles: [vga.pic(gfx.load_game), vga.pic(gfx.save_game)],
            credits: vga.pic(gfx.credits),
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
        let color = if self.blink < 0.5 { HIGHLIGHT } else { TEXTCOLOR };
        // Shadowed, near the top-left corner of the 3D view.
        self.font.draw(fb, 9, 5, "DEMO", 0);
        self.font.draw(fb, 8, 4, "DEMO", color);
    }

    /// The main menu: red backdrop, Options header, the item window and the
    /// grey/greyed item list with the gun cursor on `selected`.
    pub fn render_main(&self, fb: &mut Framebuffer, selected: usize, started: bool) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);
        blit(fb, &self.options, 84, 0);
        draw_window(fb, MENU_X - 8, MENU_Y - 3, MENU_W, MENU_H, BKGDCOLOR);

        for (i, item) in MAIN_ITEMS.iter().enumerate() {
            // Save Game greys out until a game is in progress.
            let active = item.active && (i != ITEM_SAVE || started);
            let color = if !active {
                DEACTIVE
            } else if i == selected {
                HIGHLIGHT
            } else {
                TEXTCOLOR
            };
            let y = MENU_Y + i as i32 * ITEM_STEP;
            self.font.draw(fb, MENU_X + MENU_INDENT, y, item.label, color);
        }

        // DrawGun: cursor at x = MENU_X & ~7, y = (MENU_Y - 2) + which*13.
        let cx = MENU_X & !7;
        let cy = MENU_Y - 2 + selected as i32 * ITEM_STEP;
        blit(fb, self.cursor_frame(), cx, cy);
    }

    /// Episode select: the six C_EPISODE banners stacked, gun on `selected`.
    pub fn render_episode(&self, fb: &mut Framebuffer, selected: usize) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 0);
        self.font.draw_centered(fb, 3, "Which episode to play?", HIGHLIGHT);

        for (i, pic) in self.episodes.iter().enumerate() {
            let y = NE_Y + i as i32 * 26;
            blit(fb, pic, NE_X + 32, y);
            let color = if i == selected { HIGHLIGHT } else { TEXTCOLOR };
            let tx = NE_X + 32 + pic.width as i32 + 8;
            let (title, subtitle) = EPISODE_NAMES[i];
            self.font.draw(fb, tx, y + 1, title, color);
            self.font.draw(fb, tx, y + 1 + self.font.height() as i32, subtitle, color);
        }
        let cy = NE_Y + selected as i32 * 26 + 4;
        blit(fb, self.cursor_frame(), NE_X, cy);
    }

    /// Difficulty select: the "How tough are you?" prompt over the selected
    /// skill's BJ banner (the original cycles one banner at a time, its face
    /// hardening with the skill).
    pub fn render_difficulty(&self, fb: &mut Framebuffer, selected: usize) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);
        self.font.draw_centered(fb, 68, "How tough are you?", HIGHLIGHT);

        let pic = &self.difficulty[selected];
        let x = (WIDTH as i32 - pic.width as i32) / 2;
        blit(fb, pic, x, 90);

        // The skill phrase under the mugshot, then a navigation hint.
        let phrase_y = 90 + pic.height as i32 + 6;
        self.font.draw_centered(fb, phrase_y, DIFFICULTY_NAMES[selected], HIGHLIGHT);
        self.font
            .draw_centered(fb, phrase_y + self.font.height() as i32 + 8, "Up / Down to choose, Enter to start", TEXTCOLOR);
    }

    /// The Sound options screen (WL_MENU.C CP_Sound), collapsed to two radio
    /// groups: sound effects (Digitized+AdLib / AdLib only / None) and music
    /// (AdLib / None). The filled bullet marks the active option in each group.
    pub fn render_sound(&self, fb: &mut Framebuffer, sfx: SfxMode, music_on: bool, selected: usize) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);

        // Effects group: 3 rows.
        let sfx_labels = ["Digitized + AdLib", "AdLib only", "None"];
        let sfx_active = sfx as usize; // matches SfxMode ordering
        draw_window(fb, SM_X - 8, SM_Y1 - 3, SM_W, 3 * ITEM_STEP + 2, BKGDCOLOR);
        blit(fb, &self.sound_titles[0], 100, SM_Y1 - 18);
        for (i, label) in sfx_labels.iter().enumerate() {
            let row = i;
            let y = SM_Y1 + i as i32 * ITEM_STEP + 2;
            self.draw_radio_item(fb, y, label, sfx_active == i, selected == row);
        }

        // Music group: 2 rows.
        let music_labels = ["AdLib", "None"];
        let music_active = if music_on { 0 } else { 1 };
        draw_window(fb, SM_X - 8, SM_Y2 - 3, SM_W, 2 * ITEM_STEP + 2, BKGDCOLOR);
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
        blit(fb, self.cursor_frame(), SM_X & !7, cy);
    }

    /// One radio row: the (un)filled bullet, then the label, highlighted when
    /// the cursor rests on it.
    fn draw_radio_item(&self, fb: &mut Framebuffer, y: i32, label: &str, on: bool, cursor: bool) {
        let pic = &self.radio[on as usize];
        let rx = SM_X + MENU_INDENT;
        blit(fb, pic, rx, y);
        let color = if cursor { HIGHLIGHT } else { TEXTCOLOR };
        self.font.draw(fb, rx + pic.width as i32 + 8, y, label, color);
    }

    /// The Load Game slot list (WL_MENU.C CP_LoadGame): filled slots show their
    /// name, empty slots grey out as "- EMPTY -" and cannot be chosen.
    /// The level-select cheat grid (not in the original): 6 episode columns x
    /// 10 floor rows in the house menu style; the selected map's name shows
    /// under the grid.
    pub fn render_level_select(&self, fb: &mut Framebuffer, names: &[String], selected: usize) {
        clear(fb, BORDCOLOR);
        self.font.draw_centered(fb, 4, "Warp to which level?", HIGHLIGHT);

        const GRID_X: i32 = 34; // centers 6 x 42px columns
        const GRID_Y: i32 = 24;
        const COL_W: i32 = 42;
        const ROW_H: i32 = 14;
        draw_window(fb, GRID_X - 8, GRID_Y - 3, 6 * COL_W + 10, 11 * ROW_H + 2, BKGDCOLOR);

        for ep in 0..6i32 {
            let x = GRID_X + ep * COL_W;
            self.font.draw(fb, x, GRID_Y, &format!("E{}", ep + 1), TEXTCOLOR);
            for floor in 0..10i32 {
                let idx = (ep * 10 + floor) as usize;
                let y = GRID_Y + (floor + 1) * ROW_H;
                let sel = idx == selected;
                if sel {
                    // A highlight bar behind the selected cell.
                    draw_window(fb, x - 3, y - 2, COL_W - 6, ROW_H - 2, STRIPE);
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
        self.font.draw_centered(fb, GRID_Y + 11 * ROW_H + 4, name, HIGHLIGHT);
    }

    /// Change View (WL_MENU.C CP_ChangeView): a caption band drawn over the live
    /// 3D-view preview (the world is already rendered at the chosen size). Shows
    /// the size step and the arrow controls.
    pub fn render_change_view(&self, fb: &mut Framebuffer, view_w: usize) {
        bar(fb, 0, 0, WIDTH as i32, 22, 0);
        hlin(fb, 0, WIDTH as i32 - 1, 22, STRIPE);
        self.font.draw_centered(fb, 2, "Change View - Left / Right to size", HIGHLIGHT);
        let size = view_w / 16; // 4..=20 (original's ChangeView units)
        self.font.draw_centered(
            fb,
            2 + self.font.height() as i32,
            &format!("< {size} >   (Enter / Esc accepts)"),
            TEXTCOLOR,
        );
    }

    /// Control (WL_MENU.C CP_Control, simplified): a mouse-sensitivity slider
    /// (0..=20) with a footer that documents the play shortcuts. Full key
    /// rebinding is intentionally out of scope.
    pub fn render_control(&self, fb: &mut Framebuffer, sensitivity: usize) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);
        self.font.draw_centered(fb, 34, "CONTROL", HIGHLIGHT);

        draw_window(fb, 40, 58, 240, 44, BKGDCOLOR);
        self.font.draw(fb, 52, 64, "Mouse Sensitivity", TEXTCOLOR);

        // Slider track (0..=20) with a highlight knob.
        let track_x = 52;
        let track_y = 84;
        let track_w = 190;
        bar(fb, track_x, track_y, track_w, 4, DEACTIVE);
        let knob = track_x + sensitivity as i32 * (track_w - 6) / MAX_MOUSE_SENS as i32;
        bar(fb, knob, track_y - 4, 6, 12, HIGHLIGHT);
        self.font.draw(fb, track_x + track_w + 8, track_y - 4, &format!("{sensitivity}"), HIGHLIGHT);

        let fy = 118;
        self.font.draw_centered(fb, fy, "Left / Right adjust  -  Enter / Esc accepts", TEXTCOLOR);
        self.font.draw_centered(fb, fy + 13, "Key rebinding not available", DEACTIVE);
        self.font.draw_centered(fb, fy + 30, "Shortcuts:  M music   6 warp   7 god", TEXTCOLOR);
        self.font.draw_centered(fb, fy + 43, "8 items   9 infinite ammo   0 all", TEXTCOLOR);
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
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);
        blit(fb, header, 84, 0);
        draw_window(fb, LSM_X - 8, LSM_Y - 3, LSM_W, LSM_H, BKGDCOLOR);

        let box_x = LSM_X + 16;
        let box_w = LSM_W - 16 - 15;
        for i in 0..NUM_SLOTS {
            let y = LSM_Y + i as i32 * ITEM_STEP;
            draw_outline(fb, box_x, y, box_w, 11);
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
                DEACTIVE
            };
            self.font.draw(fb, box_x + 3, y + 2, &text, color);
        }

        // The gun cursor sits in the window's left margin (hidden while typing).
        if !entering {
            let cy = LSM_Y - 2 + selected as i32 * ITEM_STEP;
            blit(fb, self.cursor_frame(), LSM_X & !7, cy);
        }
    }
}

// --- Primitives (VWB_Bar / VWB_Hlin / VWB_Vlin / VWB_DrawPic) -------------

/// Fill the whole framebuffer with a palette color.
fn clear(fb: &mut Framebuffer, color: u8) {
    let rgba = vgagraph_color(color);
    fb.pixels.fill(rgba);
}

/// DrawStripes: a 320x24 black bar at `y` with a STRIPE-colored rule near its
/// bottom (the black header band behind the menu art).
fn draw_stripes(fb: &mut Framebuffer, y: i32) {
    bar(fb, 0, y, 320, 24, 0);
    hlin(fb, 0, 319, y + 22, STRIPE);
}

/// DrawWindow: a filled rectangle with a bevelled DrawOutline (top/left in the
/// darker DEACTIVE, bottom/right in the brighter BORD2COLOR).
fn draw_window(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, color: u8) {
    bar(fb, x, y, w, h, color);
    hlin(fb, x, x + w, y, DEACTIVE);
    vlin(fb, y, y + h, x, DEACTIVE);
    hlin(fb, x, x + w, y + h, BORD2COLOR);
    vlin(fb, y, y + h, x + w, BORD2COLOR);
}

/// A bevelled empty box outline (DrawOutline) for the save-slot cells: top/left
/// in the darker DEACTIVE, bottom/right in the brighter BORD2COLOR.
fn draw_outline(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32) {
    hlin(fb, x, x + w, y, DEACTIVE);
    vlin(fb, y, y + h, x, DEACTIVE);
    hlin(fb, x, x + w, y + h, BORD2COLOR);
    vlin(fb, y, y + h, x + w, BORD2COLOR);
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

fn vgagraph_color(index: u8) -> u32 {
    crate::assets::palette::PALETTE[index as usize]
}

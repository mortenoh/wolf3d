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

use crate::assets::vgagraph::{
    Picture, C_BABYMODEPIC, C_CURSOR1PIC, C_CURSOR2PIC, C_EPISODE1PIC, C_OPTIONSPIC, TITLEPIC,
};
use crate::assets::VgaGraph;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;

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

/// The ten main-menu entries in original order. Only New Game and Quit are
/// wired up in this milestone; the rest render greyed-out (`active == false`),
/// exactly as the original draws not-yet-implemented options.
pub struct MenuItem {
    pub label: &'static str,
    pub active: bool,
}
pub const MAIN_ITEMS: [MenuItem; 10] = [
    MenuItem { label: "New Game", active: true },
    MenuItem { label: "Sound", active: false },
    MenuItem { label: "Control", active: false },
    MenuItem { label: "Load Game", active: false },
    MenuItem { label: "Save Game", active: false },
    MenuItem { label: "Change View", active: false },
    MenuItem { label: "Read This!", active: false },
    MenuItem { label: "View Scores", active: false },
    MenuItem { label: "Back to Demo", active: false },
    MenuItem { label: "Quit", active: true },
];
/// Index of New Game and Quit for the game state machine.
pub const ITEM_NEW_GAME: usize = 0;
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
    options: Picture,
    cursor: [Picture; 2],
    episodes: Vec<Picture>,
    difficulty: Vec<Picture>,
    /// Cursor animation clock, in seconds.
    blink: f32,
}

impl Menu {
    pub fn new(vga: &VgaGraph) -> Self {
        Self {
            font: Font::load(vga, 0),
            title: vga.pic(TITLEPIC),
            options: vga.pic(C_OPTIONSPIC),
            cursor: [vga.pic(C_CURSOR1PIC), vga.pic(C_CURSOR2PIC)],
            episodes: (0..NUM_EPISODES).map(|e| vga.pic(C_EPISODE1PIC + e)).collect(),
            difficulty: (0..NUM_DIFFICULTIES).map(|d| vga.pic(C_BABYMODEPIC + d)).collect(),
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

    /// TITLEPIC, full screen.
    pub fn render_title(&self, fb: &mut Framebuffer) {
        blit(fb, &self.title, 0, 0);
    }

    /// The main menu: red backdrop, Options header, the item window and the
    /// grey/greyed item list with the gun cursor on `selected`.
    pub fn render_main(&self, fb: &mut Framebuffer, selected: usize) {
        clear(fb, BORDCOLOR);
        draw_stripes(fb, 10);
        blit(fb, &self.options, 84, 0);
        draw_window(fb, MENU_X - 8, MENU_Y - 3, MENU_W, MENU_H, BKGDCOLOR);

        for (i, item) in MAIN_ITEMS.iter().enumerate() {
            let color = if !item.active {
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

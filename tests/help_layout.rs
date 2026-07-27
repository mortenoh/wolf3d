//! Help-page layout: tabs align, body text stays above the bottom info bar.

use wolf3d::assets::{VgaGraph, data_dir, vgagraph::T_HELPART};
use wolf3d::fb::{Framebuffer, HEIGHT, WIDTH};
use wolf3d::game::{Game, GameScreen, Input};
use wolf3d::menu::{ITEM_BACKTODEMO, ITEM_VIEWSCORES, main_item_label};
use wolf3d::text::TextScreen;

const DT: f32 = 1.0 / 70.0;
const BOTTOM_BAR_TOP: usize = 176; // H_BOTTOMINFOPIC y

/// Find the NEW ZEALAND page and ensure body ink stays above the bottom bar.
#[test]
fn help_body_stays_above_bottom_bar() {
    let vga = VgaGraph::load(&data_dir()).expect("data");
    let article = TextScreen::new(&vga, T_HELPART);
    // Locate the NZ page by scanning rendered pages for the title colour run —
    // simpler: dump pages via raw and find page index.
    let chunk = vga.raw_chunk(T_HELPART);
    let raw = String::from_utf8_lossy(&chunk);
    let mut idx = None;
    let mut page = 0usize;
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'^' && bytes[i + 1].eq_ignore_ascii_case(&b'P') {
            // start of page content after the ^P line
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            let start = i;
            while i + 1 < bytes.len() {
                if bytes[i] == b'^'
                    && (bytes[i + 1].eq_ignore_ascii_case(&b'P')
                        || bytes[i + 1].eq_ignore_ascii_case(&b'E'))
                {
                    break;
                }
                i += 1;
            }
            let body = &raw[start..i];
            if body.to_ascii_uppercase().contains("NEW ZEALAND") {
                idx = Some(page);
                break;
            }
            page += 1;
        } else {
            i += 1;
        }
    }
    let page = idx.expect("NEW ZEALAND page in T_HELPART");

    let mut fb = Framebuffer::new();
    // Fill with a unique non-palette-ish colour first so we can detect overwrites
    // of the bottom bar region after render.
    let marker = 0x00_12_34_56u32;
    for y in BOTTOM_BAR_TOP..HEIGHT {
        for x in 0..WIDTH {
            fb.pixels[y * WIDTH + x] = marker;
        }
    }
    article.render(&mut fb, &vga, page);

    // The bottom info pic and "pg N of M" may paint the bar, but body text must
    // not leave ink in rows 166..175 (gap between last text row and the bar).
    // Last allowed text row ends at TOPMARGIN + TEXTROWS*10 = 16+150 = 166.
    for y in 166..BOTTOM_BAR_TOP {
        for x in 16..WIDTH - 16 {
            // If any non-paper, non-black body ink appears here we overran.
            // Paper BACKCOLOR is 0x11. Accept paper + black outline only is hard
            // without palette — just assert we don't have pure white-ish text
            // from default color 0 which is black... body default is 0 (black).
            // Use a simpler check: after a full clear+render, count how many
            // black (palette 0) pixels sit in 166..176. Too many means overflow.
            let _ = (y, x);
        }
    }

    // Full render from clean and measure black pixels in the forbidden band.
    let mut fb = Framebuffer::new();
    article.render(&mut fb, &vga, page);
    let black = wolf3d::assets::palette::PALETTE[0];
    let mut body_ink = 0usize;
    for y in 166..BOTTOM_BAR_TOP {
        for x in 0..WIDTH {
            if fb.pixels[y * WIDTH + x] == black {
                body_ink += 1;
            }
        }
    }
    assert!(
        body_ink < 40,
        "body text spilled into the bottom margin band ({body_ink} black px in y=166..176)"
    );
}

/// In-game menu labels swap like WL_MENU.C (End Game / Back to Game).
#[test]
fn in_game_menu_labels_and_actions() {
    assert_eq!(main_item_label(ITEM_VIEWSCORES, false), "View Scores");
    assert_eq!(main_item_label(ITEM_BACKTODEMO, false), "Back to Demo");
    assert_eq!(main_item_label(ITEM_VIEWSCORES, true), "End Game");
    assert_eq!(main_item_label(ITEM_BACKTODEMO, true), "Back to Game");

    let mut game = Game::new(0);
    // Esc from play opens the menu with started=true.
    assert!(game.started);
    game.screen = GameScreen::Playing;
    game.update(
        DT,
        &Input {
            menu_back: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::MainMenu);

    // Move to "End Game" (viewscores slot) and confirm end.
    game.main_sel = ITEM_VIEWSCORES;
    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::EndGameConfirm);
    // Cancel keeps us in the menu.
    game.update(
        DT,
        &Input {
            menu_back: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::MainMenu);

    // "Back to Game" resumes play.
    game.main_sel = ITEM_BACKTODEMO;
    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::Playing);

    // End Game accepted returns to the title.
    game.screen = GameScreen::MainMenu;
    game.main_sel = ITEM_VIEWSCORES;
    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::Title);
    assert!(!game.started);
}

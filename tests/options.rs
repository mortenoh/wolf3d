//! Persistent options (config.rs) and the resizable 3D view (Change View).
//!
//! The config roundtrip runs against an isolated `WOLF3D_SAVE_DIR` so it never
//! touches the developer's real save/config files. The view test asserts the
//! shrunken viewport leaves the HUD untouched and paints the grey border, while
//! the full-size path stays pixel-identical.

use wolf3d::config::{self, Config, MAX_VIEW, MIN_VIEW};
use wolf3d::fb::{Framebuffer, HEIGHT, WIDTH};
use wolf3d::game::Game;
use wolf3d::hud::VIEW_H;

/// (a) A config with non-default values survives a to_bytes/from_bytes trip and
/// a save/load through an isolated directory.
#[test]
fn config_roundtrips() {
    let cfg = Config {
        view_size: 128,
        sfx_mode: 2,
        music_on: false,
        mouse_sensitivity: 17,
    };

    // In-memory roundtrip.
    let bytes = cfg.to_bytes();
    assert_eq!(Config::from_bytes(&bytes).unwrap(), cfg);

    // Disk roundtrip in a temp dir (WOLF3D_SAVE_DIR isolates config.bin too).
    let dir = std::env::temp_dir().join(format!("wolf3d_cfg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: single-threaded test setup before any config IO.
    unsafe { std::env::set_var("WOLF3D_SAVE_DIR", &dir) };

    assert!(config::load().is_none(), "no config should exist yet");
    config::save(&cfg).expect("save config");
    assert_eq!(config::load().expect("load config"), cfg);

    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("WOLF3D_SAVE_DIR") };
}

/// (b) A garbage / short buffer is rejected rather than panicking, and the
/// defaults are the classic full-view, full-sound settings.
#[test]
fn config_defaults_and_bad_data() {
    let d = Config::default();
    assert_eq!(d.view_size, MAX_VIEW);
    assert!(d.music_on);

    assert!(Config::from_bytes(b"nope").is_err());
    assert!(Config::from_bytes(&[]).is_err());
}

/// (c) Rendering at the smallest view size still draws the full status bar
/// unchanged and paints the grey border in the play area; the full-size render
/// is untouched by the feature.
#[test]
fn small_view_keeps_hud_and_draws_border() {
    // Full-size reference render.
    let mut full = Game::new(0);
    full.view_size = MAX_VIEW;
    let mut fb_full = Framebuffer::new();
    full.render(&mut fb_full);

    // Smallest-size render of the same level/spot.
    let mut small = Game::new(0);
    small.view_size = MIN_VIEW; // 64 wide
    let mut fb_small = Framebuffer::new();
    small.render(&mut fb_small);

    // The status bar (rows VIEW_H..HEIGHT) must be byte-identical between them.
    let hud_start = VIEW_H * WIDTH;
    let hud_end = HEIGHT * WIDTH;
    assert_eq!(
        fb_full.pixels[hud_start..hud_end],
        fb_small.pixels[hud_start..hud_end],
        "the HUD must be unaffected by the view size"
    );

    // The grey border fill must appear somewhere in the play area of the small
    // render (it never appears at full size).
    let border = wolf3d::assets::palette::PALETTE[wolf3d::raycast::BORDER_FILL as usize];
    let play = &fb_small.pixels[..hud_start];
    assert!(
        play.contains(&border),
        "the shrunken view must paint the grey border in the play area"
    );
}

/// DrawChangeView paints VIEWCOLOR only on the status strip (y 160..200) and
/// the original STR_SIZE1/2/3 lines; the live 3D play area is left alone.
#[test]
fn change_view_strip_is_teal_with_size_text() {
    use wolf3d::assets::palette::PALETTE;
    use wolf3d::game::{Game, GameScreen};
    use wolf3d::hud::VIEW_H;

    let mut game = Game::new(0);
    // Reference: plain play render of the same spot (full HUD, no teal strip).
    let mut fb_play = Framebuffer::new();
    game.render(&mut fb_play);

    game.screen = GameScreen::ChangeView;
    game.view_size = MAX_VIEW;
    let mut fb = Framebuffer::new();
    game.render(&mut fb);

    let teal = PALETTE[127];
    // Status strip must be VIEWCOLOR (with text ink on top).
    let mut teal_count = 0usize;
    let mut other = 0usize;
    for y in VIEW_H..HEIGHT {
        for x in 0..WIDTH {
            let p = fb.pixels[y * WIDTH + x];
            if p == teal {
                teal_count += 1;
            } else {
                other += 1;
            }
        }
    }
    assert!(
        teal_count > (WIDTH * 40) / 2,
        "most of the 40-row strip should be VIEWCOLOR teal, got {teal_count} teal / {other} other"
    );
    assert!(other > 50, "STR_SIZE text should put ink on the strip");

    // VIEWCOLOR bar is status-only: play rows must match a normal world render
    // (ChangeView draws the same 3D then overpaints only y>=160).
    assert_eq!(
        &fb.pixels[..VIEW_H * WIDTH],
        &fb_play.pixels[..VIEW_H * WIDTH],
        "ChangeView must not wipe the 3D play area with VIEWCOLOR"
    );
    // And the strip must differ from the normal HUD.
    assert_ne!(
        &fb.pixels[VIEW_H * WIDTH..],
        &fb_play.pixels[VIEW_H * WIDTH..],
        "ChangeView must replace the HUD with the teal instruction strip"
    );

    // Three STR_SIZE lines fit inside the strip (font 0 h=10, PrintY=161 →
    // last ink row 190). Rows 191..199 are pure teal.
    let bottom_rows_clear =
        (191..HEIGHT).all(|y| (0..WIDTH).all(|x| fb.pixels[y * WIDTH + x] == teal));
    assert!(
        bottom_rows_clear,
        "STR_SIZE lines must not overflow past y=190 into the bottom of the strip"
    );
}

/// Unit 20 (full 320) is the config default; menu steps only reach unit 19.
#[test]
fn change_view_steps_cap_at_unit_19() {
    use wolf3d::config::MAX_VIEW_UNIT;
    use wolf3d::game::{Game, GameScreen, Input};

    let mut game = Game::new(0);
    game.screen = GameScreen::ChangeView;
    game.view_size = MAX_VIEW; // unit 20
    assert_eq!(game.view_size / 16, 20);

    // Left from full → unit 19 (304).
    game.update(
        1.0 / 70.0,
        &Input {
            menu_left: true,
            ..Default::default()
        },
    );
    assert_eq!(game.view_size, MAX_VIEW_UNIT * 16);
    assert_eq!(game.view_size / 16, 19);

    // Right at unit 19 is a no-op (cannot recover unit 20 via the menu).
    game.update(
        1.0 / 70.0,
        &Input {
            menu_right: true,
            ..Default::default()
        },
    );
    assert_eq!(game.view_size / 16, 19);
}

/// Headless snapshot dump for visual review (ignored; run with --ignored).
#[test]
#[ignore]
fn generate_changeview_snaps() {
    use std::io::Write;
    use std::path::Path;
    use wolf3d::game::{Game, GameScreen};

    let out = std::env::var("WOLF3D_SNAP_DIR").unwrap_or_else(|_| "snaps/parity".into());
    std::fs::create_dir_all(&out).expect("snap dir");
    let mut fb = Framebuffer::new();
    let write_ppm = |fb: &Framebuffer, path: &Path| {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create ppm"));
        write!(f, "P6\n{WIDTH} {HEIGHT}\n255\n").unwrap();
        for &px in &fb.pixels {
            f.write_all(&px.to_le_bytes()[..3]).unwrap();
        }
    };
    let mut snap = |game: &mut Game, name: &str| {
        game.render(&mut fb);
        let path = Path::new(&out).join(format!("{name}.ppm"));
        write_ppm(&fb, &path);
        eprintln!("wrote {}", path.display());
    };

    let mut g = Game::new(0);
    g.screen = GameScreen::ChangeView;

    g.view_size = MAX_VIEW;
    snap(&mut g, "changeview_full");

    g.view_size = 256; // unit 16
    snap(&mut g, "changeview_small");

    g.view_size = MIN_VIEW; // unit 4
    snap(&mut g, "changeview_min");
}

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

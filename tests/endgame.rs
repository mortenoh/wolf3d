//! Endgame-milestone checks (requires `data/`): the high-score table math and
//! file roundtrip, the full boss-kill -> deathcam -> victory -> endtext ->
//! high-score-entry state-machine walk on E1M9 (Hans), and the "Read This!"
//! help pages. Nothing here opens a window or audio device.
//!
//! Set `WOLF3D_SNAP_DIR` and run the ignored `generate_snapshots` test to dump
//! PPMs of each endgame screen.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use wolf3d::actors::Kind;
use wolf3d::fb::{Framebuffer, HEIGHT, WIDTH};
use wolf3d::game::{Game, GameScreen, Input, WEAPON_CHAINGUN};
use wolf3d::highscore::{self, HighScore};

const DT: f32 = 1.0 / 70.0;

// --- Save-dir isolation (WOLF3D_SAVE_DIR is process-global) ------------------

static SAVE_ENV_LOCK: Mutex<()> = Mutex::new(());

fn unique_save_dir() -> (MutexGuard<'static, ()>, std::path::PathBuf) {
    let guard = SAVE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wolf3d-hitest-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp save dir");
    // Start clean so a stale highscores.bin never leaks between runs.
    let _ = std::fs::remove_file(dir.join("highscores.bin"));
    unsafe {
        std::env::set_var("WOLF3D_SAVE_DIR", &dir);
    }
    (guard, dir)
}

// --- Input helpers -----------------------------------------------------------

fn any() -> Input {
    Input { any_key: true, ..Default::default() }
}
fn enter() -> Input {
    Input { menu_enter: true, ..Default::default() }
}
fn esc() -> Input {
    Input { menu_back: true, ..Default::default() }
}

/// Point-blank-kill the floor's end boss under god mode: keep only the boss,
/// stand on an open tile beside it, and chaingun it until it dies (which hands
/// off to the deathcam). Panics if it cannot be positioned or killed.
fn kill_end_boss(game: &mut Game, kind: Kind) {
    game.god = true;
    game.infinite_ammo = true;
    game.weapon = WEAPON_CHAINGUN;
    game.ammo = 99;
    game.actors.list.retain(|a| a.kind == kind || a.dead);

    let (bx, by) = game.actors.find_pos(kind).expect("boss should be present");
    let placed = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
        .into_iter()
        .find(|(ox, oy)| !game.world.wall_at((bx + ox) as i32, (by + oy) as i32));
    let (ox, oy) = placed.expect("an open tile beside the boss");
    game.player.x = bx + ox;
    game.player.y = by + oy;

    for _ in 0..(30.0 / DT) as u32 {
        if game.screen != GameScreen::Playing {
            return;
        }
        // Aim straight at the boss each tic (point blank, line of sight clear).
        let Some((tx, ty)) = game
            .actors
            .list
            .iter()
            .find(|a| a.kind == kind && !a.dead)
            .map(|a| (a.x, a.y))
        else {
            return;
        };
        game.player.angle = (ty - game.player.y).atan2(tx - game.player.x);
        game.update(DT, &Input { fire: true, ..Default::default() });
    }
    panic!("{kind:?} survived 30s of point-blank chaingun fire");
}

// --- High-score table math + file roundtrip ---------------------------------

#[test]
fn high_score_table_sorts_qualifies_and_roundtrips() {
    let (_guard, _dir) = unique_save_dir();

    let table = highscore::default_table();
    assert_eq!(table.len(), highscore::MAX_SCORES);
    assert!(table.iter().all(|e| e.score == 10000 && e.completed == 1));

    // A higher score beats the whole board.
    let mut t = table.clone();
    assert_eq!(highscore::insert(&mut t, 50000, 5), Some(0));
    assert_eq!(t.len(), highscore::MAX_SCORES);
    assert_eq!(t[0].score, 50000);
    assert_eq!(t[0].name, "", "the fresh slot starts blank for name entry");

    // An equal score reaching a further level also qualifies (tiebreak).
    let mut t2 = highscore::default_table();
    assert_eq!(highscore::insert(&mut t2, 10000, 2), Some(0));
    // A strictly-lower score does not.
    let mut t3 = highscore::default_table();
    assert_eq!(highscore::insert(&mut t3, 100, 9), None);

    // Name the inserted top entry and persist it; a fresh load reads it back
    // sorted high-to-low.
    t[0].name = "B.J.".to_string();
    highscore::store(&t).expect("store high scores");
    let loaded = highscore::load();
    assert_eq!(loaded.len(), highscore::MAX_SCORES);
    assert_eq!(loaded[0], HighScore { name: "B.J.".to_string(), score: 50000, completed: 5 });
    // The 10000s remain below the 50000, still sorted.
    assert!(loaded[1].score <= loaded[0].score);
}

// --- Full victory state-machine walk ----------------------------------------

#[test]
fn killing_hans_runs_deathcam_victory_endtext_and_highscore_entry() {
    let (_guard, _dir) = unique_save_dir();

    // E1M9 (level index 8): Hans Grosse is the episode-1 end boss.
    let mut game = Game::new(8);
    assert_eq!(wolf3d::game::end_boss(8), Some(Kind::Hans));

    kill_end_boss(&mut game, Kind::Hans);

    // Killing the boss sets victory and swings into the deathcam.
    assert!(game.victory, "killing Hans should win the episode");
    assert_eq!(game.screen, GameScreen::DeathCam);

    // Any key skips the deathcam to the "YOU WIN!" screen.
    game.update(DT, &any());
    assert_eq!(game.screen, GameScreen::Victory);

    // Any key advances to the end-of-episode article.
    game.update(DT, &any());
    assert_eq!(game.screen, GameScreen::EndText);

    // Force a qualifying score so the run reaches the name-entry screen.
    game.score = 1_000_000;

    // Page through the article; the last page hands off to the high-score check.
    let mut guard = 0;
    while game.screen == GameScreen::EndText {
        game.update(DT, &any());
        guard += 1;
        assert!(guard < 50, "end article never finished paging");
    }
    assert_eq!(game.screen, GameScreen::HighScoreEntry, "a top score enters a name");

    // Type a name and confirm it.
    for c in "BJ BLAZKOWICZ".chars() {
        game.update(DT, &Input { typed: Some(c), ..Default::default() });
    }
    game.update(DT, &enter());
    assert_eq!(game.screen, GameScreen::HighScores);
    assert!(
        game.highscores.iter().any(|e| e.name == "BJ BLAZKOWICZ" && e.score == 1_000_000),
        "the entered name and score land on the board"
    );

    // Persisted to disk (a fresh load sees the same top entry).
    let reloaded = highscore::load();
    assert_eq!(reloaded[0].name, "BJ BLAZKOWICZ");
    assert_eq!(reloaded[0].score, 1_000_000);

    // Dismissing the board after a victory returns to the title.
    game.update(DT, &any());
    assert_eq!(game.screen, GameScreen::Title);
    assert!(!game.victory, "the victory flag clears on the way back to the title");
}

// --- Read This! help ---------------------------------------------------------

#[test]
fn read_this_opens_pages_and_escapes() {
    let mut game = Game::new(0);
    game.screen = GameScreen::MainMenu;
    game.started = false;
    game.main_sel = wolf3d::menu::ITEM_READ;

    game.update(DT, &enter());
    assert_eq!(game.screen, GameScreen::ReadThis);

    // Page forward a couple of times (the help article is many pages).
    game.update(DT, &any());
    assert_eq!(game.screen, GameScreen::ReadThis, "still paging the help");
    game.update(DT, &any());
    assert_eq!(game.screen, GameScreen::ReadThis);

    // Esc backs out to the menu with the cursor on Read This.
    game.update(DT, &esc());
    assert_eq!(game.screen, GameScreen::MainMenu);
    assert_eq!(game.main_sel, wolf3d::menu::ITEM_READ);
}

// --- Snapshot generation (ignored; run with WOLF3D_SNAP_DIR set) -------------

#[test]
#[ignore]
fn generate_snapshots() {
    let (_guard, _dir) = unique_save_dir();
    let out = std::env::var("WOLF3D_SNAP_DIR").unwrap_or_else(|_| "snaps".into());
    std::fs::create_dir_all(&out).expect("create snapshot dir");
    let mut fb = Framebuffer::new();
    let snap = |game: &mut Game, fb: &mut Framebuffer, name: &str| {
        let saved = game.actors.save_visible();
        game.render(fb);
        game.actors.restore_visible(&saved);
        write_ppm(fb, &Path::new(&out).join(format!("{name}.ppm")));
    };

    let mut game = Game::new(8);
    kill_end_boss(&mut game, Kind::Hans);
    assert_eq!(game.screen, GameScreen::DeathCam);
    // Let the corpse fall a moment for a more cinematic frame.
    for _ in 0..40 {
        game.update(DT, &Input::default());
    }
    snap(&mut game, &mut fb, "deathcam");

    game.update(DT, &any());
    snap(&mut game, &mut fb, "youwin");

    game.update(DT, &any());
    game.score = 1_000_000;
    snap(&mut game, &mut fb, "endtext");

    while game.screen == GameScreen::EndText {
        game.update(DT, &any());
    }
    for c in "BJ BLAZKOWICZ".chars() {
        game.update(DT, &Input { typed: Some(c), ..Default::default() });
    }
    snap(&mut game, &mut fb, "highscore_entry");
    game.update(DT, &enter());
    snap(&mut game, &mut fb, "highscores");

    // Read This! page 1.
    let mut help = Game::new(0);
    help.screen = GameScreen::MainMenu;
    help.main_sel = wolf3d::menu::ITEM_READ;
    help.update(DT, &enter());
    snap(&mut help, &mut fb, "readthis");
}

fn write_ppm(fb: &Framebuffer, path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create ppm"));
    write!(f, "P6\n{WIDTH} {HEIGHT}\n255\n").unwrap();
    for &px in &fb.pixels {
        f.write_all(&px.to_le_bytes()[..3]).unwrap();
    }
}

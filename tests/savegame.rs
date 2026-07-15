//! Save-game milestone checks (requires `data/`): the mid-fight determinism
//! roundtrip (the headline guarantee — a load resumes bit-identically), the
//! slot-header parse, and empty-slot handling. Nothing here opens a window or
//! audio device.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use wolf3d::game::{Difficulty, Game, GameScreen, Input};
use wolf3d::savegame;

const DT: f32 = 1.0 / 70.0;

/// `WOLF3D_SAVE_DIR` is process-global, so the tests in this binary must not
/// touch it concurrently. Each test holds this guard for its whole body.
static SAVE_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Point `WOLF3D_SAVE_DIR` at a fresh temp directory (so tests never touch the
/// crate's real `saves/`), returning the lock guard that must be held until the
/// test finishes writing/reading slots.
fn unique_save_dir() -> (MutexGuard<'static, ()>, std::path::PathBuf) {
    let guard = SAVE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wolf3d-savetest-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp save dir");
    unsafe {
        std::env::set_var("WOLF3D_SAVE_DIR", &dir);
    }
    (guard, dir)
}

/// Drive a firefight on E1M1: point the player at the guards and hold fire. The
/// enemies react, move and shoot back, so the world state churns every tic.
fn run_fight(game: &mut Game, tics: usize) {
    let fire = Input {
        fire: true,
        ..Default::default()
    };
    for _ in 0..tics {
        // Aim at the nearest live enemy each tic so shots actually land and the
        // fight stays lively (mirrors the demo's aimfire helper).
        let (px, py) = (game.player.x, game.player.y);
        if let Some((_, bx, by)) = game
            .actors
            .list
            .iter()
            .filter(|a| !a.dead)
            .map(|a| ((a.x - px).powi(2) + (a.y - py).powi(2), a.x, a.y))
            .min_by(|a, b| a.0.total_cmp(&b.0))
        {
            game.player.angle = (by - py).atan2(bx - px);
        }
        game.update(DT, &fire);
        let _ = game.take_sounds();
    }
}

/// A comparable fingerprint of the live simulation: player pose, stats, and
/// every actor's position/health/state.
fn fingerprint(game: &Game) -> Vec<i64> {
    let mut v = Vec::new();
    let q = |f: f32| (f * 4096.0).round() as i64; // fixed-point compare
    v.push(q(game.player.x));
    v.push(q(game.player.y));
    v.push(q(game.player.angle));
    v.push(game.health as i64);
    v.push(game.ammo as i64);
    v.push(game.score as i64);
    v.push(game.actors.list.len() as i64);
    for a in &game.actors.list {
        v.push(q(a.x));
        v.push(q(a.y));
        v.push(a.health() as i64);
        v.push(a.dead as i64);
    }
    for p in &game.actors.projectiles {
        v.push(q(p.x));
        v.push(q(p.y));
    }
    v
}

/// Save at tic T during a firefight, run N more tics, and compare against
/// load-then-run-N. A faithful save resumes the simulation bit-identically.
#[test]
fn midfight_roundtrip_is_deterministic() {
    let _guard = unique_save_dir();

    // Base run: fight up to the save point on E1M1 at hard skill.
    let mut base = Game::new(0);
    base.start_new_game(0, Difficulty::Hard);
    run_fight(&mut base, 120);

    // Save the mid-fight state to slot 0.
    base.save_to_slot(0, "midfight").expect("save");
    let saved_fp = fingerprint(&base);

    // Continue the base run N more tics.
    const N: usize = 90;
    run_fight(&mut base, N);
    let base_after = fingerprint(&base);

    // Fresh game, load the slot: it must resume exactly where we saved.
    let mut loaded = Game::new(0);
    loaded.load_from_slot(0).expect("load");
    assert_eq!(loaded.screen, GameScreen::Playing, "load resumes play");
    assert_eq!(
        fingerprint(&loaded),
        saved_fp,
        "loaded state must equal the state at save time"
    );

    // Run the same N tics; the two runs must stay bit-identical.
    run_fight(&mut loaded, N);
    assert_eq!(
        fingerprint(&loaded),
        base_after,
        "load-then-run-N diverged from run-N: save is not deterministic"
    );
}

/// The slot header parses back the name, level and difficulty written to it.
#[test]
fn slot_header_parses() {
    let _guard = unique_save_dir();

    let mut game = Game::new(0);
    game.start_new_game(0, Difficulty::Normal);
    // Advance a couple of floors so the stored level index is non-trivial.
    game.next_level();
    game.next_level();
    let level = game.level_idx;
    game.save_to_slot(3, "My Slot Name").expect("save");

    let header = savegame::read_slot_header(3).expect("slot 3 has a header");
    assert_eq!(header.name, "My Slot Name");
    assert_eq!(header.level_idx, level);
    assert_eq!(header.difficulty, Difficulty::Normal.skill());

    // The in-memory write/read round-trips the same way.
    let bytes = game.write_save("Direct");
    let mut r = savegame::Reader::new(&bytes);
    let h2 = savegame::read_header(&mut r).expect("header");
    assert_eq!(h2.name, "Direct");
    assert_eq!(h2.level_idx, level);
}

/// An empty slot reads as `None` and never errors; a bad magic is rejected.
#[test]
fn empty_and_corrupt_slots() {
    let (_guard, dir) = unique_save_dir();

    // Nothing written yet: every slot is empty.
    for slot in 0..savegame::NUM_SLOTS {
        assert!(
            savegame::read_slot_header(slot).is_none(),
            "slot {slot} should be empty"
        );
    }

    // A file that is not a save is treated as an empty slot, not a crash.
    std::fs::write(savegame::slot_path(2), b"not a save file at all").expect("write junk");
    assert!(
        savegame::read_slot_header(2).is_none(),
        "corrupt slot reads as empty"
    );

    // Applying corrupt bytes to a game surfaces an error and leaves it intact.
    let mut game = Game::new(0);
    let before_level = game.level_idx;
    assert!(
        game.apply_save(b"WOLF3DSVxx").is_err(),
        "bad data must error"
    );
    assert_eq!(
        game.level_idx, before_level,
        "a failed load must not mutate the game"
    );

    // One real save makes exactly that slot non-empty.
    game.save_to_slot(5, "Only Five").expect("save");
    assert!(savegame::read_slot_header(5).is_some());
    assert!(savegame::read_slot_header(4).is_none());

    let _ = dir; // keep the temp dir alive for the test's duration
}

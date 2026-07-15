//! Headless gameplay checks (requires `data/`): the same Game the window
//! runs, driven by synthetic input at the original 70 Hz tic rate.

use wolf3d::game::{Game, Input};

const DT: f32 = 1.0 / 70.0;

fn step(game: &mut Game, input: &Input, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, input);
    }
}

/// E1M1: spawn faces east, a door sits at x=32. Walk up, open, walk through.
#[test]
fn walk_through_first_door() {
    let mut game = Game::new(0);
    assert_eq!(game.player.x, 29.5);
    assert_eq!(game.player.y, 57.5);

    let forward = Input {
        forward: true,
        ..Default::default()
    };

    // Blocked by the closed door: walking never crosses x=32.
    step(&mut game, &forward, 2.0);
    assert!(game.player.x < 32.0, "walked through a closed door");

    // Open it (facing the adjacent tile) and it becomes passable.
    game.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    step(&mut game, &Input::default(), 1.2);
    step(&mut game, &forward, 1.0);
    assert!(game.player.x > 33.0, "door did not open");

    // The door auto-closes behind us after its hold time.
    step(&mut game, &Input::default(), 6.0);
    assert!(
        game.world.doors.iter().all(|d| d.position == 0.0),
        "doors should have auto-closed"
    );
}

/// The 6-key level-select grid: navigation moves episode/floor, Enter warps
/// with stats intact, Esc resumes without warping.
#[test]
fn level_select_warps_and_preserves_stats() {
    use wolf3d::game::GameScreen;

    let mut game = Game::new(0);
    game.score = 4200;
    game.level_sel = game.level_idx;
    game.screen = GameScreen::LevelSelect;

    // Esc backs out without changing the level.
    game.update(
        DT,
        &Input {
            menu_back: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::Playing);
    assert_eq!(game.level_idx, 0);

    // Right one episode, down one floor, Enter: warp to E2 floor 2 (index 11).
    game.screen = GameScreen::LevelSelect;
    game.update(
        DT,
        &Input {
            menu_right: true,
            ..Default::default()
        },
    );
    game.update(
        DT,
        &Input {
            menu_down: true,
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
    assert_eq!(game.screen, GameScreen::Playing);
    assert_eq!(game.level_idx, 11);
    assert_eq!(game.score, 4200, "warp must keep stats");
    assert_eq!(game.keys, 0, "warp resets keys like any floor change");
}

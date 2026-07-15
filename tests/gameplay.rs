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

    let forward = Input { forward: true, ..Default::default() };

    // Blocked by the closed door: walking never crosses x=32.
    step(&mut game, &forward, 2.0);
    assert!(game.player.x < 32.0, "walked through a closed door");

    // Open it (facing the adjacent tile) and it becomes passable.
    game.update(DT, &Input { use_door: true, ..Default::default() });
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

//! Headless combat checks on E1M1 (requires `data/`). Spawns all difficulties
//! (hardest skill). The map: player spawns at (29.5, 57.5) facing east; a
//! guard stands at about (39.5, 61.5). Tests place the player into line of
//! fire / point-blank range rather than relying on timed walks (speeds now
//! match original MOVESCALE and change with skill-independent constants).

use wolf3d::actors::Kind;
use wolf3d::game::{Game, Input};

const DT: f32 = 1.0 / 70.0;

fn hold(game: &mut Game, input: &Input, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, input);
    }
}

/// Place the player a few tiles west of the nearest live guard, facing east.
/// Returns the guard's index in `actors.list`.
fn approach_guard(game: &mut Game) -> usize {
    let idx = game
        .actors
        .list
        .iter()
        .position(|a| !a.dead && a.kind == Kind::Guard)
        .expect("E1M1 should spawn at least one guard");
    let (gx, gy) = (game.actors.list[idx].x, game.actors.list[idx].y);
    // Same tile row, a short open shot west of the guard (stays on floor tile).
    game.player.x = gx - 0.55;
    game.player.y = gy;
    game.player.angle = 0.0; // face east
    // One idle tic so SightPlayer can arm.
    game.update(DT, &Input::default());
    idx
}

/// (a) Standing exposed in the guard room draws fire: health drops with no
/// player action of any kind.
#[test]
fn guard_shoots_exposed_player() {
    let mut game = Game::new(0);
    assert_eq!(game.health, 100);
    let _ = approach_guard(&mut game);

    // Stand still and take fire for a few seconds.
    hold(&mut game, &Input::default(), 3.0);

    assert!(
        game.health < 100 || game.lives < 3 || game.died,
        "player was never shot while exposed (health={}, lives={})",
        game.health,
        game.lives,
    );
}

/// (b) Shooting the guard kills it: score rises and the corpse stops blocking.
#[test]
fn pistol_kills_guard() {
    let mut game = Game::new(0);
    game.god = true;
    let idx = approach_guard(&mut game);
    assert_eq!(game.score, 0);
    let (gx0, gy0) = (game.actors.list[idx].x, game.actors.list[idx].y);

    // Empty most of a magazine into the guard (point-blank).
    hold(
        &mut game,
        &Input {
            fire: true,
            ..Default::default()
        },
        3.0,
    );
    hold(&mut game, &Input::default(), 0.5);

    assert!(
        game.score >= 100,
        "killing the guard should score >= 100, got {}",
        game.score
    );
    assert!(
        game.actors.list[idx].dead,
        "the approached guard should be dead"
    );
    let (gx, gy) = (gx0.floor() as usize, gy0.floor() as usize);

    // One more tic so the actor system republishes blocking tiles.
    game.update(DT, &Input::default());
    assert!(
        !game.world.actor_blocked[gy * 64 + gx],
        "a corpse must not block movement",
    );
}

/// (c) Ammo decrements one per shot and never goes negative, stopping at 0.
#[test]
fn ammo_decrements_and_stops_at_zero() {
    let mut game = Game::new(0);
    assert_eq!(game.ammo, 8);

    // A single tap fires exactly one pistol shot.
    game.update(
        DT,
        &Input {
            fire: true,
            ..Default::default()
        },
    );
    hold(&mut game, &Input::default(), 0.4);
    assert_eq!(game.ammo, 7, "one shot should spend one round");

    // Hold fire well past the magazine; ammo bottoms out at 0.
    for _ in 0..600 {
        game.update(
            DT,
            &Input {
                fire: true,
                ..Default::default()
            },
        );
        assert!(game.ammo >= 0, "ammo went negative");
    }
    assert_eq!(game.ammo, 0, "ammo should stop at zero");
}

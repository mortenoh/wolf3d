//! Headless combat checks on E1M1 (requires `data/`). Spawns all difficulties
//! (hardest skill). The map: player spawns at (29.5, 57.5) facing east; the
//! first door is at x=32; through it and down the room's south exit a guard
//! stands at (39, 61) facing west. The scripted route below walks into his line
//! of fire, the same way the snapshot demos do.

use wolf3d::actors::Kind;
use wolf3d::game::{Game, Input, TURN_SPEED};

const DT: f32 = 1.0 / 70.0;

fn hold(game: &mut Game, input: &Input, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, input);
    }
}

fn turn(game: &mut Game, right: bool, degrees: f32) {
    let input = Input { turn_left: !right, turn_right: right, ..Default::default() };
    hold(game, &input, degrees.to_radians() / TURN_SPEED);
}

/// Walk from the spawn, through the first door, and down to face the guard.
fn approach_guard(game: &mut Game) {
    let fwd = Input { forward: true, ..Default::default() };
    hold(game, &fwd, 0.75); // up to the closed door
    game.update(DT, &Input { use_door: true, ..Default::default() });
    hold(game, &Input::default(), 1.2); // let it open
    hold(game, &fwd, 1.3); // into the room
    turn(game, true, 90.0); // face south
    hold(game, &fwd, 1.1); // down to the south wall
    turn(game, false, 90.0); // face east
    hold(game, &fwd, 0.6); // up to the guard
}

/// (a) Standing exposed in the guard room draws fire: health drops with no
/// player action of any kind.
#[test]
fn guard_shoots_exposed_player() {
    let mut game = Game::new(0);
    assert_eq!(game.health, 100);
    approach_guard(&mut game);

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
    approach_guard(&mut game);
    assert_eq!(game.score, 0);

    // Empty most of a magazine into the guard.
    hold(&mut game, &Input { fire: true, ..Default::default() }, 2.0);
    hold(&mut game, &Input::default(), 0.5);

    assert!(game.score >= 100, "killing the guard should score >= 100, got {}", game.score);

    let dead_guard = game
        .actors
        .list
        .iter()
        .find(|a| a.kind == Kind::Guard && a.dead && (a.x - 39.5).abs() < 2.0 && (a.y - 61.5).abs() < 2.0)
        .expect("the guard should be dead");
    let (gx, gy) = (dead_guard.x.floor() as usize, dead_guard.y.floor() as usize);

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
    game.update(DT, &Input { fire: true, ..Default::default() });
    hold(&mut game, &Input::default(), 0.4);
    assert_eq!(game.ammo, 7, "one shot should spend one round");

    // Hold fire well past the magazine; ammo bottoms out at 0.
    for _ in 0..600 {
        game.update(DT, &Input { fire: true, ..Default::default() });
        assert!(game.ammo >= 0, "ammo went negative");
    }
    assert_eq!(game.ammo, 0, "ammo should stop at zero");
}

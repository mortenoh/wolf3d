//! Headless pickups / keys / level-progression checks on real .WL6 data
//! (requires `data/`). Coordinates are located by scanning the loaded level
//! rather than hard-coded, so the tests survive map-index changes.

use wolf3d::game::{Game, Input};
use wolf3d::hud::{KEY_GOLD, KEY_SILVER};
use wolf3d::raycast::Bonus;

const DT: f32 = 1.0 / 70.0;
const N: usize = 64;

fn hold(game: &mut Game, input: &Input, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, input);
    }
}

/// A tile is walkable if it is neither a wall (1..=89) nor a door (90..=101).
fn walkable(game: &Game, x: usize, y: usize) -> bool {
    !(1..=101).contains(&game.world.level.plane0[y * N + x])
}

fn has_walkable_neighbor(game: &Game, x: usize, y: usize) -> bool {
    [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| {
        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
        nx >= 0 && ny >= 0 && (nx as usize) < N && (ny as usize) < N && walkable(game, nx as usize, ny as usize)
    })
}

/// First plane-0 tile matching `pred` that also has a walkable neighbor to
/// stand on. Returns (x, y, tile).
fn find_tile(game: &Game, pred: impl Fn(u16) -> bool) -> (usize, usize, u16) {
    for y in 1..N - 1 {
        for x in 1..N - 1 {
            let c = game.world.level.plane0[y * N + x];
            if pred(c) && has_walkable_neighbor(game, x, y) {
                return (x, y, c);
            }
        }
    }
    panic!("no matching tile with a walkable neighbor");
}

/// Stand the player on a walkable cardinal neighbor of (tx, ty), facing it.
fn place_facing(game: &mut Game, tx: usize, ty: usize) {
    use std::f32::consts::{FRAC_PI_2, PI};
    // (neighbor dx, dy, facing angle toward the target tile)
    let opts = [
        (1i32, 0i32, PI),          // east of target -> face west
        (-1, 0, 0.0),              // west of target -> face east
        (0, 1, -FRAC_PI_2),        // south of target -> face north
        (0, -1, FRAC_PI_2),        // north of target -> face south
    ];
    for (dx, dy, angle) in opts {
        let (nx, ny) = (tx as i32 + dx, ty as i32 + dy);
        if nx >= 0 && ny >= 0 && (nx as usize) < N && (ny as usize) < N && walkable(game, nx as usize, ny as usize) {
            game.player.x = nx as f32 + 0.5;
            game.player.y = ny as f32 + 0.5;
            game.player.angle = angle;
            return;
        }
    }
    panic!("no walkable neighbor to face tile ({tx},{ty})");
}

/// (a) Standing on an ammo clip raises ammo by 8 and removes the static.
#[test]
fn ammo_clip_pickup_raises_ammo_and_removes_static() {
    let mut game = Game::new(0);
    game.actors.list.clear(); // deterministic: no enemies firing / moving

    let (i, cx, cy) = game
        .world
        .statics
        .iter()
        .enumerate()
        .find_map(|(i, s)| (s.bonus == Some(Bonus::Clip)).then_some((i, s.x, s.y)))
        .expect("E1M1 has an ammo clip pickup");

    // Teleport directly onto the clip and let GetBonus fire.
    game.player.x = cx;
    game.player.y = cy;
    let before = game.ammo;
    assert!(before < 99);

    game.update(DT, &Input::default());

    assert_eq!(game.ammo, (before + 8).min(99), "a clip should add 8 ammo");
    assert!(game.world.statics[i].picked, "the collected clip must stop rendering");
}

/// (b) A locked door refuses to open without its key and opens once held.
#[test]
fn locked_door_needs_its_key() {
    // Wolf1 Map2 (level index 1) has a single gold-locked door.
    let mut game = Game::new(1);
    game.actors.list.clear(); // no enemy can auto-open the door

    let (dx, dy, code) = find_tile(&game, |c| (92..=99).contains(&c));
    let required = match code {
        92 | 93 => KEY_GOLD,
        94 | 95 => KEY_SILVER,
        96 | 97 => 4,
        _ => 8,
    };
    let di = game
        .world
        .doors
        .iter()
        .position(|d| d.x as usize == dx && d.y as usize == dy)
        .expect("locked door registered");

    place_facing(&mut game, dx, dy);

    // Without the key: use has no effect, the door stays shut.
    game.keys = 0;
    game.update(DT, &Input { use_door: true, ..Default::default() });
    hold(&mut game, &Input::default(), 1.5);
    assert_eq!(
        game.world.doors[di].position, 0.0,
        "a locked door opened without the key"
    );

    // With the key: use opens it fully.
    game.keys = required;
    game.update(DT, &Input { use_door: true, ..Default::default() });
    hold(&mut game, &Input::default(), 1.5);
    assert!(
        game.world.doors[di].position > 0.9,
        "the door should open once the key is held (pos={})",
        game.world.doors[di].position
    );
}

/// (c) Using the elevator switch advances the floor and preserves score/lives,
/// while keys reset for the new floor.
#[test]
fn elevator_advances_level_and_preserves_score() {
    let mut game = Game::new(0);
    game.actors.list.clear();

    let (ex, ey, _) = find_tile(&game, |c| c == 21); // ELEVATORTILE
    place_facing(&mut game, ex, ey);

    game.score = 1234;
    game.lives = 2;
    game.keys = KEY_GOLD; // proves keys reset on the next floor
    let before = game.level_idx;

    game.update(DT, &Input { use_door: true, ..Default::default() });

    assert_eq!(game.level_idx, before + 1, "elevator should advance the floor");
    assert_eq!(game.score, 1234, "score must carry across floors");
    assert_eq!(game.lives, 2, "lives must carry across floors");
    assert_eq!(game.keys, 0, "keys reset on the new floor");
}

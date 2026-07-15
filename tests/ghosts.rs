//! E3M10 (level index 29, "Wolf3 Secret" — the Pac-Man homage floor) ghosts.
//!
//! Fidelity checks distilled from WOLFSRC (SpawnGhosts / T_Ghosts / MoveObj):
//! - Blinky/Clyde/Pinky/Inky spawn from plane-1 codes 224..=227.
//! - They chase through the maze respecting walls (TryWalk uses CHECKSIDE with
//!   no ghost exception — ghosts do NOT phase through walls).
//! - They cannot be shot (no FL_SHOOTABLE flag).
//! - They damage the player on contact (MoveObj: `TakeDamage(tics*2)`).
//! - They are excluded from the floor's kill total (never killable).
//!
//! Deterministic: the AI runs off the US_RndT table seeded at 0 and the tests
//! step fixed 1/70 s tics.

use wolf3d::game::{Game, Input};

const DT: f32 = 1.0 / 70.0;
const E3M10: usize = 29;

/// (a) All four ghosts spawn on E3M10 from codes 224..=227.
#[test]
fn e3m10_spawns_four_ghosts() {
    let game = Game::new(E3M10);
    assert_eq!(game.world.level.name, "Wolf3 Secret");

    // One spawn code of each kind sits in plane 1.
    for code in 224u16..=227 {
        assert_eq!(
            game.world.level.plane1.iter().filter(|&&c| c == code).count(),
            1,
            "E3M10 plane 1 should hold exactly one ghost spawn code {code}"
        );
    }
    // And a live, ghost actor stands for each.
    let ghosts = game.actors.list.iter().filter(|a| a.kind.is_ghost() && !a.dead).count();
    assert_eq!(ghosts, 4, "four ghost actors should spawn on E3M10");
}

/// (b) Ghosts are not counted in the floor's kill total (they can never be
/// killed, so counting them would cap the kill ratio below 100%).
#[test]
fn ghosts_excluded_from_kill_total() {
    let game = Game::new(E3M10);
    let ghosts = game.actors.list.iter().filter(|a| a.kind.is_ghost()).count();
    let non_ghost_live =
        game.actors.list.iter().filter(|a| !a.dead && !a.kind.is_ghost()).count() as i32;
    assert!(ghosts > 0, "there must be ghosts to exclude");
    assert_eq!(
        game.stats.kill_total, non_ghost_live,
        "kill total must exclude the {ghosts} ghosts"
    );
}

/// (c) Ghosts respect walls — over a long run no ghost ever sits on a wall tile.
/// (Verified from source: TryWalk uses CHECKSIDE for ghostobj, so ghosts chase
/// through the maze corridors rather than floating through walls.)
#[test]
fn ghosts_never_enter_walls() {
    let mut game = Game::new(E3M10);
    for _ in 0..(4.0 / DT) as u32 {
        game.update(DT, &Input::default());
        for a in game.actors.list.iter().filter(|a| a.kind.is_ghost()) {
            let (tx, ty) = (a.x.floor() as i32, a.y.floor() as i32);
            assert!(
                !game.world.wall_at(tx, ty),
                "a ghost entered a wall tile at ({tx},{ty}) — ghosts must respect walls"
            );
        }
    }
}

/// (d) A ghost cannot be shot: firing straight at one point-blank scores nothing
/// and leaves it alive.
#[test]
fn ghost_cannot_be_shot() {
    let mut game = Game::new(E3M10);
    // Isolate a single ghost so it is unambiguously the nearest target.
    game.actors.list.retain(|a| a.kind.is_ghost());
    let (gx, gy) = (game.actors.list[0].x, game.actors.list[0].y);

    // Stand a tile west of it, facing east straight at it.
    game.player.x = gx - 1.0;
    game.player.y = gy;
    game.player.angle = 0.0;

    let points = game.actors.player_fire(&game.world, game.player.x, game.player.y, 0.0, false);
    assert_eq!(points, 0, "shooting a ghost must score nothing");
    assert!(!game.actors.list[0].dead, "a ghost can never be killed");
}

/// (e) A ghost hurts the player on contact (MoveObj TakeDamage(tics*2)), then
/// keeps moving. Isolate the ghosts, drop the player onto an open tile next to
/// one, and watch health fall.
#[test]
fn ghost_damages_player_on_contact() {
    let mut game = Game::new(E3M10);
    game.actors.list.retain(|a| a.kind.is_ghost());

    // Place the player on an open tile orthogonally adjacent to a ghost so it
    // is reachable and within contact range as the ghost closes in.
    let (gx, gy) = (game.actors.list[0].x, game.actors.list[0].y);
    let (gtx, gty) = (gx.floor() as i32, gy.floor() as i32);
    let mut placed = false;
    // Prefer a non-west neighbor: a ghost spawns facing east, and SelectChaseDir
    // forbids an immediate turnaround, so a westward chase would start by backing
    // away. East/north/south let it close on the player straight away.
    for (dx, dy) in [(1, 0), (0, -1), (0, 1), (-1, 0)] {
        let (nx, ny) = (gtx + dx, gty + dy);
        if !game.world.wall_at(nx, ny) && game.world.door_lookup(nx, ny).is_none() {
            game.player.x = nx as f32 + 0.5;
            game.player.y = ny as f32 + 0.5;
            placed = true;
            break;
        }
    }
    assert!(placed, "expected an open tile next to a ghost");

    let start = game.health;
    let mut damaged = false;
    for _ in 0..(4.0 / DT) as u32 {
        game.update(DT, &Input::default());
        // Break on the first hit — otherwise sustained contact could kill and
        // respawn the player back to full health, masking the damage.
        if game.health < start {
            damaged = true;
            break;
        }
    }
    assert!(damaged, "a ghost in contact must drain the player's health");
}

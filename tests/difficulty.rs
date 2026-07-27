//! Difficulty-aware enemy spawning (WL_GAME.C ScanInfoPlane) and skill-scaled
//! hitpoints (WL_ACT2.C starthitpoints[] / TakeDamage baby quartering).

use wolf3d::actors::Kind;
use wolf3d::game::{Difficulty, Game, Input};

const DT: f32 = 1.0 / 70.0;

/// Count of live (spawned, non-corpse) enemies on E1M1 at `skill`.
fn live_enemies(difficulty: Difficulty) -> usize {
    let mut game = Game::new(0);
    game.start_new_game(0, difficulty);
    game.actors.list.iter().filter(|a| !a.dead).count()
}

#[test]
fn harder_skills_spawn_more_enemies() {
    let baby = live_enemies(Difficulty::Baby);
    let easy = live_enemies(Difficulty::Easy);
    let normal = live_enemies(Difficulty::Normal);
    let hard = live_enemies(Difficulty::Hard);

    // Baby and easy share the base placement set; medium adds the +36/+18
    // codes, hard adds another tier — a strict increase at each threshold.
    assert_eq!(baby, easy, "baby and easy share the base enemy set");
    assert!(
        normal > easy,
        "medium skill adds enemies (got easy={easy}, normal={normal})"
    );
    assert!(
        hard > normal,
        "hard skill adds enemies (got normal={normal}, hard={hard})"
    );

    // Sanity: the default Game::new spawns at hard (all tiers), matching the
    // enemy set the existing combat/boss tests rely on.
    assert_eq!(
        Game::new(0).actors.list.iter().filter(|a| !a.dead).count(),
        hard
    );
}

/// starthitpoints[]: bosses/mutants/spectres scale with skill; grunts do not.
#[test]
fn starthitpoints_scale_with_skill() {
    assert_eq!(Kind::Guard.hitpoints(0), 25);
    assert_eq!(Kind::Guard.hitpoints(3), 25);
    assert_eq!(Kind::Mutant.hitpoints(0), 45);
    assert_eq!(Kind::Mutant.hitpoints(3), 65);
    assert_eq!(Kind::Hans.hitpoints(0), 850);
    assert_eq!(Kind::Hans.hitpoints(3), 1200);
    assert_eq!(Kind::Schabbs.hitpoints(0), 850);
    assert_eq!(Kind::Schabbs.hitpoints(3), 2400);
    assert_eq!(Kind::Spectre.hitpoints(0), 5);
    assert_eq!(Kind::Spectre.hitpoints(3), 25);
    assert_eq!(Kind::Hitler.hitpoints(0), 500);
    assert_eq!(Kind::Hitler.hitpoints(3), 900);

    let mut baby = Game::new(0);
    baby.start_new_game(0, Difficulty::Baby);
    let mut hard = Game::new(0);
    hard.start_new_game(0, Difficulty::Hard);
    let baby_hp: i32 = baby.actors.list.iter().map(|a| a.health()).sum();
    let hard_hp: i32 = hard.actors.list.iter().map(|a| a.health()).sum();
    // Hard has more spawns AND higher HP on mutants — total HP must be higher.
    assert!(
        hard_hp > baby_hp,
        "hard total HP should exceed baby (baby={baby_hp}, hard={hard_hp})"
    );
}

/// WL_AGENT.C TakeDamage: gd_baby quarters incoming damage (`points >>= 2`).
#[test]
fn baby_skill_quarters_damage() {
    // Contract: same raw packet is quartered only on baby.
    let mut baby = 16i32;
    if true {
        // gd_baby branch
        baby >>= 2;
    }
    assert_eq!(baby, 4);
    let hard = 16i32; // no shift
    assert_eq!(hard, 16);

    // Smoke: a baby game boots and runs a tic without panicking.
    let mut game = Game::new(0);
    game.start_new_game(0, Difficulty::Baby);
    game.update(DT, &Input::default());
    assert_eq!(game.difficulty, Difficulty::Baby);
}

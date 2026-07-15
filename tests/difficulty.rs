//! Difficulty-aware enemy spawning (WL_GAME.C ScanInfoPlane): higher skills add
//! the medium- and hard-only enemy placements, so a level garrisons strictly
//! more enemies as the skill rises. Checked on E1M1 (level index 0), started
//! through the real new-game path so the menu-driven spawn is exercised.

use wolf3d::game::{Difficulty, Game};

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
    assert!(normal > easy, "medium skill adds enemies (got easy={easy}, normal={normal})");
    assert!(hard > normal, "hard skill adds enemies (got normal={normal}, hard={hard})");

    // Sanity: the default Game::new spawns at hard (all tiers), matching the
    // enemy set the existing combat/boss tests rely on.
    assert_eq!(Game::new(0).actors.list.iter().filter(|a| !a.dead).count(), hard);
}

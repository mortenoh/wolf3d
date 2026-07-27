//! In-game autopilot (`I` key): fair hierarchical AI from [`crate::ai`].
//!
//! Assist mode grants temporary god / ammo / keys so a mid-floor toggle is not
//! stranded, but movement and combat still come from the real AI brain (no
//! elevator teleports).

use crate::ai::{Brain, Policy};
use crate::game::{Game, GameScreen, Input};
use crate::hud::{KEY_GOLD, KEY_SILVER};

/// One active take-over session.
pub struct Autopilot {
    brain: Brain,
    saved_god: bool,
    saved_infinite: bool,
    saved_keys: u8,
    saved_weapon: usize,
    saved_ammo: i32,
}

impl Autopilot {
    /// Plan a route for the current floor. Always succeeds with a brain (boss
    /// or elevator); returns `None` only if the game is not in play.
    pub fn start(game: &Game) -> Option<Self> {
        if game.screen != GameScreen::Playing {
            return None;
        }
        // Varied but stable-ish seed from level + player tile.
        let seed = (game.level_idx as u32)
            .wrapping_mul(9973)
            .wrapping_add(game.player.x.to_bits())
            .wrapping_add(game.player.y.to_bits() << 1);
        let policy = Policy {
            seed,
            // The in-game I-key is a floor-finishing assist. Forge has the
            // expensive full-clear/secret objectives.
            hunt_kills: false,
            seek_secrets: false,
            engage_range: 10.0,
            ..Policy::default()
        };
        Some(Self {
            brain: Brain::assist(policy, game),
            saved_god: game.god,
            saved_infinite: game.infinite_ammo,
            saved_keys: game.keys,
            saved_weapon: game.weapon,
            saved_ammo: game.ammo,
        })
    }

    /// Temporary loadout so the pilot is not stranded mid-floor.
    pub fn engage(&self, game: &mut Game) {
        game.god = true;
        game.infinite_ammo = true;
        game.keys = KEY_GOLD | KEY_SILVER;
        game.ammo = game.ammo.max(40);
        // Prefer the best gun the player already earned; never invent a chaingun.
        game.weapon = game.bestweapon.max(1);
    }

    /// Restore the loadout that was active when the pilot started.
    pub fn disengage(&self, game: &mut Game) {
        game.god = self.saved_god;
        game.infinite_ammo = self.saved_infinite;
        game.keys = self.saved_keys;
        game.weapon = self.saved_weapon;
        game.ammo = self.saved_ammo;
    }

    pub fn done(&self) -> bool {
        // Brain has no separate done flag; game screen change ends the pilot.
        false
    }

    /// Produce one tic of input for the current game state.
    pub fn tick(&mut self, game: &mut Game) -> Input {
        if game.screen != GameScreen::Playing {
            return Input::default();
        }
        self.brain.tick(game)
    }
}

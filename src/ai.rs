//! Fair floor-clearing AI for attract demos and the in-game `I` autopilot.
//!
//! Multi-goal hierarchical controller:
//! 1. Fight nearby threats (line-of-sight + range)
//! 2. Collect keys / ammo when the plan needs them
//! 3. **Explicit 100% waypoints** when the policy asks for them:
//!    path to remaining secret push-walls, then remaining enemies
//! 4. Path to the elevator (or boss) and use the exit
//!
//! Policies are cheap integer knobs + an RNG seed. [`search_level`] runs many
//! trials (optionally multi-threaded), scores completions, and keeps the best
//! recorded [`crate::demorec::Demo`].

use std::collections::{HashSet, VecDeque};
use std::f32::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::actors::Kind;
use crate::demorec::{AiPolicy as RecordedAiPolicy, Demo};
use crate::game::{Game, GameScreen, Input, TIC, WEAPON_CHAINGUN};
use crate::hud::{KEY_GOLD, KEY_SILVER};
use crate::raycast::{Bonus, World};

const MAP: usize = 64;
const DT: f32 = TIC;

// =============================================================================
// Policy + score
// =============================================================================

/// Searchable behaviour knobs. Different seeds + knobs explore different plays.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Drives actor RNG start and stochastic combat choices.
    pub seed: u32,
    /// How far (tiles) we engage a visible enemy.
    pub engage_range: f32,
    /// How tightly we must aim before firing (radians).
    pub aim_slack: f32,
    /// Strafe period (tics) while circle-strafing.
    pub strafe_period: u32,
    /// Prefer left (true) or right strafe when the period is even.
    pub strafe_left_bias: bool,
    /// If health drops below this while fighting, back off more aggressively.
    pub panic_health: i32,
    /// Path to remaining enemies before the exit (100% kill intent).
    pub hunt_kills: bool,
    /// Path to remaining secret push-walls before the exit (100% secret intent).
    pub seek_secrets: bool,
    /// Run invulnerably. The forge defaults this off, but can enable it
    /// explicitly for exploration-heavy searches.
    pub god: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            seed: 1,
            engage_range: 9.0,
            aim_slack: 0.32,
            strafe_period: 16,
            strafe_left_bias: true,
            panic_health: 35,
            hunt_kills: true,
            seek_secrets: true,
            god: false,
        }
    }
}

impl Policy {
    /// Deterministic family of policies for trial `i` (0..iters).
    /// Biased toward **full-clear** goals (points: +10 kill, +1 secret).
    pub fn from_trial(i: u64) -> Self {
        let s = i
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0xA24B_AED4_96E9);
        let b = s.to_le_bytes();
        // 0..=5 full clear, 6..=7 kills-only, 8 secrets-only, 9 any%
        let mode = b[7] % 10;
        let (hunt_kills, seek_secrets, engage) = match mode {
            0..=5 => (true, true, 8.0 + (b[0] % 6) as f32), // full clear ~60%
            6..=7 => (true, false, 7.0 + (b[0] % 5) as f32), // kills ~20%
            8 => (false, true, 5.0 + (b[0] % 4) as f32),    // secrets ~10%
            _ => (false, false, 3.5 + (b[0] % 4) as f32 * 0.5), // any% ~10%
        };
        Self {
            seed: s as u32,
            engage_range: engage,
            aim_slack: 0.18 + (b[1] % 25) as f32 * 0.01,
            strafe_period: 10 + (b[2] % 24) as u32,
            strafe_left_bias: b[3].is_multiple_of(2),
            panic_health: 20 + (b[4] % 40) as i32,
            hunt_kills,
            seek_secrets,
            god: false,
        }
    }

    /// Local neighbour of a known-good policy (hill-climb).
    pub fn mutate(&self, salt: u64) -> Self {
        let s = salt
            .wrapping_mul(0xD1B5_4A32_D192_ED03)
            .wrapping_add(self.seed as u64);
        let b = s.to_le_bytes();
        let mut p = self.clone();
        p.seed = self.seed.wrapping_add(b[0] as u32).wrapping_add(1);
        match b[1] % 7 {
            0 => p.engage_range = (p.engage_range + (b[2] % 5) as f32 * 0.4 - 0.8).clamp(2.5, 16.0),
            1 => p.aim_slack = (p.aim_slack + (b[2] % 7) as f32 * 0.02 - 0.06).clamp(0.12, 0.5),
            2 => {
                p.strafe_period =
                    (p.strafe_period as i32 + (b[2] % 9) as i32 - 4).clamp(6, 40) as u32
            }
            3 => p.strafe_left_bias = !p.strafe_left_bias,
            4 => p.panic_health = (p.panic_health + (b[2] % 15) as i32 - 7).clamp(10, 70),
            5 => p.hunt_kills = !p.hunt_kills,
            _ => p.seek_secrets = !p.seek_secrets,
        }
        p
    }

    /// Any-percent: minimal combat, rush the exit.
    pub fn speedrun(seed: u32) -> Self {
        Self {
            seed,
            engage_range: 3.5,
            aim_slack: 0.28,
            strafe_period: 12,
            strafe_left_bias: seed.is_multiple_of(2),
            panic_health: 40,
            hunt_kills: false,
            seek_secrets: false,
            god: false,
        }
    }

    /// Explicit 100% intent: plan secrets + kills before exit.
    pub fn full_clear(seed: u32) -> Self {
        Self {
            seed,
            engage_range: 10.0,
            aim_slack: 0.30,
            strafe_period: 14,
            strafe_left_bias: seed.is_multiple_of(2),
            panic_health: 30,
            hunt_kills: true,
            seek_secrets: true,
            god: false,
        }
    }
}

impl From<&Policy> for RecordedAiPolicy {
    fn from(policy: &Policy) -> Self {
        Self {
            seed: policy.seed,
            engage_range: policy.engage_range,
            aim_slack: policy.aim_slack,
            strafe_period: policy.strafe_period,
            strafe_left_bias: policy.strafe_left_bias,
            panic_health: policy.panic_health,
            hunt_kills: policy.hunt_kills,
            seek_secrets: policy.seek_secrets,
        }
    }
}

impl From<&RecordedAiPolicy> for Policy {
    fn from(policy: &RecordedAiPolicy) -> Self {
        Self {
            seed: policy.seed,
            engage_range: policy.engage_range,
            aim_slack: policy.aim_slack,
            strafe_period: policy.strafe_period,
            strafe_left_bias: policy.strafe_left_bias,
            panic_health: policy.panic_health,
            hunt_kills: policy.hunt_kills,
            seek_secrets: policy.seek_secrets,
            god: false,
        }
    }
}

/// How forge candidates are ranked and steered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFocus {
    /// Normal forge score: finish +100, kill +10, secret +1.
    Score,
    /// Require secret-seeking policies and rank completed runs by secrets first.
    #[default]
    Secrets,
}

impl SearchFocus {
    /// Apply run-wide constraints after a policy is generated or mutated.
    fn configure(self, mut policy: Policy, god: bool) -> Policy {
        policy.god = god;
        if self == Self::Secrets {
            policy.seek_secrets = true;
            policy.hunt_kills = false;
        }
        policy
    }

    /// Re-rank one result for the selected forge objective.
    pub fn rank(self, result: &mut TrialResult) {
        if self != Self::Secrets {
            return;
        }
        result.fitness = if result.completed {
            // Secrets dominate, then rewards collected while exploring their
            // rooms, then the documented score, then speed.
            result.secrets as i64 * 1_000_000_000_000_000
                + result.pickups as i64 * 1_000_000_000_000
                + result.treasure as i64 * 10_000_000_000
                + result.points() as i64 * 1_000_000
                - result.tics as i64
        } else {
            // A completion always beats a partial, but partial secret progress
            // gives the hill-climb a useful direction.
            -1_000_000_000_000
                + result.secrets as i64 * 1_000_000_000
                + result.pickups as i64 * 10_000_000
                + result.treasure as i64 * 5_000_000
                + result.kills as i64 * 1_000_000
                - result.tics as i64
        };
    }
}

/// Outcome of one trial (whether or not it finished the floor).
#[derive(Clone)]
pub struct TrialResult {
    pub completed: bool,
    pub tics: u32,
    pub kills: i32,
    pub secrets: i32,
    pub treasure: i32,
    /// All collected bonus statics (health, ammo, weapons, keys, treasure).
    pub pickups: i32,
    /// Map totals (known at level load) for ideal comparison.
    pub kill_total: i32,
    pub secret_total: i32,
    pub treasure_total: i32,
    pub health: i32,
    pub ammo: i32,
    pub score: i32,
    /// Higher is better. Incomplete runs are very negative.
    pub fitness: i64,
    pub policy: Policy,
    pub demo: Option<Demo>,
}

impl TrialResult {
    /// Forge score points: +100 finish (if completed), +10/kill, +1/secret.
    pub fn points(&self) -> i32 {
        let end = if self.completed { 100 } else { 0 };
        end + self.kills * 10 + self.secrets
    }

    /// Max forge points on this floor: +100 + 10×all kills + all secrets.
    pub fn ideal_points(&self) -> i32 {
        100 + self.kill_total * 10 + self.secret_total
    }

    /// Fraction of ideal points in \[0, 1\] (0 if ideal is 0).
    pub fn ideal_ratio(&self) -> f32 {
        let ideal = self.ideal_points();
        if ideal <= 0 {
            return 0.0;
        }
        (self.points() as f32 / ideal as f32).clamp(0.0, 1.0)
    }

    fn from_game(
        policy: Policy,
        tics: u32,
        game: &Game,
        completed: bool,
        demo: Option<Demo>,
        fitness: i64,
    ) -> Self {
        Self {
            completed,
            tics,
            kills: game.stats.kills,
            secrets: game.stats.secrets,
            treasure: game.stats.treasure,
            pickups: game
                .world
                .statics
                .iter()
                .filter(|s| s.picked && s.bonus.is_some())
                .count() as i32,
            kill_total: game.stats.kill_total,
            secret_total: game.stats.secret_total,
            treasure_total: game.stats.treasure_total,
            health: game.health,
            ammo: game.ammo,
            score: game.score,
            fitness,
            policy,
            demo,
        }
    }

    fn fail(policy: Policy, tics: u32, game: &Game) -> Self {
        // Reward progress a little so search can hill-climb, but never beat a clear.
        let fitness = -1_000_000_000
            + game.stats.kills as i64 * 200
            + game.stats.secrets as i64 * 500
            + game.stats.treasure as i64 * 50
            - tics as i64;
        Self::from_game(policy, tics, game, false, None, fitness)
    }

    fn success(policy: Policy, tics: u32, game: &Game, demo: Option<Demo>) -> Self {
        // User scoring: end level +100, each kill +10, each secret +1.
        // Encode so more points always beat a faster lower-score run; among equal
        // points, fewer tics wins (superspeed tie-break).
        let points = 100i64 + game.stats.kills as i64 * 10 + game.stats.secrets as i64;
        let fitness = points * 1_000_000 - tics as i64;
        Self::from_game(policy, tics, game, true, demo, fitness)
    }
}

// =============================================================================
// Brain
// =============================================================================

/// PUSHABLETILE plane-1 marker (WL_DEF.H) — secret push-walls.
const PUSHABLE_TILE: u16 = 98;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Goal {
    Elevator,
    Boss,
    Key(u8),
    Pickup,
    /// Walk toward an enemy tile, then combat.
    Hunt {
        tx: i32,
        ty: i32,
    },
    /// Stand next to a secret wall and use.
    Secret {
        wall: (i32, i32),
        stand: (i32, i32),
    },
    /// Enter a newly opened secret area, then collect useful rewards there.
    SecretLoot {
        anchor: (i32, i32),
        dir: (i32, i32),
        target: (i32, i32),
        entering: bool,
    },
}

/// Stateful controller. One instance per run.
pub struct Brain {
    policy: Policy,
    path: Vec<(i32, i32)>,
    path_i: usize,
    elev: Option<(i32, i32)>,
    goal: Goal,
    stuck: u32,
    last_pos: (f32, f32),
    tic: u32,
    door_wait: u32,
    open_door: Option<(i32, i32)>,
    use_attempts: u32,
    replan_cd: u32,
    /// Secret walls we gave up on (unreachable / blocked push).
    skipped_secrets: HashSet<(i32, i32)>,
    /// Tics spent trying to activate the current secret.
    secret_work: u32,
    /// Secrets counter when we started the current Secret goal.
    secret_baseline: i32,
    /// path_i value last time we made progress (for stale-path recovery).
    last_path_i: usize,
    /// Tics since path_i last increased.
    path_stale: u32,
    /// When true, combat is more aggressive (in-game `I` assist).
    assist: bool,
}

impl Brain {
    /// Fair AI for demo recording / search (no free keys, no god).
    pub fn fair(policy: Policy, game: &Game) -> Self {
        Self::new(policy, game, false)
    }

    /// In-game `I` toggle: floor-finishing pathing with assist combat range.
    pub fn assist(policy: Policy, game: &Game) -> Self {
        let mut p = policy;
        p.engage_range = p.engage_range.max(10.0);
        Self::new(p, game, true)
    }

    fn new(policy: Policy, game: &Game, assist: bool) -> Self {
        let elev = find_elevators(&game.world).first().copied();
        let mut brain = Self {
            policy,
            path: Vec::new(),
            path_i: 0,
            elev,
            goal: Goal::Elevator,
            stuck: 0,
            last_pos: (game.player.x, game.player.y),
            tic: 0,
            door_wait: 0,
            open_door: None,
            use_attempts: 0,
            replan_cd: 0,
            skipped_secrets: HashSet::new(),
            secret_work: 0,
            secret_baseline: game.stats.secrets,
            last_path_i: 0,
            path_stale: 0,
            assist,
        };
        brain.pick_goal(game);
        brain.replan(game);
        brain
    }

    pub fn done(&self, game: &Game) -> bool {
        game.screen != GameScreen::Playing
    }

    /// True when we should stop optional hunting and bank the +100 finish.
    /// (Still unlock elev first if it's gated.)
    fn should_force_exit(&self, game: &Game) -> bool {
        if game.health > 0 && game.health < 28 {
            return true;
        }
        if self.tic > 70 * 50 && !self.policy.seek_secrets {
            return true;
        }
        let more_kills = self.policy.hunt_kills && nearest_enemy_tile(game).is_some();
        let more_secrets =
            self.policy.seek_secrets && nearest_secret(game, &self.skipped_secrets).is_some();
        !more_kills && !more_secrets
    }

    /// Unlock the exit before anything optional. Keys, then secrets that open
    /// the route (many FAIL maps have elevators only reachable via pushwalls).
    fn pick_unlock_goal(&mut self, game: &Game) -> bool {
        if elev_reachable(game, game.keys) {
            return false;
        }
        let all = KEY_GOLD | KEY_SILVER;
        // Elev not reachable even with both keys → secrets open the map.
        if !elev_reachable(game, all)
            && let Some((wall, stand)) = nearest_secret(game, &self.skipped_secrets)
        {
            self.goal = Goal::Secret { wall, stand };
            self.secret_work = 0;
            self.secret_baseline = game.stats.secrets;
            return true;
        }
        // Prefer a key that actually opens a path to the elev.
        for need in [KEY_GOLD, KEY_SILVER] {
            if game.keys & need != 0 {
                continue;
            }
            if nearest_reachable_key_tile(game, need).is_none() {
                continue;
            }
            if elev_reachable(game, game.keys | need) {
                self.goal = Goal::Key(need);
                return true;
            }
        }
        // Any key / secret as exploration (may open detours).
        if let Some(need) = first_missing_key(game) {
            self.goal = Goal::Key(need);
            return true;
        }
        if let Some((wall, stand)) = nearest_secret(game, &self.skipped_secrets) {
            self.goal = Goal::Secret { wall, stand };
            self.secret_work = 0;
            self.secret_baseline = game.stats.secrets;
            return true;
        }
        false
    }

    /// Choose the next high-level goal from remaining work.
    fn pick_goal(&mut self, game: &Game) {
        // Boss floors (no elevator): gear up, then fight.
        if find_elevators(&game.world).is_empty() {
            if game.ammo < 20
                && let Some(p) = nearest_ammo_tile(game)
                && nearest_stand(game, tile_of(game), p).is_some()
            {
                self.goal = Goal::Pickup;
                return;
            }
            // Clear pathable trash for ammo/weapons before the boss.
            if game.ammo < 40
                && let Some((tx, ty)) = nearest_non_boss_enemy(game)
            {
                self.goal = Goal::Hunt { tx, ty };
                return;
            }
            self.goal = Goal::Boss;
            return;
        }

        // Exit gated by key/secret — always first.
        if self.pick_unlock_goal(game) {
            return;
        }

        // Bank the clear once the exit is open.
        if self.should_force_exit(game) {
            self.goal = Goal::Elevator;
            return;
        }

        if self.policy.seek_secrets
            && let Some((wall, stand)) = nearest_secret(game, &self.skipped_secrets)
        {
            self.goal = Goal::Secret { wall, stand };
            self.secret_work = 0;
            self.secret_baseline = game.stats.secrets;
            return;
        }

        if self.policy.hunt_kills
            && let Some((tx, ty)) = nearest_enemy_tile(game)
        {
            self.goal = Goal::Hunt { tx, ty };
            return;
        }

        self.goal = Goal::Elevator;
    }

    /// One tic of AI input.
    pub fn tick(&mut self, game: &Game) -> Input {
        self.tic = self.tic.wrapping_add(1);
        if game.screen != GameScreen::Playing {
            return Input::default();
        }

        // A newly activated push-wall remains solid while it slides. Wait for
        // it to finish before planning the next secret, otherwise secrets
        // behind it look unreachable and get permanently skipped.
        if let Goal::Secret { wall, .. } = self.goal
            && plane1(&game.world, wall.0, wall.1) != PUSHABLE_TILE
            && game.world.pushwall.is_some()
        {
            self.stuck = 0;
            self.last_pos = (game.player.x, game.player.y);
            return Input::default();
        }

        let moved =
            (game.player.x - self.last_pos.0).abs() + (game.player.y - self.last_pos.1).abs();
        if moved < 0.008 {
            self.stuck = self.stuck.saturating_add(1);
        } else {
            self.stuck = 0;
            self.last_pos = (game.player.x, game.player.y);
        }

        if self.replan_cd > 0 {
            self.replan_cd -= 1;
        }
        // Stuck recovery: skip a waypoint, or abandon the current goal.
        if self.stuck > 35 && self.replan_cd == 0 {
            self.stuck = 0;
            self.replan_cd = 20;
            if matches!(self.goal, Goal::Secret { .. } | Goal::SecretLoot { .. }) {
                // A fight, door, or narrow corner can hold the player still
                // for half a second. Replan from the current tile; activation
                // attempts have their own longer timeout in tick_secret.
                self.replan(game);
            } else if self.path_i + 1 < self.path.len() {
                // Nudge past a jammed corner instead of replaying the whole path.
                self.path_i += 1;
            } else if matches!(self.goal, Goal::Pickup) {
                // Ammo path failed — fight with knife / move on.
                self.pick_goal(game);
                self.replan(game);
            } else {
                self.pick_goal(game);
                self.replan(game);
            }
        }

        // Periodically refresh multi-goal plan (enemy died, secret opened, …).
        if self.replan_cd == 0 && self.tic.is_multiple_of(70) {
            self.refresh_goal(game);
            self.replan_cd = 10;
        }

        if let Some((dx, dy)) = self.open_door {
            return self.tick_open_door(game, dx, dy);
        }

        // Dry: hunt ammo before anything else (except finishing a secret push).
        if game.ammo == 0
            && !matches!(
                self.goal,
                Goal::Secret { .. } | Goal::SecretLoot { .. } | Goal::Pickup
            )
            && let Some(p) = nearest_ammo_tile(game)
            && bfs_path(game, tile_of(game), p).is_some()
        {
            self.goal = Goal::Pickup;
            self.replan(game);
            self.replan_cd = 30;
        }

        // Combat range depends on the goal. Key/exit rushes must NOT get drawn
        // into firefights (e1m5 died pathing to the gold key while "defending").
        let mut engage = match self.goal {
            Goal::Hunt { .. } | Goal::Boss => self.policy.engage_range.max(12.0),
            Goal::Secret { .. } | Goal::SecretLoot { .. } | Goal::Pickup => 2.5,
            Goal::Key(_) => 1.6, // knife-range only
            Goal::Elevator => {
                if self.policy.hunt_kills && !self.should_force_exit(game) {
                    self.policy.engage_range.min(5.0)
                } else {
                    1.6 // any% / force-exit: run past grunts
                }
            }
        };
        if self.assist && matches!(self.goal, Goal::Elevator) {
            // The I-key pilot is invulnerable and only needs to finish the
            // floor. Fight only guards close enough to physically block the
            // route; distant targets behind geometry must not monopolize aim.
            engage = 1.5;
        } else if self.assist {
            engage += 2.0;
        }
        if game.ammo == 0 {
            engage = engage.min(2.0);
        }
        if let Some(input) = self.tick_combat_range(game, engage) {
            return input;
        }

        // Low ammo while hunting → grab a clip (pathable only).
        if game.ammo > 0
            && game.ammo <= 3
            && self.replan_cd == 0
            && matches!(self.goal, Goal::Hunt { .. })
            && let Some(p) = nearest_ammo_tile(game)
            && bfs_path(game, tile_of(game), p).is_some()
        {
            self.goal = Goal::Pickup;
            self.replan(game);
            self.replan_cd = 40;
        }

        match self.goal {
            Goal::Boss => self.tick_boss(game),
            Goal::Secret { wall, stand } => self.tick_secret(game, wall, stand),
            Goal::SecretLoot {
                anchor,
                dir,
                target,
                entering,
            } => self.tick_secret_loot(game, anchor, dir, target, entering),
            Goal::Hunt { tx, ty } => self.tick_hunt(game, tx, ty),
            Goal::Elevator | Goal::Key(_) | Goal::Pickup => self.tick_navigate(game),
        }
    }

    /// If the current goal is done (dead enemy, secret activated), pick the next.
    fn refresh_goal(&mut self, game: &Game) {
        match self.goal {
            Goal::Hunt { tx, ty } => {
                let still = game.actors.list.iter().any(|a| {
                    !a.dead
                        && !a.kind.is_ghost()
                        && (a.x.floor() as i32 - tx).abs() <= 2
                        && (a.y.floor() as i32 - ty).abs() <= 2
                });
                if !still {
                    // Dead or moved — retarget (next kill or secrets/exit).
                    self.pick_goal(game);
                    self.replan(game);
                }
            }
            Goal::Secret { wall, .. } => {
                let still = plane1(&game.world, wall.0, wall.1) == PUSHABLE_TILE;
                if !still || game.stats.secrets > self.secret_baseline {
                    self.pick_goal(game);
                    self.replan(game);
                }
            }
            Goal::SecretLoot { target, .. } => {
                if tile_of(game) == target {
                    self.advance_secret_loot(game);
                }
            }
            Goal::Elevator
                if (self.policy.hunt_kills && remaining_enemies(game).next().is_some())
                    || (self.policy.seek_secrets
                        && nearest_secret(game, &self.skipped_secrets).is_some()) =>
            {
                // Don't exit early if we still want 100% work.
                self.pick_goal(game);
                self.replan(game);
            }
            _ => {}
        }
    }

    fn replan(&mut self, game: &Game) {
        let start = tile_of(game);
        match self.goal {
            Goal::Elevator => self.replan_elevator(game, start),
            Goal::Key(k) => {
                if let Some(kt) = nearest_reachable_key_tile(game, k) {
                    // Keys often sit on non-walkable tiles — stand on a neighbor.
                    let stand = nearest_stand(game, start, kt).unwrap_or(kt);
                    self.path = bfs_path(game, start, stand).unwrap_or_default();
                    self.path_i = 0;
                    self.last_path_i = 0;
                    self.path_stale = 0;
                } else {
                    self.pick_goal(game);
                    if !matches!(self.goal, Goal::Key(_)) {
                        self.replan(game);
                    }
                }
            }
            Goal::Pickup => {
                if let Some(p) = nearest_ammo_tile(game) {
                    let stand = nearest_stand(game, start, p).unwrap_or(p);
                    self.path = bfs_path(game, start, stand).unwrap_or_default();
                    self.path_i = 0;
                    self.last_path_i = 0;
                    self.path_stale = 0;
                } else {
                    self.pick_goal(game);
                    self.replan(game);
                }
            }
            Goal::Hunt { tx, ty } => {
                let stand = neighbors(tx, ty)
                    .into_iter()
                    .chain(std::iter::once((tx, ty)))
                    .find(|&(x, y)| walkable(game, x, y))
                    .unwrap_or((tx, ty));
                self.path = bfs_path(game, start, stand).unwrap_or_default();
                self.path_i = 0;
                if self.path.is_empty() {
                    // Unreachable (likely locked). Fetch a key if possible, else exit.
                    if let Some(need) = first_missing_key(game) {
                        self.goal = Goal::Key(need);
                        self.replan(game);
                    } else {
                        // Try another enemy / secret / elev.
                        self.pick_goal(game);
                        if matches!(self.goal, Goal::Hunt { tx: nx, ty: ny } if nx == tx && ny == ty)
                        {
                            // Same target loop — fall back to exit.
                            self.goal = Goal::Elevator;
                        }
                        self.replan(game);
                    }
                }
            }
            Goal::Secret { stand, wall } => {
                self.path = bfs_path(game, start, stand).unwrap_or_default();
                self.path_i = 0;
                if self.path.is_empty() {
                    self.skipped_secrets.insert(wall);
                    self.pick_goal(game);
                    self.replan(game);
                }
            }
            Goal::SecretLoot { target, .. } => {
                self.path = bfs_path(game, start, target).unwrap_or_default();
                self.path_i = 0;
                self.last_path_i = 0;
                self.path_stale = 0;
                if self.path.is_empty() && start != target {
                    // The sliding wall or an actor may still obstruct this
                    // exact route. Stay in the loot phase and retry naturally.
                    self.replan_cd = 20;
                }
            }
            Goal::Boss => {
                self.path.clear();
                self.path_i = 0;
            }
        }
    }

    fn replan_elevator(&mut self, game: &Game, start: (i32, i32)) {
        type PathElev = (Vec<(i32, i32)>, (i32, i32));
        let elevators = find_elevators(&game.world);
        let mut best: Option<PathElev> = None;
        for &(ex, ey) in &elevators {
            for stand in neighbors(ex, ey) {
                if !walkable(game, stand.0, stand.1) {
                    continue;
                }
                if let Some(path) = bfs_path(game, start, stand) {
                    let better = best
                        .as_ref()
                        .map(|(p, _)| path.len() < p.len())
                        .unwrap_or(true);
                    let tie = best.as_ref().is_some_and(|(p, _)| p.len() == path.len())
                        && (self.policy.seed ^ (ex as u32) ^ (ey as u32)).is_multiple_of(2);
                    if better || tie {
                        best = Some((path, (ex, ey)));
                    }
                }
            }
        }
        if let Some((path, elev)) = best {
            self.path = path;
            self.elev = Some(elev);
            self.path_i = 0;
        } else if let Some(&(ex, ey)) = elevators.first() {
            self.elev = Some((ex, ey));
            if let Some(need) = first_missing_key(game) {
                self.goal = Goal::Key(need);
                self.replan(game);
            } else {
                self.path.clear();
                self.path_i = 0;
            }
        }
    }

    fn tick_open_door(&mut self, game: &Game, dx: i32, dy: i32) -> Input {
        if door_passable(&game.world, dx, dy) {
            self.open_door = None;
            self.door_wait = 0;
            // Immediately step through so the door can't close on us.
            let want = ((dy as f32 + 0.5) - game.player.y).atan2((dx as f32 + 0.5) - game.player.x);
            let turn = norm_angle(want - game.player.angle);
            return Input {
                forward: turn.abs() < 0.7,
                run: true,
                turn_left: turn < -0.02,
                turn_right: turn > 0.02,
                ..Default::default()
            };
        }
        self.door_wait += 1;
        let pos = game
            .world
            .door_lookup(dx, dy)
            .map(|i| game.world.doors[i].position)
            .unwrap_or(0.0);
        // Don't spam use — it toggles the door closed while opening.
        let opening = pos > 0.02;
        let tap = !opening && (self.door_wait == 1 || self.door_wait.is_multiple_of(20));
        let want = ((dy as f32 + 0.5) - game.player.y).atan2((dx as f32 + 0.5) - game.player.x);
        let turn = norm_angle(want - game.player.angle);
        if self.door_wait > 100 {
            self.open_door = None;
            self.door_wait = 0;
            self.path_i = self.path_i.saturating_add(1);
        }
        // Once the slab is half-open, press into it (less idle chip damage).
        let press = pos > 0.45 && turn.abs() < 0.6;
        Input {
            use_door: tap,
            turn_left: turn < -0.02,
            turn_right: turn > 0.02,
            forward: press,
            run: press,
            ..Default::default()
        }
    }

    fn tick_combat_range(&mut self, game: &Game, range: f32) -> Option<Input> {
        let (px, py) = (game.player.x, game.player.y);

        let mut best: Option<(f32, f32, f32, i32)> = None; // dist2, x, y, priority
        for a in &game.actors.list {
            // Pac-Man ghosts and spectres are environmental hazards: shots
            // cannot kill them. Targeting one makes the bot back into a wall
            // and fire forever instead of navigating around it.
            if a.dead || a.kind.is_ghost() {
                continue;
            }
            let d2 = (a.x - px).powi(2) + (a.y - py).powi(2);
            let dist = d2.sqrt();
            if dist > range {
                continue;
            }
            if !has_los(&game.world, px, py, a.x, a.y) {
                continue;
            }
            let pri = if is_end_boss(a.kind) {
                0
            } else {
                match a.kind {
                    Kind::Officer | Kind::Mutant | Kind::Ss => 1,
                    _ => 2,
                }
            };
            let better = best
                .as_ref()
                .map(|b| pri < b.3 || (pri == b.3 && d2 < b.0))
                .unwrap_or(true);
            if better {
                best = Some((d2, a.x, a.y, pri));
            }
        }
        let (d2, bx, by, _) = best?;
        let dist = d2.sqrt();
        let turn = norm_angle((by - py).atan2(bx - px) - game.player.angle);
        let aim_ok = turn.abs() < self.policy.aim_slack;
        // Keep circling in one direction like a player would; reverse only
        // after pressing against an obstacle for the policy's tolerance.
        // Rapid timer-based left/right oscillation was both weak and robotic.
        let reverse = self.stuck > self.policy.strafe_period.max(4);
        let strafe_left = self.policy.strafe_left_bias != reverse;
        let (sl, sr) = (strafe_left, !strafe_left);
        let panic = game.health < self.policy.panic_health;
        let combat_aligned = turn.abs() < 0.5;
        let dry = game.ammo == 0;
        Some(Input {
            // With the knife selected, close and swing. The old shared ammo
            // gate selected the knife but could never actually attack with it.
            fire: aim_ok && (!dry || dist < 1.7),
            turn_left: turn < -0.02,
            turn_right: turn > 0.02,
            back: !dry && dist < if panic { 4.5 } else { 2.8 },
            forward: if dry {
                dist > 1.1 && aim_ok
            } else {
                dist > if panic { 9.0 } else { 6.5 } && aim_ok
            },
            // Human-rate combat: circle-strafe once the target is broadly in
            // view, but fire only under the narrower aim tolerance.
            run: combat_aligned,
            strafe_left: combat_aligned && sl,
            strafe_right: combat_aligned && sr,
            // Knife if dry.
            select_weapon: if game.ammo == 0 {
                Some(0)
            } else if game.bestweapon >= WEAPON_CHAINGUN {
                Some(3)
            } else if game.bestweapon >= 2 {
                Some(2)
            } else {
                None
            },
            ..Default::default()
        })
    }

    fn tick_boss(&mut self, game: &Game) -> Input {
        // Stay stocked — bosses eat ammo.
        if game.ammo < 8
            && let Some(p) = nearest_ammo_tile(game)
            && nearest_stand(game, tile_of(game), p).is_some()
        {
            self.goal = Goal::Pickup;
            self.replan(game);
            return Input::default();
        }
        // Long engage for bosses.
        if let Some(input) = self.tick_combat_range(game, 18.0) {
            return input;
        }
        let (px, py) = (game.player.x, game.player.y);
        let target = game
            .actors
            .list
            .iter()
            .filter(|a| !a.dead && is_end_boss(a.kind))
            .min_by(|a, b| {
                let da = (a.x - px).powi(2) + (a.y - py).powi(2);
                let db = (b.x - px).powi(2) + (b.y - py).powi(2);
                da.total_cmp(&db)
            });
        if let Some(t) = target {
            let stand = (t.x.floor() as i32, t.y.floor() as i32);
            if self.path.is_empty() || self.path_i >= self.path.len() {
                let start = tile_of(game);
                self.path = neighbors(stand.0, stand.1)
                    .into_iter()
                    .filter(|&(x, y)| walkable(game, x, y))
                    .filter_map(|near| bfs_path(game, start, near))
                    .min_by_key(Vec::len)
                    .unwrap_or_default();
                self.path_i = 0;
                if self.path.is_empty() {
                    // Some boss arenas are gated behind a key or a room of
                    // guards. Progress those normal map goals before retrying
                    // the boss instead of idling at an unreachable target.
                    if let Some(key) = first_missing_key(game) {
                        self.goal = Goal::Key(key);
                        self.replan(game);
                    } else if let Some((tx, ty)) = nearest_non_boss_enemy(game) {
                        self.goal = Goal::Hunt { tx, ty };
                        self.replan(game);
                    }
                    return Input::default();
                }
            }
            return self.tick_navigate(game);
        }
        // No boss left — maybe elevators appeared / secrets; re-plan.
        self.pick_goal(game);
        self.replan(game);
        Input::default()
    }

    fn tick_hunt(&mut self, game: &Game, tx: i32, ty: i32) -> Input {
        // Enemy already dead / left the area → re-pick.
        let alive_near = game.actors.list.iter().any(|a| {
            !a.dead
                && !a.kind.is_ghost()
                && (a.x.floor() as i32 - tx).abs() <= 3
                && (a.y.floor() as i32 - ty).abs() <= 3
        });
        if !alive_near {
            self.pick_goal(game);
            self.replan(game);
            return Input::default();
        }
        // Dry and stuck on a hunt → find ammo or skip to another objective.
        if game.ammo == 0 && self.stuck > 25 {
            if let Some(p) = nearest_ammo_tile(game)
                && bfs_path(game, tile_of(game), p).is_some()
            {
                self.goal = Goal::Pickup;
                self.replan(game);
            } else {
                self.pick_goal(game);
                self.replan(game);
            }
            return Input::default();
        }
        // Close enough: hold and fight (combat handled at top of tick).
        let dist = (tx as f32 + 0.5 - game.player.x).hypot(ty as f32 + 0.5 - game.player.y);
        if dist < 4.0 || self.path_i >= self.path.len() {
            let want = ((ty as f32 + 0.5) - game.player.y).atan2((tx as f32 + 0.5) - game.player.x);
            let turn = norm_angle(want - game.player.angle);
            return Input {
                turn_left: turn < -0.02,
                turn_right: turn > 0.02,
                forward: turn.abs() < 0.35 && dist > 1.8,
                back: dist < 1.2 && game.ammo > 0,
                run: true,
                fire: turn.abs() < self.policy.aim_slack,
                select_weapon: if game.ammo == 0 { Some(0) } else { None },
                ..Default::default()
            };
        }
        self.tick_navigate(game)
    }

    fn tick_secret(&mut self, game: &Game, wall: (i32, i32), stand: (i32, i32)) -> Input {
        // Success: marker gone or secret counter bumped.
        if plane1(&game.world, wall.0, wall.1) != PUSHABLE_TILE
            || game.stats.secrets > self.secret_baseline
        {
            self.start_secret_loot(game, wall, stand);
            return Input::default();
        }
        // Give up after ~3s of trying.
        if self.secret_work > 70 * 3 {
            self.skipped_secrets.insert(wall);
            self.pick_goal(game);
            self.replan(game);
            return Input::default();
        }

        let at_stand = (game.player.x - (stand.0 as f32 + 0.5))
            .hypot(game.player.y - (stand.1 as f32 + 0.5))
            < 0.45;
        if !at_stand && self.path_i < self.path.len() {
            return self.tick_navigate(game);
        }
        // Count only activation work at the wall. The old timeout also counted
        // travel time, so any secret more than roughly three seconds away was
        // abandoned before the bot reached it.
        self.secret_work = self.secret_work.saturating_add(1);
        // Face the wall and use (push-walls need cardinal facing).
        let want =
            ((wall.1 as f32 + 0.5) - game.player.y).atan2((wall.0 as f32 + 0.5) - game.player.x);
        let turn = norm_angle(want - game.player.angle);
        if turn.abs() > 0.12 {
            return Input {
                turn_left: turn < 0.0,
                turn_right: turn > 0.0,
                ..Default::default()
            };
        }
        Input {
            use_door: self.secret_work.is_multiple_of(6),
            ..Default::default()
        }
    }

    fn start_secret_loot(&mut self, game: &Game, wall: (i32, i32), stand: (i32, i32)) {
        let dir = (wall.0 - stand.0, wall.1 - stand.1);
        // After the wall has slid two tiles, the first tile behind its original
        // position is clear. Walk through it before considering another goal.
        let entry = (wall.0 + dir.0, wall.1 + dir.1);
        self.goal = Goal::SecretLoot {
            anchor: wall,
            dir,
            target: entry,
            entering: true,
        };
        self.replan(game);
    }

    fn advance_secret_loot(&mut self, game: &Game) {
        let Goal::SecretLoot { anchor, dir, .. } = self.goal else {
            return;
        };
        if let Some(target) = nearest_secret_loot(game, anchor, dir) {
            self.goal = Goal::SecretLoot {
                anchor,
                dir,
                target,
                entering: false,
            };
            self.replan(game);
        } else {
            self.pick_goal(game);
            self.replan(game);
        }
    }

    fn tick_secret_loot(
        &mut self,
        game: &Game,
        _anchor: (i32, i32),
        _dir: (i32, i32),
        target: (i32, i32),
        _entering: bool,
    ) -> Input {
        if tile_of(game) == target {
            self.advance_secret_loot(game);
            return Input::default();
        }
        if self.path.is_empty() || self.path_i >= self.path.len() {
            self.replan(game);
            if self.path.is_empty() {
                // A moving actor can transiently block the tile. Let the normal
                // stale-path recovery retry instead of abandoning the room.
                return Input::default();
            }
        }
        self.tick_navigate(game)
    }

    fn tick_navigate(&mut self, game: &Game) -> Input {
        // Stale path: replan from current pose if the cursor hasn't moved.
        if self.path_i > self.last_path_i {
            self.last_path_i = self.path_i;
            self.path_stale = 0;
        } else {
            self.path_stale = self.path_stale.saturating_add(1);
            if self.path_stale > 90 {
                self.path_stale = 0;
                self.replan(game);
            }
        }

        // Key collected mid-path.
        if let Goal::Key(k) = self.goal
            && game.keys & k != 0
        {
            self.pick_goal(game);
            self.replan(game);
        }
        // Arrived at key/pickup goal tile → re-pick multi-goal plan.
        if matches!(self.goal, Goal::Key(_) | Goal::Pickup)
            && (self.path_i >= self.path.len() || self.path.is_empty())
        {
            self.pick_goal(game);
            self.replan(game);
        }
        // Elevator use only when that is the goal and we are beside the switch.
        if matches!(self.goal, Goal::Elevator)
            && let Some((ex, ey)) = self.elev
        {
            let px = game.player.x.floor() as i32;
            let py = game.player.y.floor() as i32;
            if (px - ex).abs() + (py - ey).abs() == 1 {
                // Only keep hunting if we are not forcing an exit and work remains.
                if !self.should_force_exit(game)
                    && ((self.policy.hunt_kills && nearest_enemy_tile(game).is_some())
                        || (self.policy.seek_secrets
                            && nearest_secret(game, &self.skipped_secrets).is_some()))
                {
                    self.pick_goal(game);
                    self.replan(game);
                    return Input::default();
                }
                return self.tick_elevator_use(game, ex, ey);
            }
        }

        if self.path.is_empty() || self.path_i >= self.path.len() {
            if matches!(self.goal, Goal::Hunt { .. } | Goal::Secret { .. }) {
                return Input::default();
            }
            // A recovery nudge can exhaust the old route before we are truly
            // beside the elevator. Replan from the actual tile; aiming at a
            // distant switch here leaves the pilot turning in place forever.
            self.replan(game);
            if self.path.is_empty() {
                return Input::default();
            }
        }

        // Always open the next closed door on the remaining path before seeking
        // past it (the old code advanced path_i onto the far side of a door).
        if let Some((dx, dy)) = next_closed_door(game, &self.path, self.path_i) {
            if let Some(idx) = game.world.door_lookup(dx, dy) {
                let lock = game.world.doors[idx].lock;
                if lock != 0 && game.keys & lock == 0 {
                    self.goal = Goal::Key(if lock & KEY_GOLD != 0 {
                        KEY_GOLD
                    } else {
                        KEY_SILVER
                    });
                    self.replan(game);
                    return Input::default();
                }
            }
            self.open_door = Some((dx, dy));
            self.door_wait = 0;
            return Input::default();
        }

        // Tile-based progress — never jump past a still-closed door on the path.
        let tile_here = tile_of(game);
        if let Some(idx) = self.path.iter().position(|&p| p == tile_here)
            && idx + 1 > self.path_i
            && path_prefix_clear(game, &self.path, idx)
        {
            self.path_i = idx + 1;
            self.stuck = 0;
            if self.path_i >= self.path.len() {
                return Input::default();
            }
        }

        let i = self.path_i.min(self.path.len() - 1);
        let j = look_ahead_index(game, &self.path, i);
        // Don't look-ahead past a closed door.
        let j = {
            let mut jj = j;
            for k in i..=j {
                let (x, y) = self.path[k];
                if is_door_tile(&game.world, x, y) && !door_passable(&game.world, x, y) {
                    jj = k;
                    break;
                }
            }
            jj
        };
        let (gx, gy) = self.path[j];
        let txf = gx as f32 + 0.5;
        let tyf = gy as f32 + 0.5;
        let dx = txf - game.player.x;
        let dy = tyf - game.player.y;
        let dist = (dx * dx + dy * dy).sqrt();

        if dist < 0.35 || (self.stuck > 18 && dist < 1.25) {
            if path_prefix_clear(game, &self.path, j) {
                self.path_i = j.saturating_add(1).max(self.path_i + 1);
                self.stuck = 0;
            }
            return Input::default();
        }

        let want = dy.atan2(dx);
        let turn = norm_angle(want - game.player.angle);
        let mut input = Input {
            turn_left: turn < -0.02,
            turn_right: turn > 0.02,
            // Turn in place for genuinely sharp corners; a player can steer
            // through ordinary bends, but only runs once reasonably aligned.
            forward: turn.abs() < 0.85,
            run: turn.abs() < 0.50,
            ..Default::default()
        };
        if self.stuck > 10 {
            input.strafe_left = self.policy.strafe_left_bias;
            input.strafe_right = !self.policy.strafe_left_bias;
            input.forward = true;
            input.use_door = self.stuck.is_multiple_of(8);
        }
        input
    }

    fn tick_elevator_use(&mut self, game: &Game, ex: i32, ey: i32) -> Input {
        let want = ((ey as f32 + 0.5) - game.player.y).atan2((ex as f32 + 0.5) - game.player.x);
        let turn = norm_angle(want - game.player.angle);
        if turn.abs() > 0.08 {
            return Input {
                turn_left: turn < 0.0,
                turn_right: turn > 0.0,
                ..Default::default()
            };
        }
        self.use_attempts = self.use_attempts.saturating_add(1);
        // Tap use periodically (toggle-safe: only while still Playing).
        Input {
            use_door: self.use_attempts.is_multiple_of(4),
            ..Default::default()
        }
    }
}

fn is_end_boss(k: Kind) -> bool {
    matches!(
        k,
        Kind::Hans
            | Kind::Schabbs
            | Kind::MechaHitler
            | Kind::Hitler
            | Kind::Gift
            | Kind::Gretel
            | Kind::Fat
            | Kind::Angel
            | Kind::Trans
            | Kind::Uber
            | Kind::Will
            | Kind::Death
    )
}

// =============================================================================
// Trial runner + search
// =============================================================================

/// Run one policy on `level_idx` using a reusable `game`.
///
/// When `record` is false, skips allocating the demo stream (much faster for
/// bulk search). Re-run with `record: true` to capture the winner.
pub fn run_trial(
    game: &mut Game,
    level_idx: usize,
    policy: Policy,
    max_tics: u32,
    record: bool,
) -> TrialResult {
    game.prepare_ai_run(level_idx, policy.seed);
    game.god = policy.god;
    let mut brain = Brain::fair(policy.clone(), game);

    let mut demo = if record {
        let mut d = Demo::begin(game);
        d.clear_actors = false;
        d.ai_policy = Some((&policy).into());
        Some(d)
    } else {
        None
    };

    // Abort only if the player is literally frozen in place (not combat retreat
    // or key detours — those often move *away* from the elevator briefly).
    let mut last = (game.player.x, game.player.y);
    let mut frozen = 0u32;

    for t in 0..max_tics {
        if game.screen != GameScreen::Playing {
            break;
        }
        let input = brain.tick(game);
        if let Some(ref mut d) = demo {
            d.push(&input);
        }
        game.update(DT, &input);

        if game.screen != GameScreen::Playing {
            if trial_completed(game) {
                return TrialResult::success(policy, t + 1, game, demo);
            }
            return TrialResult::fail(policy, t + 1, game);
        }

        let moved = (game.player.x - last.0).abs() + (game.player.y - last.1).abs();
        let active_combat = input.fire || input.strafe_left || input.strafe_right || input.back;
        if moved < 0.01 && !active_combat {
            frozen += 1;
            // Completely frozen (not circle-strafe — that still moves). Full-clear
            // runs need more patience at doors / secret walls.
            let limit = if policy.hunt_kills || policy.seek_secrets {
                70 * 8
            } else {
                70 * 3
            };
            if frozen > limit {
                return TrialResult::fail(policy, t + 1, game);
            }
        } else {
            frozen = 0;
            last = (game.player.x, game.player.y);
        }
    }
    TrialResult::fail(policy, max_tics, game)
}

/// Record a full demo for one policy.
pub fn run_trial_record(
    game: &mut Game,
    level_idx: usize,
    policy: Policy,
    max_tics: u32,
) -> TrialResult {
    run_trial(game, level_idx, policy, max_tics, true)
}

fn trial_completed(game: &Game) -> bool {
    game.victory
        || matches!(
            game.screen,
            GameScreen::Intermission
                | GameScreen::Victory
                | GameScreen::DeathCam
                | GameScreen::GetPsyched
        )
}

/// Re-score an on-disk demo by replaying it. Returns `None` if it does not
/// complete the floor (corrupt / partial / death).
pub fn evaluate_demo(demo: Demo) -> Option<TrialResult> {
    let level_idx = demo.level_idx;
    let mut game = Game::new(level_idx);
    game.load_demo_state(&demo);
    let mut policy = demo
        .ai_policy
        .as_ref()
        .map(Policy::from)
        .unwrap_or_else(|| Policy::speedrun(demo.rng_index as u32));
    policy.god = demo.god;
    let n = demo.tics.len();
    for (i, input) in demo.tics.iter().enumerate() {
        if game.screen != GameScreen::Playing {
            break;
        }
        game.update(DT, input);
        if game.screen != GameScreen::Playing {
            if trial_completed(&game) {
                return Some(TrialResult::success(
                    policy,
                    (i + 1) as u32,
                    &game,
                    Some(demo),
                ));
            }
            return None;
        }
    }
    // Stream ended while still Playing — not a full clear.
    let _ = n;
    None
}

/// Search for the best fair completion of a floor.
///
/// Strategy:
/// 1. Optional **warm start** from a previous champion (`warm`)
/// 2. **Scatter**: multi-threaded random policies, no demo recording
/// 3. **Climb**: mutate around the best policy found so far
/// 4. **Record**: re-sim a new champion with demo capture when needed
#[allow(clippy::too_many_arguments)]
pub fn search_level(
    level_idx: usize,
    iters: u64,
    threads: usize,
    max_tics: u32,
    progress_every: u64,
    search_seed: u64,
    warm: Option<TrialResult>,
    god: bool,
    focus: SearchFocus,
) -> TrialResult {
    let threads = threads.max(1);
    let iters = iters.max(1);

    let init_tics = warm
        .as_ref()
        .filter(|w| w.completed)
        .map(|w| u64::from(w.tics))
        .unwrap_or(u64::from(max_tics));
    let init_completed = u64::from(warm.as_ref().is_some_and(|w| w.completed));

    let best: Arc<Mutex<Option<TrialResult>>> = Arc::new(Mutex::new(warm));
    // Soft time hint only (points dominate fitness); still allows longer high-kill runs.
    let best_tics = Arc::new(AtomicU64::new(init_tics));
    let counter = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(init_completed));

    let scatter = (iters * 4 / 5).max(1);
    run_scatter(
        level_idx,
        scatter,
        threads,
        max_tics,
        progress_every,
        &best,
        &best_tics,
        &counter,
        &completed,
        iters,
        search_seed,
        god,
        focus,
    );

    let climb = (iters - scatter).max(threads as u64);
    run_climb(
        level_idx,
        climb,
        threads,
        max_tics,
        progress_every,
        &best,
        &best_tics,
        &counter,
        &completed,
        iters,
        search_seed,
        god,
        focus,
    );

    let mut champion = best
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| TrialResult::fail(Policy::default(), 0, &Game::new(level_idx)));

    if champion.completed && champion.demo.is_none() {
        let mut game = Game::new(level_idx);
        let cap = champion
            .tics
            .saturating_add(140)
            .min(max_tics)
            .max(champion.tics.saturating_add(35));
        let mut recorded = run_trial(&mut game, level_idx, champion.policy.clone(), cap, true);
        focus.rank(&mut recorded);
        if recorded.completed && recorded.fitness >= champion.fitness {
            champion = recorded;
        }
    }

    champion
}

fn consider_best(
    best: &Mutex<Option<TrialResult>>,
    best_tics: &AtomicU64,
    completed: &AtomicU64,
    result: TrialResult,
) {
    if result.completed {
        completed.fetch_add(1, Ordering::Relaxed);
        best_tics.fetch_min(u64::from(result.tics), Ordering::Relaxed);
    }
    let mut g = best.lock().unwrap();
    let replace = match g.as_ref() {
        None => true,
        Some(cur) => result.fitness > cur.fitness,
    };
    if replace {
        *g = Some(result);
    }
}

fn live_cap(best_tics: &AtomicU64, max_tics: u32) -> u32 {
    // Points (kills/secrets) dominate fitness, so do not hard-cut at the
    // champion's length — a longer higher-kill run must be allowed. Use a soft
    // ceiling so pure-wander trials still die sooner once something works.
    let t = best_tics.load(Ordering::Relaxed) as u32;
    if t >= max_tics {
        return max_tics;
    }
    // 2× champion length, at least +20s, never above max_tics.
    t.saturating_mul(2)
        .max(t.saturating_add(70 * 20))
        .min(max_tics)
}

#[allow(clippy::too_many_arguments)]
fn run_scatter(
    level_idx: usize,
    iters: u64,
    threads: usize,
    max_tics: u32,
    progress_every: u64,
    best: &Arc<Mutex<Option<TrialResult>>>,
    best_tics: &Arc<AtomicU64>,
    counter: &Arc<AtomicU64>,
    completed: &Arc<AtomicU64>,
    total_display: u64,
    search_seed: u64,
    god: bool,
    focus: SearchFocus,
) {
    let chunk = iters.div_ceil(threads as u64);
    std::thread::scope(|scope| {
        for t in 0..threads {
            let best = Arc::clone(best);
            let best_tics = Arc::clone(best_tics);
            let counter = Arc::clone(counter);
            let completed = Arc::clone(completed);
            let start = t as u64 * chunk;
            let end = ((t as u64 + 1) * chunk).min(iters);
            // Every worker takes a stripe of the deterministic speed probes.
            // Previously all 48 ran serially on worker zero, making even a
            // tiny forge request spend most of its time before the search.
            let has_probe = t < 48;
            if start >= end && !has_probe {
                continue;
            }
            scope.spawn(move || {
                let mut game = Game::new(level_idx);
                for s in (t as u32..48).step_by(threads) {
                    let cap = live_cap(&best_tics, max_tics);
                    let seed = search_trial_id(search_seed, u64::from(s)) as u32;
                    let policy = focus.configure(Policy::speedrun(seed), god);
                    let mut r = run_trial(&mut game, level_idx, policy, cap, false);
                    focus.rank(&mut r);
                    consider_best(&best, &best_tics, &completed, r);
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                for i in start..end {
                    let trial_id = search_trial_id(search_seed, i.wrapping_add(48));
                    let policy = focus.configure(Policy::from_trial(trial_id), god);
                    let cap = live_cap(&best_tics, max_tics);
                    let mut result = run_trial(&mut game, level_idx, policy, cap, false);
                    focus.rank(&mut result);
                    consider_best(&best, &best_tics, &completed, result);
                    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    maybe_progress(
                        n,
                        total_display,
                        progress_every,
                        &best,
                        &completed,
                        &best_tics,
                    );
                }
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn run_climb(
    level_idx: usize,
    iters: u64,
    threads: usize,
    max_tics: u32,
    progress_every: u64,
    best: &Arc<Mutex<Option<TrialResult>>>,
    best_tics: &Arc<AtomicU64>,
    counter: &Arc<AtomicU64>,
    completed: &Arc<AtomicU64>,
    total_display: u64,
    search_seed: u64,
    god: bool,
    focus: SearchFocus,
) {
    let root = best
        .lock()
        .unwrap()
        .as_ref()
        .map(|r| r.policy.clone())
        .unwrap_or_default();

    let chunk = iters.div_ceil(threads as u64);
    std::thread::scope(|scope| {
        for t in 0..threads {
            let best = Arc::clone(best);
            let best_tics = Arc::clone(best_tics);
            let counter = Arc::clone(counter);
            let completed = Arc::clone(completed);
            let root = root.clone();
            let start = t as u64 * chunk;
            let end = ((t as u64 + 1) * chunk).min(iters);
            if start >= end {
                continue;
            }
            scope.spawn(move || {
                let mut game = Game::new(level_idx);
                let mut local = root.clone();
                for i in start..end {
                    if i.is_multiple_of(16)
                        && let Some(ref g) = *best.lock().unwrap()
                    {
                        local = g.policy.clone();
                    }
                    let mutation = search_trial_id(
                        search_seed ^ 0xD1B5_4A32_D192_ED03,
                        i.wrapping_add(t as u64 * 0x10000),
                    );
                    let policy = focus.configure(local.mutate(mutation), god);
                    let cap = live_cap(&best_tics, max_tics);
                    let mut result = run_trial(&mut game, level_idx, policy.clone(), cap, false);
                    focus.rank(&mut result);
                    let champ_fit = best
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|b| b.fitness)
                        .unwrap_or(i64::MIN);
                    if result.completed && result.fitness > champ_fit {
                        local = policy;
                    }
                    consider_best(&best, &best_tics, &completed, result);
                    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    maybe_progress(
                        n,
                        total_display,
                        progress_every,
                        &best,
                        &completed,
                        &best_tics,
                    );
                }
            });
        }
    });
}

/// SplitMix64-style deterministic mixing. A new forge `search_seed` produces a
/// disjoint-looking trial stream while an explicitly repeated seed remains
/// reproducible.
fn search_trial_id(search_seed: u64, trial: u64) -> u64 {
    let mut z = search_seed
        .wrapping_add(trial.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn maybe_progress(
    n: u64,
    total: u64,
    every: u64,
    best: &Mutex<Option<TrialResult>>,
    completed: &AtomicU64,
    best_tics: &AtomicU64,
) {
    if every == 0 || !n.is_multiple_of(every) {
        return;
    }
    let b = best.lock().unwrap();
    let done = completed.load(Ordering::Relaxed);
    let _ = (total, best_tics); // total is a budget hint only; workers may overshoot
    if let Some(ref r) = *b {
        if r.completed {
            eprintln!(
                "\r  searching…  {n} trials  ·  {done} finished  ·  best score {}  ({} kills, {} secrets, {:.0}s play)   ",
                r.points(),
                r.kills,
                r.secrets,
                r.tics as f32 / 70.0,
            );
        } else {
            eprintln!(
                "\r  searching…  {n} trials  ·  no finish yet  ·  best partial {} kills, {} secrets   ",
                r.kills, r.secrets
            );
        }
    } else {
        eprintln!("\r  searching…  {n} trials  ·  no finish yet   ");
    }
}

// =============================================================================
// Map helpers
// =============================================================================

fn tile_of(game: &Game) -> (i32, i32) {
    (game.player.x.floor() as i32, game.player.y.floor() as i32)
}

fn plane0(world: &World, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
        return 1;
    }
    world.level.plane0[y as usize * MAP + x as usize]
}

fn plane1(world: &World, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
        return 0;
    }
    world.level.plane1[y as usize * MAP + x as usize]
}

fn remaining_enemies(game: &Game) -> impl Iterator<Item = (i32, i32)> + '_ {
    game.actors.list.iter().filter_map(|a| {
        if a.dead || a.kind.is_ghost() {
            None
        } else {
            Some((a.x.floor() as i32, a.y.floor() as i32))
        }
    })
}

fn nearest_enemy_tile(game: &Game) -> Option<(i32, i32)> {
    nearest_enemy_filtered(game, |_| true)
}

fn nearest_non_boss_enemy(game: &Game) -> Option<(i32, i32)> {
    nearest_enemy_filtered(game, |k| !is_end_boss(k))
}

fn nearest_enemy_filtered(game: &Game, pred: impl Fn(Kind) -> bool) -> Option<(i32, i32)> {
    let start = tile_of(game);
    let mut best: Option<(usize, (i32, i32))> = None;
    for a in &game.actors.list {
        if a.dead || a.kind.is_ghost() || !pred(a.kind) {
            continue;
        }
        let (tx, ty) = (a.x.floor() as i32, a.y.floor() as i32);
        let Some(stand) = neighbors(tx, ty)
            .into_iter()
            .chain(std::iter::once((tx, ty)))
            .find(|&(x, y)| walkable(game, x, y))
        else {
            continue;
        };
        let Some(path) = bfs_path(game, start, stand) else {
            continue;
        };
        let cost = path.len();
        if best.map(|(c, _)| cost < c).unwrap_or(true) {
            best = Some((cost, (tx, ty)));
        }
    }
    best.map(|(_, t)| t)
}

/// Nearest remaining secret push-wall: (wall tile, stand tile beside it).
fn nearest_secret(game: &Game, skipped: &HashSet<(i32, i32)>) -> Option<((i32, i32), (i32, i32))> {
    type SecretCand = (usize, (i32, i32), (i32, i32)); // cost, wall, stand
    let start = tile_of(game);
    let mut best: Option<SecretCand> = None;
    for y in 0..MAP as i32 {
        for x in 0..MAP as i32 {
            if plane1(&game.world, x, y) != PUSHABLE_TILE {
                continue;
            }
            if skipped.contains(&(x, y)) {
                continue;
            }
            // Stand on a walkable neighbour so Cmd_Use faces the wall.
            for stand in neighbors(x, y) {
                if !walkable(game, stand.0, stand.1) {
                    continue;
                }
                // The wall moves away from the stand. Reject a side whose
                // destination is blocked; trying that side would make the
                // controller discard an otherwise usable secret.
                let dx = x - stand.0;
                let dy = y - stand.1;
                let dest = (x + dx, y + dy);
                if dest.0 < 0 || dest.1 < 0 || dest.0 >= MAP as i32 || dest.1 >= MAP as i32 {
                    continue;
                }
                let dest_idx = dest.1 as usize * MAP + dest.0 as usize;
                if game.world.blocks_move(dest.0, dest.1) || game.world.actor_blocked[dest_idx] {
                    continue;
                }
                let Some(path) = bfs_path(game, start, stand) else {
                    continue;
                };
                let cost = path.len();
                if best.map(|(c, _, _)| cost < c).unwrap_or(true) {
                    best = Some((cost, (x, y), stand));
                }
            }
        }
    }
    best.map(|(_, wall, stand)| (wall, stand))
}

fn is_door_tile(world: &World, x: i32, y: i32) -> bool {
    (90..=101).contains(&plane0(world, x, y))
}

fn door_passable(world: &World, x: i32, y: i32) -> bool {
    world
        .door_lookup(x, y)
        .is_some_and(|i| world.door_open_enough(i))
}

/// Closed door we should operate now: the path cursor door, or an adjacent
/// closed door on the path. (Using from far away never works — Cmd_Use is
/// one tile in front of the player.)
fn next_closed_door(game: &Game, path: &[(i32, i32)], path_i: usize) -> Option<(i32, i32)> {
    let here = tile_of(game);
    for &(x, y) in path.iter().skip(path_i).take(3) {
        if !is_door_tile(&game.world, x, y) || door_passable(&game.world, x, y) {
            continue;
        }
        let adjacent = (here.0 - x).abs() + (here.1 - y).abs() == 1;
        let is_cursor = path.get(path_i) == Some(&(x, y));
        if adjacent || is_cursor {
            return Some((x, y));
        }
        // Door further along — walk there first; don't try to use yet.
        return None;
    }
    None
}

/// True if every door on path[0..=idx] is open enough to walk through.
fn path_prefix_clear(game: &Game, path: &[(i32, i32)], idx: usize) -> bool {
    let end = idx.min(path.len().saturating_sub(1));
    for &(x, y) in path.iter().take(end + 1) {
        if is_door_tile(&game.world, x, y) && !door_passable(&game.world, x, y) {
            return false;
        }
    }
    true
}

/// Elevator stand reachable with the given key mask (for planning unlock order).
fn elev_reachable(game: &Game, keys: u8) -> bool {
    let start = tile_of(game);
    for &(ex, ey) in &find_elevators(&game.world) {
        for stand in neighbors(ex, ey) {
            if walkable_keys(game, stand.0, stand.1, keys)
                && bfs_path_keys(game, start, stand, keys).is_some()
            {
                return true;
            }
        }
    }
    false
}

fn walkable(game: &Game, x: i32, y: i32) -> bool {
    walkable_keys(game, x, y, game.keys)
}

fn walkable_keys(game: &Game, x: i32, y: i32, keys: u8) -> bool {
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
        return false;
    }
    // Ordinary enemies can be fought out of a choke point, but invulnerable
    // ghosts cannot. Treat only ghosts as permanent path obstacles; making
    // every live actor solid causes the planner to declare busy doorways
    // unreachable instead of engaging their guards.
    let ghost_blocked = game.actors.list.iter().any(|actor| {
        !actor.dead
            && actor.kind.is_ghost()
            && actor.x.floor() as i32 == x
            && actor.y.floor() as i32 == y
    });
    if is_door_tile(&game.world, x, y) {
        if let Some(idx) = game.world.door_lookup(x, y) {
            let lock = game.world.doors[idx].lock;
            if lock != 0 && keys & lock == 0 {
                return false;
            }
        }
        return !ghost_blocked;
    }
    !ghost_blocked && !game.world.blocks_move(x, y)
}

fn neighbors(x: i32, y: i32) -> Vec<(i32, i32)> {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .map(|(dx, dy)| (x + dx, y + dy))
        .filter(|&(nx, ny)| nx >= 0 && ny >= 0 && nx < MAP as i32 && ny < MAP as i32)
        .collect()
}

fn find_elevators(world: &World) -> Vec<(i32, i32)> {
    let mut v = Vec::new();
    for y in 0..MAP as i32 {
        for x in 0..MAP as i32 {
            if plane0(world, x, y) == 21 {
                v.push((x, y));
            }
        }
    }
    v
}

fn bfs_path(game: &Game, start: (i32, i32), goal: (i32, i32)) -> Option<Vec<(i32, i32)>> {
    bfs_path_keys(game, start, goal, game.keys)
}

fn bfs_path_keys(
    game: &Game,
    start: (i32, i32),
    goal: (i32, i32),
    keys: u8,
) -> Option<Vec<(i32, i32)>> {
    if start == goal {
        return Some(vec![start]);
    }
    let mut q = VecDeque::new();
    let mut prev: Vec<Option<(i32, i32)>> = vec![None; MAP * MAP];
    let mut seen = HashSet::new();
    q.push_back(start);
    seen.insert(start);
    while let Some(cur) = q.pop_front() {
        for n in neighbors(cur.0, cur.1) {
            if seen.contains(&n) {
                continue;
            }
            // Allow stepping onto goal even if weird; else walkable.
            if n != goal && !walkable_keys(game, n.0, n.1, keys) {
                continue;
            }
            seen.insert(n);
            prev[n.1 as usize * MAP + n.0 as usize] = Some(cur);
            if n == goal {
                let mut path = vec![goal];
                let mut c = goal;
                while c != start {
                    c = prev[c.1 as usize * MAP + c.0 as usize].unwrap();
                    path.push(c);
                }
                path.reverse();
                return Some(path);
            }
            q.push_back(n);
        }
    }
    None
}

fn look_ahead_index(game: &Game, path: &[(i32, i32)], i: usize) -> usize {
    if i >= path.len() {
        return i;
    }
    let (sx, sy) = path[i];
    let mut j = i;
    while j + 1 < path.len() {
        let (nx, ny) = path[j + 1];
        let same_row = ny == sy && path[i..=j + 1].iter().all(|&(_, y)| y == sy);
        let same_col = nx == sx && path[i..=j + 1].iter().all(|&(x, _)| x == sx);
        if !(same_row || same_col) {
            break;
        }
        if j + 1 > i && is_door_tile(&game.world, nx, ny) && !door_passable(&game.world, nx, ny) {
            break;
        }
        j += 1;
    }
    j
}

fn has_los(world: &World, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist < 0.5 {
        return true;
    }
    let steps = (dist * 4.0).ceil() as i32;
    let steps = steps.clamp(2, 64);
    for s in 1..steps {
        let t = s as f32 / steps as f32;
        let x = x0 + dx * t;
        let y = y0 + dy * t;
        let tx = x.floor() as i32;
        let ty = y.floor() as i32;
        if is_door_tile(world, tx, ty) {
            if !door_passable(world, tx, ty) {
                return false;
            }
            continue;
        }
        if world.blocks_move(tx, ty) {
            return false;
        }
    }
    true
}

#[cfg(test)]
fn nearest_key_tile(game: &Game, key: u8) -> Option<(i32, i32)> {
    nearest_bonus(game, |b| match (key, b) {
        (k, Bonus::Key1) if k & KEY_GOLD != 0 => true,
        (k, Bonus::Key2) if k & KEY_SILVER != 0 => true,
        _ => false,
    })
}

/// Closest key that can actually be approached with the keys currently held.
///
/// Manhattan-nearest is insufficient on maps where the gold key sits behind a
/// silver door (or vice versa): repeatedly selecting that key produces an
/// empty path and a frozen bot.
fn nearest_reachable_key_tile(game: &Game, key: u8) -> Option<(i32, i32)> {
    let start = tile_of(game);
    let mut best: Option<(usize, (i32, i32))> = None;
    for s in &game.world.statics {
        if s.picked {
            continue;
        }
        let matches_key = matches!(
            (key, s.bonus),
            (k, Some(Bonus::Key1)) if k & KEY_GOLD != 0
        ) || matches!(
            (key, s.bonus),
            (k, Some(Bonus::Key2)) if k & KEY_SILVER != 0
        );
        if !matches_key {
            continue;
        }
        let target = (s.x.floor() as i32, s.y.floor() as i32);
        let Some(stand) = nearest_stand(game, start, target) else {
            continue;
        };
        let cost = bfs_path(game, start, stand)
            .map(|path| path.len())
            .unwrap_or(usize::MAX);
        if best.map(|(old, _)| cost < old).unwrap_or(true) {
            best = Some((cost, target));
        }
    }
    best.map(|(_, target)| target)
}

fn nearest_ammo_tile(game: &Game) -> Option<(i32, i32)> {
    nearest_bonus(game, |b| {
        matches!(
            b,
            Bonus::Clip | Bonus::Clip2 | Bonus::MachineGun | Bonus::ChainGun | Bonus::Clip25
        )
    })
}

fn nearest_bonus(game: &Game, pred: impl Fn(Bonus) -> bool) -> Option<(i32, i32)> {
    let (px, py) = tile_of(game);
    let mut best: Option<(i32, (i32, i32))> = None;
    for s in &game.world.statics {
        if s.picked {
            continue;
        }
        let Some(b) = s.bonus else { continue };
        if !pred(b) {
            continue;
        }
        let t = (s.x.floor() as i32, s.y.floor() as i32);
        let d = (t.0 - px).abs() + (t.1 - py).abs();
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, t));
        }
    }
    best.map(|(_, t)| t)
}

/// Nearest useful pickup inside/just beyond a newly opened secret area.
fn nearest_secret_loot(game: &Game, anchor: (i32, i32), dir: (i32, i32)) -> Option<(i32, i32)> {
    let start = tile_of(game);
    let mut best: Option<(usize, (i32, i32))> = None;
    for s in &game.world.statics {
        if s.picked {
            continue;
        }
        let Some(bonus) = s.bonus else { continue };
        let target = (s.x.floor() as i32, s.y.floor() as i32);
        let rel = (target.0 - anchor.0, target.1 - anchor.1);
        // Stay on the hidden-room side of the wall and within a modest room
        // radius, so this phase does not turn into omniscient map-wide looting.
        if rel.0 * dir.0 + rel.1 * dir.1 < 0
            || rel.0.abs() + rel.1.abs() > 10
            || !bonus_useful(game, bonus)
        {
            continue;
        }
        let Some(path) = bfs_path(game, start, target) else {
            continue;
        };
        let cost = path.len();
        if best.map(|(c, _)| cost < c).unwrap_or(true) {
            best = Some((cost, target));
        }
    }
    best.map(|(_, target)| target)
}

fn bonus_useful(game: &Game, bonus: Bonus) -> bool {
    match bonus {
        Bonus::Alpo | Bonus::Food | Bonus::FirstAid => game.health < 100,
        Bonus::Clip | Bonus::Clip2 | Bonus::Clip25 => game.ammo < 99,
        Bonus::Key1 => game.keys & KEY_GOLD == 0,
        Bonus::Key2 => game.keys & KEY_SILVER == 0,
        // Weapons, treasure, gibs, full-heals, and the Spear are always worth
        // walking over; their game-side pickup rules decide the exact effect.
        _ => true,
    }
}

/// Walkable stand tile for a goal that may sit on a wall/block (keys, clips).
fn nearest_stand(game: &Game, start: (i32, i32), goal: (i32, i32)) -> Option<(i32, i32)> {
    if walkable(game, goal.0, goal.1) && bfs_path(game, start, goal).is_some() {
        return Some(goal);
    }
    let mut best: Option<(usize, (i32, i32))> = None;
    for stand in neighbors(goal.0, goal.1) {
        if !walkable(game, stand.0, stand.1) {
            continue;
        }
        let Some(path) = bfs_path(game, start, stand) else {
            continue;
        };
        let cost = path.len();
        if best.map(|(c, _)| cost < c).unwrap_or(true) {
            best = Some((cost, stand));
        }
    }
    // Also try 2-tile ring if the key is fully walled in on one side.
    if best.is_none() {
        for (dx, dy) in [
            (2, 0),
            (-2, 0),
            (0, 2),
            (0, -2),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let stand = (goal.0 + dx, goal.1 + dy);
            if !walkable(game, stand.0, stand.1) {
                continue;
            }
            let Some(path) = bfs_path(game, start, stand) else {
                continue;
            };
            let cost = path.len();
            if best.map(|(c, _)| cost < c).unwrap_or(true) {
                best = Some((cost, stand));
            }
        }
    }
    best.map(|(_, s)| s)
}

fn first_missing_key(game: &Game) -> Option<u8> {
    if game.keys & KEY_GOLD == 0 && nearest_reachable_key_tile(game, KEY_GOLD).is_some() {
        return Some(KEY_GOLD);
    }
    if game.keys & KEY_SILVER == 0 && nearest_reachable_key_tile(game, KEY_SILVER).is_some() {
        return Some(KEY_SILVER);
    }
    None
}

fn norm_angle(mut a: f32) -> f32 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    a
}

#[cfg(test)]
mod multi_goal_smoke {
    use super::*;
    #[test]
    fn full_clear_e1m1_farms_kills() {
        // Multi-goal hunt should rack up several kills in under a minute of sim.
        let mut game = Game::new(0);
        let mut best = 0;
        for seed in 1..=20 {
            let r = run_trial(
                &mut game,
                0,
                Policy::full_clear(seed * 7919),
                70 * 120,
                false,
            );
            best = best.max(r.kills);
        }
        assert!(
            best >= 5,
            "expected full-clear hunt to farm kills, best was {best}"
        );
    }

    #[test]
    fn e1m5_speedrun_can_finish() {
        // Gold-key + long elev path; Baby skill (prepare_ai_run) so fair any% lives.
        let mut game = Game::new(4);
        let mut completed = false;
        for seed in 1u32..200 {
            let r = run_trial(&mut game, 4, Policy::speedrun(seed * 9973), 70 * 300, false);
            if r.completed {
                completed = true;
                break;
            }
        }
        assert!(completed, "e1m5 never finished");
    }

    #[test]
    fn e1m1_secret_policy_activates_a_pushwall() {
        let result = search_level(0, 50, 4, 70 * 180, 0, 1, None, false, SearchFocus::Secrets);
        assert!(
            result.secrets >= 1,
            "secret-focused policy found {}/{} secrets",
            result.secrets,
            result.secret_total
        );
        let mut game = Game::new(0);
        game.prepare_ai_run(0, 47);
        let mut brain = Brain::fair(Policy::speedrun(47), &game);
        for _ in 0..500 {
            let input = brain.tick(&game);
            assert_eq!(
                input.turn_delta, 0.0,
                "AI must use player-rate turn keys, not direct angle snaps"
            );
            game.update(DT, &input);
        }
    }

    #[test]
    fn e1m9_mortal_boss_can_finish_with_carryover_loadout() {
        let mut game = Game::new(8);
        let mut completed = false;
        for seed in 1..=100 {
            let r = run_trial(&mut game, 8, Policy::speedrun(seed * 7919), 70 * 120, false);
            if r.completed && r.health > 0 {
                completed = true;
                break;
            }
        }
        assert!(completed, "mortal E1M9 boss search did not finish");
    }

    #[test]
    fn dry_combat_swings_the_selected_knife() {
        let mut game = Game::new(0);
        game.prepare_ai_run(0, 1);
        let (ax, ay, stand) = game
            .actors
            .list
            .iter()
            .filter(|actor| !actor.dead && !actor.kind.is_ghost())
            .find_map(|actor| {
                let tile = (actor.x.floor() as i32, actor.y.floor() as i32);
                neighbors(tile.0, tile.1)
                    .into_iter()
                    .find(|&(x, y)| walkable(&game, x, y))
                    .map(|stand| (actor.x, actor.y, stand))
            })
            .expect("E1M1 has an approachable guard");
        game.player.x = stand.0 as f32 + 0.5;
        game.player.y = stand.1 as f32 + 0.5;
        game.player.angle = (ay - game.player.y).atan2(ax - game.player.x);
        game.ammo = 0;

        let mut brain = Brain::fair(Policy::speedrun(1), &game);
        let input = brain
            .tick_combat_range(&game, 2.0)
            .expect("guard is in knife range");
        assert_eq!(input.select_weapon, Some(0));
        assert!(input.fire, "dry AI selected the knife but did not swing");
    }

    #[test]
    #[ignore = "diagnostic map probe"]
    fn probe_fail_levels() {
        let fails = [
            7, 8, 11, 12, 17, 18, 21, 22, 23, 27, 28, 29, 34, 36, 37, 38, 43, 44, 46, 47, 50, 51,
            53, 54, 55, 56, 57, 58, 59,
        ];
        for &level in &fails {
            let mut game = Game::new(level);
            game.prepare_ai_run(level, 1);
            let elevs = find_elevators(&game.world);
            let start = tile_of(&game);
            let mut elev_path = false;
            for &(ex, ey) in &elevs {
                for st in neighbors(ex, ey) {
                    if walkable(&game, st.0, st.1) && bfs_path(&game, start, st).is_some() {
                        elev_path = true;
                    }
                }
            }
            game.keys = KEY_GOLD | KEY_SILVER;
            let mut elev_keys = false;
            let mut elev_len = 0usize;
            for &(ex, ey) in &elevs {
                for st in neighbors(ex, ey) {
                    if walkable(&game, st.0, st.1)
                        && let Some(p) = bfs_path(&game, start, st)
                    {
                        elev_keys = true;
                        elev_len = elev_len.max(p.len());
                    }
                }
            }
            let gold = nearest_key_tile(&game, KEY_GOLD);
            let silver = nearest_key_tile(&game, KEY_SILVER);
            let reachable_gold = nearest_reachable_key_tile(&game, KEY_GOLD);
            let reachable_silver = nearest_reachable_key_tile(&game, KEY_SILVER);
            let secrets = nearest_secret(&game, &HashSet::new()).is_some();
            let name = format!("e{}m{}", level / 10 + 1, level % 10 + 1);
            eprintln!(
                "{name}: elevs={} path0={elev_path} pathKeys={elev_keys} elevLen={elev_len} gold={gold:?}/{reachable_gold:?} silver={silver:?}/{reachable_silver:?} secret={secrets} enemies={}",
                elevs.len(),
                remaining_enemies(&game).count()
            );
        }
    }

    #[test]
    #[ignore = "expensive all-map search probe"]
    fn probe_all_levels_finish() {
        let mut game = Game::new(0);
        let n = game.maps.num_levels().min(60);
        let mut fail = Vec::new();
        let mut ok = 0usize;
        for level in 0..n {
            let mut done = false;
            let mut best_k = 0i32;
            let mut best_t = 0u32;
            // Prefer speedrun (finish first); a few full_clear seeds too.
            for seed in 1u32..50 {
                let pol = if seed % 5 == 0 {
                    Policy::full_clear(seed * 7919)
                } else {
                    Policy::speedrun(seed * 7919)
                };
                let r = run_trial(&mut game, level, pol, 70 * 180, false);
                best_k = best_k.max(r.kills);
                if r.completed {
                    done = true;
                    best_t = r.tics;
                    break;
                }
            }
            let name = format!("e{}m{}", level / 10 + 1, level % 10 + 1);
            if done {
                ok += 1;
                eprintln!("OK  {name} tics={best_t} k={best_k}");
            } else {
                fail.push(level);
                eprintln!("FAIL {name} best_k={best_k}");
            }
        }
        eprintln!("summary: {ok}/{n} ok, fail={fail:?}");
        assert!(fail.is_empty() || fail.len() < n, "all failed");
    }
}

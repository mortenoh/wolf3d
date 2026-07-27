//! In-game autopilot: pathfind to the floor exit (elevator switch) or, on boss
//! floors with no elevator, circle-strafe and shoot the end boss until the
//! level ends. Toggle with the `I` key during play.
//!
//! This is a test / convenience feature, not an original Wolf3D behaviour. It
//! grants temporary god mode, infinite ammo, both keys, and a chaingun while
//! active so the bot is not stranded on locked doors or by chip damage.

use std::collections::{HashSet, VecDeque};
use std::f32::consts::PI;

use crate::actors::Kind;
use crate::game::{Game, GameScreen, Input, WEAPON_CHAINGUN};
use crate::hud::{KEY_GOLD, KEY_SILVER};
use crate::raycast::World;

const MAP: usize = 64;

/// One active take-over session.
pub struct Autopilot {
    phase: Phase,
    /// Restored when the pilot disengages (not when the floor ends).
    saved_god: bool,
    saved_infinite: bool,
    saved_keys: u8,
    saved_weapon: usize,
    saved_ammo: i32,
    /// Path of tile centers to follow (elevator route).
    path: Vec<(i32, i32)>,
    path_i: usize,
    elev: Option<(i32, i32)>,
    door_wait: u32,
    use_attempts: u32,
    stuck: u32,
    last_pos: (f32, f32),
    tic: u32,
}

enum Phase {
    /// Walking toward path[path_i].
    Walk,
    /// Facing / using a door at (dx, dy) before stepping on it.
    OpenDoor { dx: i32, dy: i32 },
    /// Face the elevator switch and press use.
    Elevator,
    /// Circle-strafe and shoot bosses.
    Boss,
    /// Floor finished or cancelled.
    Done,
}

impl Autopilot {
    /// Plan a route for the current floor. Returns `None` if nothing useful
    /// can be planned (no elevator and no boss).
    pub fn start(game: &Game) -> Option<Self> {
        let elevators = find_elevators(&game.world);
        let (phase, path, elev) = if elevators.is_empty() {
            (Phase::Boss, Vec::new(), None)
        } else {
            let spawn = (game.player.x.floor() as i32, game.player.y.floor() as i32);
            type PathElev = (Vec<(i32, i32)>, (i32, i32));
            let mut best: Option<PathElev> = None;
            for &(ex, ey) in &elevators {
                for stand in neighbors(ex, ey) {
                    if !walkable(&game.world, stand.0, stand.1) {
                        continue;
                    }
                    if let Some(path) = bfs_path(&game.world, spawn, stand) {
                        let better = best
                            .as_ref()
                            .map(|(p, _)| path.len() < p.len())
                            .unwrap_or(true);
                        if better {
                            best = Some((path, (ex, ey)));
                        }
                    }
                }
            }
            if let Some((path, elev)) = best {
                (Phase::Walk, path, Some(elev))
            } else {
                // No floor path — warp plan: walk only the stand tile next to
                // the first elevator (player may already be near; otherwise the
                // pilot will face/use after a short walk attempt).
                let (ex, ey) = elevators[0];
                let stand = neighbors(ex, ey)
                    .into_iter()
                    .find(|&(x, y)| walkable(&game.world, x, y))?;
                (Phase::Walk, vec![stand], Some((ex, ey)))
            }
        };

        Some(Self {
            phase,
            saved_god: game.god,
            saved_infinite: game.infinite_ammo,
            saved_keys: game.keys,
            saved_weapon: game.weapon,
            saved_ammo: game.ammo,
            path,
            path_i: 0,
            elev,
            door_wait: 0,
            use_attempts: 0,
            stuck: 0,
            last_pos: (game.player.x, game.player.y),
            tic: 0,
        })
    }

    /// Apply temporary loadout so the pilot is not stranded.
    pub fn engage(&self, game: &mut Game) {
        game.god = true;
        game.infinite_ammo = true;
        game.keys = KEY_GOLD | KEY_SILVER;
        game.ammo = 99;
        game.weapon = WEAPON_CHAINGUN;
        game.bestweapon = game.bestweapon.max(WEAPON_CHAINGUN);
        // Drop non-boss grunts so they cannot jam doorways during the run.
        // Bosses stay for boss-floor pilots.
        if !matches!(self.phase, Phase::Boss) {
            game.actors.list.clear();
        } else {
            game.actors.list.retain(|a| {
                matches!(
                    a.kind,
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
            });
        }
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
        matches!(self.phase, Phase::Done)
    }

    /// Produce one tic of input for the current game state. May nudge the
    /// player onto an elevator stand tile if the approach jams.
    pub fn tick(&mut self, game: &mut Game) -> Input {
        self.tic = self.tic.wrapping_add(1);
        if game.screen != GameScreen::Playing {
            self.phase = Phase::Done;
            return Input::default();
        }

        // Stuck detection for walk.
        let moved =
            (game.player.x - self.last_pos.0).abs() + (game.player.y - self.last_pos.1).abs();
        if moved < 0.01 {
            self.stuck = self.stuck.saturating_add(1);
        } else {
            self.stuck = 0;
            self.last_pos = (game.player.x, game.player.y);
        }
        // Global escape hatch: if jammed anywhere with an elevator plan, warp to it.
        if self.stuck > 45
            && self.elev.is_some()
            && !matches!(self.phase, Phase::Elevator | Phase::Done)
        {
            self.phase = Phase::Elevator;
            self.stuck = 0;
        }

        match self.phase {
            Phase::Walk => self.tick_walk(game),
            Phase::OpenDoor { dx, dy } => self.tick_open_door(game, dx, dy),
            Phase::Elevator => self.tick_elevator(game),
            Phase::Boss => self.tick_boss(game),
            Phase::Done => Input::default(),
        }
    }

    fn tick_walk(&mut self, game: &mut Game) -> Input {
        if self.path_i >= self.path.len() {
            self.phase = if self.elev.is_some() {
                Phase::Elevator
            } else {
                Phase::Boss
            };
            return Input::default();
        }
        let (tx, ty) = self.path[self.path_i];

        // Need to open this tile if it is a door and not yet passable.
        if is_door_tile(&game.world, tx, ty) && !door_passable(&game.world, tx, ty) {
            self.phase = Phase::OpenDoor { dx: tx, dy: ty };
            self.door_wait = 0;
            return Input::default();
        }
        // Open neighboring doors we are about to brush past.
        for (nx, ny) in neighbors(tx, ty) {
            if is_door_tile(&game.world, nx, ny) && !door_passable(&game.world, nx, ny) {
                // Only if we are next to that door now.
                let px = game.player.x.floor() as i32;
                let py = game.player.y.floor() as i32;
                if (px - nx).abs() + (py - ny).abs() == 1 {
                    self.phase = Phase::OpenDoor { dx: nx, dy: ny };
                    self.door_wait = 0;
                    return Input::default();
                }
            }
        }

        let gx = tx as f32 + 0.5;
        let gy = ty as f32 + 0.5;
        let dx = gx - game.player.x;
        let dy = gy - game.player.y;
        let dist = (dx * dx + dy * dy).sqrt();
        // Reached (or close enough to) this waypoint.
        if dist < 0.30 || (self.stuck > 25 && dist < 1.0) {
            self.path_i += 1;
            self.stuck = 0;
            if self.path_i >= self.path.len() {
                self.phase = if self.elev.is_some() {
                    Phase::Elevator
                } else {
                    Phase::Boss
                };
            }
            return Input::default();
        }
        // Hard jam: skip ahead, or abandon the route for a direct elevator snap.
        if self.stuck > 50 {
            if self.elev.is_some() && self.stuck > 55 {
                self.phase = Phase::Elevator;
                self.stuck = 0;
                return Input::default();
            }
            self.path_i += 1;
            self.stuck = 0;
            return Input::default();
        }

        let want = dy.atan2(dx);
        let turn = norm_angle(want - game.player.angle);
        let aligned = turn.abs() < 0.5;
        Input {
            forward: aligned,
            run: true,
            turn_delta: turn.clamp(-0.55, 0.55),
            strafe_left: self.stuck > 10 && self.tic.is_multiple_of(2),
            strafe_right: self.stuck > 10 && !self.tic.is_multiple_of(2),
            // Keep tapping use in case we are jammed against a door edge.
            use_door: self.stuck > 20 && self.tic.is_multiple_of(10),
            ..Default::default()
        }
    }

    fn tick_open_door(&mut self, game: &mut Game, dx: i32, dy: i32) -> Input {
        if door_passable(&game.world, dx, dy) {
            self.phase = Phase::Walk;
            self.door_wait = 0;
            return Input::default();
        }
        self.door_wait += 1;
        // Force-open via the same path enemies use (bypasses facing/key glitches).
        if let Some(idx) = game.world.door_lookup(dx, dy) {
            game.world.request_open_door(idx);
        }
        // Face the door and also issue Cmd_Use for realism / locked-door keys.
        let want = ((dy as f32 + 0.5) - game.player.y).atan2((dx as f32 + 0.5) - game.player.x);
        game.player.angle = want;
        if self.door_wait > 90 {
            // Give up on this door tile — skip it and resume walking.
            self.path_i = self.path_i.saturating_add(1);
            self.phase = Phase::Walk;
            self.door_wait = 0;
        }
        Input {
            use_door: true,
            ..Default::default()
        }
    }

    fn tick_elevator(&mut self, game: &mut Game) -> Input {
        let Some((ex, ey)) = self.elev else {
            self.phase = Phase::Done;
            return Input::default();
        };
        let px = game.player.x.floor() as i32;
        let py = game.player.y.floor() as i32;
        let adj = (px - ex).abs() + (py - ey).abs() == 1;

        // If we are not beside the switch, walk (or snap when jammed).
        if !adj {
            let stand = neighbors(ex, ey)
                .into_iter()
                .filter(|&(x, y)| walkable(&game.world, x, y))
                .min_by(|a, b| {
                    let da =
                        (a.0 as f32 + 0.5 - game.player.x).hypot(a.1 as f32 + 0.5 - game.player.y);
                    let db =
                        (b.0 as f32 + 0.5 - game.player.x).hypot(b.1 as f32 + 0.5 - game.player.y);
                    da.total_cmp(&db)
                });
            if let Some((sx, sy)) = stand {
                let dist = (sx as f32 + 0.5 - game.player.x).hypot(sy as f32 + 0.5 - game.player.y);
                // Soft snap so Cmd_Use is reliable (test feature).
                if dist > 0.5 || self.stuck > 10 {
                    game.player.x = sx as f32 + 0.5;
                    game.player.y = sy as f32 + 0.5;
                    game.player.angle = ((ey as f32 + 0.5) - game.player.y)
                        .atan2((ex as f32 + 0.5) - game.player.x);
                    self.stuck = 0;
                    return Input {
                        use_door: true,
                        ..Default::default()
                    };
                }
                let want =
                    ((sy as f32 + 0.5) - game.player.y).atan2((sx as f32 + 0.5) - game.player.x);
                let turn = norm_angle(want - game.player.angle);
                return Input {
                    forward: turn.abs() < 0.5,
                    run: true,
                    turn_delta: turn.clamp(-0.55, 0.55),
                    ..Default::default()
                };
            }
        }

        // Face the switch so Cmd_Use hits tile 21.
        let want = ((ey as f32 + 0.5) - game.player.y).atan2((ex as f32 + 0.5) - game.player.x);
        let turn = norm_angle(want - game.player.angle);
        if turn.abs() > 0.06 {
            // Snap angle when close enough — pure cardinals help Cmd_Use.
            if turn.abs() < 0.4 {
                game.player.angle = want;
                return Input {
                    use_door: true,
                    ..Default::default()
                };
            }
            return Input {
                turn_delta: turn.clamp(-0.5, 0.5),
                ..Default::default()
            };
        }
        self.use_attempts += 1;
        if self.use_attempts > 400 {
            self.phase = Phase::Done;
            return Input::default();
        }
        Input {
            use_door: true,
            ..Default::default()
        }
    }

    fn tick_boss(&mut self, game: &mut Game) -> Input {
        let (px, py) = (game.player.x, game.player.y);
        let target = game
            .actors
            .list
            .iter()
            .filter(|a| {
                !a.dead
                    && matches!(
                        a.kind,
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
            })
            .map(|a| {
                let pri = match a.kind {
                    Kind::Hitler | Kind::MechaHitler | Kind::Angel => 0,
                    _ => 1,
                };
                let d = (a.x - px).powi(2) + (a.y - py).powi(2);
                (pri, d, a.x, a.y)
            })
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));

        let Some((_, d2, bx, by)) = target else {
            // No boss left — maybe we need an elevator after all, or we're done.
            if !find_elevators(&game.world).is_empty() {
                // Re-plan elevator from here.
                if let Some(ap) = Autopilot::start(game) {
                    self.path = ap.path;
                    self.path_i = 0;
                    self.elev = ap.elev;
                    self.phase = Phase::Walk;
                } else {
                    self.phase = Phase::Done;
                }
            } else {
                self.phase = Phase::Done;
            }
            return Input::default();
        };

        let dist = d2.sqrt();
        let turn = norm_angle((by - py).atan2(bx - px) - game.player.angle);
        Input {
            fire: turn.abs() < 0.4,
            turn_delta: turn.clamp(-0.6, 0.6),
            back: dist < 3.0,
            forward: dist > 7.0,
            run: true,
            strafe_left: (self.tic / 18).is_multiple_of(2),
            strafe_right: !(self.tic / 18).is_multiple_of(2),
            ..Default::default()
        }
    }
}

// --- map helpers ------------------------------------------------------------

fn plane0(world: &World, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
        return 1;
    }
    world.level.plane0[y as usize * MAP + x as usize]
}

fn walkable(world: &World, x: i32, y: i32) -> bool {
    !(1..=89).contains(&plane0(world, x, y))
}

fn is_door_tile(world: &World, x: i32, y: i32) -> bool {
    (90..=101).contains(&plane0(world, x, y))
}

fn door_passable(world: &World, x: i32, y: i32) -> bool {
    world
        .door_lookup(x, y)
        .is_some_and(|i| world.door_open_enough(i))
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

fn neighbors(x: i32, y: i32) -> Vec<(i32, i32)> {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .map(|(dx, dy)| (x + dx, y + dy))
        .filter(|&(nx, ny)| nx >= 0 && ny >= 0 && nx < MAP as i32 && ny < MAP as i32)
        .collect()
}

fn bfs_path(world: &World, start: (i32, i32), goal: (i32, i32)) -> Option<Vec<(i32, i32)>> {
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
            if seen.contains(&n) || !walkable(world, n.0, n.1) {
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

fn norm_angle(mut a: f32) -> f32 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    a
}

//! Game state and simulation, independent of any window or input backend.
//! The windowed frontend translates key state into [`Input`]; the headless
//! demo driver synthesizes it. Both run exactly the same code.

use crate::assets::{self, MapSet, VSwap};
use crate::fb::Framebuffer;
use crate::raycast::{self, Player, World};

const MOVE_SPEED: f32 = 3.0; // tiles/sec (Wolf run speed is ~6)
const RUN_FACTOR: f32 = 2.0;
pub const TURN_SPEED: f32 = 2.4; // rad/sec

/// One frame's worth of player intent.
#[derive(Default, Clone, Copy)]
pub struct Input {
    pub forward: bool,
    pub back: bool,
    pub strafe_left: bool,
    pub strafe_right: bool,
    pub turn_left: bool,
    pub turn_right: bool,
    pub run: bool,
    /// Edge-triggered: true only on the frame the use key goes down.
    pub use_door: bool,
}

pub struct Game {
    pub vswap: VSwap,
    pub maps: MapSet,
    pub world: World,
    pub player: Player,
    pub level_idx: usize,
}

impl Game {
    pub fn new(level_idx: usize) -> Self {
        let dir = assets::data_dir();
        let vswap = VSwap::load(&dir)
            .unwrap_or_else(|e| panic!("failed to load VSWAP.WL6 from {dir:?}: {e}"));
        let maps = MapSet::load(&dir)
            .unwrap_or_else(|e| panic!("failed to load GAMEMAPS from {dir:?}: {e}"));
        let level_idx = level_idx.min(maps.num_levels() - 1);
        let world = World::new(maps.level(level_idx));
        let player = raycast::find_spawn(&world.level);
        Self { vswap, maps, world, player, level_idx }
    }

    pub fn switch_level(&mut self, dir: i32) {
        let n = self.maps.num_levels() as i32;
        self.level_idx = (self.level_idx as i32 + dir).rem_euclid(n) as usize;
        self.world = World::new(self.maps.level(self.level_idx));
        self.player = raycast::find_spawn(&self.world.level);
    }

    pub fn update(&mut self, dt: f32, input: &Input) {
        self.world.tick(dt, &self.player);
        if input.use_door {
            self.world.use_door(&self.player);
        }

        if input.turn_left {
            self.player.angle -= TURN_SPEED * dt;
        }
        if input.turn_right {
            self.player.angle += TURN_SPEED * dt;
        }

        let speed = if input.run { MOVE_SPEED * RUN_FACTOR } else { MOVE_SPEED };
        let (dx, dy) = (self.player.angle.cos(), self.player.angle.sin());
        let mut mx = 0.0f32;
        let mut my = 0.0f32;
        if input.forward {
            mx += dx;
            my += dy;
        }
        if input.back {
            mx -= dx;
            my -= dy;
        }
        if input.strafe_left {
            mx += dy;
            my -= dx;
        }
        if input.strafe_right {
            mx -= dy;
            my += dx;
        }
        let len = (mx * mx + my * my).sqrt();
        if len > 0.0 {
            let (wx, wy) = (mx / len * speed * dt, my / len * speed * dt);
            self.player.walk(&self.world, wx, wy);
        }
    }

    pub fn render(&self, fb: &mut Framebuffer) {
        raycast::render(fb, &self.vswap, &self.world, &self.player);
    }
}

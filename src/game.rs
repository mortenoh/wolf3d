//! Game state and simulation, independent of any window or input backend.
//! The windowed frontend translates key state into [`Input`]; the headless
//! demo driver synthesizes it. Both run exactly the same code.

use crate::actors::{Actors, Kind};
use crate::assets::{self, MapSet, VSwap, VgaGraph};
use crate::fb::Framebuffer;
use crate::hud::{self, Hud, HudState, KEY_GOLD, KEY_SILVER, VIEW_H};
use crate::menu::{self, Menu, MAIN_ITEMS};
use crate::raycast::{self, Bonus, Player, World};

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
    /// Edge-triggered weapon selection (0=knife, 1=pistol, 2=machinegun, 3=chaingun).
    pub select_weapon: Option<u8>,
    /// Fire button held state (Ctrl / mouse). Edge- and hold-aware inside Game.
    pub fire: bool,

    // --- Menu navigation (all edge-triggered: set true only on key-down) ---
    pub menu_up: bool,
    pub menu_down: bool,
    pub menu_enter: bool,
    /// Esc / back: leaves the current menu, or opens the menu from play.
    pub menu_back: bool,
    /// Any key — advances the title screen to the main menu.
    pub any_key: bool,
}

/// Which screen the game is showing. The three menu screens and the title are
/// driven by the edge-triggered menu inputs; `Playing` runs the simulation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameScreen {
    Title,
    MainMenu,
    Episode,
    Difficulty,
    Playing,
}

/// Skill level (WL_DEF.H `gd_*`). Controls enemy spawns (see
/// [`Actors::spawn_from_level`]); `skill()` is the 0..=3 index it passes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Difficulty {
    Baby,
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    pub fn skill(self) -> u8 {
        match self {
            Difficulty::Baby => 0,
            Difficulty::Easy => 1,
            Difficulty::Normal => 2,
            Difficulty::Hard => 3,
        }
    }
    fn from_index(i: usize) -> Self {
        [Difficulty::Baby, Difficulty::Easy, Difficulty::Normal, Difficulty::Hard][i.min(3)]
    }
}

/// Which weapon the player is holding. Indices match the VSWAP ready-sprite
/// order (see `hud::weapon_ready_sprite`).
pub const WEAPON_KNIFE: usize = 0;
pub const WEAPON_PISTOL: usize = 1;
pub const WEAPON_MACHINEGUN: usize = 2;
pub const WEAPON_CHAINGUN: usize = 3;

/// The original tic rate, so `update` can express timings as fractional tics.
const TIC: f32 = 1.0 / 70.0;

/// The boss whose death completes each episode's floor 9 (level index
/// episode*10 + 8 in the WL6 map set). The real Hitler — not the mecha suit —
/// is E3's end boss.
pub fn end_boss(level_idx: usize) -> Option<Kind> {
    match level_idx {
        8 => Some(Kind::Hans),
        18 => Some(Kind::Schabbs),
        28 => Some(Kind::Hitler),
        38 => Some(Kind::Gift),
        48 => Some(Kind::Gretel),
        58 => Some(Kind::Fat),
        _ => None,
    }
}

/// attackinfo[weapon][frame] = (tics, attack, frame) from WL_AGENT.C. `attack`:
/// 0 none, 1 gun fire, 2 knife, 3/4 hold-to-repeat, -1 (=5 here) end.
const ATTACK_END: i8 = -1;
const ATTACK_INFO: [[(u8, i8, u8); 4]; 4] = [
    [(6, 0, 1), (6, 2, 2), (6, 0, 3), (6, ATTACK_END, 4)], // knife
    [(6, 0, 1), (6, 1, 2), (6, 0, 3), (6, ATTACK_END, 4)], // pistol
    [(6, 0, 1), (6, 1, 2), (6, 3, 3), (6, ATTACK_END, 4)], // machinegun
    [(6, 0, 1), (6, 1, 2), (6, 4, 3), (6, ATTACK_END, 4)], // chaingun
];

pub struct Game {
    pub vswap: VSwap,
    pub maps: MapSet,
    pub vga: VgaGraph,
    pub hud: Hud,
    pub menu: Menu,
    pub world: World,
    pub player: Player,
    pub actors: Actors,
    pub level_idx: usize,

    // --- Front-end / menu state ---
    pub screen: GameScreen,
    pub difficulty: Difficulty,
    /// Current cursor position on each menu screen.
    pub main_sel: usize,
    pub episode_sel: usize,
    pub diff_sel: usize,
    /// True once a game has been started from the menu (so Esc in the menu can
    /// resume play rather than fall back to the title).
    pub started: bool,
    /// Set when the player picks Quit; the frontend maps it to an exit.
    pub should_quit: bool,

    // Player stats — displayed on the HUD.
    pub health: i32,
    pub ammo: i32,
    pub score: i32,
    pub lives: i32,
    pub keys: u8,
    pub weapon: usize,
    /// Set the frame the player dies (drained by the frontend/tests).
    pub died: bool,
    /// Set once the current episode's end boss is killed (the original sets
    /// `gamestate.victoryflag` from A_StartDeathCam and runs a deathcam plus
    /// the episode-end sequence; here the flag itself is the victory marker).
    pub victory: bool,
    /// Debug-only god mode for headless demo scripts: the player takes no
    /// damage. Set via the demo `godmode` command, never from gameplay.
    pub god: bool,

    // Weapon firing animation (WL_AGENT.C T_Attack).
    attacking: bool,
    attack_frame: usize,
    attack_count: f32,
    weapon_frame: usize,
    fire_held: bool,
    /// Player run speed exceeded the dodge threshold this frame (thrustspeed).
    running: bool,
}

impl Game {
    pub fn new(level_idx: usize) -> Self {
        let dir = assets::data_dir();
        let vswap = VSwap::load(&dir)
            .unwrap_or_else(|e| panic!("failed to load VSWAP.WL6 from {dir:?}: {e}"));
        let maps = MapSet::load(&dir)
            .unwrap_or_else(|e| panic!("failed to load GAMEMAPS from {dir:?}: {e}"));
        let vga = VgaGraph::load(&dir)
            .unwrap_or_else(|e| panic!("failed to load VGAGRAPH from {dir:?}: {e}"));
        let hud = Hud::new(&vga);
        let menu = Menu::new(&vga);
        // The tests and WOLF3D_LEVEL boot straight into a playable level at the
        // hardest skill (all difficulty tiers present), so keep that the default.
        let difficulty = Difficulty::Hard;
        let level_idx = level_idx.min(maps.num_levels() - 1);
        let world = World::new(maps.level(level_idx));
        let player = raycast::find_spawn(&world.level);
        let actors = Actors::spawn_from_level(&world.level, difficulty.skill());
        Self {
            vswap,
            maps,
            vga,
            hud,
            menu,
            world,
            player,
            actors,
            level_idx,
            screen: GameScreen::Playing,
            difficulty,
            main_sel: 0,
            episode_sel: 0,
            diff_sel: 0,
            started: true,
            should_quit: false,
            health: 100,
            ammo: 8,
            score: 0,
            lives: 3,
            keys: 0,
            weapon: WEAPON_PISTOL,
            died: false,
            victory: false,
            god: false,
            attacking: false,
            attack_frame: 0,
            attack_count: 0.0,
            weapon_frame: 0,
            fire_held: false,
            running: false,
        }
    }

    /// Rebuild the current level's world, actors, and player start.
    fn load_level(&mut self) {
        self.world = World::new(self.maps.level(self.level_idx));
        self.player = raycast::find_spawn(&self.world.level);
        self.actors = Actors::spawn_from_level(&self.world.level, self.difficulty.skill());
        self.attacking = false;
        self.attack_frame = 0;
        self.weapon_frame = 0;
    }

    pub fn switch_level(&mut self, dir: i32) {
        let n = self.maps.num_levels() as i32;
        self.level_idx = (self.level_idx as i32 + dir).rem_euclid(n) as usize;
        self.load_level();
    }

    /// Advance to the next floor via the elevator (WL_AGENT.C `ex_completed`).
    /// Health, ammo, score, lives and weapons carry over; keys reset each floor
    /// (SetupGameLevel clears `gamestate.keys`). Past the last floor of the
    /// episode this wraps to the next episode's floor 1 — a placeholder for the
    /// real end-of-episode victory/boss sequence (a later milestone).
    pub fn next_level(&mut self) {
        let n = self.maps.num_levels() as i32;
        self.level_idx = (self.level_idx as i32 + 1).rem_euclid(n) as usize;
        self.load_level();
        self.keys = 0;
    }

    /// Boot into the title screen (used when no `WOLF3D_LEVEL` pins a level).
    pub fn to_title(&mut self) {
        self.screen = GameScreen::Title;
        self.started = false;
        self.main_sel = 0;
    }

    /// Frame entry point: dispatch to the menu flow or the simulation.
    pub fn update(&mut self, dt: f32, input: &Input) {
        match self.screen {
            GameScreen::Title => self.update_title(input),
            GameScreen::MainMenu => self.update_main_menu(input),
            GameScreen::Episode => self.update_episode(input),
            GameScreen::Difficulty => self.update_difficulty(input),
            GameScreen::Playing => {
                if input.menu_back {
                    // Esc pauses to the main menu (WL_PLAY.C's US_ControlPanel).
                    self.screen = GameScreen::MainMenu;
                    return;
                }
                self.update_play(dt, input);
            }
        }
        if !matches!(self.screen, GameScreen::Playing) {
            self.menu.tick(dt);
        }
    }

    // --- Menu state machine (WL_MENU.C) ------------------------------------

    fn update_title(&mut self, input: &Input) {
        if input.any_key || input.menu_enter || input.menu_up || input.menu_down {
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_NEW_GAME;
        }
    }

    /// Move the cursor to the next selectable item in `dir` (+1 / -1), skipping
    /// the greyed-out entries (the original leaves them unselectable).
    fn move_selectable(sel: usize, dir: i32) -> usize {
        let n = MAIN_ITEMS.len() as i32;
        let mut i = sel as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if MAIN_ITEMS[i as usize].active {
                break;
            }
        }
        i as usize
    }

    fn update_main_menu(&mut self, input: &Input) {
        if input.menu_up {
            self.main_sel = Self::move_selectable(self.main_sel, -1);
        }
        if input.menu_down {
            self.main_sel = Self::move_selectable(self.main_sel, 1);
        }
        if input.menu_back {
            // Back out of the menu: resume a game in progress, else to title.
            self.screen = if self.started { GameScreen::Playing } else { GameScreen::Title };
            return;
        }
        if input.menu_enter {
            match self.main_sel {
                menu::ITEM_NEW_GAME => {
                    self.episode_sel = 0;
                    self.screen = GameScreen::Episode;
                }
                menu::ITEM_QUIT => self.should_quit = true,
                _ => {}
            }
        }
    }

    fn update_episode(&mut self, input: &Input) {
        if input.menu_up {
            self.episode_sel = (self.episode_sel + menu::NUM_EPISODES - 1) % menu::NUM_EPISODES;
        }
        if input.menu_down {
            self.episode_sel = (self.episode_sel + 1) % menu::NUM_EPISODES;
        }
        if input.menu_back {
            self.screen = GameScreen::MainMenu;
            return;
        }
        if input.menu_enter {
            self.diff_sel = self.difficulty.skill() as usize;
            self.screen = GameScreen::Difficulty;
        }
    }

    fn update_difficulty(&mut self, input: &Input) {
        if input.menu_up {
            self.diff_sel = (self.diff_sel + menu::NUM_DIFFICULTIES - 1) % menu::NUM_DIFFICULTIES;
        }
        if input.menu_down {
            self.diff_sel = (self.diff_sel + 1) % menu::NUM_DIFFICULTIES;
        }
        if input.menu_back {
            self.screen = GameScreen::Episode;
            return;
        }
        if input.menu_enter {
            self.start_new_game(self.episode_sel, Difficulty::from_index(self.diff_sel));
        }
    }

    /// Start a fresh game on `episode`'s first floor at `difficulty` with a full
    /// starting loadout (WL_PLAY.C NewGame / SetupGameLevel).
    pub fn start_new_game(&mut self, episode: usize, difficulty: Difficulty) {
        self.difficulty = difficulty;
        self.level_idx = (episode * 10).min(self.maps.num_levels() - 1);
        self.health = 100;
        self.ammo = 8;
        self.score = 0;
        self.lives = 3;
        self.keys = 0;
        self.weapon = WEAPON_PISTOL;
        self.died = false;
        self.victory = false;
        self.load_level();
        self.started = true;
        self.screen = GameScreen::Playing;
    }

    fn update_play(&mut self, dt: f32, input: &Input) {
        if let Some(w) = input.select_weapon {
            self.weapon = w as usize;
        }

        self.world.tick(dt, &self.player);
        if input.use_door {
            // Cmd_Use: elevator switch completes the floor; otherwise a door.
            if self.world.use_elevator(&self.player) {
                self.next_level();
                return;
            }
            self.world.use_door(&self.player, self.keys);
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
        self.running = len > 0.0 && input.run;
        if len > 0.0 {
            let (wx, wy) = (mx / len * speed * dt, my / len * speed * dt);
            self.player.walk(&self.world, wx, wy);
        }

        // --- Player weapon: begin an attack, run its animation, fire ---
        let mut madenoise = false;
        self.fire_held = input.fire;
        if input.fire && !self.attacking {
            self.begin_attack();
        }
        if self.attacking {
            madenoise = self.run_attack(dt);
        }

        // --- Enemies ---
        let tics = dt / TIC;
        self.actors.update(
            tics,
            &mut self.world,
            self.player.x,
            self.player.y,
            self.running,
            madenoise,
        );
        let damage = self.actors.take_damage();
        if !self.god {
            self.health -= damage;
        }

        // Spawn any enemy-death drops, then collect bonuses the player stands on.
        for (tx, ty, bonus) in self.actors.take_drops() {
            self.world.place_drop(tx, ty, bonus);
        }
        self.try_pickups();

        // Killing the floor's end boss wins the episode (in the original the
        // boss die chain ends in A_StartDeathCam, which sets victoryflag and
        // plays the deathcam; here the flag is set straight from the kill).
        for kind in self.actors.take_deaths() {
            if Some(kind) == end_boss(self.level_idx) {
                self.victory = true;
            }
        }

        if self.health <= 0 {
            self.health = 0;
            self.died = true;
            self.lives -= 1;
            // Restart the level with a fresh loadout (the original's respawn).
            self.load_level();
            self.health = 100;
            self.ammo = 8;
            self.weapon = WEAPON_PISTOL;
        }
    }

    /// Cmd_Fire (WL_AGENT.C): enter the attack animation from its first frame.
    fn begin_attack(&mut self) {
        self.attacking = true;
        self.attack_frame = 0;
        self.attack_count = ATTACK_INFO[self.weapon][0].0 as f32;
        self.weapon_frame = ATTACK_INFO[self.weapon][0].2 as usize;
    }

    /// Advance the attack animation, firing on the frames flagged for it.
    /// Returns true if a gunshot was made this frame (sets `madenoise`).
    fn run_attack(&mut self, dt: f32) -> bool {
        let mut noise = false;
        self.attack_count -= dt / TIC;
        while self.attack_count <= 0.0 {
            let (tics, attack, _) = ATTACK_INFO[self.weapon][self.attack_frame];
            match attack {
                ATTACK_END => {
                    self.attacking = false;
                    self.weapon_frame = 0;
                    self.attack_frame = 0;
                    if self.ammo == 0 {
                        self.weapon = WEAPON_KNIFE;
                    }
                    return noise;
                }
                4 => {
                    // Chaingun: repeat while held, then fall through to fire.
                    if self.ammo == 0 {
                        // no ammo: just advance
                    } else {
                        if self.fire_held {
                            self.attack_frame = self.attack_frame.saturating_sub(2);
                        }
                        self.fire_gun();
                        noise = true;
                    }
                }
                1 => {
                    if self.ammo == 0 {
                        self.attack_frame += 1;
                    } else {
                        self.fire_gun();
                        noise = true;
                    }
                }
                2 => self.fire_knife(),
                // Machinegun (attack code 3): repeat while held.
                3 if self.ammo > 0 && self.fire_held => {
                    self.attack_frame = self.attack_frame.saturating_sub(2);
                }
                _ => {}
            }
            self.attack_count += tics as f32;
            self.attack_frame += 1;
            self.weapon_frame = ATTACK_INFO[self.weapon][self.attack_frame.min(3)].2 as usize;
        }
        noise
    }

    fn fire_gun(&mut self) {
        let points = self.actors.player_fire(
            &self.world,
            self.player.x,
            self.player.y,
            self.player.angle,
            false,
        );
        self.score += points;
        self.ammo -= 1;
    }

    fn fire_knife(&mut self) {
        let points = self.actors.player_fire(
            &self.world,
            self.player.x,
            self.player.y,
            self.player.angle,
            true,
        );
        self.score += points;
    }

    // --- Pickups (WL_AGENT.C GetBonus) -------------------------------------

    /// Collect any bonus static on the player's tile. GetBonus is triggered when
    /// the player touches an item; here we test tile equality each frame.
    fn try_pickups(&mut self) {
        let ptx = self.player.x.floor() as i32;
        let pty = self.player.y.floor() as i32;
        // Gather eligible items first (immutable borrow), then apply effects.
        let eligible: Vec<(usize, Bonus)> = self
            .world
            .statics
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let b = s.bonus?;
                (!s.picked && s.x.floor() as i32 == ptx && s.y.floor() as i32 == pty)
                    .then_some((i, b))
            })
            .collect();
        for (i, bonus) in eligible {
            if self.get_bonus(bonus) {
                self.world.take_static(i);
            }
        }
    }

    /// Apply a bonus to the player's stats. Returns false (leaving the item) for
    /// health/ammo pickups when already at the cap — the original's early return.
    fn get_bonus(&mut self, bonus: Bonus) -> bool {
        match bonus {
            Bonus::Alpo => self.heal(4),
            Bonus::Food => self.heal(10),
            Bonus::FirstAid => self.heal(25),
            // Gibs heal 1 and are always consumed (the "disgusted face" pickup).
            Bonus::Gibs => {
                self.health = (self.health + 1).min(100);
                true
            }
            Bonus::Clip => self.give_ammo(8),
            Bonus::Clip2 => self.give_ammo(4),
            Bonus::MachineGun => {
                self.give_weapon(WEAPON_MACHINEGUN);
                true
            }
            Bonus::ChainGun => {
                self.give_weapon(WEAPON_CHAINGUN);
                true
            }
            Bonus::Key1 => {
                self.keys |= KEY_GOLD;
                true
            }
            Bonus::Key2 => {
                self.keys |= KEY_SILVER;
                true
            }
            Bonus::Cross => self.give_points(100),
            Bonus::Chalice => self.give_points(500),
            Bonus::Bible => self.give_points(1000),
            Bonus::Crown => self.give_points(5000),
            Bonus::FullHeal => {
                // 1-up: full health, +25 ammo, +1 life.
                self.health = 100;
                self.ammo = (self.ammo + 25).min(99);
                self.lives += 1;
                true
            }
        }
    }

    /// HealSelf: heal up to 100, skipping (not consuming) the item when full.
    fn heal(&mut self, amount: i32) -> bool {
        if self.health >= 100 {
            return false;
        }
        self.health = (self.health + amount).min(100);
        true
    }

    /// GiveAmmo: add ammo up to 99, skipping (not consuming) the item when full.
    fn give_ammo(&mut self, amount: i32) -> bool {
        if self.ammo >= 99 {
            return false;
        }
        self.ammo = (self.ammo + amount).min(99);
        true
    }

    /// GiveWeapon: +6 ammo and switch to the weapon if it out-ranks the current
    /// one (the original tracks `bestweapon`; we approximate with `weapon`).
    fn give_weapon(&mut self, weapon: usize) {
        self.ammo = (self.ammo + 6).min(99);
        if self.weapon < weapon {
            self.weapon = weapon;
        }
    }

    fn give_points(&mut self, points: i32) -> bool {
        self.score += points;
        true
    }

    pub fn render(&mut self, fb: &mut Framebuffer) {
        match self.screen {
            GameScreen::Title => {
                self.menu.render_title(fb);
                return;
            }
            GameScreen::MainMenu => {
                self.menu.render_main(fb, self.main_sel);
                return;
            }
            GameScreen::Episode => {
                self.menu.render_episode(fb, self.episode_sel);
                return;
            }
            GameScreen::Difficulty => {
                self.menu.render_difficulty(fb, self.diff_sel);
                return;
            }
            GameScreen::Playing => {}
        }
        raycast::render(fb, &self.vswap, &self.world, &mut self.actors, &self.player, VIEW_H);
        // The firing animation offsets from the weapon's ready frame.
        let weapon_sprite = hud::weapon_ready_sprite(&self.vswap, self.weapon) + self.weapon_frame;
        hud::draw_weapon(fb, &self.vswap, weapon_sprite);
        self.hud.draw(
            fb,
            &HudState {
                // Per-episode floor number, like the original's mapon+1.
                floor: (self.level_idx % 10) as i32 + 1,
                score: self.score,
                lives: self.lives,
                health: self.health,
                ammo: self.ammo,
                keys: self.keys,
            },
        );
    }
}

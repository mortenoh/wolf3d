//! Game state and simulation, independent of any window or input backend.
//! The windowed frontend translates key state into [`Input`]; the headless
//! demo driver synthesizes it. Both run exactly the same code.

use crate::actors::{Actors, Kind};
use crate::assets::vgagraph::{T_ENDART1, T_HELPART};
use crate::assets::{self, MapSet, VSwap, VgaGraph};
use crate::fb::Framebuffer;
use crate::highscore::{self, HighScore};
use crate::hud::{self, Hud, HudState, KEY_GOLD, KEY_SILVER, VIEW_H};
use crate::inter::{self, InterGfx, Intermission, LevelStats, VictoryStats};
use crate::menu::{self, Menu, MAIN_ITEMS};
use crate::raycast::{self, Bonus, ElevatorUse, Player, PushUse, World};
use crate::savegame::{self, SaveError};
use crate::sound as snd;
use crate::text::TextScreen;

/// Longest save-slot name the text field accepts (WL_MENU.C allowed 31; the
/// menu font fits a bit fewer in the slot box).
pub const SAVE_NAME_MAX: usize = 24;

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
    /// Mouse-look: radians to add to the facing this frame (already scaled by
    /// the frontend's sensitivity; positive turns right/clockwise).
    pub turn_delta: f32,

    // --- Menu navigation (all edge-triggered: set true only on key-down) ---
    pub menu_up: bool,
    pub menu_down: bool,
    pub menu_enter: bool,
    /// Esc / back: leaves the current menu, or opens the menu from play.
    pub menu_back: bool,
    /// Left/right on grid menus (currently only the level-select cheat).
    pub menu_left: bool,
    pub menu_right: bool,
    /// Any key — advances the title screen to the main menu.
    pub any_key: bool,
    /// A typed character this tic, for the save-name text field. The frontend
    /// fills it from the window's text input; the demo driver from `type:`.
    pub typed: Option<char>,
    /// Backspace pressed this tic (edge-triggered) — deletes a character.
    pub backspace: bool,
}

/// Sound-effect playback mode, the Sound menu's effects radio group. The
/// original had separate None/PC-Speaker/AdLib and None/SoundBlaster groups;
/// we collapse them into one three-way choice the frontend honors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SfxMode {
    /// Prefer digitized samples, fall back to AdLib (the default, full sound).
    DigiAdlib,
    /// AdLib synthesis only — never the digitized samples.
    AdlibOnly,
    /// No sound effects at all.
    Off,
}

/// Which screen the game is showing. The three menu screens and the title are
/// driven by the edge-triggered menu inputs; `Playing` runs the simulation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameScreen {
    Title,
    MainMenu,
    Episode,
    Difficulty,
    /// The Sound-options screen (effects + music radio groups).
    Sound,
    /// The Load Game slot list.
    LoadGame,
    /// The Save Game slot list (and, when naming, the text entry over a slot).
    SaveGame,
    Playing,
    /// The floor-completed intermission (WL_INTER.C LevelCompleted), shown after
    /// the elevator before the next floor loads.
    Intermission,
    /// The brief "Get Psyched!" load screen shown while the next floor loads.
    GetPsyched,
    /// The brief red-tint death pause (WL_GAME.C `Died`): shown for ~1s after
    /// the player is killed, before the floor restarts or the game ends.
    Death,
    /// The cinematic deathcam (WL_ACT2.C `A_StartDeathCam`): after an episode
    /// end boss dies the camera swings round to watch the corpse fall again.
    DeathCam,
    /// The "YOU WIN!" episode-victory screen (WL_INTER.C `Victory`).
    Victory,
    /// The end-of-episode article pages (WL_TEXT.C `EndText`).
    EndText,
    /// The high-score board (WL_INTER.C `DrawHighScores`).
    HighScores,
    /// Name entry on the high-score board when a run qualifies
    /// (WL_INTER.C `CheckHighScore`).
    HighScoreEntry,
    /// The "Read This!" help article (WL_TEXT.C `HelpScreens`).
    ReadThis,
    /// The level-select cheat screen (the 6 key during play): a 6x10
    /// episode/floor grid; Enter warps, keeping the current stats.
    LevelSelect,
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
pub const TIC: f32 = 1.0 / 70.0;

/// How long the "Get Psyched!" load screen lingers (real/sim seconds) before the
/// already-loaded floor starts. Loads are instant here, so this is purely cosmetic.
const LOAD_SCREEN_SECS: f32 = 0.5;

/// How long the red death-tint lingers before the floor restarts / game ends.
const DEATH_SECS: f32 = 1.0;

/// Deathcam duration (WL_ACT2.C `IN_UserInput(300)`): ~300 tics before it auto-
/// advances to the victory screen (any key skips it).
const DEATHCAM_SECS: f32 = 300.0 / 70.0;

/// Where the high-score board returns to after it is dismissed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HsReturn {
    /// Back to the title screen (after a victory or a game over).
    Title,
    /// Back to the main menu (the "View Scores" entry).
    Menu,
}

/// Whether a bonus static counts toward the floor's treasure total (WL_AGENT.C
/// GetBonus increments `treasurecount` for these): the four valuables plus the
/// one-up (`bo_fullheal`).
fn is_treasure(bonus: Option<Bonus>) -> bool {
    matches!(
        bonus,
        Some(Bonus::Cross | Bonus::Chalice | Bonus::Bible | Bonus::Crown | Bonus::FullHeal)
    )
}

/// The boss whose death completes each episode's floor 9 (level index
/// episode*10 + 8 in the WL6 map set). The real Hitler — not the mecha suit —
/// is E3's end boss.
/// The pickup sound for a bonus item (WL_AGENT.C GetBonus).
fn bonus_sound(bonus: Bonus) -> u8 {
    match bonus {
        Bonus::Alpo | Bonus::Food => snd::HEALTH1SND as u8,
        Bonus::FirstAid => snd::HEALTH2SND as u8,
        Bonus::Gibs => snd::SLURPIESND as u8,
        Bonus::Clip | Bonus::Clip2 => snd::GETAMMOSND as u8,
        Bonus::MachineGun => snd::GETMACHINESND as u8,
        Bonus::ChainGun => snd::GETGATLINGSND as u8,
        Bonus::Key1 | Bonus::Key2 => snd::GETKEYSND as u8,
        Bonus::Cross => snd::BONUS1SND as u8,
        Bonus::Chalice => snd::BONUS2SND as u8,
        Bonus::Bible => snd::BONUS3SND as u8,
        Bonus::Crown => snd::BONUS4SND as u8,
        Bonus::FullHeal => snd::BONUS1UPSND as u8,
    }
}

/// Count the floor's kill / secret / treasure totals at level setup (WL_GAME.C
/// ScanInfoPlane): every live enemy spawned (corpses excluded), every plane-1
/// PUSHABLETILE marker, and every treasure static.
fn compute_stats(world: &World, actors: &Actors) -> LevelStats {
    let kill_total = actors.list.iter().filter(|a| !a.dead).count() as i32;
    let secret_total = world.level.plane1.iter().filter(|&&c| c == 98).count() as i32;
    let treasure_total = world.statics.iter().filter(|s| is_treasure(s.bonus)).count() as i32;
    LevelStats { kill_total, secret_total, treasure_total, ..Default::default() }
}

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

    // --- Level stats + intermission (WL_INTER.C) ---
    /// Kill / secret / treasure counters and elapsed time for the current floor.
    pub stats: LevelStats,
    /// Decoded intermission / load-screen art.
    inter_gfx: InterGfx,
    /// The active floor-completed intermission, if the screen is `Intermission`.
    pub inter: Option<Intermission>,
    /// Whether to show the "Get Psyched!" load screen between floors. Off for the
    /// headless demo / `WOLF3D_LEVEL` paths so they are unaffected.
    pub show_load_screen: bool,
    /// Countdown clock while on the `GetPsyched` screen.
    psyched_clock: f32,

    // --- Endgame flow (deathcam / victory / endtext / high scores) ----------
    /// Clock on the `Death` red-tint screen.
    death_clock: f32,
    /// True when the death that started the `Death` screen was the last life,
    /// so it routes to the high-score check rather than a floor restart.
    death_game_over: bool,
    /// Clock on the `DeathCam` screen.
    deathcam_clock: f32,
    /// The averaged episode stats shown on the `Victory` screen.
    victory_stats: VictoryStats,
    /// The decoded end-of-episode article and the current page.
    endtext: Option<TextScreen>,
    endtext_page: usize,
    /// The decoded "Read This!" help article and the current page.
    readthis: Option<TextScreen>,
    readthis_page: usize,
    /// The high-score board (loaded from disk at startup).
    pub highscores: Vec<HighScore>,
    /// The board slot being named on the `HighScoreEntry` screen.
    hs_edit_slot: Option<usize>,
    /// The name buffer being typed on the `HighScoreEntry` screen.
    hs_name: String,
    /// Where the board returns to when dismissed.
    hs_return: HsReturn,
    /// Running per-episode ratio/time accumulation for the `Victory` averages.
    epi_kr_sum: i32,
    epi_sr_sum: i32,
    epi_tr_sum: i32,
    epi_time_sum: i32,
    epi_count: i32,

    // --- Front-end / menu state ---
    pub screen: GameScreen,
    pub difficulty: Difficulty,
    /// Current cursor position on each menu screen.
    pub main_sel: usize,
    pub episode_sel: usize,
    pub diff_sel: usize,
    /// Sound menu cursor (0..2 effects rows, 3..4 music rows).
    pub sound_sel: usize,
    /// Shared slot cursor for the Load/Save slot lists.
    pub ls_sel: usize,
    /// Level-select cheat cursor (a level index, 0..num_levels).
    pub level_sel: usize,
    /// Lazily-built map names for the level-select screen.
    level_names: Option<Vec<String>>,
    /// Cached slot names for the Load/Save menus (refreshed on entry).
    pub save_slots: Vec<Option<String>>,
    /// True while the Save screen is capturing a slot name.
    pub entering_name: bool,
    /// The slot-name text buffer being typed.
    pub save_name: String,

    // --- Sound options (consumed by the frontend audio path) ---
    /// Sound-effects playback mode (Sound menu effects group).
    pub sfx_mode: SfxMode,
    /// Whether music plays (Sound menu music group / the `M` shortcut).
    pub music_on: bool,
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
    /// damage. Set via the demo `godmode` command or the 7 cheat key.
    pub god: bool,
    /// Cheat: ammo never decrements (the 9 key).
    pub infinite_ammo: bool,

    // Weapon firing animation (WL_AGENT.C T_Attack).
    attacking: bool,
    attack_frame: usize,
    attack_count: f32,
    weapon_frame: usize,
    fire_held: bool,
    /// Player run speed exceeded the dodge threshold this frame (thrustspeed).
    running: bool,

    /// Sound-enum ids emitted by the player/UI this tic. The frontend drains
    /// this (plus the actor and world queues) via [`Game::take_sounds`]; the
    /// simulation itself never touches audio, so headless runs stay deterministic.
    pub sounds: Vec<u8>,
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
        let inter_gfx = InterGfx::new(&vga);
        // The tests and WOLF3D_LEVEL boot straight into a playable level at the
        // hardest skill (all difficulty tiers present), so keep that the default.
        let difficulty = Difficulty::Hard;
        let level_idx = level_idx.min(maps.num_levels() - 1);
        let world = World::new(maps.level(level_idx));
        let player = raycast::find_spawn(&world.level);
        let actors = Actors::spawn_from_level(&world.level, difficulty.skill());
        let stats = compute_stats(&world, &actors);
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
            stats,
            inter_gfx,
            inter: None,
            show_load_screen: false,
            psyched_clock: 0.0,
            death_clock: 0.0,
            death_game_over: false,
            deathcam_clock: 0.0,
            victory_stats: VictoryStats::default(),
            endtext: None,
            endtext_page: 0,
            readthis: None,
            readthis_page: 0,
            highscores: highscore::load(),
            hs_edit_slot: None,
            hs_name: String::new(),
            hs_return: HsReturn::Title,
            epi_kr_sum: 0,
            epi_sr_sum: 0,
            epi_tr_sum: 0,
            epi_time_sum: 0,
            epi_count: 0,
            screen: GameScreen::Playing,
            difficulty,
            main_sel: 0,
            episode_sel: 0,
            diff_sel: 0,
            sound_sel: 0,
            ls_sel: 0,
            level_sel: 0,
            level_names: None,
            save_slots: vec![None; savegame::NUM_SLOTS],
            entering_name: false,
            save_name: String::new(),
            sfx_mode: SfxMode::DigiAdlib,
            music_on: true,
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
            infinite_ammo: false,
            attacking: false,
            attack_frame: 0,
            attack_count: 0.0,
            weapon_frame: 0,
            fire_held: false,
            running: false,
            sounds: Vec::new(),
        }
    }

    /// Drain every sound-enum id emitted since the last call — the player/UI
    /// events plus the enemy and door queues. The frontend feeds these to the
    /// audio backend; a headless run can log them to assert sounds fired.
    pub fn take_sounds(&mut self) -> Vec<u8> {
        let mut v = std::mem::take(&mut self.sounds);
        v.extend(self.actors.take_sounds());
        v.extend(self.world.take_sounds());
        v
    }

    /// Rebuild the current level's world, actors, and player start, and recount
    /// the floor's kill/secret/treasure totals (WL_GAME.C ScanInfoPlane).
    fn load_level(&mut self) {
        self.world = World::new(self.maps.level(self.level_idx));
        self.player = raycast::find_spawn(&self.world.level);
        self.actors = Actors::spawn_from_level(&self.world.level, self.difficulty.skill());
        self.stats = compute_stats(&self.world, &self.actors);
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
            GameScreen::Sound => self.update_sound(input),
            GameScreen::LoadGame => self.update_load(input),
            GameScreen::SaveGame => self.update_save(input),
            GameScreen::Playing => {
                if input.menu_back {
                    // Esc pauses to the main menu (WL_PLAY.C's US_ControlPanel).
                    self.screen = GameScreen::MainMenu;
                    return;
                }
                self.update_play(dt, input);
            }
            GameScreen::Intermission => self.update_intermission(dt, input),
            GameScreen::GetPsyched => self.update_get_psyched(dt),
            GameScreen::Death => self.update_death(dt),
            GameScreen::DeathCam => self.update_deathcam(dt, input),
            GameScreen::Victory => self.update_victory(dt, input),
            GameScreen::EndText => self.update_endtext(input),
            GameScreen::HighScores => self.update_high_scores(input),
            GameScreen::HighScoreEntry => self.update_high_score_entry(input),
            GameScreen::ReadThis => self.update_read_this(input),
            GameScreen::LevelSelect => self.update_level_select(input),
        }
        // The menu cursor blink advances on the menu screens (not during play or
        // the intermission / load / endgame screens, which run their own clocks).
        if matches!(
            self.screen,
            GameScreen::Title
                | GameScreen::MainMenu
                | GameScreen::Episode
                | GameScreen::Difficulty
                | GameScreen::Sound
                | GameScreen::LoadGame
                | GameScreen::SaveGame
        ) {
            self.menu.tick(dt);
        }
    }

    // --- Intermission + load screen (WL_INTER.C) ---------------------------

    /// Finish the current floor via the elevator: finalize its stats, award the
    /// bonus, and enter the intermission (WL_INTER.C LevelCompleted). `secret`
    /// routes to the episode's secret floor (floor 10) rather than the next one.
    fn begin_intermission(&mut self, secret: bool) {
        let time_sec = (self.stats.time as i32).min(99 * 60);
        let par_sec = inter::par_seconds(self.level_idx);
        let ratios = [
            self.stats.kill_ratio(),
            self.stats.secret_ratio(),
            self.stats.treasure_ratio(),
        ];
        let (timeleft, bonus) = inter::compute_bonus(time_sec, par_sec, ratios[0], ratios[1], ratios[2]);
        self.score += bonus; // GivePoints(bonus)
        self.accumulate_episode(); // feed the Victory averages (LevelRatios)

        let n = self.maps.num_levels();
        let next = if secret {
            ((self.level_idx / 10) * 10 + 9).min(n - 1)
        } else {
            (self.level_idx + 1) % n
        };
        let floor = (self.level_idx % 10) as i32 + 1;
        self.inter = Some(Intermission::new(
            floor,
            time_sec,
            inter::par_string(self.level_idx),
            ratios,
            timeleft,
            bonus,
            next,
        ));
        self.screen = GameScreen::Intermission;
        self.sounds.push(snd::LEVELDONESND as u8);
    }

    fn update_intermission(&mut self, dt: f32, input: &Input) {
        if let Some(it) = self.inter.as_mut() {
            let sounds = it.update(dt);
            self.sounds.extend(sounds);
        }
        // Any key / fire / use continues to the next floor.
        if input.any_key || input.menu_enter || input.fire || input.use_door {
            let next = self.inter.take().map_or(self.level_idx, |it| it.next_level);
            self.level_idx = next;
            self.load_level();
            self.keys = 0;
            if self.show_load_screen {
                self.psyched_clock = 0.0;
                self.screen = GameScreen::GetPsyched;
            } else {
                self.screen = GameScreen::Playing;
            }
        }
    }

    fn update_get_psyched(&mut self, dt: f32) {
        self.psyched_clock += dt;
        if self.psyched_clock >= LOAD_SCREEN_SECS {
            self.screen = GameScreen::Playing;
        }
    }

    // --- Endgame flow (deathcam -> victory -> endtext -> high scores) -------

    /// Fold the just-finished floor's ratios into the running episode totals,
    /// used for the `Victory` averages (WL_INTER.C `LevelRatios`).
    fn accumulate_episode(&mut self) {
        self.epi_kr_sum += self.stats.kill_ratio();
        self.epi_sr_sum += self.stats.secret_ratio();
        self.epi_tr_sum += self.stats.treasure_ratio();
        self.epi_time_sum += (self.stats.time as i32).min(99 * 60);
        self.epi_count += 1;
    }

    /// A_StartDeathCam: the episode end boss is dead. Set the victory flag, swing
    /// the camera onto the boss corpse, and freeze into the deathcam.
    fn begin_deathcam(&mut self, kind: Kind) {
        self.victory = true;
        self.accumulate_episode();
        if let Some((bx, by)) = self.actors.find_pos(kind) {
            self.position_deathcam(bx, by);
        }
        self.deathcam_clock = 0.0;
        self.screen = GameScreen::DeathCam;
    }

    /// Place the camera on the boss->player line a few tiles from the corpse in
    /// an open tile, looking straight at it (A_StartDeathCam backs the camera off
    /// until it clears a wall).
    fn position_deathcam(&mut self, bx: f32, by: f32) {
        let (px, py) = (self.player.x, self.player.y);
        // Direction from the boss out toward the player's side of the room.
        let dir = (py - by).atan2(px - bx);
        let (mut cx, mut cy) = (px, py);
        for step in 1..=8 {
            let d = 1.5 + step as f32 * 0.3;
            let nx = bx + dir.cos() * d;
            let ny = by + dir.sin() * d;
            if self.world.wall_at(nx.floor() as i32, ny.floor() as i32) {
                break;
            }
            cx = nx;
            cy = ny;
            if d >= 3.0 {
                break;
            }
        }
        self.player.x = cx;
        self.player.y = cy;
        self.player.angle = (by - cy).atan2(bx - cx);
    }

    fn update_deathcam(&mut self, dt: f32, input: &Input) {
        // The corpse keeps falling while the camera watches.
        let sounds = self.actors.animate_dead(dt / TIC);
        self.sounds.extend(sounds);
        self.deathcam_clock += dt;
        if input.any_key
            || input.menu_enter
            || input.fire
            || input.use_door
            || self.deathcam_clock >= DEATHCAM_SECS
        {
            self.begin_victory();
        }
    }

    /// WL_INTER.C `Victory`: compute the averaged episode ratios and show the
    /// "YOU WIN!" screen.
    fn begin_victory(&mut self) {
        let n = self.epi_count.max(1);
        self.victory_stats = VictoryStats {
            time_sec: self.epi_time_sum,
            kill: self.epi_kr_sum / n,
            secret: self.epi_sr_sum / n,
            treasure: self.epi_tr_sum / n,
        };
        self.screen = GameScreen::Victory;
    }

    fn update_victory(&mut self, dt: f32, input: &Input) {
        let sounds = self.actors.animate_dead(dt / TIC);
        self.sounds.extend(sounds);
        if input.any_key || input.menu_enter || input.fire || input.use_door {
            self.begin_endtext();
        }
    }

    /// WL_TEXT.C `EndText`: page through the current episode's end article.
    fn begin_endtext(&mut self) {
        let episode = self.level_idx / 10;
        let chunk = T_ENDART1 + episode;
        self.endtext = Some(TextScreen::new(&self.vga, chunk));
        self.endtext_page = 0;
        self.screen = GameScreen::EndText;
    }

    fn update_endtext(&mut self, input: &Input) {
        if !(input.any_key || input.menu_enter || input.menu_back || input.fire || input.use_door) {
            return;
        }
        let pages = self.endtext.as_ref().map_or(1, TextScreen::num_pages);
        if self.endtext_page + 1 < pages {
            self.endtext_page += 1;
        } else {
            self.endtext = None;
            // The victory run ends at the high-score check, then the title.
            self.begin_high_score_check(HsReturn::Title);
        }
    }

    /// WL_INTER.C `CheckHighScore`: if the run qualifies, drop it on the board
    /// and enter the name; otherwise just show the board. `ret` is where the
    /// board returns when dismissed.
    fn begin_high_score_check(&mut self, ret: HsReturn) {
        self.hs_return = ret;
        let completed = self.level_idx as u32 + 1;
        if let Some(slot) = highscore::insert(&mut self.highscores, self.score, completed) {
            self.hs_edit_slot = Some(slot);
            self.hs_name.clear();
            self.screen = GameScreen::HighScoreEntry;
        } else {
            self.hs_edit_slot = None;
            self.screen = GameScreen::HighScores;
        }
    }

    fn update_high_score_entry(&mut self, input: &Input) {
        if input.menu_enter {
            if let Some(slot) = self.hs_edit_slot.take() {
                let name = if self.hs_name.trim().is_empty() {
                    "Anonymous".to_string()
                } else {
                    self.hs_name.clone()
                };
                if let Some(e) = self.highscores.get_mut(slot) {
                    e.name = name;
                }
                let _ = highscore::store(&self.highscores);
            }
            self.screen = GameScreen::HighScores;
            return;
        }
        if input.backspace {
            self.hs_name.pop();
        }
        if let Some(c) = input.typed
            && !c.is_control()
            && self.hs_name.chars().count() < highscore::MAX_HIGH_NAME
        {
            self.hs_name.push(c);
        }
    }

    fn update_high_scores(&mut self, input: &Input) {
        if input.any_key || input.menu_enter || input.menu_back {
            match self.hs_return {
                HsReturn::Menu => {
                    self.screen = GameScreen::MainMenu;
                    self.main_sel = menu::ITEM_VIEWSCORES;
                }
                HsReturn::Title => {
                    self.victory = false;
                    self.to_title();
                }
            }
        }
    }

    fn update_read_this(&mut self, input: &Input) {
        if input.menu_back {
            self.readthis = None;
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_READ;
            return;
        }
        if input.any_key || input.menu_enter || input.fire || input.use_door {
            let pages = self.readthis.as_ref().map_or(1, TextScreen::num_pages);
            if self.readthis_page + 1 < pages {
                self.readthis_page += 1;
            } else {
                self.readthis = None;
                self.screen = GameScreen::MainMenu;
                self.main_sel = menu::ITEM_READ;
            }
        }
    }

    /// The level-select cheat grid: left/right change episode, up/down change
    /// floor, Enter warps (stats carry over, keys reset like any floor change),
    /// Esc resumes play.
    fn update_level_select(&mut self, input: &Input) {
        if input.menu_back {
            self.screen = GameScreen::Playing;
            return;
        }
        let n = self.maps.num_levels(); // 60: 6 episodes x 10 floors
        let (mut ep, mut floor) = (self.level_sel / 10, self.level_sel % 10);
        if input.menu_left {
            ep = (ep + n / 10 - 1) % (n / 10);
        }
        if input.menu_right {
            ep = (ep + 1) % (n / 10);
        }
        if input.menu_up {
            floor = (floor + 9) % 10;
        }
        if input.menu_down {
            floor = (floor + 1) % 10;
        }
        self.level_sel = ep * 10 + floor;
        if input.menu_enter {
            self.level_idx = self.level_sel;
            self.keys = 0;
            self.load_level();
            self.screen = GameScreen::Playing;
        }
    }

    fn update_death(&mut self, dt: f32) {
        self.death_clock += dt;
        if self.death_clock < DEATH_SECS {
            return;
        }
        if self.death_game_over {
            // Out of lives: game over -> high-score check -> title.
            self.begin_high_score_check(HsReturn::Title);
        } else {
            // Restart the floor with a fresh loadout (the original's respawn).
            self.load_level();
            self.health = 100;
            self.ammo = 8;
            self.weapon = WEAPON_PISTOL;
            self.keys = 0;
            self.screen = GameScreen::Playing;
        }
    }

    // --- Menu state machine (WL_MENU.C) ------------------------------------

    fn update_title(&mut self, input: &Input) {
        if input.any_key || input.menu_enter || input.menu_up || input.menu_down {
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_NEW_GAME;
        }
    }

    /// Whether main-menu item `i` is selectable. Save Game is only available
    /// once a game is in progress (WL_MENU.C greys it outside of play).
    pub fn main_item_active(&self, i: usize) -> bool {
        MAIN_ITEMS[i].active && (i != menu::ITEM_SAVE || self.started)
    }

    /// Move the cursor to the next selectable item in `dir` (+1 / -1), skipping
    /// the greyed-out entries (the original leaves them unselectable).
    fn move_selectable(&self, sel: usize, dir: i32) -> usize {
        let n = MAIN_ITEMS.len() as i32;
        let mut i = sel as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if self.main_item_active(i as usize) {
                break;
            }
        }
        i as usize
    }

    fn update_main_menu(&mut self, input: &Input) {
        if input.menu_up {
            self.main_sel = self.move_selectable(self.main_sel, -1);
        }
        if input.menu_down {
            self.main_sel = self.move_selectable(self.main_sel, 1);
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
                menu::ITEM_SOUND => {
                    self.sound_sel = self.sfx_mode as usize;
                    self.screen = GameScreen::Sound;
                }
                menu::ITEM_LOAD => {
                    self.refresh_slots();
                    self.ls_sel = 0;
                    self.screen = GameScreen::LoadGame;
                }
                menu::ITEM_SAVE if self.started => {
                    self.refresh_slots();
                    self.ls_sel = 0;
                    self.entering_name = false;
                    self.screen = GameScreen::SaveGame;
                }
                menu::ITEM_READ => {
                    self.readthis = Some(TextScreen::new(&self.vga, T_HELPART));
                    self.readthis_page = 0;
                    self.screen = GameScreen::ReadThis;
                }
                menu::ITEM_VIEWSCORES => {
                    self.hs_return = HsReturn::Menu;
                    self.hs_edit_slot = None;
                    self.screen = GameScreen::HighScores;
                }
                menu::ITEM_QUIT => self.should_quit = true,
                _ => {}
            }
        }
    }

    // --- Sound options (WL_MENU.C CP_Sound) --------------------------------

    fn update_sound(&mut self, input: &Input) {
        const ROWS: usize = 5; // 3 effects rows + 2 music rows
        if input.menu_up {
            self.sound_sel = (self.sound_sel + ROWS - 1) % ROWS;
        }
        if input.menu_down {
            self.sound_sel = (self.sound_sel + 1) % ROWS;
        }
        if input.menu_back {
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_SOUND;
            return;
        }
        if input.menu_enter {
            match self.sound_sel {
                0 => self.sfx_mode = SfxMode::DigiAdlib,
                1 => self.sfx_mode = SfxMode::AdlibOnly,
                2 => self.sfx_mode = SfxMode::Off,
                3 => self.music_on = true,
                _ => self.music_on = false,
            }
        }
    }

    // --- Load / Save (WL_MENU.C CP_LoadGame / CP_SaveGame) ------------------

    /// Re-read the ten save-slot names for the menu (empty slots stay `None`).
    fn refresh_slots(&mut self) {
        self.save_slots = (0..savegame::NUM_SLOTS)
            .map(|s| savegame::read_slot_header(s).map(|h| h.name))
            .collect();
    }

    fn update_load(&mut self, input: &Input) {
        let n = savegame::NUM_SLOTS;
        if input.menu_up {
            self.ls_sel = (self.ls_sel + n - 1) % n;
        }
        if input.menu_down {
            self.ls_sel = (self.ls_sel + 1) % n;
        }
        if input.menu_back {
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_LOAD;
            return;
        }
        if input.menu_enter && self.save_slots[self.ls_sel].is_some() {
            // A failed load leaves the current game untouched (apply_save only
            // mutates self once the whole file has parsed on a fresh level).
            let _ = self.load_from_slot(self.ls_sel);
        }
    }

    fn update_save(&mut self, input: &Input) {
        let n = savegame::NUM_SLOTS;
        if self.entering_name {
            if input.menu_back {
                self.entering_name = false;
                return;
            }
            if input.menu_enter {
                let name = if self.save_name.trim().is_empty() {
                    format!("Save {}", self.ls_sel + 1)
                } else {
                    self.save_name.clone()
                };
                let _ = self.save_to_slot(self.ls_sel, &name);
                self.entering_name = false;
                self.refresh_slots();
                // Saving from the pause menu returns to play (WL_MENU.C).
                self.screen = GameScreen::Playing;
                return;
            }
            if input.backspace {
                self.save_name.pop();
            }
            if let Some(c) = input.typed
                && !c.is_control()
                && self.save_name.chars().count() < SAVE_NAME_MAX
            {
                self.save_name.push(c);
            }
            return;
        }
        if input.menu_up {
            self.ls_sel = (self.ls_sel + n - 1) % n;
        }
        if input.menu_down {
            self.ls_sel = (self.ls_sel + 1) % n;
        }
        if input.menu_back {
            self.screen = GameScreen::MainMenu;
            self.main_sel = menu::ITEM_SAVE;
            return;
        }
        if input.menu_enter {
            // Prefill the field with the existing name when overwriting a slot.
            self.save_name = self.save_slots[self.ls_sel].clone().unwrap_or_default();
            self.entering_name = true;
        }
    }

    /// Whether the frontend should route key presses to the save-name field
    /// (so gameplay/global shortcuts like `Q`/`M` don't fire while typing).
    pub fn is_text_entry(&self) -> bool {
        (self.screen == GameScreen::SaveGame && self.entering_name)
            || self.screen == GameScreen::HighScoreEntry
    }

    // --- Save-game serialization (see src/savegame.rs) ---------------------

    /// Serialize the whole live game to a versioned byte buffer.
    pub fn write_save(&self, name: &str) -> Vec<u8> {
        let mut w = savegame::Writer::new();
        savegame::write_header(&mut w, name, self.level_idx, self.difficulty.skill());
        w.put_f32(self.player.x);
        w.put_f32(self.player.y);
        w.put_f32(self.player.angle);
        w.put_i32(self.health);
        w.put_i32(self.ammo);
        w.put_i32(self.score);
        w.put_i32(self.lives);
        w.put_u8(self.keys);
        w.put_u32(self.weapon as u32);
        w.put_bool(self.victory);
        // In-progress weapon swing (WL_AGENT.C T_Attack): part of the mid-fight
        // state, so a save taken mid-swing resumes at the same animation phase.
        w.put_bool(self.attacking);
        w.put_u32(self.attack_frame as u32);
        w.put_f32(self.attack_count);
        w.put_u32(self.weapon_frame as u32);
        // Per-floor stats (kills/secrets/treasure/time) persist across a save.
        self.stats.save(&mut w);
        self.world.save(&mut w);
        self.actors.save(&mut w);
        w.buf
    }

    /// Restore the game from a save buffer. On success the game is left on the
    /// saved floor, in `Playing`, resuming exactly where it was saved. On any
    /// parse error the game is left as it was (the level is only rebuilt after
    /// the header validates, and stats/actors are applied last).
    pub fn apply_save(&mut self, data: &[u8]) -> Result<(), SaveError> {
        let mut r = savegame::Reader::new(data);
        let header = savegame::read_header(&mut r)?;
        // Peek the body into locals before touching self, so a truncated file
        // can't leave us half-loaded.
        let level_idx = header.level_idx.min(self.maps.num_levels() - 1);
        self.difficulty = Difficulty::from_index(header.difficulty as usize);
        self.level_idx = level_idx;
        self.load_level(); // rebuild world + actors + player for this floor

        self.player.x = r.get_f32()?;
        self.player.y = r.get_f32()?;
        self.player.angle = r.get_f32()?;
        self.health = r.get_i32()?;
        self.ammo = r.get_i32()?;
        self.score = r.get_i32()?;
        self.lives = r.get_i32()?;
        self.keys = r.get_u8()?;
        self.weapon = r.get_u32()? as usize;
        self.victory = r.get_bool()?;
        self.attacking = r.get_bool()?;
        self.attack_frame = r.get_u32()? as usize;
        self.attack_count = r.get_f32()?;
        self.weapon_frame = r.get_u32()? as usize;
        let stats = LevelStats::load(&mut r)?;
        self.world.load(&mut r)?;
        self.actors.load(&mut r)?;
        self.stats = stats;

        self.inter = None;
        self.died = false;
        self.started = true;
        self.screen = GameScreen::Playing;
        Ok(())
    }

    /// Save the current game to slot `slot` under `name` (creates `saves/`).
    pub fn save_to_slot(&self, slot: usize, name: &str) -> Result<(), SaveError> {
        let bytes = self.write_save(name);
        std::fs::create_dir_all(savegame::saves_dir())?;
        std::fs::write(savegame::slot_path(slot), bytes)?;
        Ok(())
    }

    /// Load slot `slot` into the current game.
    pub fn load_from_slot(&mut self, slot: usize) -> Result<(), SaveError> {
        let data = std::fs::read(savegame::slot_path(slot))?;
        self.apply_save(&data)
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
        self.reset_episode_stats();
        self.load_level();
        self.started = true;
        self.screen = GameScreen::Playing;
    }

    /// Zero the running per-episode ratio/time accumulation (a fresh episode).
    fn reset_episode_stats(&mut self) {
        self.epi_kr_sum = 0;
        self.epi_sr_sum = 0;
        self.epi_tr_sum = 0;
        self.epi_time_sum = 0;
        self.epi_count = 0;
    }

    fn update_play(&mut self, dt: f32, input: &Input) {
        if let Some(w) = input.select_weapon {
            self.weapon = w as usize;
        }

        self.stats.time += dt;
        self.world.tick(dt, &self.player);
        if input.use_door {
            // Cmd_Use (WL_AGENT.C): a secret push-wall first, then the elevator
            // switch, then a door.
            match self.world.use_pushwall(&self.player) {
                PushUse::Activated => self.stats.secrets += 1,
                PushUse::Blocked => {}
                PushUse::NotPushwall => match self.world.use_elevator(&self.player) {
                    ElevatorUse::Normal => {
                        self.begin_intermission(false);
                        return;
                    }
                    ElevatorUse::Secret => {
                        self.begin_intermission(true);
                        return;
                    }
                    ElevatorUse::None => self.world.use_door(&self.player, self.keys),
                },
            }
        }

        if input.turn_left {
            self.player.angle -= TURN_SPEED * dt;
        }
        if input.turn_right {
            self.player.angle += TURN_SPEED * dt;
        }
        self.player.angle += input.turn_delta;

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

        // Killing the floor's end boss wins the episode: the original's boss die
        // chain ends in A_StartDeathCam (deathcam + victoryflag + the episode-end
        // sequence). Detect it here and swing into the deathcam.
        let mut boss_killed = None;
        for kind in self.actors.take_deaths() {
            self.stats.kills += 1;
            if Some(kind) == end_boss(self.level_idx) {
                boss_killed = Some(kind);
            }
        }
        if let Some(kind) = boss_killed {
            self.begin_deathcam(kind);
            return;
        }

        if self.health <= 0 {
            self.health = 0;
            self.died = true;
            self.sounds.push(snd::PLAYERDEATHSND as u8);
            self.lives -= 1;
            // A red-tint pause (WL_GAME.C `Died`), then a floor restart — or,
            // out of lives, the game-over -> high-score -> title path.
            self.death_game_over = self.lives < 0;
            if self.death_game_over {
                self.sounds.push(snd::GAMEOVERSND as u8);
            }
            self.death_clock = 0.0;
            self.screen = GameScreen::Death;
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
        self.sounds.push(match self.weapon {
            WEAPON_MACHINEGUN => snd::ATKMACHINEGUNSND as u8,
            WEAPON_CHAINGUN => snd::ATKGATLINGSND as u8,
            _ => snd::ATKPISTOLSND as u8,
        });
        let points = self.actors.player_fire(
            &self.world,
            self.player.x,
            self.player.y,
            self.player.angle,
            false,
        );
        self.score += points;
        if !self.infinite_ammo {
            self.ammo -= 1;
        }
    }

    fn fire_knife(&mut self) {
        self.sounds.push(snd::ATKKNIFESND as u8);
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
    /// Emits the pickup sound only when the item is actually taken (GetBonus).
    fn get_bonus(&mut self, bonus: Bonus) -> bool {
        let took = self.apply_bonus(bonus);
        if took {
            self.sounds.push(bonus_sound(bonus));
        }
        took
    }

    fn apply_bonus(&mut self, bonus: Bonus) -> bool {
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
            Bonus::Cross => self.take_treasure(100),
            Bonus::Chalice => self.take_treasure(500),
            Bonus::Bible => self.take_treasure(1000),
            Bonus::Crown => self.take_treasure(5000),
            Bonus::FullHeal => {
                // 1-up: full health, +25 ammo, +1 life. Counts as treasure
                // (WL_AGENT.C GetBonus increments treasurecount for bo_fullheal).
                self.health = 100;
                self.ammo = (self.ammo + 25).min(99);
                self.lives += 1;
                self.stats.treasure += 1;
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

    /// A valuable pickup: score plus a treasure count (WL_AGENT.C GetBonus).
    fn take_treasure(&mut self, points: i32) -> bool {
        self.score += points;
        self.stats.treasure += 1;
        true
    }

    pub fn render(&mut self, fb: &mut Framebuffer) {
        match self.screen {
            GameScreen::Title => {
                self.menu.render_title(fb);
                return;
            }
            GameScreen::MainMenu => {
                self.menu.render_main(fb, self.main_sel, self.started);
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
            GameScreen::Sound => {
                self.menu.render_sound(fb, self.sfx_mode, self.music_on, self.sound_sel);
                return;
            }
            GameScreen::LoadGame => {
                self.menu.render_load(fb, &self.save_slots, self.ls_sel);
                return;
            }
            GameScreen::SaveGame => {
                self.menu.render_save(
                    fb,
                    &self.save_slots,
                    self.ls_sel,
                    self.entering_name,
                    &self.save_name,
                );
                return;
            }
            GameScreen::Intermission => {
                if let Some(it) = &self.inter {
                    self.inter_gfx.render_intermission(fb, it);
                }
                return;
            }
            GameScreen::GetPsyched => {
                let progress = (self.psyched_clock / LOAD_SCREEN_SECS).clamp(0.0, 1.0);
                self.inter_gfx.render_get_psyched(fb, progress);
                return;
            }
            GameScreen::Death => {
                // The frozen fight view with a deepening red death tint.
                raycast::render(fb, &self.vswap, &self.world, &mut self.actors, &self.player, VIEW_H);
                let weapon_sprite =
                    hud::weapon_ready_sprite(&self.vswap, self.weapon) + self.weapon_frame;
                hud::draw_weapon(fb, &self.vswap, weapon_sprite);
                self.draw_hud(fb);
                let t = (self.death_clock / DEATH_SECS).clamp(0.0, 1.0);
                red_tint(fb, 0.3 + 0.5 * t);
                return;
            }
            GameScreen::DeathCam => {
                raycast::render(fb, &self.vswap, &self.world, &mut self.actors, &self.player, VIEW_H);
                self.inter_gfx.draw_seeagain(fb);
                return;
            }
            GameScreen::Victory => {
                self.inter_gfx.render_victory(fb, &self.victory_stats);
                return;
            }
            GameScreen::EndText => {
                if let Some(t) = &self.endtext {
                    t.render(fb, &self.vga, self.endtext_page);
                }
                return;
            }
            GameScreen::HighScores | GameScreen::HighScoreEntry => {
                let editing = self.hs_edit_slot.map(|s| (s, self.hs_name.as_str()));
                self.inter_gfx.render_high_scores(fb, &self.highscores, editing);
                return;
            }
            GameScreen::ReadThis => {
                if let Some(t) = &self.readthis {
                    t.render(fb, &self.vga, self.readthis_page);
                }
                return;
            }
            GameScreen::LevelSelect => {
                let names = self.level_names.get_or_insert_with(|| {
                    (0..self.maps.num_levels()).map(|i| self.maps.level(i).name).collect()
                });
                self.menu.render_level_select(fb, names, self.level_sel);
                return;
            }
            GameScreen::Playing => {}
        }
        raycast::render(fb, &self.vswap, &self.world, &mut self.actors, &self.player, VIEW_H);
        // The firing animation offsets from the weapon's ready frame.
        let weapon_sprite = hud::weapon_ready_sprite(&self.vswap, self.weapon) + self.weapon_frame;
        hud::draw_weapon(fb, &self.vswap, weapon_sprite);
        self.draw_hud(fb);
    }

    /// Draw the status bar from the current player stats.
    fn draw_hud(&self, fb: &mut Framebuffer) {
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

/// Blend the whole framebuffer toward red by `amount` (0..1), the death fizzle
/// (a cheap stand-in for WL_GAME.C's per-pixel `FizzleFade` to red).
fn red_tint(fb: &mut Framebuffer, amount: f32) {
    let a = amount.clamp(0.0, 1.0);
    for px in fb.pixels.iter_mut() {
        let r = (*px & 0xFF) as f32;
        let g = ((*px >> 8) & 0xFF) as f32;
        let b = ((*px >> 16) & 0xFF) as f32;
        // Target pure red (0xAA,0,0)-ish.
        let nr = (r + (170.0 - r) * a) as u32;
        let ng = (g * (1.0 - a)) as u32;
        let nb = (b * (1.0 - a)) as u32;
        *px = 0xFF00_0000 | (nb << 16) | (ng << 8) | nr;
    }
}

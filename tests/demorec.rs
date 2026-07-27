//! Attract-demo checks (requires `data/`): the recording format roundtrip,
//! record/replay bit-determinism (including rendering during playback), the
//! title -> credits -> scores -> demo -> title attract cycle, and the shipped
//! `demos/` recordings.

use wolf3d::demorec::{self, AiPolicy, Demo};
use wolf3d::fb::Framebuffer;
use wolf3d::game::{
    ATTRACT_CREDITS_SECS, ATTRACT_SCORES_SECS, ATTRACT_TITLE_SECS, Game, GameScreen, Input,
};

const DT: f32 = 1.0 / 70.0;

/// Record a short E1M1 fight headlessly from a fresh game (door, movement,
/// turns, auto-aimed gunfire — kills guards, consumes the RNG, and takes return
/// fire), returning the demo and the game in its post-run state. Aiming is
/// expressed as per-tic `turn_delta` inputs so the demo is purely input-driven.
fn record_fight() -> (Demo, Game) {
    let mut game = Game::new(0);
    let mut demo = Demo::begin(&game);
    fn tick(game: &mut Game, demo: &mut Demo, input: Input) {
        demo.push(&input);
        game.update(DT, &input);
    }
    fn hold(game: &mut Game, demo: &mut Demo, input: Input, secs: f32) {
        for _ in 0..(secs / DT).round() as u32 {
            tick(game, demo, input);
        }
    }
    // The demo-1 route: through the first door, then south into the guard room.
    hold(
        &mut game,
        &mut demo,
        Input {
            forward: true,
            ..Default::default()
        },
        0.8,
    );
    tick(
        &mut game,
        &mut demo,
        Input {
            use_door: true,
            ..Default::default()
        },
    );
    hold(&mut game, &mut demo, Input::default(), 0.9);
    hold(
        &mut game,
        &mut demo,
        Input {
            forward: true,
            run: true,
            ..Default::default()
        },
        0.75,
    );
    hold(
        &mut game,
        &mut demo,
        Input {
            turn_right: true,
            ..Default::default()
        },
        0.65,
    );
    hold(
        &mut game,
        &mut demo,
        Input {
            forward: true,
            ..Default::default()
        },
        1.6,
    );
    // Auto-aimed fire at the nearest live guard, recorded as turn_delta.
    for _ in 0..(2.5 / DT) as u32 {
        let (px, py) = (game.player.x, game.player.y);
        let target = game
            .actors
            .list
            .iter()
            .filter(|a| !a.dead && wolf3d::actors::line_clear(&game.world, px, py, a.x, a.y))
            .map(|a| ((a.x - px).powi(2) + (a.y - py).powi(2), a.x, a.y))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let turn_delta = target.map_or(0.0, |(_, bx, by)| {
            (by - py).atan2(bx - px) - game.player.angle
        });
        tick(
            &mut game,
            &mut demo,
            Input {
                fire: target.is_some(),
                turn_delta,
                ..Default::default()
            },
        );
    }
    hold(&mut game, &mut demo, Input::default(), 1.5);
    (demo, game)
}

/// Assert the full observable simulation state of two games is identical
/// (player, stats, every actor, and the RNG cursor).
fn assert_states_match(a: &Game, b: &Game) {
    assert_eq!(
        a.player.x.to_bits(),
        b.player.x.to_bits(),
        "player x diverged"
    );
    assert_eq!(
        a.player.y.to_bits(),
        b.player.y.to_bits(),
        "player y diverged"
    );
    assert_eq!(
        a.player.angle.to_bits(),
        b.player.angle.to_bits(),
        "player angle diverged"
    );
    assert_eq!(a.health, b.health, "health diverged");
    assert_eq!(a.ammo, b.ammo, "ammo diverged");
    assert_eq!(a.score, b.score, "score diverged");
    assert_eq!(
        a.actors.rng_index(),
        b.actors.rng_index(),
        "rng index diverged"
    );
    assert_eq!(a.actors.list.len(), b.actors.list.len());
    for (i, (x, y)) in a.actors.list.iter().zip(&b.actors.list).enumerate() {
        assert_eq!(x.x.to_bits(), y.x.to_bits(), "actor {i} x diverged");
        assert_eq!(x.y.to_bits(), y.y.to_bits(), "actor {i} y diverged");
        assert_eq!(x.health(), y.health(), "actor {i} health diverged");
        assert_eq!(x.dead, y.dead, "actor {i} dead flag diverged");
    }
}

// --- Format roundtrip --------------------------------------------------------

#[test]
fn demo_format_roundtrips() {
    let (mut demo, _) = record_fight();
    demo.ai_policy = Some(AiPolicy {
        seed: 0x1234_5678,
        engage_range: 7.5,
        aim_slack: 0.25,
        strafe_period: 17,
        strafe_left_bias: false,
        panic_health: 29,
        hunt_kills: false,
        seek_secrets: true,
    });
    let bytes = demo.to_bytes();
    let loaded = Demo::from_bytes(&bytes).expect("parse recorded demo");
    assert_eq!(loaded.level_idx, demo.level_idx);
    assert_eq!(loaded.difficulty, demo.difficulty);
    assert_eq!(loaded.rng_index, demo.rng_index);
    assert_eq!(loaded.snd_rng_index, demo.snd_rng_index);
    assert_eq!(loaded.player_x.to_bits(), demo.player_x.to_bits());
    assert_eq!(loaded.player_y.to_bits(), demo.player_y.to_bits());
    assert_eq!(loaded.player_angle.to_bits(), demo.player_angle.to_bits());
    assert_eq!(loaded.health, demo.health);
    assert_eq!(loaded.ammo, demo.ammo);
    assert_eq!(loaded.weapon, demo.weapon);
    assert_eq!(loaded.keys, demo.keys);
    assert_eq!(loaded.god, demo.god);
    assert_eq!(loaded.windowed, demo.windowed);
    assert_eq!(loaded.ai_policy, demo.ai_policy);
    assert_eq!(loaded.tics.len(), demo.tics.len());
    // The parsed inputs re-serialize to the identical byte stream.
    assert_eq!(loaded.to_bytes(), bytes, "roundtrip must be byte-identical");

    // Corrupt magic / truncation are rejected cleanly.
    assert!(Demo::from_bytes(b"NOTADEMO").is_err());
    assert!(Demo::from_bytes(&bytes[..bytes.len() - 3]).is_err());
}

// --- Record / replay determinism ---------------------------------------------

#[test]
fn replay_reproduces_recorded_run_exactly() {
    let (demo, recorded) = record_fight();
    assert!(recorded.score > 0, "the scripted fight should score a kill");

    // Replay through the real attract path, rendering every frame like the
    // windowed app does (the save/restore-visible wrap must neutralize it).
    let mut replay = Game::new(0);
    replay.start_attract_demo(&demo);
    assert_eq!(replay.screen, GameScreen::Attract);
    let mut fb = Framebuffer::new();
    for _ in 0..demo.tics.len() {
        replay.update(DT, &Input::default());
        replay.render(&mut fb);
    }
    assert_eq!(
        replay.screen,
        GameScreen::Attract,
        "demo should still be playing"
    );
    assert_states_match(&recorded, &replay);

    // The tic after the last recorded input ends the demo -> back to the title.
    replay.update(DT, &Input::default());
    assert_eq!(replay.screen, GameScreen::Title);
    assert!(replay.attract_mode, "the attract loop resumes after a demo");
}

#[test]
fn any_key_stops_demo_playback() {
    let (demo, _) = record_fight();
    let mut game = Game::new(0);
    game.demos = vec![demo];
    game.to_title();
    game.start_attract_demo(&game.demos[0].clone());
    assert_eq!(game.screen, GameScreen::Attract);

    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::MainMenu,
        "a key during the demo opens the menu"
    );
    assert!(!game.attract_mode, "the attract loop stops on a key");
}

// --- Attract-loop state transitions ------------------------------------------

#[test]
fn attract_loop_cycles_title_credits_scores_demo_title() {
    let (demo, _) = record_fight();
    let mut game = Game::new(0);
    game.demos = vec![demo];
    game.to_title();
    assert_eq!(game.screen, GameScreen::Title);
    assert!(game.attract_mode);

    // Title auto-advances to the credits page after its timeout.
    let idle = Input::default();
    let step_past = |game: &mut Game, secs: f32| {
        for _ in 0..((secs + 0.5) / DT) as u32 {
            game.update(DT, &idle);
        }
    };
    step_past(&mut game, ATTRACT_TITLE_SECS);
    assert_eq!(
        game.screen,
        GameScreen::Credits,
        "title should advance to credits"
    );

    step_past(&mut game, ATTRACT_CREDITS_SECS);
    assert_eq!(
        game.screen,
        GameScreen::HighScores,
        "credits should advance to scores"
    );

    step_past(&mut game, ATTRACT_SCORES_SECS);
    assert_eq!(
        game.screen,
        GameScreen::Attract,
        "scores should advance to demo playback"
    );

    // The demo runs to completion and loops back to the title.
    let mut guard = 0;
    while game.screen == GameScreen::Attract {
        game.update(DT, &idle);
        guard += 1;
        assert!(guard < 20_000, "demo never finished");
    }
    assert_eq!(
        game.screen,
        GameScreen::Title,
        "a finished demo loops back to the title"
    );
    assert!(game.attract_mode);
}

#[test]
fn attract_loop_skips_demo_stage_when_no_demos() {
    let mut game = Game::new(0);
    assert!(game.demos.is_empty());
    game.to_title();

    let idle = Input::default();
    let step_past = |game: &mut Game, secs: f32| {
        for _ in 0..((secs + 0.5) / DT) as u32 {
            game.update(DT, &idle);
        }
    };
    step_past(&mut game, ATTRACT_TITLE_SECS);
    assert_eq!(game.screen, GameScreen::Credits);
    step_past(&mut game, ATTRACT_CREDITS_SECS);
    assert_eq!(game.screen, GameScreen::HighScores);
    step_past(&mut game, ATTRACT_SCORES_SECS);
    assert_eq!(
        game.screen,
        GameScreen::Title,
        "no demos: scores loop straight to the title"
    );
}

#[test]
fn any_key_during_attract_screens_opens_menu() {
    let mut game = Game::new(0);
    game.to_title();
    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::MainMenu,
        "a key on the title opens the menu"
    );
    assert!(!game.attract_mode);

    // Credits page.
    game.to_title();
    game.screen = GameScreen::Credits;
    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::MainMenu,
        "a key on the credits opens the menu"
    );

    // High scores during attract.
    game.to_title();
    game.screen = GameScreen::HighScores;
    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::MainMenu,
        "a key on the scores opens the menu"
    );
    assert!(!game.attract_mode);
}

#[test]
fn back_to_demo_menu_entry_starts_demo_when_available() {
    let (demo, _) = record_fight();
    let mut game = Game::new(0);
    game.demos = vec![demo];
    game.screen = GameScreen::MainMenu;
    game.started = false;
    game.attract_mode = false;
    game.main_sel = wolf3d::menu::ITEM_BACKTODEMO;
    assert!(
        game.main_item_active(wolf3d::menu::ITEM_BACKTODEMO),
        "Back to Demo is wired up"
    );

    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::Attract,
        "Back to Demo should start attract playback immediately when demos exist"
    );
    assert!(game.attract_mode);
}

#[test]
fn back_to_demo_menu_entry_returns_to_title_without_demos() {
    let mut game = Game::new(0);
    assert!(game.demos.is_empty());
    game.screen = GameScreen::MainMenu;
    game.started = false;
    game.attract_mode = false;
    game.main_sel = wolf3d::menu::ITEM_BACKTODEMO;

    game.update(
        DT,
        &Input {
            menu_enter: true,
            ..Default::default()
        },
    );
    assert_eq!(
        game.screen,
        GameScreen::Title,
        "Back to Demo returns to the title"
    );
    assert!(game.attract_mode, "and restarts the attract loop");
}

// --- Shipped demos ------------------------------------------------------------

#[test]
fn shipped_demos_load_and_replay() {
    let demos = demorec::load_all();
    assert!(
        !demos.is_empty(),
        "demos/ should ship at least one attract demo"
    );
    // Full-floor demos leave Attract early when the elevator/boss ends the
    // run; walkabout demos stay until the stream ends. Both must return to
    // the title without dying.
    for demo in &demos {
        assert!(!demo.windowed, "shipped demos are headless recordings");
        assert!(!demo.tics.is_empty());
        let mut game = Game::new(0);
        game.to_title();
        game.start_attract_demo(demo);
        for _ in 0..=demo.tics.len() {
            if game.screen != GameScreen::Attract {
                break;
            }
            game.update(DT, &Input::default());
        }
        assert_ne!(
            game.screen,
            GameScreen::Death,
            "shipped demo must not die mid-run"
        );
        for _ in 0..demo.tics.len() {
            if game.screen != GameScreen::Attract {
                break;
            }
            game.update(DT, &Input::default());
        }
        if game.screen == GameScreen::Attract {
            game.update(DT, &Input::default());
        }
        assert_eq!(
            game.screen,
            GameScreen::Title,
            "demo should return to the title when finished"
        );
        assert!(game.health > 0, "the shipped demo must survive its run");
    }
}

#[test]
fn automatic_attract_rotation_excludes_legacy_shortcuts() {
    let mut game = Game::new(0);
    game.load_attract_demos();
    assert!(!game.demos.is_empty());
    for demo in &game.demos {
        assert!(!demo.god, "automatic attract demo must be mortal");
        assert!(
            !demo.clear_actors,
            "automatic attract demo must retain enemies"
        );
        assert!(
            !demo.has_direct_turns(),
            "automatic attract demo must use normal-rate turn keys"
        );
    }
}

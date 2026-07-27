//! Front-end menu flow (requires `data/`): the Quit confirmation prompt from
//! WL_MENU.C `CP_Quit`.
//!
//! The frontend maps Y to `menu_enter` and N/Esc to `menu_back`, so these drive
//! the prompt exactly as the windowed game does.

use std::collections::HashSet;

use wolf3d::fb::Framebuffer;
use wolf3d::game::{Game, GameScreen, Input};
use wolf3d::menu::{END_STRINGS, ITEM_QUIT};

const DT: f32 = 1.0 / 70.0;

/// A game sitting on the main menu with the cursor on Quit.
fn at_quit() -> Game {
    let mut game = Game::new(0);
    game.to_title();
    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::MainMenu);
    // Quit is the last item, so one step up from New Game wraps onto it.
    game.update(
        DT,
        &Input {
            menu_up: true,
            ..Default::default()
        },
    );
    assert_eq!(game.main_sel, ITEM_QUIT);
    game
}

fn press(game: &mut Game, input: Input) {
    game.update(DT, &input);
}

/// Y at the prompt (the frontend maps it to accept).
fn enter() -> Input {
    Input {
        menu_enter: true,
        ..Default::default()
    }
}

/// N or Esc at the prompt (the frontend maps both to cancel).
fn back() -> Input {
    Input {
        menu_back: true,
        ..Default::default()
    }
}

/// Quit asks first: it opens the prompt rather than exiting outright, and
/// cancelling (N / Esc) returns to the menu with the game still running.
#[test]
fn quit_prompts_before_exiting() {
    let mut game = at_quit();

    press(&mut game, enter());
    assert_eq!(
        game.screen,
        GameScreen::QuitConfirm,
        "picking Quit should raise the confirmation"
    );
    assert!(!game.should_quit, "the prompt must not quit on its own");

    press(&mut game, back());
    assert_eq!(game.screen, GameScreen::MainMenu, "N / Esc cancels");
    assert!(!game.should_quit);
    assert_eq!(game.main_sel, ITEM_QUIT, "the cursor stays on Quit");

    // Reopen and accept.
    press(&mut game, enter());
    assert_eq!(game.screen, GameScreen::QuitConfirm);
    press(&mut game, enter());
    assert!(game.should_quit, "Y quits");
}

/// The prompt draws a message box over the still-visible main menu, so its
/// frame differs from the bare menu but keeps the menu's own pixels around it.
#[test]
fn quit_prompt_draws_over_the_menu() {
    let mut game = at_quit();
    let mut menu_fb = Framebuffer::new();
    game.render(&mut menu_fb);

    press(&mut game, enter());
    let mut prompt_fb = Framebuffer::new();
    game.render(&mut prompt_fb);

    assert_ne!(
        menu_fb.pixels, prompt_fb.pixels,
        "the prompt should be visible"
    );
    let changed = menu_fb
        .pixels
        .iter()
        .zip(&prompt_fb.pixels)
        .filter(|(a, b)| a != b)
        .count();
    let share = changed as f32 / menu_fb.pixels.len() as f32;
    assert!(
        (0.02..0.4).contains(&share),
        "the box should cover part of the screen, not none or all of it ({share:.3})"
    );
}

/// CP_Quit picks `endStrings[(US_RndT()&7) + (US_RndT()&1)]`, which spans all
/// nine taunts. Reopening the prompt reaches every one of them and never
/// indexes past the table.
#[test]
fn quit_prompt_uses_every_message() {
    let mut game = at_quit();
    let mut seen: HashSet<usize> = HashSet::new();

    for _ in 0..256 {
        press(&mut game, enter());
        assert_eq!(game.screen, GameScreen::QuitConfirm);
        assert!(
            game.quit_msg < END_STRINGS.len(),
            "message index {} is out of range",
            game.quit_msg
        );
        seen.insert(game.quit_msg);
        press(&mut game, back());
    }

    assert_eq!(
        seen.len(),
        END_STRINGS.len(),
        "all {} taunts should be reachable, saw {:?}",
        END_STRINGS.len(),
        seen.len()
    );
}

/// The prompt's random pick runs on its own stream, so opening (and cancelling)
/// the Quit prompt cannot shift the gameplay simulation.
#[test]
fn quit_prompt_does_not_perturb_gameplay_rng() {
    let fight = |prompt_first: bool| {
        let mut game = Game::new(0);
        if prompt_first {
            game.to_title();
            press(
                &mut game,
                Input {
                    any_key: true,
                    ..Default::default()
                },
            );
            press(
                &mut game,
                Input {
                    menu_up: true,
                    ..Default::default()
                },
            );
            for _ in 0..8 {
                press(&mut game, enter());
                press(&mut game, back());
            }
            game.screen = GameScreen::Playing;
        }
        let fire = Input {
            fire: true,
            ..Default::default()
        };
        for _ in 0..200 {
            game.update(DT, &fire);
        }
        (game.score, game.ammo, game.health, game.actors.list.len())
    };
    assert_eq!(fight(true), fight(false));
}

/// HandleMenu gun twitch: C_CURSOR1 for 70 tics, brief C_CURSOR2 for 8.
#[test]
fn gun_cursor_twitches_with_handlemenu_cadence() {
    let mut game = Game::new(0);
    game.to_title();
    press(
        &mut game,
        Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::MainMenu);

    // Sample a full 78-tic cycle: rest should dominate (~70/78), twitch the rest.
    let mut rest = 0u32;
    let mut twitch = 0u32;
    for _ in 0..78 {
        if game.menu.gun_is_twitching() {
            twitch += 1;
        } else {
            rest += 1;
        }
        game.update(DT, &Input::default());
    }
    assert_eq!(rest, 70, "CURSOR1 rest should be 70 tics/cycle, got {rest}");
    assert_eq!(
        twitch, 8,
        "CURSOR2 twitch should be 8 tics/cycle, got {twitch}"
    );

    // Twitch window is contiguous (no rest sandwiched inside).
    let mut saw_edge = false;
    let mut prev = game.menu.gun_is_twitching();
    for _ in 0..200 {
        game.update(DT, &Input::default());
        let now = game.menu.gun_is_twitching();
        if now && !prev {
            // Entered twitch: next 7 tics must stay twitching, 8th leaves.
            for _ in 0..7 {
                game.update(DT, &Input::default());
                assert!(game.menu.gun_is_twitching(), "twitch must be contiguous");
            }
            game.update(DT, &Input::default());
            assert!(
                !game.menu.gun_is_twitching(),
                "twitch should end after 8 tics"
            );
            saw_edge = true;
            break;
        }
        prev = now;
    }
    assert!(
        saw_edge,
        "should observe a rest->twitch edge within 200 tics"
    );
}

/// Difficulty screen draws the NM skill list + selected BJ face (not just one
/// centered banner).
#[test]
fn difficulty_screen_lists_skills_and_face() {
    let mut game = Game::new(0);
    game.to_title();
    press(
        &mut game,
        Input {
            any_key: true,
            ..Default::default()
        },
    );
    // New Game -> episode 1 -> difficulty.
    press(&mut game, enter());
    press(&mut game, enter());
    assert_eq!(game.screen, GameScreen::Difficulty);

    let mut fb = Framebuffer::new();
    game.render(&mut fb);

    // Skill phrases appear as text; sample the baby skill string area near
    // NM_X+24, NM_Y (50+24, 100) — should not be the flat bord fill.
    let sample = fb.pixels[100 * 320 + 80];
    let bord = wolf3d::menu::Colors::for_variant(false).bord();
    let bord_rgba = wolf3d::assets::palette::PALETTE[bord as usize];
    assert_ne!(
        sample, bord_rgba,
        "difficulty window should paint over the bord clear"
    );

    // BJ skill face sits at NM_X+185, NM_Y+7 = (235, 107); that cell must differ
    // from window fill too.
    let face_px = fb.pixels[110 * 320 + 240];
    assert_ne!(face_px, bord_rgba, "skill face should be drawn");

    // Cursor down cycles skills without leaving the screen.
    press(
        &mut game,
        Input {
            menu_down: true,
            ..Default::default()
        },
    );
    assert_eq!(game.screen, GameScreen::Difficulty);
}

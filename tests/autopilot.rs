//! In-game floor-finishing autopilot (`I` key).

use wolf3d::game::{Game, GameScreen, Input};

const DT: f32 = 1.0 / 70.0;

/// On E1M1 the pilot pathfinds to the elevator and finishes the floor.
#[test]
fn autopilot_finishes_e1m1() {
    let mut game = Game::new(0);
    game.screen = GameScreen::Playing;
    assert!(game.toggle_autopilot(), "autopilot should start on E1M1");
    assert!(game.autopilot_active());
    assert!(game.god, "pilot engages god mode");

    let mut finished = false;
    let mut last_active = true;
    for n in 0..(70 * 120) {
        game.update(DT, &Input::default());
        if game.screen == GameScreen::Intermission {
            finished = true;
            break;
        }
        if !game.autopilot_active() && game.screen == GameScreen::Playing {
            eprintln!(
                "pilot stopped at tic {n} pos=({:.1},{:.1}) screen={:?}",
                game.player.x, game.player.y, game.screen
            );
            last_active = false;
            break;
        }
    }
    assert!(
        finished,
        "autopilot should reach the intermission (screen={:?}, active={last_active}, pos=({:.1},{:.1}))",
        game.screen, game.player.x, game.player.y
    );
    assert!(
        !game.autopilot_active(),
        "pilot should clear when the floor ends"
    );
}

/// Toggling I twice restores player control.
#[test]
fn autopilot_toggle_off() {
    let mut game = Game::new(0);
    game.screen = GameScreen::Playing;
    game.god = false;
    assert!(game.toggle_autopilot());
    assert!(game.god);
    assert!(!game.toggle_autopilot());
    assert!(!game.autopilot_active());
    assert!(!game.god, "disengage restores prior god flag");
}

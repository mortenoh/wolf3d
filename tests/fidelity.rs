//! Fidelity checks for original-game behavior: score extralives
//! (GivePoints / EXTRAPOINTS), bestweapon tracking, per-floor ceiling colours,
//! SOD spectre rematerialisation (A_Dormant), the death FizzleFade, BJ face
//! glances (UpdateFace), and area connectivity (`areabyplayer`).

use wolf3d::actors::Kind;
use wolf3d::assets::{MapSet, data_dir};
use wolf3d::fb::{Framebuffer, WIDTH};
use wolf3d::fizzle::{self, Fizzle};
use wolf3d::game::{
    Difficulty, EXTRAPOINTS, Game, GameScreen, Input, WEAPON_CHAINGUN, WEAPON_KNIFE,
    WEAPON_MACHINEGUN, WEAPON_PISTOL,
};
use wolf3d::hud::VIEW_H;
use wolf3d::raycast::{self, Bonus, World};
use wolf3d::variant::Variant;

const DT: f32 = 1.0 / 70.0;

/// Drive point-blank fire at the nearest matching live enemy until it dies or
/// `max_tics` elapse. Returns true if something died (score rose from a kill).
fn kill_nearest(game: &mut Game, pred: impl Fn(Kind) -> bool, max_tics: u32) -> bool {
    let start_score = game.score;
    for _ in 0..max_tics {
        let target = game
            .actors
            .list
            .iter()
            .filter(|a| !a.dead && pred(a.kind))
            .map(|a| {
                let d = (a.x - game.player.x).powi(2) + (a.y - game.player.y).powi(2);
                (d, a.x, a.y)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, tx, ty)) = target else {
            break;
        };
        game.player.x = tx - 0.55;
        game.player.y = ty;
        game.player.angle = 0.0;
        game.update(
            DT,
            &Input {
                fire: true,
                ..Default::default()
            },
        );
        if game.score > start_score {
            return true;
        }
    }
    game.score > start_score
}

/// GivePoints awards a life every EXTRAPOINTS (40_000).
#[test]
fn score_thresholds_award_extralives() {
    let mut game = Game::new(0);
    game.god = true;
    game.infinite_ammo = true;
    game.weapon = WEAPON_CHAINGUN;
    game.bestweapon = WEAPON_CHAINGUN;
    game.ammo = 99;
    game.score = EXTRAPOINTS - 50;
    game.nextextra = EXTRAPOINTS;
    let lives_before = game.lives;

    assert!(
        kill_nearest(
            &mut game,
            |k| matches!(
                k,
                Kind::Guard | Kind::Officer | Kind::Ss | Kind::Mutant | Kind::Dog
            ),
            400
        ),
        "expected a kill to award points"
    );
    assert!(
        game.score >= EXTRAPOINTS,
        "score should cross EXTRAPOINTS, got {}",
        game.score
    );
    assert!(
        game.lives > lives_before,
        "crossing EXTRAPOINTS must award a life ({} -> {})",
        lives_before,
        game.lives
    );
    assert_eq!(game.nextextra, EXTRAPOINTS * 2);
}

/// GiveExtraMan caps lives at 9.
#[test]
fn extralives_cap_at_nine() {
    let mut game = Game::new(0);
    game.god = true;
    game.infinite_ammo = true;
    game.weapon = WEAPON_CHAINGUN;
    game.bestweapon = WEAPON_CHAINGUN;
    game.ammo = 99;
    game.lives = 9;
    game.score = EXTRAPOINTS - 50;
    game.nextextra = EXTRAPOINTS;

    let _ = kill_nearest(
        &mut game,
        |k| {
            matches!(
                k,
                Kind::Guard | Kind::Officer | Kind::Ss | Kind::Mutant | Kind::Dog
            )
        },
        400,
    );
    assert!(game.lives <= 9, "lives must cap at 9, got {}", game.lives);
    // Even when already at 9, crossing the threshold advances nextextra.
    if game.score >= EXTRAPOINTS {
        assert_eq!(game.lives, 9);
        assert_eq!(game.nextextra, EXTRAPOINTS * 2);
    }
}

/// bestweapon gates 1-4 weapon selection; pickups raise it.
#[test]
fn bestweapon_gates_selection_and_upgrades() {
    let mut game = Game::new(0);
    game.start_new_game(0, Difficulty::Hard);
    assert_eq!(game.weapon, WEAPON_PISTOL);
    assert_eq!(game.bestweapon, WEAPON_PISTOL);

    // Cannot select chaingun without owning it.
    game.update(
        DT,
        &Input {
            select_weapon: Some(WEAPON_CHAINGUN as u8),
            ..Default::default()
        },
    );
    assert_eq!(game.weapon, WEAPON_PISTOL);

    // Knife is always allowed.
    game.update(
        DT,
        &Input {
            select_weapon: Some(WEAPON_KNIFE as u8),
            ..Default::default()
        },
    );
    assert_eq!(game.weapon, WEAPON_KNIFE);

    // Collect a machinegun if the floor has one.
    if let Some((x, y)) = game
        .world
        .statics
        .iter()
        .find(|s| matches!(s.bonus, Some(Bonus::MachineGun)) && !s.picked)
        .map(|s| (s.x, s.y))
    {
        game.player.x = x;
        game.player.y = y;
        game.update(DT, &Input::default());
        assert_eq!(game.bestweapon, WEAPON_MACHINEGUN);
        assert_eq!(game.weapon, WEAPON_MACHINEGUN);

        // Now machinegun selection works; chaingun still does not.
        game.update(
            DT,
            &Input {
                select_weapon: Some(WEAPON_PISTOL as u8),
                ..Default::default()
            },
        );
        assert_eq!(game.weapon, WEAPON_PISTOL);
        game.update(
            DT,
            &Input {
                select_weapon: Some(WEAPON_MACHINEGUN as u8),
                ..Default::default()
            },
        );
        assert_eq!(game.weapon, WEAPON_MACHINEGUN);
        game.update(
            DT,
            &Input {
                select_weapon: Some(WEAPON_CHAINGUN as u8),
                ..Default::default()
            },
        );
        assert_eq!(game.weapon, WEAPON_MACHINEGUN);
    }
}

/// Per-floor ceiling palette matches WL_DRAW.C vgaCeiling[].
#[test]
fn ceiling_colour_varies_by_floor() {
    assert_eq!(raycast::ceiling_pal(0, false), 0x1d);
    assert_eq!(raycast::ceiling_pal(9, false), 0xbf);
    assert_ne!(
        raycast::ceiling_pal(0, false),
        raycast::ceiling_pal(10, false)
    );

    let maps = MapSet::load(&data_dir()).expect("data");
    let w0 = World::new_variant(maps.level(0), false, 0);
    let w9 = World::new_variant(maps.level(9), false, 9);
    assert_eq!(w0.ceiling_pal, 0x1d);
    assert_eq!(w9.ceiling_pal, 0xbf);
}

/// SOD spectres fade out then rematerialise via A_Dormant.
#[test]
fn sod_spectre_rematerialises() {
    if !Variant::present("SOD") {
        eprintln!("skip: no SOD data");
        return;
    }
    let maps = MapSet::load_ext(&data_dir(), "SOD").expect("sod maps");
    let floor = (0..maps.num_levels())
        .find(|&i| maps.level(i).plane1.contains(&106))
        .expect("SOD should have a spectre spawn (code 106)");

    let mut game = Game::new_variant(floor, Variant::sod());
    game.god = true;
    game.infinite_ammo = true;
    game.weapon = WEAPON_CHAINGUN;
    game.bestweapon = WEAPON_CHAINGUN;
    game.ammo = 99;

    // Isolate spectres so we can track index 0.
    game.actors.list.retain(|a| a.kind == Kind::Spectre);
    assert!(
        !game.actors.list.is_empty(),
        "expected at least one spectre on floor {floor}"
    );
    let (sx, sy) = (game.actors.list[0].x, game.actors.list[0].y);

    // Kill it point-blank.
    game.player.x = sx - 0.55;
    game.player.y = sy;
    game.player.angle = 0.0;
    let mut died = false;
    for _ in 0..300 {
        game.update(
            DT,
            &Input {
                fire: true,
                ..Default::default()
            },
        );
        if game.actors.list[0].dead {
            died = true;
            break;
        }
    }
    assert!(died, "spectre should die when shot");

    // Stand clear so A_Dormant can rematerialise after the 300-tic fade.
    game.player.x = sx + 5.0;
    game.player.y = sy + 5.0;
    let mut woke = false;
    for _ in 0..(500.0 / DT) as u32 {
        game.update(DT, &Input::default());
        if !game.actors.list[0].dead {
            woke = true;
            break;
        }
    }
    assert!(woke, "spectre should rematerialise via A_Dormant");
}

/// Count view-area pixels that match the death-red palette index 4 colour.
fn count_death_red(fb: &Framebuffer, vw: usize, vh: usize) -> usize {
    let red = fizzle::death_red();
    let mut n = 0;
    for y in 0..vh.min(VIEW_H) {
        for x in 0..vw.min(WIDTH) {
            if fb.pixels[y * WIDTH + x] == red {
                n += 1;
            }
        }
    }
    n
}

/// Dying starts a FizzleFade over the 3D view; after the dissolve + hold the
/// floor restarts with a fresh loadout (WL_GAME.C `Died`).
#[test]
fn death_fizzle_dissolves_view_then_respawns() {
    let mut game = Game::new(0);
    game.god = true; // only we control the lethal hit
    game.lives = 2;
    let lives_before = game.lives;

    // Force the Died path.
    game.god = false;
    game.health = 0;
    game.update(DT, &Input::default());
    assert_eq!(game.screen, GameScreen::Death);
    assert!(game.died);
    assert_eq!(game.lives, lives_before - 1);

    let (vx, vy, vw, vh) = game.view_rect();
    assert_eq!((vx, vy), (0, 0));
    assert_eq!((vw, vh), (WIDTH, VIEW_H));

    // Early in the dissolve: some, but not all, view pixels are red.
    for _ in 0..20 {
        game.update(DT, &Input::default());
    }
    assert_eq!(game.screen, GameScreen::Death);
    let mut fb = Framebuffer::new();
    game.render(&mut fb);
    let mid = count_death_red(&fb, vw, vh);
    assert!(mid > 0, "fizzle should have painted some red pixels");
    assert!(
        mid < vw * vh,
        "fizzle should not have finished after 20 tics (painted {mid}/{})",
        vw * vh
    );
    // The dissolve only targets the 3D view; the status bar keeps its own art
    // (which may contain palette-red texels, so we only assert the fizzle size).
    assert_eq!(vh, VIEW_H);

    // Drive through the full LFSR period + the 100-tic solid-red hold.
    let max_tics = 131_071 / fizzle::FIZZLE_PIX_PER_FRAME + 100 + 20;
    let mut finished = false;
    for _ in 0..max_tics {
        game.update(DT, &Input::default());
        if game.screen == GameScreen::Playing {
            finished = true;
            break;
        }
    }
    assert!(finished, "death screen should end after fizzle + hold");
    assert_eq!(game.health, 100);
    assert_eq!(game.ammo, 8);
    assert_eq!(game.weapon, WEAPON_PISTOL);
    assert_eq!(game.lives, lives_before - 1);
}

/// Out of lives: after the fizzle + hold, route to the high-score check.
#[test]
fn death_fizzle_game_over_leaves_play() {
    let mut game = Game::new(0);
    game.lives = 0;
    game.health = 0;
    game.update(DT, &Input::default());
    assert_eq!(game.screen, GameScreen::Death);
    assert!(game.lives < 0);

    let max_tics = 131_071 / fizzle::FIZZLE_PIX_PER_FRAME + 100 + 20;
    for _ in 0..max_tics {
        game.update(DT, &Input::default());
        if game.screen != GameScreen::Death {
            break;
        }
    }
    assert_ne!(
        game.screen,
        GameScreen::Death,
        "game-over death must leave the Death screen"
    );
    assert_ne!(
        game.screen,
        GameScreen::Playing,
        "out of lives must not restart the floor"
    );
}

/// Opening a door links the floor areas on either side immediately (WL_ACT1.C
/// DoorOpening connects on the first open frame), and closing fully unlinks them.
#[test]
fn door_open_connects_areas_closed_disconnects() {
    let mut game = Game::new(0);
    game.god = true;
    // E1M1 has a horizontal door at (34,28) between areas 3 (north) and 1 (south).
    let door = game
        .world
        .doors
        .iter()
        .find(|d| d.x == 34 && d.y == 28)
        .expect("E1M1 door at (34,28)");
    assert!(!door.vertical);
    let near = (34, 27); // area 3
    let far = (34, 29); // area 1
    let near_a = game.world.area_at(near.0, near.1);
    let far_a = game.world.area_at(far.0, far.1);
    assert_ne!(
        near_a, far_a,
        "door should separate two areas ({near_a} vs {far_a})"
    );

    // Stand north of the door, facing south.
    game.player.x = near.0 as f32 + 0.5;
    game.player.y = near.1 as f32 + 0.5;
    game.player.angle = std::f32::consts::FRAC_PI_2; // face south
    game.world.refresh_areas(near.0, near.1);
    assert!(game.world.in_player_area(near.0, near.1));
    assert!(
        !game.world.in_player_area(far.0, far.1),
        "far area must start disconnected with the door closed"
    );

    game.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    // Areas connect the instant the door leaves Closed (not after a flood %).
    assert!(
        game.world.in_player_area(far.0, far.1),
        "opening a door should connect the far area to the player"
    );

    // Wait out hold (~4.3s) + close (~0.9s).
    for _ in 0..450 {
        game.update(DT, &Input::default());
    }
    assert!(
        game.world
            .doors
            .iter()
            .find(|d| d.x == 34 && d.y == 28)
            .is_some_and(|d| d.position == 0.0),
        "test door should have auto-closed"
    );
    assert!(
        !game.world.in_player_area(far.0, far.1),
        "fully closed door should disconnect the far area again"
    );
}

/// Tiles that share a floor area stay connected even with a closed door
/// between them (the original keys AI off area ids, not geometric flood).
#[test]
fn same_area_stays_connected_through_closed_door() {
    let mut game = Game::new(0);
    // E1M1 first door (32,57): both sides are floor area 2.
    game.world.refresh_areas(29, 57);
    assert_eq!(game.world.area_at(31, 57), game.world.area_at(33, 57));
    assert!(
        game.world.in_player_area(33, 57),
        "same-area tiles remain in areabyplayer with the door shut"
    );
}

/// UpdateFace cycles `faceframe` among 0/1/2 without advancing the gameplay RNG.
#[test]
fn update_face_glances_without_touching_gameplay_rng() {
    let mut game = Game::new(0);
    game.god = true;
    // Park away from anything that could fight or roll combat RNG.
    game.player.x = 29.5;
    game.player.y = 57.5;

    let rng_before = game.actors.rng_index();
    let snd_before = game.actors.snd_rng_index();
    let start_frame = game.faceframe;
    assert!(
        start_frame <= 2,
        "faceframe must be 0..=2, got {start_frame}"
    );

    let mut seen = [false; 3];
    seen[start_frame as usize] = true;
    let mut changed = false;
    // facecount grows until it exceeds a 0..=255 roll; worst case ~256 tics.
    for _ in 0..400 {
        game.update(DT, &Input::default());
        assert!(
            game.faceframe <= 2,
            "faceframe out of range: {}",
            game.faceframe
        );
        seen[game.faceframe as usize] = true;
        if game.faceframe != start_frame {
            changed = true;
        }
    }
    assert!(changed, "faceframe should glance away from the start frame");
    assert!(
        seen.iter().filter(|&&s| s).count() >= 2,
        "expected at least two distinct glance frames over time, saw {seen:?}"
    );
    assert_eq!(
        game.actors.rng_index(),
        rng_before,
        "UpdateFace must not advance the gameplay RNG"
    );
    assert_eq!(
        game.actors.snd_rng_index(),
        snd_before,
        "UpdateFace must not advance the sound RNG"
    );
}

/// Picking up the chaingun forces GOTGATLINGPIC until the next UpdateFace redraw.
#[test]
fn chaingun_pickup_forces_gatling_grin() {
    use wolf3d::raycast::{Bonus, StaticSprite};

    let mut game = Game::new(0);
    game.god = true;
    // Place a chaingun on the player tile and collect it.
    let px = game.player.x.floor() as i32;
    let py = game.player.y.floor() as i32;
    game.world.statics.push(StaticSprite {
        x: px as f32 + 0.5,
        y: py as f32 + 0.5,
        sprite: 0,
        bonus: Some(Bonus::ChainGun),
        picked: false,
    });
    // One tic of standing still: try_pickups runs inside update_play.
    game.update(DT, &Input::default());
    assert_eq!(
        game.bestweapon, WEAPON_CHAINGUN,
        "chaingun should be collected"
    );
    // update_face runs before pickups in the same tic, so the grin is set
    // after that tic's face roll; it should be visible at least once.
    assert!(
        game.got_gatling_face,
        "chaingun pickup should force GOTGATLINGPIC"
    );
    // Drive until UpdateFace clears the grin.
    let mut cleared = false;
    for _ in 0..400 {
        game.update(DT, &Input::default());
        if !game.got_gatling_face {
            cleared = true;
            break;
        }
    }
    assert!(
        cleared,
        "gatling grin should clear after UpdateFace redraws"
    );
    assert!(game.faceframe <= 2);
}

/// The 17-bit LFSR covers every pixel of a classic 320x160 view (unit-level
/// check of the generator used by Died).
#[test]
fn fizzle_lfsr_covers_view() {
    let mut f = Fizzle::new(WIDTH, VIEW_H);
    f.step_n(131_071);
    assert!(f.finished());
    for y in 0..VIEW_H {
        for x in 0..WIDTH {
            assert!(f.is_painted(x, y), "missing fizzle pixel {x},{y}");
        }
    }
    assert_eq!(fizzle::FIZZLE_PIX_PER_FRAME, 914);
    assert_eq!(fizzle::death_red(), wolf3d::assets::palette::PALETTE[4]);
}

//! Fidelity checks for original-game behavior: score extralives
//! (GivePoints / EXTRAPOINTS), bestweapon tracking, per-floor ceiling colours,
//! and SOD spectre rematerialisation (A_Dormant).

use wolf3d::actors::Kind;
use wolf3d::assets::{MapSet, data_dir};
use wolf3d::game::{
    Difficulty, EXTRAPOINTS, Game, Input, WEAPON_CHAINGUN, WEAPON_KNIFE, WEAPON_MACHINEGUN,
    WEAPON_PISTOL,
};
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

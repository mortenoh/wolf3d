//! Spear of Destiny (SOD) checks. Gated on the SOD data being present: a
//! WL6-only checkout (no `data/VSWAP.SOD`) skips every test with an eprintln so
//! the suite still passes. When the data is present these exercise the SOD map
//! set (21 floors), the divergent VSWAP/VGAGRAPH assets, the Spear boss cast,
//! and the spear -> Angel-of-Death -> victory flow.
//!
//! Deterministic like the other headless suites: fixed 1/70 s tics over the
//! original US_RndT table.

use wolf3d::actors::Kind;
use wolf3d::assets::{MapSet, VSwap, VgaGraph, data_dir};
use wolf3d::game::{Game, Input, WEAPON_CHAINGUN};
use wolf3d::variant::Variant;

const DT: f32 = 1.0 / 70.0;
const N: usize = 64;

/// True when the SOD data set is installed. When false, every test prints a
/// skip note and returns early (so WL6-only checkouts stay green).
fn sod_present() -> bool {
    if Variant::present("SOD") {
        return true;
    }
    eprintln!("skipping SOD test: data/VSWAP.SOD not present");
    false
}

fn sod_game(level_idx: usize) -> Game {
    Game::new_variant(level_idx, Variant::sod())
}

/// SOD is one 21-floor campaign.
#[test]
fn sod_has_21_levels() {
    if !sod_present() {
        return;
    }
    let maps = MapSet::load_ext(&data_dir(), "SOD").expect("GAMEMAPS.SOD");
    assert_eq!(maps.num_levels(), 21, "Spear of Destiny is 21 floors");
}

/// Every floor decodes, has a player start, and spawns at least one actor
/// without panicking (full-campaign smoke).
#[test]
fn sod_every_floor_loads_and_spawns() {
    if !sod_present() {
        return;
    }
    let maps = MapSet::load_ext(&data_dir(), "SOD").expect("GAMEMAPS.SOD");
    assert_eq!(maps.num_levels(), 21);
    for idx in 0..maps.num_levels() {
        // Game::new_variant runs find_spawn + spawn_from_level_variant + a full
        // stat scan; a missing player start or a bad decode would panic here.
        let game = sod_game(idx);
        assert!(
            !game.actors.list.is_empty(),
            "floor {idx} ({}) spawned no actors",
            game.world.level.name
        );
    }
}

/// VSWAP.SOD and VGAGRAPH.SOD sanity: the sprite bank is large enough to hold
/// the extra SOD statics + boss cast, the weapon frames decode, and the split
/// title picture halves have sane dimensions.
#[test]
fn sod_assets_decode() {
    if !sod_present() {
        return;
    }
    let vswap = VSwap::load_ext(&data_dir(), "SOD").expect("VSWAP.SOD");
    // SOD sprites run through SPR_CHAINATK4; the four extra statics push the
    // count above WL6's. The last 20 sprites are the player weapon frames.
    assert!(
        vswap.sprites.len() >= 380,
        "SOD sprite bank unexpectedly small: {}",
        vswap.sprites.len()
    );
    assert!(!vswap.walls.is_empty());

    let vga = VgaGraph::load_ext(&data_dir(), "SOD").expect("VGAGRAPH.SOD");
    // SOD title is TITLE1PIC(79) + TITLE2PIC(80), each a 320-wide half.
    let top = vga.pic(79);
    let bottom = vga.pic(80);
    assert_eq!(top.width, 320, "TITLE1PIC should be 320 wide");
    assert_eq!(bottom.width, 320, "TITLE2PIC should be 320 wide");
    assert_eq!(
        top.height + bottom.height,
        200,
        "the two title halves stack to 200 rows"
    );
    // STATUSBARPIC (SOD chunk 90) is the 320x40 HUD bar.
    let bar = vga.pic(90);
    assert_eq!((bar.width, bar.height), (320, 40));
}

/// The five Spear bosses spawn on their floors with the right actor kind.
/// (mapon is 0-based: Trans on floor 5 = idx 4, Wilhelm on 10 = idx 9,
/// UberMutant on 16 = idx 15, Death Knight on 18 = idx 17, Angel on 21 = idx 20.)
#[test]
fn sod_bosses_spawn_on_their_floors() {
    if !sod_present() {
        return;
    }
    let cases = [
        (4usize, Kind::Trans, 125u16),
        (9, Kind::Will, 143),
        (15, Kind::Uber, 142),
        (17, Kind::Death, 161),
        (20, Kind::Angel, 107),
    ];
    for (idx, kind, code) in cases {
        let game = sod_game(idx);
        assert!(
            game.world.level.plane1.contains(&code),
            "floor {idx} plane1 should hold spawn code {code} for {kind:?}"
        );
        let boss = game.actors.list.iter().find(|a| a.kind == kind);
        assert!(
            boss.is_some(),
            "floor {idx} ({}) should spawn a {kind:?}",
            game.world.level.name
        );
        assert!(!boss.unwrap().dead);
    }
}

/// The Spear of Destiny static (plane-1 code 74) exists, and grabbing it warps
/// the player to the final Angel-of-Death floor (mapon 20) and grants the gold
/// key (WL_GAME.C spearflag).
#[test]
fn sod_grabbing_the_spear_warps_to_the_angel_floor() {
    if !sod_present() {
        return;
    }
    // Find the floor that holds the spear static (code 74).
    let maps = MapSet::load_ext(&data_dir(), "SOD").expect("GAMEMAPS.SOD");
    let mut spear = None;
    for idx in 0..maps.num_levels() {
        let level = maps.level(idx);
        if let Some(pos) = level.plane1.iter().position(|&c| c == 74) {
            spear = Some((idx, pos));
            break;
        }
    }
    let (spear_floor, pos) = spear.expect("some SOD floor holds the spear (plane-1 code 74)");
    assert!(
        spear_floor != 20,
        "the spear should not be on the Angel floor itself"
    );

    let mut game = sod_game(spear_floor);
    // Stand the player on the spear tile so the per-frame pickup test fires.
    let (tx, ty) = (pos % N, pos / N);
    game.player.x = tx as f32 + 0.5;
    game.player.y = ty as f32 + 0.5;

    game.update(DT, &Input::default());

    assert_eq!(
        game.level_idx, 20,
        "grabbing the spear warps to the Angel floor (mapon 20)"
    );
    assert!(
        !game.spear_pending,
        "the spear warp should have been consumed"
    );
    assert_eq!(
        game.keys & wolf3d::hud::KEY_GOLD,
        wolf3d::hud::KEY_GOLD,
        "floor 20 always grants the gold key"
    );
    // The Angel is waiting on the floor we warped to.
    assert!(
        game.actors.list.iter().any(|a| a.kind == Kind::Angel),
        "the Angel of Death waits on floor 20"
    );
}

/// Killing the Angel of Death on the final floor sets the victory flag
/// (WL_ACT2.C A_Victory -> ex_victorious). Fought with god mode so the test
/// cannot lose to the Angel's rockets.
#[test]
fn sod_killing_the_angel_wins() {
    if !sod_present() {
        return;
    }
    let mut game = sod_game(20);
    game.god = true;
    game.infinite_ammo = true;
    game.weapon = WEAPON_CHAINGUN;
    game.ammo = 99;
    // Isolate the duel from the rest of the arena garrison.
    game.actors.list.retain(|a| a.kind == Kind::Angel);
    let (ax, ay) = game
        .actors
        .list
        .iter()
        .find(|a| a.kind == Kind::Angel)
        .map(|a| (a.x, a.y))
        .expect("an Angel actor");

    // Stand two tiles south of the Angel with a clear shot.
    game.player.x = ax;
    game.player.y = ay + 2.0;

    let mut won = false;
    for _ in 0..(90.0 / DT) as u32 {
        // Aim straight at the (possibly dodging) Angel each tic.
        if let Some((bx, by)) = game
            .actors
            .list
            .iter()
            .find(|a| a.kind == Kind::Angel && !a.dead)
            .map(|a| (a.x, a.y))
        {
            let (dx, dy) = (bx - game.player.x, by - game.player.y);
            game.player.angle = dy.atan2(dx);
        }
        game.ammo = 99;
        game.update(
            DT,
            &Input {
                fire: true,
                ..Default::default()
            },
        );
        if game.victory {
            won = true;
            break;
        }
    }
    assert!(
        won,
        "the Angel of Death should die and set the victory flag"
    );
}

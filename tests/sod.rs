//! Spear of Destiny (SOD) checks. Gated on the SOD data being present: a
//! WL6-only checkout (no `data/VSWAP.SOD`) skips every test with an eprintln so
//! the suite still passes. When the data is present these exercise the SOD map
//! set (21 floors), the divergent VSWAP/VGAGRAPH assets, the Spear boss cast,
//! and the spear -> Angel-of-Death -> victory flow.
//!
//! Deterministic like the other headless suites: fixed 1/70 s tics over the
//! original US_RndT table.

use std::collections::{HashSet, VecDeque};

use wolf3d::actors::Kind;
use wolf3d::assets::{MapSet, VSwap, VgaGraph, data_dir};
use wolf3d::fb::Framebuffer;
use wolf3d::game::{Game, GameScreen, Input, WEAPON_CHAINGUN};
use wolf3d::raycast::World;
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

/// The Spear menu is blue, not WL6's red: WL_MENU.H swaps the 0x2x red window
/// ramp for the 0x9x blue one, and ClearMScreen paints C_BACKDROPPIC over the
/// whole screen instead of a flat BORDCOLOR fill.
#[test]
fn sod_menu_is_blue() {
    if !sod_present() {
        return;
    }
    let blue_share = |mut game: Game| {
        game.to_title();
        game.update(
            DT,
            &Input {
                any_key: true,
                ..Default::default()
            },
        );
        assert_eq!(game.screen, GameScreen::MainMenu);
        let mut fb = Framebuffer::new();
        game.render(&mut fb);
        let blue = fb
            .pixels
            .iter()
            .filter(|&&p| (p >> 16) & 0xff > p & 0xff)
            .count();
        blue as f32 / fb.pixels.len() as f32
    };

    // Most of the Spear menu is blue; almost none of the WL6 one is (its reds
    // invert the comparison, leaving only the grey text and black band).
    let sod = blue_share(sod_game(0));
    let wl6 = blue_share(Game::new(0));
    assert!(
        sod > 0.8,
        "the Spear menu should be mostly blue, got {sod:.2}"
    );
    assert!(wl6 < 0.05, "the WL6 menu should stay red, got {wl6:.2}");

    // The backdrop is a real picture, not a flat fill: it has more than the
    // handful of distinct colors a bar-and-window screen would produce.
    let mut game = sod_game(0);
    game.to_title();
    game.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    let mut fb = Framebuffer::new();
    game.render(&mut fb);
    let distinct: std::collections::HashSet<u32> = fb.pixels.iter().copied().collect();
    assert!(
        distinct.len() > 16,
        "C_BACKDROPPIC should be a textured picture, got {} colors",
        distinct.len()
    );
}

/// Every Spear floor has to be walkable from the player's start.
///
/// Spear redefines part of WL_ACT1.C's `statinfo[]`, and index 40 (spawn code
/// 63) is the case that bites: WL6 has a solid floor object there, Spear a lamp
/// hanging from the ceiling. Treating it as solid walled off six floors — worst
/// of all the finale, where the one lamp tile outside the spawn nook sealed the
/// player into 3 tiles with the Angel of Death unreachable.
#[test]
fn sod_floors_are_not_walled_off_by_hanging_lamps() {
    if !sod_present() {
        return;
    }
    let maps = MapSet::load_ext(&data_dir(), "SOD").expect("GAMEMAPS.SOD");

    // Flood fill from the spawn using the engine's own movement rules. Doors
    // and secret push-walls read as passable — they start solid but the player
    // opens or pushes them, and the two Spear secret floors are push-wall
    // mazes. Only real walls and blocking statics stop the fill.
    let walkable_from_spawn = |idx: usize| -> usize {
        let level = maps.level(idx);
        let spawn = wolf3d::raycast::find_spawn(&level);
        let (plane0, plane1) = (level.plane0.clone(), level.plane1.clone());
        let world = World::new_variant(level, true);
        let passable = |x: i32, y: i32| {
            if !(0..64).contains(&x) || !(0..64).contains(&y) {
                return false;
            }
            let i = y as usize * 64 + x as usize;
            let door = (90..=101).contains(&plane0[i]);
            let pushwall = plane1[i] == 98;
            door || pushwall || !world.blocks_move(x, y)
        };

        let start = (spawn.x as i32, spawn.y as i32);
        let mut seen = HashSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some((x, y)) = queue.pop_front() {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let next = (x + dx, y + dy);
                if passable(next.0, next.1) && seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        seen.len()
    };

    // The Angel floor (mapon 20): the whole arena has to be open, not the
    // 3-tile pocket the lamp used to leave.
    let angel = walkable_from_spawn(20);
    assert!(
        angel > 2000,
        "the Angel of Death arena should be walkable, reached {angel} tiles"
    );

    // Tunnels 1 (mapon 0): ten lamps sit in its corridors.
    let tunnels1 = walkable_from_spawn(0);
    assert!(
        tunnels1 > 450,
        "Tunnels 1 should open up past its lamps, reached {tunnels1} tiles"
    );

    // No floor should strand the player in a handful of tiles.
    for idx in 0..maps.num_levels() {
        let n = walkable_from_spawn(idx);
        assert!(
            n > 50,
            "floor {idx} ({}) strands the player: {n} reachable tiles",
            maps.level(idx).name
        );
    }
}

/// The lamp is Spear-only: WL6's index 40 is a solid object and still blocks.
#[test]
fn wl6_keeps_its_solid_statinfo_40() {
    let maps = MapSet::load(&data_dir()).expect("GAMEMAPS.WL6");
    // Drop a code-63 static on the player's start tile, then ask each variant
    // whether it is solid there.
    let solid_for = |sod: bool| {
        let mut level = maps.level(0);
        let spawn = wolf3d::raycast::find_spawn(&level);
        let (sx, sy) = (spawn.x as i32, spawn.y as i32);
        level.plane1[sy as usize * 64 + sx as usize] = 63;
        World::new_variant(level, sod).blocks_move(sx, sy)
    };
    assert!(solid_for(false), "WL6 statinfo index 40 is solid");
    assert!(
        !solid_for(true),
        "Spear's index 40 is a hanging lamp and must not block"
    );
}

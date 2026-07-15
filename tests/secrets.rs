//! Secrets + intermission milestone checks on real .WL6 data (requires `data/`).
//!
//! (a) locate and activate a real secret push-wall, watch it slide two tiles,
//!     leave passable floor, bump the secret counter, and render offset;
//! (b) the intermission scoring math against the WL_INTER.C formulas;
//! (c) the elevator-through-intermission flow lives in tests/progression.rs.

use wolf3d::game::{Game, Input};
use wolf3d::inter::{
    LevelStats, PAR_AMOUNT, PERCENT100AMT, compute_bonus, par_seconds, par_string,
};

const DT: f32 = 1.0 / 70.0;
const N: usize = 64; // MAP_SIZE

fn is_wall(t: u16) -> bool {
    (1..=89).contains(&t)
}
fn walkable(t: u16) -> bool {
    !(1..=101).contains(&t)
}

fn hold(game: &mut Game, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, &Input::default());
    }
}

/// A located push-wall: which floor, the wall tile, the push direction, and the
/// tile to stand on facing it.
struct PushSetup {
    level: usize,
    wx: i32,
    wy: i32,
    dx: i32,
    dy: i32,
    stand_x: f32,
    stand_y: f32,
    angle: f32,
}

/// Scan every floor for a plane-1 PUSHABLETILE (98) on a wall that has a walkable
/// tile to stand on and two empty tiles behind it (so the full two-tile slide can
/// complete). Returns the first such setup found.
fn find_pushwall(game: &Game) -> PushSetup {
    use std::f32::consts::{FRAC_PI_2, PI};
    let dirs = [
        (1i32, 0i32, 0.0f32), // east
        (-1, 0, PI),          // west
        (0, 1, FRAC_PI_2),    // south
        (0, -1, -FRAC_PI_2),  // north
    ];
    for level in 0..game.maps.num_levels() {
        let lvl = game.maps.level(level);
        let at = |x: i32, y: i32| -> Option<u16> {
            (x >= 0 && y >= 0 && x < N as i32 && y < N as i32)
                .then(|| lvl.plane0[y as usize * N + x as usize])
        };
        for y in 1..N as i32 - 2 {
            for x in 1..N as i32 - 2 {
                let i = y as usize * N + x as usize;
                if lvl.plane1[i] != 98 || !is_wall(lvl.plane0[i]) {
                    continue;
                }
                for (dx, dy, angle) in dirs {
                    let stand = at(x - dx, y - dy);
                    let ahead1 = at(x + dx, y + dy);
                    let ahead2 = at(x + 2 * dx, y + 2 * dy);
                    if let (Some(s), Some(a1), Some(a2)) = (stand, ahead1, ahead2)
                        && walkable(s)
                        && walkable(a1)
                        && walkable(a2)
                    {
                        return PushSetup {
                            level,
                            wx: x,
                            wy: y,
                            dx,
                            dy,
                            stand_x: (x - dx) as f32 + 0.5,
                            stand_y: (y - dy) as f32 + 0.5,
                            angle,
                        };
                    }
                }
            }
        }
    }
    panic!("no usable push-wall found in any WL6 floor");
}

/// (a) A real secret push-wall: using it starts a two-tile slide, bumps the
/// secret counter, renders offset mid-slide, and leaves passable floor behind.
#[test]
fn pushwall_slides_two_tiles_and_reveals_secret() {
    let probe = Game::new(0);
    let s = find_pushwall(&probe);
    drop(probe);

    let mut game = Game::new(s.level);
    game.actors.list.clear(); // deterministic: nothing can block the slide
    game.player.x = s.stand_x;
    game.player.y = s.stand_y;
    game.player.angle = s.angle;

    let secrets_before = game.stats.secrets;
    assert!(is_wall(
        game.world.level.plane0[s.wy as usize * N + s.wx as usize]
    ));

    // Use the wall: it activates and the secret counter ticks.
    game.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    assert!(
        game.world.pushwall.is_some(),
        "the push-wall started moving"
    );
    assert_eq!(
        game.stats.secrets,
        secrets_before + 1,
        "secret counter increments on activation"
    );

    // Mid-slide: the wall renders offset (asserted via world state, not pixels).
    hold(&mut game, 0.5);
    let offset = game.world.pushwall.as_ref().map(|p| p.offset());
    assert!(
        matches!(offset, Some(o) if o > 0.0),
        "the sliding wall carries a non-zero render offset (got {offset:?})"
    );

    // Let the whole slide finish (two tiles at 128 tics each, ~3.7s).
    hold(&mut game, 5.0);
    assert!(
        game.world.pushwall.is_none(),
        "the push-wall stops after two tiles"
    );

    // The two tiles it vacated are now passable; the tile it moved to is solid.
    let vacated0 = game.world.level.plane0[s.wy as usize * N + s.wx as usize];
    let vacated1 = game.world.level.plane0[(s.wy + s.dy) as usize * N + (s.wx + s.dx) as usize];
    let landed =
        game.world.level.plane0[(s.wy + 2 * s.dy) as usize * N + (s.wx + 2 * s.dx) as usize];
    assert!(
        walkable(vacated0),
        "the origin tile opened into passable floor"
    );
    assert!(
        walkable(vacated1),
        "the second tile opened into passable floor"
    );
    assert!(is_wall(landed), "the wall now rests two tiles back");
}

/// (b) The intermission scoring matches the WL_INTER.C LevelCompleted formulas:
/// ratios are count*100/total (0 when the total is 0), the time bonus is
/// PAR_AMOUNT per second under par, and each perfect (100%) ratio adds
/// PERCENT100AMT.
#[test]
fn intermission_bonus_matches_wl_inter_formulas() {
    // A synthetic floor: perfect kills, half the secrets, three of four treasures.
    let stats = LevelStats {
        kills: 10,
        kill_total: 10,
        secrets: 1,
        secret_total: 2,
        treasure: 3,
        treasure_total: 4,
        time: 0.0,
    };
    assert_eq!(stats.kill_ratio(), 100);
    assert_eq!(stats.secret_ratio(), 50);
    assert_eq!(stats.treasure_ratio(), 75);

    // Under par (60s on a 120s par): timeleft = 60, time bonus = 60*500 = 30000,
    // plus one perfect ratio (kills) = +10000. Total 40000.
    let (timeleft, bonus) = compute_bonus(60, 120, 100, 50, 75);
    assert_eq!(timeleft, 60);
    assert_eq!(bonus, 60 * PAR_AMOUNT + PERCENT100AMT);
    assert_eq!(bonus, 40000);

    // All three ratios perfect, well under par.
    let (tl, b) = compute_bonus(30, 120, 100, 100, 100);
    assert_eq!(tl, 90);
    assert_eq!(b, 90 * PAR_AMOUNT + 3 * PERCENT100AMT);

    // At/over par yields no time bonus; only perfect-ratio bonuses remain.
    assert_eq!(compute_bonus(200, 120, 100, 0, 0), (0, PERCENT100AMT));
    assert_eq!(compute_bonus(120, 120, 0, 0, 0), (0, 0));

    // A level with no par (boss/secret floor) never awards a time bonus.
    assert_eq!(compute_bonus(10, 0, 50, 50, 50), (0, 0));

    // A zero total reads as 0%, not 100% (WL_INTER.C guards each total).
    let empty = LevelStats {
        kill_total: 0,
        ..Default::default()
    };
    assert_eq!(empty.kill_ratio(), 0);
    assert_eq!(empty.treasure_ratio(), 0);

    // Par-time table spot checks (WL_GAME.C parTimes: E1M1 1:30, E1 boss none).
    assert_eq!(par_seconds(0), 90);
    assert_eq!(par_string(0), "01:30");
    assert_eq!(par_seconds(8), 0);
    assert_eq!(par_string(8), "??:??");
}

// --- Snapshot generation (ignored; run explicitly to eyeball the screens) ---

fn find_elevator(game: &Game) -> (f32, f32, f32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let lvl = game.maps.level(game.level_idx);
    let dirs = [
        (1i32, 0i32, PI),
        (-1, 0, 0.0),
        (0, 1, -FRAC_PI_2),
        (0, -1, FRAC_PI_2),
    ];
    for y in 1..N as i32 - 1 {
        for x in 1..N as i32 - 1 {
            if lvl.plane0[y as usize * N + x as usize] != 21 {
                continue;
            }
            for (dx, dy, angle) in dirs {
                let (nx, ny) = (x + dx, y + dy);
                if walkable(lvl.plane0[ny as usize * N + nx as usize]) {
                    return (nx as f32 + 0.5, ny as f32 + 0.5, angle);
                }
            }
        }
    }
    panic!("no elevator with a walkable neighbor");
}

fn write_ppm(fb: &wolf3d::fb::Framebuffer, path: &str) {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    write!(f, "P6\n{} {}\n255\n", wolf3d::fb::WIDTH, wolf3d::fb::HEIGHT).unwrap();
    for &px in &fb.pixels {
        f.write_all(&px.to_le_bytes()[..3]).unwrap();
    }
}

#[test]
#[ignore]
fn generate_snapshots() {
    let out = std::env::var("WOLF3D_SNAP_DIR").unwrap_or_else(|_| "snaps".into());
    std::fs::create_dir_all(&out).unwrap();
    let mut fb = wolf3d::fb::Framebuffer::new();

    // 1. A secret push-wall mid-slide in the 3D view.
    let mut g = Game::new(0);
    let s = find_pushwall(&g);
    g.actors.list.clear();
    g.player.x = s.stand_x;
    g.player.y = s.stand_y;
    g.player.angle = s.angle;
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/secret_before.ppm"));
    g.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    hold(&mut g, 0.9); // ~half a tile in
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/pushwall_midslide.ppm"));
    hold(&mut g, 5.0);
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/secret_revealed.ppm"));

    // 2. The intermission with counting percentages.
    let mut g = Game::new(0);
    let (ex, ey, angle) = find_elevator(&g);
    g.actors.list.clear();
    g.player.x = ex;
    g.player.y = ey;
    g.player.angle = angle;
    // Seed a lively scoreboard: perfect kills, all secrets, some treasure, quick run.
    g.stats.kills = g.stats.kill_total;
    g.stats.secrets = g.stats.secret_total;
    g.stats.treasure = (g.stats.treasure_total * 3 / 4).max(0);
    g.stats.time = 42.0;
    g.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    assert_eq!(g.screen, wolf3d::game::GameScreen::Intermission);
    hold(&mut g, 0.6);
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/intermission_counting.ppm"));
    hold(&mut g, 3.0);
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/intermission_final.ppm"));

    // 3. The "Get Psyched!" load screen (force it on; demos skip it).
    g.show_load_screen = true;
    g.update(
        DT,
        &Input {
            any_key: true,
            ..Default::default()
        },
    );
    assert_eq!(g.screen, wolf3d::game::GameScreen::GetPsyched);
    g.render(&mut fb);
    write_ppm(&fb, &format!("{out}/get_psyched.ppm"));

    eprintln!("wrote snapshots to {out}");
}

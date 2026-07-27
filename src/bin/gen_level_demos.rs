//! Generate one attract demo per Wolfenstein 3D floor that completes the level
//! (spawn -> elevator, with doors and keys handled).
//!
//! Usage:
//!   cargo run --release --bin gen_level_demos
//!   cargo run --release --bin gen_level_demos -- --levels 0,1,8
//!   cargo run --release --bin gen_level_demos -- --out demos
//!
//! Each demo is written as `e{episode}m{floor}.dm` (1-based episode/floor).
//! Runs in god mode with both keys so locked doors and combat cannot strand the
//! bot. Existing specialty demos (`e1m1_fight.dm`, etc.) are left alone.

use std::collections::{HashSet, VecDeque};
use std::env;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use wolf3d::demorec::Demo;
use wolf3d::game::{Game, GameScreen, Input, TIC, WEAPON_CHAINGUN};
use wolf3d::hud::{KEY_GOLD, KEY_SILVER};

const MAP: usize = 64;
const DT: f32 = TIC;
/// Max tics to spend walking toward one tile center.
const WALK_TICS: u32 = 500;
/// Max tics waiting for a door to open after use.
const DOOR_WAIT_TICS: u32 = 100;
/// Max total tics for one level's recording (safety).
const MAX_LEVEL_TICS: u32 = 70 * 180; // 3 minutes

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let out = arg_value(&args, "--out").unwrap_or_else(|| "demos".into());
    let levels = parse_levels(&args);
    let out_dir = PathBuf::from(&out);
    std::fs::create_dir_all(&out_dir).expect("create demos dir");

    println!(
        "gen_level_demos: {} levels -> {}",
        levels.len(),
        out_dir.display()
    );

    let mut ok = 0usize;
    let mut fail = 0usize;
    for level_idx in levels {
        match generate_one(level_idx, &out_dir) {
            Ok((path, tics, method)) => {
                ok += 1;
                println!(
                    "  ok  {}  ({tics} tics, {method})",
                    path.file_name().unwrap().to_string_lossy()
                );
            }
            Err(e) => {
                fail += 1;
                eprintln!("  FAIL level {level_idx}: {e}");
            }
        }
    }
    println!("done: {ok} ok, {fail} failed");
    if fail > 0 {
        std::process::exit(1);
    }
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone())
}

fn parse_levels(args: &[String]) -> Vec<usize> {
    if let Some(spec) = arg_value(args, "--levels") {
        return spec
            .split(',')
            .map(|s| s.trim().parse().expect("level index"))
            .collect();
    }
    // Default: every registered WL6 floor.
    let probe = Game::new(0);
    let n = probe.maps.num_levels();
    (0..n).collect()
}

fn level_name(level_idx: usize) -> String {
    let ep = level_idx / 10 + 1;
    let fl = level_idx % 10 + 1;
    format!("e{ep}m{fl}")
}

fn generate_one(
    level_idx: usize,
    out_dir: &Path,
) -> Result<(PathBuf, usize, &'static str), String> {
    // Game::new loads the requested floor at hard skill (all spawns present).
    let mut game = prep_game(level_idx);
    let elevators = find_elevators(&game);
    let (demo, method) = if elevators.is_empty() {
        // Boss floors (no ELEVATORTILE): kill the end boss.
        record_boss_clear(&mut game)?
    } else {
        record_elevator_clear(&mut game, &elevators)?
    };

    let path_out = out_dir.join(format!("{}.dm", level_name(level_idx)));
    demo.write_to(&path_out)
        .map_err(|e| format!("write {}: {e}", path_out.display()))?;
    Ok((path_out, demo.tics.len(), method))
}

fn prep_game(level_idx: usize) -> Game {
    let mut game = Game::new(level_idx);
    game.god = true;
    game.infinite_ammo = true;
    game.keys = KEY_GOLD | KEY_SILVER;
    game.ammo = 99;
    game.weapon = WEAPON_CHAINGUN;
    game.bestweapon = WEAPON_CHAINGUN;
    game.started = true;
    game.screen = GameScreen::Playing;
    game
}

/// Walk (or start at) the elevator and use the switch to finish the floor.
fn record_elevator_clear(
    game: &mut Game,
    elevators: &[(i32, i32)],
) -> Result<(Demo, &'static str), String> {
    // Clear grunts so they cannot block doorways.
    game.actors.list.clear();

    let spawn = (game.player.x.floor() as i32, game.player.y.floor() as i32);
    type PathElev = (Vec<(i32, i32)>, (i32, i32));
    let mut best: Option<PathElev> = None;
    for &(ex, ey) in elevators {
        for stand in neighbors(ex, ey) {
            if !walkable(game, stand.0, stand.1) {
                continue;
            }
            if let Some(path) = bfs_path(game, spawn, stand) {
                let better = best
                    .as_ref()
                    .map(|(p, _)| path.len() < p.len())
                    .unwrap_or(true);
                if better {
                    best = Some((path, (ex, ey)));
                }
            }
        }
    }

    // Prefer a full spawn→elevator walk; if that fails mid-route, fall back to
    // a short elevator-exit demo (still a valid complete-floor recording).
    if let Some((path, elev)) = best {
        match try_elevator_path(game, &path, elev) {
            Ok(demo) => return Ok((demo, "pathfind")),
            Err(e) => {
                eprintln!("    pathfind aborted ({e}); falling back to elevator-start");
            }
        }
    }

    // Secret-gated / jammed paths: start already facing the switch.
    let (ex, ey) = elevators[0];
    let stand = neighbors(ex, ey)
        .into_iter()
        .find(|&(x, y)| walkable(game, x, y))
        .ok_or("no walkable neighbor beside elevator")?;
    // Fresh sim at the elevator so the header matches the short exit run.
    *game = prep_game(game.level_idx);
    game.actors.list.clear();
    place_facing(game, ex, ey, stand);
    let demo = try_elevator_path(game, &[stand], (ex, ey))?;
    Ok((demo, "elevator-start"))
}

/// Follow `path` then use the elevator at `elev`. Caller must have prepared
/// loadout / god / keys; this captures `Demo::begin` at the current pose.
fn try_elevator_path(
    game: &mut Game,
    path: &[(i32, i32)],
    elev: (i32, i32),
) -> Result<Demo, String> {
    let mut demo = Demo::begin(game);
    let mut tics = 0u32;

    for &(tx, ty) in path {
        if is_door_tile(game, tx, ty) {
            open_door(game, &mut demo, tx, ty, &mut tics)?;
        }
        for (nx, ny) in neighbors(tx, ty) {
            if is_door_tile(game, nx, ny) {
                let _ = open_door(game, &mut demo, nx, ny, &mut tics);
            }
        }
        walk_to(game, &mut demo, tx as f32 + 0.5, ty as f32 + 0.5, &mut tics)?;
        if game.screen != GameScreen::Playing {
            break;
        }
        if tics > MAX_LEVEL_TICS {
            return Err(format!("timeout after {tics} tics mid-path"));
        }
    }

    if game.screen == GameScreen::Playing {
        face_tile(game, &mut demo, elev.0, elev.1, &mut tics)?;
        for _ in 0..10 {
            push(
                game,
                &mut demo,
                Input {
                    use_door: true,
                    ..Default::default()
                },
                &mut tics,
            );
            hold(game, &mut demo, Input::default(), 14, &mut tics);
            if game.screen != GameScreen::Playing {
                break;
            }
        }
    }

    if game.screen == GameScreen::Playing {
        return Err(format!(
            "still Playing (elevator at {:?}, player {:.1},{:.1})",
            elev, game.player.x, game.player.y
        ));
    }

    hold(game, &mut demo, Input::default(), 5, &mut tics);
    Ok(demo)
}

/// Episode boss floors have no elevator: kill the end boss (and mecha morph).
fn record_boss_clear(game: &mut Game) -> Result<(Demo, &'static str), String> {
    // Keep the end boss only (drop fakes/trash so the fight is deterministic).
    game.actors.list.retain(|a| {
        matches!(
            a.kind,
            wolf3d::actors::Kind::Hans
                | wolf3d::actors::Kind::Schabbs
                | wolf3d::actors::Kind::MechaHitler
                | wolf3d::actors::Kind::Hitler
                | wolf3d::actors::Kind::Gift
                | wolf3d::actors::Kind::Gretel
                | wolf3d::actors::Kind::Fat
        )
    });
    if game.actors.list.is_empty() {
        return Err("boss floor with no boss actors".into());
    }

    // Prefer mecha over morph (Hitler may not exist yet).
    let boss_idx = game
        .actors
        .list
        .iter()
        .position(|a| a.kind == wolf3d::actors::Kind::MechaHitler)
        .unwrap_or(0);
    let (bx, by) = (game.actors.list[boss_idx].x, game.actors.list[boss_idx].y);
    let spawn = (game.player.x.floor() as i32, game.player.y.floor() as i32);
    let goal = (bx.floor() as i32, by.floor() as i32);
    // Stand a couple tiles off so we are not inside the boss.
    let stand = neighbors(goal.0, goal.1)
        .into_iter()
        .chain(
            neighbors(goal.0, goal.1)
                .into_iter()
                .flat_map(|(x, y)| neighbors(x, y)),
        )
        .find(|&(x, y)| walkable(game, x, y) && (x - goal.0).abs() + (y - goal.1).abs() >= 2)
        .unwrap_or(spawn);

    let path = bfs_path(game, spawn, stand).unwrap_or_else(|| {
        place_facing(game, goal.0, goal.1, stand);
        vec![stand]
    });
    let method = if path.len() > 1 {
        "boss-pathfind"
    } else {
        "boss-start"
    };

    let mut demo = Demo::begin(game);
    let mut tics = 0u32;

    for &(tx, ty) in &path {
        if is_door_tile(game, tx, ty) {
            let _ = open_door(game, &mut demo, tx, ty, &mut tics);
        }
        let _ = walk_to(game, &mut demo, tx as f32 + 0.5, ty as f32 + 0.5, &mut tics);
        if game.screen != GameScreen::Playing {
            break;
        }
    }

    // Aimfire until the floor ends (deathcam / victory). God mode keeps us up.
    let fire_budget = 70 * 180; // 3 minutes — mecha + Hitler can take a while
    for _ in 0..fire_budget {
        if game.screen != GameScreen::Playing {
            break;
        }
        game.ammo = 99;
        let (px, py) = (game.player.x, game.player.y);
        // Prefer live end-bosses over anything else.
        let target = game
            .actors
            .list
            .iter()
            .filter(|a| !a.dead)
            .map(|a| {
                let pri = match a.kind {
                    wolf3d::actors::Kind::Hitler | wolf3d::actors::Kind::MechaHitler => 0,
                    _ => 1,
                };
                let d = (a.x - px).powi(2) + (a.y - py).powi(2);
                (pri, d, a.x, a.y)
            })
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let turn_delta = target.map_or(0.0, |(_, _, bx, by)| {
            norm_angle((by - py).atan2(bx - px) - game.player.angle)
        });
        let dist = target.map(|(_, d, _, _)| d.sqrt()).unwrap_or(99.0);
        // Circle-strafe at medium range for consistent hits.
        push(
            game,
            &mut demo,
            Input {
                fire: target.is_some() && turn_delta.abs() < 0.4,
                turn_delta: turn_delta.clamp(-0.6, 0.6),
                back: dist < 3.0,
                forward: dist > 7.0,
                run: true,
                strafe_left: (tics / 18).is_multiple_of(2),
                strafe_right: !(tics / 18).is_multiple_of(2),
                ..Default::default()
            },
            &mut tics,
        );
        if tics > MAX_LEVEL_TICS * 2 {
            break;
        }
    }

    if game.screen == GameScreen::Playing {
        return Err(format!(
            "boss still alive after {tics} tics (screen=Playing, live={})",
            game.actors.list.iter().filter(|a| !a.dead).count()
        ));
    }

    hold(game, &mut demo, Input::default(), 5, &mut tics);
    Ok((demo, method))
}

// --- world queries ----------------------------------------------------------

fn plane0(game: &Game, x: i32, y: i32) -> u16 {
    if x < 0 || y < 0 || x >= MAP as i32 || y >= MAP as i32 {
        return 1;
    }
    game.world.level.plane0[y as usize * MAP + x as usize]
}

fn walkable(game: &Game, x: i32, y: i32) -> bool {
    let t = plane0(game, x, y);
    // Floor / open area tiles; doors are passable once open (we treat them as
    // path-nodes and open them before stepping on).
    !(1..=89).contains(&t)
}

fn is_door_tile(game: &Game, x: i32, y: i32) -> bool {
    (90..=101).contains(&plane0(game, x, y))
}

fn find_elevators(game: &Game) -> Vec<(i32, i32)> {
    let mut v = Vec::new();
    for y in 0..MAP as i32 {
        for x in 0..MAP as i32 {
            if plane0(game, x, y) == 21 {
                v.push((x, y));
            }
        }
    }
    v
}

fn neighbors(x: i32, y: i32) -> Vec<(i32, i32)> {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .map(|(dx, dy)| (x + dx, y + dy))
        .filter(|&(nx, ny)| nx >= 0 && ny >= 0 && nx < MAP as i32 && ny < MAP as i32)
        .collect()
}

fn bfs_path(game: &Game, start: (i32, i32), goal: (i32, i32)) -> Option<Vec<(i32, i32)>> {
    if start == goal {
        return Some(vec![start]);
    }
    let mut q = VecDeque::new();
    let mut prev: Vec<Option<(i32, i32)>> = vec![None; MAP * MAP];
    let mut seen = HashSet::new();
    q.push_back(start);
    seen.insert(start);
    while let Some(cur) = q.pop_front() {
        for n in neighbors(cur.0, cur.1) {
            if seen.contains(&n) || !walkable(game, n.0, n.1) {
                continue;
            }
            seen.insert(n);
            prev[n.1 as usize * MAP + n.0 as usize] = Some(cur);
            if n == goal {
                // Reconstruct path start..goal.
                let mut path = vec![goal];
                let mut c = goal;
                while c != start {
                    c = prev[c.1 as usize * MAP + c.0 as usize].unwrap();
                    path.push(c);
                }
                path.reverse();
                return Some(path);
            }
            q.push_back(n);
        }
    }
    None
}

fn place_facing(game: &mut Game, ex: i32, ey: i32, stand: (i32, i32)) {
    game.player.x = stand.0 as f32 + 0.5;
    game.player.y = stand.1 as f32 + 0.5;
    game.player.angle = (ey as f32 + 0.5 - game.player.y).atan2(ex as f32 + 0.5 - game.player.x);
}

// --- recording helpers ------------------------------------------------------

fn push(game: &mut Game, demo: &mut Demo, input: Input, tics: &mut u32) {
    demo.push(&input);
    game.update(DT, &input);
    *tics += 1;
}

fn hold(game: &mut Game, demo: &mut Demo, input: Input, n: u32, tics: &mut u32) {
    for _ in 0..n {
        push(game, demo, input, tics);
        if game.screen != GameScreen::Playing {
            break;
        }
    }
}

fn norm_angle(mut a: f32) -> f32 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    a
}

fn walk_to(
    game: &mut Game,
    demo: &mut Demo,
    tx: f32,
    ty: f32,
    tics: &mut u32,
) -> Result<(), String> {
    let mut stuck = 0u32;
    let mut last = (game.player.x, game.player.y);
    for step in 0..WALK_TICS {
        if game.screen != GameScreen::Playing {
            return Ok(());
        }
        let dx = tx - game.player.x;
        let dy = ty - game.player.y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 0.28 {
            return Ok(());
        }
        let want = dy.atan2(dx);
        let turn = norm_angle(want - game.player.angle);
        let turn_delta = turn.clamp(-0.5, 0.5);
        let aligned = turn.abs() < 0.45;
        // If we're jammed against a corner, try strafe-alternating.
        let strafe_left = stuck > 12 && step % 2 == 0;
        let strafe_right = stuck > 12 && step % 2 == 1;
        push(
            game,
            demo,
            Input {
                forward: aligned && stuck < 40,
                run: true,
                turn_delta,
                strafe_left,
                strafe_right,
                ..Default::default()
            },
            tics,
        );
        let moved = (game.player.x - last.0).abs() + (game.player.y - last.1).abs();
        if moved < 0.002 {
            stuck += 1;
        } else {
            stuck = 0;
            last = (game.player.x, game.player.y);
        }
        // Nudge: if nearly at the tile, accept and continue.
        if stuck > 50 && dist < 0.85 {
            return Ok(());
        }
        if *tics > MAX_LEVEL_TICS {
            return Err("walk_to timeout".into());
        }
    }
    let dx = tx - game.player.x;
    let dy = ty - game.player.y;
    let dist = (dx * dx + dy * dy).sqrt();
    // Soft-fail near misses so a jammed corner does not abort a whole floor.
    if dist > 1.6 {
        return Err(format!(
            "failed to reach ({tx:.1},{ty:.1}); at ({:.1},{:.1})",
            game.player.x, game.player.y
        ));
    }
    Ok(())
}

fn face_tile(
    game: &mut Game,
    demo: &mut Demo,
    tx: i32,
    ty: i32,
    tics: &mut u32,
) -> Result<(), String> {
    let (gx, gy) = (tx as f32 + 0.5, ty as f32 + 0.5);
    for _ in 0..90 {
        let want = (gy - game.player.y).atan2(gx - game.player.x);
        let turn = norm_angle(want - game.player.angle);
        if turn.abs() < 0.08 {
            return Ok(());
        }
        push(
            game,
            demo,
            Input {
                turn_delta: turn.clamp(-0.35, 0.35),
                ..Default::default()
            },
            tics,
        );
    }
    Ok(())
}

fn open_door(
    game: &mut Game,
    demo: &mut Demo,
    dx: i32,
    dy: i32,
    tics: &mut u32,
) -> Result<(), String> {
    // Stand still and face the door tile, then use.
    face_tile(game, demo, dx, dy, tics)?;
    push(
        game,
        demo,
        Input {
            use_door: true,
            ..Default::default()
        },
        tics,
    );
    // Wait for the door to slide open.
    for _ in 0..DOOR_WAIT_TICS {
        push(game, demo, Input::default(), tics);
        if let Some(d) = game.world.doors.iter().find(|d| d.x == dx && d.y == dy) {
            if d.position >= 0.9 {
                return Ok(());
            }
        } else {
            // Not a registered door (shouldn't happen for 90..=101).
            return Ok(());
        }
        if game.screen != GameScreen::Playing {
            return Ok(());
        }
    }
    Ok(()) // continue anyway; walk may still work if door opened enough
}

//! Headless scripted playthroughs, for verifying the game plays without a
//! window (same idea as rust_demoscene's DEMOSCENE_HEADLESS). The script in
//! `WOLF3D_DEMO` runs the real `Game::update`/`render` at a fixed 70 Hz
//! timestep and dumps PPM snapshots, so gameplay is checkable from a
//! terminal or CI.
//!
//! Script: semicolon-separated commands.
//!   w:1.5 / s:… / a:… / d:…  hold that movement key for the given seconds
//!   W:… / S:… / A:… / D:…    same, at a run (shift held)
//!   l:90 / r:90              turn left/right by degrees (at turn speed)
//!   use                      tap the use key (open/close a door)
//!   fire                     tap fire for one tic
//!   firehold:secs            hold fire for the given seconds
//!   weapon:n                 select weapon n (0 knife .. 3 chaingun)
//!   key:up|down|enter|esc|any  drive the menu (title/main/episode/difficulty)
//!   type:hello               type text into the save-name field (one char/tic)
//!   backspace                delete a character from the save-name field
//!   wait:1.0                 let time pass (doors keep animating)
//!   stats                    print health/ammo/score/lives/keys/weapon
//!   sounds                   print every sound event emitted so far (name+id)
//!   snap:name                write <out>/name.ppm
//!
//! Sound events are drained from the game each step. `WOLF3D_LOG_SOUNDS=1` also
//! prints each event as it fires, so tests can assert that a combat action made
//! the right noise without any audio device.
//! Debug-only commands (for scripting fights that would otherwise need a
//! pixel-perfect human; not reachable from normal play):
//!   teleport:x,y,deg         place the player at (x, y) facing deg degrees
//!   face:deg                 snap the facing to deg degrees (aimfire leaves
//!                            the aim wherever the last target stood)
//!   godmode                  the player stops taking damage
//!   aimfire:secs             hold fire while auto-aiming at the nearest
//!                            live enemy each tic (scripts cannot track a
//!                            dodging boss with coarse turn commands)
//!   victory                  print the victory flag
//! Attract-demo recording (see src/demorec.rs):
//!   record:path              start recording every subsequent tic's input to
//!                            a demo file at `path` (header captures the
//!                            current level/rng/camera/loadout, so do any
//!                            teleport/face/godmode setup BEFORE this)
//!   endrecord                stop recording and write the file
//! Output dir comes from WOLF3D_SNAP_DIR (default "snaps").

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::demorec::Demo;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::game::{Game, Input, TURN_SPEED};

const DT: f32 = 1.0 / 70.0; // the original's tic rate

/// An in-progress `record:` capture: the output path and the demo being built.
type Recording = Option<(PathBuf, Demo)>;

pub fn run(game: &mut Game, script: &str) {
    let out = std::env::var("WOLF3D_SNAP_DIR").unwrap_or_else(|_| "snaps".into());
    std::fs::create_dir_all(&out).expect("create snapshot dir");
    let mut fb = Framebuffer::new();

    // Sound events are drained after each command so nothing accumulates unseen.
    let log_sounds = std::env::var("WOLF3D_LOG_SOUNDS").as_deref() == Ok("1");
    let mut sound_log: Vec<u8> = Vec::new();
    let mut rec: Recording = None;

    for cmd in script.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let (op, arg) = cmd.split_once(':').unwrap_or((cmd, ""));
        let secs = |what: &str| -> f32 {
            arg.parse()
                .unwrap_or_else(|_| panic!("bad {what} argument in {cmd:?}"))
        };
        match op {
            "w" | "s" | "a" | "d" | "W" | "S" | "A" | "D" => {
                let key = op.to_ascii_lowercase();
                let input = Input {
                    forward: key == "w",
                    back: key == "s",
                    strafe_left: key == "a",
                    strafe_right: key == "d",
                    run: op != key, // uppercase = shift held
                    ..Default::default()
                };
                step(game, &input, secs("hold"), &mut rec);
            }
            "l" | "r" => {
                let input = Input {
                    turn_left: op == "l",
                    turn_right: op == "r",
                    ..Default::default()
                };
                step(game, &input, secs("degrees").to_radians() / TURN_SPEED, &mut rec);
            }
            "use" => {
                tick(game, &Input { use_door: true, ..Default::default() }, &mut rec);
            }
            "fire" => {
                tick(game, &Input { fire: true, ..Default::default() }, &mut rec);
            }
            "firehold" => {
                step(game, &Input { fire: true, ..Default::default() }, secs("firehold"), &mut rec)
            }
            "weapon" => {
                let w = arg.parse::<u8>().expect("weapon wants 0..=3");
                tick(game, &Input { select_weapon: Some(w), ..Default::default() }, &mut rec);
            }
            "key" => {
                // Drive the menu flow headlessly: key:up|down|enter|esc|any.
                let mut input = Input::default();
                match arg {
                    "up" => input.menu_up = true,
                    "down" => input.menu_down = true,
                    "left" => input.menu_left = true,
                    "right" => input.menu_right = true,
                    "enter" => input.menu_enter = true,
                    "esc" => input.menu_back = true,
                    "any" => input.any_key = true,
                    _ => panic!("key wants up|down|left|right|enter|esc|any in {cmd:?}"),
                }
                game.update(DT, &input);
            }
            "type" => {
                // Feed each character to the save-name text field, one per tic.
                for c in arg.chars() {
                    game.update(DT, &Input { typed: Some(c), ..Default::default() });
                }
            }
            "backspace" => {
                game.update(DT, &Input { backspace: true, ..Default::default() });
            }
            "wait" => step(game, &Input::default(), secs("wait"), &mut rec),
            "snap" => {
                // Keep snapshots observation-pure: rendering marks actor
                // visibility (original FL_VISABLE behavior), which would
                // otherwise perturb the scripted run.
                let saved = game.actors.save_visible();
                game.render(&mut fb);
                game.actors.restore_visible(&saved);
                let path = Path::new(&out).join(format!("{arg}.ppm"));
                write_ppm(&fb, &path);
                println!("snap: {}", path.display());
            }
            "stats" => {
                println!(
                    "stats: floor={} health={} ammo={} score={} lives={} keys={:02b} weapon={}",
                    game.level_idx % 10 + 1,
                    game.health,
                    game.ammo,
                    game.score,
                    game.lives,
                    game.keys,
                    game.weapon,
                );
            }
            // --- Debug-only commands (see module docs) ---
            "teleport" => {
                let parts: Vec<f32> = arg
                    .split(',')
                    .map(|v| v.trim().parse().unwrap_or_else(|_| panic!("bad teleport argument in {cmd:?}")))
                    .collect();
                assert_eq!(parts.len(), 3, "teleport wants x,y,deg in {cmd:?}");
                game.player.x = parts[0];
                game.player.y = parts[1];
                game.player.angle = parts[2].to_radians();
            }
            "face" => {
                game.player.angle = secs("face").to_radians();
            }
            "godmode" => {
                game.god = true;
                println!("godmode: on");
            }
            "aimfire" => {
                for _ in 0..(secs("aimfire") / DT).round() as u32 {
                    let (px, py) = (game.player.x, game.player.y);
                    let target = game
                        .actors
                        .list
                        .iter()
                        .filter(|a| !a.dead && crate::actors::line_clear(&game.world, px, py, a.x, a.y))
                        .map(|a| ((a.x - px).powi(2) + (a.y - py).powi(2), a.x, a.y))
                        .min_by(|a, b| a.0.total_cmp(&b.0));
                    // Express the aim as this tic's turn_delta (rather than
                    // writing the angle directly), so a `record:` capture can
                    // replay the aiming purely from the input stream.
                    let turn_delta = target
                        .map_or(0.0, |(_, bx, by)| (by - py).atan2(bx - px) - game.player.angle);
                    // Hold fire while a target is in sight; idle otherwise.
                    tick(
                        game,
                        &Input { fire: target.is_some(), turn_delta, ..Default::default() },
                        &mut rec,
                    );
                }
            }
            // --- Attract-demo recording (src/demorec.rs) ---
            "record" => {
                assert!(rec.is_none(), "record: while already recording");
                assert!(!arg.is_empty(), "record wants a path in {cmd:?}");
                rec = Some((PathBuf::from(arg), Demo::begin(game)));
                println!("record: started ({arg})");
            }
            "endrecord" => {
                let (path, demo) = rec.take().expect("endrecord without record:");
                demo.write_to(&path).expect("write demo file");
                println!("endrecord: {} ({} tics)", path.display(), demo.tics.len());
            }
            "sounds" => {
                if sound_log.is_empty() {
                    println!("sounds: (none)");
                } else {
                    let list: Vec<String> = sound_log
                        .iter()
                        .map(|&id| format!("{}({id})", crate::sound::name(id)))
                        .collect();
                    println!("sounds: {}", list.join(" "));
                }
            }
            "levelselect" => {
                game.level_sel = game.level_idx;
                game.screen = crate::game::GameScreen::LevelSelect;
            }
            "victory" => {
                println!("victory: {}", game.victory);
            }
            "secrets" => {
                // Debug: list every push-wall tile (plane-1 code 98) with its
                // distance from the player (for choreographing demo recordings).
                let (px, py) = (game.player.x, game.player.y);
                for (i, &c) in game.world.level.plane1.iter().enumerate() {
                    if c == 98 {
                        let (x, y) = ((i % 64) as f32 + 0.5, (i / 64) as f32 + 0.5);
                        let d = ((x - px).powi(2) + (y - py).powi(2)).sqrt();
                        println!("secret: pushwall at ({}, {}) dist {d:.1}", i % 64, i / 64);
                    }
                }
            }
            "actors" => {
                // Debug: list every live actor with its distance from the player
                // (for choreographing demo recordings).
                let (px, py) = (game.player.x, game.player.y);
                for a in game.actors.list.iter().filter(|a| !a.dead) {
                    let d = ((a.x - px).powi(2) + (a.y - py).powi(2)).sqrt();
                    println!("actor: {:?} at ({:.1}, {:.1}) dist {:.1}", a.kind, a.x, a.y, d);
                }
            }
            "pos" => {
                let p = &game.player;
                println!(
                    "pos: x={:.2} y={:.2} angle={:.0}deg",
                    p.x,
                    p.y,
                    p.angle.to_degrees()
                );
                let (cx, cy) = (p.x as i32, p.y as i32);
                for y in cy - 3..=cy + 3 {
                    let row: String = (cx - 5..=cx + 5)
                        .map(|x| {
                            let t = game.world.level.plane0
                                [y as usize * 64 + x as usize];
                            match t {
                                0 => "  . ".into(),
                                90..=101 => format!(" D{t:02}"),
                                t if t >= 106 => "  _ ".into(),
                                t => format!(" {t:3}"),
                            }
                        })
                        .collect();
                    let mark = if y == cy { " <- player" } else { "" };
                    println!("{row}{mark}");
                }
            }
            _ => panic!("unknown demo command {cmd:?}"),
        }

        // Collect any sounds this command produced (and echo them live when
        // WOLF3D_LOG_SOUNDS=1).
        for id in game.take_sounds() {
            if log_sounds {
                println!("sound: {}({id})", crate::sound::name(id));
            }
            sound_log.push(id);
        }
    }

    // A recording left open at script end is closed and written implicitly.
    if let Some((path, demo)) = rec.take() {
        demo.write_to(&path).expect("write demo file");
        println!("endrecord: {} ({} tics, implicit at script end)", path.display(), demo.tics.len());
    }
}

/// Advance one tic, appending the input to the active recording (if any).
fn tick(game: &mut Game, input: &Input, rec: &mut Recording) {
    if let Some((_, demo)) = rec {
        demo.push(input);
    }
    game.update(DT, input);
}

/// Run whole tics until `secs` of virtual time has passed.
fn step(game: &mut Game, input: &Input, secs: f32, rec: &mut Recording) {
    for _ in 0..(secs / DT).round() as u32 {
        tick(game, input, rec);
    }
}

fn write_ppm(fb: &Framebuffer, path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create ppm"));
    write!(f, "P6\n{WIDTH} {HEIGHT}\n255\n").unwrap();
    for &px in &fb.pixels {
        f.write_all(&px.to_le_bytes()[..3]).unwrap(); // bytes are R,G,B,A
    }
}

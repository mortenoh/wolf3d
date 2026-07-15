//! Headless scripted playthroughs, for verifying the game plays without a
//! window (same idea as rust_demoscene's DEMOSCENE_HEADLESS). The script in
//! `WOLF3D_DEMO` runs the real `Game::update`/`render` at a fixed 70 Hz
//! timestep and dumps PPM snapshots, so gameplay is checkable from a
//! terminal or CI.
//!
//! Script: semicolon-separated commands.
//!   w:1.5 / s:… / a:… / d:…  hold that movement key for the given seconds
//!   l:90 / r:90              turn left/right by degrees (at turn speed)
//!   use                      tap the use key (open/close a door)
//!   wait:1.0                 let time pass (doors keep animating)
//!   snap:name                write <out>/name.ppm
//! Output dir comes from WOLF3D_SNAP_DIR (default "snaps").

use std::io::Write;
use std::path::Path;

use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::game::{Game, Input, TURN_SPEED};

const DT: f32 = 1.0 / 70.0; // the original's tic rate

pub fn run(game: &mut Game, script: &str) {
    let out = std::env::var("WOLF3D_SNAP_DIR").unwrap_or_else(|_| "snaps".into());
    std::fs::create_dir_all(&out).expect("create snapshot dir");
    let mut fb = Framebuffer::new();

    for cmd in script.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let (op, arg) = cmd.split_once(':').unwrap_or((cmd, ""));
        let secs = |what: &str| -> f32 {
            arg.parse()
                .unwrap_or_else(|_| panic!("bad {what} argument in {cmd:?}"))
        };
        match op {
            "w" | "s" | "a" | "d" => {
                let input = Input {
                    forward: op == "w",
                    back: op == "s",
                    strafe_left: op == "a",
                    strafe_right: op == "d",
                    ..Default::default()
                };
                step(game, &input, secs("hold"));
            }
            "l" | "r" => {
                let input = Input {
                    turn_left: op == "l",
                    turn_right: op == "r",
                    ..Default::default()
                };
                step(game, &input, secs("degrees").to_radians() / TURN_SPEED);
            }
            "use" => {
                game.update(DT, &Input { use_door: true, ..Default::default() });
            }
            "wait" => step(game, &Input::default(), secs("wait")),
            "snap" => {
                game.render(&mut fb);
                let path = Path::new(&out).join(format!("{arg}.ppm"));
                write_ppm(&fb, &path);
                println!("snap: {}", path.display());
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
    }
}

/// Run whole tics until `secs` of virtual time has passed.
fn step(game: &mut Game, input: &Input, secs: f32) {
    for _ in 0..(secs / DT).round() as u32 {
        game.update(DT, input);
    }
}

fn write_ppm(fb: &Framebuffer, path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("create ppm"));
    write!(f, "P6\n{WIDTH} {HEIGHT}\n255\n").unwrap();
    for &px in &fb.pixels {
        f.write_all(&px.to_le_bytes()[..3]).unwrap(); // bytes are R,G,B,A
    }
}

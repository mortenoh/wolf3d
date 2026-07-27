# wolf3d

Wolfenstein 3D and Spear of Destiny, rewritten from scratch in Rust. A CPU
raycaster renders into a 320x200 framebuffer that the GPU blits to the window
(winit + wgpu); everything else — enemies, bosses, doors, secrets, menus,
sound — is a faithful reimplementation of the behavior in id Software's
released source.

## What works

- The full registered Wolfenstein 3D: all 6 episodes, 60 levels
- Spear of Destiny: the original 21-floor campaign (`make run-sod`) plus the
  two GOG mission packs when extracted (`make run-sod-m2` / `run-sod-m3`)
- All enemies and bosses, including the two-phase mecha-Hitler fight, the
  Angel of Death, and the E3M10 ghosts
- Sliding doors, locked doors and keys, secret push-walls, pickups
- The complete menu system: episode/difficulty select, sound options,
  save/load with named slots, Change View, Read This!, high scores, and the
  random Y/N Quit taunt
- Sound: digitized effects plus AdLib music and effects through OPL2 emulation
  (nuked-opl3). The Sound menu's "Synthesized" mode is OPL-only (no PC-speaker
  bank — that path is intentionally not reimplemented)
- Floor-completed intermission with ratio bonuses, deathcam, victory sequence,
  end-of-episode story text
- Attract mode with recorded demo playback (our own demo format; deterministic
  bit-exact replays)

## You need the game data

No copyrighted game data is included. Buy Wolfenstein 3D + Spear of Destiny
(the GOG release works), put the installer(s) in
`data/Wolfenstein.3D.and.Spear.of.Destiny-GOG/`, then:

```
brew install innoextract          # apt install innoextract on Linux
make data                         # extracts the .WL6 files
make run                          # play Wolfenstein 3D
make data-sod                     # extracts Spear of Destiny M1 (*.SOD)
make run-sod                      # play Spear of Destiny
make data-sod-m2                  # mission pack 2 → *.SD2 (optional)
make run-sod-m2                   # Return to Danger
make data-sod-m3                  # mission pack 3 → *.SD3 (optional)
make run-sod-m3                   # Ultimate Challenge
```

`make data-all` pulls WL6 plus all three Spear packs. Mission packs share the
Spear engine tables; they are selected with `WOLF3D_GAME=sd2` / `sd3` (the
`run-sod-m*` targets set this). They are never auto-selected when WL6 or M1
data is also present.

The run targets extract the data first, so `make run` on a fresh checkout is
enough. On Linux, cpal's ALSA backend also needs the system headers
(`apt install libasound2-dev`).

## Building and developing

`make` (or `make help`) lists every target. The ones you want:

```
make build-release   # build the optimized binary
make test            # run the test suite (needs the extracted data)
make ci              # fmt-check + clippy + release build + test compile
make fmt             # format the source
make clean           # cargo clean (clean-data/clean-saves/distclean also exist)
```

`make run LEVEL=5` starts on a given level, and `make record OUT=demos/x.dm`
captures a play session as an attract demo.

## Controls

Two schemes work at the same time (there is no mode toggle):

**Modern**
- WASD move and strafe; mouse look
- Left click fire, right click open doors / push walls
- Mouse wheel cycles weapons

**Classic-style**
- Arrow keys move and turn (no strafe on the arrows alone)
- Space or Ctrl fire; E open doors / push walls
- Shift run (left or right)

**Always**
- 1-4 select knife / pistol / machine gun / chaingun
- M toggles music, F or F11 fullscreen, Esc menu, Q quit
- Main menu → Control: mouse-look sensitivity (key rebinding is not offered)

**Cheats** (during play): 6 level warp, 7 god mode, 8 free items, 9 infinite
ammo, 0 the classic MLI (full health/ammo/keys, score wiped).

## Headless verification

The simulation is deterministic and window-independent. `WOLF3D_DEMO` plays a
scripted input sequence at the original 70 Hz tic rate with no window and dumps
framebuffer snapshots (see `src/demo.rs`); the test suite uses the same path to
prove things like "the first door of E1M1 opens" and "Hitler dies and sets the
victory flag". `make test` requires the extracted game data (the Spear of
Destiny tests skip themselves when `data/VSWAP.SOD` is absent).

To drive it by hand:

```
make demo SCRIPT='w:1;use;wait:1;snap:door'   # snapshots land in snaps/
```

## Provenance and license

The code in this repository is a from-scratch rewrite: behavior (state tables,
timings, damage formulas, file formats) was extracted by reading the original
source release and community documentation, then implemented in Rust. It is
provided for personal and educational use. Wolfenstein 3D, Spear of Destiny,
and all game data are the property of id Software / ZeniMax Media; the
original source release is governed by its own license. Same posture as
long-standing community ports like Wolf4SDL and ECWolf.

# wolf3d

Wolfenstein 3D and Spear of Destiny, rewritten from scratch in Rust. A CPU
raycaster renders into a 320x200 framebuffer that the GPU blits to the window
(winit + wgpu); everything else — enemies, bosses, doors, secrets, menus,
sound — is a faithful reimplementation of the behavior in id Software's
released source.

## What works

- The full registered Wolfenstein 3D: all 6 episodes, 60 levels
- Spear of Destiny: the 21-floor campaign (`make run-sod`)
- All enemies and bosses, including the two-phase mecha-Hitler fight, the
  Angel of Death, and the E3M10 ghosts
- Sliding doors, locked doors and keys, secret push-walls, pickups
- The complete menu system: episode/difficulty select, sound options,
  save/load with named slots, Change View, Read This!, high scores
- Sound: digitized effects plus AdLib music and effects through OPL2 emulation
  (nuked-opl3)
- Floor-completed intermission with ratio bonuses, deathcam, victory sequence,
  end-of-episode story text
- Attract mode with recorded demo playback (our own demo format; deterministic
  bit-exact replays)

## You need the game data

No copyrighted game data is included. Buy Wolfenstein 3D + Spear of Destiny
(the GOG release works), put the installer(s) in
`data/Wolfenstein.3D.and.Spear.of.Destiny-GOG/`, then:

```
brew install innoextract
make data        # extracts the .WL6 files
make run         # play Wolfenstein 3D
make data-sod    # extracts Spear of Destiny (M1 campaign)
make run-sod     # play Spear of Destiny
```

## Controls

WASD + mouse (mouse look, left click fire, right click open, wheel weapon
switch), or arrows/Space/E. Shift runs, 1-4 select weapons, M toggles music,
F fullscreen, Esc menu, Q quit. Cheats: 6 level warp, 7 god, 8 items, 9
infinite ammo, 0 the classic MLI.

## Headless verification

The simulation is deterministic and window-independent. `WOLF3D_DEMO` plays a
scripted input sequence at the original 70 Hz tic rate with no window and dumps
framebuffer snapshots (see `src/demo.rs`); the test suite uses the same path to
prove things like "the first door of E1M1 opens" and "Hitler dies and sets the
victory flag". `cargo test` requires the extracted game data.

## Provenance and license

The code in this repository is a from-scratch rewrite: behavior (state tables,
timings, damage formulas, file formats) was extracted by reading the original
source release and community documentation, then implemented in Rust. It is
provided for personal and educational use. Wolfenstein 3D, Spear of Destiny,
and all game data are the property of id Software / ZeniMax Media; the
original source release is governed by its own license. Same posture as
long-standing community ports like Wolf4SDL and ECWolf.

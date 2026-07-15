//! Loader sanity checks against the real .WL6 data files (requires `data/`).

use wolf3d::assets::{MAP_SIZE, MapSet, VSwap, data_dir};

#[test]
fn gamemaps_decode() {
    let maps = MapSet::load(&data_dir()).expect("data files present");
    assert_eq!(maps.num_levels(), 60, "registered 6-episode set");

    let l1 = maps.level(0);
    assert_eq!(l1.name, "Wolf1 Map1");
    assert_eq!(l1.plane0.len(), MAP_SIZE * MAP_SIZE);
    assert_eq!(l1.plane1.len(), MAP_SIZE * MAP_SIZE);

    // The whole border must be solid wall (1..=89).
    for i in 0..MAP_SIZE {
        for &t in &[
            l1.plane0[i],                            // top row
            l1.plane0[(MAP_SIZE - 1) * MAP_SIZE + i], // bottom row
            l1.plane0[i * MAP_SIZE],                 // left col
            l1.plane0[i * MAP_SIZE + MAP_SIZE - 1],  // right col
        ] {
            assert!((1..=89).contains(&t), "border tile {t} not a wall");
        }
    }

    // Exactly one player spawn (plane1 19..=22).
    let spawns = l1.plane1.iter().filter(|t| (19..=22).contains(*t)).count();
    assert_eq!(spawns, 1);

    // Every level decodes and has a spawn.
    for n in 0..maps.num_levels() {
        let l = maps.level(n);
        let spawns = l.plane1.iter().filter(|t| (19..=22).contains(*t)).count();
        assert_eq!(spawns, 1, "level {n} ({})", l.name);
    }
}

#[test]
fn vswap_walls() {
    let vswap = VSwap::load(&data_dir()).expect("data files present");
    // Registered Wolf3D has 106 wall textures (53 light/dark pairs).
    assert_eq!(vswap.sprite_start, 106);
    // Textures are palettized: fully opaque packed colors.
    for tex in &vswap.walls {
        assert!(tex.iter().all(|&c| c >> 24 == 0xFF));
    }
}

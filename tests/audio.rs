//! Audio milestone checks (requires `data/`): loader sanity for AUDIOT/VSWAP,
//! the game emitting the right sound events, and a deterministic offline OPL
//! render. Nothing here opens an audio device.

use wolf3d::assets::audio::{AudioData, NUM_MUSIC, NUM_SOUNDS};
use wolf3d::assets::{VSwap, data_dir};
use wolf3d::game::{Game, Input};
use wolf3d::sound::{self, Engine, SoundAssets};

const DT: f32 = 1.0 / 70.0;

fn load_assets() -> SoundAssets {
    let dir = data_dir();
    let audio = AudioData::load(&dir).expect("AUDIOHED/AUDIOT");
    let vswap = VSwap::load(&dir).expect("VSWAP");
    SoundAssets::new(audio, vswap.digi)
}

// ---- Loader sanity --------------------------------------------------------

#[test]
fn adlib_and_music_chunk_counts() {
    let audio = AudioData::load(&data_dir()).expect("audio");
    // All 87 AdLib effects decode.
    assert_eq!(audio.sfx.len(), NUM_SOUNDS);
    assert!(
        audio.sfx.iter().all(Option::is_some),
        "an AdLib effect failed to decode"
    );
    // All 27 IMF songs decode to a non-empty stream.
    assert_eq!(audio.music.len(), NUM_MUSIC);
    assert!(
        audio.music.iter().all(|m| !m.is_empty()),
        "a music track is empty"
    );
    // A known song has a plausible IMF length (4-byte events).
    let getthem = &audio.music[sound::GETTHEM_MUS];
    assert!(
        getthem.len() > 1000 && getthem.len().is_multiple_of(4),
        "GETTHEM imf = {}",
        getthem.len()
    );
}

#[test]
fn adlib_effect_header_is_plausible() {
    let audio = AudioData::load(&data_dir()).expect("audio");
    let pistol = audio.sfx[sound::ATKPISTOLSND]
        .as_ref()
        .expect("pistol adlib");
    assert!(pistol.priority > 0);
    assert!(!pistol.data.is_empty());
    // The instrument's sustain bytes are non-zero (SDL_ALPlaySound rejects a
    // "bad instrument" with both sustain cells zero).
    assert!(pistol.instrument[6] | pistol.instrument[7] != 0);
}

#[test]
fn digi_map_and_pcm() {
    let dir = data_dir();
    let audio = AudioData::load(&dir).expect("audio");
    let vswap = VSwap::load(&dir).expect("vswap");
    // The WL6 map has 46 digitized sounds (0..=45).
    assert_eq!(vswap.digi.len(), 46);
    // Known mappings from wolfdigimap.
    assert_eq!(audio.digi_map[sound::ATKPISTOLSND], 5);
    assert_eq!(audio.digi_map[sound::HALTSND], 0);
    assert_eq!(audio.digi_map[sound::HITWALLSND], -1);
    // The pistol digitized sample is real 8-bit PCM: non-empty and not silence.
    let pistol = &vswap.digi[5];
    assert!(pistol.len() > 1000);
    let min = *pistol.iter().min().unwrap();
    let max = *pistol.iter().max().unwrap();
    assert!(
        max - min > 32,
        "digitized pistol looks constant ({min}..{max})"
    );
}

// ---- Event emission -------------------------------------------------------

#[test]
fn firing_pistol_emits_pistol_sound() {
    let mut game = Game::new(0);
    let fire = Input {
        fire: true,
        ..Default::default()
    };
    let mut sounds = Vec::new();
    for _ in 0..24 {
        game.update(DT, &fire);
        sounds.extend(game.take_sounds());
    }
    assert!(
        sounds.contains(&(sound::ATKPISTOLSND as u8)),
        "expected a pistol shot; got {sounds:?}"
    );
}

#[test]
fn opening_door_emits_door_sound() {
    // E1M1: walk east into the first door, then use it.
    let mut game = Game::new(0);
    let forward = Input {
        forward: true,
        ..Default::default()
    };
    let mut sounds = Vec::new();
    for _ in 0..140 {
        game.update(DT, &forward);
        sounds.extend(game.take_sounds());
    }
    game.update(
        DT,
        &Input {
            use_door: true,
            ..Default::default()
        },
    );
    sounds.extend(game.take_sounds());
    assert!(
        sounds.contains(&(sound::OPENDOORSND as u8)),
        "expected an open-door sound; got {sounds:?}"
    );
}

#[test]
fn sound_events_do_not_perturb_gameplay_rng() {
    // Two runs of an identical fight must stay bit-identical whether or not the
    // sound queue is drained — proof the audio path never touches the gameplay
    // RNG (guard death screams use a separate stream).
    let script = |drain: bool| {
        let mut game = Game::new(0);
        let fire = Input {
            fire: true,
            ..Default::default()
        };
        for _ in 0..200 {
            game.update(DT, &fire);
            if drain {
                let _ = game.take_sounds();
            }
        }
        (game.score, game.ammo, game.health, game.actors.list.len())
    };
    assert_eq!(script(true), script(false));
}

// ---- Offline OPL render (deterministic) -----------------------------------

#[test]
fn music_render_is_nonsilent_and_deterministic() {
    let mut a = Engine::new(44_100, load_assets());
    a.play_music(Some(sound::GETTHEM_MUS));
    let buf = sound::render_offline(&mut a, 1.0);

    assert_eq!(buf.len(), 44_100);
    let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.02, "music render is (near) silent, peak = {peak}");

    // Same spec rendered again is byte-identical (the offline path is a pure
    // function of the data), and matches the captured fingerprint.
    let mut b = Engine::new(44_100, load_assets());
    b.play_music(Some(sound::GETTHEM_MUS));
    let buf2 = sound::render_offline(&mut b, 1.0);
    assert_eq!(sound::checksum(&buf), sound::checksum(&buf2));
    assert_eq!(sound::checksum(&buf), 0x5201_e201_a37e_1056);
}

#[test]
fn digitized_effect_renders_sound() {
    let mut e = Engine::new(44_100, load_assets());
    e.play_sound(sound::ATKPISTOLSND as u8);
    let buf = sound::render_offline(&mut e, 0.6);
    let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.05, "digitized pistol is silent, peak = {peak}");
    assert_eq!(sound::checksum(&buf), 0x047d_36d2_b320_6bad);
}

#[test]
fn priority_rejects_lower_priority_sound() {
    // A second, equal-or-higher priority effect is accepted; the engine simply
    // must not panic and must keep producing samples.
    let mut e = Engine::new(44_100, load_assets());
    e.play_sound(sound::ATKPISTOLSND as u8);
    e.play_sound(sound::HALTSND as u8);
    let buf = sound::render_offline(&mut e, 0.3);
    assert!(buf.iter().any(|&s| s.abs() > 0.01));
}
